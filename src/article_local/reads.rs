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
        let conn = self.lock()?;
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

    /// How many articles a file has, counting no further than `cap`.
    pub(crate) fn file_count(&self, file: i64, cap: i64) -> Result<i64> {
        Ok(self.file_counts(&[(file, cap)])?[0])
    }

    /// `file_count` of each `(file, cap)`, on one read connection.
    pub(crate) fn file_counts(&self, files: &[(i64, i64)]) -> Result<Vec<i64>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        self.read(|conn| {
            let mut read =
                conn.prepare_cached("select count(*) from (select 1 from segments where file_id = ?1 limit ?2)")?;
            files.iter().map(|&(file, cap)| Ok(read.query_row(params![file, cap], |row| row.get(0))?)).collect()
        })
    }

    pub(crate) fn get(&self, release: i64, message_id: &[u8]) -> Result<Option<Segment>> {
        let conn = self.lock()?;
        let mut read =
            conn.prepare_cached(&format!("select {COLUMNS} from segments where release_id = ?1 and message_id = ?2"))?;
        Ok(read.query_row(params![release, message_id], segment).optional()?)
    }
}
