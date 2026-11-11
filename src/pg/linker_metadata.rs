//! Streaming legacy placement/seed migration. ID lines are skipped here (their
//! separate importer checkpoints within a line). CSV memberships are consumed
//! one number at a time, not collected into a group's or upload's Vec/String.

use super::linker_legacy::Committed;
use anyhow::{Result, ensure};
use postgres::{Client, Transaction};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

struct Record<'a, R> {
    reader: &'a mut R,
    offset: &'a mut u64,
    end: u64,
    ended: bool,
}
impl<R: BufRead> Record<'_, R> {
    fn byte(&mut self) -> Result<Option<u8>> {
        if *self.offset >= self.end {
            return Ok(None);
        }
        let mut b = [0u8; 1];
        self.reader.read_exact(&mut b)?;
        *self.offset += 1;
        if b[0] == b'\n' {
            self.ended = true;
        }
        Ok(Some(b[0]))
    }
    fn skip(&mut self) -> Result<()> {
        while !self.ended && self.byte()?.is_some() {}
        Ok(())
    }
    // Only scalar names are materialized. CSV fields use number(), whose
    // memory is constant even for malformed/very long numeric tokens.
    fn text(&mut self) -> Result<Option<String>> {
        if self.ended {
            return Ok(None);
        }
        let mut out = Vec::new();
        loop {
            match self.byte()? {
                Some(b'\t') => return Ok(Some(String::from_utf8_lossy(&out).into_owned())),
                Some(b'\n') | None => return Ok(None),
                Some(b) => out.push(b),
            }
        }
    }
    fn number(&mut self, stops: &[u8]) -> Result<(Option<i64>, u8)> {
        if self.ended {
            return Ok((None, b'\n'));
        }
        let (mut value, mut any, mut valid, mut negative, mut sign) = (0i64, false, true, false, false);
        loop {
            let Some(b) = self.byte()? else {
                return Ok((None, 0));
            };
            if stops.contains(&b) || b == b'\n' {
                return Ok(((valid && any).then_some(value), b));
            }
            if (b == b'-' || b == b'+') && !any && !sign {
                sign = true;
                negative = b == b'-';
                continue;
            }
            if b.is_ascii_digit() {
                any = true;
                let digit = i64::from(b - b'0');
                match value
                    .checked_mul(10)
                    .and_then(|n| if negative { n.checked_sub(digit) } else { n.checked_add(digit) })
                {
                    Some(n) => value = n,
                    None => valid = false,
                }
            } else {
                valid = false;
            }
        }
    }
    fn integer(&mut self, last: bool) -> Result<Option<i64>> {
        let (value, stop) = self.number(b"\t")?;
        Ok((stop == if last { b'\n' } else { b'\t' }).then_some(value).flatten())
    }
}

fn placement<R: BufRead>(tx: &mut Transaction<'_>, r: &mut Record<'_, R>) -> Result<bool> {
    let Some(added) = r.integer(false)? else { return Ok(false) };
    let Some(poster) = r.text()? else { return Ok(false) };
    let Some(release) = r.text()? else { return Ok(false) };
    let Some(group) = r.text()? else { return Ok(false) };
    let Some(file) = r.integer(false)? else { return Ok(false) };
    let Some(files) = r.integer(false)? else { return Ok(false) };
    let Some(parts) = r.integer(false)? else { return Ok(false) };
    let Some(oldest) = r.integer(false)? else { return Ok(false) };
    tx.execute(
        "insert into linker_places values($1,$2,$3,$4,$5,$6,$7,$8) on conflict(poster) do update set
        release_name=excluded.release_name,group_name=excluded.group_name,file_number=excluded.file_number,
        file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",
        &[&poster.as_bytes(), &release, &group, &file, &files, &parts, &oldest, &added],
    )?;
    tx.execute("delete from linker_place_leads where poster=$1", &[&poster.as_bytes()])?;
    tx.execute("delete from linker_place_firsts where poster=$1", &[&poster.as_bytes()])?;
    let mut ordinal = 0i64;
    loop {
        let (part, stop) = r.number(b",\t")?;
        if let Some(part) = part {
            ordinal += 1;
            tx.execute("insert into linker_place_leads values($1,$2,$3)", &[&poster.as_bytes(), &ordinal, &part])?;
        }
        match stop {
            b',' => {}
            b'\t' => break,
            _ => return Ok(false),
        }
    }
    loop {
        let (group, stop) = r.number(b":,\t")?;
        match stop {
            b'\n' => return Ok(true),
            b',' => continue,
            b':' => {}
            _ => return Ok(false),
        }
        let (number, stop) = r.number(b",\t")?;
        if let (Some(group), Some(number)) = (group, number) {
            tx.execute(
                "insert into linker_place_firsts values($1,$2,$3) on conflict(poster,group_id)
                do update set article_number=excluded.article_number",
                &[&poster.as_bytes(), &group, &number],
            )?;
        }
        match stop {
            b',' => {}
            b'\n' => return Ok(true),
            _ => return Ok(false),
        }
    }
}

