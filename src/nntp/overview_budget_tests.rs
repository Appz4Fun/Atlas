//! Real duplex transport controls. No provider account, PostgreSQL or NXV setup.
use super::*;
use std::io::Write;

async fn reply(body: Vec<u8>, compressed: bool, rows: u64) -> (Result<Vec<Vec<u8>>>, bool) {
    let (client, server) = tokio::io::duplex(4096);
    let mut conn = Conn::over(Box::new(client), Duration::from_secs(2));
    let writer = tokio::spawn(async move {
        let mut server = BufReader::new(server);
        let mut command = String::new();
        server.read_line(&mut command).await.unwrap();
        assert_eq!(command, format!("XOVER 1-{rows}\r\n"));
        let status: &[u8] = if compressed { b"224 [COMPRESS=GZIP]\r\n" } else { b"224 follows\r\n" };
        if server.get_mut().write_all(status).await.is_ok() {
            // Refusal deliberately closes the connection before the oversized body finishes.
            let _ = server.get_mut().write_all(&body).await;
        }
    });
    let result = conn.xover_raw(1, rows).await;
    let poisoned = conn.poisoned;
    drop(conn);
    writer.await.unwrap();
    (result, poisoned)
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}
#[test]
fn oversized_plain_overview_line_is_refused_and_connection_poisoned() {
    runtime().block_on(async {
        let mut body = vec![b'x'; (1 << 20) + 1];
        body.extend_from_slice(b"\r\n.\r\n");
        let (result, poisoned) = reply(body, false, 1).await;
        assert!(result.is_err(), "a provider line over the NXV line cap must not be retained");
        assert!(poisoned);
    });
}
#[test]
fn compressed_expansion_is_bounded_before_collecting_an_overview_line() {
    runtime().block_on(async {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![b'x'; (2 << 20) + 1]).unwrap();
        encoder.write_all(b"\r\n.\r\n").unwrap();
        let (result, poisoned) = reply(encoder.finish().unwrap(), true, 1).await;
        assert!(result.is_err(), "small compressed input must not allocate an unbounded expansion");
        assert!(poisoned);
    });
}
#[test]
fn overview_cannot_return_more_rows_than_requested() {
    runtime().block_on(async {
        let (result, poisoned) = reply(b"1\tx\r\n2\tx\r\n.\r\n".to_vec(), false, 1).await;
        assert!(result.is_err(), "the requested article range bounds retained rows");
        assert!(poisoned);
    });
}
#[test]
fn aggregate_overview_exact_boundary_is_accepted_and_one_byte_over_is_refused() {
    runtime().block_on(async {
        // Half the existing16MiB fetch headroom is a conservative charge:
        // four byte copies plus both raw-vector and parsed-row descriptors.
        let row_overhead = 2 * std::mem::size_of::<Overview>() + 2 * std::mem::size_of::<Vec<u8>>();
        let line_len = ((8 << 20) / 4 - row_overhead) / 4 - 2;
        let extent = 4 * (line_len + 2);
        let mut body = Vec::new();
        for _ in 0..4 {body.extend(std::iter::repeat_n(b'x', line_len));body.extend_from_slice(b"\r\n");}
        let exact = body.clone();
        body.extend_from_slice(b".\r\n");
        let (result, _) = reply(body, false, 4).await;
        assert_eq!(result.unwrap().iter().map(|l|l.len()+2).sum::<usize>(), extent);
        let mut over = exact;over.insert(0,b'x');over.extend_from_slice(b".\r\n");
        let (result, poisoned) = reply(over, false, 4).await;
        assert!(result.is_err());assert!(poisoned);
    });
}

