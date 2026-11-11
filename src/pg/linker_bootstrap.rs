//! One-time bounded import driver. No epoch can observe partial imported
//! records, metadata replacement, expiry, or historical-hour expansion.
use super::linker_metadata::Number;
use super::linker_stage::Budget;
use anyhow::{Result, ensure};
use postgres::Client;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub(crate) fn step(client: &mut Client, dir: &Path, now: i64, budget: Budget) -> Result<bool> {
    super::linker::durable_execute(
        client,
        "insert into linker_bootstrap(singleton,logical_now) values(true,$1) on conflict do nothing",
        &[&now],
    )?;
    let row =
        client.query_one("select logical_now,complete,import_phase from linker_bootstrap where singleton", &[])?;
    if row.get::<_, bool>(1) {
        return Ok(true);
    }
    let now: i64 = row.get(0);
    let phase: i16 = row.get(2);
    let finished = match phase {
        0 => super::linker_ids::step(client, dir, now, budget)?,
        1 => super::linker_metadata::step(client, dir, now, budget)?,
        2 => state_step(client, &dir.join("linker.rec"), budget)?,
        _ => true,
    };
    if finished {
        super::linker::durable_execute(
            client,
            "update linker_bootstrap set import_phase=import_phase+1,complete=$1 where singleton",
            &[&(phase >= 2)],
        )?;
    }
    Ok(finished && phase >= 2)
}

