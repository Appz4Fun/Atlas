//! The `/metrics` listener: its own port, soo it can be reached without the
//! api and left off the internet while the api is on it.

use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::Ordering::Relaxed;
use std::thread;
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tiny_http::{Header, Method, Response, Server};

use super::names::*;
use crate::config::Config;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Indexer,
    Api,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Indexer => "indexer",
            Role::Api => "api",
        }
    }
}

/// how long to wait before asking for a held port again
const BIND_RETRY: Duration = Duration::from_secs(5);

static SYSTEM: Mutex<Option<System>> = Mutex::new(None);

fn refresh_process() {
    let pid = Pid::from_u32(std::process::id());
    let mut system = SYSTEM.lock().unwrap();
    let system = system.get_or_insert_with(System::new);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory().with_disk_usage(),
    );
    let Some(p) = system.process(pid) else { return };
    PROCESS_CPU.set_total(&[], p.accumulated_cpu_time());
    PROCESS_RSS.set(&[], p.memory() as f64);
    let disk = p.disk_usage();
    PROCESS_DISK.set_total(&[("direction", "read")], disk.total_read_bytes);
    PROCESS_DISK.set_total(&[("direction", "written")], disk.total_written_bytes);
}

/// What a scrape gets.
pub fn scrape() -> String {
    let started = Instant::now();
    refresh_process();
    let body = super::render();
    SCRAPE_SECONDS.set(&[], started.elapsed().as_secs_f64());
    body
}

/// Serve on `addr` until the process ends. The port it got, or why it got none.
pub fn serve(addr: &str, role: Role) -> Result<SocketAddr, String> {
    let server = Server::http(addr).map_err(|e| e.to_string())?;
    let bound = server.server_addr().to_ip().ok_or("not an ip address")?;
    BUILD_INFO.set(&[("version", env!("CARGO_PKG_VERSION")), ("role", role.name())], 1.0);
    let now = chrono::Utc::now().timestamp();
    super::STARTED_AT.store(now, Relaxed);
    PROCESS_START.set(&[], now as f64);
    let text = Header::from_bytes("Content-Type", "text/plain; version=0.0.4; charset=utf-8").expect("a valid header");
    thread::Builder::new()
        .name("atlas-metrics".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let is_get = matches!(request.method(), Method::Get | Method::Head);
                let path = request.url().split('?').next().unwrap_or("").to_string();
                let response = match (is_get, path.as_str()) {
                    (false, _) => Response::from_string("").with_status_code(405),
                    (true, "/metrics") => Response::from_string(scrape()).with_header(text.clone()),
                    (true, "/healthz") => Response::from_string("ok\n").with_header(text.clone()),
                    (true, _) => Response::from_string("").with_status_code(404),
                };
                let _ = request.respond(response);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(bound)
}

/// Start the listener this process's role has, when metrics are on.
pub fn start(config: &Config, role: Role) {
    let m = &config.metrics;
    if !m.enabled {
        return;
    }
    let port = match role {
        Role::Indexer => m.indexer_port,
        Role::Api => m.api_port,
    };
    let host = if m.host.contains(':') && !m.host.starts_with('[') { format!("[{}]", m.host) } else { m.host.clone() };
    let addr = format!("{host}:{port}");
    serve_when_free(addr, role, BIND_RETRY);
}

/// Serve on `addr`, going on trying while something holds the port: a restart
/// can find its old process still on its way out, and a scrape that never comes
/// back until the next restart is what this is here to prevent.
pub fn serve_when_free(addr: String, role: Role, retry: Duration) {
    let e = match serve(&addr, role) {
        Ok(_) => return println!("metrics on http://{addr}/metrics"),
        Err(e) => e,
    };
    // nothing else depends on it: indexing goes on without
    crate::ui::warn(&format!("metrics not started on {addr} yet: {e}"));
    let _ = thread::Builder::new().name("atlas-metrics-bind".into()).spawn(move || {
        while serve(&addr, role).is_err() {
            thread::sleep(retry);
        }
    });
}
