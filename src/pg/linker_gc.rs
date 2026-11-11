//! Bounded disposal of acknowledged work. Pending source rows are never aged
//! out here: ambiguous/unpublished material remains recoverable on disk.
use anyhow::{Result, ensure};
use postgres::Client;

// Children precede parents so deleting one parent cannot cascade through a
// huge file/upload. Each step removes at most `budget` rows from ONE table.
const LEAVES: &[&str] = &[
    "linker_emission_rows",
    "linker_planned_firsts",
    "linker_planned_leads",
    "linker_planned_upload_groups",
    "linker_chain_groups",
    "linker_chain_files",
    "linker_file_rows",
    "linker_file_groups",
    "linker_seed_groups",
    "linker_inventory_blocks",
    "linker_epoch_sources",
    "linker_epoch_pending_sources",
    "linker_candidates",
    "linker_selected_rows",
    "linker_late_rows",
    "linker_late_files",
    "linker_planned_places",
    "linker_planned_uploads",
    "linker_emission_files",
    "linker_chains",
    "linker_files",
    "linker_epoch_seeds",
    "linker_epoch_inventory",
    "linker_epoch_hours",
];

/// True means useful cleanup was committed; retry before opening a new epoch.
/// Latest completed epoch's scalar waiting summary survives body disposal.
pub(crate) fn step(client: &mut Client, now: i64, budget: i64) -> Result<bool> {
    ensure!(budget > 0, "native cleanup budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let active: bool = tx.query_one("select exists(select 1 from linker_epochs where phase<>'done')", &[])?.get(0);
    if active {
        return Ok(false);
    }
    if let Some(row) = tx.query_opt(
        "select id from linker_epochs where phase='done' and not cleaned order by id limit 1 for update",
        &[],
    )? {
        let epoch: i64 = row.get(0);
        for table in LEAVES {
            // Identifiers are exclusively this private static allowlist.
            let n = tx.execute(
                &format!("delete from {table} where ctid in (select ctid from {table} where epoch=$1 limit $2)"),
                &[&epoch, &budget],
            )?;
            if n > 0 {
                tx.commit()?;
                return Ok(true);
            }
        }
        tx.execute("update linker_epochs set cleaned=true where id=$1", &[&epoch])?;
        tx.commit()?;
        return Ok(true);
    }
    let n = tx.execute(
        "delete from linker_epochs where id in (select id from linker_epochs where cleaned and phase='done'
        and id<(select max(id) from linker_epochs where phase='done') order by id limit $1)",
        &[&budget],
    )?;
    if n > 0 {
        tx.commit()?;
        return Ok(true);
    }
    // Every epoch is now completed and its references were disposed above.
    // Only rows whose writer receipt committed (including exact duplicate
    // copies of that same canonical ID) can be removed. Complete source
    // manifests remain, authorizing exact-generation raw retention coverage.
    let n = tx.execute(
        "delete from linker_staged_rows where ctid in
        (select ctid from linker_staged_rows where published limit $1)",
        &[&budget],
    )?;
    if n > 0 {
        tx.commit()?;
        return Ok(true);
    }
    if let Some(work) = tx.query_opt(
        "select message_key,message_id from linker_duplicate_work order by message_key limit 1 for update",
        &[],
    )? {
        let key: Vec<u8> = work.get(0);
        let message: Vec<u8> = work.get(1);
        let n = tx.execute(
            "delete from linker_staged_rows where ctid in(select s.ctid from linker_staged_rows s
            join linker_linked_ids i on i.message_key=s.message_key and i.message_id=s.message_id
            where s.message_key=$1 and s.message_id=$2 limit $3)",
            &[&key, &message, &budget],
        )?;
        if n > 0 {
            tx.commit()?;
            return Ok(true);
        }
        // Empty identity jobs are retired a bounded page at a time. A live job
        // stays until all exact copies are gone; hash-only matches never count.
        tx.execute("delete from linker_duplicate_work w where w.message_key in
            (select message_key from linker_duplicate_work order by message_key limit $1)
            and (not exists(select 1 from linker_staged_rows s where s.message_key=w.message_key and s.message_id=w.message_id)
              or not exists(select 1 from linker_linked_ids i where i.message_key=w.message_key and i.message_id=w.message_id))",&[&budget])?;
        tx.commit()?;
        return Ok(true);
    }
    tx.commit()?;
    Ok(super::linker_ids::prune(client, now, budget)? > 0)
}

