//! Frozen release/part plans and metadata effects. PostgreSQL externally orders
//! every file's parts and retains the plans through publication receipts. No
//! Article tree or complete membership list is retained by the planner.

use anyhow::{Context, Result, ensure};
use postgres::{Client, Transaction};

/// Copy active seeds in their legacy order, then widen them from late parts
/// before fresh files are chained. Memberships stay normalized throughout.
pub(crate) fn prepare(client: &mut Client, epoch: i64) -> Result<()> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state =
        tx.query_one("select phase,logical_now,seeds_prepared from linker_epochs where id=$1 for update", &[&epoch])?;
    if state.get::<_, bool>(2) {
        return Ok(());
    }
    ensure!(state.get::<_, String>(0) == "files", "native seed preparation requires sealed selection");
    let now: i64 = state.get(1);
    tx.execute("insert into linker_epoch_seeds(epoch,id,release_name,group_name,first_posted,last_posted,files,generation)
        select $1,row_number() over(order by first_posted,release_name collate \"C\")-1,release_name,group_name,first_posted,last_posted,files,generation
        from linker_uploads where added>=$2",&[&epoch,&now.saturating_sub(crate::xref::KEEP_SECS)])?;
    tx.execute(
        "insert into linker_seed_groups select s.epoch,s.id,g.group_id from linker_epoch_seeds s
        join linker_upload_groups g on g.release_name=s.release_name where s.epoch=$1",
        &[&epoch],
    )?;
    tx.execute("insert into linker_late_files(epoch,poster,encounter_rank,articles)
        select $1,s.poster,min(r.poster_rank),count(*) from linker_late_rows r
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1 group by s.poster",&[&epoch])?;
    tx.execute("with growth as (
        select p.release_name,min(s.posted) as first,max(s.posted) as last from linker_late_rows r
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal join linker_places p on p.poster=s.poster
        where r.epoch=$1 group by p.release_name)
        update linker_epoch_seeds e set first_posted=least(e.first_posted,g.first),last_posted=greatest(e.last_posted,g.last),grown=true
        from growth g where e.epoch=$1 and e.release_name=g.release_name",&[&epoch])?;
    tx.execute("insert into linker_seed_groups select distinct $1,e.id,s.group_id from linker_late_rows r
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
        join linker_places p on p.poster=s.poster join linker_epoch_seeds e on e.epoch=$1 and e.release_name=p.release_name
        where r.epoch=$1 on conflict do nothing",&[&epoch])?;
    tx.execute(
        "insert into linker_chains(epoch,id,seed,first_posted,last_posted,index_only)
        select epoch,id,id,first_posted,last_posted,false from linker_epoch_seeds where epoch=$1",
        &[&epoch],
    )?;
    tx.execute(
        "insert into linker_chain_groups select epoch,seed,0,group_id from linker_seed_groups where epoch=$1
        union all select epoch,seed,2,group_id from linker_seed_groups where epoch=$1",
        &[&epoch],
    )?;
    tx.execute("insert into linker_planned_uploads select epoch,release_name,group_name,first_posted,last_posted,files,generation+1,0,$2
        from linker_epoch_seeds where epoch=$1 and grown",&[&epoch,&now])?;
    tx.execute(
        "insert into linker_planned_upload_groups select g.epoch,s.release_name,g.group_id from linker_seed_groups g
        join linker_epoch_seeds s on s.epoch=g.epoch and s.id=g.seed where g.epoch=$1 and s.grown",
        &[&epoch],
    )?;
    tx.execute("update linker_epochs set seeds_prepared=true,next_chain=(select count(*) from linker_epoch_seeds where epoch=$1),
        waiting_from=case when exists(select 1 from linker_epoch_hours where epoch=$1) then null else waiting_from end where id=$1",&[&epoch])?;
    tx.commit()?;
    Ok(())
}

fn header(
    tx: &mut Transaction<'_>,
    epoch: i64,
    release: &str,
    group: &str,
    poster: &[u8],
    posted: i64,
) -> Result<(Vec<u8>, String)> {
    if let Some(row) = tx.query_opt(
        "select release_poster,release_date from linker_emission_files
        where epoch=$1 and release_name=$2 and group_name=$3 order by id limit 1",
        &[&epoch, &release, &group],
    )? {
        Ok((row.get(0), row.get(1)))
    } else {
        Ok((poster.to_vec(), crate::xref::iso_pub(posted)))
    }
}

/// One late file is planned atomically in external SQL order, including its
/// first-piece hints. Returns false when no late file remains.
pub(crate) fn late_one(client: &mut Client, epoch: i64) -> Result<bool> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state = tx.query_one(
        "select phase,logical_now,next_plan_file,planned_rows from linker_epochs where id=$1 for update",
        &[&epoch],
    )?;
    ensure!(state.get::<_, String>(0) == "plan", "native late planning requires completed chains");
    let now: i64 = state.get(1);
    let plan: i64 = state.get(2);
    let start: i64 = state.get(3);
    let next = tx.query_opt(
        "select f.poster,f.articles,p.release_name,p.group_name,p.file_number,p.file_total,p.parts,p.oldest
        from linker_late_files f join linker_places p on p.poster=f.poster
        where f.epoch=$1 and not f.planned order by f.encounter_rank limit 1",
        &[&epoch],
    )?;
    let Some(next) = next else { return Ok(false) };
    let poster: Vec<u8> = next.get(0);
    let n: i64 = next.get(1);
    let release: String = next.get(2);
    let group: String = next.get(3);
    let file: i64 = next.get(4);
    let files: i64 = next.get(5);
    let before: i64 = next.get(6);
    let oldest: i64 = next.get(7);
    let posted: i64 = tx
        .query_one(
            "select s.posted from linker_late_rows r join linker_staged_rows s
        on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1 and s.poster=$2
        order by s.group_id,s.article_number,s.message_id limit 1",
            &[&epoch, &poster],
        )?
        .get(0);
    let (release_poster, date) = header(&mut tx, epoch, &release, &group, &poster, posted)?;
    tx.execute(
        "insert into linker_emission_files values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        &[&epoch, &plan, &release, &group, &release_poster, &date, &poster, &file, &files, &(before + n)],
    )?;
    tx.execute(
        "insert into linker_emission_rows
        select $1,$3+row_number() over(order by s.group_id,s.article_number,s.message_id),$4,r.source,r.source_ordinal,
          $5+row_number() over(order by s.group_id,s.article_number,s.message_id)
        from linker_late_rows r join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
        where r.epoch=$1 and s.poster=$2",
        &[&epoch, &poster, &start, &plan, &before],
    )?;
    tx.execute("insert into linker_planned_places values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
        on conflict(epoch,poster) do update set plan_file=excluded.plan_file,release_name=excluded.release_name,group_name=excluded.group_name,
        file_number=excluded.file_number,file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",
        &[&epoch,&poster,&plan,&release,&group,&file,&files,&(before+n),&oldest,&now])?;
    tx.execute("delete from linker_planned_firsts where epoch=$1 and poster=$2", &[&epoch, &poster])?;
    tx.execute("delete from linker_planned_leads where epoch=$1 and poster=$2", &[&epoch, &poster])?;
    tx.execute("insert into linker_planned_firsts select $1,poster,group_id,article_number from linker_place_firsts where poster=$2",&[&epoch,&poster])?;
    tx.execute("insert into linker_planned_firsts select $1,$2,s.group_id,min(s.article_number) from linker_late_rows r
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal where r.epoch=$1 and s.poster=$2 group by s.group_id
        on conflict(epoch,poster,group_id) do update set article_number=least(linker_planned_firsts.article_number,excluded.article_number)",&[&epoch,&poster])?;
    tx.execute(
        "insert into linker_planned_leads select $1,poster,ordinal,part from linker_place_leads where poster=$2",
        &[&epoch, &poster],
    )?;
    tx.execute("with hints as (
        select s.group_id,min(s.article_number) as number,min(e.part) as part from linker_emission_rows e
        join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
        where e.epoch=$1 and e.file_id=$3 group by s.group_id
    ) insert into linker_planned_leads
      select $1,$2,(select coalesce(max(ordinal),0) from linker_place_leads where poster=$2)+row_number() over(order by h.group_id),h.part
      from hints h left join linker_place_firsts old on old.poster=$2 and old.group_id=h.group_id
      where old.article_number is null or h.number<old.article_number",&[&epoch,&poster,&plan])?;
    tx.execute("update linker_late_files set planned=true where epoch=$1 and poster=$2", &[&epoch, &poster])?;
    tx.execute(
        "update linker_epochs set next_plan_file=$2::bigint+1,planned_rows=$3::bigint+$4::bigint where id=$1",
        &[&epoch, &plan, &start, &n],
    )?;
    tx.commit()?;
    Ok(true)
}

/// Plan one chained upload; return true when all plans are sealed for emission.
/// A huge upload's ordering and metadata effects spill entirely in PostgreSQL.
pub(crate) fn chain_one(client: &mut Client, epoch: i64) -> Result<bool> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state = tx.query_one(
        "select phase,logical_now,next_plan_file,planned_rows from linker_epochs where id=$1 for update",
        &[&epoch],
    )?;
    let phase: String = state.get(0);
    if ["emit", "ack", "done"].contains(&phase.as_str()) {
        return Ok(true);
    }
    ensure!(phase == "plan", "native upload planning requires completed chains");
    let now: i64 = state.get(1);
    let base: i64 = state.get(2);
    let start: i64 = state.get(3);
    let late: bool = tx
        .query_one("select exists(select 1 from linker_late_files where epoch=$1 and not planned)", &[&epoch])?
        .get(0);
    ensure!(!late, "native late files must precede fresh upload plans");
    let chain = tx.query_opt(
        "select id,seed,first_posted,last_posted from linker_chains
        where epoch=$1 and file_count>0 and not planned order by id limit 1",
        &[&epoch],
    )?;
    let Some(chain) = chain else {
        tx.execute("update linker_epochs set phase='emit' where id=$1", &[&epoch])?;
        tx.commit()?;
        return Ok(true);
    };
    let id: i64 = chain.get(0);
    let seed: Option<i64> = chain.get(1);
    let upload_first: i64 = chain.get(2);
    let upload_last: i64 = chain.get(3);
    let summary=tx.query_one("select min(f.first_posted),max(f.last_posted),sum(f.articles)::bigint,bool_or(f.new_material),count(*)
        from linker_chain_files m join linker_files f on f.epoch=m.epoch and f.id=m.file_id where m.epoch=$1 and m.chain_id=$2",&[&epoch,&id])?;
    let first: i64 = summary.get(0);
    let last: i64 = summary.get(1);
    let articles: i64 = summary.get(2);
    let material: bool = summary.get(3);
    let files: i64 = summary.get(4);
    let ready: bool = tx
        .query_one(
            "select not exists(select 1 from generate_series($2::bigint,$3::bigint) h
        where not exists(select 1 from linker_linked_hours l where l.hour=h)
          and not exists(select 1 from linker_epoch_hours e where e.epoch=$1 and e.hour=h))",
            &[&epoch, &first.div_euclid(3600), &last.div_euclid(3600)],
        )?
        .get(0);
    if !ready {
        tx.execute("update linker_epochs set waiting_from=case when waiting_from is null then $2 else least(waiting_from,$2) end
            where id=$1 and exists(select 1 from linker_epoch_hours where epoch=$1)",&[&epoch,&first])?;
    }
    if !ready || !material || (seed.is_none() && articles < crate::xref::MIN_UPLOAD_ARTICLES) {
        tx.execute("update linker_chains set planned=true where epoch=$1 and id=$2", &[&epoch, &id])?;
        tx.commit()?;
        return Ok(false);
    }
    let first_file = tx.query_one(
        "select f.poster,f.first_posted,f.again,s.posted from linker_chain_files m
        join linker_files f on f.epoch=m.epoch and f.id=m.file_id
        join linker_file_rows r on r.epoch=f.epoch and r.file_id=f.id
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
        where m.epoch=$1 and m.chain_id=$2 order by m.ordinal,s.group_id,s.article_number,s.message_id limit 1",
        &[&epoch, &id],
    )?;
    let poster: Vec<u8> = first_file.get(0);
    let posted: i64 = first_file.get(3);
    let (release, group, before, generation) = if let Some(seed) = seed {
        let s = tx.query_one(
            "select release_name,group_name,files,generation from linker_epoch_seeds where epoch=$1 and id=$2",
            &[&epoch, &seed],
        )?;
        (s.get::<_, String>(0), s.get::<_, String>(1), s.get::<_, i64>(2), s.get::<_, i64>(3) + 1)
    } else {
        let poster_text = std::str::from_utf8(&poster).context("invalid native file poster")?;
        let key = if first_file.get::<_, bool>(2) {
            format!("{poster_text}\0{}", first_file.get::<_, i64>(1).div_euclid(3600))
        } else {
            poster_text.to_owned()
        };
        let group: String = tx
            .query_one(
                "select max(b.group_name collate \"C\") from linker_chain_files m
            join linker_file_rows r on r.epoch=m.epoch and r.file_id=m.file_id
            join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
            join linker_source_blocks b on b.id=s.source where m.epoch=$1 and m.chain_id=$2
            group by s.group_id order by count(*) desc,s.group_id limit 1",
                &[&epoch, &id],
            )?
            .get(0);
        (crate::xref::upload_name(&key), group, 0, 0)
    };
    let total = before.checked_add(files).context("native upload file count overflow")?;
    let (release_poster, date) = header(&mut tx, epoch, &release, &group, &poster, posted)?;
    let inserted = tx.execute(
        "insert into linker_emission_files
        select $1,$3+m.ordinal-1,$4,$5,$6,$7,f.poster,$8+m.ordinal,$9,f.articles
        from linker_chain_files m join linker_files f on f.epoch=m.epoch and f.id=m.file_id
        where m.epoch=$1 and m.chain_id=$2",
        &[&epoch, &id, &base, &release, &group, &release_poster, &date, &before, &total],
    )?;
    ensure!(inserted == files as u64, "native upload plan lost a file");
    let inserted = tx.execute(
        "insert into linker_emission_rows
        select $1,$3+row_number() over(order by m.ordinal,s.group_id,s.article_number,s.message_id),
          $4+m.ordinal-1,r.source,r.source_ordinal,
          row_number() over(partition by m.file_id order by s.group_id,s.article_number,s.message_id)
        from linker_chain_files m join linker_file_rows r on r.epoch=m.epoch and r.file_id=m.file_id
        join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
        where m.epoch=$1 and m.chain_id=$2",
        &[&epoch, &id, &start, &base],
    )?;
    ensure!(inserted == articles as u64, "native upload plan lost an article");
    let end = base.checked_add(files).context("native plan file counter overflow")?;
    // A repeated poster can contribute several runs to one chain. Only its
    // final F record wins, exactly as the legacy append log applies them.
    tx.execute("insert into linker_planned_places
        select distinct on(poster) $1,poster,id,release_name,group_name,file_number,file_total,part_total,$4,$4
        from linker_emission_files where epoch=$1 and id>=$2 and id<$3 order by poster,id desc
        on conflict(epoch,poster) do update set plan_file=excluded.plan_file,release_name=excluded.release_name,group_name=excluded.group_name,
        file_number=excluded.file_number,file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",
        &[&epoch,&base,&end,&now])?;
    tx.execute(
        "delete from linker_planned_firsts f where epoch=$1 and exists(select 1 from linker_planned_places p
        where p.epoch=f.epoch and p.poster=f.poster and p.plan_file>=$2 and p.plan_file<$3)",
        &[&epoch, &base, &end],
    )?;
    tx.execute(
        "delete from linker_planned_leads f where epoch=$1 and exists(select 1 from linker_planned_places p
        where p.epoch=f.epoch and p.poster=f.poster and p.plan_file>=$2 and p.plan_file<$3)",
        &[&epoch, &base, &end],
    )?;
    tx.execute(
        "insert into linker_planned_firsts select $1,p.poster,s.group_id,min(s.article_number)
        from linker_planned_places p join linker_emission_rows e on e.epoch=p.epoch and e.file_id=p.plan_file
        join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
        where p.epoch=$1 and p.plan_file>=$2 and p.plan_file<$3 group by p.poster,s.group_id",
        &[&epoch, &base, &end],
    )?;
    tx.execute("with hints as (
        select p.poster,s.group_id,min(e.part) as part from linker_planned_places p
        join linker_emission_rows e on e.epoch=p.epoch and e.file_id=p.plan_file
        join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
        where p.epoch=$1 and p.plan_file>=$2 and p.plan_file<$3 group by p.poster,s.group_id
    ) insert into linker_planned_leads select $1,poster,row_number() over(partition by poster order by group_id),part from hints",
        &[&epoch,&base,&end])?;
    tx.execute("insert into linker_planned_uploads values($1,$2,$3,$4,$5,$6,$7,0,$8)
        on conflict(epoch,release_name) do update set group_name=excluded.group_name,first_posted=excluded.first_posted,last_posted=excluded.last_posted,
        files=excluded.files,generation=excluded.generation,probed=excluded.probed,added=excluded.added",
        &[&epoch,&release,&group,&upload_first,&upload_last,&total,&generation,&now])?;
    tx.execute("delete from linker_planned_upload_groups where epoch=$1 and release_name=$2", &[&epoch, &release])?;
    tx.execute(
        "insert into linker_planned_upload_groups select $1,$2,group_id from linker_chain_groups
        where epoch=$1 and chain_id=$3 and kind=0",
        &[&epoch, &release, &id],
    )?;
    tx.execute("update linker_chains set planned=true where epoch=$1 and id=$2", &[&epoch, &id])?;
    tx.execute(
        "update linker_epochs set next_plan_file=$2,planned_rows=$3::bigint+$4::bigint where id=$1",
        &[&epoch, &end, &start, &articles],
    )?;
    tx.commit()?;
    Ok(false)
}

/// Commit metadata only after every immutable publication page was receipted.
/// An ambiguous acknowledgement is safe to repeat; no file/part counters move.
pub(crate) fn ack(client: &mut Client, epoch: i64) -> Result<()> {
    let mut tx = super::linker::durable_transaction(client)?;
    tx.batch_execute("set local work_mem='4MB'")?;
    let state =
        tx.query_one("select phase,emitted,planned_rows from linker_epochs where id=$1 for update", &[&epoch])?;
    let phase: String = state.get(0);
    if phase == "done" {
        return Ok(());
    }
    ensure!(
        phase == "ack" && state.get::<_, i64>(1) == state.get::<_, i64>(2),
        "native acknowledgement requires every publication receipt"
    );
    tx.execute("insert into linker_places select poster,release_name,group_name,file_number,file_total,parts,oldest,added from linker_planned_places where epoch=$1
      on conflict(poster) do update set release_name=excluded.release_name,group_name=excluded.group_name,file_number=excluded.file_number,
      file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",&[&epoch])?;
    tx.execute(
        "delete from linker_place_firsts where poster in(select poster from linker_planned_places where epoch=$1)",
        &[&epoch],
    )?;
    tx.execute(
        "delete from linker_place_leads where poster in(select poster from linker_planned_places where epoch=$1)",
        &[&epoch],
    )?;
    tx.execute("insert into linker_place_firsts select poster,group_id,article_number from linker_planned_firsts where epoch=$1",&[&epoch])?;
    tx.execute(
        "insert into linker_place_leads select poster,ordinal,part from linker_planned_leads where epoch=$1",
        &[&epoch],
    )?;
    tx.execute("insert into linker_uploads select release_name,group_name,first_posted,last_posted,files,generation,probed,added from linker_planned_uploads where epoch=$1
      on conflict(release_name) do update set group_name=excluded.group_name,first_posted=excluded.first_posted,last_posted=excluded.last_posted,
      files=excluded.files,generation=excluded.generation,probed=excluded.probed,added=excluded.added",&[&epoch])?;
    tx.execute("delete from linker_upload_groups where release_name in(select release_name from linker_planned_uploads where epoch=$1)",&[&epoch])?;
    tx.execute("insert into linker_upload_groups select release_name,group_id from linker_planned_upload_groups where epoch=$1",&[&epoch])?;
    tx.execute(
        "insert into linker_linked_hours select hour from linker_epoch_hours where epoch=$1 on conflict do nothing",
        &[&epoch],
    )?;
    tx.execute("update linker_source_blocks set was_read=true where id in(select source from linker_epoch_sources where epoch=$1)",&[&epoch])?;
    tx.execute("update linker_epochs set phase='done' where id=$1", &[&epoch])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn delayed_file_remains_pending_until_its_whole_chain_is_ready() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let source:i64=c.query_one("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete) values('group.example',1,$1,0,$1,12,12,true) returning id",&[&&[1u8][..]]).unwrap().get(0);
        for n in 1..=12i64 {
            let message = format!("<delayed{n}@example.invalid>");
            let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            let posted = if n <= 6 { 99000i64 } else { 103000 };
            c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines) values($1,$2,$3,0,$4,$5,$6,1,$2,100,1)",&[&source,&n,&&key[..],&message.as_bytes(),&b"delayed@example.invalid".as_slice(),&posted]).unwrap();
        }
        for (hour, expected) in [(27i64, 0i64), (28, 12)] {
            let epoch: i64 = c
                .query_one("insert into linker_epochs(logical_now,phase) values(120000,'files') returning id", &[])
                .unwrap()
                .get(0);
            c.execute("insert into linker_epoch_hours values($1,$2)", &[&epoch, &hour]).unwrap();
            c.execute("insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new) select $1,source,ordinal,ordinal,false,true from linker_staged_rows where source=$2",&[&epoch,&source]).unwrap();
            prepare(&mut c, epoch).unwrap();
            super::super::linker_files::infer(&mut c, epoch).unwrap();
            while !super::super::linker::plan_files(&mut c, epoch, 1).unwrap() {}
            while !chain_one(&mut c, epoch).unwrap() {}
            assert_eq!(
                c.query_one("select count(*) from linker_emission_rows where epoch=$1", &[&epoch])
                    .unwrap()
                    .get::<_, i64>(0),
                expected
            );
            let waiting = c
                .query_one("select waiting_from from linker_epochs where id=$1", &[&epoch])
                .unwrap()
                .get::<_, Option<i64>>(0);
            assert_eq!(waiting, if expected == 0 { Some(99000) } else { None });
            c.execute("update linker_epochs set phase='ack',emitted=planned_rows where id=$1", &[&epoch]).unwrap();
            ack(&mut c, epoch).unwrap();
        }
    }

    #[test]
    fn late_growth_and_fresh_seed_extension_share_one_generation() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let old = b"old@example.invalid".as_slice();
        c.execute(
            "insert into linker_places values($1,'xref-upload-generic','group.example',1,2,3,100000,100000)",
            &[&old],
        )
        .unwrap();
        c.batch_execute(
            "insert into linker_uploads values('xref-upload-generic','group.example',99000,99100,2,7,9,100000);
            insert into linker_upload_groups values('xref-upload-generic',1)",
        )
        .unwrap();
        c.execute("insert into linker_place_firsts values($1,1,100)", &[&old]).unwrap();
        c.execute("insert into linker_place_leads values($1,1,1)", &[&old]).unwrap();
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values(100000,'files') returning id", &[])
            .unwrap()
            .get(0);
        let source:i64=c.query_one("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete) values('group.example',1,$1,0,$1,12,12,true) returning id",&[&&[1u8][..]]).unwrap().get(0);
        c.execute("insert into linker_epoch_hours values($1,27)", &[&epoch]).unwrap();
        for n in 1..=12i64 {
            let message = format!("<late{n}@example.invalid>");
            let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            let poster = if n <= 2 { old } else { b"fresh@example.invalid".as_slice() };
            c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines) values($1,$2,$3,0,$4,$5,99000,1,$2,100,1)",&[&source,&n,&&key[..],&message.as_bytes(),&poster]).unwrap();
            if n <= 2 {
                c.execute("insert into linker_late_rows values($1,$2,$3,$3,1)", &[&epoch, &source, &n]).unwrap();
            } else {
                c.execute("insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new) values($1,$2,$3,$3,false,true)",&[&epoch,&source,&n]).unwrap();
            }
        }
        prepare(&mut c, epoch).unwrap();
        super::super::linker_files::infer(&mut c, epoch).unwrap();
        while !super::super::linker::plan_files(&mut c, epoch, 1).unwrap() {}
        assert!(late_one(&mut c, epoch).unwrap());
        assert!(!late_one(&mut c, epoch).unwrap());
        while !chain_one(&mut c, epoch).unwrap() {}
        c.execute("update linker_epochs set phase='ack',emitted=planned_rows where id=$1", &[&epoch]).unwrap();
        ack(&mut c, epoch).unwrap();
        let r = c
            .query_one(
                "select files,generation,probed from linker_uploads where release_name='xref-upload-generic'",
                &[],
            )
            .unwrap();
        assert_eq!((r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2)), (3, 8, 0));
        assert_eq!(
            c.query_one("select parts from linker_places where poster=$1", &[&old]).unwrap().get::<_, i64>(0),
            5
        );
        assert_eq!(
            c.query_one("select max(part) from linker_place_leads where poster=$1", &[&old]).unwrap().get::<_, i64>(0),
            4
        );
    }

    #[test]
    fn fresh_plan_is_restart_stable_and_ack_requires_all_receipts() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values(100000,'files') returning id", &[])
            .unwrap()
            .get(0);
        let source:i64=c.query_one("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete) values('group.example',1,$1,0,$1,12,12,true) returning id",&[&&[1u8][..]]).unwrap().get(0);
        c.execute("insert into linker_epoch_sources values($1,$2,false,true)", &[&epoch, &source]).unwrap();
        c.execute("insert into linker_epoch_hours values($1,27)", &[&epoch]).unwrap();
        for n in 1..=12i64 {
            let message = format!("<part{n}@example.invalid>");
            let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines) values($1,$2,$3,0,$4,$5,99000,1,$2,100,1)",&[&source,&n,&&key[..],&message.as_bytes(),&b"poster@example.invalid".as_slice()]).unwrap();
            c.execute("insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new) values($1,$2,$3,$3,false,true)",&[&epoch,&source,&n]).unwrap();
        }
        prepare(&mut c, epoch).unwrap();
        prepare(&mut c, epoch).unwrap();
        super::super::linker_files::infer(&mut c, epoch).unwrap();
        while !super::super::linker::plan_files(&mut c, epoch, 1).unwrap() {}
        assert!(!late_one(&mut c, epoch).unwrap());
        assert!(!chain_one(&mut c, epoch).unwrap());
        drop(c);
        let mut c = schema.pool.get().unwrap();
        assert!(chain_one(&mut c, epoch).unwrap());
        assert!(chain_one(&mut c, epoch).unwrap());
        let r = c
            .query_one("select count(*),min(part),max(part) from linker_emission_rows where epoch=$1", &[&epoch])
            .unwrap();
        assert_eq!((r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2)), (12, 1, 12));
        assert!(ack(&mut c, epoch).is_err());
        c.execute("update linker_epochs set phase='ack',emitted=12 where id=$1", &[&epoch]).unwrap();
        ack(&mut c, epoch).unwrap();
        ack(&mut c, epoch).unwrap();
        let r = c.query_one("select parts,file_number,file_total from linker_places", &[]).unwrap();
        assert_eq!((r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2)), (12, 1, 1));
        assert!(
            c.query_one("select was_read from linker_source_blocks where id=$1", &[&source]).unwrap().get::<_, bool>(0)
        );
    }
}
