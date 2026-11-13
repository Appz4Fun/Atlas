//! Frozen release/part plans and metadata effects. PostgreSQL externally orders
//! every file's parts and retains the plans through publication receipts. No
//! Article tree or complete membership list is retained by the planner.

use anyhow::{Result, ensure};
use postgres::Client;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Prepare {
    stage: u8,
    upload: Option<(i64, String)>,
    seeds: i64,
    seed: Option<i64>,
    group: Option<i64>,
    rank: i64,
    copied_seed: i64,
    late_position: i64,
    copied_group: Option<(i64, i64)>,
}

pub(crate) fn prepare_step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "seed reducer budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let row =
        tx.query_one("select phase,logical_now,seeds_prepared from linker_epochs where id=$1 for update", &[&epoch])?;
    if row.get::<_, bool>(2) {
        return Ok(true);
    }
    ensure!(row.get::<_, String>(0) == "files", "seed preparation requires selection");
    let now: i64 = row.get(1);
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='prepare'", &[&epoch])?;
    let mut s: Prepare = saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let mut done = false;
    for _ in 0..budget {
        match s.stage {
            0 => {
                let after = s.upload.clone().unwrap_or((i64::MIN, String::new()));
                let next = tx.query_opt(
                    "select release_name,group_name,first_posted,last_posted,files,generation,added
                    from linker_uploads where (first_posted,release_name collate \"C\")>($1,$2 collate \"C\")
                    order by first_posted,release_name collate \"C\" limit 1",
                    &[&after.0, &after.1],
                )?;
                if let Some(row) = next {
                    let release: String = row.get(0);
                    let first: i64 = row.get(2);
                    s.upload = Some((first, release.clone()));
                    if row.get::<_, i64>(6) >= now.saturating_sub(crate::xref::KEEP_SECS) {
                        tx.execute("insert into linker_epoch_seeds(epoch,id,release_name,group_name,first_posted,last_posted,files,generation)
                            values($1,$2,$3,$4,$5,$6,$7,$8)", &[&epoch,&s.seeds,&release,&row.get::<_,String>(1),&first,
                                &row.get::<_,i64>(3),&row.get::<_,i64>(4),&row.get::<_,i64>(5)])?;
                        s.seeds += 1;
                    }
                } else {
                    s.stage = 1;
                    s.seed = Some(0);
                }
            }
            1 => {
                let seed = s.seed.unwrap();
                if seed >= s.seeds {
                    s.stage = 2;
                    continue;
                }
                let release: String = tx
                    .query_one(
                        "select release_name from linker_epoch_seeds where epoch=$1 and id=$2",
                        &[&epoch, &seed],
                    )?
                    .get(0);
                let next=match s.group {
                    Some(group)=>tx.query_opt("select group_id from linker_upload_groups where release_name=$1 and group_id>$2 order by group_id limit 1", &[&release,&group])?,
                    None=>tx.query_opt("select group_id from linker_upload_groups where release_name=$1 order by group_id limit 1", &[&release])?,
                };
                if let Some(row) = next {
                    let group: i64 = row.get(0);
                    s.group = Some(group);
                    tx.execute("insert into linker_seed_groups values($1,$2,$3)", &[&epoch, &seed, &group])?;
                } else {
                    s.seed = Some(seed + 1);
                    s.group = None;
                }
            }
            2 => {
                let next=tx.query_opt("select r.source_rank,r.poster_rank,r.source,r.source_ordinal,s.poster,s.posted,s.group_id,s.article_number,s.message_id,p.release_name
                    from linker_late_rows r join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
                    join linker_places p on p.poster_key=sha256(s.poster) and p.poster=s.poster where r.epoch=$1 and r.source_rank>$2 order by r.source_rank limit 1", &[&epoch,&s.rank])?;
                let Some(row) = next else {
                    s.stage = 3;
                    continue;
                };
                s.rank = row.get(0);
                let poster: Vec<u8> = row.get(4);
                let posted: i64 = row.get(5);
                let group: i64 = row.get(6);
                tx.execute("insert into linker_late_files(epoch,poster,encounter_rank,articles) values($1,$2,$3,1)
                    on conflict(epoch,poster_key) do update set articles=linker_late_files.articles+1,encounter_rank=least(linker_late_files.encounter_rank,excluded.encounter_rank)",
                    &[&epoch,&poster,&row.get::<_,i64>(1)])?;
                s.late_position += 1;
                tx.execute(
                    "insert into linker_sort_rows values($1,1,0,$2,$3,$4,$5,$6,$7,$8,$9)",
                    &[
                        &epoch,
                        &s.late_position,
                        &row.get::<_, i64>(1),
                        &group,
                        &row.get::<_, i64>(7),
                        &row.get::<_, Vec<u8>>(8),
                        &row.get::<_, i64>(2),
                        &row.get::<_, i64>(3),
                        &posted,
                    ],
                )?;
                if let Some(seed)=tx.query_opt("update linker_epoch_seeds set first_posted=least(first_posted,$3),last_posted=greatest(last_posted,$3),grown=true
                    where epoch=$1 and release_name=$2 returning id", &[&epoch,&row.get::<_,String>(9),&posted])? {
                    tx.execute("insert into linker_seed_groups values($1,$2,$3) on conflict do nothing", &[&epoch,&seed.get::<_,i64>(0),&group])?;
                }
            }
            3 => {
                if s.copied_seed >= s.seeds {
                    s.stage = 4;
                    continue;
                }
                let row=tx.query_one("select release_name,group_name,first_posted,last_posted,files,generation,grown from linker_epoch_seeds where epoch=$1 and id=$2", &[&epoch,&s.copied_seed])?;
                tx.execute("insert into linker_chains(epoch,id,seed,first_posted,last_posted,index_only) values($1,$2,$2,$3,$4,false)",
                    &[&epoch,&s.copied_seed,&row.get::<_,i64>(2),&row.get::<_,i64>(3)])?;
                if row.get::<_, bool>(6) {
                    tx.execute(
                        "insert into linker_planned_uploads values($1,$2,$3,$4,$5,$6,$7,0,$8)",
                        &[
                            &epoch,
                            &row.get::<_, String>(0),
                            &row.get::<_, String>(1),
                            &row.get::<_, i64>(2),
                            &row.get::<_, i64>(3),
                            &row.get::<_, i64>(4),
                            &(row.get::<_, i64>(5) + 1),
                            &now,
                        ],
                    )?;
                }
                s.copied_seed += 1;
            }
            4 => {
                let next=match s.copied_group {
                    Some((seed,group))=>tx.query_opt("select seed,group_id from linker_seed_groups where epoch=$1 and (seed,group_id)>($2,$3) order by seed,group_id limit 1", &[&epoch,&seed,&group])?,
                    None=>tx.query_opt("select seed,group_id from linker_seed_groups where epoch=$1 order by seed,group_id limit 1", &[&epoch])?,
                };
                if let Some(row) = next {
                    let seed: i64 = row.get(0);
                    let group: i64 = row.get(1);
                    s.copied_group = Some((seed, group));
                    tx.execute(
                        "insert into linker_chain_groups values($1,$2,0,$3),($1,$2,2,$3)",
                        &[&epoch, &seed, &group],
                    )?;
                    let row = tx.query_one(
                        "select release_name,grown from linker_epoch_seeds where epoch=$1 and id=$2",
                        &[&epoch, &seed],
                    )?;
                    if row.get::<_, bool>(1) {
                        tx.execute(
                            "insert into linker_planned_upload_groups values($1,$2,$3)",
                            &[&epoch, &row.get::<_, String>(0), &group],
                        )?;
                    }
                } else {
                    s.stage = 5;
                }
            }
            5 => {
                tx.execute("update linker_epochs set seeds_prepared=true,next_chain=$2,
                    waiting_from=case when exists(select 1 from linker_epoch_hours where epoch=$1) then null else waiting_from end where id=$1", &[&epoch,&s.seeds])?;
                done = true;
                break;
            }
            _ => unreachable!(),
        }
    }
    tx.execute(
        "insert into linker_work values($1,'prepare',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&s)?],
    )?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
pub(crate) fn prepare(client: &mut Client, epoch: i64) -> Result<()> {
    while !prepare_step(client, epoch, 2)? {}
    Ok(())
}

#[cfg(test)]
pub(crate) fn late_one(client: &mut Client, epoch: i64) -> Result<bool> {
    let next = client.query_opt(
        "select poster from linker_late_files where epoch=$1 and not planned order by encounter_rank limit 1",
        &[&epoch],
    )?;
    let Some(row) = next else { return Ok(false) };
    let poster: Vec<u8> = row.get(0);
    loop {
        super::linker_output::step(client, epoch, 2)?;
        if client
            .query_one(
                "select planned from linker_late_files where epoch=$1 and poster_key=sha256($2) and poster=$2",
                &[&epoch, &poster],
            )?
            .get::<_, bool>(0)
        {
            return Ok(true);
        }
    }
}

#[cfg(test)]
pub(crate) fn chain_one(client: &mut Client, epoch: i64) -> Result<bool> {
    let next = client.query_opt(
        "select id from linker_chains where epoch=$1 and file_count>0 and not planned order by id limit 1",
        &[&epoch],
    )?;
    let Some(row) = next else {
        while !super::linker_output::step(client, epoch, 2)? {}
        return Ok(true);
    };
    let chain: i64 = row.get(0);
    loop {
        super::linker_output::step(client, epoch, 2)?;
        if client
            .query_one("select planned from linker_chains where epoch=$1 and id=$2", &[&epoch, &chain])?
            .get::<_, bool>(0)
        {
            return Ok(false);
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Ack {
    stage: u8,
    poster: Option<Vec<u8>>,
    after_poster: Option<Vec<u8>>,
    release: Option<String>,
    after_release: Option<String>,
    member: Option<i64>,
    hour: Option<i64>,
    source: i64,
}

/// Apply at most `budget` metadata effects plus one durable cursor. Intermediate
/// replacements are hidden by phase=ack: the single fenced driver cannot open
/// another epoch, expire seeds or enable archive traversal until the final flip.
pub(crate) fn ack_step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "ack budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let row = tx.query_one("select phase,emitted,planned_rows from linker_epochs where id=$1 for update", &[&epoch])?;
    if row.get::<_, String>(0) == "done" {
        return Ok(true);
    }
    ensure!(
        row.get::<_, String>(0) == "ack" && row.get::<_, i64>(1) == row.get::<_, i64>(2),
        "native acknowledgement requires every publication receipt"
    );
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='ack'", &[&epoch])?;
    let mut s: Ack = saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let mut done = false;
    for _ in 0..budget {
        match s.stage {
            0 => {
                let next = match &s.after_poster {
                    Some(poster) => tx.query_opt(
                        "select poster from linker_planned_places where epoch=$1 and poster_key>sha256($2) order by poster_key limit 1",
                        &[&epoch, poster],
                    )?,
                    None => tx.query_opt(
                        "select poster from linker_planned_places where epoch=$1 order by poster_key limit 1",
                        &[&epoch],
                    )?,
                };
                if let Some(row) = next {
                    s.poster = Some(row.get(0));
                    s.stage = 1;
                } else {
                    s.stage = 6;
                }
            }
            1 | 2 => {
                let poster = s.poster.as_ref().unwrap();
                let (table, key) =
                    if s.stage == 1 { ("linker_place_firsts", "group_id") } else { ("linker_place_leads", "ordinal") };
                let n=tx.execute(&format!("delete from {table} where poster_key=sha256($1) and poster=$1 and {key}=(select {key} from {table} where poster_key=sha256($1) and poster=$1 order by {key} limit 1)"), &[poster])?;
                if n == 0 {
                    s.stage += 1;
                }
            }
            3 => {
                tx.execute("insert into linker_places select poster,release_name,group_name,file_number,file_total,parts,oldest,added
                    from linker_planned_places where epoch=$1 and poster_key=sha256($2) and poster=$2 on conflict(poster_key) do update set
                    release_name=excluded.release_name,group_name=excluded.group_name,file_number=excluded.file_number,
                    file_total=excluded.file_total,parts=excluded.parts,oldest=excluded.oldest,added=excluded.added",
                    &[&epoch,s.poster.as_ref().unwrap()])?;
                s.stage = 4;
                s.member = None;
            }
            4 | 5 => {
                let poster = s.poster.as_ref().unwrap();
                let (planned, live, key, value) = if s.stage == 4 {
                    ("linker_planned_firsts", "linker_place_firsts", "group_id", "article_number")
                } else {
                    ("linker_planned_leads", "linker_place_leads", "ordinal", "part")
                };
                let base = format!(
                    "select {key},{value} from {planned} where epoch=$1 and poster_key=sha256($2) and poster=$2"
                );
                let next = match s.member {
                    Some(member) => tx.query_opt(
                        &format!("{base} and {key}>$3 order by {key} limit 1"),
                        &[&epoch, poster, &member],
                    )?,
                    None => tx.query_opt(&format!("{base} order by {key} limit 1"), &[&epoch, poster])?,
                };
                if let Some(row) = next {
                    let key: i64 = row.get(0);
                    s.member = Some(key);
                    tx.execute(
                        &format!("insert into {live} values($1,$2,$3)"),
                        &[poster, &key, &row.get::<_, i64>(1)],
                    )?;
                } else if s.stage == 4 {
                    s.stage = 5;
                    s.member = None;
                } else {
                    s.after_poster = s.poster.take();
                    s.stage = 0;
                }
            }
            6 => {
                let next=match &s.after_release {
                    Some(release)=>tx.query_opt("select release_name from linker_planned_uploads where epoch=$1 and release_name>$2 order by release_name limit 1", &[&epoch,release])?,
                    None=>tx.query_opt("select release_name from linker_planned_uploads where epoch=$1 order by release_name limit 1", &[&epoch])?,
                };
                if let Some(row) = next {
                    s.release = Some(row.get(0));
                    s.stage = 7;
                } else {
                    s.stage = 10;
                }
            }
            7 => {
                let n=tx.execute("delete from linker_upload_groups where release_name=$1 and group_id=(select group_id from linker_upload_groups where release_name=$1 order by group_id limit 1)", &[s.release.as_ref().unwrap()])?;
                if n == 0 {
                    s.stage = 8;
                }
            }
            8 => {
                tx.execute("insert into linker_uploads select release_name,group_name,first_posted,last_posted,files,generation,probed,added
                    from linker_planned_uploads where epoch=$1 and release_name=$2 on conflict(release_name) do update set
                    group_name=excluded.group_name,first_posted=excluded.first_posted,last_posted=excluded.last_posted,
                    files=excluded.files,generation=excluded.generation,
                    probed=case when linker_uploads.generation=excluded.generation then greatest(linker_uploads.probed,excluded.probed) else excluded.probed end,
                    added=excluded.added", &[&epoch,s.release.as_ref().unwrap()])?;
                s.stage = 9;
                s.member = None;
            }
            9 => {
                let release = s.release.as_ref().unwrap();
                let next=match s.member {
                    Some(group)=>tx.query_opt("select group_id from linker_planned_upload_groups where epoch=$1 and release_name=$2 and group_id>$3 order by group_id limit 1", &[&epoch,release,&group])?,
                    None=>tx.query_opt("select group_id from linker_planned_upload_groups where epoch=$1 and release_name=$2 order by group_id limit 1", &[&epoch,release])?,
                };
                if let Some(row) = next {
                    let group: i64 = row.get(0);
                    s.member = Some(group);
                    tx.execute("insert into linker_upload_groups values($1,$2)", &[release, &group])?;
                } else {
                    s.after_release = s.release.take();
                    s.stage = 6;
                }
            }
            10 => {
                let next = match s.hour {
                    Some(hour) => tx.query_opt(
                        "select hour from linker_epoch_hours where epoch=$1 and hour>$2 order by hour limit 1",
                        &[&epoch, &hour],
                    )?,
                    None => tx.query_opt(
                        "select hour from linker_epoch_hours where epoch=$1 order by hour limit 1",
                        &[&epoch],
                    )?,
                };
                if let Some(row) = next {
                    let hour: i64 = row.get(0);
                    s.hour = Some(hour);
                    tx.execute("insert into linker_linked_hours values($1) on conflict do nothing", &[&hour])?;
                } else {
                    s.stage = 11;
                }
            }
            11 => {
                let next = tx.query_opt(
                    "select source from linker_epoch_sources where epoch=$1 and source>$2 order by source limit 1",
                    &[&epoch, &s.source],
                )?;
                if let Some(row) = next {
                    s.source = row.get(0);
                    tx.execute("update linker_source_blocks set was_read=true where id=$1", &[&s.source])?;
                } else {
                    tx.execute("update linker_epochs set phase='done' where id=$1", &[&epoch])?;
                    done = true;
                    break;
                }
            }
            _ => unreachable!(),
        }
    }
    tx.execute(
        "insert into linker_work values($1,'ack',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&s)?],
    )?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
pub(crate) fn ack(client: &mut Client, epoch: i64) -> Result<()> {
    while !ack_step(client, epoch, 2)? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn native_metadata_promotion_keeps_newer_tried_counts_and_resets_on_growth() {
        let Some(schema)=crate::pg::testing::Schema::new() else {return};
        let runtime=crate::pg::stage_queue_tests::runtime(&schema);
        runtime.owner.with(|client| {
            let epoch:i64=client.query_one("insert into linker_epochs(logical_now,phase) values(1,'ack') returning id",&[])?.get(0);
            client.batch_execute("insert into linker_uploads values('xref-upload-fixture','example.group',0,1,1,7,9,1)")?;
            client.execute("insert into linker_planned_uploads select $1,release_name,group_name,first_posted,last_posted,files,generation,2,added from linker_uploads",&[&epoch])?;
            let cursor=serde_json::to_string(&Ack {stage:8,release:Some("xref-upload-fixture".into()),..Default::default()})?;
            client.execute("insert into linker_work(epoch,kind,state) values($1,'ack',$2)",&[&epoch,&cursor])?;
            assert!(!ack_step(client,epoch,1)?);
            let row=client.query_one("select generation,probed from linker_uploads",&[])?;
            assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)),(7,9));
            client.execute("update linker_planned_uploads set generation=8,probed=0,files=2 where epoch=$1",&[&epoch])?;
            client.execute("update linker_work set state=$2 where epoch=$1 and kind='ack'",&[&epoch,&cursor])?;
            assert!(!ack_step(client,epoch,1)?);
            let row=client.query_one("select generation,probed from linker_uploads",&[])?;
            assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)),(8,0));Ok(())
        }).unwrap();runtime.owner.close();
    }

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
        c.batch_execute("create function fail_fresh_plan() returns trigger language plpgsql as $$ begin
            if new.poster=convert_to('fresh@example.invalid','UTF8') then raise exception 'synthetic plan storage exhaustion' using errcode='53100'; end if; return new; end $$;
            create trigger fail_fresh_plan before insert or update on linker_planned_places for each row execute function fail_fresh_plan()").unwrap();
        assert!(chain_one(&mut c, epoch).is_err());
        assert_eq!(
            c.query_one("select count(*) from linker_emission_rows where epoch=$1", &[&epoch])
                .unwrap()
                .get::<_, i64>(0),
            2
        );
        c.batch_execute("drop trigger fail_fresh_plan on linker_planned_places").unwrap();
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
            c.query_one("select parts from linker_places where poster_key=sha256($1) and poster=$1", &[&old])
                .unwrap()
                .get::<_, i64>(0),
            5
        );
        assert_eq!(
            c.query_one("select max(part) from linker_place_leads where poster_key=sha256($1) and poster=$1", &[&old])
                .unwrap()
                .get::<_, i64>(0),
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
        c.batch_execute("create function fail_ack() returns trigger language plpgsql as $$ begin raise exception 'synthetic acknowledgement storage exhaustion' using errcode='53100'; end $$;
            create trigger fail_ack before insert or update on linker_uploads for each row execute function fail_ack()").unwrap();
        assert!(ack(&mut c, epoch).is_err());
        // Earlier bounded metadata pages stay committed. The failed upload
        // page and its cursor roll back; phase=ack fences all future epochs.
        assert_eq!(c.query_one("select count(*) from linker_places", &[]).unwrap().get::<_, i64>(0), 1);
        let checkpoint: String =
            c.query_one("select state from linker_work where epoch=$1 and kind='ack'", &[&epoch]).unwrap().get(0);
        assert!(matches!(serde_json::from_str::<Ack>(&checkpoint).unwrap().stage, 7 | 8));
        assert_eq!(
            c.query_one("select phase from linker_epochs where id=$1", &[&epoch]).unwrap().get::<_, String>(0),
            "ack"
        );
        c.batch_execute("drop trigger fail_ack on linker_uploads").unwrap();
        ack(&mut c, epoch).unwrap();
        ack(&mut c, epoch).unwrap();
        let r = c.query_one("select parts,file_number,file_total from linker_places", &[]).unwrap();
        assert_eq!((r.get::<_, i64>(0), r.get::<_, i64>(1), r.get::<_, i64>(2)), (12, 1, 1));
        assert!(
            c.query_one("select was_read from linker_source_blocks where id=$1", &[&source]).unwrap().get::<_, bool>(0)
        );
    }
}
