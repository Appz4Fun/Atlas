use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

enum Control<T> {
    Elem(T),
    Unblock,
}

pub struct MessagesQueue<T>
where
    T: Send,
{
    queue: Mutex<VecDeque<Control<T>>>,
    condvar: Condvar,
    closed: AtomicBool,
}

impl<T> MessagesQueue<T>
where
    T: Send,
{
    pub fn with_capacity(capacity: usize) -> Arc<MessagesQueue<T>> {
        Arc::new(MessagesQueue {
            queue: Mutex::new(VecDeque::with_capacity(capacity)),
            condvar: Condvar::new(),
            closed: AtomicBool::new(false),
        })
    }

    /// Pushes an element to the queue.
    pub fn push(&self, value: T) {
        let mut queue = self.queue.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            drop(queue);
            drop(value);
            return;
        }
        queue.push_back(Control::Elem(value));
        self.condvar.notify_one();
    }

    // Used only by the opt-in bounded listener. Drop queued Requests outside
    // the queue lock so response notification/body cleanup cannot deadlock it.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let queued = {
            let mut queue = self.queue.lock().unwrap();
            std::mem::take(&mut *queue)
        };
        self.condvar.notify_all();
        drop(queued);
    }

    /// Unblock one thread stuck in pop loop.
    pub fn unblock(&self) {
        let mut queue = self.queue.lock().unwrap();
        if self.closed.load(Ordering::Acquire) { return; }
        queue.push_back(Control::Unblock);
        self.condvar.notify_one();
    }

    /// Pops an element. Blocks until one is available.
    /// Returns None in case unblock() was issued.
    pub fn pop(&self) -> Option<T> {
        let mut queue = self.queue.lock().unwrap();

        loop {
            match queue.pop_front() {
                Some(Control::Elem(value)) => return Some(value),
                Some(Control::Unblock) => return None,
                None if self.closed.load(Ordering::Acquire) => return None,
                None => (),
            }

            queue = self.condvar.wait(queue).unwrap();
        }
    }

    /// Tries to pop an element without blocking.
    pub fn try_pop(&self) -> Option<T> {
        let mut queue = self.queue.lock().unwrap();
        match queue.pop_front() {
            Some(Control::Elem(value)) => Some(value),
            Some(Control::Unblock) | None => None,
        }
    }

    /// Tries to pop an element without blocking
    /// more than the specified timeout duration
    /// or unblock() was issued
    pub fn pop_timeout(&self, timeout: Duration) -> Option<T> {
        let mut queue = self.queue.lock().unwrap();
        let mut duration = timeout;
        loop {
            match queue.pop_front() {
                Some(Control::Elem(value)) => return Some(value),
                Some(Control::Unblock) => return None,
                None if self.closed.load(Ordering::Acquire) => return None,
                None => (),
            }
            let now = Instant::now();
            let (_queue, result) = self.condvar.wait_timeout(queue, timeout).unwrap();
            queue = _queue;
            let sleep_time = now.elapsed();
            duration = if duration > sleep_time {
                duration - sleep_time
            } else {
                Duration::from_millis(0)
            };
            if result.timed_out()
                || (duration.as_secs() == 0 && duration.subsec_nanos() < 1_000_000)
            {
                return None;
            }
        }
    }
}
