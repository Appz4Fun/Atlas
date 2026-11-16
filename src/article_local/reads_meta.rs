//! What the web metadata pages ask the local article store.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{Segment, Store};

/// A file's first or last article as the metadata pages order them: an
/// unnumbered part counts as part 0, then message id.
fn edge(last: bool) -> &'static str {
    if last { "coalesce(part, 0) desc, message_id desc" } else { "coalesce(part, 0), message_id" }
}

impl Store {
    /// How many articles a release has, counting no further than `cap`.
    pub(crate) fn release_count(&self, release: i64, cap: i64) -> Result<i64> {
        let conn = self.on(release).lock()?;
        let mut read =
            conn.prepare_cached("select count(*) from (select 1 from segments where release_id = ?1 limit ?2)")?;
        Ok(read.query_row(params![release, cap], |row| row.get(0))?)
    }

    /// The length of the message id `file_edge` would return, so the caller can admit it first.
    pub(crate) fn file_edge_len(&self, release: i64, file: i64, last: bool) -> Result<Option<i64>> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached(&format!(
            "select length(message_id) from segments where file_id = ?2 and release_id = ?1 order by {} limit 1",
            edge(last)
        ))?;
        Ok(read.query_row(params![release, file], |row| row.get(0)).optional()?)
    }

    /// The first or last article of a release's file.
    pub(crate) fn file_edge(&self, release: i64, file: i64, last: bool) -> Result<Option<Segment>> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached(&format!(
            "select release_id, file_id, message_id, part, bytes from segments
             where file_id = ?2 and release_id = ?1 order by {} limit 1",
            edge(last)
        ))?;
        Ok(read
            .query_row(params![release, file], |row| {
                Ok(Segment { release_id: row.get(0)?, file_id: row.get(1)?, message_id: row.get(2)?, part: row.get(3)?, bytes: row.get(4)? })
            })
            .optional()?)
    }

    /// For each inclusive part range, how many distinct part numbers the file has in it.
    pub(crate) fn file_range_counts(&self, file: i64, ranges: &[(i64, i64)]) -> Result<Vec<i64>> {
        let conn = self.home(file)?.lock()?;
        let mut read = conn
            .prepare_cached("select count(distinct part) from segments where file_id = ?1 and part >= ?2 and part <= ?3")?;
        let mut counts = Vec::with_capacity(ranges.len());
        for (at, (lo, hi)) in ranges.iter().enumerate() {
            // Small files repeat one range over many cells.
            let count = match at.checked_sub(1).filter(|before| ranges[*before] == (*lo, *hi)) {
                Some(before) => counts[before],
                None => read.query_row(params![file, lo, hi], |row| row.get(0))?,
            };
            counts.push(count);
        }
        Ok(counts)
    }

    /// The smallest and largest article of a file and how many of its part
    /// numbers (unnumbered counted as 0) came more than once. `None` for a file
    /// without articles.
    pub(crate) fn file_numbers(&self, file: i64) -> Result<Option<(Option<i64>, Option<i64>, i64)>> {
        let conn = self.home(file)?.lock()?;
        let mut read = conn.prepare_cached("select count(*), min(bytes), max(bytes) from segments where file_id = ?1")?;
        let (count, smallest, largest): (i64, Option<i64>, Option<i64>) =
            read.query_row(params![file], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        if count == 0 {
            return Ok(None);
        }
        let mut read = conn.prepare_cached(
            "select count(*) from (select 1 from segments where file_id = ?1 group by coalesce(part, 0) having count(*) > 1)",
        )?;
        Ok(Some((smallest, largest, read.query_row(params![file], |row| row.get(0))?)))
    }

    /// A release's message ids in order, resuming behind `after`. A page ends
    /// after `rows` ids or once it holds `bytes` of them, whichever comes first.
    pub(crate) fn release_ids(&self, release: i64, after: Option<&[u8]>, rows: usize, bytes: usize) -> Result<Vec<Vec<u8>>> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached(
            "select message_id from segments where release_id = ?1 and (?2 is null or message_id > ?2)
             order by message_id limit ?3",
        )?;
        let mut found = read.query(params![release, after, rows as i64])?;
        let (mut ids, mut held) = (Vec::new(), 0usize);
        while let Some(row) = found.next()? {
            let id: Vec<u8> = row.get(0)?;
            held = held.saturating_add(id.len());
            ids.push(id);
            if held >= bytes {
                break;
            }
        }
        Ok(ids)
    }

    /// The numbered articles `lo..=hi` of a release's file by part, then message
    /// id. `after` is the part and message id to resume behind; without a
    /// message id it resumes at the next part.
    pub(crate) fn file_part_ids(
        &self, release: i64, file: i64, lo: i64, hi: i64, after: Option<(i64, Option<&[u8]>)>, limit: usize,
    ) -> Result<Vec<(i64, Vec<u8>)>> {
        let conn = self.on(release).lock()?;
        let (part, id) = after.map_or((None, None), |(part, id)| (Some(part), id));
        let mut read = conn.prepare_cached(
            "select part, message_id from segments
             where file_id = ?2 and release_id = ?1 and part >= ?3 and part <= ?4
               and (?5 is null or part > ?5 or (part = ?5 and ?6 is not null and message_id > ?6))
             order by part, message_id limit ?7",
        )?;
        Ok(read
            .query_map(params![release, file, lo, hi, part, id, limit as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
