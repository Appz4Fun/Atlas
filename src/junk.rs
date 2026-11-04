//! Releases nobody here wants, gone for good: anything that carries a program
//! (an installer, a script, a `.mkv.exe`, or one inside an archive) and what
//! the categories call PC. A removed release leaves its name and group in
//! `junk_releases`, soo the articles still to come for it are never saved again.

use rusqlite::{Connection, OptionalExtension, Result, params};

use crate::postprocess::schema::ShardTable;

pub const JUNK_RELEASES: ShardTable = ShardTable {
    name: "junk_releases",
    create: "create table if not exists junk_releases (
        name       TEXT NOT NULL,
        group_name TEXT NOT NULL,
        reason     TEXT NOT NULL,
        at         INTEGER NOT NULL,
        primary key (name, group_name)
    ) without rowid",
    indexes: &[],
    release_key: None,
    added: &[],
};

/// The collection keys of removed posts (`collections.rs`): a post's later slices can carry another name, and find
/// their release by its key, which a removed release no longer answers to.
pub const JUNK_COLLECTIONS: ShardTable = ShardTable {
    name: "junk_collections",
    create: "create table if not exists junk_collections (key INTEGER PRIMARY KEY) without rowid",
    indexes: &[],
    release_key: None,
    added: &[],
};

pub const EXECUTABLE: &str = "executable";
pub const EXECUTABLE_INSIDE: &str = "executable_inside";
pub const PC: &str = "pc_category";

/// What runs on a computer or a phone. Not `.bin`, `.iso` or `.img`: discs are media too.
const PROGRAMS: &[&str] = &[
    "exe",
    "msi",
    "msp",
    "msu",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "bat",
    "cmd",
    "com",
    "scr",
    "pif",
    "cpl",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "ps1",
    "psm1",
    "lnk",
    "jar",
    "dll",
    "sys",
    "reg",
    "gadget",
    "application",
    "apk",
    "xapk",
    "ipa",
    "dmg",
    "pkg",
    "deb",
    "rpm",
    "run",
    "sh",
    "command",
    "app",
];

/// A file name that is a program, by its last extension.
pub fn is_executable(filename: &str) -> bool {
    let name = filename.trim().trim_end_matches(['"', '\'', ']', ')']).trim_end();
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 11 => {
            PROGRAMS.iter().any(|p| p.eq_ignore_ascii_case(ext))
        }
        _ => false,
    }
}

/// Video a PC match can't take away: a film posted to a software group is still a film.
pub fn is_video(filename: &str) -> bool {
    let ext = filename.trim().trim_end_matches('"').rsplit_once('.').map_or("", |(_, e)| e);
    ["mkv", "mp4", "m4v", "avi", "ts", "m2ts", "mts", "wmv", "mov", "mpg", "mpeg", "webm", "vob"]
        .iter()
        .any(|v| v.eq_ignore_ascii_case(ext))
}

pub fn is_pc(category: i64) -> bool {
    crate::category::root_of(category) == 4000
}

/// Whether a save must leave this release alone.
pub fn is_junk(conn: &Connection, name: &str, group: &str) -> Result<bool> {
    conn.prepare_cached("select 1 from junk_releases where name = ?1 and group_name = ?2")?.exists(params![name, group])
}

/// Whether a save must leave this collection alone.
pub fn is_junk_collection(conn: &Connection, key: i64) -> Result<bool> {
    conn.prepare_cached("select 1 from junk_collections where key = ?1")?.exists([key])
}

pub fn remember_collection(conn: &Connection, key: i64) -> Result<()> {
    conn.prepare_cached("insert or ignore into junk_collections (key) values (?1)")?.execute([key])?;
    Ok(())
}

