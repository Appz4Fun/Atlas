use ascii::AsciiString;

use std::io::Error as IoError;
use std::io::Result as IoResult;
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};

use std::net::SocketAddr;
use std::str::FromStr;

use crate::common::{HTTPVersion, Method};
use crate::util::RefinedTcpStream;
use crate::util::{SequentialReader, SequentialReaderBuilder, SequentialWriterBuilder};
use crate::Request;

/// A ClientConnection is an object that will store a socket to a client
/// and return Request objects.
pub struct ClientConnection {
    // address of the client
    remote_addr: IoResult<Option<SocketAddr>>,

    // sequence of Readers to the stream, so that the data is not read in
    //  the wrong order
    source: SequentialReaderBuilder<BufReader<RefinedTcpStream>>,

    // sequence of Writers to the stream, to avoid writing response #2 before
    //  response #1
    sink: SequentialWriterBuilder<BufWriter<RefinedTcpStream>>,

    // Reader to read the next header from
    next_header_source: SequentialReader<BufReader<RefinedTcpStream>>,

    // set to true if we know that the previous request is the last one
    no_more_requests: bool,

    // true if the connection goes through SSL
    secure: bool,
    control: Option<crate::bounded::RequestControl>,
    header_bytes: usize,
}

/// Error that can happen when reading a request.
#[derive(Debug)]
enum ReadError {
    WrongRequestLine,
    WrongHeader(HTTPVersion),
    /// the client sent an unrecognized `Expect` header
    ExpectationFailed(HTTPVersion),
    ReadIoError(IoError),
    ProtocolLimit(u16),
}

impl ClientConnection {
    /// Creates a new `ClientConnection` that takes ownership of the `TcpStream`.
    pub fn new(
        write_socket: RefinedTcpStream,
        mut read_socket: RefinedTcpStream,
    ) -> ClientConnection {
        let remote_addr = read_socket.peer_addr();
        let secure = read_socket.secure();
        let control = read_socket.control();

        let mut source = SequentialReaderBuilder::new(BufReader::with_capacity(1024, read_socket));
        let first_header = source.next().unwrap();

        ClientConnection {
            source,
            sink: SequentialWriterBuilder::new(BufWriter::with_capacity(1024, write_socket)),
            remote_addr,
            next_header_source: first_header,
            no_more_requests: false,
            secure,
            control,
            header_bytes: 0,
        }
    }

    /// true if the connection is HTTPS
    pub fn secure(&self) -> bool {
        self.secure
    }

    pub(crate) fn control(&self) -> Option<crate::bounded::RequestControl> {
        self.control.clone()
    }

    /// Reads the next line from self.next_header_source.
    ///
    /// Reads until `CRLF` is reached. The next read will start
    ///  at the first byte of the new line.
    fn read_next_line(&mut self, request_line: bool) -> Result<AsciiString, ReadError> {
        let mut buf = Vec::new();
        let mut prev_byte_was_cr = false;

        loop {
            let byte = self.next_header_source.by_ref().bytes().next();

            let byte = match byte {
                Some(b) => b.map_err(ReadError::ReadIoError)?,
                None => return Err(ReadError::ReadIoError(IoError::new(ErrorKind::ConnectionAborted, "Unexpected EOF"))),
            };
            self.header_bytes = self.header_bytes.saturating_add(1);
            if let Some(control) = &self.control {
                let limits = control.limits();
                if self.header_bytes > limits.max_header_bytes {
                    return Err(ReadError::ProtocolLimit(431));
                }
                if request_line && buf.len() >= limits.max_request_line_bytes {
                    return Err(ReadError::ProtocolLimit(414));
                }
            }

            if byte == b'\n' && prev_byte_was_cr {
                buf.pop(); // removing the '\r'
                return AsciiString::from_ascii(buf)
                    .map_err(|_| ReadError::ReadIoError(IoError::new(ErrorKind::InvalidInput, "Header is not in ASCII")));
            }

            prev_byte_was_cr = byte == b'\r';

            buf.push(byte);
        }
    }

