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
#[derive(Default)]
struct ValidationWire {
    extent_rows: AtomicUsize,
    payload_rows: AtomicUsize,
    extent_bytes: AtomicUsize,
    payload_bytes: AtomicUsize,
}
impl ValidationWire {
    fn snapshot(&self) -> (usize,usize,usize) {
        (self.extent_rows.load(Ordering::SeqCst), self.payload_rows.load(Ordering::SeqCst),
         self.extent_bytes.load(Ordering::SeqCst)+self.payload_bytes.load(Ordering::SeqCst))
    }
}
// Parse server framing and lengths, never retain/log any field payload. Count
// whole DataRow envelopes independently of importer counters, including every
// unchosen extent and both identifiers accompanying each payload substring.
fn relay_backend(from:&mut impl Read,to:&mut impl Write,meter:&ValidationWire)->std::io::Result<()> {
    let mut class=0;
    loop {
        let mut header=[0;5];
        match from.read_exact(&mut header) {Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::UnexpectedEof=>return Ok(()),Err(e)=>return Err(e)}
        let length=u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
        assert!((4..=64*1024*1024).contains(&length));to.write_all(&header)?;
        if header[0]==b'T' {
            let mut n=[0;2];from.read_exact(&mut n)?;to.write_all(&n)?;
            let count=u16::from_be_bytes(n) as usize;let mut names=Vec::new();let mut used=2;
            for _ in 0..count {
                let mut name=Vec::new();loop {let mut b=[0];from.read_exact(&mut b)?;to.write_all(&b)?;used+=1;if b[0]==0{break;}name.push(b[0]);assert!(name.len()<=256);}
                names.push(name);relay_payload(from,to,18)?;used+=18;
            }
            assert_eq!(used,length-4);
            let names:Vec<&[u8]>=names.iter().map(Vec::as_slice).collect();
            class=if names==[b"object_id".as_slice(),b"ordinal",b"byte_offset",b"octet_length",b"digest",b"byte_length",b"digest",b"validated"] {1}
                else if names==[b"object_id".as_slice(),b"ordinal",b"substring"] {2} else {0};
        } else {
            relay_payload(from,to,length-4)?;
            if header[0]==b'D' {
                let (rows,bytes)=match class {1=>(&meter.extent_rows,&meter.extent_bytes),2=>(&meter.payload_rows,&meter.payload_bytes),_=>continue};
                rows.fetch_add(1,Ordering::SeqCst);bytes.fetch_add(length+1,Ordering::SeqCst);
            }
        }
    }
}
fn protocol_proxy(url: &str) -> Option<(String, std::sync::Arc<AtomicUsize>, std::thread::JoinHandle<()>)> {
    let (url,counter,_,thread)=protocol_proxy_with_reads(url)?;Some((url,counter,thread))
}
fn protocol_proxy_with_reads(url: &str) -> Option<(String, std::sync::Arc<AtomicUsize>,std::sync::Arc<ValidationWire>,std::thread::JoinHandle<()>)> {
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
    let reads=std::sync::Arc::new(ValidationWire::default());let observed=reads.clone();
    let thread = std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        let mut backend = TcpStream::connect((host.as_str(), backend_port)).unwrap();
        client.set_nodelay(true).unwrap();
        backend.set_nodelay(true).unwrap();
        let mut upstream = backend.try_clone().unwrap();
        let mut downstream = client.try_clone().unwrap();
        let returning = std::thread::spawn(move || {
            relay_backend(&mut upstream,&mut downstream,&observed).unwrap();
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
    Some((proxy, count, reads, thread))
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


#[test]
fn selected_frozen_metadata_survives_copy_limited_pages_and_owner_resume() {
    use atlas::pg::history_catalog::{Lookup, LookupPurpose, ReadBudget, Snapshot};
    const LENGTH: usize = 9 * 1024 * 1024;
    for column in [Column::Name, Column::Poster] {
        for copy_bytes in [4 * 1024 * 1024, 384 * 1024] {
            let files = tempfile::tempdir().unwrap();
            let path = history_catalog_support::manifest(files.path());
            let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let packed = files.path().join("selected.zst");
            let mut encoder = zstd::stream::write::Encoder::new(std::fs::File::create(&packed).unwrap(), 1).unwrap();
            encoder.write_all(&[1]).unwrap(); varint(&mut encoder,0); varint(&mut encoder,1);
            bytes(&mut encoder,b"releases"); varint(&mut encoder,4);
            for name in [b"id".as_slice(),b"name",b"group_name",b"poster"] { bytes(&mut encoder,name); }
            varint(&mut encoder,1); encoder.write_all(&[1]).unwrap(); varint(&mut encoder,82);
            let mut expected = Sha256::new();
            for field in [Column::Name, Column::Group, Column::Poster] {
                encoder.write_all(&[3]).unwrap();
                if field == column {
                    varint(&mut encoder,LENGTH as u64);
                    let chunk=[b'x';65536];
                    for _ in 0..LENGTH/chunk.len() { encoder.write_all(&chunk).unwrap();expected.update(chunk); }
                } else { bytes(&mut encoder,if field == Column::Group {b"test.group"} else {b"small"}); }
            }
            encoder.finish().unwrap().sync_all().unwrap();
            let expected:[u8;32]=expected.finalize().into();
            let frozen=manifest.files.iter().find(|f|f.kind==SourceKind::Frozen).unwrap();
            let c=rusqlite::Connection::open(&frozen.path).unwrap();
            c.execute("insert into frozen_releases(id,name,group_name,reason,frozen_at,payload) values(41,'Header.Title','test.group','synthetic',1,?1)",[std::fs::read(&packed).unwrap()]).unwrap();drop(c);
            history_catalog_support::refresh(&mut manifest);
            let source=Source::open(manifest,4*1024*1024).unwrap();
            let target_files=tempfile::tempdir().unwrap();
            let Some((mut target,_))=history_catalog_support::destination(target_files.path()) else{return};
            let spool=tempfile::tempdir().unwrap();
            #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(spool.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
            let mut owner=target.owner(spool.path(),32*1024*1024);
            let id=owner.begin(&source).unwrap();let budget=ImportBudget{copy_bytes,..Default::default()};
            let mut sequence=0;let mut resumed=false;
            loop {
                let receipt=owner.step(&source,id,sequence,budget).unwrap();sequence=receipt.sequence;
                assert!(receipt.copy_bytes<=copy_bytes);assert!(receipt.source_bytes+receipt.decoded_bytes<=budget.source_bytes);
                let checkpoint:Vec<u8>=target.client.query_one(&format!("select checkpoint from {}.history_imports",target.name),&[]).unwrap().get(0);
                let state:serde_json::Value=serde_json::from_slice(&checkpoint).unwrap();
                let projection=&state["pending"]["frozen"];
                if !resumed && projection["phase"]==2 && projection["projection_data"]["offset"].as_u64().is_some_and(|n|n>0 && n<LENGTH as u64) {
                    drop(owner);owner=target.owner(spool.path(),32*1024*1024);assert_eq!(owner.begin(&source).unwrap(),id);resumed=true;
                }
                if receipt.complete {break;}assert!(sequence<3000);
            }
            assert!(resumed,"must resume a committed partial selected-field upload");
            owner.finish_identity(&source,id).unwrap();
            target.client.batch_execute(&format!("set search_path to {}",target.name)).unwrap();
            let mut catalog=Snapshot::open(&mut target.client,id).unwrap();
            let Lookup::Found(found)=catalog.lookup(41,LookupPurpose::Raw,ReadBudget::default()).unwrap() else{panic!("frozen lookup")};
            let mut cursor=catalog.value_cursor(found.metadata.field(column).unwrap()).unwrap();let mut actual=Sha256::new();let mut length=0;
            loop {let receipt=catalog.read_value(&mut cursor,ReadBudget::default(),|part|{actual.update(part);length+=part.len();Ok(())}).unwrap();if receipt.complete{break;}}
            assert_eq!(length,LENGTH);assert_eq!(<[u8;32]>::from(actual.finalize()),expected);
            println!("{}",serde_json::json!({"case":"selected_frozen_copy_resume","column":column.name(),"copy_bytes":copy_bytes,"pages":sequence,"readback_bytes":length}));
        }
    }
}

#[test]
fn validation_transfer_admits_all_extent_rows_and_payload_identifiers() {
    let files=tempfile::tempdir().unwrap();let path=history_catalog_support::manifest(files.path());
    let mut manifest:Manifest=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let current=manifest.files.iter().find(|f|f.kind==SourceKind::Current).unwrap();
    let c=rusqlite::Connection::open(&current.path).unwrap();c.execute("update releases set poster=zeroblob(?1) where id=3",[40*1024*1024]).unwrap();drop(c);
    history_catalog_support::refresh(&mut manifest);let source=Source::open(manifest,48*1024*1024).unwrap();
    let target_files=tempfile::tempdir().unwrap();let Some((mut target,_))=history_catalog_support::destination(target_files.path()) else{return};
    let Some((url,counter,wire,proxy))=protocol_proxy_with_reads(&target.url) else{panic!("validation transfer measurement requires the TCP fixture endpoint")};
    let spool=tempfile::tempdir().unwrap();
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;std::fs::set_permissions(spool.path(),std::fs::Permissions::from_mode(0o700)).unwrap();}
    let mut owner=atlas::pg::history_import::ImportOwner::connect(atlas::pg::StorageConfig{backend:atlas::pg::Backend::Postgres,url,schema:target.name.clone(),max_connections:1,allow_plaintext:true},target.expected.clone(),spool.path().into(),64*1024*1024).unwrap();
    let id=owner.begin(&source).unwrap();let mut sequence=0;let mut measured=0;let mut maximum=0;let mut validation_pages=0;
    loop {
        let before=wire.snapshot();let before_trips=counter.load(Ordering::SeqCst);
        let budget=ImportBudget{source_bytes:if measured>0 && validation_pages%2==1 {16384} else {4*1024*1024},..Default::default()};
        let receipt=owner.step(&source,id,sequence,budget).unwrap();sequence=receipt.sequence;
        let after=wire.snapshot();let rows=after.0-before.0;let payload_rows=after.1-before.1;let bytes=after.2-before.2;
        assert_eq!((counter.load(Ordering::SeqCst)-before_trips) as u64,receipt.sql_round_trips);
        assert!(bytes as u64<=budget.source_bytes,"independent validation transfer {bytes} exceeds {} at page {sequence}",budget.source_bytes);
        assert!(rows<=budget.source_rows as usize,"all fetched extent rows must be admitted");
        assert!(payload_rows<=rows,"payload lookup must reuse admitted extent identities");
        assert!(bytes as u64<=receipt.source_bytes+receipt.decoded_bytes,"receipt omitted independently observed validation bytes");
        if rows>0 {validation_pages+=1;measured+=bytes;maximum=maximum.max(bytes);}
        if receipt.complete{break;}assert!(sequence<10000);
    }
    owner.finish_identity(&source,id).unwrap();drop(owner);proxy.join().unwrap();
    assert!(wire.extent_rows.load(Ordering::SeqCst)>512);assert!(wire.payload_rows.load(Ordering::SeqCst)>512);
    assert!(validation_pages>1);println!("{}",serde_json::json!({"case":"validation_wire_admission","data_row_bytes":measured,"maximum_step_bytes":maximum,"extent_rows":wire.extent_rows.load(Ordering::SeqCst),"payload_rows":wire.payload_rows.load(Ordering::SeqCst),"pages":sequence,"validation_pages":validation_pages}));
    target.client.batch_execute(&format!("set search_path to {}",target.name)).unwrap();
}
