//! Immutable row selection and old-before-late deduplication. Poster placement
//! is classified before file inference, without holding any poster's articles.

use anyhow::{Result, ensure};
use postgres::Client;

pub(crate) fn select(client: &mut Client, epoch: i64) -> Result<()> {
    let mut tx = client.transaction()?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state = tx.query_one("select phase,logical_now,lo,hi from linker_epochs where id=$1 for update", &[&epoch])?;
    let phase: String = state.get(0);
    if phase != "select" {
        ensure!(
            ["files", "chain", "plan", "emit", "ack", "done"].contains(&phase.as_str()),
            "native linker selection needs sealed scope"
        );
        return Ok(());
    }
    let now: i64 = state.get(1);
    let lo: Option<i64> = state.get(2);
    let hi: Option<i64> = state.get(3);
    let before = now.saturating_sub(8 * crate::xref::KEEP_SECS);
    tx.execute(CANDIDATES, &[&epoch, &lo, &hi, &before])?;
    let keep = now.saturating_sub(crate::xref::KEEP_SECS);
    let gap = crate::nxv_store::linker::JOIN_GAP;
    tx.execute(
        "insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new,poster_rank)
        select c.epoch,c.source,c.source_ordinal,c.source_rank,p.poster is not null,c.is_new,c.poster_rank
        from linker_candidates c join linker_staged_rows s on s.source=c.source and s.ordinal=c.source_ordinal
        left join linker_places p on p.poster=s.poster left join linker_uploads u on u.release_name=p.release_name
        where c.epoch=$1 and (p.poster is null or (c.is_new and
          (u.release_name is null or s.posted<u.first_posted-$2 or s.posted>u.last_posted+$2)))",
        &[&epoch, &gap],
    )?;
    tx.execute(
        "insert into linker_late_rows(epoch,source,source_ordinal,source_rank,poster_rank)
        select c.epoch,c.source,c.source_ordinal,c.source_rank,c.poster_rank
        from linker_candidates c join linker_staged_rows s on s.source=c.source and s.ordinal=c.source_ordinal
        join linker_places p on p.poster=s.poster join linker_uploads u on u.release_name=p.release_name
        where c.epoch=$1 and c.is_new and s.posted between u.first_posted-$2 and u.last_posted+$2 and p.oldest>=$3",
        &[&epoch, &gap, &keep],
    )?;
    tx.execute("update linker_epochs set phase='files' where id=$1", &[&epoch])?;
    tx.commit()?;
    Ok(())
}

const CANDIDATES:&str="
with eligible as (
 select s.source,s.ordinal,s.poster,s.message_key,s.message_id,b.group_name,b.day,b.seq,e.old_block,
    (not e.old_block or exists(select 1 from linker_epoch_hours h where h.epoch=$1 and h.hour=floor(s.posted::numeric/3600)::bigint)) as is_new
 from linker_epoch_sources e join linker_source_blocks b on b.id=e.source join linker_staged_rows s on s.source=b.id
 where e.epoch=$1 and not s.published and (
    (e.old_block and s.posted between $2::bigint and $3::bigint) or
    (not e.old_block and (exists(select 1 from linker_linked_hours h where h.hour=floor(s.posted::numeric/3600)::bigint)
      or exists(select 1 from linker_epoch_hours h where h.epoch=$1 and h.hour=floor(s.posted::numeric/3600)::bigint))))
 and not exists(select 1 from linker_linked_ids i where i.message_key=s.message_key and i.message_id=s.message_id and i.added>=$4)
 and not exists(select 1 from linker_legacy_ids i where i.hash=s.legacy_hash and i.added>=$4)
), winners as (
 select distinct on(message_key,message_id) * from eligible
 order by message_key,message_id,old_block desc,group_name collate \"C\",day,seq,ordinal,source
), ranked as (
 select *,row_number() over(order by old_block desc,group_name collate \"C\",day,seq,ordinal,source) as rank from winners
) insert into linker_candidates(epoch,source,source_ordinal,source_rank,poster_rank,is_new)
 select $1,source,ordinal,rank,min(rank) over(partition by poster),is_new from ranked";

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn old_context_wins_cross_group_duplicate_and_future_rows_stay_pending() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one(
                "insert into linker_epochs(logical_now,phase,lo,hi) values(20000,'select',3600,10800) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        c.execute("insert into linker_epoch_hours values($1,2)", &[&epoch]).unwrap();
        c.execute("insert into linker_linked_hours values(1)", &[]).unwrap();
        let mut sources = Vec::new();
        for (group, old) in [("first.example", true), ("second.example", false)] {
            let source:i64=c.query_one("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete,was_read)
                values($1,0,$2,0,$2,3,3,true,$3) returning id",&[&group,&&[1u8][..],&old]).unwrap().get(0);
            c.execute("insert into linker_epoch_sources values($1,$2,$3)", &[&epoch, &source, &old]).unwrap();
            sources.push(source);
        }
        for (source, ordinal, message, poster, posted) in [
            (sources[0], 1i64, "<same@example.invalid>", "old@example.invalid", 6000i64),
            (sources[1], 1, "<same@example.invalid>", "late@example.invalid", 6000),
            (sources[1], 2, "<new@example.invalid>", "new@example.invalid", 9000),
            (sources[1], 3, "<future@example.invalid>", "future@example.invalid", 18000),
        ] {
            let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines)
                values($1,$2,$3,0,$4,$5,$6,$1,$2,100,1)",&[&source,&ordinal,&&key[..],&message.as_bytes(),&poster.as_bytes(),&posted]).unwrap();
        }
        select(&mut c, epoch).unwrap();
        select(&mut c, epoch).unwrap();
        let rows = c
            .query(
                "select s.poster,r.is_new from linker_selected_rows r join linker_staged_rows s
            on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1 order by r.source_rank limit 3",
                &[&epoch],
            )
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, Vec<u8>>(0), b"old@example.invalid");
        assert!(!rows[0].get::<_, bool>(1));
        assert_eq!(rows[1].get::<_, Vec<u8>>(0), b"new@example.invalid");
        assert!(rows[1].get::<_, bool>(1));
        let pending: i64 =
            c.query_one("select count(*) from linker_staged_rows where not published", &[]).unwrap().get(0);
        assert_eq!(pending, 4, "selection does not acknowledge filtered future material");
    }
}
