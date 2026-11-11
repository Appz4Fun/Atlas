//! Consumer bounds include allocation, source reads, staging and round trips.
#[allow(dead_code)]
mod history_catalog_support;
use atlas::history_identity::Column;
use atlas::history_source::{Manifest, Source, SourceKind};
use atlas::pg::history_import::ImportBudget;
use sha2::{Digest, Sha256};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static LARGEST: AtomicUsize = AtomicUsize::new(0);
fn allocated(bytes: usize) {
    let now = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(now, Ordering::Relaxed);
    LARGEST.fetch_max(bytes, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, old: Layout, new: usize) -> *mut u8 {
        let p = unsafe { System.realloc(p, old, new) };
        if !p.is_null() {
            LIVE.fetch_sub(old.size(), Ordering::Relaxed);
            allocated(new);
        }
        p
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn varint(out: &mut impl Write, mut value: u64) {
    while value >= 128 {
        out.write_all(&[(value as u8) | 128]).unwrap();
        value >>= 7;
    }
    out.write_all(&[value as u8]).unwrap();
}
fn bytes(out: &mut impl Write, value: &[u8]) {
    varint(out, value.len() as u64);
    out.write_all(value).unwrap();
}
fn random(out: &mut impl Write, length: usize) -> [u8; 32] {
    let mut state = 0x9e3779b97f4a7c15u64;
    let mut buffer = [0u8; 65536];
    let mut hash = Sha256::new();
    let mut left = length;
    while left > 0 {
        let n = left.min(buffer.len());
        for chunk in buffer[..n].chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes()[..chunk.len()]);
        }
        out.write_all(&buffer[..n]).unwrap();
        hash.update(&buffer[..n]);
        left -= n;
    }
    hash.finalize().into()
}
fn hash_file(path: &std::path::Path) -> ([u8; 32], u64) {
    let mut file = std::fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut count = 0;
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        count += n as u64;
    }
    (hash.finalize().into(), count)
}
fn phase(name: &str) {
    if let Some(path) = std::env::var_os("ATLAS_HISTORY_MEASUREMENT_MARKER") {
        std::fs::write(path, name).unwrap();
    }
}

