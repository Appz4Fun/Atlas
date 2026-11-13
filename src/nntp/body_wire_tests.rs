//! Owned-loopback transport boundaries; no configured provider participates.
use super::*;
use tokio::net::TcpListener;

async fn response(wire: Vec<u8>) -> (Conn, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let mut command = String::new();
        socket.read_line(&mut command).await.unwrap();
        assert!(command.starts_with("BODY ") || command.starts_with("ARTICLE ") || command.starts_with("HEAD "));
        let _ = tokio::time::timeout(Duration::from_secs(15), socket.get_mut().write_all(&wire)).await;
    });
    let socket = TcpStream::connect(address).await.unwrap();
    (Conn::over(Box::new(socket), Duration::from_secs(15)), server)
}
fn run(future: impl Future<Output = ()>) {
    Runtime::new().unwrap().block_on(future);
}

#[test]
fn capped_body_admits_joined_output_before_return_and_poisons_overflow() {
    run(async {
        let (mut conn, server) = response(b"222 body\r\nabcdefghijklmnop\r\nqrstuvwxyzABCDEF\r\n.\r\n".to_vec()).await;
        assert_eq!(conn.body_capped("<owned@test>", 32).await.unwrap(), None);
        assert!(conn.poisoned, "an over-budget multiline response cannot reenter the pool");
        drop(conn);
        server.await.unwrap();
    });
}
#[test]
fn body_and_article_prefixes_do_not_return_an_over_budget_whole_line() {
    run(async {
        let (mut body, server) = response(b"222 body\r\nabc\r\ndefghij\r\n.\r\n".to_vec()).await;
        let (bytes, whole) = body.body_prefix("<owned@test>", 8).await.unwrap();
        assert_eq!(bytes, b"abc");
        assert!(!whole);
        assert!(body.poisoned);
        drop(body);
        server.await.unwrap();
        let (mut article, server) = response(b"220 article\r\nabc\r\ndefghij\r\n.\r\n".to_vec()).await;
        let (bytes, whole) = article.article_raw_prefix("<owned@test>", 8).await.unwrap();
        assert_eq!(bytes, b"abc\n");
        assert!(!whole);
        assert!(article.poisoned);
        drop(article);
        server.await.unwrap();
    });
}
#[test]
fn exact_body_and_raw_boundaries_preserve_dot_stuffing_and_whole_state() {
    run(async {
        let (mut body, server) = response(b"222 body\r\n..abcdef\r\n.\r\n".to_vec()).await;
        assert_eq!(body.body_capped("<owned@test>", 7).await.unwrap(), Some(b".abcdef".to_vec()));
        assert!(!body.poisoned);
        drop(body);
        server.await.unwrap();
        let (mut article, server) = response(b"220 article\r\n..abcdef\r\n.\r\n".to_vec()).await;
        assert_eq!(article.article_raw("<owned@test>", 8).await.unwrap(), Some(b".abcdef\n".to_vec()));
        assert!(!article.poisoned);
        drop(article);
        server.await.unwrap();
    });
}
#[test]
fn head_aggregate_overflow_is_not_a_reusable_response() {
    run(async {
        let (mut conn, server) = response(b"221 head\r\n12345678\r\n.\r\n".to_vec()).await;
        assert_eq!(conn.head("<owned@test>", 8).await.unwrap(), None);
        assert!(conn.poisoned);
        drop(conn);
        server.await.unwrap();
    });
}
#[test]
fn full_body_refuses_oversized_plain_line_and_multiline_aggregate() {
    run(async {
        let limit = crate::config::PostprocessConfig::default().body_max_bytes as usize;
        let mut row = vec![b'x'; 4096];
        row.extend(b"\r\n");
        for chunk in [vec![b'x'; limit + 1], row.repeat(limit / 4096 + 1)] {
            let mut wire = b"222 body\r\n".to_vec();
            wire.extend(chunk);
            wire.extend(b"\r\n.\r\n");
            let (mut conn, server) = response(wire).await;
            assert!(matches!(conn.body("<owned@test>").await, Err(NntpError::Protocol(_))));
            assert!(conn.poisoned);
            drop(conn);
            server.await.unwrap();
        }
    });
}
#[test]
fn full_body_bounds_compressed_expansion_before_retaining_it() {
    use std::io::Write;
    run(async {
        let limit = crate::config::PostprocessConfig::default().body_max_bytes as usize;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        for _ in 0..=limit / 4096 {
            encoder.write_all(&[b'x'; 4096]).unwrap();
        }
        encoder.write_all(b"\r\n.\r\n").unwrap();
        let mut wire = b"222 [COMPRESS=GZIP]\r\n".to_vec();
        wire.extend(encoder.finish().unwrap());
        let (mut conn, server) = response(wire).await;
        assert!(conn.body("<owned@test>").await.is_err());
        assert!(conn.poisoned);
        drop(conn);
        server.await.unwrap();
    });
}
#[test]
fn forged_yenc_declared_size_never_controls_output_capacity() {
    run(async {
        let lines = [
            b"ignored preamble".to_vec(),
            b"=ybegin line=128 size=18446744073709551615 name=x".to_vec(),
            b"=ypart begin=1 end=18446744073709551615".to_vec(),
            b"klm=".to_vec(),
            b"=yend size=18446744073709551615".to_vec(),
            b"ignored suffix".to_vec(),
        ];
        let expected = yenc_decode(&lines).unwrap();
        let mut wire = b"222 body\r\n".to_vec();
        for line in &lines {
            wire.extend(line);
            wire.extend(b"\r\n");
        }
        wire.extend(b".\r\n");
        let (mut conn, server) = response(wire).await;
        let actual = conn.body("<owned@test>").await.unwrap();
        assert_eq!(actual, expected);
        assert!(actual.capacity() < 4096);
        assert!(!conn.poisoned);
        drop(conn);
        server.await.unwrap();
    });
}

