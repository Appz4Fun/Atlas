//! Durable winner selection in old-before-late encounter order. Each step
//! visits a bounded number of source/row keys, including rejected duplicates.
use anyhow::{Result, ensure};
use postgres::Client;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Cursor {
    ordered: bool,
    copied: i64,
    source: Option<i64>,
    ordinal: i64,
    rank: i64,
}

pub(crate) fn select_step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "selection budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let epoch_row =
        tx.query_one("select phase,logical_now,lo,hi from linker_epochs where id=$1 for update", &[&epoch])?;
    let phase: String = epoch_row.get(0);
    if phase != "select" {
        ensure!(
            ["files", "chain", "plan", "emit", "ack", "done"].contains(&phase.as_str()),
            "selection requires sealed scope"
        );
        return Ok(true);
    }
    let now: i64 = epoch_row.get(1);
    let lo: Option<i64> = epoch_row.get(2);
    let hi: Option<i64> = epoch_row.get(3);
    let before = now.saturating_sub(8 * crate::xref::KEEP_SECS);
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='select'", &[&epoch])?;
    let mut state: Cursor =
        saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let mut done = false;
    for _ in 0..budget {
        if !state.ordered {
            let next = tx.query_opt(
                "select e.source,e.old_block,e.new_block,b.group_name,b.day,b.seq
                from linker_epoch_sources e join linker_source_blocks b on b.id=e.source
                where e.epoch=$1 and e.source>$2 order by e.source limit 1",
                &[&epoch, &state.copied],
            )?;
            if let Some(row) = next {
                let source: i64 = row.get(0);
                let old: bool = row.get(1);
                tx.execute(
                    "insert into linker_selection_sources(epoch,source,priority,new_block,group_name,day,seq)
                    values($1,$2,$3,$4,$5,$6,$7)",
                    &[
                        &epoch,
                        &source,
                        &i16::from(!old),
                        &row.get::<_, bool>(2),
                        &row.get::<_, String>(3),
                        &row.get::<_, i64>(4),
                        &row.get::<_, i64>(5),
                    ],
                )?;
                state.copied = source;
            } else {
                state.ordered = true;
            }
            continue;
        }
        if state.source.is_none() {
            let next = tx.query_opt(
                "select source from linker_selection_sources where epoch=$1 and not consumed
                order by priority,group_name collate \"C\",day,seq,source limit 1",
                &[&epoch],
            )?;
            if let Some(row) = next {
                state.source = Some(row.get(0));
                state.ordinal = -1;
            } else {
                tx.execute("update linker_epochs set phase='files' where id=$1", &[&epoch])?;
                done = true;
                break;
            }
            continue;
        }
        let source = state.source.unwrap();
        let next = tx.query_opt(
            "select ordinal,message_key,message_id,legacy_hash,poster,posted,published
            from linker_staged_rows where source=$1 and ordinal>$2 order by ordinal limit 1",
            &[&source, &state.ordinal],
        )?;
        let Some(row) = next else {
            tx.execute(
                "update linker_selection_sources set consumed=true where epoch=$1 and source=$2",
                &[&epoch, &source],
            )?;
            state.source = None;
            continue;
        };
        let ordinal: i64 = row.get(0);
        state.ordinal = ordinal;
        if row.get::<_, bool>(6) {
            continue;
        }
        let posted: i64 = row.get(5);
        let hour = posted.div_euclid(3600);
        let flags = tx.query_one(
            "select priority=0,new_block,
            exists(select 1 from linker_linked_hours where hour=$3),
            exists(select 1 from linker_epoch_hours where epoch=$1 and hour=$3)
            from linker_selection_sources where epoch=$1 and source=$2",
            &[&epoch, &source, &hour],
        )?;
        let old: bool = flags.get(0);
        let new: bool = flags.get(1);
        let linked: bool = flags.get(2);
        let ready: bool = flags.get(3);
        if !(old && lo.zip(hi).is_some_and(|(lo, hi)| posted >= lo && posted <= hi) || new && (linked || ready)) {
            continue;
        }
        let is_new = new && linked || ready;
        let key: Vec<u8> = row.get(1);
        let message: Vec<u8> = row.get(2);
        let hash: i64 = row.get(3);
        let remembered: bool = tx
            .query_one(
                "select exists(select 1 from linker_linked_ids where message_key=$1 and message_id=$2 and added>=$4)
            or exists(select 1 from linker_legacy_ids where hash=$3 and added>=$4)",
                &[&key, &message, &hash, &before],
            )?
            .get(0);
        if remembered {
            continue;
        }
        if let Some(winner) = tx.query_opt(
            "select message_id from linker_selection_winners where epoch=$1 and message_key=$2",
            &[&epoch, &key],
        )? {
            ensure!(winner.get::<_, Vec<u8>>(0) == message, "canonical message digest collision");
            continue;
        }
        tx.execute("insert into linker_selection_winners values($1,$2,$3)", &[&epoch, &key, &message])?;
        state.rank += 1;
        let poster: Vec<u8> = row.get(4);
        tx.execute(
            "insert into linker_poster_order values($1,$2,$3) on conflict do nothing",
            &[&epoch, &poster, &state.rank],
        )?;
        let poster_rank: i64 = tx
            .query_one(
                "select first_rank from linker_poster_order where epoch=$1 and poster_key=sha256($2) and poster=$2",
                &[&epoch, &poster],
            )?
            .get(0);
        tx.execute("insert into linker_candidates(epoch,source,source_ordinal,source_rank,poster_rank,is_new) values($1,$2,$3,$4,$5,$6)",
            &[&epoch,&source,&ordinal,&state.rank,&poster_rank,&is_new])?;
        let placement = tx.query_opt(
            "select p.oldest,u.first_posted,u.last_posted from linker_places p
            left join linker_uploads u on u.release_name=p.release_name where p.poster_key=sha256($1) and p.poster=$1",
            &[&poster],
        )?;
        let again = placement.is_some();
        let within = placement
            .as_ref()
            .and_then(|r| r.get::<_, Option<i64>>(1).zip(r.get::<_, Option<i64>>(2)))
            .is_some_and(|(first, last)| {
                posted >= first.saturating_sub(crate::nxv_store::linker::JOIN_GAP)
                    && posted <= last.saturating_add(crate::nxv_store::linker::JOIN_GAP)
            });
        if !again || is_new && !within {
            tx.execute("insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new,poster_rank) values($1,$2,$3,$4,$5,$6,$7)",
                &[&epoch,&source,&ordinal,&state.rank,&again,&is_new,&poster_rank])?;
        } else if is_new
            && within
            && placement.as_ref().unwrap().get::<_, i64>(0) >= now.saturating_sub(crate::xref::KEEP_SECS)
        {
            tx.execute("insert into linker_late_rows(epoch,source,source_ordinal,source_rank,poster_rank) values($1,$2,$3,$4,$5)",
                &[&epoch,&source,&ordinal,&state.rank,&poster_rank])?;
        }
    }
    tx.execute(
        "insert into linker_work values($1,'select',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&state)?],
    )?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