#[cfg(test)]
pub(crate) fn import(client: &mut Client, dir: &Path, now: i64) -> Result<i64> {
    while !step(client, dir, now, Budget::default())? {}
    Ok(client.query_one("select logical_now from linker_bootstrap where singleton", &[])?.get(0))
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct State {
    phase: u8,
    ordinal: i64,
    offset: u64,
    end: u64,
    tag: u8,
    field: u8,
    number: Number,
    text: Vec<u8>,
    a: i64,
    b: i64,
    invalid_numeric: bool,
    applied: i64,
    range: Option<(i64, i64)>,
}
impl State {
    fn delimiter(&mut self, b: u8) -> Result<()> {
        if !(self.tag == b'C' && self.field == 3) {
            match self.number.take() {
                Some(value) => {
                    if self.field == 1 {
                        self.a = value;
                    } else if self.field == 2 {
                        self.b = value;
                    }
                }
                None => self.invalid_numeric = true,
            }
        }
        let last = if self.tag == b'W' {
            1
        } else if self.tag == b'C' {
            3
        } else {
            2
        };
        if b == b'\n' {
            // Legacy splitn parsing ignores wrong-shaped records altogether;
            // numeric errors are fatal only for a recognized complete shape.
            if self.field != last {
                self.phase = 0;
                return Ok(());
            }
            ensure!(!self.invalid_numeric, "invalid legacy numeric state");
            if self.tag == b'C' {
                ensure!((0..=u32::MAX as i64).contains(&self.b), "invalid legacy block count");
            }
            self.phase = 2;
        } else {
            self.field = self.field.saturating_add(1);
        }
        Ok(())
    }
}

fn state_step(client: &mut Client, path: &Path, budget: Budget) -> Result<bool> {
    const NAME: &str = "linker.rec:state";
    if !super::linker_import_scan::step(client, path, "linker.rec", budget)? {
        return Ok(false);
    }
    if !path.exists() {
        return Ok(true);
    }
    ensure!(
        client.query_one("select ended from linker_import_scans where name='linker.rec'", &[])?.get::<_, bool>(0),
        "legacy linker state is cut short"
    );
    let mut file = File::open(path)?;
    let identity = super::linker_import_scan::identity(&file)?;
    let mut tx = super::linker::durable_transaction(client)?;
    let saved = tx.query_opt(
        "select source_identity,state,complete from linker_import_jobs where name=$1 for update",
        &[&NAME],
    )?;
    let mut s: State = if let Some(row) = saved {
        ensure!(row.get::<_, Vec<u8>>(0) == identity, "legacy state changed during import");
        if row.get::<_, bool>(2) {
            return Ok(true);
        }
        serde_json::from_str(&row.get::<_, String>(1))?
    } else {
        State::default()
    };
    let mut done = false;
    match s.phase {
        0 => {
            if let Some((ordinal, start, end)) = super::linker_import_scan::next(&mut tx, "linker.rec", s.ordinal)? {
                s = State { ordinal, offset: start, end, ..Default::default() };
                file.seek(SeekFrom::Start(start))?;
                let mut tag = [0; 2];
                if end - start >= 2 {
                    file.read_exact(&mut tag)?;
                }
                if matches!(tag[0], b'L' | b'S' | b'C' | b'W') && tag[1] == b'\t' {
                    s.tag = tag[0];
                    s.field = 1;
                    s.offset += 2;
                    s.phase = 1;
                }
            } else {
                s.phase = 3;
            }
        }
        1 => {
            file.seek(SeekFrom::Start(s.offset))?;
            let length = (s.end - s.offset).min(budget.bytes as u64) as usize;
            let mut bytes = vec![0; length];
            file.read_exact(&mut bytes)?;
            for b in bytes {
                s.offset += 1;
                if b == b'\n' || b == b'\t' && !(s.tag == b'C' && s.field == 3) {
                    s.delimiter(b)?;
                } else if s.tag == b'C' && s.field == 3 {
                    s.text.push(b);
                } else {
                    s.number.byte(b);
                }
            }
            // BufRead::lines historically accepts a final scalar record with
            // no newline. The durable index preserves that state-file case.
            if s.offset == s.end && s.phase == 1 {
                s.delimiter(b'\n')?;
            }
        }
        2 => {
            tx.execute(
                "insert into linker_import_state_records values($1,$2,$3,$4,$5)",
                &[&s.ordinal, &i16::from(s.tag), &s.a, &s.b, &String::from_utf8_lossy(&s.text).as_ref()],
            )?;
            s = State { ordinal: s.ordinal, ..Default::default() };
        }
        3 => {
            // Every accepted scalar record has validated before ANY historical
            // hour becomes visible; an enormous L interval is then paged.
            if let Some((next,last))=s.range {
                let end=last.min(next.saturating_add(i64::try_from(budget.rows)?-1));
                tx.execute("insert into linker_linked_hours select generate_series($1::bigint,$2::bigint) on conflict do nothing", &[&next,&end])?;
                s.range=if end==last {None}else{Some((end+1,last))};
            }else if let Some(row)=tx.query_opt("select ordinal,tag,a,b,group_name from linker_import_state_records where ordinal>$1 order by ordinal limit 1", &[&s.applied])? {
                s.applied=row.get(0);let a:i64=row.get(2);let b:i64=row.get(3);
                match u8::try_from(row.get::<_,i16>(1))? {
                    b'L'=>{if a<=b {s.range=Some((a,b));}}
                    b'S'=>{tx.execute("insert into linker_day_seen values($1,$2) on conflict(day) do update set first_seen=excluded.first_seen", &[&a,&b])?;}
                    b'C'=>{tx.execute("insert into linker_legacy_counts values($1,$2,$3) on conflict(group_name,day) do update set blocks=excluded.blocks", &[&row.get::<_,String>(4),&a,&b])?;}
                    b'W'=>{tx.execute("update linker_bootstrap set waiting_from=$1 where singleton", &[&a])?;}
                    _=>unreachable!(),
                }
            }else{done=true;}
        }
        _ => unreachable!(),
    }
    tx.execute("insert into linker_import_jobs values($1,$2,$3,$4) on conflict(name) do update set state=excluded.state,complete=excluded.complete", &[&NAME,&identity,&serde_json::to_string(&s)?,&done])?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_range_pages_survive_failure_without_replaying_completed_hours() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let text = "L\t1\t137\nL\tbad\t2\textra\nS\t2\t50\nC\t2\t7\tgroup.example\nW\t100\nE";
        std::fs::write(dir.path().join("linker.rec"), text).unwrap();
        let mut c = schema.pool.get().unwrap();
        c.batch_execute("create table bootstrap_audit(tx bigint primary key, mutations bigint not null);
            create function audit_bootstrap() returns trigger language plpgsql as $$ begin
              insert into bootstrap_audit values(txid_current(),1) on conflict(tx) do update set mutations=bootstrap_audit.mutations+1;
              return null; end $$;
            create function fail_hour() returns trigger language plpgsql as $$ begin
              if new.hour=42 then raise exception 'synthetic range storage exhaustion' using errcode='53100'; end if;return new;end $$;
            create trigger fail before insert on linker_linked_hours for each row execute function fail_hour()").unwrap();
        for table in [
            "linker_bootstrap",
            "linker_import_scans",
            "linker_import_lines",
            "linker_import_ranges",
            "linker_import_jobs",
            "linker_import_state_records",
            "linker_linked_hours",
            "linker_day_seen",
            "linker_legacy_counts",
        ] {
            c.batch_execute(&format!("create trigger audit after insert or update or delete on {table} for each row execute function audit_bootstrap()" )).unwrap();
        }
        let budget = Budget { rows: 1, bytes: 7 };
        for turn in 0..500 {
            assert!(turn < 499, "range did not reach injected failure");
            match step(&mut c, dir.path(), 1000, budget) {
                Ok(false) => {}
                Ok(true) => panic!("range skipped injected hour"),
                Err(_) => break,
            }
        }
        assert_eq!(c.query_one("select count(*) from linker_linked_hours", &[]).unwrap().get::<_, i64>(0), 41);
        let state: State = serde_json::from_str(
            &c.query_one("select state from linker_import_jobs where name='linker.rec:state'", &[])
                .unwrap()
                .get::<_, String>(0),
        )
        .unwrap();
        assert_eq!(state.range, Some((42, 137)));
        drop(c);
        let mut c = schema.pool.get().unwrap();
        c.batch_execute("drop trigger fail on linker_linked_hours").unwrap();
        while !step(&mut c, dir.path(), 9000, budget).unwrap() {}
        assert_eq!(c.query_one("select count(*) from linker_linked_hours", &[]).unwrap().get::<_, i64>(0), 137);
        assert_eq!(c.query_one("select logical_now from linker_bootstrap", &[]).unwrap().get::<_, i64>(0), 1000);
        assert!(c.query_one("select max(mutations) from bootstrap_audit", &[]).unwrap().get::<_, i64>(0) <= 3);
        assert_eq!(std::fs::read_to_string(dir.path().join("linker.rec")).unwrap(), text);
    }

    #[test]
    fn bootstrap_preserves_legacy_ranges_counts_and_frozen_clock() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("linker.rec"), "L\t1\t1000\nS\t2\t50\nC\t2\t7\tgroup.example\nW\t100\nE\n")
            .unwrap();
        let mut c = schema.pool.get().unwrap();
        assert_eq!(import(&mut c, dir.path(), 1000).unwrap(), 1000);
        assert_eq!(import(&mut c, dir.path(), 2000).unwrap(), 1000);
        let hours: i64 = c.query_one("select count(*) from linker_linked_hours", &[]).unwrap().get(0);
        assert_eq!(hours, 1000);
        let count: i64 = c
            .query_one("select blocks from linker_legacy_counts where group_name='group.example' and day=2", &[])
            .unwrap()
            .get(0);
        assert_eq!(count, 7);
        let waiting: Option<i64> =
            c.query_one("select waiting_from from linker_bootstrap where singleton", &[]).unwrap().get(0);
        assert_eq!(waiting, Some(100));
    }

    #[test]
    fn bootstrap_refuses_torn_state_before_acknowledging_hours() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("linker.rec"), "L\t1\t1000\n").unwrap();
        let mut c = schema.pool.get().unwrap();
        assert!(import(&mut c, dir.path(), 1000).is_err());
        let hours: i64 = c.query_one("select count(*) from linker_linked_hours", &[]).unwrap().get(0);
        assert_eq!(hours, 0);
    }
}
