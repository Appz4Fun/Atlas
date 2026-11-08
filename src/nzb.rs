use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use indexmap::IndexMap;

use crate::atomic::write_atomic;
use crate::dates::article_timestamp;
use crate::search::{ArticleRow, ReleaseRow, get_articles, get_release};
use crate::ui;

pub fn nzb_filename(name: &str, release_id: Option<i64>) -> String {
    let bad = "<>:\"/\\|?*";
    let safe: String = name.chars().map(|c| if bad.contains(c) || (c as u32) < 32 { '_' } else { c }).collect();
    let safe = safe.trim_matches([' ', '.']);
    let safe = if safe.is_empty() { "release" } else { safe };

    match release_id {
        Some(id) if id != 0 => format!("{safe}.{id}.nzb"),
        _ => format!("{safe}.nzb"),
    }
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if is_xml_char(c) => out.push(c),
            _ => {}
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#09;"),
            c if is_xml_char(c) => out.push(c),
            _ => {}
        }
    }
    out
}

/// xml 1.0 cant carry most control chars, even escaped
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

/// A file of an NZB whose segment numbers don't run exactly 1..=total, or (`files`) the files an NZB lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileGap {
    /// true: not one file but the whole NZB, short of files its `[n/N]` counter names; `total`, `have` and
    /// `missing` then count files, not parts
    pub files: bool,
    pub filename: Option<String>,
    /// the most parts any of its articles says the file has; None: none says
    pub total: Option<i64>,
    /// how many distinct numbers in 1..=total it has
    pub have: i64,
    /// the first numbers it lacks, `GAP_LIST` at most
    pub missing: Vec<i64>,
    /// articles with no number, or one outside 1..=total
    pub stray: i64,
}

/// how many missing numbers a gap names
const GAP_LIST: usize = 16;

/// The files of `articles` (grouped as `render_nzb_in` groups them) whose numbers aren't exactly 1..=total.
/// Empty: every file is whole. These are the same articles an NZB is rendered from, so a release is only whole
/// by what its NZB lists, not by what was counted when its articles were saved.
pub fn gaps(articles: &[ArticleRow]) -> Vec<FileGap> {
    let mut out = Vec::new();
    for file_articles in files(articles) {
        let filename = file_articles[0].filename.as_deref();
        let total = file_articles.iter().filter_map(|a| a.total_parts).max();
        let limit = total.unwrap_or(0).clamp(0, crate::parser::MAX_PARTS);
        let zero_based = file_articles.iter().any(|a| a.part == Some(0));
        let mut parts = std::collections::BTreeSet::new();
        let mut stray = 0;
        for a in &file_articles {
            let in_range = if zero_based {
                (0..limit).contains(&a.part.unwrap_or(-1))
            } else {
                (1..=limit).contains(&a.part.unwrap_or(-1))
            };
            match a.part {
                Some(p) if in_range => {
                    parts.insert(p);
                }
                _ => stray += 1,
            }
        }
        let have = parts.len() as i64;
        if total.is_some() && have == limit && limit > 0 && stray == 0 {
            continue;
        }
        let missing: Vec<i64> = if zero_based {
            (0..limit).filter(|p| !parts.contains(p)).take(GAP_LIST).collect()
        } else {
            (1..=limit).filter(|p| !parts.contains(p)).take(GAP_LIST).collect()
        };
        out.push(FileGap { files: false, filename: filename.map(str::to_string), total, have, missing, stray });
    }
    out
}

/// The files `articles` lack by their subjects' `[n/N]` file counter: None when it lists as many as the counter
/// says (or more, or no counter says). With more than one total among the files the counters disagree, and say
/// nothing, as in `parser::is_complete`.
pub fn missing_files(articles: &[ArticleRow]) -> Option<FileGap> {
    let files = files(articles);
    let counters: Vec<(i64, i64)> = files.iter().filter_map(|f| file_counter(f)).collect();
    let totals: std::collections::BTreeSet<i64> = counters.iter().map(|&(_, total)| total).collect();
    let [total] = totals.into_iter().collect::<Vec<_>>()[..] else { return None };
    if !(1..=crate::parser::MAX_PARTS).contains(&total) {
        return None;
    }
    let zero_based = counters.iter().any(|&(index, _)| index == 0);
    let wanted = total + i64::from(zero_based);
    let have = files.len() as i64;
    if have >= wanted {
        return None;
    }
    let present: std::collections::BTreeSet<i64> = counters.iter().map(|&(index, _)| index).collect();
    let first = i64::from(!zero_based);
    let missing = (first..=total).filter(|i| !present.contains(i)).take(GAP_LIST).collect();
    Some(FileGap { files: true, filename: None, total: Some(wanted), have, missing, stray: 0 })
}

/// The <file>s of an NZB, in order: one per filename, and one per article that has no filename.
fn files(articles: &[ArticleRow]) -> Vec<Vec<&ArticleRow>> {
    let mut by_filename: IndexMap<String, Vec<&ArticleRow>> = IndexMap::new();
    for (index, a) in articles.iter().enumerate() {
        let key = a.filename.clone().unwrap_or_else(|| format!("\0{index}"));
        by_filename.entry(key).or_default().push(a);
    }
    by_filename.into_values().collect()
}

/// The articles of the `file`th (1-based) <file> the whole NZB lists.
fn nth_file(articles: &[ArticleRow], file: usize) -> Option<Vec<ArticleRow>> {
    let at = file.checked_sub(1)?;
    files(articles).into_iter().nth(at).map(|f| f.into_iter().cloned().collect())
}

