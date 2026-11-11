//! Restartable external merge ordering of article references. No arbitrary
//! message ID is a B-tree key: even long valid IDs compare as bounded row
//! payloads, while all scan/continuation keys are fixed-width integers.
use anyhow::{Result, ensure};
use postgres::{Client, Row, Transaction};
use serde::{Deserialize, Serialize};
#[derive(Default, Serialize, Deserialize)]
struct Sort {
    count: i64,
    level: i64,
    width: i64,
    base: i64,
    left: i64,
    right: i64,
    out: i64,
}

/// Copy immutable file summaries into one-row merge runs. File posters remain
/// TOAST-able payloads; the merge compares the exact legacy lexical tuple.
pub(crate) fn files_step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    let mut tx = super::linker::durable_transaction(client)?;
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='file-sort'", &[&epoch])?;
    let (mut after, mut position, mut copied): (i64, i64, bool) =
        saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or((-1, 0, false));
    if !copied {
        for _ in 0..budget {
            let next=tx.query_opt("select id,first_posted,articles,last_posted,poster from linker_files where epoch=$1 and id>$2 order by id limit 1", &[&epoch,&after])?;
            let Some(row) = next else {
                copied = true;
                break;
            };
            after = row.get(0);
            position += 1;
            tx.execute(
                "insert into linker_sort_rows values($1,2,0,$2,$3,$4,$5,$6,$7,0,0)",
                &[
                    &epoch,
                    &position,
                    &row.get::<_, i64>(1),
                    &i64::from(row.get::<_, i64>(2) != 1),
                    &row.get::<_, i64>(3),
                    &row.get::<_, Vec<u8>>(4),
                    &after,
                ],
            )?;
        }
        tx.execute("insert into linker_work values($1,'file-sort',$2) on conflict(epoch,kind) do update set state=excluded.state",
            &[&epoch,&serde_json::to_string(&(after,position,copied))?])?;
        tx.commit()?;
        return Ok(false);
    }
    tx.commit()?;
    step(client, epoch, 2, budget)
}

pub(crate) fn step(client: &mut Client, epoch: i64, kind: i16, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "sort budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let saved = tx.query_opt(
        "select state,complete from linker_sort_state where epoch=$1 and kind=$2 for update",
        &[&epoch, &kind],
    )?;
    let mut s: Sort = if let Some(row) = saved {
        if row.get::<_, bool>(1) {
            return Ok(true);
        }
        serde_json::from_str(&row.get::<_, String>(0))?
    } else {
        let count: i64 = tx
            .query_one(
                "select coalesce(max(position),0) from linker_sort_rows where epoch=$1 and kind=$2 and level=0",
                &[&epoch, &kind],
            )?
            .get(0);
        Sort { count, width: 1, base: 1, left: 1, right: 2, out: 1, ..Default::default() }
    };
    for _ in 0..budget {
        if s.width >= s.count {
            break;
        }
        let middle = (s.base + s.width).min(s.count + 1);
        let end = (s.base + s.width.saturating_mul(2)).min(s.count + 1);
        if s.out >= end {
            s.base = end;
            if s.base > s.count {
                s.level += 1;
                s.width = s.width.saturating_mul(2);
                s.base = 1;
            }
            s.left = s.base;
            s.right = (s.base + s.width).min(s.count + 1);
            s.out = s.base;
            continue;
        }
        let left = if s.left < middle { Some(load(&mut tx, epoch, kind, s.level, s.left)?) } else { None };
        let right = if s.right < end { Some(load(&mut tx, epoch, kind, s.level, s.right)?) } else { None };
        let choose_left = match (&left, &right) {
            (Some(l), Some(r)) => key(l) <= key(r),
            (Some(_), None) => true,
            _ => false,
        };
        let (row, position) = if choose_left {
            let n = s.left;
            s.left += 1;
            (left.unwrap(), n)
        } else {
            let n = s.right;
            s.right += 1;
            (right.expect("merge has an input head"), n)
        };
        tx.execute(
            "insert into linker_sort_rows values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[
                &epoch,
                &kind,
                &(s.level + 1),
                &s.out,
                &row.get::<_, i64>(0),
                &row.get::<_, i64>(1),
                &row.get::<_, i64>(2),
                &row.get::<_, Vec<u8>>(3),
                &row.get::<_, i64>(4),
                &row.get::<_, i64>(5),
                &row.get::<_, i64>(6),
            ],
        )?;
        tx.execute(
            "delete from linker_sort_rows where epoch=$1 and kind=$2 and level=$3 and position=$4",
            &[&epoch, &kind, &s.level, &position],
        )?;
        s.out += 1;
    }
    let complete = s.width >= s.count;
    tx.execute("insert into linker_sort_state values($1,$2,$3,$4,$5) on conflict(epoch,kind) do update set state=excluded.state,complete=excluded.complete,level=excluded.level",
        &[&epoch,&kind,&serde_json::to_string(&s)?,&complete,&s.level])?;
    tx.commit()?;
    Ok(complete)
}
fn load(tx: &mut Transaction<'_>, epoch: i64, kind: i16, level: i64, position: i64) -> Result<Row> {
    Ok(tx.query_one(
        "select file_id,group_id,article_number,message_id,source,source_ordinal,posted
        from linker_sort_rows where epoch=$1 and kind=$2 and level=$3 and position=$4",
        &[&epoch, &kind, &level, &position],
    )?)
}
fn key(row: &Row) -> (i64, i64, i64, &[u8]) {
    (row.get(0), row.get(1), row.get(2), row.get(3))
}

