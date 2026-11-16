//! What the inferred, repair and Spotnet services ask the local article store.

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use super::Store;

/// One article of a file as the inferred-parts proof reads it. A message id
/// longer than `longest` stays in the store and only its length comes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProofRow {
    pub release_id: i64,
    pub part: Option<i64>,
    pub message_id: Option<Vec<u8>>,
    pub message_id_len: i64,
}

fn proof_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProofRow> {
    Ok(ProofRow { release_id: row.get(0)?, part: row.get(1)?, message_id: row.get(2)?, message_id_len: row.get(3)? })
}

const PROOF: &str = "select release_id, part, case when length(message_id) <= ?2 then message_id end, length(message_id) from segments";

impl Store {
    /// A file's articles in the order of the `(file_id, part)` index:
    /// unnumbered parts first, then by part, release and message id. `after`
    /// is the `(part, release_id, message_id)` of the last row of the page
    /// before, so each page is one index seek however large the file is.
    pub(crate) fn proof_page(
        &self,
        file: i64,
        longest: i64,
        after: Option<(Option<i64>, i64, &[u8])>,
        limit: usize,
    ) -> Result<Vec<ProofRow>> {
        let conn = self.home(file)?.lock()?;
        let numbered = |limit: usize| -> Result<Vec<ProofRow>> {
            let mut read = conn.prepare_cached(&format!(
                "{PROOF} where file_id = ?1 and part is not null order by part, release_id, message_id limit ?3"
            ))?;
            Ok(read.query_map(params![file, longest, limit as i64], proof_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
        };
        match after {
            None => {
                let mut read = conn.prepare_cached(&format!(
                    "{PROOF} where file_id = ?1 order by part, release_id, message_id limit ?3"
                ))?;
                Ok(read.query_map(params![file, longest, limit as i64], proof_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
            }
            Some((None, release, id)) => {
                let mut rows = {
                    let mut read = conn.prepare_cached(&format!(
                        "{PROOF} where file_id = ?1 and part is null and (release_id, message_id) > (?3, ?4)
                         order by release_id, message_id limit ?5"
                    ))?;
                    read.query_map(params![file, longest, release, id, limit as i64], proof_row)?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                if rows.len() < limit {
                    rows.extend(numbered(limit - rows.len())?);
                }
                Ok(rows)
            }
            Some((Some(part), release, id)) => {
                let mut read = conn.prepare_cached(&format!(
                    "{PROOF} where file_id = ?1 and (part, release_id, message_id) > (?3, ?4, ?5)
                     order by part, release_id, message_id limit ?6"
                ))?;
                Ok(read
                    .query_map(params![file, longest, part, release, id, limit as i64], proof_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }
        }
    }

    /// The article size a file has most often, the larger of two equally
    /// common ones. `except` leaves out the articles of that one part number.
    pub(crate) fn common_bytes(&self, file: i64, except: Option<i64>) -> Result<Option<i64>> {
        let conn = self.home(file)?.lock()?;
        Ok(match except {
            Some(part) => conn
                .prepare_cached(
                    "select bytes from segments where file_id = ?1 and part is not ?2
                     group by bytes order by count(*) desc, bytes desc limit 1",
                )?
                .query_row(params![file, part], |row| row.get(0))
                .optional()?,
            None => conn
                .prepare_cached(
                    "select bytes from segments where file_id = ?1 group by bytes order by count(*) desc, bytes desc limit 1",
                )?
                .query_row(params![file], |row| row.get(0))
                .optional()?,
        })
    }

    /// The highest and the lowest part number a file has within `0..=max_part`,
    /// the range `file_parts` kept.
    pub(crate) fn file_part_bounds(&self, file: i64, max_part: i64) -> Result<(Option<i64>, Option<i64>)> {
        let conn = self.home(file)?.lock()?;
        let mut read = conn
            .prepare_cached("select max(part), min(part) from segments where file_id = ?1 and part between 0 and ?2")?;
        Ok(read.query_row(params![file, max_part], |row| Ok((row.get(0)?, row.get(1)?)))?)
    }

    /// The distinct part numbers a file has above `after` and within
    /// `0..=max_part`, ascending: a page of what `file_parts` kept.
    pub(crate) fn file_parts_within(&self, file: i64, after: i64, max_part: i64, limit: usize) -> Result<Vec<i64>> {
        let conn = self.home(file)?.lock()?;
        let mut read = conn.prepare_cached(
            "select distinct part from segments where file_id = ?1 and part > ?2 and part between 0 and ?3
             order by part limit ?4",
        )?;
        Ok(read.query_map(params![file, after, max_part, limit as i64], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The bytes of every article one file of a release has; 0 when it has none.
    pub(crate) fn file_bytes(&self, release: i64, file: i64) -> Result<i64> {
        let conn = self.on(release).lock()?;
        let mut read =
            conn.prepare_cached("select coalesce(sum(bytes), 0) from segments where file_id = ?1 and release_id = ?2")?;
        Ok(read.query_row(params![file, release], |row| row.get(0))?)
    }

    /// For each message id, the file of `release` that has that article.
    pub(crate) fn files_of(&self, release: i64, message_ids: &[Vec<u8>]) -> Result<Vec<Option<i64>>> {
        let conn = self.on(release).lock()?;
        let mut read = conn.prepare_cached("select file_id from segments where release_id = ?1 and message_id = ?2")?;
        message_ids.iter().map(|id| Ok(read.query_row(params![release, id], |row| row.get(0)).optional()?)).collect()
    }

    /// Whether the store holds no article and no envelope at all.
    pub(crate) fn holds_nothing(&self) -> Result<bool> {
        for shard in self.shards() {
            let empty: bool = shard.lock()?.query_row(
                "select not exists(select 1 from segments) and not exists(select 1 from envelopes)",
                [],
                |row| row.get(0),
            )?;
            if !empty {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// How many articles the store holds.
    pub(crate) fn segment_count(&self) -> Result<i64> {
        let mut count = 0;
        for shard in self.shards() {
            count += shard.lock()?.query_row("select count(*) from segments", [], |row| row.get::<_, i64>(0))?;
        }
        Ok(count)
    }
}

/// One article and the file it belongs to, as a test's assertions read it.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TestArticle {
    pub release_id: i64,
    pub file_id: i64,
    pub message_id: Vec<u8>,
    pub part: Option<i64>,
    pub filename: Vec<u8>,
}

#[cfg(test)]
impl Store {
    /// Every article of the catalogue `client` reads, by release and message
    /// id, from the local store when the schema has one and from PostgreSQL
    /// otherwise: what a fixture's `select .. from segments` asked.
    pub(crate) fn articles_for_test(client: &mut impl postgres::GenericClient) -> Vec<TestArticle> {
        let names: std::collections::HashMap<i64, Vec<u8>> = client
            .query("select id, original_filename from files", &[])
            .unwrap()
            .into_iter()
            .map(|row| (row.get(0), row.get::<_, Option<Vec<u8>>>(1).unwrap_or_default()))
            .collect();
        let rows: Vec<(i64, i64, Vec<u8>, Option<i64>)> = match super::of(client).unwrap() {
            // every file of the store, merged back into one order
            Some(store) => {
                let mut rows = Vec::new();
                for shard in store.shards() {
                    let conn = shard.lock().unwrap();
                    let mut read = conn.prepare("select release_id, file_id, message_id, part from segments").unwrap();
                    rows.extend(
                        read.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                            .unwrap()
                            .collect::<rusqlite::Result<Vec<_>>>()
                            .unwrap(),
                    );
                }
                rows.sort_by(|a: &(i64, i64, Vec<u8>, Option<i64>), b| (a.0, &a.2).cmp(&(b.0, &b.2)));
                rows
            }
            None => client
                .query("select release_id, file_id, message_id, part from segments order by release_id, message_id", &[])
                .unwrap()
                .into_iter()
                .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
                .collect(),
        };
        rows.into_iter()
            .map(|(release_id, file_id, message_id, part)| TestArticle {
                release_id,
                file_id,
                message_id,
                part,
                filename: names.get(&file_id).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// How many `(file, part)` pairs `file_parts` would hold for that catalogue.
    pub(crate) fn file_parts_for_test(client: &mut impl postgres::GenericClient) -> i64 {
        let parts: std::collections::BTreeSet<(i64, i64)> = Self::articles_for_test(client)
            .into_iter()
            .filter_map(|a| a.part.filter(|part| (0..=crate::parser::MAX_PARTS).contains(part)).map(|part| (a.file_id, part)))
            .collect();
        parts.len() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NewRow, Store};

    fn row<'a>(release: i64, file: i64, id: &'a [u8], part: Option<i64>, bytes: i64) -> NewRow<'a> {
        NewRow { release_id: release, file_id: file, message_id: id, part, bytes, envelope: None }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_writer(dir.path(), "test").unwrap();
        (dir, store)
    }

    #[test]
    fn a_proof_walk_meets_every_article_of_the_file_once_and_keeps_long_ids_back() {
        let (_dir, store) = store();
        let long = vec![b'x'; 40];
        let ids: Vec<Vec<u8>> = (0..40).map(|n| format!("<{n:02}>").into_bytes()).collect();
        let mut rows = vec![row(1, 7, b"<none-b>", None, 1), row(1, 7, b"<none-a>", None, 1), row(1, 7, &long, Some(2), 1)];
        // duplicate parts, and another file of the same release in between
        rows.extend(ids.iter().enumerate().map(|(n, id)| row(1, if n % 5 == 4 { 8 } else { 7 }, id, Some(n as i64 / 3), 1)));
        store.add("1", &rows, 100).unwrap();
        for page in [1, 2, 3, 16, 100] {
            let mut seen = Vec::new();
            let mut after: Option<(Option<i64>, i64, Vec<u8>)> = None;
            loop {
                let rows = store.proof_page(7, 30, after.as_ref().map(|(p, r, m)| (*p, *r, m.as_slice())), page).unwrap();
                if rows.is_empty() {
                    break;
                }
                assert!(rows.len() <= page);
                for row in rows {
                    assert_eq!(row.message_id.is_none(), row.message_id_len > 30);
                    let id = row.message_id.unwrap_or_else(|| long.clone());
                    assert_eq!(id.len() as i64, row.message_id_len);
                    after = Some((row.part, row.release_id, id.clone()));
                    seen.push((row.part, id));
                }
            }
            let mut expected: Vec<(Option<i64>, Vec<u8>)> = vec![(None, b"<none-a>".to_vec()), (None, b"<none-b>".to_vec()), (Some(2), long.clone())];
            expected.extend(ids.iter().enumerate().filter(|(n, _)| n % 5 != 4).map(|(n, id)| (Some(n as i64 / 3), id.clone())));
            expected.sort();
            assert_eq!(seen, expected, "page size {page}");
        }
        assert!(store.proof_page(9, 30, None, 16).unwrap().is_empty());
    }

    #[test]
    fn the_common_size_prefers_the_larger_of_a_tie_and_can_leave_one_part_out() {
        let (_dir, store) = store();
        store
            .add(
                "1",
                &[
                    row(1, 7, b"<a>", Some(1), 500),
                    row(1, 7, b"<b>", Some(2), 700),
                    row(1, 7, b"<c>", Some(3), 90),
                    row(1, 7, b"<d>", Some(3), 90),
                    row(1, 7, b"<e>", Some(3), 90),
                    row(1, 7, b"<f>", None, 700),
                    row(1, 8, b"<g>", Some(4), 33),
                ],
                100,
            )
            .unwrap();
        assert_eq!(store.common_bytes(7, None).unwrap(), Some(90));
        // an unnumbered article is distinct from every part number, as in PostgreSQL
        assert_eq!(store.common_bytes(7, Some(3)).unwrap(), Some(700));
        assert_eq!(store.common_bytes(7, Some(9)).unwrap(), Some(90));
        assert_eq!(store.common_bytes(8, Some(4)).unwrap(), None);
        assert_eq!(store.common_bytes(8, None).unwrap(), Some(33));
        assert_eq!(store.common_bytes(9, None).unwrap(), None);
        assert_eq!(store.file_bytes(1, 7).unwrap(), 2170);
        assert_eq!(store.file_bytes(2, 7).unwrap(), 0);
        assert_eq!(store.file_bytes(1, 9).unwrap(), 0);
    }

    #[test]
    fn a_files_parts_are_its_distinct_numbers_within_the_bound() {
        let (_dir, store) = store();
        assert!(store.holds_nothing().unwrap());
        assert_eq!(store.segment_count().unwrap(), 0);
        store
            .add(
                "1",
                &[
                    row(1, 7, b"<a>", Some(0), 1),
                    row(1, 7, b"<b>", Some(5), 1),
                    row(1, 7, b"<c>", Some(5), 1),
                    row(1, 7, b"<d>", Some(100), 1),
                    row(1, 7, b"<e>", Some(101), 1),
                    row(1, 7, b"<f>", Some(-3), 1),
                    row(1, 7, b"<g>", None, 1),
                    row(1, 8, b"<h>", Some(101), 1),
                    row(1, 8, b"<i>", None, 1),
                ],
                100,
            )
            .unwrap();
        assert!(!store.holds_nothing().unwrap());
        assert_eq!(store.segment_count().unwrap(), 9);
        assert_eq!(store.file_part_bounds(7, 100).unwrap(), (Some(100), Some(0)));
        assert_eq!(store.file_part_bounds(8, 100).unwrap(), (None, None));
        assert_eq!(store.file_parts_within(7, -1, 100, 4096).unwrap(), vec![0, 5, 100]);
        assert_eq!(store.file_parts_within(7, -1, 100, 2).unwrap(), vec![0, 5]);
        assert_eq!(store.file_parts_within(7, 5, 100, 2).unwrap(), vec![100]);
        assert!(store.file_parts_within(7, 100, 100, 2).unwrap().is_empty());
        assert!(store.file_parts_within(8, -1, 100, 2).unwrap().is_empty());
        assert_eq!(
            store.files_of(1, &[b"<a>".to_vec(), b"<h>".to_vec(), b"<zz>".to_vec()]).unwrap(),
            vec![Some(7), Some(8), None]
        );
        assert_eq!(store.files_of(2, &[b"<a>".to_vec()]).unwrap(), vec![None]);
    }
}
