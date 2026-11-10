//! Durable storage for sealed NXV blocks. The source identifies a store generation. A group and
//! day's sequence numbers must remain stable within that generation.

use anyhow::{Context, Result, ensure};
use postgres::Client;
use postgres::binary_copy::BinaryCopyInWriter;
use postgres::types::Type;

use crate::nxv_store::segment::{ENTRY_LEN, Entry, entry_for};

/// A complete sealed block with its manifest entry.
#[derive(Clone, Copy)]
pub struct Block<'a> {
    group: &'a str,
    day: i64,
    entry: Entry,
    bytes: &'a [u8],
}

impl<'a> Block<'a> {
    pub fn new(group: &'a str, day: i64, entry: Entry, bytes: &'a [u8]) -> Result<Self> {
        ensure!(!group.is_empty() && !group.contains('\0'), "invalid header block group");
        ensure!(day.checked_mul(86_400).and_then(|start| start.checked_add(86_399)).is_some(), "invalid header day");
        ensure!(entry.len as usize == bytes.len(), "header block length differs from the manifest");
        ensure!(
            entry.rows > 0 && entry_for(bytes, entry.offset, entry.seq, day) == Some(entry),
            "invalid header block or manifest"
        );
        Ok(Self { group, day, entry, bytes })
    }
}

/// Commit a batch atomically with synchronous commit. Replaying a committed block is safe.
/// Reusing a key with different bytes fails the entire batch.
pub fn append(conn: &mut Client, source: &str, blocks: &[Block<'_>]) -> Result<u64> {
    ensure!(!source.is_empty() && !source.contains('\0'), "invalid header block source");
    if blocks.is_empty() {
        return Ok(0);
    }
    let mut tx = conn.transaction()?;
    tx.batch_execute(
        "set local synchronous_commit = on;
         create temporary table if not exists atlas_header_batch (like header_blocks) on commit delete rows;",
    )?;
    {
        let sink = tx.copy_in(
            "copy pg_temp.atlas_header_batch (source, group_name, day, sequence, header_count, manifest, payload)
             from stdin binary",
        )?;
        let types = [Type::TEXT, Type::TEXT, Type::INT8, Type::INT8, Type::INT8, Type::BYTEA, Type::BYTEA];
        let mut writer = BinaryCopyInWriter::new(sink, &types);
        for block in blocks {
            writer.write(&[
                &source,
                &block.group,
                &block.day,
                &i64::from(block.entry.seq),
                &i64::from(block.entry.rows),
                &block.entry.to_bytes().as_slice(),
                &block.bytes,
            ])?;
        }
        writer.finish()?;
    }
    let inserted = tx.execute(
        "insert into header_blocks (source, group_name, day, sequence, header_count, manifest, payload)
         select source, group_name, day, sequence, header_count, manifest, payload
         from pg_temp.atlas_header_batch order by source, group_name, day, sequence
         on conflict (source, group_name, day, sequence) do nothing",
        &[],
    )?;
    if inserted != blocks.len() as u64 {
        let conflict = tx.query_one(
            "select exists (
               select 1 from pg_temp.atlas_header_batch b join header_blocks h using (source, group_name, day, sequence)
               where b.manifest <> h.manifest or b.payload <> h.payload
             )",
            &[],
        )?;
        ensure!(!conflict.get::<_, bool>(0), "a header block key already holds different data");
    }
    tx.commit()?;
    Ok(inserted)
}

/// Read up to `limit` blocks of a day from the given inclusive sequence, in sequence order.
pub fn read_page(
    conn: &mut Client,
    source: &str,
    group: &str,
    day: i64,
    first_sequence: u32,
    limit: u32,
) -> Result<Vec<(Entry, Vec<u8>)>> {
    conn.query(
        "select manifest, payload from header_blocks
         where source = $1 and group_name = $2 and day = $3 and sequence >= $4 order by sequence limit $5",
        &[&source, &group, &day, &i64::from(first_sequence), &i64::from(limit)],
    )?
    .into_iter()
    .map(|row| {
        let manifest: &[u8] = row.get(0);
        ensure!(manifest.len() == ENTRY_LEN, "invalid stored manifest");
        let entry = Entry::from_bytes(manifest);
        let bytes: Vec<u8> = row.get(1);
        Block::new(group, day, entry, &bytes).context("checking the stored header block")?;
        Ok((entry, bytes))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nntp_xover_engine::container::NxvBlockBuilder;

    const DAY: i64 = 20_766;
    const GROUP: &str = "alt.binaries.test";

    fn fixture() -> (Entry, Vec<u8>) {
        let line = b"1\tExample\tPoster\tMon, 09 Nov 2026 12:00:00 +0000\t<a@example.test>\t\t1024\t8";
        let bytes = NxvBlockBuilder::build_hot_block(&[line.as_slice()]).unwrap();
        let entry = entry_for(&bytes, 0, 3, DAY).unwrap();
        (entry, bytes)
    }

    #[test]
    fn a_manifest_must_describe_its_block() {
        let (entry, bytes) = fixture();
        assert!(Block::new(GROUP, DAY, entry, &bytes).is_ok());
        assert!(Block::new(GROUP, DAY, Entry { rows: entry.rows + 1, ..entry }, &bytes).is_err());
        assert!(Block::new(GROUP, DAY, entry, &bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn committed_blocks_can_be_retried_and_read_back() {
        let Some(schema) = crate::pg::testing::Schema::new() else { return };
        let mut conn = schema.pool.get().unwrap();
        let (entry, bytes) = fixture();
        let block = Block::new(GROUP, DAY, entry, &bytes).unwrap();
        assert_eq!(append(&mut conn, "test-source", &[block]).unwrap(), 1);
        assert_eq!(append(&mut conn, "test-source", &[block, block]).unwrap(), 0);
        let blocks = read_page(&mut conn, "test-source", GROUP, DAY, 0, 10).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, entry);
        assert_eq!(blocks[0].1, bytes);

        let incompatible_entry = Entry { offset: 64, ..entry };
        let incompatible = Block::new(GROUP, DAY, incompatible_entry, &bytes).unwrap();
        let new = Block::new(GROUP, DAY, Entry { seq: 4, ..entry }, &bytes).unwrap();
        assert!(append(&mut conn, "test-source", &[new, incompatible]).is_err());
        assert_eq!(read_page(&mut conn, "test-source", GROUP, DAY, 0, 10).unwrap().len(), 1);
    }
}
