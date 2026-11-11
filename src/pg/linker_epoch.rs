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
    tx.execute("update linker_epochs set pending_source_through=(select coalesce(max(id),0) from linker_source_blocks) where id=$1",&[&epoch])?;
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

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Scope {
    part: u8,
    inventory: Option<(String, i64, i64)>,
    pending: i64,
    day: Option<i64>,
    hour_offset: i64,
    hour: Option<i64>,
    first: Option<i64>,
    last: Option<i64>,
    source: i64,
}

/// Source capture, day/hour reduction, readiness and scope membership each
/// advance by indexed keys. No retry repeats a completed whole-inbox phase.
pub(crate) fn scope_step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "scope budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let row = tx.query_one(
        "select phase,logical_now,inventory_sealed,pending_source_through from linker_epochs where id=$1 for update",
        &[&epoch],
    )?;
    if row.get::<_, String>(0) != "stage" {
        return Ok(true);
    }
    ensure!(row.get::<_, bool>(2), "scope requires sealed inventory");
    ensure!(
        !tx.query_one(
            "select exists(select 1 from linker_epoch_inventory where epoch=$1 and scanned_blocks<blocks)",
            &[&epoch]
        )?
        .get::<_, bool>(0),
        "scope requires staged sources"
    );
    let now: i64 = row.get(1);
    let through: i64 = row.get(3);
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='scope'", &[&epoch])?;
    let mut state: Scope = saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let mut done = false;
    for _ in 0..budget {
        match state.part {
            0 => {
                let after = state.inventory.clone().unwrap_or((String::new(), i64::MIN, i64::MIN));
                let next = tx.query_opt(
                    "select group_name,day,seq,source from linker_inventory_blocks
                    where epoch=$1 and (group_name,day,seq)>($2,$3,$4) order by group_name,day,seq limit 1",
                    &[&epoch, &after.0, &after.1, &after.2],
                )?;
                if let Some(row) = next {
                    state.inventory = Some((row.get(0), row.get(1), row.get(2)));
                    capture(&mut tx, epoch, row.get(3))?;
                } else {
                    state.part = 1;
                }
            }
            1 => {
                let next=tx.query_opt("select id,complete,exists(select 1 from linker_staged_rows r where r.source=b.id and not r.published)
                    from linker_source_blocks b where id>$1 and id<=$2 order by id limit 1", &[&state.pending,&through])?;
                if let Some(row) = next {
                    state.pending = row.get(0);
                    if row.get::<_, bool>(1) && row.get::<_, bool>(2) {
                        capture(&mut tx, epoch, state.pending)?;
                    }
                } else {
                    state.part = 2;
                }
            }
            2 => {
                if state.hour_offset == 0 {
                    let next = tx.query_opt(
                        "select day from linker_scope_days where epoch=$1 and day>$2 order by day limit 1",
                        &[&epoch, &state.day.unwrap_or(i64::MIN)],
                    )?;
                    let Some(row) = next else {
                        state.part = 3;
                        continue;
                    };
                    state.day = Some(row.get(0));
                }
                let hour = state.day.unwrap() * 24 - 14 + state.hour_offset;
                tx.execute("insert into linker_scope_hours values($1,$2) on conflict do nothing", &[&epoch, &hour])?;
                state.hour_offset = (state.hour_offset + 1) % 52;
            }
            3 => {
                let next = tx.query_opt(
                    "select hour from linker_scope_hours where epoch=$1 and hour>$2 order by hour limit 1",
                    &[&epoch, &state.hour.unwrap_or(i64::MIN)],
                )?;
                if let Some(row) = next {
                    let hour: i64 = row.get(0);
                    state.hour = Some(hour);
                    let linked: bool = tx
                        .query_one("select exists(select 1 from linker_linked_hours where hour=$1)", &[&hour])?
                        .get(0);
                    if !linked && ready(&mut tx, hour, now)? {
                        state.first = Some(hour);
                        state.last = Some(hour);
                        state.part = 4;
                        tx.execute("insert into linker_epoch_hours values($1,$2)", &[&epoch, &hour])?;
                    }
                } else {
                    state.part = 5;
                }
            }
            4 => {
                let first = state.first.unwrap();
                let hour = state.last.unwrap() + 1;
                if hour >= first + crate::nxv_store::linker::SPAN_HOURS {
                    state.part = 5;
                    continue;
                }
                let eligible: bool = tx
                    .query_one(
                        "select exists(select 1 from linker_scope_hours where epoch=$1 and hour=$2)
                    and not exists(select 1 from linker_linked_hours where hour=$2)",
                        &[&epoch, &hour],
                    )?
                    .get(0);
                if eligible && ready(&mut tx, hour, now)? {
                    tx.execute("insert into linker_epoch_hours values($1,$2)", &[&epoch, &hour])?;
                    state.last = Some(hour);
                } else {
                    state.part = 5;
                }
            }
            5 => {
                let waiting: Option<i64> = tx
                    .query_one(
                        "select case when exists(select 1 from linker_epochs where id<$1 and phase='done')
                    then (select waiting_from from linker_epochs where id<$1 and phase='done' order by id desc limit 1)
                    else (select waiting_from from linker_bootstrap where singleton) end",
                        &[&epoch],
                    )?
                    .get(0);
                let (lo, hi) = match state.first.zip(state.last) {
                    Some((first, last)) => {
                        let lo = first * HOUR - MARGIN;
                        (
                            Some(waiting.map_or(lo, |w| lo.min(w.max(lo - crate::xref::KEEP_SECS)))),
                            Some((last + 1) * HOUR + MARGIN - 1),
                        )
                    }
                    None => (None, None),
                };
                tx.execute(
                    "update linker_epochs set lo=$2,hi=$3,waiting_from=$4 where id=$1",
                    &[&epoch, &lo, &hi, &waiting],
                )?;
                state.part = 6;
            }
            6 => {
                let next = tx.query_opt(
                    "select source,day,old_block,new_block from linker_scope_sources
                    where epoch=$1 and source>$2 order by source limit 1",
                    &[&epoch, &state.source],
                )?;
                let Some(row) = next else {
                    let selected: bool = tx
                        .query_one("select exists(select 1 from linker_epoch_sources where epoch=$1)", &[&epoch])?
                        .get(0);
                    let phase = if state.first.is_none() && !selected { "done" } else { "select" };
                    tx.execute("update linker_epochs set phase=$2 where id=$1", &[&epoch, &phase])?;
                    done = true;
                    break;
                };
                let source: i64 = row.get(0);
                let day: i64 = row.get(1);
                state.source = source;
                let limits = tx.query_one("select lo,hi from linker_epochs where id=$1", &[&epoch])?;
                let within = limits
                    .get::<_, Option<i64>>(0)
                    .zip(limits.get::<_, Option<i64>>(1))
                    .is_some_and(|(lo, hi)| day >= (lo - ZONE).div_euclid(DAY) && day <= (hi + ZONE).div_euclid(DAY));
                // 52 fixed hour keys, independent of source/upload/catalog size.
                let late: bool = if within {
                    false
                } else {
                    tx.query_one(
                        "select has_new and not exists(
                    select 1 from generate_series($3::bigint,$3::bigint+51) h
                    where not exists(select 1 from linker_linked_hours l where l.hour=h))
                    from linker_scope_days where epoch=$1 and day=$2",
                        &[&epoch, &day, &(day * 24 - 14)],
                    )?
                    .get(0)
                };
                if within || late {
                    tx.execute(
                        "insert into linker_epoch_sources(epoch,source,old_block,new_block) values($1,$2,$3,$4)",
                        &[&epoch, &source, &row.get::<_, bool>(2), &row.get::<_, bool>(3)],
                    )?;
                }
            }
            _ => unreachable!(),
        }
    }
    tx.execute(
        "insert into linker_work values($1,'scope',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&state)?],
    )?;
    tx.commit()?;
    Ok(done)
}

fn capture(tx: &mut Transaction<'_>, epoch: i64, source: i64) -> Result<()> {
    let row = tx.query_one("select day,was_read,legacy_priority from linker_source_blocks where id=$1", &[&source])?;
    let day: i64 = row.get(0);
    let read: bool = row.get(1);
    let old = read || row.get::<_, bool>(2);
    tx.execute(
        "insert into linker_scope_sources values($1,$2,$3,$4,$5) on conflict do nothing",
        &[&epoch, &source, &day, &old, &!read],
    )?;
    tx.execute("insert into linker_scope_days values($1,$2,$3) on conflict(epoch,day) do update set has_new=linker_scope_days.has_new or excluded.has_new", &[&epoch,&day,&!read])?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn scope(client: &mut Client, epoch: i64) -> Result<bool> {
    while !scope_step(client, epoch, 2)? {}
    Ok(client.query_one("select phase<>'done' from linker_epochs where id=$1", &[&epoch])?.get(0))
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
