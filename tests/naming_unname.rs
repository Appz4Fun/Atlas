//! Classifier policy retained after retirement of the bulk unname command.
//! The mixed later-step bit oracle remains unchanged pending its native port.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use atlas::parser::Release;
use atlas::{db, store};
use common::stage::{STAGE_GROUP, release_of};
use rusqlite::{Connection, params};

struct Home {
    dir: tempfile::TempDir,
    main: PathBuf,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    Home { dir, main }
}

fn release(name: &str) -> Release {
    release_of(name, "x.bin", 2, 2)
}

impl Home {
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_atlas"));
        c.arg("--unname").args(args).env("ATLAS_HOME", self.dir.path()).env("ATLAS_NO_KEYRING", "1");
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args, &[]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{text}");
        text
    }

    fn shard(&self) -> Connection {
        db::open_shard(&store::shard_path(&self.main, store::shard_of(STAGE_GROUP))).unwrap()
    }
}

fn every_source(h: &Home) {
    let names = ["src3", "src5", "src10", "src15", "src20", "src100", "inline", "plain"];
    let rows: Vec<Release> = names.map(release).to_vec();
    store::save(&h.main, &rows).unwrap();
    let conn = h.shard();
    for (name, source) in [("src3", 3), ("src5", 5), ("src10", 10), ("src15", 15), ("src20", 20), ("src100", 100)] {
        conn.execute(
            "update releases set display_name = ?, name_source = ? where name = ?",
            params![format!("Named.By.{source}.2024"), source, name],
        )
        .unwrap();
    }
    conn.execute("update releases set display_name = 'Inline.Named.2020' where name = 'inline'", []).unwrap();
}

#[test]
fn current_classifier_vectors_survive_unname_retirement() {
    assert!(atlas::parser::is_obfuscated("kP93xQ7rL.ZMtwBfa"));
    assert!(atlas::parser::is_obfuscated("Xk3Mz9PqL [12345678]"));
    assert!(!atlas::parser::is_obfuscated("Good.Release.2024"));
}

#[test]
fn naming_keeps_its_bit_and_failed_later_steps_clear() {
    let h = home();
    every_source(&h);
    let conn = h.shard();
    let id: i64 = conn.query_row("select id from releases where name = 'src5'", [], |r| r.get(0)).unwrap();
    let naming = 1i64 << 8;
    conn.execute(
        "insert into pp_state (release_id, done, failed, db_attempts) values (?, ?, 0, 2)",
        params![id, naming],
    )
    .unwrap();
    h.ok(&["--apply"]);
    let (done, attempts): (i64, i64) = conn
        .query_row("select done, db_attempts from pp_state where release_id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_ne!(done & naming, 0, "the name stays cleared");
    assert_eq!(attempts, 0);
}
