//! Recoverable bounded staging of one immutable NXV block. The original block
//! must stay pinned until complete. Each transaction persists both its retained
//! rows and its raw-row cursor; filtered rows count toward scan progress too.

use anyhow::{Context, Result, ensure};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::FieldCursor;
use postgres::Client;
use sha2::{Digest, Sha256};

use crate::nxv_store::segment::{BlockMap, Entry, Snapshot};

#[derive(Clone, Copy)]
pub(crate) struct Budget {
    pub rows: usize,
    pub bytes: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self { rows: 256, bytes: 1024 * 1024 }
    }
}

pub(crate) struct Progress {
    pub source: i64,
    pub scanned: i64,
    pub complete: bool,
}

/// A verified immutable block prefix. Retaining one mapping and digest avoids
/// rehashing the entire block at each database page boundary. Recreated files
/// and a different manifest entry cannot reuse this verification.
pub(crate) struct VerifiedBlock {
    pub(crate) identity: [u8; 32],
    pub(crate) entry: Entry,
    map: BlockMap,
    digest: [u8; 32],
}
impl VerifiedBlock {
    pub(crate) fn new(snapshot: &Snapshot, entry: Entry) -> Result<Self> {
        ensure!((entry.seq as usize) < snapshot.count, "block is outside native linker snapshot");
        let map = snapshot.map_block(entry)?;
        let bytes = &map[..];
        let digest = Sha256::digest(bytes).into();
        let view = NxvBlockView::from_mmap_slice(bytes).context("reading native linker source block")?;
        ensure!(view.verify_body(), "native linker source block checksum failed");
        Ok(Self { identity: snapshot.identity, entry, map, digest })
    }
}

#[cfg(test)]
pub(crate) fn stage_block(
    client: &mut Client,
    snapshot: &Snapshot,
    group: &str,
    day: i64,
    entry: &Entry,
    budget: Budget,
) -> Result<Progress> {
    stage_verified(client, &VerifiedBlock::new(snapshot, *entry)?, group, day, budget)
}

/// Stage one bounded page from a verified immutable block. A row checkpoint
/// limits numeric prefix decoding independently of the durable row offset.
pub(crate) fn stage_verified(
    client: &mut Client,
    block: &VerifiedBlock,
    group: &str,
    day: i64,
    budget: Budget,
) -> Result<Progress> {
    ensure!(budget.rows > 0 && budget.bytes > 0, "native linker staging budget must be positive");
    let entry = &block.entry;
    let view = NxvBlockView::from_mmap_slice(&block.map).context("reading verified native source block")?;
    let mut tx = super::linker::durable_transaction(client)?;
    let source = tx.query_one(
        "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,manifest)
         values($1,$2,$3,$4,$5,$6,$7) on conflict(group_name,day,generation,seq,digest)
         do update set rows=linker_source_blocks.rows,manifest=excluded.manifest returning id,scanned,complete,rows",
        &[
            &group,
            &day,
            &&block.identity[..],
            &(entry.seq as i64),
            &&block.digest[..],
            &(entry.rows as i64),
            &&entry.to_bytes()[..],
        ],
    )?;
    let id: i64 = source.get(0);
    let before: i64 = source.get(1);
    let complete: bool = source.get(2);
    ensure!(source.get::<_, i64>(3) == entry.rows as i64, "native linker source row count changed");
    if complete {
        tx.commit()?;
        return Ok(Progress { source: id, scanned: before, complete });
    }
    // Resume from the format checkpoint: at most 128 numeric rows, with no
    // committed-prefix strings reconstructed on normal pages or restart.
    let mut cursor = FieldCursor::new_at_row(view, usize::try_from(before)?)
        .context("opening native linker source cursor at durable offset")?;
    let mut scanned = before;
    let mut retained_bytes = 0usize;
    let mut read = 0usize;
    let mut ended = false;
    while read < budget.rows && retained_bytes < budget.bytes {
        let Some(r) = cursor.next_row() else {
            ended = true;
            break;
        };
        scanned += 1;
        read += 1;
        let parsed;
        let (author, msgid, posted, bytes, lines, number);
        if let Some(line) = r.literal {
            let Some(overview) = crate::nntp::overview_of(line) else { continue };
            parsed = overview.into_article();
            if !crate::xref::keeps(&parsed) {
                continue;
            }
            let Some(wall) = crate::dates::naive_timestamp(&parsed.date) else { continue };
            (author, msgid, posted, bytes, lines, number) = (
                parsed.author.as_bytes(),
                parsed.message_id.as_bytes(),
                wall,
                parsed.bytes,
                parsed.lines,
                parsed.number,
            );
        } else {
            if !crate::xref::keeps_fields(r.subject, r.author, r.msgid) {
                continue;
            }
            let (b, l) = (r.bytes.unwrap_or(0), r.lines.unwrap_or(0));
            (author, msgid, posted, bytes, lines, number) =
                (r.author, r.msgid, r.wall, if b > 0 { b } else { crate::nntp::bytes_from_lines(l) }, l, r.art);
        }
        let message = String::from_utf8_lossy(msgid);
        let poster = if author.is_ascii() {
            String::from_utf8_lossy(author).into_owned()
        } else {
            crate::nntp::undo_mojibake(String::from_utf8_lossy(author).into_owned())
        };
        let key: [u8; 32] = Sha256::digest(message.as_bytes()).into();
        // Reinterpret u64 bits, rather than narrowing its identity. Historical
        // hash-only records use the same representation in PostgreSQL bigint.
        let legacy_hash = nntp_xover_engine::msgid_index::msgid_hash(message.as_bytes()) as i64;
        tx.execute(
            "insert into linker_staged_rows(source,ordinal,message_key,legacy_hash,message_id,poster,posted,group_id,article_number,bytes,lines,published) values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,exists(select 1 from linker_linked_ids where message_key=$3 and message_id=$5))",
            &[
                &id,
                &scanned,
                &&key[..],
                &legacy_hash,
                &message.as_bytes(),
                &poster.as_bytes(),
                &posted,
                &crate::nxv_store::linker::group_id(group),
                &(number as i64),
                &bytes,
                &lines,
            ],
        )?;
        retained_bytes = retained_bytes.saturating_add(message.len()).saturating_add(poster.len()).saturating_add(128);
    }
    ensure!(scanned <= entry.rows as i64, "native linker source exceeds manifest row count");
    if scanned == entry.rows as i64 {
        ensure!(cursor.next_row().is_none(), "native linker source exceeds manifest row count");
        ended = true;
    }
    ensure!(!ended || scanned == entry.rows as i64, "native linker source ended before manifest row count");
    tx.execute("update linker_source_blocks set scanned=$2,complete=$3 where id=$1", &[&id, &scanned, &ended])?;
    tx.commit()?;
    Ok(Progress { source: id, scanned, complete: ended })
}