/// Whether `articles` make a whole release: some articles, and every file numbered exactly 1..=total.
pub fn articles_complete(articles: &[ArticleRow]) -> bool {
    !articles.is_empty() && gaps(articles).is_empty()
}

/// A file's `[n/N]` file counter, read the way `parser` read it when the articles were saved. A saved file keeps
/// one subject for all its articles, soo the subject alone can't tell a file counter from a bracket counter
/// `parser` turned into part numbers: it counts files only when `parse_subject` takes it as one (not
/// `name [n/N]`), and the parts the subject's own `(p/t)` counter gives are the parts the file has (bracket
/// parts are single `(1/1)` articles the file got N parts from). Live rows carry each article's own subject,
/// soo all of them have to agree too.
fn file_counter(file: &[&ArticleRow]) -> Option<(i64, i64)> {
    let expected = file.iter().filter_map(|a| a.total_parts).max();
    let mut each = file.iter().map(|a| {
        let parsed = crate::parser::parse_subject(a.subject.as_deref()?)?;
        (Some(parsed.total_parts) == expected).then_some((parsed.file_index?, parsed.file_total?))
    });
    let first = each.next()??;
    each.all(|c| c == Some(first)).then_some(first)
}

/// One line for a gap, for the NZB's head and the log.
fn describe(g: &FileGap) -> String {
    let total = g.total.map_or_else(|| "?".into(), |t| t.to_string());
    let missing: Vec<String> = g.missing.iter().map(i64::to_string).collect();
    let more = if (g.total.unwrap_or(0) - g.have) as usize > g.missing.len() { ",..." } else { "" };
    if g.files {
        let missing = if missing.is_empty() { "-".into() } else { missing.join(",") };
        return format!("{} of {total} files, missing file {missing}{more}", g.have);
    }
    format!(
        "{}: {} of {total} parts, missing {}{more}{}",
        g.filename.as_deref().unwrap_or(""),
        g.have,
        if missing.is_empty() { "-".into() } else { missing.join(",") },
        if g.stray > 0 { format!(", {} unnumbered or out of range", g.stray) } else { String::new() },
    )
}

/// The segments one file lacks, named by its own message-ids: a poster (PowerPost and its kin) that ids every part
/// `part{N}of{T}.{rest}` gives each missing part's id outright. A server that never showed us an article may still
/// have it, and another provider of a downloader's may too, soo the NZB lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InferredFile {
    /// which file of `files(articles)` it is, from 0
    pub file: usize,
    pub filename: String,
    pub total: i64,
    /// the parts added, each a whole row copied from the file's own with its number, id and size put in
    pub rows: Vec<ArticleRow>,
}

impl InferredFile {
    fn describe(&self) -> String {
        let parts: Vec<String> =
            self.rows.iter().take(GAP_LIST).filter_map(|r| r.part).map(|p| p.to_string()).collect();
        let more = if self.rows.len() > GAP_LIST { ",..." } else { "" };
        format!(
            "{}: {} of {} parts listed by the partNofT pattern of its ids, not seen on our servers: {}{more}",
            self.filename,
            self.rows.len(),
            self.total,
            parts.join(",")
        )
    }
}

/// `id` (with or without its `<>`) split as `part{p}of{total}.{rest}`, written just soo (no leading zeros):
/// the rest, or None.
fn pattern_rest(id: &str, p: i64, total: i64) -> Option<&str> {
    // a number exactly as `{n}` writes it
    fn number(s: &str, n: i64) -> Option<&str> {
        let digits = s.bytes().take_while(u8::is_ascii_digit).count();
        let (head, tail) = s.split_at(digits);
        (digits > 0 && !(digits > 1 && head.starts_with('0')) && head.parse() == Ok(n)).then_some(tail)
    }
    let id = id.trim_matches(['<', '>']);
    let rest = number(id.strip_prefix("part")?, p)?;
    let rest = number(rest.strip_prefix("of")?, total)?.strip_prefix('.')?;
    // the rest has to be an id's own tail: something@domain, nothing that could be another counter
    let (left, domain) = rest.split_once('@')?;
    (!left.is_empty() && !domain.is_empty() && !rest.contains(['<', '>', ' '])).then_some(rest)
}

/// The most common of `sizes`, the larger on a tie. None: no sizes.
fn common_size(sizes: impl Iterator<Item = i64>) -> Option<i64> {
    let mut counts: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
    for s in sizes {
        *counts.entry(s).or_default() += 1;
    }
    counts.into_iter().max_by_key(|&(size, n)| (n, size)).map(|(size, _)| size)
}

