//! Read a native PostgreSQL release and its segments from one snapshot.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use postgres::fallible_iterator::FallibleIterator;
use postgres::{Client, IsolationLevel};

use crate::search::{ArticleRow, ReleaseRow};
use crate::store::{FileInfo, SegmentRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    pub release: ReleaseRow,
    /// Filename order, with file ID breaking ties.
    pub files: Vec<InventoryFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InventoryFile {
    pub info: FileInfo,
    /// Part order with missing parts first, then exact message ID.
    pub segments: Vec<SegmentRef>,
}

impl Inventory {
    /// Adapt this release's own rows for the existing NZB renderer.
    pub fn into_nzb_rows(self) -> (ReleaseRow, Vec<ArticleRow>) {
        let Self { release, files } = self;
        let rows = files
            .into_iter()
            .flat_map(|InventoryFile { info, segments }| {
                let filename = (!info.filename.is_empty()).then_some(info.filename);
                let release = &release;
                segments.into_iter().map(move |segment| ArticleRow {
                    message_id: segment.message_id,
                    filename: filename.clone(),
                    part: segment.part,
                    total_parts: info.expected,
                    bytes: Some(segment.bytes),
                    subject: info.subject.clone(),
                    poster: release.poster.clone(),
                    posted_date: release.posted_date.clone(),
                })
            })
            .collect();
        (release, rows)
    }
}

/// Load a release and its own files as they stood at the start of the read.
/// Visibility checks, linked uploads and duplicate recovery are caller policy.
pub fn inventory(client: &mut Client, id: i64) -> Result<Option<Inventory>> {
    let mut tx = client.build_transaction().isolation_level(IsolationLevel::RepeatableRead).read_only(true).start()?;
    let row = tx.query_opt(
        "select id, coalesce(display_name, name), group_name, poster, posted_date, size, complete, parts
         from releases where id = $1",
        &[&id],
    )?;
    let Some(row) = row else { return Ok(None) };
    let release = ReleaseRow {
        id: row.try_get(0)?,
        name: row.try_get(1)?,
        group_name: row.try_get(2)?,
        poster: row.try_get(3)?,
        posted_date: row.try_get(4)?,
        size: row.try_get(5)?,
        complete: row.try_get(6)?,
        parts: row.try_get(7)?,
    };

    let mut files = BTreeMap::new();
    for row in tx.query(
        "select id, filename, subject, subject_part, expected, file_total, distinct_parts, touched_at
         from files where release_id = $1",
        &[&id],
    )? {
        let info = FileInfo {
            id: row.try_get(0)?,
            filename: row.try_get(1)?,
            subject: row.try_get(2)?,
            subject_part: row.try_get(3)?,
            expected: row.try_get(4)?,
            file_total: row.try_get(5)?,
            parts_seen: row.try_get(6)?,
            touched_at: row.try_get(7)?,
            sealed: false,
        };
        files.insert(info.id, InventoryFile { info, segments: Vec::new() });
    }
    {
        let mut rows = tx.query_raw(
            "select file_id, message_id, part, bytes from segments where release_id = $1
             order by file_id, part nulls first, message_id",
            [&id],
        )?;
        while let Some(row) = rows.next()? {
            let file_id: i64 = row.try_get(0)?;
            let file = files.get_mut(&file_id).with_context(|| format!("segment belongs to missing file {file_id}"))?;
            let message_id = String::from_utf8(row.try_get(1)?).context("segment message ID is not UTF-8")?;
            file.segments.push(SegmentRef { message_id, part: row.try_get(2)?, bytes: row.try_get(3)? });
        }
    }
    tx.commit()?;

    let mut files: Vec<_> = files.into_values().collect();
    files.sort_unstable_by(|a, b| (&a.info.filename, a.info.id).cmp(&(&b.info.filename, b.info.id)));
    Ok(Some(Inventory { release, files }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};

    fn article(filename: Option<&str>, message_id: &str, part: Option<i64>) -> Article {
        Article {
            filename: filename.map(str::to_owned),
            subject: "sample.bin yEnc (1/2)".into(),
            message_id: message_id.into(),
            part,
            total_parts: Some(2),
            bytes: 17,
            ..Article::default()
        }
    }

    #[test]
    fn inventory_adapts_to_the_checked_nzb_renderer() {
        let inventory = Inventory {
            release: ReleaseRow {
                id: 7,
                name: "Display.Sample".into(),
                group_name: "alt.binaries.test".into(),
                poster: Some("poster@example.test".into()),
                posted_date: Some("2020-01-01 00:00:00".into()),
                complete: true,
                ..ReleaseRow::default()
            },
            files: vec![InventoryFile {
                info: FileInfo {
                    id: 1,
                    filename: "sample.bin".into(),
                    subject: Some("sample.bin yEnc (1/1)".into()),
                    subject_part: Some(1),
                    expected: Some(1),
                    file_total: Some(1),
                    parts_seen: 1,
                    touched_at: None,
                    sealed: false,
                },
                segments: vec![SegmentRef { message_id: "<part1@example.test>".into(), part: Some(1), bytes: 17 }],
            }],
        };
        let (release, rows) = inventory.into_nzb_rows();
        assert_eq!(rows[0].poster, release.poster);
        assert_eq!(rows[0].posted_date, release.posted_date);
        let built = crate::nzb::render_checked(&release, &rows, std::slice::from_ref(&release.group_name), true);
        assert!(built.complete());
        assert!(built.xml.contains("<segment bytes=\"17\" number=\"1\">part1@example.test</segment>"));
    }

    #[test]
    fn inventory_reads_the_native_writers_exact_message_ids_in_order() {
        let Some(test) = crate::pg::testing::Schema::new() else { return };
        let mut client = test.pool.get().unwrap();
        assert!(inventory(&mut client, 1).unwrap().is_none());

        let long_id = format!("<{}@example.test>", "x".repeat(4_000));
        let release = Release {
            name: "Original.Sample".into(),
            display_name: Some("Display.Sample".into()),
            group: "alt.binaries.test".into(),
            poster: "poster@example.test".into(),
            date: "2020-01-01 00:00:00".into(),
            articles: vec![
                article(Some("z.bin"), &long_id, Some(2)),
                article(Some("a.bin"), "<z@example.test>", Some(1)),
                article(Some("a.bin"), "<a@example.test>", Some(1)),
                article(Some("a.bin"), "<unknown@example.test>", None),
                article(None, "exact\0id", Some(1)),
            ],
            ..Release::default()
        };
        crate::pg::release_writer::save(&mut client, &[release], 100).unwrap();
        let id: i64 = client.query_one("select id from releases", &[]).unwrap().get(0);
        let found = inventory(&mut client, id).unwrap().unwrap();
        assert_eq!(found.release.name, "Display.Sample");
        assert_eq!(found.release.parts, Some(5));
        assert_eq!(
            found.files.iter().map(|file| file.info.filename.as_str()).collect::<Vec<_>>(),
            ["", "a.bin", "z.bin"]
        );
        let (_, rows) = found.into_nzb_rows();
        let identifiers = rows.iter().map(|row| (row.filename.as_deref(), row.part, row.message_id.as_str()));
        assert_eq!(
            identifiers.collect::<Vec<_>>(),
            [
                (None, Some(1), "exact\0id"),
                (Some("a.bin"), None, "<unknown@example.test>"),
                (Some("a.bin"), Some(1), "<a@example.test>"),
                (Some("a.bin"), Some(1), "<z@example.test>"),
                (Some("z.bin"), Some(2), long_id.as_str()),
            ],
        );
    }
}