#[test]
#[ignore = "run the compiled executable under explicit runtime resource admission"]
fn giant_current_cell_and_frozen_envelope_remain_bounded() {
    const HUGE: usize = 160 * 1024 * 1024;
    const CELL: usize = 9 * 1024 * 1024;
    let files = tempfile::tempdir().unwrap();
    let path = history_catalog_support::manifest(files.path());
    let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let current = manifest.files.iter().find(|f| f.kind == SourceKind::Current).unwrap();
    let mut c = rusqlite::Connection::open(&current.path).unwrap();
    let tx = c.transaction().unwrap();
    tx.execute("update releases set poster=zeroblob(?1) where id=3", [CELL as i64]).unwrap();
    let mut blob = tx.blob_open("main", "releases", "poster", 3, false).unwrap();
    let cell_digest = random(&mut blob, CELL);
    drop(blob);
    tx.commit().unwrap();
    drop(c);
    let packed = files.path().join("original-frame.zst");
    let file = std::fs::File::create(&packed).unwrap();
    let mut encoder = zstd::stream::write::Encoder::new(file, 1).unwrap();
    encoder.window_log(27).unwrap();
    encoder.write_all(&[1]).unwrap();
    varint(&mut encoder, 0);
    varint(&mut encoder, 2);
    bytes(&mut encoder, b"releases");
    varint(&mut encoder, 3);
    for column in [b"id".as_slice(), b"name", b"group_name"] {
        bytes(&mut encoder, column);
    }
    varint(&mut encoder, 1);
    encoder.write_all(&[1]).unwrap();
    varint(&mut encoder, 82);
    encoder.write_all(&[3]).unwrap();
    bytes(&mut encoder, b"Frozen.Title");
    encoder.write_all(&[3]).unwrap();
    bytes(&mut encoder, b"test.group");
    bytes(&mut encoder, b"original_data");
    varint(&mut encoder, 1);
    bytes(&mut encoder, b"original_bytes");
    varint(&mut encoder, 1);
    encoder.write_all(&[4]).unwrap();
    varint(&mut encoder, HUGE as u64);
    random(&mut encoder, HUGE);
    encoder.finish().unwrap().sync_all().unwrap();
    let (packed_digest, packed_length) = hash_file(&packed);
    assert!(packed_length > HUGE as u64 - 1024 * 1024, "incompressible fixture exceeds ordinary multi-page envelopes");
    let frozen = manifest.files.iter().find(|f| f.kind == SourceKind::Frozen).unwrap();
    let mut c = rusqlite::Connection::open(&frozen.path).unwrap();
    let tx = c.transaction().unwrap();
    tx.execute("insert into frozen_releases(id,name,group_name,reason,frozen_at,payload) values(41,'Header.Title','test.group','synthetic',1,zeroblob(?1))",[packed_length as i64]).unwrap();
    let mut blob = tx.blob_open("main", "frozen_releases", "payload", 41, false).unwrap();
    std::io::copy(&mut std::fs::File::open(&packed).unwrap(), &mut blob).unwrap();
    drop(blob);
    tx.commit().unwrap();
    drop(c);
    history_catalog_support::refresh(&mut manifest);
    let destination_files = tempfile::tempdir().unwrap();
    let Some((mut target, _)) = history_catalog_support::destination(destination_files.path()) else { return };
    let spool = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    LARGEST.store(0, Ordering::Relaxed);
    phase("consumer");
    let source = Source::open(manifest, 256 * 1024 * 1024).unwrap();
    let verified = source.verified_bytes;
    let mut owner = target.owner(spool.path(), 512 * 1024 * 1024);
    let id = owner.begin(&source).unwrap();
    let budget = ImportBudget::default();
    let mut sequence = 0;
    let mut trips = 0;
    let mut maximum_source = 0;
    let mut maximum_copy = 0;
    let mut maximum_mutations = 0;
    loop {
        let receipt = owner.step(&source, id, sequence, budget).unwrap();
        sequence = receipt.sequence;
        trips += receipt.sql_round_trips;
        assert!(receipt.source_bytes + receipt.decoded_bytes <= budget.source_bytes);
        assert!(receipt.copy_bytes <= budget.copy_bytes);
        assert!(receipt.mutations <= u64::from(budget.mutations));
        assert!(receipt.callback_events <= u64::from(budget.source_rows));
        maximum_source = maximum_source.max(receipt.source_bytes + receipt.decoded_bytes);
        maximum_copy = maximum_copy.max(receipt.copy_bytes);
        maximum_mutations = maximum_mutations.max(receipt.mutations);
        if receipt.complete {
            break;
        }
        assert!(sequence < 10000);
    }
    owner.finish_identity(&source, id).unwrap();
    assert_eq!(source.verified_bytes, verified, "ordinary pages do not repeat initial hashing");
    let io = source.cell_io();
    assert!(io.max_request <= 65536);
    assert!(
        io.bytes <= packed_length + CELL as u64 + 16384,
        "ordinary cells are read linearly, without growing prefixes"
    );
    target.client.batch_execute(&format!("set search_path to \"{}\"", target.name)).unwrap();
    use atlas::pg::history_catalog::{Lookup, LookupPurpose, ReadBudget, Snapshot};
    let mut catalog = Snapshot::open(&mut target.client, id).unwrap();
    let Lookup::Found(current) = catalog.lookup(3, LookupPurpose::Raw, ReadBudget::default()).unwrap() else {
        panic!("current metadata missing")
    };
    let mut cursor = catalog.value_cursor(current.metadata.field(Column::Poster).unwrap()).unwrap();
    let mut hash = Sha256::new();
    loop {
        let r = catalog
            .read_value(&mut cursor, ReadBudget::default(), |bytes| {
                hash.update(bytes);
                Ok(())
            })
            .unwrap();
        if r.complete {
            break;
        }
    }
    assert_eq!(<[u8; 32]>::from(hash.finalize()), cell_digest);
    let Lookup::Found(frozen) = catalog.lookup(41, LookupPurpose::Raw, ReadBudget::default()).unwrap() else {
        panic!("frozen metadata missing")
    };
    let mut cursor = catalog.object_cursor(frozen.payload_object.unwrap()).unwrap();
    let mut hash = Sha256::new();
    let mut seen = 0u64;
    loop {
        let r = catalog
            .read_value(&mut cursor, ReadBudget::default(), |bytes| {
                seen += bytes.len() as u64;
                hash.update(bytes);
                Ok(())
            })
            .unwrap();
        if r.complete {
            break;
        }
    }
    assert_eq!(seen, packed_length);
    assert_eq!(<[u8; 32]>::from(hash.finalize()), packed_digest);
    drop(catalog);
    assert_eq!(std::fs::read_dir(spool.path()).unwrap().count(), 1);
    let rust_peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    let largest = LARGEST.load(Ordering::Relaxed);
    assert!(rust_peak <= 64 * 1024 * 1024, "Rust allocation peak {rust_peak}");
    assert!(largest <= 8 * 1024 * 1024, "one Rust allocation {largest}");
    println!(
        "{}",
        serde_json::json!({"case":"giant_history_consumer","packed_bytes":packed_length,"current_cell_bytes":CELL,"initial_verified_bytes":verified,"pages":sequence,"sql_round_trips":trips,"max_source_and_expanded_bytes":maximum_source,"max_copy_bytes":maximum_copy,"max_mutations":maximum_mutations,"source_cell_bytes":io.bytes,"source_cell_reads":io.read_calls,"source_cursor_opens":io.cursor_opens,"source_max_request":io.max_request,"rust_peak_above_baseline":rust_peak,"largest_rust_allocation":largest})
    );
    phase("complete");
}