/// A durable contiguous prefix avoids COUNT/DISTINCT over an arbitrary day.
/// Normal sequential staging advances it with two fixed-key mutations.
pub(crate) fn record_coverage(
    tx: &mut postgres::Transaction<'_>,
    group: &str,
    day: i64,
    identity: [u8; 32],
    seq: i64,
) -> Result<()> {
    tx.execute(
        "insert into linker_coverage values($1,$2,$3,0) on conflict do nothing",
        &[&group, &day, &&identity[..]],
    )?;
    tx.execute(
        "update linker_coverage set through=$4+1 where group_name=$1 and day=$2 and generation=$3 and through=$4",
        &[&group, &day, &&identity[..], &seq],
    )?;
    Ok(())
}

/// Missing/out-of-order coverage repairs at most 32 sequence keys per callback.
/// Until that cursor catches up, expiry fails closed and the next sweep resumes.
pub(crate) fn covered(client: &mut Client, group: &str, day: i64, identity: [u8; 32], count: usize) -> Result<bool> {
    let count = i64::try_from(count)?;
    let prior = client.query_opt(
        "select through from linker_coverage where group_name=$1 and day=$2 and generation=$3",
        &[&group, &day, &&identity[..]],
    )?;
    let mut through = prior.map_or(0, |row| row.get::<_, i64>(0));
    if through >= count {
        return Ok(true);
    }
    let mut tx = super::linker::durable_transaction(client)?;
    for _ in 0..32 {
        if through >= count {
            break;
        }
        let complete:bool=tx.query_one("select exists(select 1 from linker_source_blocks where group_name=$1 and day=$2 and generation=$3 and seq=$4 and complete)", &[&group,&day,&&identity[..],&through])?.get(0);
        if !complete {
            break;
        }
        through += 1;
    }
    tx.execute("insert into linker_coverage values($1,$2,$3,$4) on conflict(group_name,day,generation) do update set through=greatest(linker_coverage.through,excluded.through)", &[&group,&day,&&identity[..],&through])?;
    tx.commit()?;
    Ok(through >= count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nxv_store::segment::{ENTRY_LEN, Segment, parse_day_name};
    use nntp_xover_engine::container::{BlockMeta, NxvBlockBuilder};
    use std::os::unix::fs::FileExt;

    #[test]
    fn coverage_repairs_only_a_fixed_prefix_and_keeps_gaps_and_new_generations() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut c = schema.pool.get().unwrap();
        let identity = [7u8; 32];
        c.execute("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete) select 'group.example',1,$1,n,decode(md5(n::text),'hex'),1,1,true from generate_series(0,999) n where n<>70", &[&&identity[..]]).unwrap();
        c.batch_execute("set synchronous_commit=off").unwrap();
        assert!(!covered(&mut c, "group.example", 1, identity, 1000).unwrap());
        assert_eq!(c.query_one("select through from linker_coverage", &[]).unwrap().get::<_, i64>(0), 32);
        drop(c);
        let mut c = schema.pool.get().unwrap();
        assert!(!covered(&mut c, "group.example", 1, identity, 1000).unwrap());
        assert_eq!(c.query_one("select through from linker_coverage", &[]).unwrap().get::<_, i64>(0), 64);
        assert!(!covered(&mut c, "group.example", 1, identity, 1000).unwrap());
        assert_eq!(c.query_one("select through from linker_coverage", &[]).unwrap().get::<_, i64>(0), 70);
        assert!(!covered(&mut c, "group.example", 1, [8; 32], 1000).unwrap());
        c.execute("insert into linker_source_blocks(group_name,day,generation,seq,digest,rows,scanned,complete) values('group.example',1,$1,70,$1,1,1,true)", &[&&identity[..]]).unwrap();
        let mut previous = 70;
        loop {
            let done = covered(&mut c, "group.example", 1, identity, 1000).unwrap();
            let through: i64 = c
                .query_one("select through from linker_coverage where generation=$1", &[&&identity[..]])
                .unwrap()
                .get(0);
            assert!(through > previous && through - previous <= 32);
            previous = through;
            if done {
                break;
            }
        }
        assert_eq!(previous, 1000);
        assert!(!covered(&mut c, "group.example", 1, identity, 1001).unwrap());
        assert!(covered(&mut c, "group.example", 1, identity, 1000).unwrap());
        c.batch_execute("analyze linker_source_blocks").unwrap();
        let plans=c.query("explain (analyze,format text) select exists(select 1 from linker_source_blocks where group_name=$1 and day=$2 and generation=$3 and seq=$4 and complete)", &[&"group.example",&1i64,&&identity[..],&998i64]).unwrap();
        let text = plans.iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Index"), "{text}");
        assert!(!text.contains("Seq Scan"), "{text}");
    }

    #[test]
    fn staged_block_resumes_small_pages_without_dropping_filtered_rows() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let day = parse_day_name("2026-11-08").unwrap();
        let (seg, _) = Segment::open(dir.path(), day).unwrap();
        let rows: Vec<_> = (1..=5).map(|n| {
            let id=format!("{:032x}",n);
            let subject=if n==3 {"[1/1] - \"file.part01.rar\" yEnc (1/3)".to_string()}else{id.clone()};
            format!("{n}\t{subject}\tposter@example.invalid\tSun, 08 Nov 2026 10:00:00 +0000\t<{id}@example.invalid>\t\t700000\t5000").into_bytes()
        }).collect();
        let refs: Vec<&[u8]> = rows.iter().map(Vec::as_slice).collect();
        let bytes = NxvBlockBuilder::build_hot_block_meta(&refs, BlockMeta { server_id: 1, slab_gen: 1 }).unwrap();
        let reserved = seg.reserve(&bytes).unwrap().unwrap();
        reserved.arena.write_all_at(&bytes, reserved.entry.offset).unwrap();
        reserved.index.write_all_at(&reserved.entry.to_bytes(), reserved.entry.seq as u64 * ENTRY_LEN as u64).unwrap();
        seg.publish(&reserved).unwrap();
        let snap = seg.bounded_snapshot().unwrap();
        let budget = Budget { rows: 2, bytes: 1024 };
        let mut client = schema.pool.get().unwrap();
        let first = stage_block(&mut client, &snap, "group.example", day, &reserved.entry, budget).unwrap();
        assert_eq!(first.scanned, 2);
        assert!(!first.complete);
        // Inject the database's disk-full SQLSTATE at the next page boundary.
        // The same connection is usable after rollback, and its cursor has not
        // acknowledged rows whose insert could not be made durable.
        client
            .batch_execute(
                "create function fail_linker_stage() returns trigger language plpgsql as $$
            begin raise exception 'synthetic storage exhaustion' using errcode='53100'; end $$;
            create trigger fail_linker_stage before insert on linker_staged_rows
            for each row execute function fail_linker_stage()",
            )
            .unwrap();
        assert!(stage_block(&mut client, &snap, "group.example", day, &reserved.entry, budget).is_err());
        let scanned: i64 =
            client.query_one("select scanned from linker_source_blocks where id=$1", &[&first.source]).unwrap().get(0);
        assert_eq!(scanned, 2);
        client.batch_execute("drop trigger fail_linker_stage on linker_staged_rows").unwrap();
        drop(client);
        let mut client = schema.pool.get().unwrap();
        let second = stage_block(&mut client, &snap, "group.example", day, &reserved.entry, budget).unwrap();
        assert_eq!(second.scanned, 4);
        let last = stage_block(&mut client, &snap, "group.example", day, &reserved.entry, budget).unwrap();
        assert!(last.complete);
        assert_eq!(last.scanned, 5);
        assert_eq!(last.source, first.source);
        assert!(stage_block(&mut client, &snap, "group.example", day, &reserved.entry, budget).unwrap().complete);
        let count: i64 = client
            .query_one("select count(*) from linker_staged_rows where source=$1", &[&first.source])
            .unwrap()
            .get(0);
        assert_eq!(count, 4);
    }
}
