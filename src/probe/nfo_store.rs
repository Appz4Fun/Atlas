//! A release's NFO, read once and kept: the probe keeps the one it read in its own JSON (`nfo`, `nfo_file`), and a
//! step that read one before the probe did (the archive listing looking for a password, the name or IMDb step on a
//! release the probe passed over) keeps it here. Every reader looks here first, soo an NFO's article is fetched at
//! most once a release. Only an NFO read from its file's first article is kept: a later article's text, read while
//! the first wasn't indexed yet, isn't the NFO, and the first is read once it's there.

use rusqlite::{Connection, OptionalExtension};

use crate::postprocess::schema::ShardTable;
use crate::postprocess::step::StepWrite;
use crate::store;

/// An NFO read by a step other than the probe, while the probe has none. The probe moves it into its JSON.
pub const RELEASE_NFO: ShardTable = ShardTable {
    name: "release_nfo",
    create: "create table if not exists release_nfo (
        release_id INTEGER PRIMARY KEY,
        file       TEXT NOT NULL,
        text       TEXT NOT NULL
    )",
    indexes: &[],
    release_key: Some("release_id"),
    added: &[],
};

/// The first NFO read stays: a second reader would have used it.
const KEEP: &str = "insert or ignore into release_nfo (release_id, file, text) values (?1, ?2, ?3)";
/// Once the probe's JSON holds the same NFO.
pub const DROP: &str = "delete from release_nfo where release_id = ?1 and file = ?2";

/// As much of an NFO as is kept, in characters: the probe's own cap.
pub const MAX_CHARS: usize = 256 * 1024;

/// An NFO already read for a release.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredNfo {
    /// the release file it was read from
    pub file: String,
    pub text: String,
}

impl StoredNfo {
    /// Whether this is the NFO in `file`, posted under that name or with `par2_name` as its PAR2 set names it.
    pub fn is_of(&self, file: &str, par2_name: Option<&str>) -> bool {
        self.file == file || par2_name == Some(self.file.as_str())
    }
}

/// The NFO already read for a release, the probe's first, on a shard connection or one with the shard attached as
/// `s{n}`. A probe's NFO without its file (read before it kept one, or from a later article) isn't known to be the
/// NFO's start, soo it isn't one. Any failure, such as a shard without the table yet, reads as none.
pub fn stored(conn: &Connection, release_id: i64) -> Option<StoredNfo> {
    from_probe(conn, release_id).or_else(|| from_table(conn, release_id))
}

fn on_shard<T>(
    conn: &Connection,
    release_id: i64,
    sql: &str,
    row: impl Fn(&rusqlite::Row) -> rusqlite::Result<T>,
) -> Option<T> {
    let s = store::shard_of_id(release_id);
    let qualified = sql.replace("{schema}", &format!("s{s}."));
    let plain = sql.replace("{schema}", "");
    let ask = |q: &str| conn.query_row(q, [release_id], &row).optional();
    match ask(&qualified) {
        Ok(found) => found,
        Err(_) => ask(&plain).ok().flatten(),
    }
}

fn from_probe(conn: &Connection, release_id: i64) -> Option<StoredNfo> {
    let (text, file) = on_shard(
        conn,
        release_id,
        "select json_extract(info, '$.nfo'), json_extract(info, '$.nfo_file') from {schema}release_probe
         where release_id = ?1",
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
    )?;
    Some(StoredNfo { file: file?, text: text? })
}

/// Any NFO text read for a release, the probe's even from a later article: for the page to say what's in it.
pub fn any_text(conn: &Connection, release_id: i64) -> Option<String> {
    on_shard(
        conn,
        release_id,
        "select json_extract(info, '$.nfo') from {schema}release_probe where release_id = ?1",
        |r| r.get::<_, Option<String>>(0),
    )
    .flatten()
    .or_else(|| from_table(conn, release_id).map(|nfo| nfo.text))
}

fn from_table(conn: &Connection, release_id: i64) -> Option<StoredNfo> {
    on_shard(conn, release_id, "select file, text from {schema}release_nfo where release_id = ?1", |r| {
        Ok(StoredNfo { file: r.get(0)?, text: r.get(1)? })
    })
}

/// Keeps an NFO a step read, for the probe and the other steps to use in place of fetching it again.
pub fn keep(release_id: i64, file: &str, text: &str) -> StepWrite {
    let text: String = text.chars().take(MAX_CHARS).collect();
    StepWrite::new(KEEP, vec![release_id.into(), file.to_string().into(), text.into()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table release_probe (release_id INTEGER PRIMARY KEY, crc32 INTEGER, agent TEXT, info TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute_batch(RELEASE_NFO.create).unwrap();
        conn
    }

    fn apply(conn: &Connection, w: StepWrite) {
        conn.execute(w.sql, rusqlite::params_from_iter(w.params.iter())).unwrap();
    }

    #[test]
    fn the_probes_nfo_comes_first_then_one_a_step_kept() {
        let conn = shard();
        assert_eq!(stored(&conn, 8), None);
        apply(&conn, keep(8, "a.nfo", "kept by a step"));
        assert_eq!(stored(&conn, 8), Some(StoredNfo { file: "a.nfo".into(), text: "kept by a step".into() }));
        // a second reader's NFO doesn't replace the first
        apply(&conn, keep(8, "b.nfo", "another"));
        assert_eq!(stored(&conn, 8).unwrap().text, "kept by a step");
        conn.execute(
            "insert into release_probe (release_id, info) values (8, '{\"nfo\":\"from the probe\",\"nfo_file\":\"a.nfo\"}')",
            [],
        )
        .unwrap();
        assert_eq!(stored(&conn, 8), Some(StoredNfo { file: "a.nfo".into(), text: "from the probe".into() }));
        // a probe without an NFO leaves the kept one
        conn.execute("insert into release_probe (release_id, info) values (16, '{\"nfo\":null}')", []).unwrap();
        apply(&conn, keep(16, "c.nfo", "kept"));
        assert_eq!(stored(&conn, 16).unwrap().text, "kept");
    }

    #[test]
    fn a_probes_nfo_without_its_file_isnt_reused() {
        let conn = shard();
        // an old probe, or one that read a later article: the step reads the NFO's start itself
        conn.execute("insert into release_probe (release_id, info) values (8, '{\"nfo\":\"part 2\"}')", []).unwrap();
        assert_eq!(stored(&conn, 8), None);
        assert_eq!(any_text(&conn, 8).as_deref(), Some("part 2"), "the page still reads it");
        apply(&conn, keep(8, "a.nfo", "the start"));
        assert_eq!(stored(&conn, 8).unwrap().text, "the start");
        let named = StoredNfo { file: "Real.nfo".into(), text: String::new() };
        assert!(named.is_of("Real.nfo", None));
        assert!(named.is_of("x7f3a9", Some("Real.nfo")));
        assert!(!named.is_of("Other.nfo", None));
    }

    #[test]
    fn it_is_capped() {
        let conn = shard();
        apply(&conn, keep(8, "a.nfo", &"x".repeat(MAX_CHARS + 10)));
        assert_eq!(stored(&conn, 8).unwrap().text.len(), MAX_CHARS);
    }
}
