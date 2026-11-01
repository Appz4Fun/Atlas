//! A loopback HTTP server answering from fixtures: no test reaches a real provider.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Route {
    pub path: &'static str,
    pub status: u16,
    pub body: String,
    pub headers: Vec<(&'static str, String)>,
}

pub fn route(path: &'static str, body: String) -> Route {
    Route { path, status: 200, body, headers: Vec::new() }
}

pub fn status(path: &'static str, status: u16) -> Route {
    Route { path, status, body: String::new(), headers: Vec::new() }
}

/// a request's "path?query" and its headers
type Seen = (String, Vec<(String, String)>);

#[derive(Default)]
struct Shared {
    routes: Mutex<Vec<Route>>,
    requests: Mutex<Vec<Seen>>,
    stop: AtomicBool,
    /// answers wait while set
    held: AtomicBool,
    waiting: AtomicUsize,
}

pub struct FixtureServer {
    pub base: String,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FixtureServer {
    /// The first route whose path is a prefix of the request's answers it; otherwise 404.
    pub fn start(routes: Vec<Route>) -> FixtureServer {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let shared = Arc::new(Shared { routes: Mutex::new(routes), ..Shared::default() });
        let on_thread = shared.clone();
        let thread = std::thread::spawn(move || {
            while !on_thread.stop.load(Relaxed) {
                let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else { continue };
                let url = request.url().to_string();
                let headers = request.headers().iter().map(|h| (h.field.to_string(), h.value.to_string())).collect();
                on_thread.requests.lock().unwrap().push((url.clone(), headers));
                on_thread.waiting.fetch_add(1, Relaxed);
                while on_thread.held.load(Relaxed) && !on_thread.stop.load(Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                on_thread.waiting.fetch_sub(1, Relaxed);
                let path = url.split('?').next().unwrap_or_default();
                let found = on_thread.routes.lock().unwrap().iter().find(|r| path.starts_with(r.path)).cloned();
                let found = found.unwrap_or(Route { path: "", status: 404, body: String::new(), headers: Vec::new() });
                let mut response = tiny_http::Response::from_string(found.body).with_status_code(found.status);
                for (name, value) in &found.headers {
                    response.add_header(tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap());
                }
                let _ = request.respond(response);
            }
        });
        FixtureServer { base: format!("http://127.0.0.1:{port}"), shared, thread: Some(thread) }
    }

    /// "path?query", in order.
    pub fn requests(&self) -> Vec<String> {
        self.shared.requests.lock().unwrap().iter().map(|(url, _)| url.clone()).collect()
    }

    pub fn headers_of(&self, i: usize) -> Vec<(String, String)> {
        self.shared.requests.lock().unwrap()[i].1.clone()
    }

    pub fn set_routes(&self, routes: Vec<Route>) {
        *self.shared.routes.lock().unwrap() = routes;
    }

    /// Answers wait until `release`.
    pub fn hold(&self) {
        self.shared.held.store(true, Relaxed);
    }

    pub fn release(&self) {
        self.shared.held.store(false, Relaxed);
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// tests/fixtures/media/{name}
pub fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/fixtures/media/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}
