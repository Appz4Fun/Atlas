//! Publication of immutable plans in bounded pages. A save and its receipt are
//! separate commits: on an ambiguous writer commit the same plan is replayed.
//! Article identity, parts, subjects and logical time never get recomputed from
//! changed source blocks or upload seeds during that replay.

use anyhow::{Context, Result, ensure};
use postgres::Client;

use crate::indexer::{Db, on_native_db};
use crate::parser::Release;
use crate::xref::{Place, Row};

use super::linker_stage::Budget;

struct Page {
    before: i64,
    logical_now: i64,
    through: i64,
    releases: Vec<Release>,
    finished: bool,
}

fn page(client: &mut Client, epoch: i64, budget: Budget) -> Result<Page> {
    ensure!(budget.rows > 0 && budget.bytes > 0, "native linker emission budget must be positive");
    let state = client.query_one("select phase,emitted,logical_now from linker_epochs where id=$1", &[&epoch])?;
    let phase: String = state.get(0);
    let before: i64 = state.get(1);
    let logical_now: i64 = state.get(2);
    if phase == "ack" || phase == "done" {
        return Ok(Page { before, logical_now, through: before, releases: Vec::new(), finished: true });
    }
    ensure!(phase == "emit", "native linker emission requires a sealed plan");
    let mut through = before;
    let mut releases = Vec::new();
    let mut bytes = 0usize;
    let mut finished = false;
    while releases.len() < budget.rows && bytes < budget.bytes {
        let next=client.query_opt(
            "select e.ordinal,e.part,f.release_name,f.group_name,f.release_poster,f.release_date,f.poster,
                    f.file_number,f.file_total,f.part_total,s.message_id,s.posted,s.group_id,s.article_number,s.bytes,s.lines
             from linker_emission_rows e join linker_emission_files f on f.epoch=e.epoch and f.id=e.file_id
             join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
             where e.epoch=$1 and e.ordinal>$2 order by e.ordinal limit 1",&[&epoch,&through],
        )?;
        let Some(next) = next else {
            finished = true;
            break;
        };
        through = next.get(0);
        let part: i64 = next.get(1);
        let release: String = next.get(2);
        let group: String = next.get(3);
        let release_poster = String::from_utf8(next.get(4)).context("invalid native linker release poster")?;
        let release_date: String = next.get(5);
        let poster = String::from_utf8(next.get(6)).context("invalid native linker file poster")?;
        let file: i64 = next.get(7);
        let files: i64 = next.get(8);
        let parts: i64 = next.get(9);
        let message = String::from_utf8(next.get(10)).context("invalid canonical native linker message ID")?;
        let place = Place { release, group, file, files, parts: part - 1, leads: String::new(), oldest: 0 };
        let row = Row {
            message_id: message,
            added: 0,
            lead: false,
            posted: next.get(11),
            grp: next.get(12),
            number: next.get(13),
            bytes: next.get(14),
            lines: next.get(15),
        };
        // Reuse the identity/subject formatting contract on ONE article. The
        // persisted part offset makes this independent of page boundaries.
        let mut out = std::collections::HashMap::new();
        crate::xref::add_file(&mut out, &place, &poster, vec![row], parts);
        let mut release = out.into_values().next().context("empty native linker emission")?;
        release.poster = release_poster;
        release.date = release_date;
        let article = &release.articles[0];
        bytes = bytes
            .saturating_add(512)
            .saturating_add(release.name.len())
            .saturating_add(release.group.len())
            .saturating_add(release.poster.len())
            .saturating_add(article.author.len())
            .saturating_add(article.subject.len())
            .saturating_add(article.message_id.len())
            .saturating_add(article.filename.as_ref().map_or(0, String::len));
        releases.push(release);
    }
    Ok(Page { before, logical_now, through, releases, finished })
}

fn receipt(client: &mut Client, epoch: i64, before: i64, through: i64, finished: bool) -> Result<()> {
    let mut tx = super::linker::durable_transaction(client)?;
    let state =
        tx.query_one("select emitted,logical_now,phase from linker_epochs where id=$1 for update", &[&epoch])?;
    let current: i64 = state.get(0);
    let now: i64 = state.get(1);
    if current >= through && current != before {
        return Ok(());
    }
    ensure!(current == before, "native linker emission receipt cursor changed");
    let phase: String = state.get(2);
    ensure!(phase == "emit", "native linker receipt requires an emitting epoch");
    let conflict:bool=tx.query_one("select exists(
        select 1 from linker_emission_rows e join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
        join linker_linked_ids i on i.message_key=s.message_key
        where e.epoch=$1 and e.ordinal>$2 and e.ordinal<=$3 and i.message_id<>s.message_id)",
        &[&epoch,&before,&through])?.get(0);
    let page_conflict:bool=tx.query_one("select exists(
        select s.message_key from linker_emission_rows e
        join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
        where e.epoch=$1 and e.ordinal>$2 and e.ordinal<=$3 group by s.message_key having count(distinct s.message_id)>1)",
        &[&epoch,&before,&through])?.get(0);
    ensure!(!conflict && !page_conflict, "native linker message identity digest collision");
    tx.execute(
        "insert into linker_linked_ids(message_key,message_id,added)
        select distinct on(s.message_key) s.message_key,s.message_id,$4 from linker_emission_rows e
        join linker_staged_rows s on s.source=e.source and s.ordinal=e.source_ordinal
        where e.epoch=$1 and e.ordinal>$2 and e.ordinal<=$3 order by s.message_key,e.ordinal
        on conflict(message_key) do update set added=greatest(linker_linked_ids.added,excluded.added)",
        &[&epoch, &before, &through, &now],
    )?;
    tx.execute(
        "update linker_staged_rows s set published=true from linker_emission_rows e
        where e.epoch=$1 and e.ordinal>$2 and e.ordinal<=$3 and s.source=e.source and s.ordinal=e.source_ordinal",
        &[&epoch, &before, &through],
    )?;
    tx.execute(
        "update linker_epochs set emitted=$2,phase=case when $3 then 'ack' else 'emit' end where id=$1",
        &[&epoch, &through, &finished],
    )?;
    tx.commit()?;
    Ok(())
}

