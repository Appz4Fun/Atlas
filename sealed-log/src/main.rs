//! usage: sealed-log DATA_DIR [HOST:PORT]
//!
//! SEALED_LOG_METRICS is where Prometheus scrapes (default 0.0.0.0:7712, empty for none).
use std::net::TcpListener;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(dir) = args.get(1) else {
        eprintln!("usage: sealed-log DATA_DIR [HOST:PORT]");
        std::process::exit(2);
    };
    let addr = args.get(2).map_or("0.0.0.0:7711", String::as_str);
    let metrics = std::env::var("SEALED_LOG_METRICS").unwrap_or_else(|_| "0.0.0.0:7712".into());
    let log = sealed_log::Log::open(Path::new(dir))?;
    println!("sealed-log on {addr}, data in {dir}: {}", log.stats());
    if !metrics.is_empty() {
        let listener = TcpListener::bind(&metrics)?;
        println!("sealed-log metrics on http://{metrics}/metrics");
        let log = log.clone();
        std::thread::Builder::new()
            .name("sealed-log-metrics".into())
            .spawn(move || sealed_log::metrics::serve(log, listener))?;
    }
    sealed_log::serve(log, TcpListener::bind(addr)?)
}
