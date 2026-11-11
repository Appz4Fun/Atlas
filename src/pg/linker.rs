//! Disk-backed native linker planning. File summaries, active chains and group
//! memberships live in PostgreSQL; the planner retains only one file/chain ID.
//! Each step commits its membership and cursor together. Retrying a step after
//! an ambiguous commit selects the next file, never appends the old one twice.

use anyhow::{Context, Result, ensure};
use postgres::Client;

/// Coverage/publication receipts must survive a database restart before raw
/// expiry can observe them, even when role/database/URL defaults disable WAL
/// flush on commit. Keep this fence local to each transaction.
pub(crate) fn durable_transaction(client: &mut Client) -> Result<postgres::Transaction<'_>> {
    let mut tx = client.transaction()?;
    tx.batch_execute("set local synchronous_commit=on")?;
    Ok(tx)
}

pub(crate) fn durable_execute(
    client: &mut Client,
    query: &str,
    params: &[&(dyn postgres::types::ToSql + Sync)],
) -> Result<u64> {
    let mut tx = durable_transaction(client)?;
    let changed = tx.execute(query, params)?;
    tx.commit()?;
    Ok(changed)
}

/// Execute at most `budget` indexed microsteps. `true` means all files are assigned.
/// Call only on the ownership session. Staging must finish before phase chain;
/// the epoch's files and initial seeds are immutable from that point onward.
pub(crate) fn plan_files(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "linker planning budget must be positive");
    if !super::linker_sort::files_step(client, epoch, budget)? {
        return Ok(false);
    }
    for _ in 0..budget {
        if plan_one(client, epoch)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ChainCursor {
    stage: u8,
    position: i64,
    file: i64,
    articles: i64,
    material: bool,
    first: i64,
    last: i64,
    after_active: i64,
    candidate: i64,
    candidate_last: i64,
    candidate_order: i64,
    candidate_index: bool,
    candidate_seed: bool,
    member: Option<i64>,
    file_subset: bool,
    chain_subset: bool,
    index_subset: bool,
    overlap: bool,
    groups: i64,
    best: Option<((i64, i64, i64, i64), i64)>,
    chain: i64,
    new_chain: bool,
    copy_groups: bool,
}

/// One microstep: one activation, expiry, candidate membership or file effect.
/// A huge frontier/group set advances a persisted cursor instead of retrying
/// one whole-file transaction. Candidate reduction preserves PICK's tuple order.
fn plan_one(client: &mut Client, epoch: i64) -> Result<bool> {
    let mut tx = durable_transaction(client)?;
    let row =
        tx.query_one("select phase,active_serial,next_chain from linker_epochs where id=$1 for update", &[&epoch])?;
    let phase: String = row.get(0);
    if ["plan", "emit", "ack", "done"].contains(&phase.as_str()) {
        return Ok(true);
    }
    ensure!(phase == "chain", "chain reducer requires sealed files");
    let serial: i64 = row.get(1);
    let next_chain: i64 = row.get(2);
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='chain'", &[&epoch])?;
    let mut s: ChainCursor =
        saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let gap = crate::xref::GAP_SECS;
    let mut done = false;
    match s.stage {
        0 => {
            let next = tx.query_opt(
                "select f.id,f.articles,f.first_posted,f.last_posted,f.new_material,r.position
                from linker_sort_rows r join linker_files f on f.epoch=r.epoch and f.id=r.source
                where r.epoch=$1 and r.kind=2 and r.level=(select level from linker_sort_state where epoch=$1 and kind=2) and r.position>$2
                order by r.position limit 1",
                &[&epoch,&s.position],
            )?;
            if let Some(row) = next {
                s = ChainCursor {
                    stage: 1,
                    position: row.get(5),
                    file: row.get(0),
                    articles: row.get(1),
                    first: row.get(2),
                    last: row.get(3),
                    material: row.get(4),
                    ..Default::default()
                };
            } else {
                tx.execute("update linker_epochs set phase='plan' where id=$1", &[&epoch])?;
                done = true;
            }
        }
        1 => {
            let next = tx.query_opt(
                "select id from linker_chains where epoch=$1 and active_order is null and not expired
                and first_posted<=$2 order by first_posted,id desc limit 1",
                &[&epoch, &s.first.saturating_add(gap)],
            )?;
            if let Some(row) = next {
                let serial = serial.checked_add(1).context("chain serial overflow")?;
                tx.execute(
                    "update linker_chains set active_order=$3 where epoch=$1 and id=$2",
                    &[&epoch, &row.get::<_, i64>(0), &serial],
                )?;
                tx.execute("update linker_epochs set active_serial=$2 where id=$1", &[&epoch, &serial])?;
            } else {
                s.stage = 2;
            }
        }
        2 => {
            let next = tx.query_opt(
                "select id from linker_chains where epoch=$1 and active_order is not null and not expired
                and last_posted<$2 order by last_posted,first_posted limit 1",
                &[&epoch, &s.first.saturating_sub(gap)],
            )?;
            if let Some(row) = next {
                tx.execute(
                    "update linker_chains set expired=true where epoch=$1 and id=$2",
                    &[&epoch, &row.get::<_, i64>(0)],
                )?;
            } else {
                s.stage = if s.articles == 1 { 7 } else { 3 };
            }
        }
        3 => {
            let next = tx.query_opt(
                "select id,last_posted,active_order,index_only,seed is not null from linker_chains
                where epoch=$1 and active_order>$2 and not expired order by active_order limit 1",
                &[&epoch, &s.after_active],
            )?;
            if let Some(row) = next {
                s.candidate = row.get(0);
                s.candidate_last = row.get(1);
                s.candidate_order = row.get(2);
                s.candidate_index = row.get(3);
                s.candidate_seed = row.get(4);
                s.after_active = s.candidate_order;
                s.member = None;
                s.file_subset = true;
                s.chain_subset = true;
                s.index_subset = true;
                s.overlap = false;
                s.groups = 0;
                s.stage = 4;
            } else {
                s.stage = 7;
            }
        }
        4 => {
            let next=match s.member {
                Some(group)=>tx.query_opt("select group_id from linker_file_groups where epoch=$1 and file_id=$2 and group_id>$3 order by group_id limit 1", &[&epoch,&s.file,&group])?,
                None=>tx.query_opt("select group_id from linker_file_groups where epoch=$1 and file_id=$2 order by group_id limit 1", &[&epoch,&s.file])?,
            };
            if let Some(row) = next {
                let group: i64 = row.get(0);
                s.member = Some(group);
                let row=tx.query_one("select exists(select 1 from linker_chain_groups where epoch=$1 and chain_id=$2 and kind=0 and group_id=$3),
                    exists(select 1 from linker_chain_groups where epoch=$1 and chain_id=$2 and kind=2 and group_id=$3)", &[&epoch,&s.candidate,&group])?;
                s.file_subset &= row.get::<_, bool>(0);
                s.overlap |= row.get::<_, bool>(1);
            } else {
                s.stage = 5;
                s.member = None;
            }
        }
        5 | 6 => {
            let kind = if s.stage == 5 { 0i16 } else { 1i16 };
            let next=match s.member {
                Some(group)=>tx.query_opt("select group_id from linker_chain_groups where epoch=$1 and chain_id=$2 and kind=$3 and group_id>$4 order by group_id limit 1", &[&epoch,&s.candidate,&kind,&group])?,
                None=>tx.query_opt("select group_id from linker_chain_groups where epoch=$1 and chain_id=$2 and kind=$3 order by group_id limit 1", &[&epoch,&s.candidate,&kind])?,
            };
            if let Some(row) = next {
                let group: i64 = row.get(0);
                s.member = Some(group);
                let exists: bool = tx
                    .query_one(
                        "select exists(select 1 from linker_file_groups where epoch=$1 and file_id=$2 and group_id=$3)",
                        &[&epoch, &s.file, &group],
                    )?
                    .get(0);
                if s.stage == 5 {
                    s.groups += 1;
                    s.chain_subset &= exists;
                } else {
                    s.index_subset &= exists;
                }
            } else if s.stage == 5 && s.candidate_index {
                s.stage = 6;
                s.member = None;
            } else {
                let mut consider = |key: (i64, i64, i64, i64)| {
                    if s.best.as_ref().is_none_or(|(old, _)| key < *old) {
                        s.best = Some((key, s.candidate));
                    }
                };
                if s.candidate_index && s.index_subset || !s.candidate_index && s.file_subset && s.chain_subset {
                    consider((0, -s.candidate_last, i64::from(!s.candidate_index), -s.candidate_order));
                }
                if !s.candidate_index && s.file_subset {
                    consider((1, s.groups, -s.candidate_last, s.candidate_order));
                }
                if s.candidate_seed && !s.overlap {
                    consider((2, (s.first - s.candidate_last).abs(), 0, s.candidate_order));
                }
                s.stage = 3;
            }
        }
        7 => {
            s.new_chain = s.best.is_none();
            s.chain = s.best.as_ref().map_or(next_chain, |(_, id)| *id);
            if s.new_chain {
                tx.execute("insert into linker_chains(epoch,id,first_posted,last_posted,index_only,active_order) values($1,$2,$3,$4,$5,$6)",
                    &[&epoch,&s.chain,&s.first,&s.last,&(s.articles==1),&(serial+1)])?;
                tx.execute(
                    "update linker_epochs set active_serial=$2,next_chain=$3 where id=$1",
                    &[&epoch, &(serial + 1), &(next_chain + 1)],
                )?;
            }
            let row = tx.query_one(
                "select index_only,seed is not null,file_count from linker_chains where epoch=$1 and id=$2",
                &[&epoch, &s.chain],
            )?;
            let index: bool = row.get(0);
            s.copy_groups = index && s.articles > 1 || row.get::<_, bool>(1) || row.get::<_, i64>(2) == 0 && !index;
            s.member = None;
            s.stage = 8;
        }
        8 => {
            let next=match s.member {
                Some(group)=>tx.query_opt("select group_id from linker_file_groups where epoch=$1 and file_id=$2 and group_id>$3 order by group_id limit 1", &[&epoch,&s.file,&group])?,
                None=>tx.query_opt("select group_id from linker_file_groups where epoch=$1 and file_id=$2 order by group_id limit 1", &[&epoch,&s.file])?,
            };
            if let Some(row) = next {
                let group: i64 = row.get(0);
                s.member = Some(group);
                if s.new_chain {
                    tx.execute(
                        "insert into linker_chain_groups values($1,$2,1,$3) on conflict do nothing",
                        &[&epoch, &s.chain, &group],
                    )?;
                }
                if s.copy_groups {
                    tx.execute(
                        "insert into linker_chain_groups values($1,$2,0,$3) on conflict do nothing",
                        &[&epoch, &s.chain, &group],
                    )?;
                }
            } else {
                s.stage = 9;
            }
        }
        9 => {
            let ordinal: i64 = tx
                .query_one(
                    "update linker_chains set first_posted=least(first_posted,$3),last_posted=greatest(last_posted,$4),
                index_only=index_only and $5::bigint=1,file_count=file_count+1,
                articles=articles+$5,new_material=new_material or $6,
                fresh_first=least(fresh_first,$3),fresh_last=greatest(fresh_last,$4)
                where epoch=$1 and id=$2 returning file_count",
                    &[&epoch, &s.chain, &s.first, &s.last, &s.articles, &s.material],
                )?
                .get(0);
            tx.execute("insert into linker_chain_files values($1,$2,$3,$4)", &[&epoch, &s.chain, &ordinal, &s.file])?;
            tx.execute("update linker_files set planned=true where epoch=$1 and id=$2", &[&epoch, &s.file])?;
            s = ChainCursor { position: s.position, ..Default::default() };
        }
        _ => unreachable!(),
    }
    tx.execute(
        "insert into linker_work values($1,'chain',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&s)?],
    )?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xref::{FileSum, Seed};
    use std::collections::BTreeSet;

    fn file(poster: &str, n: i64, first: i64, last: i64, groups: &[i64]) -> FileSum {
        FileSum { poster: poster.into(), n, first, last, added: 0, groups: groups.iter().copied().collect() }
    }

    fn parity(files: &[FileSum], seeds: &[Seed], budget: usize) {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut client = schema.pool.get().unwrap();
        let epoch: i64 = client
            .query_one(
                "insert into linker_epochs(logical_now,phase,next_chain) values(1000,'chain',$1) returning id",
                &[&(seeds.len() as i64)],
            )
            .unwrap()
            .get(0);
        for (i, s) in seeds.iter().enumerate() {
            client
                .execute(
                    "insert into linker_chains(epoch,id,seed,first_posted,last_posted,index_only)
                values($1,$2,$2,$3,$4,false)",
                    &[&epoch, &(i as i64), &s.first, &s.last],
                )
                .unwrap();
            for g in &s.groups {
                for kind in [0i16, 2] {
                    client
                        .execute(
                            "insert into linker_chain_groups values($1,$2,$3,$4)",
                            &[&epoch, &(i as i64), &kind, g],
                        )
                        .unwrap();
                }
            }
        }
        for (i, f) in files.iter().enumerate() {
            client
                .execute(
                    "insert into linker_files(epoch,id,poster,articles,first_posted,last_posted)
                values($1,$2,$3,$4,$5,$6)",
                    &[&epoch, &(i as i64), &f.poster.as_bytes(), &f.n, &f.first, &f.last],
                )
                .unwrap();
            for g in &f.groups {
                client.execute("insert into linker_file_groups values($1,$2,$3)", &[&epoch, &(i as i64), g]).unwrap();
            }
        }
        // Re-enter the public planner after each bounded step. A second call
        // after completion must not mutate membership or file ordering.
        while !plan_files(&mut client, epoch, budget).unwrap() {}
        assert!(plan_files(&mut client, epoch, budget).unwrap());
        let mut actual = Vec::new();
        let mut chain_cursor = -1i64;
        while let Some(c) = client
            .query_opt(
                "select id,seed from linker_chains
            where epoch=$1 and id>$2 and file_count>0 order by id limit 1",
                &[&epoch, &chain_cursor],
            )
            .unwrap()
        {
            chain_cursor = c.get(0);
            let seed: Option<i64> = c.get(1);
            let mut members = Vec::new();
            let mut cursor = 0i64;
            while let Some(r) = client
                .query_opt(
                    "select ordinal,file_id from linker_chain_files
                where epoch=$1 and chain_id=$2 and ordinal>$3 order by ordinal limit 1",
                    &[&epoch, &chain_cursor, &cursor],
                )
                .unwrap()
            {
                cursor = r.get(0);
                members.push(r.get::<_, i64>(1) as usize);
            }
            actual.push((seed.map(|v| v as usize), members));
        }
        assert_eq!(actual, crate::xref::chain(files, seeds));
    }

    #[test]
    fn raw_callbacks_work_on_current_thread_runtime_with_borrowed_input() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let database: String = schema.pool.get().unwrap().query_one("select current_database()", &[]).unwrap().get(0);
        let db = crate::indexer::shared_postgres_db(
            &crate::pg::StorageConfig {
                backend: crate::pg::Backend::Postgres,
                url: schema.url.clone(),
                schema: schema.name.clone(),
                allow_plaintext: true,
                ..Default::default()
            },
            &database,
            crate::indexer::PgWriteLimits::default(),
        )
        .unwrap();
        assert!(db.is_native());
        let mut owner_client = postgres::Client::connect(&schema.url, postgres::NoTls).unwrap();
        owner_client.batch_execute(&format!("set search_path to \"{}\"", schema.name)).unwrap();
        let locked: bool = owner_client
            .query_one("select pg_try_advisory_lock(hashtext('atlas.indexer'),hashtext($1))", &[&schema.name])
            .unwrap()
            .get(0);
        assert!(locked);
        let owner_pid: i32 = owner_client.query_one("select pg_backend_pid()", &[]).unwrap().get(0);
        let owner = std::sync::Arc::new(crate::pg::indexer_runtime::Owner::new(owner_client));
        let db = crate::indexer::owned_postgres_db(db, owner, crate::indexer::PgWriteLimits::default()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let borrowed = String::from("generic callback fixture");
            let answer: String =
                crate::indexer::on_native_db_sync(&db, |c| Ok(c.query_one("select $1::text", &[&borrowed])?.get(0)))
                    .unwrap();
            assert_eq!(answer, borrowed);
            let answer: i64 =
                crate::indexer::on_native_db(&db, |c| Ok(c.query_one("select 42::bigint", &[])?.get(0))).await.unwrap();
            assert_eq!(answer, 42);
            let sync_pid: i32 =
                crate::indexer::on_native_db_sync(&db, |c| Ok(c.query_one("select pg_backend_pid()", &[])?.get(0)))
                    .unwrap();
            let async_pid: i32 =
                crate::indexer::on_native_db(&db, |c| Ok(c.query_one("select pg_backend_pid()", &[])?.get(0)))
                    .await
                    .unwrap();
            assert_eq!((sync_pid, async_pid), (owner_pid, owner_pid));
        });
    }

    #[test]
    fn durable_transactions_override_async_commit_and_flush_staged_wal() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        c.batch_execute("set synchronous_commit=off").unwrap();
        let mut tx = durable_transaction(&mut c).unwrap();
        assert_eq!(tx.query_one("show synchronous_commit", &[]).unwrap().get::<_, String>(0), "on");
        tx.execute("insert into meta(key,value) values('linker-durability-fixture',1)", &[]).unwrap();
        let lsn: String = tx.query_one("select pg_current_wal_insert_lsn()::text", &[]).unwrap().get(0);
        tx.commit().unwrap();
        assert!(c.query_one("select pg_current_wal_flush_lsn()>=$1::text::pg_lsn", &[&lsn]).unwrap().get::<_, bool>(0));
        assert_eq!(c.query_one("show synchronous_commit", &[]).unwrap().get::<_, String>(0), "off");
        c.batch_execute("create function require_durable_linker() returns trigger language plpgsql as $$ begin
            if current_setting('synchronous_commit') <> 'on' then raise exception 'asynchronous linker receipt'; end if; return new; end $$;
            create trigger require_durable_linker before insert or update on linker_epochs for each row execute function require_durable_linker()").unwrap();
        let epoch = super::super::linker_epoch::open(&mut c, 100000).unwrap();
        durable_execute(&mut c, "update linker_epochs set inventory_sealed=true where id=$1", &[&epoch]).unwrap();
        assert!(!super::super::linker_epoch::scope(&mut c, epoch).unwrap());
    }

    #[test]
    fn historical_chains_leave_the_indexed_matching_frontier() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch:i64=c.query_one("insert into linker_epochs(logical_now,phase,active_serial,next_chain) values(100000,'chain',10000,10000) returning id",&[]).unwrap().get(0);
        c.execute("insert into linker_chains(epoch,id,first_posted,last_posted,index_only,active_order) select $1,n,0,0,false,n+1 from generate_series(0,9999) n",&[&epoch]).unwrap();
        c.execute("insert into linker_files(epoch,id,poster,articles,first_posted,last_posted) values($1,0,$2,10,100000,100000),($1,1,$3,10,200000,200000)",&[&epoch,&b"one@example.invalid".as_slice(),&b"two@example.invalid".as_slice()]).unwrap();
        c.execute("insert into linker_file_groups values($1,0,1),($1,1,1)", &[&epoch]).unwrap();
        c.batch_execute("create table chain_audit(tx bigint primary key, mutations bigint not null);
            create function audit_chain() returns trigger language plpgsql as $$ begin
              insert into chain_audit values(txid_current(),1) on conflict(tx) do update set mutations=chain_audit.mutations+1;
              return null; end $$").unwrap();
        for table in [
            "linker_chains",
            "linker_chain_groups",
            "linker_chain_files",
            "linker_files",
            "linker_epochs",
            "linker_work",
        ] {
            c.batch_execute(&format!("create trigger audit after insert or update or delete on {table} for each row execute function audit_chain()" )).unwrap();
        }
        for turn in 0..11000 {
            assert!(turn < 10999, "chain reducer did not advance");
            assert!(!plan_files(&mut c, epoch, 1).unwrap());
            if c.query_one("select planned from linker_files where epoch=$1 and id=0", &[&epoch])
                .unwrap()
                .get::<_, bool>(0)
            {
                break;
            }
        }
        assert!(c.query_one("select max(mutations) from chain_audit", &[]).unwrap().get::<_, i64>(0) <= 4);
        assert_eq!(
            c.query_one(
                "select count(*) from linker_chains where epoch=$1 and not expired and active_order is not null",
                &[&epoch]
            )
            .unwrap()
            .get::<_, i64>(0),
            1
        );
        c.batch_execute("analyze linker_chains; analyze linker_chain_groups; analyze linker_file_groups").unwrap();
        let explain = c
            .query(
                "explain (analyze,buffers,format text) select id from linker_chains where epoch=$1 and active_order>0 and not expired order by active_order limit 1",
                &[&epoch],
            )
            .unwrap();
        let plan = explain.iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>().join("\n");
        assert!(plan.contains("linker_active_encounter"), "matching did not use bounded frontier: {plan}");
        assert!(!plan.contains("Seq Scan on linker_chains"), "matching scanned expired history: {plan}");
        while !plan_files(&mut c, epoch, 1).unwrap() {}
        assert_eq!(
            c.query_one("select active_serial from linker_epochs where id=$1", &[&epoch]).unwrap().get::<_, i64>(0),
            10002
        );
        assert_eq!(
            c.query_one(
                "select count(*) from linker_chains where epoch=$1 and not expired and active_order is not null",
                &[&epoch]
            )
            .unwrap()
            .get::<_, i64>(0),
            1
        );
    }

    #[test]
    fn external_chain_preserves_singletons_sets_and_seed_ties() {
        let seeds = [
            Seed { first: 100, last: 105, groups: BTreeSet::from([1, 2]) },
            Seed { first: 100, last: 105, groups: BTreeSet::from([1, 2]) },
            Seed { first: 90, last: 105, groups: BTreeSet::from([7, 8]) },
        ];
        let files = [
            file("a", 1, 101, 101, &[1]),
            file("b", 20, 101, 103, &[1, 2]),
            file("c", 30, 104, 105, &[1]),
            file("d", 15, 105, 108, &[3, 4]),
            file("e", 1, 130, 130, &[5]),
            file("f", 20, 130, 132, &[5, 6]),
            file("g", 1, 130, 130, &[5]),
            file("h", 20, 131, 133, &[5, 6]),
        ];
        parity(&files, &seeds, 1);
    }

    #[test]
    fn external_chain_matches_many_files_and_large_summary() {
        let files: Vec<_> = (0..60)
            .map(|i| {
                file(&format!("file{i:03}"), if i % 7 == 0 { 1 } else { 1_000_000 }, 100 + i, 100 + i + 2, &[i % 3, 9])
            })
            .collect();
        parity(&files, &[], 3);
    }
}
