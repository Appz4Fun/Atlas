//! Is this release that title? Keys only nominate: a literal check accepts.
//!
//! Ported from newznab-tmux `app/Services/NameFixing/{PredbMatchSelector,ConstrainedPredbMatcher,OriginalFilenameEvidence}.php`, GPLv3.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::Connection;

use crate::naming::{
    basename, clean_candidate, distinct_episodes, identity_key, is_episode_name, parent_dir, strip_file_suffixes,
};

use super::keys::{hash_key, md5_hex, name_key, sha1_hex};

static FULL_TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([\w()]+[\s._-]){2,}[\w()]+-\w+").unwrap());
pub const MAX_ROWS_PER_KEY: usize = 16;
pub const MAX_NAMES: usize = 16;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Exact = 1,
    Normalized = 2,
    Hash = 3,
    InnerFile = 4,
}
#[derive(Clone, Debug)]
pub struct TitleRow {
    pub id: i64,
    pub title: String,
    pub category: Option<String>,
    pub size_bytes: Option<i64>,
    pub pre_at: Option<i64>,
    pub nuked: i64,
}
#[derive(Clone, Debug)]
pub struct Match {
    pub title: TitleRow,
    pub kind: Kind,
}
pub struct Evidence<'a> {
    pub name: &'a str,
    pub display_name: Option<&'a str>,
    pub size: i64,
    pub files: Vec<&'a str>,
    pub inner: Vec<&'a str>,
}

pub fn rows_for_key(conn: &Connection, key: i64) -> rusqlite::Result<Vec<TitleRow>> {
    conn.prepare_cached(
        "select t.id, t.title, t.category, t.size_bytes, t.pre_at, t.nuked
         from title_keys k join titles t on t.id = k.title_id where k.key = ?1 order by t.id limit 17",
    )?
    .query_map([key], |r| {
        Ok(TitleRow {
            id: r.get(0)?,
            title: r.get(1)?,
            category: r.get(2)?,
            size_bytes: r.get(3)?,
            pre_at: r.get(4)?,
            nuked: r.get(5)?,
        })
    })?
    .collect()
}
/// Strong enough that folding separators can't be a coincidence: 15+ bytes, 3+ words and a -GROUP.
pub fn is_full_title(title: &str) -> bool {
    title.len() >= 15 && FULL_TITLE.is_match(title)
}
/// `candidate` is `title` plus only a file suffix, letter case aside.
pub fn literally(candidate: &str, title: &str) -> bool {
    let (c, t) = (candidate.to_lowercase(), title.to_lowercase());
    if c == t {
        return true;
    }
    match c.strip_prefix(t.as_str()) {
        Some(rest) if rest.starts_with('.') && rest.is_ascii() => {
            strip_file_suffixes(&format!("title{rest}")) == "title"
        }
        _ => false,
    }
}
pub fn sizes_agree(title: Option<i64>, release: i64) -> bool {
    match title {
        Some(t) if t > 0 && release > 0 => release.saturating_mul(3) >= t && t.saturating_mul(3) >= release,
        _ => true,
    }
}
/// One identity or nothing. Err: two different titles.
fn unique(mut found: Vec<TitleRow>) -> Result<Option<TitleRow>, ()> {
    found.sort_by_key(|t| t.id);
    found.dedup_by_key(|t| t.id);
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        _ => {
            let first = found[0].title.to_lowercase();
            if found.iter().all(|t| t.title.to_lowercase() == first) { Ok(found.into_iter().next()) } else { Err(()) }
        }
    }
}
pub fn find(conn: &Connection, e: &Evidence<'_>) -> rusqlite::Result<Option<Match>> {
    // hash-named posts: the full digest has to agree, a key only nominates, and all of them have to point at one title
    let mut hashed = Vec::new();
    for raw in std::iter::once(e.name).chain(e.files.iter().copied()).take(MAX_NAMES) {
        // cleaned, so `<hash>.mkv` and Part 2's `<hash> [12345678]` count
        let Some(name) = clean_candidate(raw) else { continue };
        let Some(key) = hash_key(&name) else { continue };
        let want = name.to_ascii_lowercase();
        hashed.extend(
            rows_for_key(conn, key)?.into_iter().filter(|t| md5_hex(&t.title) == want || sha1_hex(&t.title) == want),
        );
    }
    match unique(hashed) {
        Ok(Some(t)) => return Ok(Some(Match { title: t, kind: Kind::Hash })),
        Ok(None) => {}
        Err(()) => return Ok(None),
    }
    // the release's own names
    for name in e.display_name.into_iter().chain([e.name]) {
        let clean = clean_candidate(name).unwrap_or_else(|| name.to_string());
        let rows = rows_for_key(conn, name_key(&clean))?;
        if rows.len() > MAX_ROWS_PER_KEY {
            return Ok(None);
        }
        let exact = rows.iter().filter(|t| literally(name, &t.title) || literally(&clean, &t.title)).cloned().collect();
        match unique(exact) {
            Ok(Some(t)) => return Ok(Some(Match { title: t, kind: Kind::Exact })),
            Ok(None) => {}
            Err(()) => return Ok(None),
        }
        let folded = rows
            .into_iter()
            .filter(|t| {
                is_full_title(&t.title)
                    && sizes_agree(t.size_bytes, e.size)
                    && identity_key(&clean) == identity_key(&t.title)
            })
            .collect();
        match unique(folded) {
            Ok(Some(t)) => return Ok(Some(Match { title: t, kind: Kind::Normalized })),
            Ok(None) => {}
            Err(()) => return Ok(None),
        }
    }
    // inner and posted files: all of them have to point at one title
    let many_episodes = distinct_episodes(e.inner.iter().chain(e.files.iter()).copied().map(basename)) >= 2;
    let (mut found, mut tried) = (Vec::new(), HashSet::new());
    for path in e.inner.iter().chain(e.files.iter()).copied() {
        if tried.len() >= MAX_NAMES {
            break;
        }
        for name in clean_candidate(path).into_iter().chain(parent_dir(path).and_then(|p| clean_candidate(&p))) {
            let key = name_key(&name);
            if !tried.insert(key) {
                continue;
            }
            for t in rows_for_key(conn, key)? {
                let same = literally(&name, &t.title) || identity_key(&name) == identity_key(&t.title);
                if same
                    && is_full_title(&t.title)
                    && sizes_agree(t.size_bytes, e.size)
                    && !(many_episodes && is_episode_name(&t.title))
                {
                    found.push(t);
                }
            }
        }
    }
    Ok(unique(found).ok().flatten().map(|t| Match { title: t, kind: Kind::InnerFile }))
}

