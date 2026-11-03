//! Storing titles: one row per title whatever its case, and a source is never relabelled.
//!
//! Ported from newznab-tmux `app/Services/Predb/Feeds/PredbFeedImporter.php`, GPLv3.

use rusqlite::{Connection, params};

use super::keys::{name_key, title_keys, valid_title};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub title: String,
    pub category: Option<String>,
    pub size_bytes: Option<i64>,
    pub files: Option<i64>,
    pub pre_at: Option<i64>,
    pub nuked: i64,
    pub nuke_reason: Option<String>,
}

#[derive(Default, Debug, PartialEq, Clone, Copy)]
pub struct Counts {
    pub inserted: u64,
    pub updated: u64,
    pub skipped: u64,
    /// the lowest `titles.id` among these entries that were stored before this call
    pub min_known_id: Option<i64>,
}

/// Longest `category` kept, in bytes: longer is None.
pub const MAX_CATEGORY: usize = 64;
/// Longest `nuke_reason` kept, in bytes: longer is cut at a character boundary.
pub const MAX_NUKE_REASON: usize = 256;

fn bounded_reason(reason: &str) -> &str {
    let mut end = reason.len().min(MAX_NUKE_REASON);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}

/// One transaction. For each entry: trimmed; `valid_title` or skipped; `category` and `nuke_reason` bounded.
/// Rows under `name_key` whose title is equal when lowercased are this title. None: insert it and its three
/// keys. Found: empty `category`, `size_bytes`, `files` and `pre_at` are filled; a non-zero `nuked` replaces,
/// with its reason when it has one; `source` and `title` never change. `meta.titles` grows by the number inserted.
/// `cursor`, in the same transaction when any entry was inserted: `update feeds set resume_page = page, gap_floor = floor`.
pub fn import(
    conn: &mut Connection,
    source: i64,
    entries: &[Entry],
    now: i64,
    cursor: Option<(i64, i64)>,
) -> rusqlite::Result<Counts> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut counts = Counts::default();
    let top_before: i64 = tx.query_row("select coalesce(max(id), 0) from titles", [], |r| r.get(0))?;
    {
        let mut same_key = tx.prepare_cached(
            "select t.id, t.title from title_keys k join titles t on t.id = k.title_id where k.key = ?1 order by t.id",
        )?;
        let mut insert = tx.prepare_cached(
            "insert into titles (title, source, category, size_bytes, files, pre_at, nuked, nuke_reason, added_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?;
        let mut add_key = tx.prepare_cached("insert or ignore into title_keys (key, title_id) values (?1, ?2)")?;
        let mut fill = tx.prepare_cached(
            "update titles set category = coalesce(category, ?2), size_bytes = coalesce(size_bytes, ?3),
                files = coalesce(files, ?4), pre_at = coalesce(pre_at, ?5)
             where id = ?1 and ((category is null and ?2 is not null) or (size_bytes is null and ?3 is not null)
                or (files is null and ?4 is not null) or (pre_at is null and ?5 is not null))",
        )?;
        let mut nuke = tx.prepare_cached(
            "update titles set nuked = ?2, nuke_reason = coalesce(?3, nuke_reason)
             where id = ?1 and (nuked != ?2 or (?3 is not null and nuke_reason is not ?3))",
        )?;
        for e in entries {
            let title = e.title.trim();
            if !valid_title(title) {
                counts.skipped += 1;
                continue;
            }
            let category = e.category.as_deref().filter(|c| !c.is_empty() && c.len() <= MAX_CATEGORY);
            let reason = e.nuke_reason.as_deref().filter(|r| !r.is_empty()).map(bounded_reason);
            let lower = title.to_lowercase();
            let found: Option<i64> = same_key
                .query_map([name_key(title)], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .filter_map(Result::ok)
                .find(|(_, t)| t.to_lowercase() == lower)
                .map(|(id, _)| id);
            let Some(id) = found else {
                insert.execute(params![
                    title,
                    source,
                    category,
                    e.size_bytes,
                    e.files,
                    e.pre_at,
                    e.nuked,
                    if e.nuked != 0 { reason } else { None },
                    now
                ])?;
                let id = tx.last_insert_rowid();
                for key in title_keys(title) {
                    add_key.execute(params![key, id])?;
                }
                counts.inserted += 1;
                continue;
            };
            if id <= top_before {
                counts.min_known_id = Some(counts.min_known_id.map_or(id, |m| m.min(id)));
            }
            let mut changed = fill.execute(params![id, category, e.size_bytes, e.files, e.pre_at])?;
            if e.nuked != 0 {
                changed += nuke.execute(params![id, e.nuked, reason])?;
            }
            if changed > 0 {
                counts.updated += 1;
            } else {
                counts.skipped += 1;
            }
        }
    }
    if counts.inserted > 0 {
        tx.execute("update meta set value = value + ?1 where key = 'titles'", params![counts.inserted as i64])?;
        if let Some((page, floor)) = cursor {
            tx.execute(
                "update feeds set resume_page = ?1, gap_floor = ?2 where source = ?3",
                params![page, floor, source],
            )?;
        }
    }
    tx.commit()?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predb::db;

    fn fixture() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::create(&dir.path().join("atlas.predb.db")).unwrap();
        (dir, conn)
    }

    fn entry(title: &str) -> Entry {
        Entry { title: title.into(), ..Default::default() }
    }

    #[test]
    fn import_bounds_what_a_feed_sends() {
        let (_dir, mut conn) = fixture();
        let e = Entry {
            category: Some("c".repeat(1 << 20)),
            nuked: 1,
            nuke_reason: Some("é".repeat(1 << 19)),
            ..entry("Alpha.Beta.Title-GRP")
        };
        import(&mut conn, 1, &[e], 5, None).unwrap();
        let (category, reason): (Option<String>, String) =
            conn.query_row("select category, nuke_reason from titles", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(category, None);
        assert_eq!(reason.len(), 256);
    }

    #[test]
    fn import_dedupes_and_skips() {
        let (_dir, mut conn) = fixture();
        let entries = [
            entry("Alpha.Beta.Title-GRP"),
            entry("Alpha.Beta.Title-GRP"),
            entry("short"),
            entry("bad/slash.title-GRP"),
            entry("Another.Name.Here-GRP"),
        ];
        let counts = import(&mut conn, 1, &entries, 5, None).unwrap();
        assert_eq!(counts, Counts { inserted: 2, updated: 0, skipped: 3, min_known_id: None });
        let titles: i64 = conn.query_row("select value from meta where key = 'titles'", [], |r| r.get(0)).unwrap();
        assert_eq!(titles, 2);
        let keys: i64 = conn.query_row("select count(*) from title_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(keys, 6);
    }

    #[test]
    fn only_empty_fields_fill() {
        let (_dir, mut conn) = fixture();
        import(&mut conn, 1, &[entry("Alpha.Beta.Title-GRP")], 5, None).unwrap();
        let more = Entry { category: Some("TV".into()), size_bytes: Some(9), ..entry("alpha.beta.title-grp") };
        let counts = import(&mut conn, 2, &[more], 6, None).unwrap();
        assert_eq!(counts.updated, 1);
        assert_eq!(counts.min_known_id, Some(1));
        let row = |c: &Connection| -> (String, i64, Option<String>, Option<i64>) {
            c.query_row("select title, source, category, size_bytes from titles", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
        };
        assert_eq!(row(&conn), ("Alpha.Beta.Title-GRP".into(), 1, Some("TV".into()), Some(9)));
        let other = Entry { category: Some("MOVIE".into()), ..entry("Alpha.Beta.Title-GRP") };
        assert_eq!(import(&mut conn, 3, &[other], 7, None).unwrap().updated, 0);
        assert_eq!(row(&conn).2.as_deref(), Some("TV"));
    }

    #[test]
    fn a_nuke_updates() {
        let (_dir, mut conn) = fixture();
        let first = Entry { category: Some("TV".into()), ..entry("Alpha.Beta.Title-GRP") };
        import(&mut conn, 1, &[first], 5, None).unwrap();
        let nuked = Entry { nuked: 1, nuke_reason: Some("bad.audio".into()), ..entry("Alpha.Beta.Title-GRP") };
        assert_eq!(import(&mut conn, 2, &[nuked], 6, None).unwrap().updated, 1);
        import(&mut conn, 2, &[entry("Alpha.Beta.Title-GRP")], 7, None).unwrap();
        let row: (i64, Option<String>, Option<String>) = conn
            .query_row("select nuked, nuke_reason, category from titles", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(row, (1, Some("bad.audio".into()), Some("TV".into())));
    }

    #[test]
    fn the_same_page_again_changes_nothing() {
        let (_dir, mut conn) = fixture();
        let page = [entry("Alpha.Beta.Title-GRP"), entry("Another.Name.Here-GRP")];
        import(&mut conn, 1, &page, 5, None).unwrap();
        let counts = import(&mut conn, 1, &page, 6, None).unwrap();
        assert_eq!((counts.inserted, counts.updated, counts.skipped), (0, 0, 2));
    }

    #[test]
    fn the_cursor_moves_only_with_an_insert() {
        let (_dir, mut conn) = fixture();
        conn.execute("insert into feeds (source, key) values (1, 'x')", []).unwrap();
        let cursor = |c: &Connection| -> (i64, i64) {
            c.query_row("select resume_page, gap_floor from feeds", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
        };
        import(&mut conn, 1, &[entry("Alpha.Beta.Title-GRP")], 5, Some((4, 9))).unwrap();
        assert_eq!(cursor(&conn), (4, 9));
        import(&mut conn, 1, &[entry("Alpha.Beta.Title-GRP")], 5, Some((7, 7))).unwrap();
        assert_eq!(cursor(&conn), (4, 9));
    }
}