fn upload<R: BufRead>(tx: &mut Transaction<'_>, r: &mut Record<'_, R>) -> Result<bool> {
    let Some(added) = r.integer(false)? else { return Ok(false) };
    let Some(release) = r.text()? else { return Ok(false) };
    let Some(group) = r.text()? else { return Ok(false) };
    // Placeholder values are hidden by this record's transaction until its
    // numeric tail and complete line have been checked.
    tx.execute(
        "insert into linker_uploads values($1,$2,0,0,0,0,0,$3) on conflict(release_name)
        do update set group_name=excluded.group_name,added=excluded.added",
        &[&release, &group, &added],
    )?;
    tx.execute("delete from linker_upload_groups where release_name=$1", &[&release])?;
    loop {
        let (group, stop) = r.number(b",\t")?;
        if let Some(group) = group {
            tx.execute("insert into linker_upload_groups values($1,$2) on conflict do nothing", &[&release, &group])?;
        }
        match stop {
            b',' => {}
            b'\t' => break,
            _ => return Ok(false),
        }
    }
    let Some(first) = r.integer(false)? else { return Ok(false) };
    let Some(last) = r.integer(false)? else { return Ok(false) };
    let Some(files) = r.integer(false)? else { return Ok(false) };
    let Some(generation) = r.integer(false)? else { return Ok(false) };
    let Some(probed) = r.integer(true)? else { return Ok(false) };
    tx.execute("update linker_uploads set first_posted=$2,last_posted=$3,files=$4,generation=$5,probed=$6 where release_name=$1",
        &[&release,&first,&last,&files,&generation,&probed])?;
    Ok(true)
}

fn probe<R: BufRead>(tx: &mut Transaction<'_>, r: &mut Record<'_, R>) -> Result<bool> {
    let Some(release) = r.text()? else { return Ok(false) };
    let Some(generation) = r.integer(false)? else { return Ok(false) };
    let Some(probed) = r.integer(true)? else { return Ok(false) };
    tx.execute(
        "update linker_uploads set probed=greatest(probed,$3) where release_name=$1 and generation=$2",
        &[&release, &generation, &probed],
    )?;
    Ok(true)
}

