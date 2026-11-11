//! Checkpointed legacy F/U/P import. Memberships are shadow rows until a whole
//! committed record validates; replacement and expiry then advance by keys.
use super::linker_stage::Budget;
use anyhow::{Result, ensure};
use postgres::{Client, Transaction};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(super) struct Number {
    value: i64,
    any: bool,
    valid: bool,
    negative: bool,
    sign: bool,
}
impl Default for Number {
    fn default() -> Self {
        Self { value: 0, any: false, valid: true, negative: false, sign: false }
    }
}
impl Number {
    pub(super) fn byte(&mut self, b: u8) {
        if (b == b'-' || b == b'+') && !self.any && !self.sign {
            self.sign = true;
            self.negative = b == b'-';
            return;
        }
        if b.is_ascii_digit() {
            self.any = true;
            let digit = i64::from(b - b'0');
            if let Some(n) = self
                .value
                .checked_mul(10)
                .and_then(|n| if self.negative { n.checked_sub(digit) } else { n.checked_add(digit) })
            {
                self.value = n;
            } else {
                self.valid = false;
            }
        } else {
            self.valid = false;
        }
    }
    pub(super) fn take(&mut self) -> Option<i64> {
        let result = (self.any && self.valid).then_some(self.value);
        *self = Self::default();
        result
    }
}
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Meta {
    stage: u8,
    ordinal: i64,
    offset: u64,
    end: u64,
    tag: u8,
    field: u8,
    valid: bool,
    number: Number,
    text: Vec<u8>,
    pair: Option<i64>,
    in_pair: bool,
    added: i64,
    poster: Vec<u8>,
    release: String,
    group: String,
    file: i64,
    files: i64,
    parts: i64,
    oldest: i64,
    first: i64,
    last: i64,
    generation: i64,
    probed: i64,
    lead: i64,
    cursor: Option<i64>,
}
impl Meta {
    fn text_field(&self) -> bool {
        matches!((self.tag, self.field), (b'F', 2..=4) | (b'U', 2 | 3) | (b'P', 1))
    }
    fn delimiter(&mut self, tx: &mut Transaction<'_>, b: u8) -> Result<()> {
        let csv = matches!((self.tag, self.field), (b'F', 9 | 10) | (b'U', 4));
        if csv {
            let value = self.number.take();
            match (self.tag, self.field) {
                (b'F', 9) => {
                    if let Some(part) = value {
                        self.lead += 1;
                        tx.execute("insert into linker_import_members values(0,$1,$2)", &[&self.lead, &part])?;
                    }
                    if !matches!(b, b',' | b'\t') {
                        self.valid = false;
                    }
                }
                (b'U', 4) => {
                    if let Some(group) = value {
                        tx.execute(
                            "insert into linker_import_members values(2,$1,0) on conflict do nothing",
                            &[&group],
                        )?;
                    }
                    if !matches!(b, b',' | b'\t') {
                        self.valid = false;
                    }
                }
                (b'F', 10) => {
                    if b == b':' && !self.in_pair {
                        self.pair = value;
                        self.in_pair = true;
                        return Ok(());
                    }
                    if self.in_pair
                        && let Some((group, number)) = self.pair.zip(value)
                    {
                        tx.execute("insert into linker_import_members values(1,$1,$2) on conflict(kind,key) do update set value=excluded.value", &[&group,&number])?;
                    }
                    self.pair = None;
                    self.in_pair = false;
                    if !matches!(b, b',' | b'\n') {
                        self.valid = false;
                    }
                }
                _ => unreachable!(),
            }
            if b == b',' {
                return Ok(());
            }
        } else if self.text_field() {
            let text = String::from_utf8_lossy(&self.text).into_owned();
            self.text.clear();
            match (self.tag, self.field) {
                (b'F', 2) => self.poster = text.into_bytes(),
                (b'F', 3) | (b'U', 2) | (b'P', 1) => self.release = text,
                (b'F', 4) | (b'U', 3) => self.group = text,
                _ => unreachable!(),
            }
        } else {
            let Some(value) = self.number.take() else {
                self.valid = false;
                return Ok(());
            };
            match (self.tag, self.field) {
                (b'F' | b'U', 1) => self.added = value,
                (b'F', 5) => self.file = value,
                (b'F', 6) | (b'U', 7) => self.files = value,
                (b'F', 7) => self.parts = value,
                (b'F', 8) => self.oldest = value,
                (b'U', 5) => self.first = value,
                (b'U', 6) => self.last = value,
                (b'U', 8) | (b'P', 2) => self.generation = value,
                (b'U', 9) | (b'P', 3) => self.probed = value,
                _ => self.valid = false,
            }
        }
        let last = match self.tag {
            b'F' => 10,
            b'U' => 9,
            b'P' => 3,
            _ => 0,
        };
        if b == b'\n' {
            self.valid &= self.field == last;
            self.stage = 2;
        } else if b == b'\t' {
            self.valid &= self.field < last;
            self.field += 1;
        } else {
            self.valid = false;
        }
        Ok(())
    }
}

