//! Bounded, restartable migration of historical hash-only membership. Full IDs
//! cannot be reconstructed from these logs. Preserve their conservative hash
//! semantics until expiry, rather than pretending a hash identifies one string.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use postgres::Client;
use sha2::{Digest, Sha256};

use super::linker_legacy::Committed;

const BATCH: usize = 256;

#[derive(Default)]
struct Hex {
    value: u64,
    valid: bool,
    any: bool,
    sign: bool,
}
impl Hex {
    fn new() -> Self {
        Self { valid: true, ..Self::default() }
    }
    fn byte(&mut self, b: u8) {
        if b == b'+' && !self.any && !self.sign {
            self.sign = true;
            return;
        }
        let digit = match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        self.any = true;
        match digit.and_then(|d| self.value.checked_mul(16)?.checked_add(u64::from(d))) {
            Some(n) => self.value = n,
            None => self.valid = false,
        }
    }
    fn value(&self) -> Option<u64> {
        (self.valid && self.any).then_some(self.value)
    }
}

/// Constant-state I-line decoder. Numeric fields can contain arbitrarily many
/// leading zeroes without allocating a token. Byte positions are checkpointed
/// only at hash/line boundaries, where `time` suffices to resume decoding.
struct Decoder {
    state: u8,
    time: i64,
    negative: bool,
    sign: bool,
    any_time: bool,
    time_valid: bool,
    hash: Hex,
}
impl Decoder {
    fn new(time: Option<i64>) -> Self {
        Self {
            state: if time.is_some() { 3 } else { 0 },
            time: time.unwrap_or(0),
            negative: false,
            sign: false,
            any_time: false,
            time_valid: true,
            hash: Hex::new(),
        }
    }
    fn byte(&mut self, b: u8) -> Option<(u64, i64)> {
        let mut out = None;
        match self.state {
            0 => self.state = if b == b'I' { 1 } else { 4 },
            1 => self.state = if b == b'\t' { 2 } else { 4 },
            2 => {
                if b == b'\t' {
                    self.state = if self.any_time && self.time_valid { 3 } else { 4 };
                } else if (b == b'-' || b == b'+') && !self.any_time && !self.sign {
                    self.negative = b == b'-';
                    self.sign = true;
                } else if b.is_ascii_digit() {
                    self.any_time = true;
                    let digit = i64::from(b - b'0');
                    let next = self
                        .time
                        .checked_mul(10)
                        .and_then(|n| if self.negative { n.checked_sub(digit) } else { n.checked_add(digit) });
                    match next {
                        Some(n) => self.time = n,
                        None => self.time_valid = false,
                    }
                } else {
                    self.time_valid = false;
                }
            }
            3 => {
                if b == b',' || b == b'\n' {
                    out = self.hash.value().map(|h| (h, self.time));
                    self.hash = Hex::new();
                } else {
                    self.hash.byte(b);
                }
            }
            _ => {}
        }
        if b == b'\n' {
            *self = Self::new(None);
        }
        out
    }
    fn resume_time(&self) -> Option<i64> {
        (self.state == 3).then_some(self.time)
    }
}

fn checkpoint(
    client: &mut Client,
    name: &str,
    offset: u64,
    time: Option<i64>,
    ids: &mut Vec<(i64, i64)>,
) -> Result<()> {
    let mut tx = super::linker::durable_transaction(client)?;
    // At most BATCH identities are held. Expired entries can be pruned later in
    // bounded pages without ever loading native membership into a HashSet.
    for &(hash, added) in ids.iter() {
        tx.execute(
            "insert into linker_legacy_ids values($1,$2,$3) on conflict(origin,hash)
            do update set added=case when excluded.origin='links.rec' then excluded.added
                else greatest(linker_legacy_ids.added,excluded.added) end",
            &[&name, &hash, &added],
        )?;
    }
    tx.execute(
        "update linker_id_imports set offset_bytes=$2,id_time=$3 where name=$1",
        &[&name, &i64::try_from(offset)?, &time],
    )?;
    tx.commit()?;
    ids.clear();
    Ok(())
}