/// Whether the release has a video file now, as its stored files say.
pub fn has_video(conn: &Connection, release_id: i64) -> Result<bool> {
    let mut stmt = conn.prepare_cached("select filename from files where release_id = ?")?;
    let mut rows = stmt.query([release_id])?;
    while let Some(r) = rows.next()? {
        if is_video(r.get_ref(0)?.as_str()?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A PC category row left on a release that has video: there is no PC any more, see the category step.
pub fn stale_pc(conn: &Connection, release_id: i64, has_video: bool) -> Result<bool> {
    if !has_video {
        return Ok(false);
    }
    let category: Option<i64> = conn
        .prepare_cached("select category from release_category where release_id = ?")?
        .query_row([release_id], |r| r.get(0))
        .optional()?;
    Ok(category.is_some_and(is_pc))
}

pub const DROP_CATEGORY: &str = "delete from release_category where release_id = ?1";

/// Whether a PC match keeps its release: only one with a video file.
pub fn pc_is_junk(filenames: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    !filenames.into_iter().any(|f| is_video(f.as_ref()))
}

/// Why a stored release is junk, from what is stored about it: its posted
/// files, the names inside its archives and PAR2 sets, and its category.
pub fn stored_reason(conn: &Connection, release_id: i64) -> Result<Option<&'static str>> {
    let names: Vec<String> = conn
        .prepare_cached("select filename from files where release_id = ?")?
        .query_map([release_id], |r| r.get(0))?
        .collect::<Result<_>>()?;
    if names.iter().any(|n| is_executable(n)) {
        return Ok(Some(EXECUTABLE));
    }
    inner_or_pc(conn, release_id, names.iter().any(|n| is_video(n)))
}

/// The part of `stored_reason` past the posted files, for a caller that has read those.
pub fn inner_or_pc(conn: &Connection, release_id: i64, has_video: bool) -> Result<Option<&'static str>> {
    // what archives list, and what a PAR2 set describes: past the names quota only the set's rows are kept
    for sql in [
        "select name from archive_inner_files where release_id = ?",
        "select name from par2_files where release_id = ?",
    ] {
        let mut inner = conn.prepare_cached(sql)?;
        let mut rows = inner.query([release_id])?;
        while let Some(r) = rows.next()? {
            if is_executable(r.get_ref(0)?.as_str()?) {
                return Ok(Some(EXECUTABLE_INSIDE));
            }
        }
    }
    let category: Option<i64> = conn
        .prepare_cached("select category from release_category where release_id = ?")?
        .query_row([release_id], |r| r.get(0))
        .optional()?;
    if category.is_some_and(is_pc) && !has_video {
        return Ok(Some(PC));
    }
    Ok(None)
}

/// The releases a shard's stored facts already call junk, without reading `files`: a PC category, or a program among
/// the names inside archives and PAR2 sets. For queueing; each visit decides again.
pub fn known_junk(conn: &Connection) -> Result<Vec<i64>> {
    let mut ids: Vec<i64> = conn
        .prepare("select release_id from release_category where category between 4000 and 4999")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_>>()?;
    for sql in ["select release_id, name from archive_inner_files", "select release_id, name from par2_files"] {
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if is_executable(r.get_ref(1)?.as_str()?) {
                ids.push(r.get(0)?);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Note `name` in `group` as junk, in the caller's transaction.
pub fn remember(conn: &Connection, name: &str, group: &str, reason: &str, now: i64) -> Result<()> {
    conn.prepare_cached("insert or ignore into junk_releases (name, group_name, reason, at) values (?1, ?2, ?3, ?4)")?
        .execute(params![name, group, reason, now])?;
    Ok(())
}

/// A release and everything of its gone, its key kept as junk, in the caller's
/// transaction. False: no such release.
pub fn remove(conn: &Connection, release_id: i64, reason: &str, now: i64) -> Result<bool> {
    let key: Option<(String, String)> = conn
        .prepare_cached("select name, group_name from releases where id = ?")?
        .query_row([release_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((name, group)) = key else { return Ok(false) };
    // a PC verdict rests on there being no video, and one may have come since the step looked
    if reason == PC && has_video(conn, release_id)? {
        return Ok(false);
    }
    remember(conn, &name, &group, reason, now)?;
    conn.prepare_cached(
        "insert or ignore into junk_collections (key) select key from collections where release_id = ?",
    )?
    .execute([release_id])?;
    let deleted = crate::store::delete_release(conn, release_id)?;
    let mut less = conn.prepare_cached("update meta set value = max(value - ?, 0) where key = ?")?;
    less.execute(params![deleted.articles, "articles"])?;
    less.execute(params![1, "releases"])?;
    crate::metrics::names::JUNK_REMOVED.inc(&[("reason", reason)], 1);
    Ok(true)
}

/// `name` in `group` as junk, and its row gone if there is one.
pub fn remove_by_key(conn: &Connection, name: &str, group: &str, reason: &str, now: i64) -> Result<()> {
    let id: Option<i64> = conn
        .prepare_cached("select id from releases where name = ?1 and group_name = ?2")?
        .query_row(params![name, group], |r| r.get(0))
        .optional()?;
    match id {
        Some(id) => {
            remove(conn, id, reason, now)?;
        }
        None => remember(conn, name, group, reason, now)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};
    use crate::{db, store};

    const G: &str = "alt.binaries.t";

    fn article(name: &str, n: usize) -> Article {
        Article {
            message_id: format!("<{name}.{n}@x>"),
            subject: format!("\"{name}\" yEnc (1/1)"),
            filename: Some(name.into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        }
    }

    fn post(release: &str, files: &[&str]) -> Release {
        let articles = files.iter().enumerate().map(|(n, f)| article(f, n)).collect();
        Release { name: release.into(), group: G.into(), articles, ..Default::default() }
    }

    fn shard() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("atlas.db");
        db::create_db_at(&main).unwrap();
        (dir, main)
    }

    fn open(main: &std::path::Path) -> Connection {
        db::open_shard(&store::shard_path(main, store::shard_of(G))).unwrap()
    }

    fn counts(conn: &Connection) -> (i64, i64, i64) {
        let meta = |k: &str| conn.query_row("select value from meta where key = ?", [k], |r| r.get(0)).unwrap();
        let rows = conn.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
        (rows, meta("releases"), meta("articles"))
    }

    #[test]
    fn a_post_with_a_program_is_never_saved_and_nothing_after_it_is() {
        let (_dir, main) = shard();
        store::save(&main, &[post("Free.Movie.2024", &["Free.Movie.2024.mkv.exe", "Free.Movie.2024.nfo"])]).unwrap();
        let conn = open(&main);
        assert_eq!(counts(&conn), (0, 0, 0));
        assert!(is_junk(&conn, "Free.Movie.2024", G).unwrap());
        drop(conn);
        store::save(&main, &[post("Free.Movie.2024", &["Free.Movie.2024.part01.rar"])]).unwrap();
        assert_eq!(counts(&open(&main)), (0, 0, 0), "what comes later for it is left out too");
        store::save(&main, &[post("Real.Movie.2024", &["Real.Movie.2024.mkv"])]).unwrap();
        assert_eq!(counts(&open(&main)), (1, 1, 1));
    }

    #[test]
    fn a_program_arriving_late_takes_the_release_and_its_counts() {
        let (_dir, main) = shard();
        store::save(&main, &[post("Some.Thing", &["a.part01.rar", "a.part02.rar"])]).unwrap();
        store::save(&main, &[post("Other.Thing", &["b.mkv"])]).unwrap();
        assert_eq!(counts(&open(&main)), (2, 2, 3));
        store::save(&main, &[post("Some.Thing", &["setup.exe"])]).unwrap();
        let conn = open(&main);
        assert_eq!(counts(&conn), (1, 1, 1));
        let reason: String = conn.query_row("select reason from junk_releases", [], |r| r.get(0)).unwrap();
        assert_eq!(reason, EXECUTABLE);
    }

    #[test]
    fn what_is_stored_tells_a_release_is_junk() {
        let (_dir, main) = shard();
        store::save(&main, &[post("Packed", &["x.part01.rar"]), post("Film", &["Film.2024.mkv"])]).unwrap();
        let conn = open(&main);
        let id =
            |n: &str| -> i64 { conn.query_row("select id from releases where name = ?", [n], |r| r.get(0)).unwrap() };
        let (packed, film) = (id("Packed"), id("Film"));
        assert_eq!(stored_reason(&conn, packed).unwrap(), None);
        conn.execute("insert into release_category (release_id, category) values (?, 4010)", [packed]).unwrap();
        conn.execute("insert into release_category (release_id, category) values (?, 4010)", [film]).unwrap();
        assert_eq!(stored_reason(&conn, packed).unwrap(), Some(PC));
        assert_eq!(stored_reason(&conn, film).unwrap(), None, "a PC match with video stays");
        conn.execute(
            "insert into archive_inner_files (release_id, seq, name, size, source) values (?, 0, 'bin/Crack.exe', 1, 1)",
            [film],
        )
        .unwrap();
        assert_eq!(stored_reason(&conn, film).unwrap(), Some(EXECUTABLE_INSIDE));
        assert_eq!(known_junk(&conn).unwrap(), {
            let mut both = vec![packed, film];
            both.sort_unstable();
            both
        });
        assert!(stale_pc(&conn, film, true).unwrap());
        assert!(!stale_pc(&conn, packed, false).unwrap());
        assert!(!remove(&conn, film, PC, 5).unwrap(), "video that came since the step looked keeps it");
        conn.execute("insert into collections (key, release_id) values (77, ?)", [film]).unwrap();
        assert!(remove(&conn, film, EXECUTABLE_INSIDE, 5).unwrap());
        assert!(is_junk_collection(&conn, 77).unwrap(), "its later slices find it by key");
        assert!(!remove(&conn, film, EXECUTABLE_INSIDE, 5).unwrap());
        assert_eq!(counts(&conn), (1, 1, 1));
        let inner: i64 = conn.query_row("select count(*) from archive_inner_files", [], |r| r.get(0)).unwrap();
        assert_eq!(inner, 0, "its rows go with it");
        assert!(is_junk(&conn, "Film", G).unwrap());
    }

    #[test]
    fn programs_are_told_by_their_last_extension() {
        for name in [
            "setup.exe",
            "Movie.2024.2160p.mkv.exe",
            "INSTALL.MSI",
            "run.bat",
            "x.cmd",
            "codec.scr",
            "free.com",
            "a.vbs",
            "b.ps1",
            "c.lnk",
            "game.apk",
            "app.dmg",
            "\"setup.exe\"",
            "dir/sub\\Setup.Exe",
        ] {
            assert!(is_executable(name), "{name}");
        }
        for name in [
            "Movie.2024.2160p.mkv",
            "movie.part01.rar",
            "disc.iso",
            "disc.bin",
            "x.par2",
            "x.vol07+08.par2",
            "x.nfo",
            "exe",
            ".exe",
            "Some.Show.S01E01.www.example.com.mkv",
            "x.7z.001",
        ] {
            assert!(!is_executable(name), "{name}");
        }
    }

    #[test]
    fn a_program_a_par2_set_describes_is_junk() {
        let (_dir, main) = shard();
        store::save(&main, &[post("Hidden", &["a1b2c3.part01.rar", "a1b2c3.par2"])]).unwrap();
        let conn = open(&main);
        let id: i64 = conn.query_row("select id from releases", [], |r| r.get(0)).unwrap();
        conn.execute(
            "insert into par2_files (release_id, file_id, set_id, md5, md5_16k, size, name)
             values (?, x'01', x'02', x'03', x'04', 5, 'Keygen.EXE')",
            [id],
        )
        .unwrap();
        assert_eq!(stored_reason(&conn, id).unwrap(), Some(EXECUTABLE_INSIDE));
    }

    #[test]
    fn a_pc_match_with_a_video_stays() {
        assert!(pc_is_junk(["setup.rar", "x.nfo"]));
        assert!(!pc_is_junk(["Film.2024.mkv", "x.nfo"]));
        assert!(pc_is_junk(Vec::<String>::new()));
    }
}