pub(crate) fn select(client: &mut Client, epoch: i64) -> Result<()> {
    while !select_step(client, epoch, 2)? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn poster_handle_collision_never_overwrites_exact_identity() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 =
            c.query_one("insert into linker_epochs(logical_now) values(1) returning id", &[]).unwrap().get(0);
        c.execute(
            "insert into linker_poster_order values($1,$2,1)",
            &[&epoch, &b"original@example.invalid".as_slice()],
        )
        .unwrap();
        // Simulate a digest collision without claiming to find a SHA-256 collision.
        c.batch_execute("alter table linker_poster_order alter column poster_key drop expression").unwrap();
        c.execute("update linker_poster_order set poster_key=sha256($1)", &[&b"other@example.invalid".as_slice()])
            .unwrap();
        let error=c.execute("insert into linker_poster_order values($1,$2,2,sha256($2)) on conflict(epoch,poster_key) do update set first_rank=excluded.first_rank", &[&epoch,&b"other@example.invalid".as_slice()]).unwrap_err();
        assert!(error.to_string().contains("db error"));
        assert!(error.as_db_error().unwrap().message().contains("poster digest collision"));
        let row = c.query_one("select poster,first_rank from linker_poster_order", &[]).unwrap();
        assert_eq!(row.get::<_, Vec<u8>>(0), b"original@example.invalid");
        assert_eq!(row.get::<_, i64>(1), 1);
    }

    #[test]
    fn duplicate_heavy_selection_advances_one_scanned_row_per_step() {
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
        let source: i64 = c
            .query_one(
                "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete)
            values('group.example',0,$1,0,$1,113,113,true) returning id",
                &[&&[1u8][..]],
            )
            .unwrap()
            .get(0);
        c.execute("insert into linker_epoch_sources values($1,$2,false,true)", &[&epoch, &source]).unwrap();
        c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines)
            select $1,n,decode(md5('same'),'hex'),0,convert_to('<same@example.invalid>','UTF8'),convert_to('poster@example.invalid','UTF8'),9000,1,n,100,1
            from generate_series(1,113) n",&[&source]).unwrap();
        c.batch_execute("create table selection_audit(tx bigint primary key, mutations bigint not null);
            create function audit_selection() returns trigger language plpgsql as $$ begin
              insert into selection_audit values(txid_current(),1) on conflict(tx) do update set mutations=selection_audit.mutations+1;
              return null; end $$").unwrap();
        for table in [
            "linker_selection_sources",
            "linker_selection_winners",
            "linker_poster_order",
            "linker_candidates",
            "linker_selected_rows",
            "linker_late_rows",
            "linker_work",
            "linker_epochs",
        ] {
            c.batch_execute(&format!("create trigger audit after insert or update or delete on {table} for each row execute function audit_selection()" )).unwrap();
        }
        let mut previous = 0;
        loop {
            let done = select_step(&mut c, epoch, 1).unwrap();
            let saved: String = c
                .query_one("select state from linker_work where epoch=$1 and kind='select'", &[&epoch])
                .unwrap()
                .get(0);
            let state: Cursor = serde_json::from_str(&saved).unwrap();
            if state.ordinal > 0 {
                assert!(state.ordinal <= previous + 1);
                previous = state.ordinal;
            }
            if previous == 41 {
                drop(c);
                c = schema.pool.get().unwrap();
            }
            if done {
                break;
            }
        }
        assert_eq!(previous, 113);
        assert_eq!(
            c.query_one("select count(*) from linker_candidates where epoch=$1", &[&epoch]).unwrap().get::<_, i64>(0),
            1
        );
        assert!(c.query_one("select max(mutations) from selection_audit", &[]).unwrap().get::<_, i64>(0) <= 5);
    }

    #[test]
    fn legacy_priority_future_context_is_not_a_ready_late_part() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one(
                "insert into linker_epochs(logical_now,phase,lo,hi) values(120000,'select',90000,111599) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        c.execute("insert into linker_epoch_hours values($1,27),($1,28)", &[&epoch]).unwrap();
        c.batch_execute("insert into linker_linked_hours values(26)").unwrap();
        let poster = b"placed@example.invalid".as_slice();
        c.execute(
            "insert into linker_places values($1,'xref-upload-generic','group.example',1,1,12,120000,120000)",
            &[&poster],
        )
        .unwrap();
        c.batch_execute(
            "insert into linker_uploads values('xref-upload-generic','group.example',95400,95400,1,0,0,120000)",
        )
        .unwrap();
        let source:i64=c.query_one("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete,legacy_priority) values('group.example',1,$1,0,$1,1,1,true,true) returning id",&[&&[1u8][..]]).unwrap().get(0);
        c.execute("insert into linker_epoch_sources values($1,$2,true,true)", &[&epoch, &source]).unwrap();
        let message = b"<future@example.invalid>".as_slice();
        let key: [u8; 32] = Sha256::digest(message).into();
        c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines) values($1,1,$2,0,$3,$4,104500,1,1,100,1)",&[&source,&&key[..],&message,&poster]).unwrap();
        select(&mut c, epoch).unwrap();
        assert_eq!(c.query_one("select count(*) from linker_late_rows", &[]).unwrap().get::<_, i64>(0), 0);
        assert!(!c.query_one("select published from linker_staged_rows", &[]).unwrap().get::<_, bool>(0));
        c.execute("update linker_epochs set phase='done' where id=$1", &[&epoch]).unwrap();
        let next: i64 = c
            .query_one(
                "insert into linker_epochs(logical_now,phase,lo,hi) values(130000,'select',97200,115199) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        c.execute("insert into linker_epoch_hours values($1,29)", &[&next]).unwrap();
        c.execute("insert into linker_epoch_sources values($1,$2,true,false)", &[&next, &source]).unwrap();
        select(&mut c, next).unwrap();
        assert_eq!(
            c.query_one("select count(*) from linker_late_rows where epoch=$1", &[&next]).unwrap().get::<_, i64>(0),
            1
        );
    }

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
            c.execute(
                "insert into linker_epoch_sources(epoch,source,old_block,new_block) values($1,$2,$3,not $3)",
                &[&epoch, &source, &old],
            )
            .unwrap();
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