/// Import both historical formats without modifying them. The native ownership
/// lock must already be held, so no legacy linker can append concurrently.
/// `now` is the immutable migration logical time, not a fresh clock per retry.
pub(crate) fn import(client: &mut Client, dir: &Path, now: i64) -> Result<()> {
    for (name, binary) in [("links.rec", false), ("linked-ids.rec", true)] {
        let path = dir.join(name);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let pending: bool = client
                    .query_one(
                        "select exists(select 1 from linker_id_imports where name=$1 and not complete)",
                        &[&name],
                    )?
                    .get(0);
                ensure!(!pending, "legacy linker source disappeared before migration completed");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let metadata = file.metadata()?;
        let mut identity = Sha256::new();
        identity.update(metadata.dev().to_le_bytes());
        identity.update(metadata.ino().to_le_bytes());
        identity.update(metadata.len().to_le_bytes());
        identity.update(metadata.mtime().to_le_bytes());
        identity.update(metadata.mtime_nsec().to_le_bytes());
        let identity: [u8; 32] = identity.finalize().into();
        super::linker::durable_execute(
            client,
            "insert into linker_id_imports(name,source_identity) values($1,$2) on conflict do nothing",
            &[&name, &&identity[..]],
        )?;
        let state = client.query_one(
            "select source_identity,offset_bytes,id_time,complete from linker_id_imports where name=$1",
            &[&name],
        )?;
        ensure!(state.get::<_, Vec<u8>>(0) == identity, "legacy linker source changed during migration");
        if state.get::<_, bool>(3) {
            continue;
        }
        let mut offset = u64::try_from(state.get::<_, i64>(1))?;
        let resume: Option<i64> = state.get(2);
        let before = now.saturating_sub(8 * crate::xref::KEEP_SECS);
        let mut ids = Vec::with_capacity(BATCH);
        if binary {
            let mut reader = BufReader::new(file);
            ensure!(offset % 16 == 0, "invalid legacy binary checkpoint");
            reader.seek(SeekFrom::Start(offset))?;
            let mut record = [0u8; 16];
            let mut scanned = 0;
            loop {
                match reader.read_exact(&mut record) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => return Err(e.into()),
                }
                let hash = i64::from_le_bytes(record[..8].try_into().expect("8 bytes"));
                let at = i64::from_le_bytes(record[8..].try_into().expect("8 bytes"));
                if at >= before {
                    ids.push((hash, at));
                }
                offset += 16;
                scanned += 1;
                if scanned == BATCH {
                    checkpoint(client, name, offset, None, &mut ids)?;
                    scanned = 0;
                }
            }
        } else {
            let mut ranges = Committed::new(file)?;
            let mut reader = BufReader::new(File::open(&path)?);
            let mut decoder = Decoder::new(resume);
            let mut scanned = 0;
            while let Some(range) = ranges.next_range()? {
                if range.end <= offset {
                    continue;
                }
                if offset < range.start {
                    offset = range.start;
                    decoder = Decoder::new(None);
                }
                reader.seek(SeekFrom::Start(offset))?;
                let mut byte = [0u8; 1];
                while offset < range.end {
                    reader.read_exact(&mut byte).context("reading committed legacy range")?;
                    offset += 1;
                    if let Some((hash, at)) = decoder.byte(byte[0]) {
                        ids.push((hash as i64, at));
                        scanned += 1;
                        if scanned == BATCH {
                            checkpoint(client, name, offset, decoder.resume_time(), &mut ids)?;
                            scanned = 0;
                        }
                    }
                }
                checkpoint(client, name, offset, None, &mut ids)?;
            }
        }
        checkpoint(client, name, offset, None, &mut ids)?;
        super::linker::durable_execute(client, "update linker_id_imports set complete=true where name=$1", &[&name])?;
    }
    Ok(())
}

/// Native membership preserves canonical bytes, using SHA-256 only as an
/// index. Historical hashes remain a separate conservative fallback.
#[cfg(test)]
pub(crate) fn contains(client: &mut Client, message: &[u8], now: i64) -> Result<bool> {
    let key: [u8; 32] = Sha256::digest(message).into();
    let hash = nntp_xover_engine::msgid_index::msgid_hash(message) as i64;
    let before = now.saturating_sub(8 * crate::xref::KEEP_SECS);
    Ok(client
        .query_one(
            "select exists(select 1 from linker_linked_ids where message_key=$1 and message_id=$2 and added>=$4)
        or exists(select 1 from linker_legacy_ids where hash=$3 and added>=$4)",
            &[&&key[..], &message, &hash, &before],
        )?
        .get(0))
}

/// Invoke with the publication receipt. A hypothetical SHA-256 collision is
/// an explicit error, never a silent merge of two different article IDs.
#[cfg(test)]
pub(crate) fn remember(client: &mut Client, message: &[u8], added: i64) -> Result<()> {
    let key: [u8; 32] = Sha256::digest(message).into();
    let changed = super::linker::durable_execute(
        client,
        "insert into linker_linked_ids values($1,$2,$3)
        on conflict(message_key) do update set added=greatest(linker_linked_ids.added,excluded.added)
        where linker_linked_ids.message_id=excluded.message_id",
        &[&&key[..], &message, &added],
    )?;
    ensure!(changed == 1, "native linker message identity digest collision");
    Ok(())
}

