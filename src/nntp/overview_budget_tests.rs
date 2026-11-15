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

#[test]
fn paged_xzver_continuations_keep_one_fetch_budget_and_retry_exact_leaves() {
    runtime().block_on(async {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg=UsenetServer::new("127.0.0.1","","",listener.local_addr().unwrap().port());
        cfg.ssl=Some(false);cfg.compress=Some(true);cfg.connections=Some(1);
        let asked=Arc::new(Mutex::new(Vec::new()));let log=asked.clone();
        let peer=tokio::spawn(async move {
            loop {
                let (socket,_)=listener.accept().await.unwrap();let mut socket=BufReader::new(socket);
                if socket.get_mut().write_all(b"200 owned\r\n").await.is_err(){continue}
                loop {
                    let mut command=String::new();if socket.read_line(&mut command).await.unwrap_or(0)==0 {break}
                    let response=if command=="GROUP g\r\n" {b"211 4 1 4 g\r\n".to_vec()}
                    else if command.starts_with("XFEATURE "){b"500 unavailable\r\n".to_vec()}
                    else if let Some(range)=command.strip_prefix("XZVER ") {
                        let (a,b)=range.trim().split_once('-').unwrap();let (a,b)=(a.parse::<u64>().unwrap(),b.parse::<u64>().unwrap());
                        {let mut log=log.lock().unwrap();assert!(log.len()<16);log.push((a,b));}
                        // A genuine provider-style truncated compressed page:
                        // every separately bounded answer returns just one row.
                        let mut compressor=flate2::write::ZlibEncoder::new(Vec::new(),flate2::Compression::default());
                        compressor.write_all(format!("{a}\t{}\tp\tdate\t<m{a}@owned.test>\t\t1\t1\r\n","s".repeat(600_000)).as_bytes()).unwrap();
                        let mut out=b"224 follows\r\n".to_vec();
                        for b in compressor.finish().unwrap(){let b=b.wrapping_add(42);if [0,10,13,61].contains(&b){out.extend_from_slice(&[b'=',b.wrapping_add(64)])}else{out.push(b)}}
                        out.extend_from_slice(b"\r\n.\r\n");out
                    } else {panic!("unexpected paged XZVER command: {command}")};
                    if socket.get_mut().write_all(&response).await.is_err(){break}
                }
            }
        });
        let pool=Arc::new(Pool::new(&[cfg]).with_max_unsaved(4));
        let error=pool.xover_on(0,"g",1,4).await.unwrap_err();
        assert!(error.is_overview_budget(),"individually admitted pages cannot accumulate beyond one fetch");
        asked.lock().unwrap().clear();
        let mut replies=pool.stream_headers("g",0,vec![(1,4)],1,false,Arc::new(AtomicBool::new(false)));
        let mut ranges=Vec::new();let mut numbers=Vec::new();
        while let Some(slice)=tokio::time::timeout(Duration::from_secs(5),replies.recv()).await.unwrap() {
            ranges.push((slice.start,slice.end));numbers.extend(slice.result.unwrap().into_iter().map(|r|r.number));drop(slice.unsaved);
        }
        peer.abort();let _=peer.await;
        assert_eq!(ranges,[(1,2),(3,4)]);assert_eq!(numbers,[1,2,3,4]);
        assert_eq!(*asked.lock().unwrap(),[(1,4),(2,4),(3,4),(4,4),(1,2),(2,2),(3,4),(4,4)]);
    });
}