#[test]
fn compressed_valid_rows_and_xzver_expansion_share_the_same_admission() {
    runtime().block_on(async {
        let rows=b"1\tx\r\n2\ty\r\n.\r\n";
        let mut encoder=flate2::write::GzEncoder::new(Vec::new(),flate2::Compression::default());
        encoder.write_all(rows).unwrap();
        let (result,poisoned)=reply(encoder.finish().unwrap(),true,2).await;
        assert_eq!(result.unwrap(),[b"1\tx".to_vec(),b"2\ty".to_vec()]);assert!(!poisoned);
        let mut encoder=flate2::write::ZlibEncoder::new(Vec::new(),flate2::Compression::default());
        encoder.write_all(&vec![b'x';OVERVIEW_TEXT+4]).unwrap();
        // Literal headerless yEnc, using the existing scalar wire convention.
        let encoded=encoder.finish().unwrap().into_iter().flat_map(|b| {
            let b=b.wrapping_add(42);if [0,10,13,61].contains(&b){vec![b'=',b.wrapping_add(64)]}else{vec![b]}
        }).collect::<Vec<_>>();
        assert!(decode_xzver_bounded(&[encoded],1,1).unwrap_err().is_overview_budget());
    });
}

#[test]
fn capped_line_never_accepts_a_newline_beyond_the_callers_article_bound() {
    runtime().block_on(async {
        let (client,mut server)=tokio::io::duplex(4096);
        let writer=tokio::spawn(async move {let _=server.write_all(&[b'x';4096]).await;});
        let mut conn=Conn::over(Box::new(client),Duration::from_secs(1));
        assert!(conn.read_line_capped(17).await.unwrap().is_none());assert!(conn.poisoned);
        drop(conn);writer.await.unwrap();
    });
}

#[test]
fn oversized_overview_frame_is_retried_as_owned_leaves_with_exact_coverage() {
    runtime().block_on(async {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg=UsenetServer::new("127.0.0.1","","",listener.local_addr().unwrap().port());
        cfg.ssl=Some(false);cfg.compress=Some(false);cfg.connections=Some(1);
        let asked=Arc::new(Mutex::new(Vec::new()));let log=asked.clone();
        let peer=tokio::spawn(async move {
            loop {
                let (socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
                if socket.get_mut().write_all(b"200 owned\r\n").await.is_err(){continue}
                loop {
                    let mut command=String::new();if socket.read_line(&mut command).await.unwrap_or(0)==0 {break}
                    if command=="GROUP g\r\n" {if socket.get_mut().write_all(b"211 4 1 4 g\r\n").await.is_err(){break}continue}
                    let range=command.strip_prefix("XOVER ").expect("fixed owned XOVER fixture").trim();
                    let (a,b)=range.split_once('-').unwrap();let (a,b)=(a.parse::<u64>().unwrap(),b.parse::<u64>().unwrap());
                    {let mut log=log.lock().unwrap();assert!(log.len()<8);log.push((a,b));}
                    if socket.get_mut().write_all(b"224 follows\r\n").await.is_err(){break}
                    let mut failed=false;
                    for n in a..=b {
                        let line=format!("{n}\t{}\tp\t\t<m{n}@owned.test>\t\t1\t1\r\n","s".repeat(600_000));
                        if socket.get_mut().write_all(line.as_bytes()).await.is_err(){failed=true;break}
                    }
                    if failed||socket.get_mut().write_all(b".\r\n").await.is_err(){break}
                }
            }
        });
        let pool=Arc::new(Pool::new(&[cfg]).with_max_unsaved(4));
        let mut replies=pool.stream_headers("g",0,vec![(1,4)],1,false,Arc::new(AtomicBool::new(false)));
        let mut ranges=Vec::new();let mut numbers=Vec::new();
        while let Some(slice)=tokio::time::timeout(Duration::from_secs(5),replies.recv()).await.unwrap() {
            ranges.push((slice.start,slice.end));numbers.extend(slice.result.unwrap().into_iter().map(|r|r.number));
            // The original slice's room must already have been released before
            // either smaller leaf can be admitted under max_unsaved4/width1.
            drop(slice.unsaved);
        }
        peer.abort();let _=peer.await;
        assert_eq!(ranges,[(1,2),(3,4)]);assert_eq!(numbers,[1,2,3,4]);
        assert_eq!(*asked.lock().unwrap(),[(1,4),(1,2),(3,4)]);
    });
}
