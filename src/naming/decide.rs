//! A release's best name, from what's known of it. Pure: every rule has a table test.
//!
//! The order of checks and file priorities are ported from newznab-tmux
//! `app/Services/NameFixing/{ReleaseUpdateService,FilePrioritizer}.php`, GPLv3.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::archive::store::{InnerFileRow, SOURCE_PAR2};
use crate::collections;
use crate::nfo;
use crate::postprocess::step::{NameChange, ReleaseInfo};
use crate::store::FileInfo;

use super::clean::{basename, clean_candidate, has_collision_suffix, identity_key, parent_dir, rot13_subject};
use super::judge::{Grade, distinct_episodes, is_episode_name, judge, renames_pack_to_episode};
use super::{NameSource, regexes};

const SUBJECTS_TRIED: usize = 4;
const FILES_TRIED: usize = 256;

pub fn current_name(r: &ReleaseInfo) -> &str {
    r.display_name.as_deref().unwrap_or(&r.name)
}

pub fn effective_source(r: &ReleaseInfo) -> i64 {
    match (r.name_source, &r.display_name) {
        (Some(s), _) => s,
        (None, Some(d)) if judge(d) >= Grade::Weak => NameSource::InlineBody as i64,
        _ => NameSource::Ingest as i64,
    }
}

/// What the current name is worth: what it would clean to counts, soo `Title-GRP.mkv` is as good as `Title-GRP`.
pub fn grade_now(current: &str) -> Grade {
    let g = judge(current);
    clean_candidate(current).map_or(g, |c| g.max(judge(&c)))
}

/// The whole precedence rule, for every source but PreDB. A candidate needs a source at least as good and a strictly
/// better grade. Soo a well-named post keeps its name whatever an inner file says, and a second run changes nothing.
/// The one exception is a candidate that is the current name cleaned. That is a tidy, not new evidence: it is allowed
/// for a `display_name` or Part 2's collision suffix, and never for a posted `name` whose only flaw is its extension.
pub fn improves(r: &ReleaseInfo, candidate: &str, source: NameSource) -> bool {
    let (grade, current) = (judge(candidate), current_name(r));
    if grade == Grade::Junk || (source as i64) < effective_source(r) || renames_pack_to_episode(current, candidate) {
        return false;
    }
    if identity_key(candidate) == identity_key(current) {
        return false;
    }
    if clean_candidate(current).is_some_and(|c| identity_key(&c) == identity_key(candidate)) {
        return r.display_name.is_some() || has_collision_suffix(current);
    }
    grade > grade_now(current)
}

static IMAGE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(jpe?g|png|gif|webp|bmp|tiff?)$").unwrap());

pub fn is_image_name(name: &str) -> bool {
    IMAGE_NAME.is_match(name)
}

/// `Naming::applies`: could a run improve this name? No I/O.
pub fn worth_running(r: &ReleaseInfo) -> bool {
    if r.name_source.is_some_and(|s| s >= NameSource::PreDb as i64) {
        return false;
    }
    let current = current_name(r);
    if has_collision_suffix(current) {
        return true;
    }
    if grade_now(current) == Grade::Scene && clean_candidate(current).as_deref() == Some(current) {
        return false;
    }
    let rules = regexes::group_has_rules(&r.group);
    if r.parts <= 1 && !rules {
        return false;
    }
    !(is_image_name(&r.name) && (r.parts <= 1 || !rules))
}

static IGNORABLE_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(url|lnk|jpg|jpeg|png|gif|bmp|webp|tif|tiff|txt|sfv|md5|db)$").unwrap());
static FIRST_VOLUME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.part0*1\.rar$").unwrap());
static OTHER_VOLUME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(part\d+\.rar|r0\d?)$").unwrap());
static MAIN_FILE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\.(mkv|avi|mp4|m4v|wmv|mpg|mpeg|mov|ts|m2ts|iso|img|epub|mobi|pdf|flac|mp3)$").unwrap()
});

fn ignorable(path: &str) -> bool {
    let name = basename(path).to_lowercase();
    IGNORABLE_END.is_match(&name)
        || ["sample", "proof", "password", "passwort"].iter().any(|w| name.contains(w))
        || name.starts_with("www.")
}

fn rank(path: &str) -> u8 {
    let name = basename(path).to_lowercase();
    if FIRST_VOLUME.is_match(&name) || (name.ends_with(".rar") && !name.contains(".part")) {
        0
    } else if MAIN_FILE.is_match(&name) {
        1
    } else if name.ends_with(".nfo") || name.ends_with(".7z") || name.ends_with(".zip") || OTHER_VOLUME.is_match(&name)
    {
        2
    } else {
        3
    }
}