/// For each file of `articles` whose ids prove the `part{N}of{T}.{rest}` pattern, the parts it lacks. Proven means:
/// a file name; one part count T (2..=MAX_PARTS) on every article; every article numbered 1..=T and its id exactly
/// `part{its number}of{T}.{rest}`, one rest throughout; at least two parts known, and at least half of T, soo a
/// couple of articles can't call up a hundred thousand segments. Any file short of that gets nothing: an id is
/// never guessed past what the file's own ids say.
pub fn inferred_segments(articles: &[ArticleRow]) -> Vec<InferredFile> {
    let mut out = Vec::new();
    for (index, file) in files(articles).into_iter().enumerate() {
        let Some(filename) = file[0].filename.clone() else { continue };
        let Some(total) = file[0].total_parts else { continue };
        if !(2..=crate::parser::MAX_PARTS).contains(&total) || file.iter().any(|a| a.total_parts != Some(total)) {
            continue;
        }
        let mut rest: Option<&str> = None;
        let mut known = std::collections::BTreeSet::new();
        let proven = file.iter().all(|a| {
            let Some(p) = a.part.filter(|p| (1..=total).contains(p)) else { return false };
            let Some(r) = pattern_rest(&a.message_id, p, total) else { return false };
            known.insert(p);
            *rest.get_or_insert(r) == r
        });
        let Some(rest) = rest.filter(|_| proven && known.len() >= 2 && known.len() as i64 * 2 >= total) else {
            continue;
        };
        if known.len() as i64 == total {
            continue;
        }
        // a missing part is as big as the file's parts mostly are; the last is never seen when it's the one missing
        let size = common_size(file.iter().filter(|a| a.part != Some(total)).filter_map(|a| a.bytes))
            .or_else(|| common_size(file.iter().filter_map(|a| a.bytes)));
        let rows = (1..=total)
            .filter(|p| !known.contains(p))
            .map(|p| ArticleRow {
                message_id: format!("<part{p}of{total}.{rest}>"),
                part: Some(p),
                bytes: size,
                ..file[0].clone()
            })
            .collect();
        out.push(InferredFile { file: index, filename, total, rows });
    }
    out
}

/// `articles` with the `inferred` rows put in among their files' own, each such file in part order; every other file
/// keeps its order, soo the NZB lists the same files in the same order.
fn with_inferred(articles: &[ArticleRow], inferred: &[InferredFile]) -> Vec<ArticleRow> {
    let mut out = Vec::with_capacity(articles.len() + inferred.iter().map(|f| f.rows.len()).sum::<usize>());
    for (index, file) in files(articles).into_iter().enumerate() {
        let mut rows: Vec<ArticleRow> = file.into_iter().cloned().collect();
        if let Some(f) = inferred.iter().find(|f| f.file == index) {
            rows.extend(f.rows.iter().cloned());
            rows.sort_by_key(|a| a.part);
        }
        out.extend(rows);
    }
    out
}

/// An NZB and what it lacks: `gaps` is empty for a whole release.
#[derive(Clone, Debug)]
pub struct NzbBuild {
    pub xml: String,
    pub gaps: Vec<FileGap>,
}

impl NzbBuild {
    pub fn complete(&self) -> bool {
        self.gaps.is_empty()
    }
}

/// Render the NZB and check it: a file missing parts is named in the head (`<meta type="x-atlas-incomplete">`),
/// so an NZB short of segments is never passed off as whole. `whole`: these are all the release's files, soo
/// files its file counter names and the NZB lacks are a gap too (not for one file of it, `?file=N`). A release
/// whose saving found it complete with fewer files (a PAR2 set or its own whole video vouching for it, when the
/// counter counted a file never posted) keeps that verdict on files.
pub(crate) fn render_checked(
    release: &ReleaseRow,
    articles: &[ArticleRow],
    groups: &[String],
    whole: bool,
) -> NzbBuild {
    let mut gaps = gaps(articles);
    if whole && !release.complete {
        gaps.extend(missing_files(articles));
    }
    // the gaps are what the store holds; the segments a file's own ids name past that go in the NZB, said so
    let inferred = inferred_segments(articles);
    let mut xml = if inferred.is_empty() {
        render_nzb_in(release, articles, groups)
    } else {
        render_nzb_in(release, &with_inferred(articles, &inferred), groups)
    };
    if !gaps.is_empty() || !inferred.is_empty() {
        let mut meta = String::new();
        for g in &gaps {
            let _ = writeln!(meta, "    <meta type=\"x-atlas-incomplete\">{}</meta>", escape_text(&describe(g)));
        }
        for f in &inferred {
            let _ = writeln!(meta, "    <meta type=\"x-atlas-inferred\">{}</meta>", escape_text(&f.describe()));
        }
        xml = xml.replacen("  </head>", &format!("{meta}  </head>"), 1);
    }
    NzbBuild { xml, gaps }
}

/// Count and log an NZB built with gaps. One for a release flagged complete means the flag and the articles disagree.
pub(crate) fn note_gaps(release: &ReleaseRow, built: &NzbBuild) {
    if built.complete() {
        return;
    }
    let flag = if release.complete { "complete" } else { "incomplete" };
    crate::metrics::names::NZB_GAPS.inc(&[("flag", flag)], 1);
    if release.complete {
        let first = built.gaps.first().map(describe).unwrap_or_default();
        eprintln!(
            "nzb for release {} is flagged complete but {} file(s) miss parts (first: {first})",
            release.id,
            built.gaps.len()
        );
    }
}

pub fn render_nzb(release: &ReleaseRow, articles: &[ArticleRow]) -> String {
    render_nzb_in(release, articles, std::slice::from_ref(&release.group_name))
}