/// Import committed F/U/P records, with one atomic receipt per complete record.
/// A torn record or failed transaction never changes the live placement/seed.
/// Kept scalar fields are bounded by one historical header/record; membership
/// length does not affect client memory. The source is never rewritten.
pub(crate) fn import(client: &mut Client, dir: &Path, now: i64) -> Result<()> {
    const NAME: &str = "links.rec:metadata";
    let path = dir.join("links.rec");
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let pending: bool = client
                .query_one("select exists(select 1 from linker_id_imports where name=$1 and not complete)", &[&NAME])?
                .get(0);
            ensure!(!pending, "legacy metadata source disappeared during migration");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let metadata = file.metadata()?;
    let mut digest = Sha256::new();
    digest.update(metadata.dev().to_le_bytes());
    digest.update(metadata.ino().to_le_bytes());
    digest.update(metadata.len().to_le_bytes());
    digest.update(metadata.mtime().to_le_bytes());
    digest.update(metadata.mtime_nsec().to_le_bytes());
    let identity: [u8; 32] = digest.finalize().into();
    client.execute(
        "insert into linker_id_imports(name,source_identity) values($1,$2) on conflict do nothing",
        &[&NAME, &&identity[..]],
    )?;
    let saved = client
        .query_one("select source_identity,offset_bytes,complete from linker_id_imports where name=$1", &[&NAME])?;
    ensure!(saved.get::<_, Vec<u8>>(0) == identity, "legacy metadata source changed during migration");
    if saved.get::<_, bool>(2) {
        return Ok(());
    }
    let mut offset = u64::try_from(saved.get::<_, i64>(1))?;
    let mut ranges = Committed::new(file)?;
    let mut reader = BufReader::new(File::open(&path)?);
    while let Some(range) = ranges.next_range()? {
        if range.end <= offset {
            continue;
        }
        offset = offset.max(range.start);
        reader.seek(SeekFrom::Start(offset))?;
        while offset < range.end {
            let mut record = Record { reader: &mut reader, offset: &mut offset, end: range.end, ended: false };
            let tag = record.byte()?;
            if record.ended {
                continue;
            }
            if !matches!(tag, Some(b'F' | b'U' | b'P')) || record.byte()? != Some(b'\t') {
                record.skip()?;
                // No identifiers are buffered even for a giant I line.
                client.execute(
                    "update linker_id_imports set offset_bytes=$2 where name=$1",
                    &[&NAME, &i64::try_from(offset)?],
                )?;
                continue;
            }
            let mut tx = client.transaction()?;
            let valid = match tag {
                Some(b'F') => placement(&mut tx, &mut record)?,
                Some(b'U') => upload(&mut tx, &mut record)?,
                _ => probe(&mut tx, &mut record)?,
            };
            record.skip()?;
            if valid {
                tx.execute(
                    "update linker_id_imports set offset_bytes=$2 where name=$1",
                    &[&NAME, &i64::try_from(offset)?],
                )?;
                tx.commit()?;
            } else {
                tx.rollback()?;
                client.execute(
                    "update linker_id_imports set offset_bytes=$2 where name=$1",
                    &[&NAME, &i64::try_from(offset)?],
                )?;
            }
        }
    }
    let before = now.saturating_sub(8 * crate::xref::KEEP_SECS);
    let mut tx = client.transaction()?;
    tx.execute("delete from linker_places where added<$1", &[&before])?;
    tx.execute("delete from linker_uploads where added<$1", &[&before])?;
    tx.execute("update linker_id_imports set complete=true where name=$1", &[&NAME])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_import_streams_groups_and_preserves_probe_generation() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let groups = (1..=300).map(|g| g.to_string()).collect::<Vec<_>>().join(",");
        let text = format!(
            "V\t2\nU\t1000\trelease\tgroup.example\t{groups}\t10\t20\t1\t2\t0\nF\t1000\tposter@example.invalid\trelease\tgroup.example\t1\t1\t8\t1000\t1,5\t1:10,2:20\nP\trelease\t1\t99\nP\trelease\t2\t3\nC\t4\tok\nF\tuncommitted\n"
        );
        std::fs::write(dir.path().join("links.rec"), &text).unwrap();
        let mut c = schema.pool.get().unwrap();
        import(&mut c, dir.path(), 1000).unwrap();
        import(&mut c, dir.path(), 1000).unwrap();
        let row =
            c.query_one("select generation,probed from linker_uploads where release_name='release'", &[]).unwrap();
        assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (2, 3));
        let groups: i64 = c.query_one("select count(*) from linker_upload_groups", &[]).unwrap().get(0);
        assert_eq!(groups, 300);
        let leads = c.query("select part from linker_place_leads order by ordinal limit 3", &[]).unwrap();
        assert_eq!(leads.iter().map(|r| r.get::<_, i64>(0)).collect::<Vec<_>>(), vec![1, 5]);
        let firsts: i64 = c.query_one("select count(*) from linker_place_firsts", &[]).unwrap().get(0);
        assert_eq!(firsts, 2);
        assert_eq!(std::fs::read_to_string(dir.path().join("links.rec")).unwrap(), text);
    }
}
