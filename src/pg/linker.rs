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

/// Plan at most `budget` files. `true` means every staged file is assigned.
/// Call only on the ownership session. Staging must finish before phase chain;
/// the epoch's files and initial seeds are immutable from that point onward.
pub(crate) fn plan_files(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "linker planning budget must be positive");
    for _ in 0..budget {
        if plan_one(client, epoch)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plan_one(client: &mut Client, epoch: i64) -> Result<bool> {
    let mut tx = durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state = tx
        .query_one("select phase,active_serial,next_chain from linker_epochs where id=$1 for update", &[&epoch])
        .context("opening native linker epoch")?;
    let phase: String = state.get(0);
    if phase == "plan" || phase == "emit" || phase == "ack" || phase == "done" {
        return Ok(true);
    }
    ensure!(phase == "chain", "linker file planning requires a sealed file snapshot");
    let serial: i64 = state.get(1);
    let next_chain: i64 = state.get(2);
    let file = tx.query_opt(
        "select id,articles,first_posted,last_posted from linker_files
         where epoch=$1 and not planned
         order by first_posted,articles<>1,last_posted,poster,id limit 1",
        &[&epoch],
    )?;
    let Some(file) = file else {
        tx.execute("update linker_epochs set phase='plan' where id=$1", &[&epoch])?;
        tx.commit()?;
        return Ok(true);
    };
    let id: i64 = file.get(0);
    let articles: i64 = file.get(1);
    let first: i64 = file.get(2);
    let last: i64 = file.get(3);
    let gap = crate::xref::GAP_SECS;
    // xref::chain's stable descending seed-start sort is popped from the end.
    // Seeds with the same start therefore activate in reverse seed order.
    // Keep activation order separate from chain order: same chooses the LAST
    // maximum, while within/read-since choose the FIRST minimum.
    let entered = tx.execute(
        "with entering as (
            select id,$3+row_number() over(order by first_posted,id desc) as ord
            from linker_chains where epoch=$1 and active_order is null and not expired and first_posted<=$2
         ) update linker_chains c set active_order=e.ord from entering e where c.epoch=$1 and c.id=e.id",
        &[&epoch, &first.saturating_add(gap), &serial],
    )?;
    let serial = serial.checked_add(i64::try_from(entered)?).context("native chain activation counter overflow")?;
    tx.execute("update linker_chains set expired=true where epoch=$1 and active_order is not null and not expired and last_posted<$2",
        &[&epoch,&first.saturating_sub(gap)])?;
    let pick = if articles == 1 {
        None
    } else {
        tx.query_opt(PICK, &[&epoch, &id, &first, &gap])?.map(|r| r.get::<_, i64>(0))
    };
    let chain = pick.unwrap_or(next_chain);
    if pick.is_none() {
        tx.execute(
            "insert into linker_chains(epoch,id,first_posted,last_posted,index_only,active_order)
             values($1,$2,$3,$4,$5,$6)",
            &[&epoch, &chain, &first, &last, &(articles == 1), &(serial + 1)],
        )?;
        tx.execute(
            "insert into linker_chain_groups(epoch,chain_id,kind,group_id)
             select epoch,$3,1,group_id from linker_file_groups where epoch=$1 and file_id=$2",
            &[&epoch, &id, &chain],
        )?;
    }
    // An index acquires its first full file's group set; a seed accumulates new
    // groups. Ordinary established uploads retain their original group set.
    tx.execute(
        "insert into linker_chain_groups(epoch,chain_id,kind,group_id)
         select f.epoch,c.id,0,f.group_id from linker_file_groups f
         join linker_chains c on c.epoch=f.epoch and c.id=$3
         where f.epoch=$1 and f.file_id=$2 and
           (c.index_only and $4::bigint>1 or c.seed is not null or c.file_count=0 and not c.index_only)
         on conflict do nothing",
        &[&epoch, &id, &chain, &articles],
    )?;
    let ordinal: i64 = tx
        .query_one(
            "update linker_chains set first_posted=least(first_posted,$3),last_posted=greatest(last_posted,$4),
         index_only=index_only and $5::bigint=1,file_count=file_count+1
         where epoch=$1 and id=$2 returning file_count",
            &[&epoch, &chain, &first, &last, &articles],
        )?
        .get(0);
    tx.execute("insert into linker_chain_files values($1,$2,$3,$4)", &[&epoch, &chain, &ordinal, &id])?;
    tx.execute("update linker_files set planned=true where epoch=$1 and id=$2", &[&epoch, &id])?;
    tx.execute(
        "update linker_epochs set active_serial=$2,next_chain=$3 where id=$1",
        &[&epoch, &(serial + i64::from(pick.is_none())), &(next_chain + i64::from(pick.is_none()))],
    )?;
    tx.commit()?;
    Ok(false)
}

// Set comparisons use indexed NOT EXISTS, never array/string aggregation.
// PostgreSQL supplies external ordering/spill under the session work_mem cap.
const PICK: &str = "
with candidates as (
 select c.*,
   (select count(*) from linker_chain_groups g where g.epoch=c.epoch and g.chain_id=c.id and g.kind=0) as groups
 from linker_chains c where c.epoch=$1 and c.active_order is not null and not c.expired
 and c.first_posted<=$3::bigint+$4::bigint and c.last_posted>=$3::bigint-$4::bigint
), choices as (
 select id,0 as priority,-last_posted as a,case when index_only then 0 else 1 end::bigint as b,-active_order as d
 from candidates c where
 (c.index_only and not exists (
   select 1 from linker_chain_groups g where g.epoch=$1 and g.chain_id=c.id and g.kind=1
   and not exists(select 1 from linker_file_groups f where f.epoch=$1 and f.file_id=$2 and f.group_id=g.group_id)))
 or (not c.index_only and
   not exists(select 1 from linker_chain_groups g where g.epoch=$1 and g.chain_id=c.id and g.kind=0
     and not exists(select 1 from linker_file_groups f where f.epoch=$1 and f.file_id=$2 and f.group_id=g.group_id))
   and not exists(select 1 from linker_file_groups f where f.epoch=$1 and f.file_id=$2
     and not exists(select 1 from linker_chain_groups g where g.epoch=$1 and g.chain_id=c.id and g.kind=0 and g.group_id=f.group_id)))
 union all
 select id,1,groups,-last_posted,active_order from candidates c where not index_only
 and not exists(select 1 from linker_file_groups f where f.epoch=$1 and f.file_id=$2
   and not exists(select 1 from linker_chain_groups g where g.epoch=$1 and g.chain_id=c.id and g.kind=0 and g.group_id=f.group_id))
 union all
 select id,2,abs($3-last_posted),0,active_order from candidates c where seed is not null
 and not exists(select 1 from linker_file_groups f join linker_chain_groups g
   on g.epoch=f.epoch and g.group_id=f.group_id and g.chain_id=c.id and g.kind=2
   where f.epoch=$1 and f.file_id=$2)
) select id from choices order by priority,a,b,d limit 1";

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
        assert!(!plan_files(&mut c, epoch, 1).unwrap());
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
                &format!("explain (analyze,buffers,format text) {PICK}"),
                &[&epoch, &1i64, &200000i64, &crate::xref::GAP_SECS],
            )
            .unwrap();
        let plan = explain.iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>().join("\n");
        assert!(plan.contains("linker_active_chains"), "matching did not use bounded frontier: {plan}");
        assert!(!plan.contains("Seq Scan on linker_chains"), "matching scanned expired history: {plan}");
        assert!(!plan_files(&mut c, epoch, 1).unwrap());
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
