//! What NZB building and file lists ask the local article store.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::{Segment, Store};

/// What a nameless article's envelope holds, without its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EnvelopeExtent {
    pub expected: Option<i64>,
    pub subject_bytes: i64,
    pub poster_bytes: i64,
    pub nzb_timestamp: i64,
}

fn nzb_segment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment { release_id: row.get(0)?, file_id: row.get(1)?, message_id: row.get(2)?, part: row.get(3)?, bytes: row.get(4)? })
}

/// Steps `rows` into `out` until it holds `limit` rows or more than `max_bytes` of message ids.
fn fill(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Segment>>,
    out: &mut Vec<Segment>,
    held: &mut usize,
    limit: usize,
    max_bytes: usize,
) -> rusqlite::Result<()> {
    for row in rows {
        let row = row?;
        *held += row.message_id.len();
        out.push(row);
        if out.len() >= limit || *held > max_bytes {
            break;
        }
    }
    Ok(())
}

impl Store {
    /// Whether a release has any article.
    pub(crate) fn nzb_release_has_articles(&self, release: i64) -> Result<bool> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached("select exists(select 1 from segments where release_id = ?1)")?;
        Ok(read.query_row(params![release], |row| row.get(0))?)
    }

    /// How many articles of a release one file holds.
    pub(crate) fn nzb_file_articles(&self, release: i64, file: i64) -> Result<i64> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached("select count(*) from segments where file_id = ?1 and release_id = ?2")?;
        Ok(read.query_row(params![file, release], |row| row.get(0))?)
    }

    /// How many articles of a release one file holds, and the bytes they add up to.
    pub(crate) fn nzb_file_totals(&self, release: i64, file: i64) -> Result<(i64, i64)> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached(
            "select count(*), coalesce(sum(bytes), 0) from segments where file_id = ?1 and release_id = ?2",
        )?;
        Ok(read.query_row(params![file, release], |row| Ok((row.get(0)?, row.get(1)?)))?)
    }

    /// A file's articles in NZB order: unnumbered parts first, then part, then
    /// message id. `after` resumes behind the last row of the page before. A
    /// page ends at `limit` rows or once its message ids pass `max_bytes`, so
    /// the end of the file is an empty page, not a short one.
    pub(crate) fn nzb_page(
        &self,
        release: i64,
        file: i64,
        after: Option<(Option<i64>, &[u8])>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<Segment>> {
        let conn = self.on(release).lock()?;
        let limit = limit.max(1);
        let mut out = Vec::new();
        let mut held = 0;
        // Each statement walks `segments_file_part` (file_id, part, release_id, message_id) in its own order.
        match after {
            None => {
                let mut read = conn.prepare_cached(
                    "select release_id, file_id, message_id, part, bytes from segments
                     where file_id = ?1 and part is null and release_id = ?2 order by message_id limit ?3",
                )?;
                fill(read.query_map(params![file, release, limit as i64], nzb_segment)?, &mut out, &mut held, limit, max_bytes)?;
            }
            Some((None, id)) => {
                let mut read = conn.prepare_cached(
                    "select release_id, file_id, message_id, part, bytes from segments
                     where file_id = ?1 and part is null and release_id = ?2 and message_id > ?3
                     order by message_id limit ?4",
                )?;
                fill(read.query_map(params![file, release, id, limit as i64], nzb_segment)?, &mut out, &mut held, limit, max_bytes)?;
            }
            Some((Some(_), _)) => {}
        }
        if out.len() >= limit || held > max_bytes {
            return Ok(out);
        }
        let left = (limit - out.len()) as i64;
        match after {
            Some((Some(part), id)) => {
                let mut read = conn.prepare_cached(
                    "select release_id, file_id, message_id, part, bytes from segments
                     where file_id = ?1 and part >= ?2 and (part > ?2 or message_id > ?4) and release_id = ?3
                     order by part, release_id, message_id limit ?5",
                )?;
                fill(read.query_map(params![file, part, release, id, left], nzb_segment)?, &mut out, &mut held, limit, max_bytes)?;
            }
            _ => {
                let mut read = conn.prepare_cached(
                    "select release_id, file_id, message_id, part, bytes from segments
                     where file_id = ?1 and part is not null and release_id = ?2
                     order by part, release_id, message_id limit ?3",
                )?;
                fill(read.query_map(params![file, release, left], nzb_segment)?, &mut out, &mut held, limit, max_bytes)?;
            }
        }
        Ok(out)
    }

    /// The sizes of the envelope kept for a nameless article, if one was kept.
    pub(crate) fn nzb_envelope_extent(&self, release: i64, message_id: &[u8]) -> Result<Option<EnvelopeExtent>> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached(
            "select expected, length(subject), length(poster), nzb_timestamp
             from envelopes where release_id = ?1 and message_id = ?2",
        )?;
        Ok(read
            .query_row(params![release, message_id], |row| {
                Ok(EnvelopeExtent {
                    expected: row.get(0)?,
                    subject_bytes: row.get(1)?,
                    poster_bytes: row.get(2)?,
                    nzb_timestamp: row.get(3)?,
                })
            })
            .optional()?)
    }

    /// `count` bytes of an envelope's subject (or else its poster) from byte `at`; None when the envelope is gone.
    pub(crate) fn nzb_envelope_range(
        &self,
        release: i64,
        message_id: &[u8],
        subject: bool,
        at: i64,
        count: i64,
    ) -> Result<Option<Vec<u8>>> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached(if subject {
            "select substr(subject, ?3, ?4) from envelopes where release_id = ?1 and message_id = ?2"
        } else {
            "select substr(poster, ?3, ?4) from envelopes where release_id = ?1 and message_id = ?2"
        })?;
        Ok(read.query_row(params![release, message_id, at + 1, count], |row| row.get(0)).optional()?)
    }

    /// Give an article another message id, as a test's fixture SQL did in PostgreSQL.
    #[cfg(test)]
    pub(crate) fn test_rewrite_message_id(&self, release: i64, from: &[u8], to: &[u8]) -> Result<usize> {
        let conn = self.on(release).lock()?;
        conn.execute(
            "update envelopes set message_id = ?3 where release_id = ?1 and message_id = ?2",
            params![release, from, to],
        )?;
        Ok(conn.execute(
            "update segments set message_id = ?3 where release_id = ?1 and message_id = ?2",
            params![release, from, to],
        )?)
    }

    /// Drop one article's envelope and leave the article.
    #[cfg(test)]
    pub(crate) fn test_forget_envelope(&self, release: i64, message_id: &[u8]) -> Result<usize> {
        let conn = self.on(release).lock()?;
        Ok(conn.execute("delete from envelopes where release_id = ?1 and message_id = ?2", params![release, message_id])?)
    }

    #[cfg(test)]
    pub(crate) fn test_envelope_count(&self) -> Result<i64> {
        let mut count = 0;
        for shard in self.shards() {
            count += shard.lock()?.query_row("select count(*) from envelopes", [], |row| row.get::<_, i64>(0))?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NewRow, Store};

    #[test]
    fn nzb_pages_walk_a_file_unnumbered_parts_first_whatever_the_page_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_writer(dir.path(), "test").unwrap();
        let long = vec![b'm'; 300];
        let rows: Vec<(i64, i64, &[u8], Option<i64>)> = vec![
            (1, 7, b"<z>", None),
            (1, 7, b"<a>", None),
            (1, 7, b"<b>", Some(2)),
            (1, 7, b"<a2>", Some(2)),
            (1, 7, b"<c>", Some(1)),
            (1, 7, &long, Some(2)),
            (1, 7, b"<d>", Some(0)),
            (1, 7, b"<e>", Some(300_000)),
            (1, 8, b"<other-file>", Some(1)),
            (2, 9, b"<other-release>", None),
        ];
        let new: Vec<_> = rows
            .iter()
            .map(|&(release_id, file_id, message_id, part)| NewRow { release_id, file_id, message_id, part, bytes: 10, envelope: None })
            .collect();
        store.add("1", &new, 250_000).unwrap();
        let mut expected: Vec<_> =
            rows.iter().filter(|row| row.1 == 7).map(|&(_, _, id, part)| (part, id.to_vec())).collect();
        // None sorts before Some: PostgreSQL's `part nulls first, message_id`.
        expected.sort();
        for (limit, max_bytes) in [(1, usize::MAX), (2, usize::MAX), (3, usize::MAX), (100, usize::MAX), (100, 5), (100, 0)] {
            let mut walked = Vec::new();
            let mut after: Option<(Option<i64>, Vec<u8>)> = None;
            loop {
                let page =
                    store.nzb_page(1, 7, after.as_ref().map(|(part, id)| (*part, id.as_slice())), limit, max_bytes).unwrap();
                assert!(page.len() <= limit);
                let Some(last) = page.last() else { break };
                after = Some((last.part, last.message_id.clone()));
                walked.extend(page.into_iter().map(|row| (row.part, row.message_id)));
            }
            assert_eq!(walked, expected, "limit {limit}, bytes {max_bytes}");
        }
        assert!(store.nzb_page(1, 8, Some((Some(1), b"<other-file>")), 10, usize::MAX).unwrap().is_empty());
        assert_eq!(store.nzb_file_articles(1, 7).unwrap(), 8);
        assert_eq!(store.nzb_file_totals(1, 7).unwrap(), (8, 80));
        assert_eq!(store.nzb_file_totals(1, 99).unwrap(), (0, 0));
        assert!(store.nzb_release_has_articles(2).unwrap() && !store.nzb_release_has_articles(3).unwrap());
    }
}
