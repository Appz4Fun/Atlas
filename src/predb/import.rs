//! Storing titles: one row per title whatever its case, and a source is never relabelled.
//!
//! Ported from newznab-tmux `app/Services/Predb/Feeds/PredbFeedImporter.php`, GPLv3.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::predb_cache::{BuildStatus, ImportStatus, advance_in, import_in};

    fn fixture(run: impl FnOnce(&mut postgres::Client)) {
        let Some(schema) = crate::pg::testing::Schema::new() else {
            eprintln!("skipping PostgreSQL test: ATLAS_PG_TEST_URL is required");
            return;
        };
        let mut client = schema.pool.get().unwrap();
        client.execute("insert into ext_predb_feeds(source,key) values(1,'fixture')", &[]).unwrap();
        let mut tx = client.transaction().unwrap();
        assert!(matches!(advance_in(&mut tx, 512).unwrap(), BuildStatus::Ready(_)));
        tx.commit().unwrap();
        run(&mut client);
    }
    fn entry(title: &str) -> Entry {
        Entry { title: title.into(), ..Default::default() }
    }
    fn import(
        client: &mut postgres::Client,
        source: i64,
        entries: &[Entry],
        now: i64,
        cursor: Option<(i64, i64)>,
    ) -> Counts {
        let mut tx = client.transaction().unwrap();
        let ImportStatus::Applied { counts, .. } = import_in(&mut tx, source, entries, now, cursor).unwrap() else {
            panic!("genuine completed index required")
        };
        tx.commit().unwrap();
        counts
    }
    #[test]
    fn import_bounds_what_a_feed_sends() {
        fixture(|client| {
            let row = Entry {
                category: Some("c".repeat(MAX_CATEGORY + 1)),
                nuked: 1,
                nuke_reason: Some("é".repeat(MAX_NUKE_REASON / 2 + 1)),
                ..entry("Alpha.Beta.Title-GRP")
            };
            import(client, 1, &[row], 5, None);
            let row = client.query_one("select category,nuke_reason from ext_predb_titles", &[]).unwrap();
            assert_eq!(row.get::<_, Option<String>>(0), None);
            assert_eq!(row.get::<_, String>(1), "é".repeat(128));
        });
    }
    #[test]
    fn import_dedupes_and_skips() {
        fixture(|client| {
            let entries = [
                entry("Alpha.Beta.Title-GRP"),
                entry("Alpha.Beta.Title-GRP"),
                entry("short"),
                entry("bad/slash.title-GRP"),
                entry("Another.Name.Here-GRP"),
            ];
            assert_eq!(
                import(client, 1, &entries, 5, None),
                Counts { inserted: 2, updated: 0, skipped: 3, min_known_id: None }
            );
            assert_eq!(
                client.query_one("select value from ext_predb_meta where key='titles'", &[]).unwrap().get::<_, i64>(0),
                2
            );
            assert_eq!(client.query_one("select count(*) from ext_predb_title_keys", &[]).unwrap().get::<_, i64>(0), 6);
        });
    }
    #[test]
    fn only_empty_fields_fill() {
        fixture(|client| {
            import(client, 1, &[entry("Alpha.Beta.Title-GRP")], 5, None);
            let more = Entry { category: Some("TV".into()), size_bytes: Some(9), ..entry("alpha.beta.title-grp") };
            let counts = import(client, 2, &[more], 6, None);
            assert_eq!(counts.updated, 1);
            assert_eq!(counts.min_known_id, Some(1));
            let row = client.query_one("select title,source,category,size_bytes from ext_predb_titles", &[]).unwrap();
            assert_eq!(row.get::<_, String>(0), "Alpha.Beta.Title-GRP");
            assert_eq!(row.get::<_, i64>(1), 1);
            assert_eq!(row.get::<_, Option<String>>(2), Some("TV".into()));
            assert_eq!(row.get::<_, Option<i64>>(3), Some(9));
            let other = Entry { category: Some("MOVIE".into()), ..entry("Alpha.Beta.Title-GRP") };
            assert_eq!(import(client, 3, &[other], 7, None).updated, 0);
            assert_eq!(
                client.query_one("select category from ext_predb_titles", &[]).unwrap().get::<_, String>(0),
                "TV"
            );
        });
    }
    #[test]
    fn a_nuke_updates() {
        fixture(|client| {
            import(client, 1, &[Entry { category: Some("TV".into()), ..entry("Alpha.Beta.Title-GRP") }], 5, None);
            let nuked = Entry { nuked: 1, nuke_reason: Some("bad.audio".into()), ..entry("Alpha.Beta.Title-GRP") };
            assert_eq!(import(client, 2, &[nuked], 6, None).updated, 1);
            import(client, 2, &[entry("Alpha.Beta.Title-GRP")], 7, None);
            let row = client.query_one("select nuked,nuke_reason,category from ext_predb_titles", &[]).unwrap();
            assert_eq!(row.get::<_, i64>(0), 1);
            assert_eq!(row.get::<_, Option<String>>(1), Some("bad.audio".into()));
            assert_eq!(row.get::<_, Option<String>>(2), Some("TV".into()));
        });
    }
    #[test]
    fn the_same_page_again_changes_nothing() {
        fixture(|client| {
            let page = [entry("Alpha.Beta.Title-GRP"), entry("Another.Name.Here-GRP")];
            import(client, 1, &page, 5, None);
            let counts = import(client, 1, &page, 6, None);
            assert_eq!((counts.inserted, counts.updated, counts.skipped), (0, 0, 2));
        });
    }
    #[test]
    fn the_cursor_moves_only_with_an_insert() {
        fixture(|client| {
            let cursor = |c: &mut postgres::Client| {
                let row = c.query_one("select resume_page,gap_floor from ext_predb_feeds where source=1", &[]).unwrap();
                (row.get::<_, i64>(0), row.get::<_, i64>(1))
            };
            import(client, 1, &[entry("Alpha.Beta.Title-GRP")], 5, Some((4, 9)));
            assert_eq!(cursor(client), (4, 9));
            import(client, 1, &[entry("Alpha.Beta.Title-GRP")], 5, Some((7, 7)));
            assert_eq!(cursor(client), (4, 9));
        });
    }
}