/// A single bounded TCP peer owns every socket; aborting it also closes the
/// current connection. No detached connection handlers survive a failed test.
struct DateProbePeer {
    port: u16,
    asked: Arc<Mutex<Vec<(usize, u64, u64, bool)>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for DateProbePeer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl DateProbePeer {
    async fn start(first: u64, last: u64, subject: usize, gzip: bool, empty_code: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        let task = tokio::spawn(async move {
            let serve = async move {
                for connection in 0..128 {
                    let (socket, _) = listener.accept().await.unwrap();
                    let mut socket = BufReader::new(socket);
                    if socket.get_mut().write_all(b"200 date probe\r\n").await.is_err() {
                        continue;
                    }
                    let mut compressed = false;
                    loop {
                        let mut command = String::new();
                        if socket.read_line(&mut command).await.unwrap_or(0) == 0 {
                            break;
                        }
                        if command == "XFEATURE COMPRESS GZIP\r\n" {
                            assert!(gzip, "plain policy must not negotiate compression");
                            compressed = true;
                            if socket.get_mut().write_all(b"290 enabled\r\n").await.is_err() {
                                break;
                            }
                            continue;
                        }
                        if command == "GROUP g\r\n" {
                            let count = last.checked_sub(first).map_or(0, |n| n + 1);
                            let reply = format!("211 {count} {first} {last} g\r\n");
                            if socket.get_mut().write_all(reply.as_bytes()).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        let range = command.strip_prefix("XOVER ").expect("date probe uses XOVER").trim();
                        let (a, b) = range.split_once('-').unwrap();
                        let (a, b) = (a.parse::<u64>().unwrap(), b.parse::<u64>().unwrap());
                        assert!(a <= b);
                        {
                            let mut log = log.lock().unwrap();
                            assert!(log.len() < 256, "date probe must terminate within bounded work");
                            log.push((connection, a, b, compressed));
                        }
                        let (lo, hi) = (a.max(first), b.min(last));
                        if first > last || lo > hi {
                            let reply = format!("{empty_code} no articles\r\n");
                            if socket.get_mut().write_all(reply.as_bytes()).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        let mut body = Vec::new();
                        let mut previous = None;
                        for n in lo..=hi {
                            assert!(a <= n && n <= b && previous.is_none_or(|p| p < n));
                            previous = Some(n);
                            body.extend_from_slice(&date_probe_row(n, n - first + 1, subject));
                        }
                        body.extend_from_slice(b".\r\n");
                        let status: &[u8] = if compressed { b"224 [COMPRESS=GZIP]\r\n" } else { b"224 follows\r\n" };
                        if compressed {
                            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                            encoder.write_all(&body).unwrap();
                            body = encoder.finish().unwrap();
                        }
                        if socket.get_mut().write_all(status).await.is_err()
                            || socket.get_mut().write_all(&body).await.is_err()
                        {
                            break;
                        }
                    }
                }
                panic!("date probe exceeded bounded connection count");
            };
            tokio::time::timeout(Duration::from_secs(20), serve).await.expect("date fixture deadline");
        });
        Self { port, asked, task: Some(task) }
    }

    fn pool(&self, gzip: bool) -> Pool {
        let mut cfg = UsenetServer::new("127.0.0.1", "", "", self.port);
        cfg.ssl = Some(false);
        cfg.compress = Some(gzip);
        cfg.connections = Some(1);
        Pool::new(&[cfg])
    }

    async fn stop(mut self) -> Vec<(usize, u64, u64, bool)> {
        let task = self.task.take().unwrap();
        task.abort();
        if let Err(error) = task.await {
            assert!(error.is_cancelled(), "date fixture failed: {error}");
        }
        let asked = self.asked.lock().unwrap().clone();
        asked
    }
}

fn date_probe_row(number: u64, ordinal: u64, subject: usize) -> Vec<u8> {
    let date = chrono::DateTime::from_timestamp(1_790_935_200 + ordinal as i64, 0).unwrap().to_rfc2822();
    format!("{number}\t{}\tp@example.test\t{date}\t<m{number}@date.test>\t\t100\t1\r\n", "s".repeat(subject))
        .into_bytes()
}

fn assert_dense_date_fixture_exceeds_admission() {
    let mut charge = 0;
    for n in 1..=6400 {
        let line = date_probe_row(n, n, 256);
        assert!(line.len() - 2 < OVERVIEW_LINE);
        let parsed = parse_overview(&line[..line.len() - 2]).unwrap();
        assert_eq!(parsed.number, n);
        assert_eq!(parsed.subject.len(), 256);
        assert!(plausible_post_time(&parsed.date).is_some());
        charge += 4 * line.len() + 2 * std::mem::size_of::<Overview>() + 2 * std::mem::size_of::<Vec<u8>>();
    }
    assert!(charge > OVERVIEW_CHARGE, "6400 valid normal-width rows must exceed the original aggregate charge");
}

async fn dense_date_probe(gzip: bool) {
    assert_dense_date_fixture_exceeds_admission();
    let peer = DateProbePeer::start(1, 6400, 256, gzip, 423).await;
    let pool = peer.pool(gzip);
    let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 1, 6400)).await;
    let downgraded = pool.servers[0].no_compress.load(Ordering::Relaxed);
    drop(pool);
    let asked = peer.stop().await;
    let ends =
        result.expect("bounded date probe").expect("normal dense dates must recover from aggregate admission refusal");
    assert_eq!(
        ends,
        Ends {
            first: Some((1, 1_790_935_201)),
            last: Some((6400, 1_790_941_600)),
            rows: 6400,
            read_to: Some(6400),
            whole: true
        }
    );
    assert_eq!(asked.iter().map(|&(_, a, b, _)| (a, b)).collect::<Vec<_>>(), [(1, 6400), (1, 3200), (3201, 6400)]);
    assert_ne!(asked[0].0, asked[1].0, "the refused connection must be discarded before retry");
    assert!(asked.iter().all(|r| r.3 == gzip));
    assert!(!downgraded, "admission refusal is not a decompression failure");
}

#[test]
fn date_probe_dense_plain_recovers_exact_dates_with_bounded_halves() {
    runtime().block_on(dense_date_probe(false));
}

#[test]
fn date_probe_dense_gzip_recovers_exact_dates_without_policy_downgrade() {
    runtime().block_on(dense_date_probe(true));
}

#[test]
fn date_probe_last_at_recovers_the_actual_last_dense_article() {
    runtime().block_on(async {
        let peer = DateProbePeer::start(1, 6400, 256, false, 423).await;
        let pool = peer.pool(false);
        let result = tokio::time::timeout(Duration::from_secs(10), pool.last_at(0, "g", 1, 6400)).await;
        drop(pool);
        peer.stop().await;
        assert_eq!(result.unwrap().unwrap(), Some((6400, 1_790_941_600)));
    });
}

#[test]
fn date_probe_sparse_first_in_crosses_empty_windows_before_dense_rows() {
    runtime().block_on(async {
        let peer = DateProbePeer::start(10_001, 16_400, 256, false, 423).await;
        let pool = peer.pool(false);
        let result = tokio::time::timeout(Duration::from_secs(10), pool.first_in(0, "g", 1, 16_401)).await;
        drop(pool);
        let asked = peer.stop().await;
        assert_eq!(result.unwrap().unwrap(), Some((10_001, 1_790_935_201)));
        assert_eq!(asked[0].1, 1);
        assert_eq!(asked[0].2, 100);
    });
}

#[test]
fn date_probe_sparse_article_at_finds_the_exact_date_after_a_hole() {
    runtime().block_on(async {
        let peer = DateProbePeer::start(10_001, 16_400, 256, false, 423).await;
        let pool = peer.pool(false);
        let result =
            tokio::time::timeout(Duration::from_secs(10), pool.article_at(0, "g", 1, 16_400, 1_790_938_400)).await;
        drop(pool);
        peer.stop().await;
        assert_eq!(result.unwrap().unwrap(), 13_200);
    });
}

#[test]
fn date_probe_empty_left_half_keeps_the_nonempty_right_half() {
    runtime().block_on(async {
        let peer = DateProbePeer::start(6401, 12_800, 256, false, 423).await;
        let pool = peer.pool(false);
        let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 1, 12_800)).await;
        drop(pool);
        let asked = peer.stop().await;
        assert_eq!(
            result.unwrap().unwrap(),
            Ends {
                first: Some((6401, 1_790_935_201)),
                last: Some((12_800, 1_790_941_600)),
                rows: 6400,
                read_to: Some(12_800),
                whole: true
            }
        );
        assert!(asked.iter().any(|&(_, a, b, _)| (a, b) == (1, 6400)));
    });
}

