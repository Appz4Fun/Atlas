//! Checked native download headers without an unbounded owned Header value.
//!
//! Only completed, validated spools can reach this sender. The raw tiny_http
//! writer preserves its response ordering and request connection lifecycle.

use std::io::{self, Read, Write};
use std::sync::Arc;
use crate::nzb_spool::{Pool, Spool};

#[derive(Debug, PartialEq, Eq)]
pub struct Disposition {
    name: Spool,
    release_id: i64,
}

impl Disposition {
    /// The same filename sanitizer, whitespace/dot trim, empty fallback and
    /// Unicode-header eligibility as the existing tiny_http response path.
    /// The full sanitized name is verified before any HTTP bytes are exposed.
    pub fn prepare(pool: &Arc<Pool>, name: impl Read, release_id: i64) -> io::Result<Option<Self>> {
        let mut spool = pool.open()?;
        let mut start = None;
        let mut end = 0u64;
        let mut position = 0u64;
        let mut ascii = true;
        crate::nzb_stream::text_chunks(name, |text| {
            if !ascii { return Ok(()) }
            let mut buffer = [0u8; 8196];
            let mut used = 0;
            for character in text.chars() {
                let character = crate::nzb::filename_char(character);
                ascii &= character.is_ascii();
                let mut utf8 = [0u8; 4];
                let encoded = character.encode_utf8(&mut utf8).as_bytes();
                if used + encoded.len() > buffer.len() {
                    spool.write_all(&buffer[..used])?;
                    used = 0;
                }
                buffer[used..used + encoded.len()].copy_from_slice(encoded);
                used += encoded.len();
                let next = position.checked_add(encoded.len() as u64).ok_or_else(|| io::Error::other("filename extent overflow"))?;
                if character != ' ' && character != '.' {
                    start.get_or_insert(position);
                    end = next;
                }
                position = next;
            }
            if ascii { spool.write_all(&buffer[..used]) } else { Ok(()) }
        })?;
        if !ascii {
            // Header::from_bytes rejects non-ASCII values in the legacy path.
            // The original Unicode catalog/effective name remains unchanged.
            return Ok(None);
        }
        let name = match start {
            Some(start) => spool.finish_range(start, end)?,
            None => {
                drop(spool);
                let mut fallback = pool.open()?;
                fallback.write_all(b"release")?;
                fallback.finish()?
            }
        };
        Ok(Some(Self { name, release_id }))
    }

    fn write(mut self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(b"Content-Disposition: attachment; filename=\"")?;
        io::copy(&mut self.name, output)?;
        if self.release_id != 0 { write!(output, ".{}", self.release_id)? }
        output.write_all(b".nzb\"\r\n")
    }
}

/// This consumes and drops the raw request writer promptly on EVERY outcome,
/// including a broken pipe, partial header, HEAD or body cancellation. A slow
/// socket retains bounded disk permits, never a PostgreSQL snapshot.
pub fn send(
    request: tiny_http::Request,
    body: Spool,
    disposition: Option<Disposition>,
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
) -> io::Result<()> {
    if !body.is_complete() || !(200..=599).contains(&status)
        || !content_type.is_ascii() || content_type.bytes().any(|byte| byte < 32 || byte == 127) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid native response envelope"));
    }
    let head = *request.method() == tiny_http::Method::Head;
    let version = request.http_version().clone();
    let close = request.response_requires_close()
        || request.headers().iter().any(|header| {
            header.field.equiv("Connection") && header.value.as_str().split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        });
    let mut fixed = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_\x60|~".contains(&byte)) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid native response header name"));
        }
        if ["Content-Length", "Content-Disposition", "Transfer-Encoding", "Connection", "Content-Type", "Date", "Server"]
            .iter().any(|reserved| name.eq_ignore_ascii_case(reserved)) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "reserved native response header"));
        }
        if !value.is_ascii() || value.bytes().any(|byte| byte < 32 || byte == 127) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid native response header value"));
        }
        fixed.push(tiny_http::Header::from_bytes(name, value)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid native response header name"))?);
    }
    if close {
        fixed.push(tiny_http::Header::from_bytes("Connection", "close").expect("static protocol header"));
    }
    let request_control = request.control();
    request.respond_raw(|mut output| {
        if let Some(control) = request_control.as_ref() { control.remaining()?; }
        print(&mut output, body, disposition, status, content_type, &fixed, version, head)
    })
}