/// The same, for articles found in any of `groups`.
pub(crate) fn render_nzb_in(release: &ReleaseRow, articles: &[ArticleRow], groups: &[String]) -> String {
    let mut x = String::new();
    x.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    x.push_str(
        "<!DOCTYPE nzb PUBLIC \"-//newzBin//DTD NZB 1.1//EN\" \"http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd\">\n",
    );
    x.push_str("<nzb xmlns=\"http://www.newzbin.com/DTD/2003/nzb\">\n");
    let _ = writeln!(x, "  <head>\n    <meta type=\"category\">{}</meta>\n  </head>", escape_text(&release.group_name));

    // one <file> per physical file, articles are already sorted filename then part.
    // Articles without a filename are separate physical files: merging them would
    // make unrelated obfuscated posts look like one incomplete/complete file.
    for file_articles in &files(articles) {
        let first = file_articles[0];
        let _ = writeln!(
            x,
            "  <file poster=\"{}\" date=\"{}\" subject=\"{}\">",
            escape_attr(first.poster.as_deref().unwrap_or("")),
            article_timestamp(first.posted_date.as_deref().unwrap_or("")),
            escape_attr(first.subject.as_deref().unwrap_or("")),
        );
        x.push_str("    <groups>\n");
        for group in groups {
            let _ = writeln!(x, "      <group>{}</group>", escape_text(group));
        }
        x.push_str("    </groups>\n");
        x.push_str("    <segments>\n");

        for a in file_articles {
            // nzb doesnt want the <>
            let _ = writeln!(
                x,
                "      <segment bytes=\"{}\" number=\"{}\">{}</segment>",
                a.bytes.unwrap_or(0),
                a.part.map(|p| p.to_string()).unwrap_or_else(|| "None".into()),
                escape_text(a.message_id.trim_matches(['<', '>'])),
            );
        }

        x.push_str("    </segments>\n  </file>\n");
    }

    x.push_str("</nzb>");
    x
}

pub fn build_nzb(release_id: i64) -> Result<Option<String>> {
    Ok(build_nzb_checked(release_id)?.map(|b| b.xml))
}

/// `build_nzb` with the gaps it found: the release is complete only when they are none.
pub fn build_nzb_checked(release_id: i64) -> Result<Option<NzbBuild>> {
    crate::metrics::timed("nzb.build", || whole_nzb(release_id))
}

/// A release's articles and groups. For a post spread over several groups, or an upload linked by its PAR2 set, that's
/// all of them: it only makes its files with all of them.
/// Also returns the rows the articles came from: the release's own, or every row of its post.
pub(crate) fn inventory(release: &ReleaseRow) -> crate::db::Result<(Vec<ArticleRow>, Vec<String>, Vec<i64>)> {
    let conn = crate::db::open()?;
    if let Some(post) = crate::release_info::whole_post_with(&conn, release)? {
        let mut rows = Vec::new();
        for id in &post.ids {
            rows.push((*id, crate::store::articles(&conn, *id)?));
        }
        // the rows of a linked upload are separate files, where a post cut across groups is one file under one name
        if post.lead.is_some() {
            keep_files_apart(&mut rows);
        }
        let mut all: Vec<ArticleRow> = rows.into_iter().flat_map(|(_, articles)| articles).collect();
        all.sort_by(|a, b| (&a.filename, a.part, &a.message_id).cmp(&(&b.filename, b.part, &b.message_id)));
        return Ok((all, post.groups, post.ids));
    }
    Ok((get_articles(release.id)?, vec![release.group_name.clone()], vec![release.id]))
}

/// A file whose name another row already has keeps its own <file>: the name the NZB groups it by gets its row's id.
fn keep_files_apart(rows: &mut [(i64, Vec<ArticleRow>)]) {
    let mut taken: std::collections::HashMap<Option<String>, i64> = std::collections::HashMap::new();
    for (id, articles) in rows {
        for a in articles {
            if *taken.entry(a.filename.clone()).or_insert(*id) != *id {
                a.filename = Some(format!("{} #{id}", a.filename.as_deref().unwrap_or("")));
            }
        }
    }
}

/// A release's articles with its gaps filled from its twins and file twins (`duplicates`): an article the same bytes
/// were posted as, by anyone, stands in for one this posting lost.
fn filled_inventory(release: &ReleaseRow) -> Result<(Vec<ArticleRow>, Vec<String>)> {
    let (mut articles, groups, rows) = inventory(release)?;
    if !articles.is_empty() {
        let conn = crate::db::open()?;
        let added = crate::duplicates::fill_from_copies_of(&conn, release.id, &rows, &mut articles)?;
        if added > 0 {
            crate::metrics::names::NZB_FILLED.inc(&[], added as u64);
        }
    }
    Ok((articles, groups))
}

fn whole_nzb(release_id: i64) -> Result<Option<NzbBuild>> {
    let Some(release) = get_release(release_id)? else { return Ok(None) };
    let (articles, groups) = filled_inventory(&release)?;

    if articles.is_empty() {
        return Ok(None);
    }

    let built = render_checked(&release, &articles, &groups, true);
    note_gaps(&release, &built);
    Ok(Some(built))
}

/// One file of a release as its own NZB: `file` counts from 1, in the order the release's whole NZB lists them.
pub fn build_nzb_file(release_id: i64, file: usize) -> Result<Option<String>> {
    Ok(build_nzb_file_checked(release_id, file)?.map(|b| b.xml))
}

/// `build_nzb_file` with the gaps it found.
pub fn build_nzb_file_checked(release_id: i64, file: usize) -> Result<Option<NzbBuild>> {
    crate::metrics::timed("nzb.build_file", || one_file_nzb(release_id, file))
}