#[test]
fn date_probe_budget_halves_retain_only_the_original_global_row_prefix() {
    runtime().block_on(async {
        let peer = DateProbePeer::start(1, 25_600, 256, false, 423).await;
        let pool = peer.pool(false);
        let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 1, 25_600)).await;
        drop(pool);
        let asked = peer.stop().await;
        assert_eq!(
            result.unwrap().unwrap(),
            Ends {
                first: Some((1, 1_790_935_201)),
                last: Some((6400, 1_790_941_600)),
                rows: 6400,
                read_to: Some(6400),
                whole: false
            }
        );
        assert!(
            asked.iter().all(|&(_, a, _, _)| a <= 6400),
            "an unread left prefix must stop before querying right-hand ranges"
        );
    });
}

#[test]
fn date_probe_short_partial_prefix_discards_its_connection() {
    runtime().block_on(async {
        for gzip in [false, true] {
            let peer = DateProbePeer::start(1, 12_800, 8, gzip, 423).await;
            let pool = peer.pool(gzip);
            let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 1, 12_800)).await;
            let again = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 12_800, 12_800)).await;
            drop(pool);
            let asked = peer.stop().await;
            assert_eq!(
                result.unwrap().unwrap(),
                Ends {
                    first: Some((1, 1_790_935_201)),
                    last: Some((6400, 1_790_941_600)),
                    rows: 6400,
                    read_to: Some(6400),
                    whole: false
                }
            );
            assert_eq!(again.unwrap().unwrap().first, Some((12_800, 1_790_948_000)));
            assert_eq!(asked.len(), 2);
            assert_ne!(asked[0].0, asked[1].0);
        }
    });
}