/// Reclamation never materializes membership in the process. The caller fixes
/// `now` for its sweep and repeats bounded pages as necessary.
pub(crate) fn prune(client: &mut Client, now: i64, budget: i64) -> Result<u64> {
    ensure!(budget > 0, "native linker expiry budget must be positive");
    let before = now.saturating_sub(8 * crate::xref::KEEP_SECS);
    let n = super::linker::durable_execute(
        client,
        "delete from linker_linked_ids where ctid in
        (select ctid from linker_linked_ids where added<$1 order by added limit $2)",
        &[&before, &budget],
    )?;
    let m = super::linker::durable_execute(
        client,
        "delete from linker_legacy_ids where ctid in
        (select ctid from linker_legacy_ids where added<$1 and not exists
            (select 1 from linker_id_imports where not complete) order by added limit $2)",
        &[&before, &budget],
    )?;
    Ok(n + m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_id_import_resumes_inside_committed_line_after_storage_error() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let now = 100_000i64;
        let hashes = (1..=600).map(|n| format!("{n:x}")).collect::<Vec<_>>().join(",");
        let log = format!("V\t2\nI\t{now}\t{hashes}\nC\t1\tok\nI\t{now}\tffff\n");
        std::fs::write(dir.path().join("links.rec"), &log).unwrap();
        let mut binary = 601u64.to_le_bytes().to_vec();
        binary.extend_from_slice(&now.to_le_bytes());
        binary.extend_from_slice(&999u64.to_le_bytes()); // torn binary tail
        std::fs::write(dir.path().join("linked-ids.rec"), &binary).unwrap();
        let mut client = schema.pool.get().unwrap();
        client.batch_execute("create function fail_legacy_ids() returns trigger language plpgsql as $$
            begin if new.hash>256 then raise exception 'synthetic storage exhaustion' using errcode='53100'; end if;
            return new; end $$;
            create trigger fail_legacy_ids before insert on linker_legacy_ids for each row execute function fail_legacy_ids()").unwrap();
        assert!(import(&mut client, dir.path(), now).is_err());
        let count: i64 = client.query_one("select count(*) from linker_legacy_ids", &[]).unwrap().get(0);
        assert_eq!(count, 256);
        let checkpoint =
            client.query_one("select offset_bytes,id_time from linker_id_imports where name='links.rec'", &[]).unwrap();
        assert!(checkpoint.get::<_, i64>(0) > 0);
        assert_eq!(checkpoint.get::<_, Option<i64>>(1), Some(now));
        client.batch_execute("drop trigger fail_legacy_ids on linker_legacy_ids").unwrap();
        drop(client);
        let mut client = schema.pool.get().unwrap();
        import(&mut client, dir.path(), now).unwrap();
        import(&mut client, dir.path(), now).unwrap();
        let count: i64 = client.query_one("select count(*) from linker_legacy_ids", &[]).unwrap().get(0);
        assert_eq!(count, 601);
        assert_eq!(std::fs::read_to_string(dir.path().join("links.rec")).unwrap(), log);
        assert_eq!(std::fs::read(dir.path().join("linked-ids.rec")).unwrap(), binary);
    }

    #[test]
    fn membership_expires_in_pages_and_legacy_clock_regression_uses_last_line() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let now = 1_000_000i64;
        std::fs::write(dir.path().join("links.rec"), format!("V\t2\nI\t{now}\t1\nI\t1\t1\nC\t2\tok\n")).unwrap();
        let mut client = schema.pool.get().unwrap();
        import(&mut client, dir.path(), now).unwrap();
        let at: i64 = client
            .query_one("select added from linker_legacy_ids where origin='links.rec' and hash=1", &[])
            .unwrap()
            .get(0);
        assert_eq!(at, 1, "legacy links.log uses the last occurrence even if its clock moved backwards");
        for message in [b"<a@example.invalid>".as_slice(), b"<b@example.invalid>", b"<c@example.invalid>"] {
            remember(&mut client, message, 1).unwrap();
        }
        assert!(!contains(&mut client, b"<a@example.invalid>", now).unwrap());
        assert_eq!(prune(&mut client, now, 1).unwrap(), 2);
        let remaining: i64 = client.query_one("select count(*) from linker_linked_ids", &[]).unwrap().get(0);
        assert_eq!(remaining, 2);
        remember(&mut client, b"<new@example.invalid>", now).unwrap();
        assert!(contains(&mut client, b"<new@example.invalid>", now).unwrap());
        assert!(!contains(&mut client, b"<absent@example.invalid>", now).unwrap());
    }

    #[test]
    fn decoder_preserves_hash_bits_and_can_resume_inside_a_large_id_line() {
        let mut decoder = Decoder::new(None);
        let mut found = Vec::new();
        for b in b"I\t-7\tffffffffffffffff,000000000000000000000000000000000a," {
            if let Some(v) = decoder.byte(*b) {
                found.push(v);
            }
        }
        assert_eq!(found, vec![(u64::MAX, -7), (10, -7)]);
        let mut restarted = Decoder::new(decoder.resume_time());
        for b in b"badhex,2\nF\tignored\nI\t9\t3\n" {
            if let Some(v) = restarted.byte(*b) {
                found.push(v);
            }
        }
        assert_eq!(found, vec![(u64::MAX, -7), (10, -7), (2, -7), (3, 9)]);
    }
}