    /// Reads a request from the stream.
    /// Blocks until the header has been read.
    fn read(&mut self) -> Result<Request, ReadError> {
        if let Some(control) = &self.control {
            control.begin().map_err(ReadError::ReadIoError)?;
        }
        self.header_bytes = 0;
        let (method, path, version, headers) = {
            // reading the request line
            let (method, path, version) = {
                let line = self.read_next_line(true)?;

                parse_request_line(
                    line.as_str().trim(), // TODO: remove this conversion
                )?
            };

            // getting all headers
            let headers = {
                let mut headers = Vec::new();
                loop {
                    let line = self.read_next_line(false)?;

                    if line.is_empty() {
                        break;
                    };
                    if let Some(control) = &self.control {
                        if headers.len() >= control.limits().max_header_count {
                            return Err(ReadError::ProtocolLimit(431));
                        }
                    }
                    headers.push(match FromStr::from_str(line.as_str().trim()) {
                        // TODO: remove this conversion
                        Ok(h) => h,
                        _ => return Err(ReadError::WrongHeader(version)),
                    });
                }

                headers
            };

            (method, path, version, headers)
        };
        if self.control.is_some() {
            // A malformed/ambiguous length must not silently become an empty
            // body and let its bytes be interpreted as a pipelined request.
            let mut lengths = headers.iter().filter(|header: &&crate::Header| header.field.equiv("Content-Length"));
            let length = lengths.next();
            if lengths.next().is_some()
                || length.map_or(false, |header| {
                    let value = header.value.as_str();
                    value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
                        || value.parse::<usize>().is_err()
                }) {
                return Err(ReadError::ProtocolLimit(400));
            }
            let mut encodings = headers.iter().filter(|header| header.field.equiv("Transfer-Encoding"));
            if let Some(encoding) = encodings.next() {
                if length.is_some() || encodings.next().is_some()
                    || !encoding.value.as_str().eq_ignore_ascii_case("chunked") {
                    return Err(ReadError::ProtocolLimit(400));
                }
            }
        }

        // building the writer for the request
        let writer = self.sink.next().unwrap();

        // follow-up for next potential request
        let mut data_source = self.source.next().unwrap();
        std::mem::swap(&mut self.next_header_source, &mut data_source);
        let data_source = crate::bounded::BodyWire::new(data_source, self.control.clone());

        // building the next reader
        let request = crate::request::new_request(
            self.secure,
            method,
            path,
            version.clone(),
            headers,
            *self.remote_addr.as_ref().unwrap(),
            data_source,
            writer,
        )
        .map_err(|e| {
            use crate::request;
            match e {
                request::RequestCreationError::CreationIoError(e) => ReadError::ReadIoError(e),
                request::RequestCreationError::ExpectationFailed => {
                    ReadError::ExpectationFailed(version)
                }
            }
        })?;

        // return the request
        Ok(match &self.control {
            Some(control) => request.with_control(control.clone()),
            None => request,
        })
    }
}

impl Iterator for ClientConnection {
    type Item = Request;