struct Candidate {
    name: String,
    grade: Grade,
    weight: u64,
    rank: u8,
}

/// Names in these (path, weight) pairs, best first: grade, summed weight, file rank, longer, lower. Each path offers
/// `clean_candidate` and `clean_candidate(parent_dir)`. With 2 or more distinct episodes, episode names are left out.
pub fn best_from_paths<'a>(paths: impl Iterator<Item = (&'a str, u64)>, min: Grade) -> Vec<String> {
    let mut found: HashMap<String, Candidate> = HashMap::new();
    for (path, weight) in paths.take(FILES_TRIED) {
        if ignorable(path) {
            continue;
        }
        let names = [clean_candidate(path), parent_dir(path).and_then(|d| clean_candidate(&d))];
        for name in names.into_iter().flatten() {
            let entry = found.entry(identity_key(&name)).or_insert_with(|| Candidate {
                grade: judge(&name),
                name,
                weight: 0,
                rank: u8::MAX,
            });
            entry.weight = entry.weight.saturating_add(weight);
            entry.rank = entry.rank.min(rank(path));
        }
    }
    let many_episodes = distinct_episodes(found.values().map(|c| c.name.as_str())) >= 2;
    let mut kept: Vec<Candidate> =
        found.into_values().filter(|c| c.grade >= min && !(many_episodes && is_episode_name(&c.name))).collect();
    kept.sort_by(|a, b| {
        b.grade
            .cmp(&a.grade)
            .then(b.weight.cmp(&a.weight))
            .then(a.rank.cmp(&b.rank))
            .then(b.name.len().cmp(&a.name.len()))
            .then(a.name.cmp(&b.name))
    });
    kept.into_iter().map(|c| c.name).collect()
}

/// Up to `SUBJECTS_TRIED` distinct subjects, lowest file rank first, leaving out scrambled ones.
fn subjects(files: &[FileInfo]) -> Vec<&str> {
    let mut ranked: Vec<(u8, &str)> = files
        .iter()
        .filter_map(|f| f.subject.as_deref().map(|s| (rank(&f.filename), s)))
        .filter(|(_, s)| !rot13_subject(s))
        .collect();
    ranked.sort();
    let mut picked: Vec<&str> = Vec::with_capacity(SUBJECTS_TRIED);
    for (_, subject) in ranked {
        if !picked.contains(&subject) {
            picked.push(subject);
        }
        if picked.len() == SUBJECTS_TRIED {
            break;
        }
    }
    picked
}

/// A scene name the subjects state outright, in a shape no group's regex knows: `Title.2019.720p.x264-GRP - [1/9] - "x.rar"`.
/// Only full scene form counts, because a subject can be an advert, and only when the subjects agree:
/// files of a season pack each name their own episode.
fn scene_name_in_subjects(r: &ReleaseInfo, files: &[FileInfo]) -> Option<String> {
    let mut found: Option<String> = None;
    for subject in subjects(files) {
        let Some(name) = nfo::scene_tokens(subject).find(|t| judge(t) == Grade::Scene) else { continue };
        match &found {
            Some(first) if identity_key(first) != identity_key(name) => return None,
            Some(_) => {}
            None => found = Some(name.to_string()),
        }
    }
    found.filter(|name| improves(r, name, NameSource::NamingRegex))
}

