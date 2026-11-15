//! What post-processing steps ask the local article store.
//!
//! A file belongs to one release, so `release_id` is the same on every row of
//! a file. Naming it in an `order by` between `part` and `message_id` changes
//! no order and lets the `(file_id, part)` index, which ends in the primary
//! key, hand the rows out already sorted.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Store;

/// One article of a file as a step reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StepSegment {
    pub message_id: Vec<u8>,
    pub part: Option<i64>,
    pub bytes: i64,
}

/// Collects rows while their message ids stay within `max_id_bytes` together.
/// `None` once they do not: the caller refuses the read as PostgreSQL's length
/// check did, and no more than one id past the limit was ever copied.
fn admitted(
    rows: &mut rusqlite::Rows<'_>,
    out: &mut Vec<StepSegment>,
    id_bytes: &mut i64,
    max_id_bytes: i64,
) -> Result<bool> {
    while let Some(row) = rows.next()? {
        let length = row.get_ref(0)?.as_blob()?.len() as i64;
        *id_bytes = id_bytes.saturating_add(length);
        if *id_bytes > max_id_bytes {
            return Ok(false);
        }
        out.push(StepSegment { message_id: row.get(0)?, part: row.get(1)?, bytes: row.get(2)? });
    }
    Ok(true)
}

impl Store {
    /// The first `limit` articles of a file by part (unnumbered first) then
    /// message id, or with `last` the last `limit` of that order. Either way
    /// they come back ascending. `None` when their ids exceed `max_id_bytes`.
    pub(crate) fn file_ends(
        &self,
        file: i64,
        last: bool,
        limit: usize,
        max_id_bytes: i64,
    ) -> Result<Option<Vec<StepSegment>>> {
        let conn = self.lock()?;
        // SQLite's own NULL placement (first ascending, last descending) is
        // the one PostgreSQL was asked for here, so the index order serves both.
        let mut read = conn.prepare_cached(if last {
            "select message_id, part, bytes from segments where file_id = ?1
             order by part desc nulls last, release_id desc, message_id desc limit ?2"
        } else {
            "select message_id, part, bytes from segments where file_id = ?1
             order by part asc nulls first, release_id, message_id limit ?2"
        })?;
        let (mut out, mut id_bytes) = (Vec::new(), 0);
        if !admitted(&mut read.query(params![file, limit as i64])?, &mut out, &mut id_bytes, max_id_bytes)? {
            return Ok(None);
        }
        if last {
            out.reverse();
        }
        Ok(Some(out))
    }

    /// Up to `per_part` articles, lowest message ids first, for each of the
    /// named parts of a file, ordered by part then message id. `None` when
    /// their ids exceed `max_id_bytes`.
    pub(crate) fn file_part_copies(
        &self,
        file: i64,
        parts: &[i64],
        per_part: usize,
        max_id_bytes: i64,
    ) -> Result<Option<Vec<StepSegment>>> {
        let mut parts = parts.to_vec();
        parts.sort_unstable();
        parts.dedup();
        let conn = self.lock()?;
        let mut read = conn.prepare_cached(
            "select message_id, part, bytes from segments where file_id = ?1 and part = ?2
             order by release_id, message_id limit ?3",
        )?;
        let (mut out, mut id_bytes) = (Vec::new(), 0);
        for part in parts {
            if !admitted(&mut read.query(params![file, part, per_part as i64])?, &mut out, &mut id_bytes, max_id_bytes)? {
                return Ok(None);
            }
        }
        Ok(Some(out))
    }

    /// The message ids of up to `per_part` articles, lowest first, of each part
    /// `from..=to` of a file, ordered by part then message id.
    pub(crate) fn file_part_copies_capped(&self, file: i64, from: i64, to: i64, per_part: usize) -> Result<Vec<(i64, Vec<u8>)>> {
        let conn = self.lock()?;
        let mut read = conn.prepare_cached(
            "select part, message_id from segments where file_id = ?1 and part between ?2 and ?3
             order by part, release_id, message_id",
        )?;
        let mut rows = read.query(params![file, from, to])?;
        let mut out = Vec::new();
        let (mut current, mut taken) = (None, 0);
        while let Some(row) = rows.next()? {
            let part: i64 = row.get(0)?;
            if current != Some(part) {
                (current, taken) = (Some(part), 0);
            }
            // Further copies of a part are passed over without copying their ids.
            if taken < per_part {
                taken += 1;
                out.push((part, row.get(1)?));
            }
        }
        Ok(out)
    }

    /// The summed `bytes` of each file's articles, 0 for a file with none, in
    /// the order the files were named.
    pub(crate) fn files_bytes(&self, files: &[i64]) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        let mut read = conn.prepare_cached("select coalesce(sum(bytes), 0) from segments where file_id = ?1")?;
        files.iter().map(|file| Ok(read.query_row([file], |row| row.get(0))?)).collect()
    }

    /// The first non-empty message id of a release's file by part (unnumbered
    /// first) then message id: its length, and the id itself unless it is
    /// longer than `max_len`.
    pub(crate) fn first_message_id(&self, file: i64, release: i64, max_len: usize) -> Result<Option<(usize, Option<Vec<u8>>)>> {
        let conn = self.lock()?;
        let mut read = conn.prepare_cached(
            "select message_id from segments where file_id = ?1 and release_id = ?2 and length(message_id) > 0
             order by part asc nulls first, release_id, message_id limit 1",
        )?;
        Ok(read
            .query_row(params![file, release], |row| {
                let length = row.get_ref(0)?.as_blob()?.len();
                Ok((length, if length <= max_len { Some(row.get(0)?) } else { None }))
            })
            .optional()?)
    }

    /// The distinct part numbers `0..=max_part` a file has above `after`,
    /// ascending: what PostgreSQL's `file_parts` held for the file.
    pub(crate) fn file_parts_page(&self, file: i64, after: Option<i64>, max_part: i64, limit: usize) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        let mut read = conn.prepare_cached(
            "select distinct part from segments where file_id = ?1 and part between 0 and ?2 and (?3 is null or part > ?3)
             order by part limit ?4",
        )?;
        Ok(read.query_map(params![file, max_part, after, limit as i64], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
