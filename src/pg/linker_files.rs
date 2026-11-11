//! Checkpointed external poster/run inference. Each input row performs a fixed
//! number of indexed reads/mutations; even a single giant file is resumable.
use anyhow::{Result, ensure};
use postgres::Client;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Cursor {
    ordered: bool,
    copied: i64,
    after: Option<(i64, i64, i64)>,
    file: i64,
    run: i64,
    part_position: i64,
}

/// A transaction touches at most `budget` input rows plus fixed reducer
/// bookkeeping. Selection is sealed; file output stays hidden until completion.
pub(crate) fn infer_step(client: &mut Client, epoch: i64, budget: usize) -> Result<bool> {
    ensure!(budget > 0, "file reducer budget must be positive");
    let mut tx = super::linker::durable_transaction(client)?;
    let phase: String = tx.query_one("select phase from linker_epochs where id=$1 for update", &[&epoch])?.get(0);
    if phase != "files" {
        ensure!(
            ["chain", "plan", "emit", "ack", "done"].contains(&phase.as_str()),
            "file inference requires selection"
        );
        return Ok(true);
    }
    let saved = tx.query_opt("select state from linker_work where epoch=$1 and kind='files'", &[&epoch])?;
    let mut state: Cursor =
        saved.map(|r| serde_json::from_str(&r.get::<_, String>(0))).transpose()?.unwrap_or_default();
    let mut done = false;
    for _ in 0..budget {
        if !state.ordered {
            let row = tx.query_opt("select r.source_rank,r.source,r.source_ordinal,r.poster_rank,r.again,r.is_new,s.poster,s.posted,s.group_id,s.article_number,s.message_id
                from linker_selected_rows r join linker_staged_rows s on s.source=r.source and s.ordinal=r.source_ordinal
                where r.epoch=$1 and r.source_rank>$2 order by r.source_rank limit 1", &[&epoch, &state.copied])?;
            if let Some(row) = row {
                let rank: i64 = row.get(0);
                let mut poster_rank: i64 = row.get(3);
                if poster_rank == 0 {
                    let poster: Vec<u8> = row.get(6);
                    tx.execute(
                        "insert into linker_file_posters values($1,$2,$3) on conflict do nothing",
                        &[&epoch, &poster, &rank],
                    )?;
                    poster_rank = tx
                        .query_one(
                            "select first_rank from linker_file_posters where epoch=$1 and poster_key=sha256($2) and poster=$2",
                            &[&epoch, &poster],
                        )?
                        .get(0);
                }
                tx.execute(
                    "insert into linker_file_input values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
                    &[
                        &epoch,
                        &poster_rank,
                        &row.get::<_, i64>(7),
                        &rank,
                        &row.get::<_, i64>(1),
                        &row.get::<_, i64>(2),
                        &row.get::<_, Vec<u8>>(6),
                        &row.get::<_, bool>(4),
                        &row.get::<_, bool>(5),
                        &row.get::<_, i64>(8),
                        &row.get::<_, i64>(9),
                        &row.get::<_, Vec<u8>>(10),
                    ],
                )?;
                state.copied = rank;
            } else {
                state.ordered = true;
            }
            continue;
        }
        let after = state.after.unwrap_or((i64::MIN, i64::MIN, i64::MIN));
        let row = tx.query_opt(
            "select poster_rank,posted,source_rank,source,source_ordinal,poster,again,is_new,group_id,article_number,message_id
            from linker_file_input where epoch=$1 and (poster_rank,posted,source_rank)>($2,$3,$4)
            order by poster_rank,posted,source_rank limit 1",
            &[&epoch, &after.0, &after.1, &after.2],
        )?;
        let Some(row) = row else {
            tx.execute("update linker_epochs set phase='chain' where id=$1", &[&epoch])?;
            done = true;
            break;
        };
        let poster_rank: i64 = row.get(0);
        let posted: i64 = row.get(1);
        let rank: i64 = row.get(2);
        let again: bool = row.get(6);
        let is_new: bool = row.get(7);
        let changed_poster = state.after.is_none_or(|a| a.0 != poster_rank);
        let split = !changed_poster && again && posted.saturating_sub(after.1) > crate::nxv_store::linker::JOIN_GAP;
        if changed_poster || split {
            if state.after.is_some() {
                state.file += 1;
            }
            state.run = if changed_poster { 0 } else { state.run + 1 };
            tx.execute("insert into linker_files(epoch,id,poster,run_no,first_rank,new_material,again,articles,first_posted,last_posted)
                values($1,$2,$3,$4,$5,$6,$7,1,$8,$8)",
                &[&epoch,&state.file,&row.get::<_,Vec<u8>>(5),&state.run,&poster_rank,&is_new,&again,&posted])?;
        } else {
            tx.execute(
                "update linker_files set articles=articles+1,last_posted=$3,new_material=new_material or $4
                where epoch=$1 and id=$2",
                &[&epoch, &state.file, &posted, &is_new],
            )?;
        }
        tx.execute(
            "insert into linker_file_rows values($1,$2,$3,$4,$5)",
            &[&epoch, &state.file, &row.get::<_, i64>(3), &row.get::<_, i64>(4), &rank],
        )?;
        tx.execute(
            "insert into linker_file_groups values($1,$2,$3) on conflict do nothing",
            &[&epoch, &state.file, &row.get::<_, i64>(8)],
        )?;
        state.part_position += 1;
        tx.execute(
            "insert into linker_sort_rows values($1,0,0,$9,$2,$3,$4,$5,$6,$7,$8)",
            &[
                &epoch,
                &state.file,
                &row.get::<_, i64>(8),
                &row.get::<_, i64>(9),
                &row.get::<_, Vec<u8>>(10),
                &row.get::<_, i64>(3),
                &row.get::<_, i64>(4),
                &posted,
                &state.part_position,
            ],
        )?;
        state.after = Some((poster_rank, posted, rank));
    }
    tx.execute(
        "insert into linker_work values($1,'files',$2) on conflict(epoch,kind) do update set state=excluded.state",
        &[&epoch, &serde_json::to_string(&state)?],
    )?;
    tx.commit()?;
    Ok(done)
}

#[cfg(test)]
pub(crate) fn infer(client: &mut Client, epoch: i64) -> Result<()> {
    while !infer_step(client, epoch, 2)? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn giant_file_commits_bounded_reducer_progress_and_resumes() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values(100000,'files') returning id", &[])
            .unwrap()
            .get(0);
        let source: i64 = c
            .query_one(
                "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete)
            values('group.example',1,$1,0,$1,137,137,true) returning id",
                &[&&[1u8][..]],
            )
            .unwrap()
            .get(0);
        c.execute("insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines)
            select $1,n,decode(md5(n::text),'hex'),0,convert_to(n::text,'UTF8'),convert_to('poster@example.invalid','UTF8'),n,1,n,100,1 from generate_series(1,137) n", &[&source]).unwrap();
        c.execute(
            "insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new,poster_rank)
            select $1,$2,n,n,false,true,1 from generate_series(1,137) n",
            &[&epoch, &source],
        )
        .unwrap();
        c.batch_execute("create table reducer_audit(tx bigint primary key, mutations bigint not null);
            create function audit_reducer() returns trigger language plpgsql as $$ begin
              insert into reducer_audit values(txid_current(),1) on conflict(tx) do update set mutations=reducer_audit.mutations+1;
              return null; end $$").unwrap();
        for table in [
            "linker_file_input",
            "linker_sort_rows",
            "linker_files",
            "linker_file_rows",
            "linker_file_groups",
            "linker_work",
            "linker_epochs",
        ] {
            c.batch_execute(&format!("create trigger audit after insert or update or delete on {table} for each row execute function audit_reducer()" )).unwrap();
        }
        for _ in 0..41 {
            assert!(!infer_step(&mut c, epoch, 1).unwrap());
        }
        let checkpoint: String =
            c.query_one("select state from linker_work where epoch=$1 and kind='files'", &[&epoch]).unwrap().get(0);
        assert_eq!(serde_json::from_str::<Cursor>(&checkpoint).unwrap().copied, 41);
        drop(c);
        let mut c = schema.pool.get().unwrap();
        assert!(!infer_step(&mut c, epoch, 1).unwrap());
        let checkpoint: String =
            c.query_one("select state from linker_work where epoch=$1 and kind='files'", &[&epoch]).unwrap().get(0);
        assert_eq!(serde_json::from_str::<Cursor>(&checkpoint).unwrap().copied, 42);
        while !infer_step(&mut c, epoch, 1).unwrap() {}
        assert_eq!(
            c.query_one("select articles from linker_files where epoch=$1", &[&epoch]).unwrap().get::<_, i64>(0),
            137
        );
        assert!(c.query_one("select max(mutations) from reducer_audit", &[]).unwrap().get::<_, i64>(0) <= 5);
        assert_eq!(
            c.query_one("select count(*) from linker_file_rows where epoch=$1", &[&epoch]).unwrap().get::<_, i64>(0),
            137
        );
    }

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
                "insert into linker_selected_rows(epoch,source,source_ordinal,source_rank,again,is_new,poster_rank) values($1,$2,$3,$3,$4,true,case when $4 then 2 else 1 end)",
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
