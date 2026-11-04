//! Who gets processed, the one definition: the stage, the walks and purge all
//! call these. Ported from newznab-tmux, GPLv3
//! (`ReleaseProcessingService::deleteCollectionsWithFewFiles` and its size rule,
//! `CollectionReleasePolicy`). Size, file type and completeness never reject here:
//! upstream's 1 MiB and 100 GiB gate only its body pass, and so they do in `body_gate`.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use rusqlite::{Connection, params};

use crate::config::PostprocessConfig;
use crate::filters::{self, StoredPost};

pub(crate) static PAR2: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(vol[0-9]+[+-][0-9]+\.par2|par2)$").unwrap());
/// a file nothing should fetch a body of
pub(crate) static IMAGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(jpe?g|png|gif|webp|bmp|tiff?)$").unwrap());

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    Blocked,
    Empty,
    TooSmall,
    Fragment,
    TooFewFiles,
    Par2Only,
    /// a program among its files: the release is deleted, see `junk`
    Executable,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Reason::Blocked => "blocked",
            Reason::Empty => "empty",
            Reason::TooSmall => "too_small",
            Reason::Fragment => "fragment",
            Reason::TooFewFiles => "too_few_files",
            Reason::Par2Only => "par2_only",
            Reason::Executable => "executable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Wait(i64),
    Reject(Reason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyGate {
    Open,
    Outside,
    NotYet,
}

/// What a `releases` row says.
#[derive(Clone, Copy, Debug)]
pub struct RowFacts<'a> {
    pub group: &'a str,
    pub name: &'a str,
    pub size: i64,
    pub parts: i64,
    pub complete: bool,
    pub file_total: Option<i64>,
}

/// What a release's `files` rows say, numbers only but for a one-file release's subject.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileSummary {
    pub files: i64,
    pub last_touch: i64,
    pub present: i64,
    pub wanted: i64,
    pub all_par2: bool,
    pub all_images: bool,
    /// a file that is a program
    pub executable: bool,
    /// a video file: what keeps a PC match, see `junk`
    pub has_video: bool,
    pub single_part: Option<i64>,
    pub single_expected: Option<i64>,
    pub single_subject: Option<String>,
}

/// The settings the rules read.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub config: PostprocessConfig,
}

impl Rules {
    pub fn new(config: &PostprocessConfig) -> Rules {
        Rules { config: config.clone() }
    }

    fn min_size(&self, group: &str) -> i64 {
        self.config.min_size_bytes.max(self.config.group_rules.get(group).map_or(0, |g| g.min_size_bytes))
    }

    fn min_files(&self, group: &str) -> i64 {
        self.config.min_files.max(self.config.group_rules.get(group).map_or(0, |g| g.min_files))
    }
}

/// The tests that need no `files` read.
pub fn judge_row(rules: &Rules, row: &RowFacts<'_>) -> Option<Reason> {
    if filters::group_blocked(row.group) {
        Some(Reason::Blocked)
    } else if row.parts <= 0 {
        Some(Reason::Empty)
    } else if row.size < rules.min_size(row.group) {
        Some(Reason::TooSmall)
    } else {
        None
    }
}

/// Each release's files, streamed with no limit: a cap would make a big album
/// read as half complete, and `--dead` would delete it.
pub fn file_summaries(conn: &Connection, ids: &[i64]) -> rusqlite::Result<HashMap<i64, FileSummary>> {
    let mut stmt = conn.prepare_cached(
        "select filename, subject_part, expected, seen, touched_at, subject from files where release_id = ?",
    )?;
    let mut all = HashMap::with_capacity(ids.len());
    for id in ids {
        let mut s = FileSummary { all_par2: true, all_images: true, ..Default::default() };
        let mut rows = stmt.query(params![id])?;
        while let Some(r) = rows.next()? {
            let (filename, part, expected): (String, Option<i64>, Option<i64>) = (r.get(0)?, r.get(1)?, r.get(2)?);
            s.files += 1;
            s.last_touch = s.last_touch.max(r.get::<_, Option<i64>>(4)?.unwrap_or(0));
            let seen = crate::store::seen_count(r.get_ref(3)?.as_blob()?);
            match expected {
                Some(e) if e > 0 => {
                    s.present += seen.min(e);
                    s.wanted += e;
                }
                _ => {
                    s.present += 1;
                    s.wanted += 1;
                }
            }
            s.all_par2 &= PAR2.is_match(&filename);
            s.all_images &= IMAGE.is_match(&filename);
            s.executable |= crate::junk::is_executable(&filename);
            s.has_video |= crate::junk::is_video(&filename);
            if s.files == 1 {
                s.single_part = part;
                s.single_expected = expected;
                s.single_subject = r.get(5)?;
            } else {
                s.single_subject = None;
            }
        }
        all.insert(*id, s);
    }
    Ok(all)
}

