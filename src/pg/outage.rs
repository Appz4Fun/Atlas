//! Native attempts may replace a lost session, but never replay an old session's work.
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Unavailable,
    Fatal,
}

pub(crate) enum AttemptOutcome {
    Stopped,
    Unavailable,
    Fatal,
}
impl AttemptOutcome {
    pub(crate) fn from_exit(code: i32, failure: Option<Failure>) -> Self {
        match (code, failure) {
            (_, Some(Failure::Fatal)) => Self::Fatal,
            (_, Some(Failure::Unavailable)) => Self::Unavailable,
            (0, None) => Self::Stopped,
            _ => Self::Fatal,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SessionUnavailable;
impl std::fmt::Display for SessionUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native PostgreSQL session unavailable")
    }
}
impl std::error::Error for SessionUnavailable {}

/// A queued callback rejected by an already-canceled/closed owner did no work.
/// It must not hide a separate storage/invariant failure produced while draining.
#[derive(Debug)]
pub(crate) struct AttemptEnded;
impl std::fmt::Display for AttemptEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native attempt has ended")
    }
}
impl std::error::Error for AttemptEnded {}

pub(crate) fn is_attempt_ended(error: &anyhow::Error) -> bool {
    error.chain().any(|source| source.is::<AttemptEnded>())
}

/// Only a PostgreSQL driver's I/O provenance permits a transport retry. An NXV
/// file error with the same OS kind must remain fatal.
pub(crate) fn classify(error: &anyhow::Error) -> Failure {
    for source in error.chain() {
        if source.is::<SessionUnavailable>() || source.is::<AttemptEnded>() {
            return Failure::Unavailable;
        }
        if let Some(pg) = source.downcast_ref::<postgres::Error>() {
            if let Some(code) = pg.code() {
                return if (code.code().starts_with("08") && *code != postgres::error::SqlState::PROTOCOL_VIOLATION)
                    || matches!(code.code(), "57P01" | "57P02" | "57P03" | "57P05" | "53300")
                {
                    Failure::Unavailable
                } else {
                    Failure::Fatal
                };
            }
            if pg.is_closed() {
                return Failure::Unavailable;
            }
            let mut nested = std::error::Error::source(pg);
            while let Some(source) = nested {
                if let Some(io) = source.downcast_ref::<std::io::Error>() {
                    use std::io::ErrorKind::*;
                    return if matches!(
                        io.kind(),
                        ConnectionRefused
                            | ConnectionReset
                            | ConnectionAborted
                            | BrokenPipe
                            | TimedOut
                            | NotConnected
                            | UnexpectedEof
                            | NotFound
                    ) {
                        Failure::Unavailable
                    } else {
                        Failure::Fatal
                    };
                }
                nested = source.source();
            }
            return Failure::Fatal;
        }
    }
    Failure::Fatal
}

pub(crate) struct Backoff(Duration);
impl Default for Backoff {
    fn default() -> Self {
        Self(Duration::from_secs(1))
    }
}
impl Backoff {
    pub(crate) fn next(&mut self) -> Duration {
        let value = self.0;
        self.0 = (value * 2).min(Duration::from_secs(30));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attempt_end_provenance_is_typed_not_a_matching_message() {
        let canceled = anyhow::Error::new(AttemptEnded).context("queued owner callback");
        assert!(is_attempt_ended(&canceled));
        assert_eq!(classify(&canceled), Failure::Unavailable);
        let independent = anyhow::anyhow!("native attempt has ended");
        assert!(!is_attempt_ended(&independent));
        assert_eq!(classify(&independent), Failure::Fatal);
    }
    #[test]
    fn filesystem_errors_are_fatal_even_with_nested_context() {
        for kind in [std::io::ErrorKind::NotFound, std::io::ErrorKind::TimedOut, std::io::ErrorKind::ConnectionReset] {
            let error = anyhow::Error::new(std::io::Error::from(kind)).context("NXV storage operation");
            assert_eq!(classify(&error), Failure::Fatal);
        }
        assert_eq!(classify(&anyhow::Error::new(SessionUnavailable).context("owner operation")), Failure::Unavailable);
    }
    #[test]
    fn retry_schedule_is_bounded() {
        let mut backoff = Backoff::default();
        assert_eq!((0..9).map(|_| backoff.next().as_secs()).collect::<Vec<_>>(), [1, 2, 4, 8, 16, 30, 30, 30, 30]);
    }

    #[test]
    fn nested_driver_io_is_transient_but_storage_io_is_not() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        // Exercise driver transport provenance inside the same loopback-only
        // sandbox as the LIB suite, without requiring a host Unix socket.
        let peer = std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                        // Leave the rest of the startup packet unread so closing
                        // the owned peer produces a transport reset, not SQL.
                        stream.read_exact(&mut [0]).unwrap();
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < until, "owned driver peer was not connected");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("owned driver peer failed: {error}"),
                }
            }
        });
        let mut config = postgres::Config::new();
        config.host("127.0.0.1").port(address.port()).ssl_mode(postgres::config::SslMode::Disable)
            .dbname("example").user("example").connect_timeout(Duration::from_secs(1));
        let error = config.connect(postgres::NoTls).err();
        peer.join().unwrap();
        let error = error.expect("owned peer closes before PostgreSQL startup");
        assert!(std::error::Error::source(&error).is_some_and(|source| source.is::<std::io::Error>()));
        let wrapped = rusqlite::Error::UserFunctionError(Box::new(crate::pg::sql::Error::Postgres(error)));
        assert_eq!(classify(&anyhow::Error::new(wrapped).context("native SQL callback")), Failure::Unavailable);
        let file = std::io::Error::from(std::io::ErrorKind::NotFound);
        let wrapped = rusqlite::Error::UserFunctionError(Box::new(file));
        assert_eq!(classify(&anyhow::Error::new(wrapped).context("NXV callback")), Failure::Fatal);
    }

    #[test]
    fn sql_data_and_authentication_classes_are_fatal() {
        let Some(schema) = super::super::testing::Schema::new() else { return };
        let mut client = schema.pool.get().unwrap();
        let error = client.simple_query("select cast('invalid' as bigint)").unwrap_err();
        assert_eq!(classify(&anyhow::Error::new(error).context("native query")), Failure::Fatal);
        for code in ["08P01", "28P01", "XX001"] {
            let query = format!("do $$ begin raise sqlstate '{code}'; end $$");
            let error = client.simple_query(&query).unwrap_err();
            assert_eq!(classify(&anyhow::Error::new(error).context("native query")), Failure::Fatal, "{code}");
        }
    }
}