fn print(
    output: &mut impl Write,
    mut body: Spool,
    disposition: Option<Disposition>,
    status: u16,
    content_type: &'static str,
    headers: &[tiny_http::Header],
    version: tiny_http::HTTPVersion,
    head: bool,
) -> io::Result<()> {
    let reason = tiny_http::StatusCode(status).default_reason_phrase();
    write!(output, "HTTP/{}.{} {status} {reason}\r\n", version.0, version.1)?;
    output.write_all(b"Server: tiny-http (Rust)\r\nDate: ")?;
    write!(output, "{}\r\n", chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT"))?;
    write!(output, "Content-Type: {content_type}\r\nContent-Length: {}\r\n", body.len())?;
    for header in headers { write!(output, "{header}\r\n")? }
    if let Some(disposition) = disposition { disposition.write(output)? }
    output.write_all(b"\r\n")?;
    if !head { io::copy(&mut body, output)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::time::Duration;
    use crate::nzb_spool::Limits;

    fn pool(root: &std::path::Path) -> Arc<Pool> {
        Pool::new(root.to_owned(), Limits { requests: 4, per_request_bytes: 16 * 1024 * 1024, total_bytes: 32 * 1024 * 1024, free_bytes: 0 }).unwrap()
    }
    fn body(pool: &Arc<Pool>) -> Spool {
        let mut body = pool.open().unwrap();
        body.write_all(b"<checked/>").unwrap();
        body.finish().unwrap()
    }

    #[test]
    fn native_disposition_matches_the_actual_legacy_header_oracle() {
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path());
        let controls: String = (0u8..=31).chain(std::iter::once(127)).map(char::from).collect();
        for name in [" ..Example.Release.. ", "", "   ... ", "a\"\\\r\nb", "Café", controls.as_str()] {
            let filename = crate::nzb::nzb_filename(name, Some(7)).replace(['"', '\\'], "_");
            let legacy = tiny_http::Header::from_bytes("Content-Disposition", format!("attachment; filename=\"{filename}\"")).ok();
            let native = Disposition::prepare(&pool, name.as_bytes(), 7).unwrap();
            assert_eq!(native.is_some(), legacy.is_some(), "{name:?}");
            if let Some(native) = native {
                let mut bytes = Vec::new();
                native.write(&mut bytes).unwrap();
                assert_eq!(bytes, format!("{}\r\n", legacy.unwrap()).as_bytes(), "{name:?}");
                assert_eq!(bytes.windows(2).filter(|bytes| *bytes == b"\r\n").count(), 1);
            }
        }
    }

    #[test]
    fn raw_writer_preserves_head_and_pipelined_connection_order() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path());
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let request = server.recv_timeout(Duration::from_secs(3)).unwrap().expect("synthetic request");
                let disposition = Disposition::prepare(&pool, b"Example.Release".as_slice(), 7).unwrap();
                send(request, body(&pool), disposition, 200, "application/x-nzb", Vec::new()).unwrap();
            }
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        client.write_all(b"HEAD /ui/nzb HTTP/1.1\r\nHost: localhost\r\n\r\nGET /ui/nzb HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        worker.join().unwrap();
        assert_eq!(response.matches("HTTP/1.1 200 OK\r\n").count(), 2);
        assert_eq!(response.matches("Content-Length: 10\r\n").count(), 2);
        assert_eq!(response.matches("<checked/>").count(), 1);
        assert!(response.contains("filename=\"Example.Release.7.nzb\"\r\n"));
        assert!(!response.contains("Transfer-Encoding"));
    }

    #[test]
    fn raw_writer_supports_http_10_without_chunking_or_body_buffering() {
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path());
        let mut output = Vec::new();
        print(&mut output, body(&pool), None, 200, "application/x-nzb", &[], tiny_http::HTTPVersion(1, 0), false).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("HTTP/1.0 200 OK\r\n"));
        assert!(output.ends_with("\r\n\r\n<checked/>"));
        assert!(!output.contains("Transfer-Encoding"));
    }

    #[test]
    fn giant_ascii_disposition_is_written_in_bounded_chunks() {
        struct Count { bytes: u64, max: usize }
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes += bytes.len() as u64;
                self.max = self.max.max(bytes.len());
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path());
        let header = Disposition::prepare(&pool, io::repeat(b'x').take(8 * 1024 * 1024), 7).unwrap().unwrap();
        let mut count = Count { bytes: 0, max: 0 };
        header.write(&mut count).unwrap();
        assert!(count.bytes > 8 * 1024 * 1024);
        assert!(count.max <= 8192);
    }

    #[test]
    fn a_giant_header_crosses_real_tcp_without_an_owned_header_buffer() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let root = tempfile::tempdir().unwrap();
        let pool = pool(root.path());
        let worker = std::thread::spawn(move || {
            let request = server.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
            let name = Disposition::prepare(&pool, io::repeat(b'x').take(8 * 1024 * 1024), 7).unwrap();
            send(request, body(&pool), name, 200, "application/x-nzb", Vec::new()).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        client.write_all(b"GET /ui/nzb HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
        let mut buffer = [0u8; 8192];
        let mut window = [0u8; 4];
        let mut in_body = false;
        let mut xs = 0u64;
        let mut body = Vec::new();
        loop {
            let got = client.read(&mut buffer).unwrap();
            if got == 0 { break }
            for &byte in &buffer[..got] {
                if in_body { body.push(byte); continue }
                xs += u64::from(byte == b'x');
                window.rotate_left(1);
                window[3] = byte;
                if &window == b"\r\n\r\n" { in_body = true }
            }
        }
        worker.join().unwrap();
        assert_eq!(xs, 8 * 1024 * 1024 + 1, "filename plus application/x-nzb");
        assert_eq!(body, b"<checked/>");
    }

    #[test]
    fn invalid_extra_headers_fail_before_status_and_release_spools() {
        for (name, value) in [("X-Test\r\nInjected", "value"), ("X-Test", "value\r\nInjected: yes")] {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let address = server.server_addr().to_ip().unwrap();
            let root = tempfile::tempdir().unwrap();
            let pool = Pool::new(root.path().to_owned(), Limits { requests: 1, per_request_bytes: 1024, total_bytes: 1024, free_bytes: 0 }).unwrap();
            let worker = std::thread::spawn(move || {
                let request = server.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
                let result = send(request, body(&pool), None, 200, "application/x-nzb", vec![(name.into(), value.into())]);
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
                assert!(pool.open().is_ok(), "failed response released disk/concurrency admission");
            });
            let mut client = TcpStream::connect(address).unwrap();
            client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            client.write_all(b"GET /ui/nzb HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            worker.join().unwrap();
            assert!(response.starts_with("HTTP/1.1 500 "), "tiny_http handles pre-send refusal");
            assert!(!response.contains("Injected"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_tcp_consumer_releases_body_and_writer() {
        use std::os::fd::AsRawFd;
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let root = tempfile::tempdir().unwrap();
        let pool = Pool::new(root.path().to_owned(), Limits { requests: 1, per_request_bytes: 8 * 1024 * 1024, total_bytes: 8 * 1024 * 1024, free_bytes: 0 }).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let request = server.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
            let mut content = pool.open().unwrap();
            io::copy(&mut io::repeat(b'x').take(8 * 1024 * 1024), &mut content).unwrap();
            let content = content.finish().unwrap();
            ready_tx.send(()).unwrap();
            go_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(send(request, content, None, 200, "application/x-nzb", Vec::new()).is_err());
            assert!(pool.open().is_ok());
        });
        let mut client = TcpStream::connect(address).unwrap();
        client.write_all(b"GET /ui/nzb HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let linger = libc::linger { l_onoff: 1, l_linger: 0 };
        // SAFETY: the owned synthetic socket and properly sized linger value
        // are valid for setsockopt. This touches no external service socket.
        let set = unsafe { libc::setsockopt(client.as_raw_fd(), libc::SOL_SOCKET, libc::SO_LINGER, (&linger as *const libc::linger).cast(), std::mem::size_of_val(&linger) as libc::socklen_t) };
        assert_eq!(set, 0);
        drop(client);
        go_tx.send(()).unwrap();
        worker.join().unwrap();
    }
}

