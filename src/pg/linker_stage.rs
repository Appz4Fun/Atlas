//! Recoverable bounded staging of one immutable NXV block. The original block
//! must stay pinned until complete. Each transaction persists both its retained
//! rows and its raw-row cursor; filtered rows count toward scan progress too.

use anyhow::{Context, Result, ensure};
use nntp_xover_engine::container::NxvBlockView;
use nntp_xover_engine::streamer::FieldCursor;
use postgres::Client;
use sha2::{Digest, Sha256};

use crate::nxv_store::segment::{Entry, Snapshot};

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

/// Stage one bounded page, preserving the rank of every original row. A source
/// recreation is a different identity, and an identical prefix is safely read
/// again: message deduplication happens later against immutable epoch ranking.
/// Appends beyond the snapshot's count belong to a subsequent snapshot.
pub(crate) fn stage_block(
    client: &mut Client,
    snapshot: &Snapshot,
    group: &str,
    day: i64,
    entry: &Entry,
    budget: Budget,
) -> Result<Progress> {
    ensure!(budget.rows > 0 && budget.bytes > 0, "native linker staging budget must be positive");
    ensure!((entry.seq as usize) < snapshot.count, "block is outside native linker snapshot");
    let map = snapshot.map.as_ref().context("native linker snapshot has no mapping")?;
    let start = usize::try_from(entry.offset)?;
    let end = start.checked_add(entry.len as usize).context("native linker block extent overflow")?;
    let bytes = map.get(start..end).context("native linker source block is truncated")?;
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let view = NxvBlockView::from_mmap_slice(bytes).context("reading native linker source block")?;
    ensure!(view.verify_body(), "native linker source block checksum failed");
    let mut cursor = FieldCursor::new(view).context("opening native linker source cursor")?;
    let mut tx = client.transaction()?;
    let source = tx.query_one(
        "insert into linker_source_blocks(group_name,day,generation,seq,digest,rows)
         values($1,$2,$3,$4,$5,$6) on conflict(group_name,day,generation,seq,digest)
         do update set rows=linker_source_blocks.rows returning id,scanned,complete,rows",
        &[&group, &day, &&snapshot.identity[..], &(entry.seq as i64), &&digest[..], &(entry.rows as i64)],
    )?;
    let id: i64 = source.get(0);
    let before: i64 = source.get(1);
    let complete: bool = source.get(2);
    ensure!(source.get::<_, i64>(3) == entry.rows as i64, "native linker source row count changed");
    if complete {
        tx.commit()?;
        return Ok(Progress { source: id, scanned: before, complete });
    }
    // FieldCursor has no seek primitive. Re-reading the prefix after restart
    // costs CPU, but retains no prefix rows and never repeats a committed page.
    for _ in 0..before {
        ensure!(cursor.next_row().is_some(), "native linker source ended before staged cursor");
    }
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
            "insert into linker_staged_rows values($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nxv_store::segment::{ENTRY_LEN, Segment, parse_day_name};
    use nntp_xover_engine::container::{BlockMeta, NxvBlockBuilder};
    use std::os::unix::fs::FileExt;

    #[test]
    fn staged_block_resumes_small_pages_without_dropping_filtered_rows() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let dir = tempfile::tempdir().unwrap();
        let day = parse_day_name("2026-11-08").unwrap();
        let (seg, _) = Segment::open(dir.path(), day).unwrap();
        let rows: Vec<_> = (1..=5).map(|n| {
            let id=format!("{:032x}",n);
            format!("{n}\t{id}\tposter@example.invalid\tSun, 08 Nov 2026 10:00:00 +0000\t<{id}@example.invalid>\t\t700000\t5000").into_bytes()
        }).collect();
        let refs: Vec<&[u8]> = rows.iter().map(Vec::as_slice).collect();
        let bytes = NxvBlockBuilder::build_hot_block_meta(&refs, BlockMeta { server_id: 1, slab_gen: 1 }).unwrap();
        let reserved = seg.reserve(&bytes).unwrap().unwrap();
        reserved.arena.write_all_at(&bytes, reserved.entry.offset).unwrap();
        reserved.index.write_all_at(&reserved.entry.to_bytes(), reserved.entry.seq as u64 * ENTRY_LEN as u64).unwrap();
        seg.publish(&reserved).unwrap();
        let snap = seg.bounded_snapshot();
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
        assert_eq!(count, 5);
    }
}
