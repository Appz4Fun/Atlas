//! Bounded, restartable migration of historical hash-only membership. Full IDs
//! cannot be reconstructed from these logs. Preserve their conservative hash
//! semantics until expiry, rather than pretending a hash identifies one string.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Result, ensure};
use postgres::Client;
#[cfg(test)]
use sha2::{Digest, Sha256};

#[derive(Default, serde::Serialize, serde::Deserialize)]
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
#[derive(serde::Serialize, serde::Deserialize)]
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

#[derive(serde::Serialize, serde::Deserialize)]
struct IdState {
    ordinal: i64,
    offset: u64,
    end: u64,
    decoder: Decoder,
}
impl Default for IdState {
    fn default() -> Self {
        Self { ordinal: 0, offset: 0, end: 0, decoder: Decoder::new(None) }
    }
}

/// Each call commits one byte/identity page, including the decoder state inside
/// an arbitrarily long token. Source range discovery is independently paged.
pub(crate) fn step(client: &mut Client, dir: &Path, now: i64, budget: super::linker_stage::Budget) -> Result<bool> {
    for (name, binary) in [("links.rec", false), ("linked-ids.rec", true)] {
        let path = dir.join(name);
        if !binary && !super::linker_import_scan::step(client, &path, name, budget)? {
            return Ok(false);
        }
        if !step_file(client, &path, name, binary, now, budget)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn step_file(
    client: &mut Client,
    path: &Path,
    name: &str,
    binary: bool,
    now: i64,
    budget: super::linker_stage::Budget,
) -> Result<bool> {
    ensure!(budget.rows > 0 && budget.bytes > 0, "ID import budget must be positive");
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ensure!(
                !client
                    .query_one(
                        "select exists(select 1 from linker_id_imports where name=$1 and not complete)",
                        &[&name]
                    )?
                    .get::<_, bool>(0),
                "legacy ID source disappeared during import"
            );
            return Ok(true);
        }
        Err(e) => return Err(e.into()),
    };
    let identity = super::linker_import_scan::identity(&file)?;
    let job = format!("{name}:ids");
    let mut tx = super::linker::durable_transaction(client)?;
    let saved = tx
        .query_opt("select source_identity,state,complete from linker_import_jobs where name=$1 for update", &[&job])?;
    let mut s: IdState = if let Some(row) = saved {
        ensure!(row.get::<_, Vec<u8>>(0) == identity, "legacy ID source changed during import");
        if row.get::<_, bool>(2) {
            return Ok(true);
        }
        serde_json::from_str(&row.get::<_, String>(1))?
    } else {
        IdState::default()
    };
    let mut ids = Vec::with_capacity(budget.rows);
    let mut done = false;
    if binary {
        file.seek(SeekFrom::Start(s.offset))?;
        for _ in 0..budget.rows.min((budget.bytes / 16).max(1)) {
            let mut bytes = [0; 16];
            match file.read_exact(&mut bytes) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    done = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            s.offset += 16;
            let hash = i64::from_le_bytes(bytes[..8].try_into().unwrap());
            let added = i64::from_le_bytes(bytes[8..].try_into().unwrap());
            if added >= now.saturating_sub(8 * crate::xref::KEEP_SECS) {
                ids.push((hash, added));
            }
        }
    } else if s.offset >= s.end {
        if let Some((ordinal, start, end)) = super::linker_import_scan::next(&mut tx, name, s.ordinal)? {
            s.ordinal = ordinal;
            s.offset = start;
            s.end = end;
            s.decoder = Decoder::new(None);
            file.seek(SeekFrom::Start(start))?;
            let mut tag = [0; 2];
            if end - start >= 2 {
                file.read_exact(&mut tag)?;
            }
            if tag != *b"I\t" {
                s.offset = end;
            }
        } else {
            done = true;
        }
    } else {
        file.seek(SeekFrom::Start(s.offset))?;
        let mut bytes = vec![0; (s.end - s.offset).min(budget.bytes as u64) as usize];
        file.read_exact(&mut bytes)?;
        for byte in bytes {
            s.offset += 1;
            if let Some((hash, added)) = s.decoder.byte(byte) {
                ids.push((hash as i64, added));
            }
            if ids.len() == budget.rows {
                break;
            }
        }
    }
    for (hash, added) in ids {
        tx.execute("insert into linker_legacy_ids values($1,$2,$3) on conflict(origin,hash) do update set
            added=case when excluded.origin='links.rec' then excluded.added else greatest(linker_legacy_ids.added,excluded.added) end", &[&name,&hash,&added])?;
    }
    tx.execute("insert into linker_id_imports(name,source_identity,offset_bytes,id_time,complete) values($1,$2,$3,$4,$5)
        on conflict(name) do update set offset_bytes=excluded.offset_bytes,id_time=excluded.id_time,complete=excluded.complete",
        &[&name,&identity,&i64::try_from(s.offset)?,&s.decoder.resume_time(),&done])?;
    tx.execute("insert into linker_import_jobs values($1,$2,$3,$4) on conflict(name) do update set state=excluded.state,complete=excluded.complete",
        &[&job,&identity,&serde_json::to_string(&s)?,&done])?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
pub(crate) fn import(client: &mut Client, dir: &Path, now: i64) -> Result<()> {
    while !step(client, dir, now, super::linker_stage::Budget::default())? {}
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