/// Return true once the entire immutable plan has durable save receipts. Link
/// metadata/hour acknowledgements are a later phase; scanned source coverage
/// must never be confused with successful publication.
pub(crate) async fn emit_page(db: &Db, epoch: i64, budget: Budget) -> Result<bool> {
    let page = on_native_db(db, move |c| page(c, epoch, budget)).await?;
    if page.finished && page.releases.is_empty() && page.before == page.through {
        // Still record an empty plan's transition from emit to ack, unless the
        // earlier receipt already made that transition.
        let phase: String = on_native_db(db, move |c| {
            Ok(c.query_one("select phase from linker_epochs where id=$1", &[&epoch])?.get(0))
        })
        .await?;
        if phase == "ack" || phase == "done" {
            return Ok(true);
        }
    }
    let Page { before, logical_now, through, releases, finished } = page;
    if !releases.is_empty() {
        db.save_native_at(releases, logical_now).await?;
    }
    on_native_db(db, move |c| receipt(c, epoch, before, through, finished)).await?;
    Ok(finished)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn immutable_emission_replays_committed_save_before_receipt_without_renumbering() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let now = 1_800_000_000i64;
        let epoch: i64 = c
            .query_one("insert into linker_epochs(logical_now,phase) values($1,'emit') returning id", &[&now])
            .unwrap()
            .get(0);
        let source: i64 = c
            .query_one(
                "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete)
            values('alt.binaries.example',1,$1,0,$1,3,3,true) returning id",
                &[&&[1u8][..]],
            )
            .unwrap()
            .get(0);
        let release = crate::xref::upload_name("poster@example.invalid");
        let date = crate::xref::iso_pub(now);
        c.execute(
            "insert into linker_emission_files values($1,1,$2,'alt.binaries.example',$3,$4,$3,1,1,3)",
            &[&epoch, &release, &b"poster@example.invalid".as_slice(), &date],
        )
        .unwrap();
        for n in 1..=3i64 {
            let message = format!("<part{n}@example.invalid>");
            let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
            c.execute(
                "insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines) values($1,$2,$3,0,$4,$5,$6,1,$2,100,1)",
                &[&source, &n, &&key[..], &message.as_bytes(), &b"poster@example.invalid".as_slice(), &now],
            )
            .unwrap();
            c.execute("insert into linker_emission_rows values($1,$2,1,$3,$2,$2)", &[&epoch, &n, &source]).unwrap();
        }
        let budget = Budget { rows: 1, bytes: 1024 };
        let first = page(&mut c, epoch, budget).unwrap();
        assert_eq!(first.releases[0].articles[0].part, Some(1));
        assert_eq!(first.releases[0].articles[0].total_parts, Some(3));
        crate::pg::release_writer::save(&mut c, &first.releases, now).unwrap();
        let generation: i64 = c.query_one("select generation from pp_queue limit 1", &[]).unwrap().get(0);
        // Crash boundary: writer committed, durable cursor still points before
        // that page. Re-read the SAME sealed plan, then save and acknowledge it.
        let replay = page(&mut c, epoch, budget).unwrap();
        assert_eq!(replay.before, 0);
        assert_eq!(replay.releases[0].articles[0].subject, first.releases[0].articles[0].subject);
        crate::pg::release_writer::save(&mut c, &replay.releases, now).unwrap();
        let replay_generation: i64 = c.query_one("select generation from pp_queue limit 1", &[]).unwrap().get(0);
        assert_eq!(generation, replay_generation);
        receipt(&mut c, epoch, replay.before, replay.through, replay.finished).unwrap();
        loop {
            let p = page(&mut c, epoch, budget).unwrap();
            if !p.releases.is_empty() {
                crate::pg::release_writer::save(&mut c, &p.releases, now).unwrap();
            }
            receipt(&mut c, epoch, p.before, p.through, p.finished).unwrap();
            if p.finished {
                break;
            }
        }
        let count: i64 = c.query_one("select count(*) from segments", &[]).unwrap().get(0);
        assert_eq!(count, 3);
        let state = c.query_one("select emitted,phase from linker_epochs where id=$1", &[&epoch]).unwrap();
        assert_eq!(state.get::<_, i64>(0), 3);
        assert_eq!(state.get::<_, String>(1), "ack");
        let remembered: i64 = c.query_one("select count(*) from linker_linked_ids", &[]).unwrap().get(0);
        assert_eq!(remembered, 3);
        assert!(page(&mut c, epoch, budget).unwrap().finished);
    }
}

#[cfg(test)]
pub(super) fn test_page(client: &mut Client, epoch: i64, budget: Budget) -> Result<Vec<Release>> {
    Ok(page(client, epoch, budget)?.releases)
}
