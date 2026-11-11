//! Durable byte/line index for immutable legacy logs. Commit suffixes are
//! represented as ordinal ranges, never expanded or rescanned on restart.
use super::linker_stage::Budget;
use anyhow::{Result, ensure};
use postgres::{Client, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Scan {
    offset: u64,
    start: u64,
    prefix: Vec<u8>,
    length: u64,
    marked: Option<bool>,
    ordinal: i64,
    pending: i64,
    ended: bool,
}

pub(crate) fn identity(file: &File) -> Result<Vec<u8>> {
    let m = file.metadata()?;
    let mut h = Sha256::new();
    h.update(m.dev().to_le_bytes());
    h.update(m.ino().to_le_bytes());
    h.update(m.len().to_le_bytes());
    h.update(m.mtime().to_le_bytes());
    h.update(m.mtime_nsec().to_le_bytes());
    Ok(h.finalize().to_vec())
}

/// Returns true once every complete line and valid commit suffix is indexed.
/// At most budget.bytes bytes and budget.rows line endings are visited per call.
pub(crate) fn step(client: &mut Client, path: &Path, name: &str, budget: Budget) -> Result<bool> {
    ensure!(budget.rows > 0 && budget.bytes > 0, "import scan budget must be positive");
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                !client
                    .query_one("select exists(select 1 from linker_import_scans where name=$1)", &[&name])?
                    .get::<_, bool>(0),
                "legacy source disappeared during import"
            );
            return Ok(true);
        }
        Err(e) => return Err(e.into()),
    };
    let identity = identity(&file)?;
    let mut tx = super::linker::durable_transaction(client)?;
    let saved = tx.query_opt(
        "select source_identity,state,complete from linker_import_scans where name=$1 for update",
        &[&name],
    )?;
    let mut s: Scan = if let Some(row) = saved {
        ensure!(row.get::<_, Vec<u8>>(0) == identity, "legacy source changed during import");
        if row.get::<_, bool>(2) {
            return Ok(true);
        }
        serde_json::from_str(&row.get::<_, String>(1))?
    } else {
        Scan::default()
    };
    file.seek(SeekFrom::Start(s.offset))?;
    // Read at most the byte budget. A line-count stop may leave a suffix in
    // this bounded buffer; the committed byte cursor seeks directly on retry.
    let mut bytes = vec![0; budget.bytes];
    let mut n = file.read(&mut bytes)?;
    let eof = n == 0;
    let terminal = eof && name == "linker.rec" && s.length > 0;
    if terminal {
        bytes[0] = b'\n';
        n = 1;
    }
    let mut lines = 0;
    for &byte in &bytes[..n] {
        s.offset += u64::from(!terminal);
        s.length += 1;
        if s.prefix.len() < 64 {
            s.prefix.push(byte);
        }
        if byte != b'\n' {
            continue;
        }
        let text = if s.length <= 64 { std::str::from_utf8(&s.prefix).ok() } else { None };
        if s.marked.is_none() {
            s.marked = Some(text == Some("V\t2\n"));
        }
        if text == Some("E\n") {
            s.ended = true;
        }
        if s.marked == Some(true) && s.prefix.starts_with(b"C\t") {
            let count = text.and_then(|t| t.strip_prefix("C\t")?.strip_suffix("\tok\n")?.parse::<i64>().ok());
            if let Some(count) = count.filter(|n| *n >= 0 && *n <= s.pending)
                && count > 0
            {
                tx.execute(
                    "insert into linker_import_ranges values($1,$2,$3)",
                    &[&name, &(s.ordinal - count + 1), &s.ordinal],
                )?;
            }
            s.pending = 0;
        } else if !(s.marked == Some(true) && text == Some("V\t2\n")) {
            s.ordinal += 1;
            s.pending += 1;
            tx.execute(
                "insert into linker_import_lines values($1,$2,$3,$4)",
                &[&name, &s.ordinal, &i64::try_from(s.start)?, &i64::try_from(s.offset)?],
            )?;
            if s.marked == Some(false) {
                tx.execute("insert into linker_import_ranges values($1,$2,$2)", &[&name, &s.ordinal])?;
            }
        }
        s.start = s.offset;
        s.length = 0;
        s.prefix.clear();
        lines += 1;
        if lines == budget.rows {
            break;
        }
    }
    tx.execute(
        "insert into linker_import_scans(name,source_identity,state,complete,ended) values($1,$2,$3,$4,$5)
        on conflict(name) do update set state=excluded.state,complete=excluded.complete,ended=excluded.ended",
        &[&name, &identity, &serde_json::to_string(&s)?, &eof, &s.ended],
    )?;
    tx.commit()?;
    Ok(eof)
}

/// The next accepted record, using one range seek and one ordinal point lookup.
/// Invalid/uncommitted prefixes are skipped by arithmetic, not scanned again.
pub(crate) fn next(tx: &mut Transaction<'_>, name: &str, after: i64) -> Result<Option<(i64, u64, u64)>> {
    let range=tx.query_opt("select first_ordinal,last_ordinal from linker_import_ranges where name=$1 and last_ordinal>$2 order by last_ordinal limit 1", &[&name,&after])?;
    let Some(range) = range else { return Ok(None) };
    let ordinal = (after + 1).max(range.get(0));
    let row = tx.query_one(
        "select start_bytes,end_bytes from linker_import_lines where name=$1 and ordinal=$2",
        &[&name, &ordinal],
    )?;
    Ok(Some((ordinal, u64::try_from(row.get::<_, i64>(0))?, u64::try_from(row.get::<_, i64>(1))?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_record_scan_resumes_by_byte_and_commit_suffix_without_prefix_replay() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.rec");
        let mut data = b"V\t2\nF\tabandoned\nI\t7\t".to_vec();
        data.extend(std::iter::repeat_n(b'0', 4097));
        data.extend_from_slice(b"\nF\tkept\nC\t2\tok\nF\ttorn");
        std::fs::write(&path, &data).unwrap();
        let mut c = schema.pool.get().unwrap();
        let budget = Budget { rows: 1, bytes: 31 };
        let mut previous = 0;
        loop {
            let done = step(&mut c, &path, "links.rec", budget).unwrap();
            let saved: String =
                c.query_one("select state from linker_import_scans where name='links.rec'", &[]).unwrap().get(0);
            let scan: Scan = serde_json::from_str(&saved).unwrap();
            assert!(scan.offset - previous <= 31);
            previous = scan.offset;
            if previous > 200 && previous < 240 {
                drop(c);
                c = schema.pool.get().unwrap();
            }
            if done {
                break;
            }
        }
        let mut tx = c.transaction().unwrap();
        let (ordinal, start, end) = next(&mut tx, "links.rec", 0).unwrap().unwrap();
        assert_eq!(ordinal, 2);
        assert!(data[start as usize..end as usize].starts_with(b"I\t7\t"));
        let (_, start, end) = next(&mut tx, "links.rec", ordinal).unwrap().unwrap();
        assert_eq!(&data[start as usize..end as usize], b"F\tkept\n");
        assert!(next(&mut tx, "links.rec", 3).unwrap().is_none());
    }
}