/// One file of a release, as its listing and `t=files` show it: where the whole NZB puts it, from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedFile {
    pub n: usize,
    /// None: its articles had no file name, soo only `n` picks it
    pub name: Option<String>,
    /// as posted: the articles' bytes, yEnc and all
    pub bytes: i64,
    pub articles: usize,
}

/// The files of `articles` in the order the whole NZB lists them.
fn listed(articles: &[ArticleRow]) -> Vec<ListedFile> {
    files(articles)
        .into_iter()
        .enumerate()
        .map(|(at, file)| ListedFile {
            n: at + 1,
            name: file.first().and_then(|a| a.filename.clone()),
            bytes: file.iter().filter_map(|a| a.bytes).sum(),
            articles: file.len(),
        })
        .collect()
}

/// The files a release's whole NZB lists, gaps filled from its copies as `t=get` fills them. None: no such release.
pub fn file_list(release_id: i64) -> Result<Option<Vec<ListedFile>>> {
    crate::metrics::timed("nzb.file_list", || {
        let Some(release) = get_release(release_id)? else { return Ok(None) };
        let (articles, _) = filled_inventory(&release)?;
        Ok(Some(listed(&articles)))
    })
}

/// The articles of every file named `name` in the whole NZB of `articles`: the one file, or, in an upload linked by
/// its PAR2 set, each row's file of that name, which the NZB keeps apart as `name #id` and a release page lists as one.
fn files_named(articles: &[ArticleRow], name: &str) -> Vec<ArticleRow> {
    let apart = format!("{name} #");
    files(articles)
        .into_iter()
        .filter(|file| {
            file.first().and_then(|a| a.filename.as_deref()).is_some_and(|f| {
                f == name
                    || f.strip_prefix(&apart).is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .flatten()
        .cloned()
        .collect()
}

/// One file of a release as its own NZB, picked by its name: a release page links each file this way, since its
/// list is sorted by name and not in the NZB's order.
pub fn build_nzb_named_checked(release_id: i64, name: &str) -> Result<Option<NzbBuild>> {
    crate::metrics::timed("nzb.build_file", || {
        let Some(release) = get_release(release_id)? else { return Ok(None) };
        let (articles, groups) = filled_inventory(&release)?;
        let one = files_named(&articles, name);
        if name.is_empty() || one.is_empty() {
            return Ok(None);
        }
        let built = render_checked(&release, &one, &groups, false);
        note_gaps(&release, &built);
        Ok(Some(built))
    })
}

fn one_file_nzb(release_id: i64, file: usize) -> Result<Option<NzbBuild>> {
    let Some(release) = get_release(release_id)? else { return Ok(None) };
    let (articles, groups) = filled_inventory(&release)?;
    // the same files, in the same order, as the whole NZB
    let Some(one) = nth_file(&articles, file) else { return Ok(None) };
    let built = render_checked(&release, &one, &groups, false);
    note_gaps(&release, &built);
    Ok(Some(built))
}

/// Write the nzb for a release, into `output_dir` or the current dir.
pub fn generate_nzb(release_id: i64, output_dir: Option<&Path>) -> Result<PathBuf> {
    let release = get_release(release_id)?.ok_or_else(|| {
        ui::error("Release not found");
        anyhow!("release not found")
    })?;

    let Some(built) = build_nzb_checked(release_id)? else {
        ui::error("No articles found");
        return Err(anyhow!("no articles found"));
    };
    for g in &built.gaps {
        ui::warn(&format!("incomplete: {}", describe(g)));
    }
    let content = built.xml;

    let filename = nzb_filename(&release.name, Some(release.id));
    let target = match output_dir {
        Some(dir) => dir.join(&filename),
        None => PathBuf::from(&filename),
    };

    if let Err(e) = write_atomic(&target, content) {
        ui::error(&format!("couldnt save nzb: {e}"));
        return Err(e.into());
    }

    ui::success(&format!("Saved {}", target.display()));
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_listed_and_picked_in_the_nzbs_order() {
        let art = |file: Option<&str>, part: i64, bytes: i64| ArticleRow {
            message_id: format!("<{file:?}{part}@x>"),
            filename: file.map(String::from),
            part: Some(part),
            total_parts: Some(2),
            bytes: Some(bytes),
            subject: None,
            poster: None,
            posted_date: None,
        };
        let articles =
            [art(Some("b.mkv"), 1, 10), art(Some("a.par2"), 1, 3), art(Some("b.mkv"), 2, 5), art(None, 1, 7)];
        let list = listed(&articles);
        assert_eq!(
            list,
            vec![
                ListedFile { n: 1, name: Some("b.mkv".into()), bytes: 15, articles: 2 },
                ListedFile { n: 2, name: Some("a.par2".into()), bytes: 3, articles: 1 },
                ListedFile { n: 3, name: None, bytes: 7, articles: 1 },
            ]
        );
        assert_eq!(files_named(&articles, "a.par2").len(), 1);
        assert!(files_named(&articles, "c.nfo").is_empty());
        assert_eq!(files_named(&articles, "b.mkv").len(), 2);
        // a linked upload's second row of the same name, kept apart in the NZB, comes along
        let linked = [
            art(Some("v.par2"), 1, 1),
            art(Some("v.par2 #42"), 1, 1),
            art(Some("v.par2 #x"), 1, 1),
            art(Some("v.par2 #"), 1, 1),
        ];
        assert_eq!(files_named(&linked, "v.par2").len(), 2);
    }

    #[test]
    fn filenames_are_safe() {
        assert_eq!(nzb_filename("a/b:c?.mkv", Some(7)), "a_b_c_.mkv.7.nzb");
        assert_eq!(nzb_filename(" .. ", None), "release.nzb");
        assert_eq!(nzb_filename("x\u{1}y", Some(0)), "x_y.nzb");
    }

    #[test]
    fn same_named_files_of_a_linked_upload_stay_apart() {
        let art = |file: &str, part: i64| ArticleRow {
            message_id: format!("<{file}{part}@x>"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(2),
            bytes: Some(1),
            subject: None,
            poster: None,
            posted_date: None,
        };
        let mut rows = vec![
            (8, vec![art("x.bin", 1), art("x.bin", 2)]),
            (16, vec![art("x.bin", 1), art("x.bin", 2)]),
            (24, vec![art("y.bin", 1)]),
        ];
        keep_files_apart(&mut rows);
        let names = |row: usize| rows[row].1.iter().map(|a| a.filename.clone().unwrap()).collect::<Vec<_>>();
        assert_eq!(names(0), ["x.bin", "x.bin"]);
        assert_eq!(names(1), ["x.bin #16", "x.bin #16"]);
        assert_eq!(names(2), ["y.bin"]);
    }

    #[test]
    fn renders_files_and_segments() {
        let release = ReleaseRow {
            id: 1,
            name: "Rel".into(),
            group_name: "alt.binaries.test".into(),
            poster: None,
            posted_date: None,
            size: Some(3),
            complete: true,
            parts: Some(3),
        };
        let art = |file: &str, part: i64, id: &str| ArticleRow {
            message_id: id.into(),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(2),
            bytes: Some(1),
            subject: Some(format!("\"{file}\" yEnc ({part}/2)")),
            poster: Some("me <me@x>".into()),
            posted_date: Some("2026-10-02 10:00:00".into()),
        };
        let xml =
            render_nzb(&release, &[art("a.rar", 1, "<a1@x>"), art("a.rar", 2, "<a2@x>"), art("b.rar", 1, "<b1@x>")]);

        assert_eq!(xml.matches("<file ").count(), 2);
        assert!(xml.contains("<segment bytes=\"1\" number=\"2\">a2@x</segment>"));
        assert!(xml.contains("poster=\"me &lt;me@x&gt;\""));
        assert!(xml.contains("subject=\"&quot;a.rar&quot; yEnc (1/2)\""));
        assert!(xml.contains("<meta type=\"category\">alt.binaries.test</meta>"));
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE nzb"));
    }

    fn part(file: &str, part: i64, total: i64) -> ArticleRow {
        ArticleRow {
            message_id: format!("<{file}.{part}@x>"),
            filename: Some(file.into()),
            part: Some(part),
            total_parts: Some(total),
            bytes: Some(700_000),
            subject: Some(format!("\"{file}\" yEnc ({part}/{total})")),
            poster: None,
            posted_date: None,
        }
    }

    fn release() -> ReleaseRow {
        ReleaseRow {
            id: 8,
            name: "Rel".into(),
            group_name: "alt.binaries.test".into(),
            complete: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_file_missing_a_part_is_not_complete() {
        let articles = [part("a.mkv", 1, 4), part("a.mkv", 2, 4), part("a.mkv", 4, 4)];
        assert!(!articles_complete(&articles));
        let gaps = gaps(&articles);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].missing, [3]);
        assert_eq!((gaps[0].have, gaps[0].total), (3, Some(4)));
    }

    #[test]
    fn a_whole_file_is_complete() {
        let articles: Vec<_> = (1..=5).map(|p| part("a.mkv", p, 5)).chain([part("a.par2", 1, 1)]).collect();
        assert!(articles_complete(&articles));
        let built = render_checked(&release(), &articles, &["alt.binaries.test".into()], true);
        assert!(built.complete());
        assert!(!built.xml.contains("x-atlas-incomplete"));
    }

    #[test]
    fn the_nzb_build_names_the_gap() {
        let articles = [part("a.mkv", 1, 4), part("a.mkv", 2, 4), part("a.mkv", 4, 4), part("b.par2", 1, 1)];
        let built = render_checked(&release(), &articles, &["alt.binaries.test".into()], true);
        assert!(!built.complete());
        assert_eq!(built.gaps.len(), 1, "only a.mkv lacks a part");
        assert!(
            built.xml.contains("<meta type=\"x-atlas-incomplete\">a.mkv: 3 of 4 parts, missing 3</meta>\n  </head>"),
            "{}",
            built.xml
        );
        // the segments it has are still all listed
        assert_eq!(built.xml.matches("<segment ").count(), 4);
    }

    #[test]
    fn a_lone_late_segment_is_a_gap_not_a_file() {
        // one article: segment 17 of an obfuscated file
        let gaps = gaps(&[part("x", 17, 40)]);
        assert_eq!(gaps[0].have, 1);
        assert_eq!(gaps[0].missing.len(), GAP_LIST);
        assert!(describe(&gaps[0]).ends_with(",..."));
    }

    #[test]
    fn unnumbered_or_out_of_range_parts_are_gaps() {
        let mut none = part("a", 1, 1);
        none.part = None;
        assert!(!articles_complete(&[part("a", 1, 2), part("a", 2, 2), part("a", 3, 2)]));
        assert!(!articles_complete(&[none]));
        let mut untotalled = part("a", 1, 1);
        untotalled.total_parts = None;
        assert!(!articles_complete(&[untotalled]));
        assert!(!articles_complete(&[]));
    }

    fn counted(file: &str, index: i64, of: i64, part_no: i64, total: i64) -> ArticleRow {
        let mut a = part(file, part_no, total);
        a.subject = Some(format!("[{index}/{of}] \"{file}\" yEnc ({part_no}/{total})"));
        a
    }

    #[test]
    fn a_file_the_counter_names_and_the_nzb_lacks_is_a_gap() {
        // [1/2] is whole, [2/2] never came
        let articles = [counted("a.mkv", 1, 2, 1, 1)];
        assert!(articles_complete(&articles), "every file it has is whole");
        let gap = missing_files(&articles).expect("file 2 is missing");
        assert!(gap.files);
        assert_eq!((gap.have, gap.total, gap.missing.clone()), (1, Some(2), vec![2]));
        assert_eq!(describe(&gap), "1 of 2 files, missing file 2");
        let incomplete = ReleaseRow { complete: false, ..release() };
        let built = render_checked(&incomplete, &articles, &["alt.binaries.test".into()], true);
        assert!(!built.complete());
        assert!(
            built.xml.contains("<meta type=\"x-atlas-incomplete\">1 of 2 files, missing file 2</meta>"),
            "{}",
            built.xml
        );
        // one file of it, `?file=1`, is whole
        assert!(render_checked(&incomplete, &articles, &["alt.binaries.test".into()], false).complete());
        // saving found it whole with fewer files (vouched by a PAR2 set): that verdict stands
        assert!(render_checked(&release(), &articles, &["alt.binaries.test".into()], true).complete());
    }

    #[test]
    fn file_counters_that_agree_with_the_nzb_are_no_gap() {
        let articles = [counted("a.mkv", 1, 2, 1, 2), counted("a.mkv", 1, 2, 2, 2), counted("a.par2", 2, 2, 1, 1)];
        assert_eq!(missing_files(&articles), None);
        // counted from 0: [0/2] is a third file
        let articles = [counted("a.nfo", 0, 2, 1, 1), counted("a.mkv", 1, 2, 1, 1), counted("a.par2", 2, 2, 1, 1)];
        assert_eq!(missing_files(&articles), None);
        // without a [0/2] nothing says it counts from 0
        assert_eq!(missing_files(&articles[1..]), None);
        let gap = missing_files(&articles[..2]).expect("file 2 is missing");
        assert_eq!(gap.missing, [2]);
    }

    #[test]
    fn a_bracket_counter_that_numbers_parts_is_not_a_file_counter() {
        // one file posted as single-article bracketed parts: [1/3]..[3/3], each (1/1), read as parts 1..3
        let articles: Vec<_> = (1..=3)
            .map(|p| {
                let mut a = counted("x", p, 3, p, 3);
                a.subject = Some(format!("x [{p}/3] yEnc (1/1)"));
                a
            })
            .collect();
        assert!(articles_complete(&articles));
        assert_eq!(missing_files(&articles), None);
        // read back from storage every article has the file's one subject, and part 3 never came
        let saved: Vec<_> = articles[..2]
            .iter()
            .cloned()
            .map(|mut a| {
                a.subject = Some("[1/3] \"x\" yEnc (1/1)".into());
                a
            })
            .collect();
        assert_eq!(missing_files(&saved), None, "a part gap, not a file gap");
        assert_eq!(gaps(&saved)[0].missing, [3]);
        // nor `name [n/N]`, which is only ever a part counter
        let short: Vec<_> = saved
            .into_iter()
            .map(|mut a| {
                a.subject = Some("abcdefghijklmnop [1/3]".into());
                a
            })
            .collect();
        assert_eq!(missing_files(&short), None);
        // and counters that disagree say nothing
        assert_eq!(missing_files(&[counted("a", 1, 3, 1, 1), counted("b", 2, 4, 1, 1)]), None);
    }

    #[test]
    fn zero_based_parts_are_complete() {
        let articles: Vec<_> = (0..4).map(|p| part("a.mkv", p, 4)).collect();
        assert!(articles_complete(&articles));
        assert!(render_checked(&release(), &articles, &["alt.binaries.test".into()], true).complete());
    }

    #[test]
    fn unnamed_articles_are_not_merged() {
        let mut first = part("a", 1, 1);
        first.filename = None;
        let mut second = part("b", 1, 1);
        second.filename = None;
        let xml = render_nzb(&release(), &[first, second]);
        assert_eq!(xml.matches("<file ").count(), 2);
    }

    #[test]
    fn file_n_is_the_nth_file_of_the_whole_nzb() {
        let mut first = part("x", 1, 1);
        first.filename = None;
        first.message_id = "first@x".into();
        let mut second = part("y", 1, 1);
        second.filename = None;
        second.message_id = "second@x".into();
        let third = part("c.mkv", 1, 1);
        let articles = vec![first, second, third];
        let whole = render_nzb(&release(), &articles);
        assert_eq!(whole.matches("<file ").count(), 3);
        let ids = |f: usize| nth_file(&articles, f).unwrap().into_iter().map(|a| a.message_id).collect::<Vec<_>>();
        assert_eq!(ids(1), vec!["first@x"]);
        assert_eq!(ids(2), vec!["second@x"]);
        assert_eq!(ids(3), vec![articles[2].message_id.clone()]);
        assert!(nth_file(&articles, 4).is_none());
        assert!(nth_file(&articles, 0).is_none());
    }

    fn powerpost(file: &str, part: i64, total: i64, token: &str, bytes: i64) -> ArticleRow {
        ArticleRow {
            message_id: format!("<part{part}of{total}.{token}@bzh29>"),
            bytes: Some(bytes),
            ..counted(file, 1, 1, part, total)
        }
    }

    fn post(file: &str, total: i64, have: &[i64], token: &str) -> Vec<ArticleRow> {
        have.iter().map(|&p| powerpost(file, p, total, token, if p == total { 300 } else { 640 })).collect()
    }

    #[test]
    fn a_proven_pattern_lists_the_missing_parts() {
        let articles = post("a.rar", 6, &[1, 2, 4, 6], "tok$A");
        let inferred = inferred_segments(&articles);
        assert_eq!(inferred.len(), 1);
        let ids: Vec<&str> = inferred[0].rows.iter().map(|r| r.message_id.as_str()).collect();
        assert_eq!(ids, ["<part3of6.tok$A@bzh29>", "<part5of6.tok$A@bzh29>"]);
        assert!(inferred[0].rows.iter().all(|r| r.bytes == Some(640) && r.filename.as_deref() == Some("a.rar")));

        let built = render_checked(&release(), &articles, &["alt.binaries.test".into()], true);
        // the store still lacks them: the gap stays, and the NZB says which parts it filled in
        assert!(!built.complete());
        assert!(built.xml.contains("<meta type=\"x-atlas-incomplete\">a.rar: 4 of 6 parts, missing 3,5</meta>"));
        assert!(built.xml.contains("<meta type=\"x-atlas-inferred\">a.rar: 2 of 6 parts"));
        assert!(built.xml.contains("not seen on our servers: 3,5</meta>"));
        let numbers: Vec<&str> = built.xml.match_indices("number=\"").map(|(i, _)| &built.xml[i + 8..i + 9]).collect();
        assert_eq!(numbers, ["1", "2", "3", "4", "5", "6"]);
        assert!(built.xml.contains("<segment bytes=\"640\" number=\"3\">part3of6.tok$A@bzh29</segment>"));
        assert!(built.xml.contains("<segment bytes=\"300\" number=\"6\">part6of6.tok$A@bzh29</segment>"));
    }

    #[test]
    fn a_missing_last_part_takes_the_common_size() {
        let inferred = inferred_segments(&post("a.rar", 4, &[1, 2], "t"));
        let sizes: Vec<(Option<i64>, Option<i64>)> = inferred[0].rows.iter().map(|r| (r.part, r.bytes)).collect();
        assert_eq!(sizes, [(Some(3), Some(640)), (Some(4), Some(640))]);
    }

    #[test]
    fn only_a_proven_pattern_is_filled_in() {
        let none = |articles: Vec<ArticleRow>| assert!(inferred_segments(&articles).is_empty(), "{articles:?}");
        // two tokens in one file
        let mut mixed = post("a.rar", 6, &[1, 2], "one");
        mixed.extend(post("a.rar", 6, &[4], "two"));
        none(mixed);
        // an id whose number isn't the segment's
        let mut off = post("a.rar", 6, &[1, 2], "t");
        off[1].part = Some(3);
        none(off);
        // an id whose total isn't the file's
        let mut total = post("a.rar", 6, &[1, 2], "t");
        total[0].message_id = "<part1of7.t@bzh29>".into();
        none(total);
        // leading zeros aren't the pattern
        let mut zeros = post("a.rar", 6, &[1, 2], "t");
        zeros[0].message_id = "<part01of6.t@bzh29>".into();
        none(zeros);
        // one known part proves nothing
        none(post("a.rar", 6, &[2], "t"));
        // nor do two of a hundred thousand
        none(post("a.rar", 100_000, &[1, 2], "t"));
        none(post("a.rar", 7, &[1, 2, 3], "t"));
        // ids of another shape
        none(vec![part("a.rar", 1, 3), part("a.rar", 3, 3)]);
        // no file name
        let mut unnamed = post("a.rar", 6, &[1, 2], "t");
        unnamed.iter_mut().for_each(|a| a.filename = None);
        none(unnamed);
        // a whole file needs nothing
        none(post("a.rar", 3, &[1, 2, 3], "t"));
        // no domain
        let mut bare = post("a.rar", 6, &[1, 2], "t");
        bare.iter_mut().for_each(|a| a.message_id = a.message_id.replace("@bzh29", ""));
        none(bare);
    }

    #[test]
    fn other_files_and_known_segments_are_left_as_they_are() {
        let mut articles = vec![part("z.nfo", 1, 2)];
        articles.extend(post("a.rar", 4, &[1, 3, 4], "t"));
        let plain = render_nzb(&release(), &articles);
        let built = render_checked(&release(), &articles, &["alt.binaries.test".into()], false);
        assert!(built.xml.find("z.nfo").unwrap() < built.xml.find("a.rar").unwrap());
        for line in plain.lines().filter(|l| l.contains("<segment")) {
            assert!(built.xml.contains(line), "{line}");
        }
        assert_eq!(built.xml.matches("<segment").count(), plain.matches("<segment").count() + 1);
        assert!(built.xml.contains("part2of4.t@bzh29"));
        // the listing still counts what the store holds
        assert_eq!(listed(&articles).iter().map(|f| f.articles).sum::<usize>(), 4);
    }
}