#[cfg(test)]
mod tests {
    use super::super::import::{Entry, import};
    use super::*;
    use crate::predb::db;

    const T: &str = "Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264-TESTGRP";
    const SECOND: &str = "Second.Title.Name.2023.720p.WEB-OTHER";
    const GIB5: i64 = 5 << 30;

    fn fixture(titles: &[&str]) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = db::create(&dir.path().join("atlas.predb.db")).unwrap();
        let entries: Vec<Entry> = titles
            .iter()
            .map(|t| Entry { title: t.to_string(), size_bytes: Some(GIB5), ..Default::default() })
            .collect();
        import(&mut conn, 1, &entries, 5, None).unwrap();
        (dir, conn)
    }

    fn evidence(name: &str) -> Evidence<'_> {
        Evidence { name, display_name: None, size: GIB5, files: Vec::new(), inner: Vec::new() }
    }

    fn found(conn: &Connection, e: &Evidence<'_>) -> Option<(Kind, String)> {
        find(conn, e).unwrap().map(|m| (m.kind, m.title.title))
    }

    fn check(name: &str, want: Option<Kind>) {
        let (_dir, conn) = fixture(&[T, SECOND]);
        assert_eq!(found(&conn, &evidence(name)), want.map(|k| (k, T.to_string())), "{name}");
    }

    #[test]
    fn exact_names() {
        check(T, Some(Kind::Exact));
        check(&T.to_uppercase(), Some(Kind::Exact));
        check(&format!("{T}.mkv"), Some(Kind::Exact));
        check(&format!("{T} [12345678]"), Some(Kind::Exact));
        check(&format!("{T}.mkv [12345678]"), Some(Kind::Exact));
    }

    #[test]
    fn separators_folded() {
        check("Alpha_Beta Gamma_2024_1080p_WEB-DL_H_264-TESTGRP", Some(Kind::Normalized));
    }

    #[test]
    fn folded_but_sizes_disagree() {
        let (_dir, conn) = fixture(&[T]);
        let e = Evidence { size: 500, ..evidence("Alpha_Beta Gamma_2024_1080p_WEB-DL_H_264-TESTGRP") };
        assert_eq!(found(&conn, &e), None);
    }

    #[test]
    fn display_name_counts() {
        let (_dir, conn) = fixture(&[T]);
        let e = Evidence { display_name: Some(T), ..evidence("Xk3Mz9PqL") };
        assert_eq!(found(&conn, &e), Some((Kind::Exact, T.to_string())));
    }

    #[test]
    fn hashes() {
        let (md5, sha1) = (md5_hex(T), sha1_hex(T));
        check(&md5, Some(Kind::Hash));
        check(&sha1.to_uppercase(), Some(Kind::Hash));
        check(&format!("{sha1}.mkv"), Some(Kind::Hash));
        check(&format!("{md5} [12345678]"), Some(Kind::Hash));
        let (_dir, conn) = fixture(&[T]);
        let file = format!("{md5}.rar");
        let e = Evidence { files: vec![&file], ..evidence("Xk3Mz9PqL") };
        assert_eq!(found(&conn, &e), Some((Kind::Hash, T.to_string())));
    }

    #[test]
    fn hash_files_of_two_titles_are_no_match() {
        let (_dir, conn) = fixture(&[T, SECOND]);
        let (one, two) = (format!("{}.rar", md5_hex(T)), format!("{}.rar", md5_hex(SECOND)));
        let e = Evidence { files: vec![&one, &two], ..evidence("Xk3Mz9PqL") };
        assert_eq!(found(&conn, &e), None);
        let again = format!("{}.r00", sha1_hex(T));
        let e = Evidence { files: vec![&one, &again], ..evidence("Xk3Mz9PqL") };
        assert_eq!(found(&conn, &e), Some((Kind::Hash, T.to_string())), "two hashes of one title agree");
    }

    #[test]
    fn same_prefix_wrong_digest() {
        let mut md5 = md5_hex(T);
        md5.pop();
        md5.push(if md5_hex(T).ends_with('0') { '1' } else { '0' });
        let (_dir, conn) = fixture(&[T]);
        assert!(!rows_for_key(&conn, hash_key(&md5).unwrap()).unwrap().is_empty(), "the key finds it");
        assert_eq!(found(&conn, &evidence(&md5)), None);
    }

    fn with_inner<'a>(inner: Vec<&'a str>) -> Evidence<'a> {
        Evidence { inner, ..evidence("Xk3Mz9PqL") }
    }

    #[test]
    fn inner_files() {
        let (_dir, conn) = fixture(&[T, SECOND]);
        let (dir_path, file) = (format!("{T}/x.mkv"), format!("{T}.mkv"));
        assert_eq!(found(&conn, &with_inner(vec![&dir_path])), Some((Kind::InnerFile, T.to_string())));
        assert_eq!(found(&conn, &with_inner(vec![&file])), Some((Kind::InnerFile, T.to_string())));
        let second = format!("{SECOND}.mkv");
        assert_eq!(found(&conn, &with_inner(vec![&file, &second])), None, "two titles are no match");
    }

    #[test]
    fn extra_text_and_missing_group() {
        check(&format!("{T}.Extra"), None);
        check("Alpha.Beta.Gamma.2024.1080p.WEB-DL.H.264", None);
    }

    #[test]
    fn episodes() {
        let e3 = "Show.Name.S02E03.1080p.WEB-DL.x264-GRP";
        let (_dir, conn) = fixture(&[e3]);
        let (three, four) = (format!("{e3}.mkv"), "Show.Name.S02E04.1080p.WEB-DL.x264-GRP.mkv");
        assert_eq!(found(&conn, &with_inner(vec![&three, four])), None);
        let e = Evidence { files: vec![&three, four], ..evidence("Xk3Mz9PqL") };
        assert_eq!(found(&conn, &e), None, "posted files of many episodes name nothing");
        assert_eq!(found(&conn, &with_inner(vec![&three])), Some((Kind::InnerFile, e3.to_string())));
    }

    #[test]
    fn a_short_title_needs_exact() {
        let (_dir, conn) = fixture(&["Foo.Bar-GRP"]);
        assert_eq!(found(&conn, &evidence("foo_bar-grp")), None);
        assert_eq!(found(&conn, &evidence("foo.bar-grp")), Some((Kind::Exact, "Foo.Bar-GRP".to_string())));
    }

    #[test]
    fn too_many_rows_is_no_match() {
        let (_dir, conn) = fixture(&[T]);
        let key = name_key(T);
        for i in 0..16 {
            conn.execute(
                "insert into titles (title, source, added_at) values (?1, 1, 5)",
                [format!("Forced.Title.Number.{i:02}-GRP")],
            )
            .unwrap();
            let id = conn.last_insert_rowid();
            conn.execute("insert into title_keys (key, title_id) values (?1, ?2)", rusqlite::params![key, id]).unwrap();
        }
        assert_eq!(found(&conn, &evidence(T)), None);
    }

    #[test]
    fn literally_table() {
        assert!(literally("Foo.Bar-GRP.part01.rar", "foo.bar-grp"));
        assert!(!literally("Foo.Bar-GRP.Extra", "foo.bar-grp"));
        assert!(!literally("Foo.Bar-GRP2", "foo.bar-grp"));
    }

    #[test]
    fn is_full_title_table() {
        assert!(!is_full_title("Foo.Bar-GRP"));
        assert!(is_full_title(T));
        assert!(!is_full_title("Just.Words.And.More.Words"));
    }
}
