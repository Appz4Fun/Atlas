//! Optional transport admission and per-request deadlines.

use std::io::{self, ErrorKind};
use std::io::Read;
use std::net::Shutdown;
use std::sync::{Arc, Mutex, Weak};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use crate::connection::Connection;

/// Limits for the explicitly bounded HTTP constructor. Ordinary constructors
/// retain their existing behavior. These limits apply to incoming protocol
/// envelopes and transport lifetimes, never outgoing application metadata.
#[derive(Clone, Debug)]
pub struct RequestLimits {
    pub max_connections: usize,
    pub read_idle: Duration,
    pub write_idle: Duration,
    pub request_timeout: Duration,
    pub max_header_bytes: usize,
    pub max_header_count: usize,
    pub max_request_line_bytes: usize,
    /// Aggregate incoming body bytes, including chunk framing/extensions.
    /// This bounds third-party chunk decoder metadata before it allocates.
    pub max_body_wire_bytes: usize,
}

impl RequestLimits {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.max_connections == 0 || self.read_idle.is_zero()
            || self.write_idle.is_zero() || self.request_timeout.is_zero()
            || self.max_header_bytes == 0 || self.max_header_count == 0
            || self.max_request_line_bytes == 0 || self.max_body_wire_bytes == 0 {
            return Err(io::Error::new(ErrorKind::InvalidInput, "bounded HTTP limits must be positive"));
        }
        if Instant::now().checked_add(self.request_timeout).is_none() {
            return Err(io::Error::new(ErrorKind::InvalidInput, "HTTP deadline is out of range"));
        }
        Ok(())
    }
}

pub(crate) struct BodyWire<R> {
    input: R,
    remaining: usize,
    control: Option<RequestControl>,
}
impl<R> BodyWire<R> {
    pub(crate) fn new(input: R, control: Option<RequestControl>) -> Self {
        let remaining = control.as_ref().map_or(usize::MAX, |control| control.limits().max_body_wire_bytes);
        Self { input, remaining, control }
    }
}
impl<R: Read> Read for BodyWire<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() { return Ok(0); }
        let Some(control) = &self.control else { return self.input.read(output); };
        control.remaining()?;
        if self.remaining == 0 {
            control.0.body_limit_exceeded.store(true, Ordering::Release);
            control.close();
            return Err(io::Error::new(ErrorKind::InvalidData, "HTTP body wire admission exceeded"));
        }
        let length = output.len().min(self.remaining);
        let got = self.input.read(&mut output[..length])?;
        if got == 0 {
            control.0.body_eof_seen.store(true, Ordering::Release);
            // A framed body finishes before asking its raw source for EOF.
            // In particular, chunked_transfer can otherwise report Ok(0)
            // while still inside an unfinished data chunk.
            return Err(io::Error::new(ErrorKind::UnexpectedEof,
                "HTTP body framing ended before completion"));
        }
        self.remaining -= got;
        Ok(got)
    }
}

pub(crate) struct Admission {
    limits: RequestLimits,
    active: AtomicUsize,
    stopping: AtomicBool,
    controls: Mutex<Vec<Weak<Control>>>,
}
impl Admission {
    pub(crate) fn new(limits: RequestLimits) -> io::Result<Arc<Self>> {
        limits.validate()?;
        Ok(Arc::new(Self {
            limits, active: AtomicUsize::new(0), stopping: AtomicBool::new(false),
            controls: Mutex::new(Vec::new()),
        }))
    }

    pub(crate) fn accept(self: &Arc<Self>, socket: &Connection) -> io::Result<Option<RequestControl>> {
        if self.stopping.load(Ordering::Acquire) {
            socket.shutdown(Shutdown::Both).ok();
            return Ok(None);
        }
        if self.active.fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < self.limits.max_connections).then(|| active + 1)
        }).is_err() {
            socket.shutdown(Shutdown::Both).ok();
            return Ok(None);
        }
        let socket = match socket.try_clone() {
            Ok(socket) => socket,
            Err(error) => {
                self.active.fetch_sub(1, Ordering::AcqRel);
                return Err(error);
            }
        };
        let control = Arc::new(Control {
            admission: self.clone(), socket,
            deadline: Mutex::new(Instant::now() + self.limits.request_timeout),
            closed: AtomicBool::new(false), body_limit_exceeded: AtomicBool::new(false),
            body_eof_seen: AtomicBool::new(false),
        });
        let mut controls = self.controls.lock().unwrap_or_else(|error| error.into_inner());
        if self.stopping.load(Ordering::Acquire) {
            control.socket.shutdown(Shutdown::Both).ok();
            return Ok(None);
        }
        controls.retain(|control| control.strong_count() != 0);
        controls.push(Arc::downgrade(&control));
        Ok(Some(RequestControl(control)))
    }

    pub(crate) fn close(&self) {
        self.stopping.store(true, Ordering::Release);
        let controls = self.controls.lock().unwrap_or_else(|error| error.into_inner());
        for control in controls.iter().filter_map(Weak::upgrade) {
            RequestControl(control).close();
        }
    }

    pub(crate) fn active(&self) -> usize { self.active.load(Ordering::Acquire) }
}

struct Control {
    admission: Arc<Admission>,
    socket: Connection,
    deadline: Mutex<Instant>,
    closed: AtomicBool,
    body_limit_exceeded: AtomicBool,
    body_eof_seen: AtomicBool,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.socket.shutdown(Shutdown::Both).ok();
        self.admission.active.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A read-only cancellation/deadline handle for a bounded request. Cloning the
/// handle retains connection admission until the application releases it.
#[derive(Clone)]
pub struct RequestControl(Arc<Control>);
impl RequestControl {
    pub(crate) fn body_eof_seen(&self) -> bool {
        self.0.body_eof_seen.load(Ordering::Acquire)
    }

    pub fn body_limit_exceeded(&self) -> bool {
        self.0.body_limit_exceeded.load(Ordering::Acquire)
    }

    pub fn remaining(&self) -> io::Result<Duration> {
        if self.0.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(ErrorKind::ConnectionAborted, "HTTP connection is closed"));
        }
        let remaining = self.0.deadline.lock().unwrap_or_else(|error| error.into_inner())
            .checked_duration_since(Instant::now());
        match remaining.filter(|remaining| !remaining.is_zero()) {
            Some(remaining) => Ok(remaining),
            None => {
                self.close();
                Err(io::Error::new(ErrorKind::TimedOut, "HTTP request deadline expired"))
            }
        }
    }

    pub(crate) fn begin(&self) -> io::Result<()> {
        // A closed transport can never be reused/reset for another request.
        self.remaining()?;
        *self.0.deadline.lock().unwrap_or_else(|error| error.into_inner()) =
            Instant::now() + self.0.admission.limits.request_timeout;
        self.0.body_eof_seen.store(false, Ordering::Release);
        Ok(())
    }
    pub(crate) fn limits(&self) -> &RequestLimits { &self.0.admission.limits }
    pub(crate) fn close(&self) {
        if !self.0.closed.swap(true, Ordering::AcqRel) {
            self.0.socket.shutdown(Shutdown::Both).ok();
        }
    }
    pub(crate) fn before_read(&self) -> io::Result<()> {
        let timeout = self.remaining()?.min(self.limits().read_idle);
        self.0.socket.set_read_timeout(Some(timeout))
    }
    pub(crate) fn before_write(&self) -> io::Result<()> {
        let timeout = self.remaining()?.min(self.limits().write_idle);
        self.0.socket.set_write_timeout(Some(timeout))
    }
    pub(crate) fn after<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() { self.close(); }
        result
    }
}
