//! usage: sealed-log DATA_DIR [HOST:PORT]
use std::net::TcpListener;
use std::path::Path;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(dir) = args.get(1) else {
        eprintln!("usage: sealed-log DATA_DIR [HOST:PORT]");
        std::process::exit(2);
    };
    let addr = args.get(2).map_or("0.0.0.0:7711", String::as_str);
    let log = sealed_log::Log::open(Path::new(dir))?;
    println!("sealed-log on {addr}, data in {dir}: {}", log.stats());
    sealed_log::serve(log, TcpListener::bind(addr)?)
}