pub(crate) fn next(
    tx: &mut Transaction<'_>,
    epoch: i64,
    kind: i16,
    file: i64,
    after: Option<i64>,
) -> Result<Option<Row>> {
    let state =
        tx.query_one("select level,complete from linker_sort_state where epoch=$1 and kind=$2", &[&epoch, &kind])?;
    ensure!(state.get::<_, bool>(1), "part ordering must be sealed before plan emission");
    let level: i64 = state.get(0);
    Ok(tx.query_opt(
        "select group_id,article_number,message_id,source,source_ordinal,posted,position from linker_sort_rows
        where epoch=$1 and kind=$2 and level=$3 and file_id=$4 and position>$5 order by position limit 1",
        &[&epoch, &kind, &level, &file, &after.unwrap_or(0)],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn keyset_queries_touch_only_the_next_rows_after_large_completed_prefixes() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values(1,'chain') returning id", &[])
            .unwrap()
            .get(0);
        c.execute("insert into linker_files(epoch,id,poster,articles,first_posted,last_posted) select $1,n,convert_to('poster'||n,'UTF8'),12,n,n from generate_series(1,10000) n", &[&epoch]).unwrap();
        c.execute("insert into linker_sort_rows select $1,2,10,n,n,1,n,convert_to('poster'||n,'UTF8'),n,0,0 from generate_series(1,10000) n", &[&epoch]).unwrap();
        c.execute("insert into linker_sort_state values($1,2,'{}',true,10)", &[&epoch]).unwrap();
        c.execute("insert into linker_file_input select $1,n,n,n,1,n,convert_to('poster'||n,'UTF8'),false,true,1,n,convert_to('message'||n,'UTF8') from generate_series(1,10000) n", &[&epoch]).unwrap();
        c.execute("insert into linker_planned_places select $1,convert_to('poster'||n,'UTF8'),n,'release','group.example',n,10000,12,1,1 from generate_series(1,10000) n", &[&epoch]).unwrap();
        c.execute("insert into linker_planned_firsts select $1,convert_to('poster1','UTF8'),n,n from generate_series(1,10000) n", &[&epoch]).unwrap();
        c.execute("insert into linker_scope_days select $1,n,true from generate_series(1,10000) n", &[&epoch]).unwrap();
        c.batch_execute("insert into linker_import_ranges select 'links.rec',n,n from generate_series(1,10000) n; analyze linker_sort_rows; analyze linker_sort_state; analyze linker_files; analyze linker_file_input; analyze linker_planned_places; analyze linker_planned_firsts; analyze linker_scope_days; analyze linker_import_ranges").unwrap();
        // These are the production continuation shapes, with a completed
        // prefix of 9,999 rows. Inspect actual executor rows, not LIMIT output.
        for query in [
            format!("select f.id,f.articles,f.first_posted,f.last_posted,f.new_material,r.position from linker_sort_rows r join linker_files f on f.epoch=r.epoch and f.id=r.source where r.epoch={epoch} and r.kind=2 and r.level=(select level from linker_sort_state where epoch={epoch} and kind=2) and r.position>9999 order by r.position limit 1"),
            format!("select poster_rank,posted,source_rank from linker_file_input where epoch={epoch} and (poster_rank,posted,source_rank)>(9999,9999,9999) order by poster_rank,posted,source_rank limit 1"),
            format!("select group_id,article_number from linker_planned_firsts where epoch={epoch} and poster_key=sha256(convert_to('poster1','UTF8')) and poster=convert_to('poster1','UTF8') and group_id>9999 order by group_id limit 1"),
            format!("select day from linker_scope_days where epoch={epoch} and day>9999 order by day limit 1"),
            "select first_ordinal,last_ordinal from linker_import_ranges where name='links.rec' and last_ordinal>9999 order by last_ordinal limit 1".into(),
        ] {
            let rows=c.query(&format!("explain (analyze,format text) {query}"), &[]).unwrap();
            let plan=rows.iter().map(|r|r.get::<_,String>(0)).collect::<Vec<_>>().join("\n");
            assert!(!plan.contains("Sort "),"{plan}");
            // The scalar one-row sort-state table may legitimately use a
            // sequential scan; input-bearing tables must seek their indexes.
            for line in plan.lines().filter(|line|line.contains("actual time=")) {
                let actual=line.split("actual time=").nth(1).unwrap();
                let n:f64=actual.split("rows=").nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap();
                let loops:f64=actual.split("loops=").nth(1).unwrap().split(')').next().unwrap().parse().unwrap();
                assert!(n*loops<=4.0,"unbounded actual input visits: {plan}");
            }
            for line in plan.lines().filter(|line|line.contains("Rows Removed")) {
                let removed:f64=line.rsplit(':').next().unwrap().trim().parse().unwrap();
                assert!(removed<=1.0,"completed prefix scanned: {plan}");
            }
        }
    }

    #[test]
    fn merge_pages_resume_and_order_long_ids_without_btree_payload_keys() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values(100000,'plan') returning id", &[])
            .unwrap()
            .get(0);
        let mut expected = Vec::new();
        for position in 1..=19i64 {
            let message = (0..80)
                .map(|n| format!("{:x}", Sha256::digest(format!("generic-{position}-{n}").as_bytes())))
                .collect::<String>()
                .into_bytes();
            let file = position % 2;
            c.execute(
                "insert into linker_sort_rows values($1,0,0,$2,$3,1,1,$4,1,$2,100)",
                &[&epoch, &position, &file, &message],
            )
            .unwrap();
            expected.push((file, message));
        }
        expected.sort();
        c.batch_execute("create table sort_audit(tx bigint primary key, mutations bigint not null);
            create function audit_sort() returns trigger language plpgsql as $$ begin
              insert into sort_audit values(txid_current(),1) on conflict(tx) do update set mutations=sort_audit.mutations+1;
              return null; end $$;
            create trigger audit after insert or update or delete on linker_sort_rows for each row execute function audit_sort();
            create trigger audit after insert or update or delete on linker_sort_state for each row execute function audit_sort()").unwrap();
        for _ in 0..7 {
            assert!(!step(&mut c, epoch, 0, 1).unwrap());
        }
        let checkpoint: String =
            c.query_one("select state from linker_sort_state where epoch=$1 and kind=0", &[&epoch]).unwrap().get(0);
        drop(c);
        let mut c = schema.pool.get().unwrap();
        assert!(!step(&mut c, epoch, 0, 1).unwrap());
        assert_ne!(
            checkpoint,
            c.query_one("select state from linker_sort_state where epoch=$1 and kind=0", &[&epoch])
                .unwrap()
                .get::<_, String>(0)
        );
        while !step(&mut c, epoch, 0, 1).unwrap() {}
        let actual: Vec<(i64, Vec<u8>)> = c
            .query(
                "select file_id,message_id from linker_sort_rows where epoch=$1 and kind=0 order by position",
                &[&epoch],
            )
            .unwrap()
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        assert_eq!(actual, expected);
        assert!(c.query_one("select max(mutations) from sort_audit", &[]).unwrap().get::<_, i64>(0) <= 3);
        assert_eq!(
            c.query_one("select count(*) from linker_sort_rows where epoch=$1", &[&epoch]).unwrap().get::<_, i64>(0),
            19
        );
    }
}
