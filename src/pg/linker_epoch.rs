//! Epoch scope and readiness. PostgreSQL owns historical sets; Rust retains at
//! most the three ready hours of the legacy span contract. All time decisions
//! use the epoch clock and are sealed before row selection begins.

use anyhow::{Result, ensure};
use postgres::{Client, Transaction};

const HOUR: i64 = 3600;
const DAY: i64 = 86400;
const ZONE: i64 = 14 * HOUR;
const MARGIN: i64 = 2 * HOUR;

/// Resume the only incomplete epoch, or allocate a frozen logical clock for a
/// new one. Called only on the fenced ownership session.
pub(crate) fn open(client: &mut Client, now: i64) -> Result<i64> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    if let Some(row) = tx.query_opt("select id from linker_epochs where phase<>'done' order by id limit 1", &[])? {
        return Ok(row.get(0));
    }
    let epoch: i64 = tx.query_one("insert into linker_epochs(logical_now) values($1) returning id", &[&now])?.get(0);
    tx.execute(
        "insert into linker_epoch_pending_sources(epoch,source)
        select $1,b.id from linker_source_blocks b where complete and exists
        (select 1 from linker_staged_rows s where s.source=b.id and not published)",
        &[&epoch],
    )?;
    tx.commit()?;
    Ok(epoch)
}

fn ready(tx: &mut Transaction<'_>, hour: i64, now: i64) -> Result<bool> {
    let end = (hour + 1) * HOUR;
    if end + MARGIN + crate::xref::SETTLE_SECS > now {
        return Ok(false);
    }
    let (first, last) = ((hour * HOUR - MARGIN - ZONE).div_euclid(DAY), (end - 1 + MARGIN + ZONE).div_euclid(DAY));
    let pending: bool = tx
        .query_one(
            "select exists(select 1 from backfill_chunks where state<>2 and day between $1 and $2
        and day>=coalesce((select value from meta where key='backfill_forward_start'),'-9223372036854775808'::bigint))",
            &[&first, &last],
        )?
        .get(0);
    if !pending {
        return Ok(true);
    }
    let (first, last) = ((hour * HOUR - ZONE).div_euclid(DAY), (end + ZONE).div_euclid(DAY));
    Ok(tx
        .query_one(
            "select count(*)>0 and coalesce(bool_and($3-first_seen>$4),false)
        from linker_day_seen where day between $1 and $2",
            &[&first, &last, &now, &crate::xref::KEEP_SECS],
        )?
        .get(0))
}