/// The tests that read `files`. The settling wait is the only `Wait`, soo no
/// queue row waits past settling, and a late article judges a rejected row again.
pub fn judge_files(rules: &Rules, row: &RowFacts<'_>, s: &FileSummary, now: i64) -> Verdict {
    if s.files == 0 {
        return Verdict::Reject(Reason::Empty);
    }
    // nothing still to come can make it wanted
    if s.executable {
        return Verdict::Reject(Reason::Executable);
    }
    let quiet = if row.complete { rules.config.complete_settle_secs } else { rules.config.settle_secs };
    if now - s.last_touch < quiet {
        return Verdict::Wait(s.last_touch + quiet);
    }
    let post = StoredPost {
        name: row.name,
        parts: row.parts,
        files: s.files as usize,
        part: s.single_part,
        expected: s.single_expected,
        subject: s.single_subject.as_deref(),
    };
    if filters::stored_fragment(&post) {
        return Verdict::Reject(Reason::Fragment);
    }
    if s.files < rules.min_files(row.group) {
        return Verdict::Reject(Reason::TooFewFiles);
    }
    if rules.config.reject_par2_only && s.all_par2 && !crate::parser::is_obfuscated(row.name) {
        return Verdict::Reject(Reason::Par2Only);
    }
    Verdict::Pass
}

/// How complete a release is, out of 1000.
pub fn permille(row: &RowFacts<'_>, s: &FileSummary) -> i64 {
    if row.complete {
        return 1000;
    }
    let by_parts = if s.wanted > 0 { s.present * 1000 / s.wanted } else { 0 };
    if s.files == 1 && crate::parser::is_obfuscated(row.name) {
        return by_parts;
    }
    match row.file_total {
        Some(total) if total > s.files => by_parts.min(s.files * 1000 / total),
        _ => by_parts,
    }
}

