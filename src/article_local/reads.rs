//! What readers ask the local article store.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{Envelope, Store};

/// One stored article.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub release_id: i64,
    pub file_id: i64,
    pub message_id: Vec<u8>,
    pub part: Option<i64>,
    pub bytes: i64,
}

fn segment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment { release_id: row.get(0)?, file_id: row.get(1)?, message_id: row.get(2)?, part: row.get(3)?, bytes: row.get(4)? })
}

const COLUMNS: &str = "release_id, file_id, message_id, part, bytes";

impl Store {
    pub(crate) fn envelope(&self, release: i64, message_id: &[u8]) -> Result<Option<Envelope>> {
        let conn = self.on(release).lock()?;
        Ok(conn
            .query_row(
                "select subject, poster, posted_date, nzb_timestamp, expected, subject_file_index, subject_file_total
                 from envelopes where release_id = ?1 and message_id = ?2",
                params![release, message_id],
                |row| {
                    Ok(Envelope {
                        subject: row.get(0)?,
                        poster: row.get(1)?,
                        posted_date: row.get(2)?,
                        nzb_timestamp: row.get(3)?,
                        expected: row.get(4)?,
                        subject_file_index: row.get(5)?,
                        subject_file_total: row.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    /// How many articles each `(release, file, cap)` names has, counting no further than `cap`, on one read
    /// connection of each file of the store the releases are in.
    pub(crate) fn file_counts(&self, files: &[(i64, i64, i64)]) -> Result<Vec<i64>> {
        let mut counts = vec![0; files.len()];
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); self.shards().len()];
        for (i, &(release, _, _)) in files.iter().enumerate() {
            groups[self.index_of(release)].push(i);
        }
        for (shard, group) in self.shards().iter().zip(&groups) {
            if group.is_empty() {
                continue;
            }
            let found: Vec<i64> = shard.read(|conn| {
                let mut read =
                    conn.prepare_cached("select count(*) from (select 1 from segments where file_id = ?1 limit ?2)")?;
                group.iter().map(|&i| Ok(read.query_row(params![files[i].1, files[i].2], |row| row.get(0))?)).collect()
            })?;
            for (&i, count) in group.iter().zip(found) {
                counts[i] = count;
            }
        }
        Ok(counts)
    }

    pub(crate) fn get(&self, release: i64, message_id: &[u8]) -> Result<Option<Segment>> {
        let conn = self.on(release).lock()?;
        let mut read =
            conn.prepare_cached(&format!("select {COLUMNS} from segments where release_id = ?1 and message_id = ?2"))?;
        Ok(read.query_row(params![release, message_id], segment).optional()?)
    }
}
