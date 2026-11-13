//! Current classifier vectors retained after retiring the bulk reflag command.
//! The mixed visibility oracle below remains unchanged pending its native read port.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use atlas::parser::Release;
use atlas::{db, store};
use common::stage::{STAGE_GROUP, release_of};
use rusqlite::Connection;

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
        c.arg("--reflag-obfuscated").args(args).env("ATLAS_HOME", self.dir.path()).env("ATLAS_NO_KEYRING", "1");
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

    /// as an old build left them: flagged by the old rule only
    fn saved(&self, releases: &[Release]) {
        store::save(&self.main, releases).unwrap();
        self.shard()
            .execute(
                "update releases set is_obfuscated = name glob '*[0-9]*' and length(name) = 16 and name not like '%.%'",
                [],
            )
            .unwrap();
    }

    fn flag(&self, name: &str) -> i64 {
        self.shard().query_row("select is_obfuscated from releases where name = ?", [name], |r| r.get(0)).unwrap()
    }
}

#[test]
fn current_classifier_vectors_survive_reflag_retirement() {
    for name in ["a1B2c3D4e5F6g7H8", "kP93xQ7rL.ZMtwBfa", "Xk3Mz9PqL"] {
        assert!(atlas::parser::is_obfuscated(name), "{name}");
    }
    let identity = atlas::naming::without_collision_suffix("hQ72vB9nK [12345678]");
    assert_eq!(identity, "hQ72vB9nK");
    assert!(atlas::parser::is_obfuscated(identity));
    assert!(!atlas::parser::is_obfuscated("Good.Name.2024"));
}

#[test]
fn a_named_row_stays_visible() {
    let h = home();
    h.saved(&[Release { display_name: Some("Real.Name.2024".into()), ..release("Xk3Mz9PqL") }]);
    h.ok(&["--apply"]);
    assert_eq!(h.flag("Xk3Mz9PqL"), 1);
    let visible: i64 = h
        .shard()
        .query_row("select count(*) from releases where is_obfuscated = 1 and display_name is null", [], |r| r.get(0))
        .unwrap();
    assert_eq!(visible, 0, "search shows a flagged release that has a real name");
}