pub(crate) fn step(client: &mut Client, dir: &Path, now: i64, budget: Budget) -> Result<bool> {
    const NAME: &str = "links.rec:metadata";
    let path = dir.join("links.rec");
    if !super::linker_import_scan::step(client, &path, "links.rec", budget)? {
        return Ok(false);
    }
    if !path.exists() {
        return Ok(true);
    }
    let mut file = File::open(&path)?;
    let identity = super::linker_import_scan::identity(&file)?;
    let mut tx = super::linker::durable_transaction(client)?;
    let saved = tx.query_opt(
        "select source_identity,state,complete from linker_import_jobs where name=$1 for update",
        &[&NAME],
    )?;
    let mut s: Meta = if let Some(row) = saved {
        ensure!(row.get::<_, Vec<u8>>(0) == identity, "legacy metadata changed during import");
        if row.get::<_, bool>(2) {
            return Ok(true);
        }
        serde_json::from_str(&row.get::<_, String>(1))?
    } else {
        Meta::default()
    };
    let mut done = false;
    match s.stage {
        0 => {
            if let Some((ordinal, start, end)) = super::linker_import_scan::next(&mut tx, "links.rec", s.ordinal)? {
                s = Meta { ordinal, offset: start, end, valid: true, ..Default::default() };
                file.seek(SeekFrom::Start(start))?;
                let mut tag = [0; 2];
                if end - start >= 2 {
                    file.read_exact(&mut tag)?;
                }
                if matches!(tag[0], b'F' | b'U' | b'P') && tag[1] == b'\t' {
                    s.tag = tag[0];
                    s.field = 1;
                    s.offset += 2;
                    s.stage = 1;
                }
                // Other record classes are already indexed; skip in O(1),
                // including an arbitrarily large historical I record.
            } else {
                s.stage = 30;
            }
        }
        1 => {
            file.seek(SeekFrom::Start(s.offset))?;
            let length = (s.end - s.offset).min(budget.bytes as u64) as usize;
            let mut bytes = vec![0; length];
            file.read_exact(&mut bytes)?;
            let mut tokens = 0;
            for b in bytes {
                s.offset += 1;
                let csv = matches!((s.tag, s.field), (b'F', 9 | 10) | (b'U', 4));
                let delimiter = b == b'\t'
                    || b == b'\n'
                    || csv && (b == b',' || s.tag == b'F' && s.field == 10 && b == b':' && !s.in_pair);
                if delimiter {
                    s.delimiter(&mut tx, b)?;
                    tokens += 1;
                } else if s.text_field() {
                    s.text.push(b);
                } else {
                    s.number.byte(b);
                }
                if !s.valid {
                    s.offset = s.end;
                    s.stage = 20;
                    break;
                }
                if s.stage != 1 || tokens >= budget.rows {
                    break;
                }
            }
        }
        2 => {
            s.stage = match s.tag {
                b'F' => 3,
                b'U' => 10,
                b'P' => 15,
                _ => 20,
            };
        }
        3 | 4 => {
            let (table, key) =
                if s.stage == 3 { ("linker_place_leads", "ordinal") } else { ("linker_place_firsts", "group_id") };
            let n=tx.execute(&format!("delete from {table} where poster_key=sha256($1) and poster=$1 and {key}=(select {key} from {table} where poster_key=sha256($1) and poster=$1 order by {key} limit 1)"), &[&s.poster])?;
            if n == 0 {
                s.stage += 1;
            }
        }
        5 => {
            tx.execute(
                "insert into linker_places values($1,$2,$3,$4,$5,$6,$7,$8) on conflict(poster_key) do update set
                release_name=excluded.release_name,group_name=excluded.group_name,file_number=excluded.file_number,
                file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",
                &[&s.poster, &s.release, &s.group, &s.file, &s.files, &s.parts, &s.oldest, &s.added],
            )?;
            s.stage = 6;
            s.cursor = None;
        }
        6 | 7 | 12 => {
            let kind = match s.stage {
                6 => 0i16,
                7 => 1,
                12 => 2,
                _ => unreachable!(),
            };
            let next = match s.cursor {
                Some(key) => tx.query_opt(
                    "select key,value from linker_import_members where kind=$1 and key>$2 order by key limit 1",
                    &[&kind, &key],
                )?,
                None => tx.query_opt(
                    "select key,value from linker_import_members where kind=$1 order by key limit 1",
                    &[&kind],
                )?,
            };
            if let Some(row) = next {
                let key: i64 = row.get(0);
                let value: i64 = row.get(1);
                s.cursor = Some(key);
                match s.stage {
                    6 => {
                        tx.execute("insert into linker_place_leads values($1,$2,$3)", &[&s.poster, &key, &value])?;
                    }
                    7 => {
                        tx.execute("insert into linker_place_firsts values($1,$2,$3)", &[&s.poster, &key, &value])?;
                    }
                    12 => {
                        tx.execute("insert into linker_upload_groups values($1,$2)", &[&s.release, &key])?;
                    }
                    _ => unreachable!(),
                }
            } else {
                s.stage = if s.stage == 6 { 7 } else { 20 };
                s.cursor = None;
            }
        }
        10 => {
            let n=tx.execute("delete from linker_upload_groups where release_name=$1 and group_id=(select group_id from linker_upload_groups where release_name=$1 order by group_id limit 1)", &[&s.release])?;
            if n == 0 {
                s.stage = 11;
            }
        }
        11 => {
            tx.execute(
                "insert into linker_uploads values($1,$2,$3,$4,$5,$6,$7,$8) on conflict(release_name) do update set
                group_name=excluded.group_name,first_posted=excluded.first_posted,last_posted=excluded.last_posted,
                files=excluded.files,generation=excluded.generation,probed=excluded.probed,added=excluded.added",
                &[&s.release, &s.group, &s.first, &s.last, &s.files, &s.generation, &s.probed, &s.added],
            )?;
            s.stage = 12;
            s.cursor = None;
        }
        15 => {
            tx.execute(
                "update linker_uploads set probed=greatest(probed,$3) where release_name=$1 and generation=$2",
                &[&s.release, &s.generation, &s.probed],
            )?;
            s.stage = 20;
        }
        20 => {
            let n=tx.execute("delete from linker_import_members where (kind,key)=(select kind,key from linker_import_members order by kind,key limit 1)", &[])?;
            if n == 0 {
                s = Meta { ordinal: s.ordinal, ..Default::default() };
            }
        }
        30 => {
            // Existing child-first expiry already provides a bounded commit.
            // End this transaction before invoking it; never nest callbacks.
            tx.rollback()?;
            match super::linker_gc::expire_metadata(client, now, i64::try_from(budget.rows)?)? {
                super::linker_gc::MetadataExpiry::Progress => return Ok(false),
                super::linker_gc::MetadataExpiry::Deferred => {
                    anyhow::bail!("bootstrap metadata expiry cannot overlap an epoch")
                }
                super::linker_gc::MetadataExpiry::Complete => done = true,
            }
            tx = super::linker::durable_transaction(client)?;
        }
        _ => unreachable!(),
    }
    tx.execute("insert into linker_import_jobs values($1,$2,$3,$4) on conflict(name) do update set state=excluded.state,complete=excluded.complete",
        &[&NAME,&identity,&serde_json::to_string(&s)?,&done])?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
pub(crate) fn import(client: &mut Client, dir: &Path, now: i64) -> Result<()> {
    while !step(client, dir, now, Budget::default())? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn huge_metadata_records_resume_members_and_expire_children_in_pages() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let groups = (1..=73).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let firsts = (1..=73).map(|n| format!("{n}:{}", n + 100)).collect::<Vec<_>>().join(",");
        let text = format!(
            "V\t2\nU\t1000\trelease\tgroup.example\t{groups}\t10\t20\t1\t2\t0\nF\t1000\tposter@example.invalid\trelease\tgroup.example\t1\t1\t73\t1000\t{groups}\t{firsts}\nC\t2\tok\n"
        );
        std::fs::write(dir.path().join("links.rec"), &text).unwrap();
        let mut c = schema.pool.get().unwrap();
        c.batch_execute("create table import_audit(tx bigint primary key, mutations bigint not null);
            create function audit_import() returns trigger language plpgsql as $$ begin
              insert into import_audit values(txid_current(),1) on conflict(tx) do update set mutations=import_audit.mutations+1;
              return null; end $$").unwrap();
        for table in [
            "linker_import_members",
            "linker_import_jobs",
            "linker_import_scans",
            "linker_import_lines",
            "linker_import_ranges",
            "linker_places",
            "linker_place_leads",
            "linker_place_firsts",
            "linker_uploads",
            "linker_upload_groups",
        ] {
            c.batch_execute(&format!("create trigger audit after insert or update or delete on {table} for each row execute function audit_import()" )).unwrap();
        }
        let budget = Budget { rows: 2, bytes: 17 };
        let mut interrupted = false;
        for turn in 0..2000 {
            assert!(turn < 1999, "metadata import stopped advancing");
            if step(&mut c, dir.path(), 1000, budget).unwrap() {
                break;
            }
            if !interrupted {
                if let Some(row) =
                    c.query_opt("select state from linker_import_jobs where name='links.rec:metadata'", &[]).unwrap()
                {
                    let state: Meta = serde_json::from_str(&row.get::<_, String>(0)).unwrap();
                    if state.stage == 1 && state.tag == b'U' && state.offset > 100 {
                        let offset = state.offset;
                        drop(c);
                        c = schema.pool.get().unwrap();
                        step(&mut c, dir.path(), 1000, budget).unwrap();
                        let state: Meta = serde_json::from_str(
                            &c.query_one("select state from linker_import_jobs where name='links.rec:metadata'", &[])
                                .unwrap()
                                .get::<_, String>(0),
                        )
                        .unwrap();
                        assert!(state.offset > offset && state.offset - offset <= 17);
                        interrupted = true;
                    }
                }
            }
        }
        assert!(interrupted);
        for table in ["linker_upload_groups", "linker_place_leads", "linker_place_firsts"] {
            assert_eq!(c.query_one(&format!("select count(*) from {table}"), &[]).unwrap().get::<_, i64>(0), 73);
        }
        assert!(c.query_one("select max(mutations) from import_audit", &[]).unwrap().get::<_, i64>(0) <= 5);
        // Resume the initial expiry subphase, retaining the frozen record and
        // using the same child-first page path as first bootstrap completion.
        let state = Meta { stage: 30, ..Default::default() };
        c.execute(
            "update linker_import_jobs set complete=false,state=$1 where name='links.rec:metadata'",
            &[&serde_json::to_string(&state).unwrap()],
        )
        .unwrap();
        assert!(!step(&mut c, dir.path(), 1000 + 9 * crate::xref::KEEP_SECS, budget).unwrap());
        drop(c);
        let mut c = schema.pool.get().unwrap();
        while !step(&mut c, dir.path(), 1000 + 9 * crate::xref::KEEP_SECS, budget).unwrap() {}
        assert_eq!(c.query_one("select count(*) from linker_places", &[]).unwrap().get::<_, i64>(0), 0);
        assert_eq!(c.query_one("select count(*) from linker_uploads", &[]).unwrap().get::<_, i64>(0), 0);
        assert_eq!(std::fs::read_to_string(dir.path().join("links.rec")).unwrap(), text);
    }

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
