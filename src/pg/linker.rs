//! Disk-backed native linker planning. File summaries, active chains and group
//! memberships live in PostgreSQL; the planner retains only one file/chain ID.
//! Each step commits its membership and cursor together. Retrying a step after
//! an ambiguous commit selects the next file, never appends the old one twice.

use anyhow::{Context, Result, ensure};
use postgres::Client;

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
    let mut tx = client.transaction()?;
    let state = tx
        .query_one("select phase,active_serial,next_chain from linker_epochs where id=$1 for update", &[&epoch])
        .context("opening native linker epoch")?;
    let phase: String = state.get(0);
    if phase == "emit" || phase == "ack" || phase == "done" {
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
        tx.execute("update linker_epochs set phase='emit' where id=$1", &[&epoch])?;
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
    tx.execute(
        "with entering as (
            select id,$3+row_number() over(order by first_posted,id desc) as ord
            from linker_chains where epoch=$1 and active_order is null and first_posted<=$2
         ) update linker_chains c set active_order=e.ord from entering e where c.epoch=$1 and c.id=e.id",
        &[&epoch, &first.saturating_add(gap), &serial],
    )?;
    let serial: i64 = tx
        .query_one("select coalesce(max(active_order),$2) from linker_chains where epoch=$1", &[&epoch, &serial])?
        .get(0);
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
 from linker_chains c where c.epoch=$1 and c.active_order is not null
 and $3>=c.first_posted-$4 and $3<=c.last_posted+$4
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
                    &[&epoch, &(i as i64), &f.poster, &f.n, &f.first, &f.last],
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
        });
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