pub fn decide(r: &ReleaseInfo, files: &[FileInfo], inner: &[InnerFileRow], passworded: bool) -> Option<NameChange> {
    let mut best: Option<(Grade, NameSource, String)> = None;
    let mut offer = |name: Option<String>, source: NameSource| -> bool {
        let Some(name) = name else { return false };
        let grade = judge(&name);
        if best.as_ref().is_none_or(|(g, s, _)| (grade, source) > (*g, *s)) {
            best = Some((grade, source, name));
        }
        grade == Grade::Scene
    };
    let pick = |source: NameSource, min: Grade, kinds: &dyn Fn(i64) -> bool| {
        let paths =
            inner.iter().filter(|f| kinds(f.source)).map(|f| (f.name.as_str(), f.size.unwrap_or(1).max(1) as u64));
        best_from_paths(paths, min).into_iter().find(|n| improves(r, n, source))
    };
    // names in passworded posts are where fakes live
    let archive_min = if passworded { Grade::Good } else { Grade::Weak };
    let done = offer(pick(NameSource::Par2, Grade::Weak, &|s| s == SOURCE_PAR2), NameSource::Par2)
        || offer(pick(NameSource::ArchiveInner, archive_min, &|s| s != SOURCE_PAR2), NameSource::ArchiveInner);
    if !done && grade_now(current_name(r)) != Grade::Scene {
        let from_subject = subjects(files).into_iter().find_map(|subject| {
            regexes::names(&r.group, &collections::subject_base(subject))
                .iter()
                .filter_map(|n| clean_candidate(n))
                .find(|n| judge(n) >= Grade::Good && improves(r, n, NameSource::NamingRegex))
        });
        let from_subject = from_subject.or_else(|| scene_name_in_subjects(r, files));
        if !offer(from_subject, NameSource::NamingRegex) {
            let posted = files
                .iter()
                .filter(|f| f.subject.as_deref().is_none_or(|s| !rot13_subject(s)))
                .map(|f| (f.filename.as_str(), f.parts_seen.max(1) as u64));
            let name =
                best_from_paths(posted, Grade::Good).into_iter().find(|n| improves(r, n, NameSource::PostedFile));
            offer(name, NameSource::PostedFile);
        }
    }
    best.map(|(_, source, name)| NameChange {
        display_name: Some(name),
        obfuscated: Some(false),
        source: source.value(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUNK: &str = "Xk3Mz9PqL";
    const SCENE: &str = "Real.Movie.2024.1080p.WEB-DL.x264-GRP";
    const TEEVEE: &str = "alt.binaries.teevee";
    const TEEVEE_SUBJECT: &str = r#"[#####]-[FULL]-[#a.b.teevee@EFNet]-[Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo]-[00/35] "Xk3Mz9PqL.nfo" yEnc (1/1)"#;
    const TEEVEE_NAME: &str = "Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo";

    fn rel(name: &str) -> ReleaseInfo {
        ReleaseInfo { name: name.into(), group: "alt.binaries.no.rules".into(), parts: 50, ..Default::default() }
    }

    fn named(display: &str, source: Option<NameSource>) -> ReleaseInfo {
        ReleaseInfo { display_name: Some(display.into()), name_source: source.map(|s| s as i64), ..rel(JUNK) }
    }

    fn file(name: &str, subject: Option<&str>) -> FileInfo {
        FileInfo {
            id: 1,
            filename: name.into(),
            subject: subject.map(String::from),
            subject_part: Some(1),
            expected: Some(1),
            file_total: None,
            parts_seen: 1,
            touched_at: None,
            sealed: false,
        }
    }

    fn row(name: &str, source: i64) -> InnerFileRow {
        InnerFileRow { seq: 0, name: name.into(), size: Some(100), source }
    }

    /// `decide`, and a second run on its result changes nothing
    fn check(r: &ReleaseInfo, files: &[FileInfo], inner: &[InnerFileRow], passworded: bool) -> Option<(String, u8)> {
        let change = decide(r, files, inner, passworded)?;
        let name = change.display_name.clone().unwrap();
        let next =
            ReleaseInfo { display_name: Some(name.clone()), name_source: Some(i64::from(change.source)), ..r.clone() };
        assert!(decide(&next, files, inner, passworded).is_none(), "a second run on {name}");
        Some((name, change.source))
    }

    fn teevee(r: ReleaseInfo) -> ReleaseInfo {
        ReleaseInfo { group: TEEVEE.into(), ..r }
    }

    fn got(name: &str, source: NameSource) -> Option<(String, u8)> {
        Some((name.to_string(), source.value()))
    }

    #[test]
    fn a_regex_name_replaces_junk() {
        let files = [file("Xk3Mz9PqL.nfo", Some(TEEVEE_SUBJECT))];
        assert_eq!(check(&teevee(rel(JUNK)), &files, &[], false), got(TEEVEE_NAME, NameSource::NamingRegex));
    }

    #[test]
    fn inner_names_beat_a_regex() {
        let files = [file("Xk3Mz9PqL.nfo", Some(TEEVEE_SUBJECT))];
        let inner = [row(&format!("{SCENE}/grp-real.mkv"), 2)];
        assert_eq!(check(&teevee(rel(JUNK)), &files, &inner, false), got(SCENE, NameSource::ArchiveInner));
    }

    #[test]
    fn a_worse_source_never_overwrites() {
        let files = [file("Xk3Mz9PqL.nfo", Some(TEEVEE_SUBJECT))];
        assert_eq!(check(&teevee(named("Other Film Title", Some(NameSource::Nfo))), &files, &[], false), None);
        let inner = [row(&format!("{SCENE}.mkv"), 2)];
        assert_eq!(check(&named("Other Film Title", Some(NameSource::Par2)), &[], &inner, false), None);
    }

    #[test]
    fn a_better_source_and_grade_replaces() {
        let inner = [row(&format!("{SCENE}.mkv"), 2)];
        let r = named("Other Film Title", Some(NameSource::NamingRegex));
        assert_eq!(check(&r, &[], &inner, false), got(SCENE, NameSource::ArchiveInner));
    }

    #[test]
    fn an_equal_grade_never_replaces() {
        let other = "Other.Movie.2019.720p.BluRay.x264-OTHER";
        let inner = [row(&format!("{SCENE}.mkv"), 2)];
        assert_eq!(check(&named(other, Some(NameSource::NamingRegex)), &[], &inner, false), None);
        assert_eq!(check(&named(other, Some(NameSource::ArchiveInner)), &[], &inner, false), None);
        assert_eq!(check(&rel(other), &[], &inner, false), None);
    }

    #[test]
    fn a_well_named_post_is_not_renamed() {
        let inner = [row("grp-sometitle-1080p.mkv", 2)];
        assert_eq!(check(&rel("Some Film 2024 1080p"), &[], &inner, false), None);
        assert_eq!(check(&rel(JUNK), &[], &inner, false), got("grp-sometitle-1080p", NameSource::ArchiveInner));
    }

    #[test]
    fn a_higher_source_with_a_worse_grade_does_not_replace() {
        let inner = [row("Some Book Title.pdf", 1)];
        assert_eq!(check(&named(SCENE, Some(NameSource::NamingRegex)), &[], &inner, false), None);
    }

    #[test]
    fn grade_comes_first_within_a_run() {
        let inner = [row("Some Book Title.pdf", 1), row(&format!("{SCENE}.mkv"), 2)];
        assert_eq!(check(&rel(JUNK), &[], &inner, false), got(SCENE, NameSource::ArchiveInner));
    }

    #[test]
    fn predb_is_never_replaced() {
        let inner = [row(&format!("{SCENE}.mkv"), 1)];
        assert_eq!(check(&named("Some Book Title", Some(NameSource::PreDb)), &[], &inner, false), None);
    }

    #[test]
    fn a_good_inline_name_counts_as_a_body_source() {
        let files = [file("Xk3Mz9PqL.nfo", Some(TEEVEE_SUBJECT))];
        let r = teevee(named("Inline Film 2020 720p", None));
        assert_eq!(check(&r, &files, &[], false), None);
        let inner = [row(&format!("{SCENE}.mkv"), 2)];
        assert_eq!(check(&r, &files, &inner, false), got(SCENE, NameSource::ArchiveInner));
    }

    #[test]
    fn a_junk_inline_name_is_replaced() {
        let files = [file("Xk3Mz9PqL.nfo", Some(TEEVEE_SUBJECT))];
        let r = teevee(named("aB3dE9Xk.mkv", None));
        assert_eq!(check(&r, &files, &[], false), got(TEEVEE_NAME, NameSource::NamingRegex));
    }

    #[test]
    fn a_file_suffix_is_tidied() {
        let inner = [row("Real.Movie.2024.mkv", 1)];
        let r = named("Real.Movie.2024.mkv", Some(NameSource::Par2));
        assert_eq!(check(&r, &[], &inner, false), got("Real.Movie.2024", NameSource::Par2));
    }

    #[test]
    fn a_posted_suffix_is_not_a_rename() {
        for name in ["Hidden.Show.S01E01.mkv", "Some.Movie.2024.1080p.WEB-DL.x264-GRP.mkv"] {
            let files = [file(name, None)];
            assert_eq!(check(&rel(name), &files, &[], false), None, "{name}");
        }
    }

    #[test]
    fn a_collision_suffix_is_tidied() {
        let files = [file("Some.Show.S01E02.mkv", None)];
        let r = rel("Some.Show.S01E02 [12345678]");
        assert_eq!(check(&r, &files, &[], false), got("Some.Show.S01E02", NameSource::PostedFile));
    }

    #[test]
    fn an_ad_prefix_is_not_a_name() {
        let subject = r#"Brought to you by Uploader [01/35] - "kP93xQ7rL.ZMtwBfa.part01.rar" yEnc (1/50)"#;
        let files = [file("kP93xQ7rL.ZMtwBfa.part01.rar", Some(subject))];
        let r = ReleaseInfo { group: "alt.binaries.boneless".into(), ..rel(JUNK) };
        assert_eq!(check(&r, &files, &[], false), None);
    }

    #[test]
    fn a_scene_name_in_a_subject_replaces_junk() {
        let subject = r#"Some.Film.1989.German.AC3.720p.BDRip.XviD-GRP - [10/25] - "SoFi.part08.rar" yEnc (84/262)"#;
        let files = [file("SoFi.part08.rar", Some(subject))];
        let r = ReleaseInfo { group: "alt.binaries.movies.divx".into(), ..rel("SoFi") };
        assert_eq!(
            check(&r, &files, &[], false),
            got("Some.Film.1989.German.AC3.720p.BDRip.XviD-GRP", NameSource::NamingRegex)
        );
    }

    #[test]
    fn a_scene_name_in_a_subject_replaces_a_short_file_name() {
        let subject = r#"Another.Film.2012.German.AC3.DVDRip.XViD.iNTERNAL-GRP [- some.site powered by other.news -] [1,51 GB] [14/89] "grp-another.xvid.nfo" yEnc"#;
        let files = [file("grp-another.xvid.nfo", Some(subject))];
        let r = rel("grp-another.xvid");
        assert_eq!(
            check(&r, &files, &[], false),
            got("Another.Film.2012.German.AC3.DVDRip.XViD.iNTERNAL-GRP", NameSource::NamingRegex)
        );
    }

    #[test]
    fn subjects_naming_different_things_name_nothing() {
        let files = [
            file("a.rar", Some(r#"Show.Name.S02E03.1080p.WEB-DL.x264-GRP [1/9] "a.rar" yEnc (1/2)"#)),
            file("b.rar", Some(r#"Show.Name.S02E04.1080p.WEB-DL.x264-GRP [2/9] "b.rar" yEnc (1/2)"#)),
        ];
        assert_eq!(check(&rel(JUNK), &files, &[], false), None);
    }

    #[test]
    fn subjects_that_agree_are_one_name() {
        let files = [
            file("a.rar", Some(r#"Some.Film.2019.720p.WEB-DL.x264-GRP [1/9] "a.rar" yEnc (1/2)"#)),
            file("b.rar", Some(r#"Some.Film.2019.720p.WEB-DL.x264-GRP [2/9] "b.rar" yEnc (1/2)"#)),
        ];
        assert_eq!(
            check(&rel(JUNK), &files, &[], false),
            got("Some.Film.2019.720p.WEB-DL.x264-GRP", NameSource::NamingRegex)
        );
    }

    #[test]
    fn a_subject_needs_full_scene_form() {
        for subject in [
            r#"Some Film 2019 720p [1/9] "a.rar" yEnc (1/2)"#,
            r#"Brought-to-you-by-Uploader [1/9] "a.rar" yEnc (1/2)"#,
            r#"Some-Random-Words-Here-GRP [1/9] "a.rar" yEnc (1/2)"#,
        ] {
            let files = [file("a.rar", Some(subject))];
            assert_eq!(check(&rel(JUNK), &files, &[], false), None, "{subject}");
        }
    }

    #[test]
    fn a_piece_of_a_name_is_no_name() {
        for name in ["Some.Show.S01E01.It's.Coming.Up.1080p.WEB-DL.x264-GRP", "Some&Show.2019.1080p.WEB-DL.x264-GRP"] {
            let subject = format!(r#"{name} [1/9] "a.rar" yEnc (1/2)"#);
            let files = [file("a.rar", Some(&subject))];
            assert_eq!(check(&rel(JUNK), &files, &[], false), None, "{name}");
        }
    }

    #[test]
    fn a_subjects_scene_name_doesnt_beat_a_better_source() {
        let files = [file("a.rar", Some(r#"Some.Film.2019.720p.WEB-DL.x264-GRP [1/9] "a.rar" yEnc (1/2)"#))];
        assert_eq!(check(&named("Inline Film 2020 720p", None), &files, &[], false), None, "a good inline name");
        assert_eq!(check(&named("Other Film Title", Some(NameSource::Nfo)), &files, &[], false), None);
        let pack = rel("Show.Name.S02.1080p.WEB-DL.x264-GRP");
        let episode = [file("a.rar", Some(r#"Show.Name.S02E03.1080p.WEB-DL.x264-GRP [1/9] "a.rar" yEnc (1/2)"#))];
        assert_eq!(check(&pack, &episode, &[], false), None, "a season pack keeps its name");
    }

    #[test]
    fn a_weak_posted_name_is_not_taken() {
        let files = [file("Some Book Title.pdf", None)];
        assert_eq!(check(&rel(JUNK), &files, &[], false), None);
        let inner = [row("Some Book Title.pdf", 2)];
        assert_eq!(check(&rel(JUNK), &[], &inner, false), got("Some Book Title", NameSource::ArchiveInner));
    }

    #[test]
    fn a_season_pack_keeps_its_name() {
        let inner = [row("Show.Name.S02E03.1080p.WEB-DL.x264-GRP.mkv", 2)];
        assert_eq!(check(&rel("Show.Name.S02.1080p.WEB-DL.x264-GRP"), &[], &inner, false), None);
        assert_eq!(check(&named("Show.Name.S02 Complete", Some(NameSource::NamingRegex)), &[], &inner, false), None);
    }

    #[test]
    fn many_episodes_name_nothing() {
        let inner = [
            row("Show.Name.S02E03.1080p.WEB-DL.x264-GRP.mkv", 2),
            row("Show.Name.S02E04.1080p.WEB-DL.x264-GRP.mkv", 2),
        ];
        assert_eq!(check(&rel(JUNK), &[], &inner, false), None);
    }

    #[test]
    fn samples_and_junk_are_ignored() {
        let inner = [row(&format!("{SCENE}-sample.mkv"), 2), row("abc123Xy9.mkv", 2)];
        assert_eq!(check(&rel(JUNK), &[], &inner, false), None);
    }

    #[test]
    fn passworded_needs_a_good_name() {
        let inner = [row("Some Book Title.pdf", 2)];
        assert_eq!(check(&rel(JUNK), &[], &inner, false), got("Some Book Title", NameSource::ArchiveInner));
        assert_eq!(check(&rel(JUNK), &[], &inner, true), None);
    }

    #[test]
    fn sizes_pick_the_set() {
        let inner = [
            InnerFileRow { size: Some(900), ..row("Rel.Name.2024.x264-GRP.part01.rar", 1) },
            InnerFileRow { size: Some(5), ..row("grp-rn.nfo", 1) },
        ];
        assert_eq!(check(&rel(JUNK), &[], &inner, false), got("Rel.Name.2024.x264-GRP", NameSource::Par2));
    }

    #[test]
    fn separators_are_not_a_rename() {
        let name = "Some.Movie.2024.1080p.WEB-DL.x264-GRP";
        let files = [file("Some Movie 2024 1080p WEB-DL x264-GRP.mkv", None)];
        assert_eq!(check(&rel(name), &files, &[], false), None);
    }

    #[test]
    fn scrambled_subjects_are_skipped() {
        let subject = r#""Some.Movie.2024.1080p.WEB-DL.x264-GRP.mkv" lRap (1/2)"#;
        let files = [file("Some.Movie.2024.1080p.WEB-DL.x264-GRP.mkv", Some(subject))];
        assert_eq!(check(&rel(JUNK), &files, &[], false), None);
    }

    #[test]
    fn worth_running_table() {
        let of = |name: &str, parts: i64, group: &str| ReleaseInfo {
            name: name.into(),
            parts,
            group: group.into(),
            ..Default::default()
        };
        let none = "alt.binaries.no.rules";
        let predb = ReleaseInfo { name_source: Some(100), ..of(JUNK, 50, none) };
        for r in [
            predb,
            of("Some.Movie.2024.1080p.WEB-DL.x264-GRP", 50, none),
            of(JUNK, 1, none),
            of("DSC_0001.jpg", 1, none),
            of("DSC_0001.jpg", 1, TEEVEE),
            of("DSC_0001.jpg", 7, none),
        ] {
            assert!(!worth_running(&r), "{r:?}");
        }
        for r in [
            of(JUNK, 50, none),
            of(JUNK, 1, TEEVEE),
            of("DSC_0001.jpg", 7, TEEVEE),
            of("Some.Movie.2024.1080p.WEB-DL.x264-GRP.mkv", 50, none),
            of("Some.Show.S01E02 [12345678]", 1, none),
        ] {
            assert!(worth_running(&r), "{r:?}");
        }
    }
}
