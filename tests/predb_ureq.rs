//! The one real fetcher, on loopback only. Its own binary: every other test asserts it made no request.

use std::time::Duration;

use atlas::predb::poll::{Fetch, UreqFetch};

fn server() -> (tiny_http::Server, u16) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    (server, port)
}

#[test]
fn ureq_fetch_reports_status_and_headers() {
    let (server, port) = server();
    let fetch = UreqFetch::new(Duration::from_secs(5));
    let thread = std::thread::spawn(move || {
        let request = server.recv().unwrap();
        let header = tiny_http::Header::from_bytes(&b"Retry-After"[..], &b"120"[..]).unwrap();
        request
            .respond(tiny_http::Response::from_string("slow down").with_status_code(429).with_header(header))
            .unwrap();
        let request = server.recv().unwrap();
        request.respond(tiny_http::Response::from_data(vec![b'x'; 9 << 20])).unwrap();
    });
    let reply = fetch.get(&format!("http://127.0.0.1:{port}/a")).unwrap();
    assert_eq!((reply.status, reply.retry_after.as_deref(), reply.body.as_str()), (429, Some("120"), "slow down"));
    assert!(fetch.get(&format!("http://127.0.0.1:{port}/b")).is_err(), "over the body cap");
    thread.join().unwrap();
}

#[test]
fn a_redirect_isnt_followed() {
    let (second, second_port) = server();
    let (first, port) = server();
    let thread = std::thread::spawn(move || {
        let request = first.recv().unwrap();
        let to = format!("http://127.0.0.1:{second_port}/");
        let header = tiny_http::Header::from_bytes(&b"Location"[..], to.as_bytes()).unwrap();
        request.respond(tiny_http::Response::empty(302).with_header(header)).unwrap();
    });
    let reply = UreqFetch::new(Duration::from_secs(5)).get(&format!("http://127.0.0.1:{port}/")).unwrap();
    assert_eq!(reply.status, 302);
    thread.join().unwrap();
    assert!(second.recv_timeout(Duration::from_millis(200)).unwrap().is_none());
}
