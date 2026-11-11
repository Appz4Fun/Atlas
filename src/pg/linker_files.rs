//! External poster/run inference. The epoch selection is immutable and stored
//! before entry. No Vec of a poster's rows, time summaries, or memberships is
//! constructed in Rust, including for one enormous file.

use anyhow::{Result, ensure};
use postgres::Client;

/// Infer files from already selected fresh material, retaining the legacy
/// distinction: an unplaced poster is one file regardless of internal gaps;
/// only a previously placed poster's later material splits at JOIN_GAP.
/// Source rank is its original old-block-before-late encounter order.
pub(crate) fn infer(client: &mut Client, epoch: i64) -> Result<()> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let phase: String = tx.query_one("select phase from linker_epochs where id=$1 for update", &[&epoch])?.get(0);
    if phase != "files" {
        ensure!(
            ["chain", "plan", "emit", "ack", "done"].contains(&phase.as_str()),
            "native linker inference needs sealed selection"
        );
        return Ok(());
    }
    // Window frames and grouping may spill to PostgreSQL's temporary storage.
    // A disk failure rolls back the phase, preserving both source and selection.
    // Dense IDs follow poster encounter order and then chronological run order,
    // which is the tie-break used by stable xref::chain sorting.
    tx.execute(INFER_FILES, &[&epoch, &crate::nxv_store::linker::JOIN_GAP])?;
    tx.execute(INFER_MEMBERS, &[&epoch, &crate::nxv_store::linker::JOIN_GAP])?;
    tx.execute(
        "insert into linker_file_groups(epoch,file_id,group_id)
        select distinct r.epoch,r.file_id,s.group_id from linker_file_rows r
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1",
        &[&epoch],
    )?;
    tx.execute("update linker_epochs set phase='chain' where id=$1", &[&epoch])?;
    tx.commit()?;
    Ok(())
}

// The same immutable ordering drives summaries and row membership. CTEs are
// bounded server operators; no per-poster array_agg/string_agg is permitted.
const INFER_FILES:&str="
with previous as (
 select r.*,s.poster,s.posted,lag(s.posted) over(partition by s.poster order by s.posted,r.source_rank) as previous,
   coalesce(nullif(r.poster_rank,0),min(r.source_rank) over(partition by s.poster)) as encounter_rank
 from linker_selected_rows r join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1
), runs as (
 select *,sum(case when again and posted-previous>$2 then 1 else 0 end)
    over(partition by poster order by posted,source_rank rows unbounded preceding) as run
 from previous
), summaries as (
 select poster,run,encounter_rank,count(*) as articles,min(posted) as first_posted,max(posted) as last_posted,
    bool_or(is_new) as new_material,bool_or(again) as again
 from runs group by poster,run,encounter_rank
) insert into linker_files(epoch,id,poster,run_no,first_rank,new_material,again,articles,first_posted,last_posted)
 select $1,row_number() over(order by encounter_rank,run)-1,poster,run,encounter_rank,new_material,again,articles,first_posted,last_posted
 from summaries";

const INFER_MEMBERS:&str="
with previous as (
 select r.*,s.poster,s.posted,lag(s.posted) over(partition by s.poster order by s.posted,r.source_rank) as previous
 from linker_selected_rows r join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1
), runs as (
 select *,sum(case when again and posted-previous>$2 then 1 else 0 end)
    over(partition by poster order by posted,source_rank rows unbounded preceding) as run
 from previous
) insert into linker_file_rows(epoch,file_id,source,source_ordinal,source_rank)
 select $1,f.id,r.source,r.source_ordinal,r.source_rank from runs r
 join linker_files f on f.epoch=$1 and f.poster=r.poster and f.run_no=r.run";

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn external_inference_keeps_unplaced_poster_whole_and_splits_repeated_poster() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values(100000,'files') returning id", &[])
            .unwrap()
            .get(0);
        let source: i64 = c
            .query_one(
                "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete)
            values('group.example',1,$1,0,$1,20,20,true) returning id",
                &[&&[1u8][..]],
            )
            .unwrap()
            .get(0);
        for n in 1..=20i64 {
            let poster = if n % 2 == 1 {
                b"unplaced@example.invalid".as_slice()
            } else {
                b"repeated@example.invalid".as_slice()
            };
            let posted = n + if n > 10 { crate::nxv_store::linker::JOIN_GAP + 100 } else { 0 };
            let message = format!("<part{n}@example.invalid>");
            let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            c.execute(
                "insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines) values($1,$2,$3,0,$4,$5,$6,$7,$2,100,1)",
                &[&source, &n, &&key[..], &message.as_bytes(), &poster, &posted, &(n % 2)],
            )
            .unwrap();
            c.execute(
                "insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new) values($1,$2,$3,$3,$4,true)",
                &[&epoch, &source, &n, &(n % 2 == 0)],
            )
            .unwrap();
        }
        infer(&mut c, epoch).unwrap();
        infer(&mut c, epoch).unwrap();
        let count: i64 = c.query_one("select count(*) from linker_files where epoch=$1", &[&epoch]).unwrap().get(0);
        assert_eq!(count, 3);
        let rows = c
            .query("select id,articles,run_no from linker_files where epoch=$1 order by id limit 3", &[&epoch])
            .unwrap();
        let got: Vec<(i64, i64, i64)> = rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect();
        assert_eq!(got, vec![(0, 10, 0), (1, 5, 0), (2, 5, 1)]);
        let members: i64 =
            c.query_one("select count(*) from linker_file_rows where epoch=$1", &[&epoch]).unwrap().get(0);
        assert_eq!(members, 20);
        assert!(!super::super::linker::plan_files(&mut c, epoch, 1).unwrap());
        while !super::super::linker::plan_files(&mut c, epoch, 1).unwrap() {}
    }
}
