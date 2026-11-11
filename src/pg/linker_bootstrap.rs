//! One-time state migration before native epoch work can start. The original
//! files remain unchanged; all import clocks and receipts live on the fenced
//! PostgreSQL session. No legacy Linker/Links/LinkedIds object is constructed.

use anyhow::{Context, Result, ensure};
use postgres::Client;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Finish any interrupted bootstrap and return its immutable migration clock.
pub(crate) fn import(client: &mut Client, dir: &Path, now: i64) -> Result<i64> {
    super::linker::durable_execute(
        client,
        "insert into linker_bootstrap(singleton,logical_now) values(true,$1) on conflict do nothing",
        &[&now],
    )?;
    let state = client.query_one("select logical_now,complete from linker_bootstrap where singleton", &[])?;
    let now: i64 = state.get(0);
    if state.get::<_, bool>(1) {
        return Ok(now);
    }
    super::linker_ids::import(client, dir, now)?;
    super::linker_metadata::import(client, dir, now)?;
    import_state(client, &dir.join("linker.rec"))?;
    super::linker::durable_execute(client, "update linker_bootstrap set complete=true where singleton", &[])?;
    Ok(now)
}

fn checked(line: &str) -> Result<()> {
    let f: Vec<_> = line.splitn(4, '\t').collect();
    match f.as_slice() {
        ["L", a, b] | ["S", a, b] => {
            a.parse::<i64>()?;
            b.parse::<i64>()?;
        }
        ["C", day, count, _] => {
            day.parse::<i64>()?;
            count.parse::<u32>()?;
        }
        ["W", at] => {
            at.parse::<i64>()?;
        }
        _ => {}
    }
    Ok(())
}

fn import_state(client: &mut Client, path: &Path) -> Result<()> {
    const NAME: &str = "linker.rec:state";
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let pending: bool = client
                .query_one("select exists(select 1 from linker_id_imports where name=$1 and not complete)", &[&NAME])?
                .get(0);
            ensure!(!pending, "legacy linker state disappeared during migration");
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
    // Validate first: a truncated/invalid state must not partially acknowledge
    // historical hours. This scan retains only one compact scalar record.
    let mut ended = false;
    for line in BufReader::new(file).lines() {
        let line = line?;
        checked(&line).context("invalid legacy linker state")?;
        if line == "E" {
            ended = true;
        }
    }
    ensure!(ended, "legacy linker state is cut short");
    super::linker::durable_execute(
        client,
        "insert into linker_id_imports(name,source_identity) values($1,$2) on conflict do nothing",
        &[&NAME, &&identity[..]],
    )?;
    let saved = client
        .query_one("select source_identity,offset_bytes,complete from linker_id_imports where name=$1", &[&NAME])?;
    ensure!(saved.get::<_, Vec<u8>>(0) == identity, "legacy linker state changed during migration");
    if saved.get::<_, bool>(2) {
        return Ok(());
    }
    // offset_bytes here is the ordinal of a state record, never expanded hours.
    let done: i64 = saved.get(1);
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let index = i64::try_from(index)? + 1;
        let line = line?;
        if index <= done {
            continue;
        }
        let f: Vec<_> = line.splitn(4, '\t').collect();
        let mut tx = super::linker::durable_transaction(client)?;
        tx.batch_execute("set local work_mem='4MB'")?;
        match f.as_slice() {
            ["L", a, b] => {
                tx.execute("insert into linker_linked_hours select generate_series($1::bigint,$2::bigint) on conflict do nothing",&[&a.parse::<i64>()?,&b.parse::<i64>()?])?;
            }
            ["S", day, at] => {
                tx.execute("insert into linker_day_seen values($1,$2) on conflict(day) do update set first_seen=excluded.first_seen",&[&day.parse::<i64>()?,&at.parse::<i64>()?])?;
            }
            ["C", day, count, group] => {
                tx.execute("insert into linker_legacy_counts values($1,$2,$3) on conflict(group_name,day) do update set blocks=excluded.blocks",&[group,&day.parse::<i64>()?,&(count.parse::<u32>()? as i64)])?;
            }
            ["W", at] => {
                tx.execute("update linker_bootstrap set waiting_from=$1 where singleton", &[&at.parse::<i64>()?])?;
            }
            _ => {}
        }
        tx.execute("update linker_id_imports set offset_bytes=$2 where name=$1", &[&NAME, &index])?;
        tx.commit()?;
    }
    super::linker::durable_execute(client, "update linker_id_imports set complete=true where name=$1", &[&NAME])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