    /// Blocks until the next Request is available.
    /// Returns None when no new Requests will come from the client.
    fn next(&mut self) -> Option<Request> {
        use crate::{Response, StatusCode};

        // the client sent a "connection: close" header in this previous request
        //  or is using HTTP 1.0, meaning that no new request will come
        if self.no_more_requests {
            return None;
        }

        loop {
            let rq = match self.read() {
                Err(ReadError::ProtocolLimit(status)) => {
                    let mut writer = self.sink.next().unwrap();
                    Response::new_empty(StatusCode(status))
                        .raw_print(writer.by_ref(), HTTPVersion(1, 1), &[], false, None).ok();
                    writer.flush().ok();
                    if let Some(control) = &self.control { control.close(); }
                    return None;
                }
                Err(ReadError::WrongRequestLine) => {
                    let writer = self.sink.next().unwrap();
                    let response = Response::new_empty(StatusCode(400));
                    response
                        .raw_print(writer, HTTPVersion(1, 1), &[], false, None)
                        .ok();
                    return None; // we don't know where the next request would start,
                                 // se we have to close
                }

                Err(ReadError::WrongHeader(ver)) => {
                    let writer = self.sink.next().unwrap();
                    let response = Response::new_empty(StatusCode(400));
                    response.raw_print(writer, ver, &[], false, None).ok();
                    return None; // we don't know where the next request would start,
                                 // se we have to close
                }

                Err(ReadError::ReadIoError(ref err)) if err.kind() == ErrorKind::TimedOut => {
                    // request timeout
                    let writer = self.sink.next().unwrap();
                    let response = Response::new_empty(StatusCode(408));
                    response
                        .raw_print(writer, HTTPVersion(1, 1), &[], false, None)
                        .ok();
                    return None; // closing the connection
                }

                Err(ReadError::ExpectationFailed(ver)) => {
                    let writer = self.sink.next().unwrap();
                    let response = Response::new_empty(StatusCode(417));
                    response.raw_print(writer, ver, &[], true, None).ok();
                    return None; // TODO: should be recoverable, but needs handling in case of body
                }

                Err(ReadError::ReadIoError(_)) => return None,

                Ok(rq) => rq,
            };

            // checking HTTP version
            if *rq.http_version() > (1, 1) {
                let writer = self.sink.next().unwrap();
                let response = Response::from_string(
                    "This server only supports HTTP versions 1.0 and 1.1".to_owned(),
                )
                .with_status_code(StatusCode(505));
                response
                    .raw_print(writer, HTTPVersion(1, 1), &[], false, None)
                    .ok();
                continue;
            }

            // updating the status of the connection
            let connection_header = rq
                .headers()
                .iter()
                .find(|h| h.field.equiv("Connection"))
                .map(|h| h.value.as_str());

            let lowercase = connection_header.map(|h| h.to_ascii_lowercase());

            match lowercase {
                Some(ref val) if val.contains("close") => self.no_more_requests = true,
                Some(ref val) if val.contains("upgrade") => self.no_more_requests = true,
                Some(ref val)
                    if !val.contains("keep-alive") && *rq.http_version() == HTTPVersion(1, 0) =>
                {
                    self.no_more_requests = true
                }
                None if *rq.http_version() == HTTPVersion(1, 0) => self.no_more_requests = true,
                _ => (),
            };

            // returning the request
            return Some(rq);
        }
    }
}

/// Parses a "HTTP/1.1" string.
fn parse_http_version(version: &str) -> Result<HTTPVersion, ReadError> {
    let (major, minor) = match version {
        "HTTP/0.9" => (0, 9),
        "HTTP/1.0" => (1, 0),
        "HTTP/1.1" => (1, 1),
        "HTTP/2.0" => (2, 0),
        "HTTP/3.0" => (3, 0),
        _ => return Err(ReadError::WrongRequestLine),
    };

    Ok(HTTPVersion(major, minor))
}

/// Parses the request line of the request.
/// eg. GET / HTTP/1.1
fn parse_request_line(line: &str) -> Result<(Method, String, HTTPVersion), ReadError> {
    let mut parts = line.split(' ');

    let method = parts.next().and_then(|w| w.parse().ok());
    let path = parts.next().map(ToOwned::to_owned);
    let version = parts.next().and_then(|w| parse_http_version(w).ok());

    method
        .and_then(|method| Some((method, path?, version?)))
        .ok_or(ReadError::WrongRequestLine)
}

#[cfg(test)]
mod test {
    #[test]
    fn test_parse_request_line() {
        let (method, path, ver) = super::parse_request_line("GET /hello HTTP/1.1").unwrap();

        assert!(method == crate::Method::Get);
        assert!(path == "/hello");
        assert!(ver == crate::common::HTTPVersion(1, 1));

        assert!(super::parse_request_line("GET /hello").is_err());
        assert!(super::parse_request_line("qsd qsd qsd").is_err());
    }
}
