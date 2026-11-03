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

    // one <file> per physical file, articles are already sorted filename then part
    let mut by_filename: IndexMap<Option<&str>, Vec<&ArticleRow>> = IndexMap::new();
    for a in articles {
        by_filename.entry(a.filename.as_deref()).or_default().push(a);
    }

    for file_articles in by_filename.values() {
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
    crate::metrics::timed("nzb.build", || whole_nzb(release_id))
}

/// A release's articles and groups. For a post spread over several groups, or an upload linked by its PAR2 set, that's
/// all of them: it only makes its files with all of them.
pub(crate) fn inventory(release: &ReleaseRow) -> crate::db::Result<(Vec<ArticleRow>, Vec<String>)> {
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
        return Ok((all, post.groups));
    }
    Ok((get_articles(release.id)?, vec![release.group_name.clone()]))
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
    let (mut articles, groups) = inventory(release)?;
    if !articles.is_empty() {
        let conn = crate::db::open()?;
        let added = crate::duplicates::fill_from_copies_with(&conn, release.id, &mut articles)?;
        if added > 0 {
            crate::metrics::names::NZB_FILLED.inc(&[], added as u64);
        }
    }
    Ok((articles, groups))
}

fn whole_nzb(release_id: i64) -> Result<Option<String>> {
    let Some(release) = get_release(release_id)? else { return Ok(None) };
    let (articles, groups) = filled_inventory(&release)?;

    if articles.is_empty() {
        return Ok(None);
    }

    Ok(Some(render_nzb_in(&release, &articles, &groups)))
}

/// One file of a release as its own NZB: `file` counts from 1, in the order the release's whole NZB lists them.
pub fn build_nzb_file(release_id: i64, file: usize) -> Result<Option<String>> {
    crate::metrics::timed("nzb.build_file", || one_file_nzb(release_id, file))
}

fn one_file_nzb(release_id: i64, file: usize) -> Result<Option<String>> {
    let Some(release) = get_release(release_id)? else { return Ok(None) };
    let (articles, groups) = filled_inventory(&release)?;
    // sorted by filename, soo each file's articles are one run
    let mut runs = articles.chunk_by(|a, b| a.filename == b.filename);
    let Some(one) = file.checked_sub(1).and_then(|at| runs.nth(at)) else { return Ok(None) };
    Ok(Some(render_nzb_in(&release, one, &groups)))
}

/// Write the nzb for a release, into `output_dir` or the current dir.
pub fn generate_nzb(release_id: i64, output_dir: Option<&Path>) -> Result<PathBuf> {
    let release = get_release(release_id)?.ok_or_else(|| {
        ui::error("Release not found");
        anyhow!("release not found")
    })?;

    let Some(content) = build_nzb(release_id)? else {
        ui::error("No articles found");
        return Err(anyhow!("no articles found"));
    };

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
}
