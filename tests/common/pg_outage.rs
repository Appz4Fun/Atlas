//! A bounded, owned relay. It never stops the upstream PostgreSQL service.
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Proxy {
    pub port: u16,
    pub online: Arc<AtomicBool>,
    pub blackhole: Arc<AtomicBool>,
    pub suppress_commit: Arc<AtomicBool>,
    pub ambiguous: Arc<AtomicUsize>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
enum Upstream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}
impl Read for Upstream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(out),
            #[cfg(unix)]
            Self::Unix(s) => s.read(out),
        }
    }
}
impl Write for Upstream {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(input),
            #[cfg(unix)]
            Self::Unix(s) => s.write(input),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Upstream {
    fn connect(config: &postgres::Config) -> Self {
        let port = config.get_ports().first().copied().unwrap_or(5432);
        match config.get_hosts().first().expect("explicit test endpoint") {
            postgres::config::Host::Tcp(host) => {
                let address = (host.as_str(), port).to_socket_addrs().unwrap().next().unwrap();
                let stream = TcpStream::connect_timeout(&address, Duration::from_secs(1)).unwrap();
                stream.set_nonblocking(true).unwrap();
                Self::Tcp(stream)
            }
            #[cfg(unix)]
            postgres::config::Host::Unix(path) => {
                let stream = std::os::unix::net::UnixStream::connect(path.join(format!(".s.PGSQL.{port}"))).unwrap();
                stream.set_nonblocking(true).unwrap();
                Self::Unix(stream)
            }
            #[allow(unreachable_patterns)]
            _ => panic!("unsupported owned test transport"),
        }
    }
}
impl Proxy {
    pub fn new(url: &str) -> Self {
        let config: postgres::Config = url.parse().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let online = Arc::new(AtomicBool::new(false));
        let blackhole = Arc::new(AtomicBool::new(false));
        let suppress_commit = Arc::new(AtomicBool::new(false));
        let ambiguous = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (on, hole, suppress, lost, epoch, stopping) = (
            online.clone(),
            blackhole.clone(),
            suppress_commit.clone(),
            ambiguous.clone(),
            generation.clone(),
            stop.clone(),
        );
        let thread = std::thread::spawn(move || {
            let mut workers: Vec<JoinHandle<()>> = Vec::new();
            while !stopping.load(Ordering::Acquire) {
                let mut i = 0;
                while i < workers.len() {
                    if workers[i].is_finished() {
                        workers.swap_remove(i).join().unwrap();
                    } else {
                        i += 1;
                    }
                }
                match listener.accept() {
                    Ok((mut downstream, _)) if on.load(Ordering::Acquire) && workers.len() < 8 => {
                        downstream.set_nonblocking(true).unwrap();
                        let mut upstream = Upstream::connect(&config);
                        let (on, hole, suppress, lost, epoch, stopping) =
                            (on.clone(), hole.clone(), suppress.clone(), lost.clone(), epoch.clone(), stopping.clone());
                        let initial = epoch.load(Ordering::Acquire);
                        workers.push(std::thread::spawn(move || {
                            let mut request = std::collections::VecDeque::new();
                            let mut response = std::collections::VecDeque::new();
                            let mut frontend_tail = Vec::new();
                            let mut backend_tail = Vec::new();
                            let mut release_transaction = false;
                            while !stopping.load(Ordering::Acquire)
                                && on.load(Ordering::Acquire)
                                && epoch.load(Ordering::Acquire) == initial
                            {
                                if hole.load(Ordering::Acquire) {
                                    std::thread::sleep(Duration::from_millis(5));
                                    continue;
                                }
                                let mut buffer = [0; 8192];
                                if request.len() < 32768 {
                                    match downstream.read(&mut buffer) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            frontend_tail.extend_from_slice(&buffer[..n]);
                                            release_transaction |=
                                                frontend_tail.windows(20).any(|w| w == b"insert into segments");
                                            if frontend_tail.len() > 128 {
                                                frontend_tail.drain(..frontend_tail.len() - 128);
                                            }
                                            request.extend(&buffer[..n]);
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                        Err(_) => break,
                                    }
                                }
                                if !request.is_empty() {
                                    match upstream.write(request.make_contiguous()) {
                                        Ok(n) => {
                                            request.drain(..n);
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                        Err(_) => break,
                                    }
                                }
                                if response.len() < 32768 {
                                    match upstream.read(&mut buffer) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            backend_tail.extend_from_slice(&buffer[..n]);
                                            let committed = backend_tail.windows(7).any(|w| w == b"COMMIT\0");
                                            if release_transaction
                                                && committed
                                                && suppress.swap(false, Ordering::AcqRel)
                                            {
                                                lost.fetch_add(1, Ordering::AcqRel);
                                                break;
                                            }
                                            if committed {
                                                release_transaction = false;
                                                backend_tail.clear();
                                            }
                                            if backend_tail.len() > 32 {
                                                backend_tail.drain(..backend_tail.len() - 32);
                                            }
                                            response.extend(&buffer[..n]);
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                        Err(_) => break,
                                    }
                                }
                                if !response.is_empty() {
                                    match downstream.write(response.make_contiguous()) {
                                        Ok(n) => {
                                            response.drain(..n);
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                        Err(_) => break,
                                    }
                                }
                                std::thread::sleep(Duration::from_millis(2));
                            }
                            let _ = downstream.shutdown(Shutdown::Both);
                        }));
                    }
                    Ok((stream, _)) => {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("owned proxy listener failed: {e}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self { port, online, blackhole, suppress_commit, ambiguous, generation, stop, thread: Some(thread) }
    }
    pub fn drop_connections(&self) {
        self.online.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
    pub fn url(&self, original: &str) -> String {
        let config: postgres::Config = original.parse().unwrap();
        let quote = |value: &str| format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
        let mut url = format!("host=127.0.0.1 port={} connect_timeout=1 sslmode=disable", self.port);
        if let Some(value) = config.get_user() {
            url.push_str(&format!(" user={}", quote(value)));
        }
        if let Some(value) = config.get_dbname() {
            url.push_str(&format!(" dbname={}", quote(value)));
        }
        if let Some(value) = config.get_password() {
            url.push_str(&format!(" password={}", quote(std::str::from_utf8(value).unwrap())));
        }
        url
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