/// Seal the same ready span and late-only day scope as the old linker, without
/// materializing historical day/hour sets. Returns false when no work is ready.
/// Inventory and all its source blocks must already be durably staged.
pub(crate) fn scope(client: &mut Client, epoch: i64) -> Result<bool> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state =
        tx.query_one("select phase,logical_now,inventory_sealed from linker_epochs where id=$1 for update", &[&epoch])?;
    let phase: String = state.get(0);
    if phase != "stage" {
        return Ok(phase != "done");
    }
    let now: i64 = state.get(1);
    ensure!(state.get::<_, bool>(2), "native linker scope requires a sealed source inventory");
    let incomplete: bool = tx
        .query_one(
            "select exists(select 1 from linker_epoch_inventory where epoch=$1 and scanned_blocks<>blocks)",
            &[&epoch],
        )?
        .get(0);
    ensure!(!incomplete, "native linker scope requires complete source staging");
    // External set evaluation avoids rescanning a prefix of pending hours on
    // every client round trip. PostgreSQL spills the distinct day/hour set.
    let first:Option<i64>=tx.query_one("with hours as (
        select distinct generate_series(day*24-14,day*24+37) as h from (
          select day from linker_epoch_inventory where epoch=$1 and blocks>0 union
          select b.day from linker_epoch_pending_sources p join linker_source_blocks b on b.id=p.source where p.epoch=$1) days)
        select min(h) from hours where not exists(select 1 from linker_linked_hours l where l.hour=h)
        and (h+1)*3600+$4<=$2 and (
          not exists(select 1 from backfill_chunks where state<>2
            and day between floor((h*3600-57600)::numeric/86400)::bigint
                        and floor(((h+1)*3600-1+57600)::numeric/86400)::bigint
            and day>=coalesce((select value from meta where key='backfill_forward_start'),'-9223372036854775808'::bigint))
          or (select count(*)>0 and coalesce(bool_and($2-first_seen>$3),false) from linker_day_seen
              where day between floor((h*3600-50400)::numeric/86400)::bigint
                            and floor(((h+1)*3600+50400)::numeric/86400)::bigint))",
        &[&epoch,&now,&crate::xref::KEEP_SECS,&(MARGIN+crate::xref::SETTLE_SECS)])?.get(0);
    let mut last = first;
    if let Some(first) = first {
        for hour in first..first + crate::nxv_store::linker::SPAN_HOURS {
            let eligible: bool = tx
                .query_one(
                    "select (exists(select 1 from linker_epoch_inventory where epoch=$1 and blocks>0
                and $2 between day*24-14 and day*24+37) or exists(
                  select 1 from linker_epoch_pending_sources p join linker_source_blocks b on b.id=p.source
                  where p.epoch=$1 and $2 between b.day*24-14 and b.day*24+37))
                and not exists(select 1 from linker_linked_hours where hour=$2)",
                    &[&epoch, &hour],
                )?
                .get(0);
            if !eligible || !ready(&mut tx, hour, now)? {
                break;
            }
            tx.execute("insert into linker_epoch_hours values($1,$2)", &[&epoch, &hour])?;
            last = Some(hour);
        }
    }
    let waiting: Option<i64> = tx
        .query_opt(
            "select case when exists(select 1 from linker_epochs where id<$1 and phase='done')
                 then (select waiting_from from linker_epochs where id<$1 and phase='done' order by id desc limit 1)
                 else (select waiting_from from linker_bootstrap where singleton) end",
            &[&epoch],
        )?
        .and_then(|r| r.get(0));
    let (lo, hi) = match (first, last) {
        (Some(first), Some(last)) => {
            let lo = first * HOUR - MARGIN;
            (
                Some(waiting.map_or(lo, |w| lo.min(w.max(lo - crate::xref::KEEP_SECS)))),
                Some((last + 1) * HOUR + MARGIN - 1),
            )
        }
        _ => (None, None),
    };
    // Sources selected earlier keep their old-block precedence. New blocks of
    // entirely linked days are read together, regardless of historical span.
    tx.execute("delete from linker_epoch_sources where epoch=$1", &[&epoch])?;
    tx.execute("with captured as (
          select source from linker_inventory_blocks where epoch=$1 union
          select source from linker_epoch_pending_sources where epoch=$1
        ) insert into linker_epoch_sources(epoch,source,old_block,new_block)
        select $1,b.id,(b.was_read or b.legacy_priority),not b.was_read from captured join linker_source_blocks b on b.id=captured.source
        where (
        ($2::bigint is not null and b.day between floor(($2-$4)::numeric/86400)::bigint and floor(($3+$4)::numeric/86400)::bigint)
          or (not exists(select 1 from generate_series(b.day*24-14,b.day*24+37) h
                where not exists(select 1 from linker_linked_hours l where l.hour=h))
              and exists(select 1 from captured ib join linker_source_blocks late on late.id=ib.source
                  where late.day=b.day and not late.was_read)))",
        &[&epoch,&lo,&hi,&ZONE])?;
    let selected: bool =
        tx.query_one("select exists(select 1 from linker_epoch_sources where epoch=$1)", &[&epoch])?.get(0);
    if first.is_none() && !selected {
        tx.execute("update linker_epochs set phase='done',waiting_from=$2 where id=$1", &[&epoch, &waiting])?;
        tx.commit()?;
        return Ok(false);
    }
    tx.execute(
        "update linker_epochs set lo=$2,hi=$3,phase='select',waiting_from=$4 where id=$1",
        &[&epoch, &lo, &hi, &waiting],
    )?;
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashSet};

    #[test]
    fn cleared_waiting_summary_does_not_revive_bootstrap_delay() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        c.batch_execute(
            "insert into linker_bootstrap(singleton,logical_now,waiting_from) values(true,100000,90000);
            insert into linker_epochs(logical_now,phase,waiting_from) values(100000,'done',null)",
        )
        .unwrap();
        let epoch = open(&mut c, 200000).unwrap();
        c.execute("update linker_epochs set inventory_sealed=true where id=$1", &[&epoch]).unwrap();
        assert!(!scope(&mut c, epoch).unwrap());
        assert_eq!(
            c.query_one("select waiting_from from linker_epochs where id=$1", &[&epoch])
                .unwrap()
                .get::<_, Option<i64>>(0),
            None
        );
    }

    #[test]
    fn epoch_scope_uses_frozen_inventory_and_three_hour_span() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let now = 300 * DAY;
        let day = 298i64;
        let epoch = open(&mut c, now).unwrap();
        assert_eq!(open(&mut c, now + 1000).unwrap(), epoch);
        c.execute(
            "insert into linker_epoch_inventory(epoch,group_name,day,generation,blocks,scanned_blocks) values($1,'group.example',$2,$3,1,1)",
            &[&epoch, &day, &&[1u8][..]],
        )
        .unwrap();
        let source: i64 = c
            .query_one(
                "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete)
            values('group.example',$1,$2,0,$2,1,1,true) returning id",
                &[&day, &&[1u8][..]],
            )
            .unwrap()
            .get(0);
        c.execute("insert into linker_inventory_blocks values($1,'group.example',$2,0,$3)", &[&epoch, &day, &source])
            .unwrap();
        // A recreated/appended source not in the snapshot cannot enter its plan.
        c.execute(
            "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete)
            values('group.example',$1,$2,0,$2,1,1,true)",
            &[&day, &&[2u8][..]],
        )
        .unwrap();
        c.execute("update linker_epochs set inventory_sealed=true where id=$1", &[&epoch]).unwrap();
        assert!(scope(&mut c, epoch).unwrap());
        assert!(scope(&mut c, epoch).unwrap());
        let counts = c
            .query_one(
                "select (select count(*) from linker_epoch_hours where epoch=$1),
            (select count(*) from linker_epoch_sources where epoch=$1)",
                &[&epoch],
            )
            .unwrap();
        assert_eq!(counts.get::<_, i64>(0), 3);
        assert_eq!(counts.get::<_, i64>(1), 1);
        let row = c.query_one("select logical_now,lo,phase from linker_epochs where id=$1", &[&epoch]).unwrap();
        assert_eq!(row.get::<_, i64>(0), now);
        assert_eq!(row.get::<_, i64>(1), (day * 24 - 14) * HOUR - MARGIN);
        assert_eq!(row.get::<_, String>(2), "select");
    }

    #[test]
    fn epoch_readiness_matches_legacy_zone_margin_and_fallback_rules() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let now = 300 * DAY;
        let days = HashSet::from([298i64, 299]);
        for &day in &days {
            c.execute("insert into backfill_chunks(grp,day,state,server,claimed_at) values('group.example',$1,1,'provider.example',1)",&[&day]).unwrap();
        }
        for first_seen in [now - crate::xref::KEEP_SECS - 1, now - crate::xref::KEEP_SECS, now] {
            let seen = BTreeMap::from([(298i64, first_seen), (299, first_seen)]);
            c.execute("delete from linker_day_seen", &[]).unwrap();
            for (&day, &at) in &seen {
                c.execute("insert into linker_day_seen values($1,$2)", &[&day, &at]).unwrap();
            }
            let mut tx = c.transaction().unwrap();
            for h in [298 * 24 - 15, 298 * 24, 298 * 24 + 23, 299 * 24 - 1, 299 * 24 + 12, 299 * 24 + 23, 300 * 24] {
                assert_eq!(
                    ready(&mut tx, h, now).unwrap(),
                    crate::nxv_store::linker::ready(h, now, &days, &seen),
                    "hour {h} seen {first_seen}"
                );
            }
        }
    }
}
