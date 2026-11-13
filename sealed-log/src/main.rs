//! Standalone authenticated storage. TLS material is loaded before the log
//! directory or listeners are opened; no plaintext compatibility mode exists.
use sealed_log::{CertificateGrant, ServerTls, TransportLimits};
use std::{io, net::TcpListener, path::PathBuf, sync::Arc};

struct Options {
    dir: PathBuf,
    listen: String,
    metrics: String,
    cert: PathBuf,
    key: PathBuf,
    client_ca: PathBuf,
    grants: Vec<CertificateGrant>,
}
const USAGE: &str = "usage: sealed-log DATA_DIR [--listen HOST:PORT] [--metrics HOST:PORT]\n\
    --cert SERVER_CHAIN.pem --key SERVER_KEY.pem --client-ca CLIENT_CA.pem\n\
    --grant SHA256:READ,APPEND,STATS [--grant SHA256:READ ...]\n\
    Defaults: data 127.0.0.1:7711, metrics 127.0.0.1:7712. Empty --metrics disables scrapes.\n\
    Certificate fingerprints identify exact CA-verified clients. All connections use mTLS.";

fn input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn options(args: &[String], metrics_env: Option<&str>) -> io::Result<Options> {
    let dir = args.first().filter(|s| !s.is_empty() && !s.starts_with('-')).ok_or_else(|| input(USAGE))?;
    let (mut cert, mut key, mut ca) = (None, None, None);
    let mut grants = Vec::new();
    let mut listen = None;
    let mut metrics = None;
    let mut at = 1;
    while at < args.len() {
        let flag = args[at].as_str();
        let value = args.get(at + 1).ok_or_else(|| input("option requires a value"))?;
        at += 2;
        match flag {
            "--listen" if listen.is_none() => listen = Some(value.clone()),
            "--metrics" if metrics.is_none() => metrics = Some(value.clone()),
            "--cert" if cert.is_none() => cert = Some(PathBuf::from(value)),
            "--key" if key.is_none() => key = Some(PathBuf::from(value)),
            "--client-ca" if ca.is_none() => ca = Some(PathBuf::from(value)),
            "--grant" => grants.push(value.parse()?),
            _ => return Err(input("unknown or duplicate option")),
        }
    }
    if grants.is_empty() {
        return Err(input("at least one --grant is required"));
    }
    Ok(Options {
        dir: dir.into(),
        listen: listen.unwrap_or_else(|| "127.0.0.1:7711".into()),
        metrics: metrics.unwrap_or_else(|| metrics_env.unwrap_or("127.0.0.1:7712").into()),
        cert: cert.ok_or_else(|| input("--cert is required"))?,
        key: key.ok_or_else(|| input("--key is required"))?,
        client_ca: ca.ok_or_else(|| input("--client-ca is required"))?,
        grants,
    })
}

fn main() -> io::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let metrics_env = std::env::var("SEALED_LOG_METRICS").ok();
    let opts = options(&args, metrics_env.as_deref())?;
    let tls = Arc::new(ServerTls::from_pem_files(
        &opts.cert,
        &opts.key,
        &opts.client_ca,
        &opts.grants,
        TransportLimits::default(),
    )?);
    let data = TcpListener::bind(&opts.listen)?;
    let metrics = if opts.metrics.is_empty() { None } else { Some(TcpListener::bind(&opts.metrics)?) };
    let log = sealed_log::Log::open(&opts.dir)?;
    if let Some(listener) = metrics {
        let (log, tls) = (log.clone(), tls.clone());
        std::thread::Builder::new().name("sealed-log-metrics".into()).spawn(move || {
            if let Err(error) = sealed_log::metrics::serve(log, listener, tls) {
                eprintln!("sealed-log metrics listener stopped: {error}");
            }
        })?;
    }
    sealed_log::serve(log, data, tls)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args() -> Vec<String> {
        [
            "fixture",
            "--cert",
            "server.pem",
            "--key",
            "key.pem",
            "--client-ca",
            "ca.pem",
            "--grant",
            "0000000000000000000000000000000000000000000000000000000000000000:READ",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
    #[test]
    fn secure_defaults_are_private_and_missing_credentials_are_refused() {
        let opts = options(&args(), None).unwrap();
        assert_eq!(opts.listen, "127.0.0.1:7711");
        assert_eq!(opts.metrics, "127.0.0.1:7712");
        assert!(options(&["fixture".into()], None).is_err());
        for flag in ["--cert", "--key", "--client-ca"] {
            let mut missing = args();
            let at = missing.iter().position(|s| s == flag).unwrap();
            missing.drain(at..at + 2);
            assert!(options(&missing, None).is_err(), "{flag}");
        }
    }
    #[test]
    fn explicit_metrics_selection_is_preserved_without_changing_data_default() {
        let mut selected = args();
        selected.extend(["--metrics".into(), String::new()]);
        let opts = options(&selected, Some("127.0.0.1:9")).unwrap();
        assert!(opts.metrics.is_empty());
        assert_eq!(opts.listen, "127.0.0.1:7711");
        assert_eq!(options(&args(), Some("127.0.0.1:9")).unwrap().metrics, "127.0.0.1:9");
    }
}