#[test]
fn date_probe_single_oversized_row_is_a_terminal_typed_refusal() {
    runtime().block_on(async {
        for gzip in [false, true] {
            let peer = DateProbePeer::start(1, 1, OVERVIEW_LINE + 1, gzip, 423).await;
            let pool = peer.pool(gzip);
            let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 1, 1)).await;
            let downgraded = pool.servers[0].no_compress.load(Ordering::Relaxed);
            drop(pool);
            let asked = peer.stop().await;
            assert!(result.unwrap().unwrap_err().is_overview_budget());
            assert_eq!(asked.len(), 1, "a singleton cannot be subdivided");
            assert!(!downgraded);
        }
    });
}

#[test]
fn date_probe_empty_420_and_423_remain_whole_empty_results() {
    runtime().block_on(async {
        for code in [420, 423] {
            let peer = DateProbePeer::start(2, 1, 256, false, code).await;
            let pool = peer.pool(false);
            let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", 1, 6400)).await;
            drop(pool);
            let asked = peer.stop().await;
            assert_eq!(result.unwrap().unwrap(), Ends { whole: true, ..Ends::default() });
            assert_eq!(asked.len(), 1);
        }
    });
}

#[test]
fn date_probe_budget_split_is_safe_at_the_u64_upper_boundary() {
    runtime().block_on(async {
        let peer = DateProbePeer::start(u64::MAX - 6399, u64::MAX, 256, false, 423).await;
        let pool = peer.pool(false);
        let result = tokio::time::timeout(Duration::from_secs(10), pool.ends(0, "g", u64::MAX - 6399, u64::MAX)).await;
        drop(pool);
        peer.stop().await;
        assert_eq!(
            result.unwrap().unwrap(),
            Ends {
                first: Some((u64::MAX - 6399, 1_790_935_201)),
                last: Some((u64::MAX, 1_790_941_600)),
                rows: 6400,
                read_to: Some(u64::MAX),
                whole: true
            }
        );
    });
}