/// Whether a release may have a body fetched: only these decide on size, type and completeness.
pub fn body_gate(rules: &Rules, row: &RowFacts<'_>, s: &FileSummary) -> BodyGate {
    let c = &rules.config;
    if (row.size < c.body_min_size_bytes && !s.all_par2)
        || (c.body_max_size_bytes > 0 && row.size > c.body_max_size_bytes)
        || s.all_images
    {
        BodyGate::Outside
    } else if permille(row, s) < c.min_completion_percent * 10 {
        BodyGate::NotYet
    } else {
        BodyGate::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GroupRule;
    use crate::parser::{Article, Release};

    const G: &str = "alt.binaries.t";
    const NOW: i64 = 1_000_000;
    const TOKEN: &str = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6";

    fn rules() -> Rules {
        Rules::new(&PostprocessConfig::default())
    }

    fn row(size: i64, parts: i64, complete: bool) -> RowFacts<'static> {
        RowFacts { group: G, name: "Some.Name", size, parts, complete, file_total: None }
    }

    /// quiet for a day
    fn summary() -> FileSummary {
        FileSummary { files: 2, last_touch: NOW - 86_400, present: 2, wanted: 2, ..Default::default() }
    }

    #[test]
    fn judging_a_row() {
        let r = rules();
        assert_eq!(
            judge_row(&r, &RowFacts { group: "alt.binaries.erotica.teen", ..row(10, 1, true) }),
            Some(Reason::Blocked)
        );
        assert_eq!(judge_row(&r, &row(10, 0, true)), Some(Reason::Empty));
        assert_eq!(judge_row(&r, &row(10, 1, true)), None, "no size floor by default");
        let mut c = PostprocessConfig { min_size_bytes: 100, ..Default::default() };
        assert_eq!(judge_row(&Rules::new(&c), &row(10, 1, true)), Some(Reason::TooSmall));
        c.min_size_bytes = 5;
        c.group_rules.insert(G.into(), GroupRule { min_size_bytes: 100, min_files: 0 });
        assert_eq!(judge_row(&Rules::new(&c), &row(10, 1, true)), Some(Reason::TooSmall), "the larger of the two");
        c.group_rules.get_mut(G).unwrap().min_size_bytes = 1;
        assert_eq!(judge_row(&Rules::new(&c), &row(10, 1, true)), None);
    }

    #[test]
    fn judging_files() {
        let r = rules();
        let judge = |row: &RowFacts<'_>, s: &FileSummary| judge_files(&r, row, s, NOW);
        assert_eq!(judge(&row(10, 1, true), &FileSummary::default()), Verdict::Reject(Reason::Empty));
        // settling is the only wait
        let fresh = FileSummary { last_touch: NOW - 10, ..summary() };
        assert_eq!(judge(&row(10, 1, true), &fresh), Verdict::Wait(NOW - 10 + 900));
        assert_eq!(judge(&row(10, 1, false), &fresh), Verdict::Wait(NOW - 10 + 7200));
        // no touch recorded: old
        assert_eq!(judge(&row(10, 2, true), &FileSummary { last_touch: 0, ..summary() }), Verdict::Pass);
        // a program goes at once, settled or not
        assert_eq!(
            judge(&row(10, 1, false), &FileSummary { executable: true, ..fresh.clone() }),
            Verdict::Reject(Reason::Executable)
        );

        // a settled fragment, with no wait past settling
        let subject = format!(r#""{TOKEN}" yEnc (7/9)"#);
        let fragment = FileSummary {
            files: 1,
            single_part: Some(7),
            single_expected: Some(9),
            single_subject: Some(subject),
            ..summary()
        };
        let named = RowFacts { name: TOKEN, ..row(740_000, 1, false) };
        assert_eq!(judge(&named, &fragment), Verdict::Reject(Reason::Fragment));
        // not one: first and last parts, and (1/1)
        for (part, total, subject) in [(1, 9, "1/9"), (9, 9, "9/9"), (1, 1, "1/1")] {
            let s = FileSummary {
                single_part: Some(part),
                single_expected: Some(total),
                single_subject: Some(format!(r#""{TOKEN}" yEnc ({subject})"#)),
                ..fragment.clone()
            };
            assert_eq!(judge(&named, &s), Verdict::Pass, "{subject}");
        }

        // files
        let c = PostprocessConfig { min_files: 3, ..Default::default() };
        assert_eq!(
            judge_files(&Rules::new(&c), &row(10, 2, true), &summary(), NOW),
            Verdict::Reject(Reason::TooFewFiles)
        );
        // par2 only: kept by default, so Par2Step and LinkStep can name and link its upload; rejected when asked
        assert_eq!(judge(&row(10, 2, true), &FileSummary { all_par2: true, ..summary() }), Verdict::Pass);
        let on = PostprocessConfig { reject_par2_only: true, ..Default::default() };
        assert_eq!(
            judge_files(&Rules::new(&on), &row(10, 2, true), &FileSummary { all_par2: true, ..summary() }, NOW),
            Verdict::Reject(Reason::Par2Only)
        );
        // an obfuscated par2-only row is kept even then
        assert_eq!(
            judge_files(
                &Rules::new(&on),
                &RowFacts { name: TOKEN, ..row(10, 2, true) },
                &FileSummary { all_par2: true, ..summary() },
                NOW
            ),
            Verdict::Pass
        );

        // none of size, type or completeness reject
        let one = FileSummary { files: 1, single_part: Some(1), single_expected: Some(1), ..summary() };
        assert_eq!(judge(&row(740_000, 1, true), &one), Verdict::Pass, "one article of 740 KB");
        assert_eq!(judge(&row(10_000, 1, true), &one), Verdict::Pass, "10 KB");
        assert_eq!(
            judge(&row(10_000, 1, true), &FileSummary { all_images: true, ..one.clone() }),
            Verdict::Pass,
            "images"
        );
        assert_eq!(
            judge(&row(10_000, 1, false), &FileSummary { present: 1, wanted: 3, ..one }),
            Verdict::Pass,
            "incomplete"
        );
    }

    #[test]
    fn the_body_gate() {
        // the defaults keep nothing out: the bounds and the completion floor are what a config can set
        let open = rules();
        let big = 10 << 20;
        let whole = summary();
        assert_eq!(body_gate(&open, &row(1024, 1, true), &whole), BodyGate::Open, "no floor by default");
        assert_eq!(body_gate(&open, &row(200 << 30, 1, true), &whole), BodyGate::Open, "no ceiling by default");
        assert_eq!(
            body_gate(&open, &row(big, 100, false), &FileSummary { present: 1, wanted: 100, ..summary() }),
            BodyGate::Open,
            "however incomplete"
        );
        let r = Rules::new(&PostprocessConfig {
            body_min_size_bytes: 1 << 20,
            body_max_size_bytes: 100 << 30,
            min_completion_percent: 95,
            ..Default::default()
        });
        assert_eq!(body_gate(&r, &row(1024, 1, true), &whole), BodyGate::Outside, "below the bounds");
        assert_eq!(body_gate(&r, &row(big, 1, true), &whole), BodyGate::Open);
        assert_eq!(body_gate(&r, &row(200 << 30, 1, true), &whole), BodyGate::Outside, "above the bounds");
        let no_limit = PostprocessConfig { body_min_size_bytes: 1 << 20, body_max_size_bytes: 0, ..Default::default() };
        assert_eq!(body_gate(&Rules::new(&no_limit), &row(200 << 30, 1, true), &whole), BodyGate::Open);
        assert_eq!(
            body_gate(&r, &row(big, 1, true), &FileSummary { all_images: true, ..summary() }),
            BodyGate::Outside
        );
        let parts = |present: i64| FileSummary { present, wanted: 100, ..summary() };
        assert_eq!(body_gate(&r, &row(big, 100, false), &parts(94)), BodyGate::NotYet);
        assert_eq!(body_gate(&r, &row(big, 100, false), &parts(95)), BodyGate::Open);
        let short = RowFacts { file_total: Some(4), ..row(big, 100, false) };
        assert_eq!(permille(&short, &parts(100)), 500, "two files of four");
        assert_eq!(body_gate(&r, &short, &parts(100)), BodyGate::NotYet);
        let one_missing = RowFacts { file_total: Some(3), ..row(big, 100, false) };
        assert_eq!(permille(&one_missing, &parts(100)), 666, "two files of three");
        assert_eq!(body_gate(&r, &one_missing, &parts(100)), BodyGate::NotYet);
        // an obfuscated single-file row with a post-wide counter and a small par2 row open the gate
        let obf_one = RowFacts { name: TOKEN, file_total: Some(10), ..row(big, 100, false) };
        let one_whole = FileSummary { files: 1, present: 100, wanted: 100, ..summary() };
        assert_eq!(body_gate(&r, &obf_one, &one_whole), BodyGate::Open);
        let small_par2 = FileSummary { files: 1, present: 1, wanted: 1, all_par2: true, ..summary() };
        assert_eq!(body_gate(&r, &RowFacts { name: TOKEN, ..row(50_000, 1, true) }, &small_par2), BodyGate::Open);
    }

    #[test]
    fn over_a_thousand_files_one_article_short_is_not_dead() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        crate::db::create_db_at(&main).unwrap();
        let articles: Vec<Article> = (0..1500)
            .map(|f| Article {
                message_id: format!("<{f}@x>"),
                subject: format!("[{}/1500] \"f{f}.bin\" yEnc (1/1)", f + 1),
                filename: Some(format!("f{f}.bin")),
                part: Some(if f == 0 { 2 } else { 1 }),
                total_parts: Some(if f == 0 { 2 } else { 1 }),
                file_total: Some(1500),
                bytes: 10,
                ..Default::default()
            })
            .collect();
        let release = Release { name: "Album".into(), group: G.into(), articles, ..Default::default() };
        crate::store::save(&main, &[release]).unwrap();
        let conn = crate::db::open_shard(&crate::store::shard_path(&main, crate::store::shard_of(G))).unwrap();
        let (id, complete): (i64, bool) =
            conn.query_row("select id, complete from releases", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(!complete);
        let s = file_summaries(&conn, &[id]).unwrap().remove(&id).unwrap();
        assert_eq!(s.files, 1500);
        let facts = RowFacts { file_total: Some(1500), ..row(10, 1500, false) };
        assert!(permille(&facts, &s) >= 950, "{}", permille(&facts, &s));
    }
}