/// Restart-time metadata retention follows legacy Links::open. A frozen
/// in-flight epoch must finish before its seed/placement inputs can expire.
pub(crate) enum MetadataExpiry {
    Deferred,
    Progress,
    Complete,
}
pub(crate) fn expire_metadata(client: &mut Client, session_now: i64, budget: i64) -> Result<MetadataExpiry> {
    ensure!(budget > 0, "native metadata expiry budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let active: bool = tx.query_one("select exists(select 1 from linker_epochs where phase<>'done')", &[])?.get(0);
    if active {
        return Ok(MetadataExpiry::Deferred);
    }
    let before = session_now.saturating_sub(8 * crate::xref::KEEP_SECS);
    if let Some(row) = tx.query_opt(
        "select poster from linker_places where added<$1 order by added,poster limit 1 for update",
        &[&before],
    )? {
        let poster: Vec<u8> = row.get(0);
        for table in ["linker_place_leads", "linker_place_firsts"] {
            let n = tx.execute(
                &format!("delete from {table} where ctid in(select ctid from {table} where poster=$1 limit $2)"),
                &[&poster, &budget],
            )?;
            if n > 0 {
                tx.commit()?;
                return Ok(MetadataExpiry::Progress);
            }
        }
        tx.execute("delete from linker_places where poster=$1", &[&poster])?;
        tx.commit()?;
        return Ok(MetadataExpiry::Progress);
    }
    if let Some(row) = tx.query_opt(
        "select release_name from linker_uploads where added<$1 order by added,release_name limit 1 for update",
        &[&before],
    )? {
        let release: String = row.get(0);
        let n=tx.execute("delete from linker_upload_groups where ctid in(select ctid from linker_upload_groups where release_name=$1 limit $2)",&[&release,&budget])?;
        if n == 0 {
            tx.execute("delete from linker_uploads where release_name=$1", &[&release])?;
        }
        tx.commit()?;
        return Ok(MetadataExpiry::Progress);
    }
    tx.commit()?;
    Ok(MetadataExpiry::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_expiry_defers_frozen_epochs_and_pages_large_memberships() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let poster = b"poster@example.invalid".as_slice();
        c.execute("insert into linker_places values($1,'old-upload','group.example',1,1,12,100,100)", &[&poster])
            .unwrap();
        c.batch_execute(
            "insert into linker_uploads values('old-upload','group.example',99000,99000,1,2,3,100);
            insert into linker_upload_groups select 'old-upload',g from generate_series(1,5) g",
        )
        .unwrap();
        c.execute("insert into linker_place_leads select $1,n,n from generate_series(1,5) n", &[&poster]).unwrap();
        let epoch = super::super::linker_epoch::open(&mut c, 200).unwrap();
        assert!(matches!(expire_metadata(&mut c, 1_000_000, 2).unwrap(), MetadataExpiry::Deferred));
        assert_eq!(c.query_one("select count(*) from linker_place_leads", &[]).unwrap().get::<_, i64>(0), 5);
        c.execute("update linker_epochs set phase='done' where id=$1", &[&epoch]).unwrap();
        assert!(matches!(expire_metadata(&mut c, 1_000_000, 2).unwrap(), MetadataExpiry::Progress));
        assert_eq!(c.query_one("select count(*) from linker_place_leads", &[]).unwrap().get::<_, i64>(0), 3);
        for turn in 0..30 {
            assert!(turn < 29);
            if matches!(expire_metadata(&mut c, 1_000_000, 2).unwrap(), MetadataExpiry::Complete) {
                break;
            }
        }
        assert_eq!(c.query_one("select count(*) from linker_places", &[]).unwrap().get::<_, i64>(0), 0);
        assert_eq!(c.query_one("select count(*) from linker_uploads", &[]).unwrap().get::<_, i64>(0), 0);
    }

    #[test]
    fn cleanup_restarts_in_pages_and_keeps_pending_rows_and_latest_waiting_summary() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let source:i64=c.query_one("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete) values('group.example',1,$1,0,$1,5,5,true) returning id",&[&&[1u8;32][..]]).unwrap().get(0);
        let epoch: i64 = c
            .query_one(
                "insert into linker_epochs(logical_now,phase,waiting_from) values(100000,'ack',99000) returning id",
                &[],
            )
            .unwrap()
            .get(0);
        for n in 1..=5i64 {
            let text = format!("<part{n}@example.invalid>");
            c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines,published) values($1,$2,$3,0,$3,$3,99000,1,$2,100,1,$4)",&[&source,&n,&text.as_bytes(),&(n<=3)]).unwrap();
            c.execute("insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new) values($1,$2,$3,$3,false,true)",&[&epoch,&source,&n]).unwrap();
        }
        assert!(!step(&mut c, 100000, 2).unwrap());
        c.execute("update linker_epochs set phase='done' where id=$1", &[&epoch]).unwrap();
        assert!(step(&mut c, 100000, 2).unwrap());
        assert_eq!(c.query_one("select count(*) from linker_selected_rows", &[]).unwrap().get::<_, i64>(0), 3);
        drop(c);
        let mut c = schema.pool.get().unwrap();
        for turn in 0..100 {
            assert!(turn < 99);
            if !step(&mut c, 100000, 2).unwrap() {
                break;
            }
        }
        assert_eq!(c.query_one("select count(*) from linker_staged_rows", &[]).unwrap().get::<_, i64>(0), 2);
        let duplicate = b"<part4@example.invalid>".as_slice();
        c.execute(
            "insert into linker_linked_ids values($1,$2,100000)",
            &[&duplicate, &b"<different@example.invalid>".as_slice()],
        )
        .unwrap();
        assert!(!step(&mut c, 100000, 2).unwrap(), "hash equality alone must not dispose a row");
        c.execute("update linker_linked_ids set message_id=$1", &[&duplicate]).unwrap();
        c.execute("insert into linker_duplicate_work values($1,$1)", &[&duplicate]).unwrap();
        assert!(step(&mut c, 100000, 2).unwrap());
        assert_eq!(c.query_one("select count(*) from linker_staged_rows", &[]).unwrap().get::<_, i64>(0), 1);

        assert_eq!(c.query_one("select waiting_from from linker_epochs", &[]).unwrap().get::<_, i64>(0), 99000);
        assert!(super::super::linker_stage::covered(&mut c, "group.example", 1, [1u8; 32], 1).unwrap());
        assert!(c.query_one("select complete from linker_source_blocks", &[]).unwrap().get::<_, bool>(0));
    }
}