// Forward opaque protocol bytes in fixed storage. Only Sync/simple Query
// envelopes are counted; authentication and data contents are never recorded.
fn relay_payload(from: &mut impl Read, to: &mut impl Write, mut remaining: usize) -> std::io::Result<()> {
    let mut buffer = [0u8; 65536];
    while remaining > 0 {
        let n = remaining.min(buffer.len());
        from.read_exact(&mut buffer[..n])?;
        to.write_all(&buffer[..n])?;
        remaining -= n;
    }
    Ok(())
}
fn protocol_proxy(url: &str) -> Option<(String, std::sync::Arc<AtomicUsize>, std::thread::JoinHandle<()>)> {
    use std::net::{Shutdown, TcpListener, TcpStream};
    let config: postgres::Config = url.parse().unwrap();
    let host = if let Some(address) = config.get_hostaddrs().first() {
        address.to_string()
    } else {
        match config.get_hosts().first()? {
            postgres::config::Host::Tcp(host) => host.clone(),
            _ => return None,
        }
    };
    let backend_port = config.get_ports().first().copied().unwrap_or(5432);
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let thread = std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        let mut backend = TcpStream::connect((host.as_str(), backend_port)).unwrap();
        client.set_nodelay(true).unwrap();
        backend.set_nodelay(true).unwrap();
        let mut upstream = backend.try_clone().unwrap();
        let mut downstream = client.try_clone().unwrap();
        let returning = std::thread::spawn(move || {
            let _ = std::io::copy(&mut upstream, &mut downstream);
            let _ = downstream.shutdown(Shutdown::Write);
        });
        let result = (|| -> std::io::Result<()> {
            let mut length = [0u8; 4];
            client.read_exact(&mut length)?;
            backend.write_all(&length)?;
            let n = u32::from_be_bytes(length) as usize;
            assert!((8..=1024 * 1024).contains(&n));
            relay_payload(&mut client, &mut backend, n - 4)?;
            loop {
                let mut header = [0u8; 5];
                match client.read_exact(&mut header) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                    Err(error) => return Err(error),
                }
                let n = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
                assert!((4..=64 * 1024 * 1024).contains(&n));
                if matches!(header[0], b'S' | b'Q') {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                backend.write_all(&header)?;
                relay_payload(&mut client, &mut backend, n - 4)?;
            }
        })();
        let _ = backend.shutdown(Shutdown::Write);
        returning.join().unwrap();
        result.unwrap();
    });
    let quote = |value: &str| format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"));
    let mut proxy = format!("host=127.0.0.1 port={port}");
    if let Some(user) = config.get_user() {
        proxy.push_str(&format!(" user={}", quote(user)));
    }
    if let Some(database) = config.get_dbname() {
        proxy.push_str(&format!(" dbname={}", quote(database)));
    }
    if let Some(password) = config.get_password() {
        proxy.push_str(&format!(" password={}", quote(std::str::from_utf8(password).unwrap())));
    }
    Some((proxy, count, thread))
}
#[test]
fn multi_row_pages_have_exact_constant_protocol_cost_and_indexed_keysets() {
    let source_files = tempfile::tempdir().unwrap();
    let path = history_catalog_support::manifest(source_files.path());
    let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let current = manifest.files.iter().find(|f| f.kind == SourceKind::Current).unwrap();
    let mut c = rusqlite::Connection::open(&current.path).unwrap();
    let tx = c.transaction().unwrap();
    for index in 1..256 {
        tx.execute(
            "insert into releases(id,name,group_name) values(?1,?2,'test.group')",
            rusqlite::params![3 + 2 * index, format!("Posted.{index}")],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let sqlite_plan: String = c
        .query_row("explain query plan select id from releases where id>?1 order by id limit 1", [3], |r| r.get(3))
        .unwrap();
    assert!(sqlite_plan.contains("PRIMARY KEY"));
    drop(c);
    history_catalog_support::refresh(&mut manifest);
    let source = Source::open(manifest, 4 * 1024 * 1024).unwrap();
    let destination_files = tempfile::tempdir().unwrap();
    let Some((mut target, _)) = history_catalog_support::destination(destination_files.path()) else { return };
    let Some((url, counter, proxy)) = protocol_proxy(&target.url) else {
        eprintln!("SKIP protocol envelope counter requires a TCP test endpoint");
        return;
    };
    let spool = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(spool.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut owner = atlas::pg::history_import::ImportOwner::connect(
        atlas::pg::StorageConfig {
            backend: atlas::pg::Backend::Postgres,
            url,
            schema: target.name.clone(),
            max_connections: 1,
            allow_plaintext: true,
        },
        target.expected.clone(),
        spool.path().into(),
        4 * 1024 * 1024,
    )
    .unwrap();
    let id = owner.begin(&source).unwrap();
    let mut sequence = 0;
    let mut measurements = Vec::new();
    loop {
        // Conversion commit drains startup statement closes. Per-page typed
        // unnamed queries need no implicit prepare/close exchanges.
        let before = counter.load(Ordering::SeqCst);
        let receipt = owner.step(&source, id, sequence, ImportBudget::default()).unwrap();
        let observed = counter.load(Ordering::SeqCst) - before;
        assert_eq!(observed as u64, receipt.sql_round_trips, "wire envelope cost at sequence{}", receipt.sequence);
        measurements.push((receipt.source_rows, observed));
        sequence = receipt.sequence;
        if receipt.complete {
            break;
        }
    }
    owner.finish_identity(&source, id).unwrap();
    drop(owner);
    proxy.join().unwrap();
    assert!(
        measurements.iter().any(|(rows, trips)| *rows >= 256 && *trips <= 32),
        "ordinary source rows must share fixed set-based page cost"
    );
    target
        .client
        .batch_execute(&format!("set search_path to \"{}\";analyze history_object_chunks", target.name))
        .unwrap();
    let plans:Vec<String>=target.client.query("explain (analyze,costs off,buffers,timing off) select object_id,ordinal,byte_offset from history_object_chunks where import_id=$1 and (object_id,ordinal)>($2,$3) order by object_id,ordinal limit 32",&[&&id[..],&0i64,&0i64]).unwrap().iter().map(|r|r.get(0)).collect();
    assert!(
        plans.iter().any(|line| line.contains("Index Scan") || line.contains("Index Only Scan")),
        "keyset query must use its matching index: {plans:?}"
    );
    println!(
        "{}",
        serde_json::json!({"case":"multi_row_page_protocol","page_rows_and_round_trips":measurements,"sqlite_keyset_plan":sqlite_plan,"pg_chunk_keyset_plan":plans})
    );
}
