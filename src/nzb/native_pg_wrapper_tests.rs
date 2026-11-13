//! Real public NZB entrypoints in a fresh private child. These controls require
//! the actual original owner and accepted native header rows, not copy grants.
use super::*;
use crate::pg;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const CHILD: &str = "ATLAS_NATIVE_NZB_WRAPPER_CHILD";
const SELECTOR: &str =
    "nzb::native_pg_wrapper_tests::native_public_nzb_wrappers_preserve_owned_copies_and_refuse_unproven_lending";
const FILE: &str = "file&one.bin";

fn private_write(path: &Path, bytes: &[u8]) {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).unwrap().write_all(bytes).unwrap();
}

fn posting(name: &str, articles: &[(&str, i64)], complete: bool) -> crate::parser::Release {
    crate::parser::Release {
        name: name.into(),
        group: "alt.binaries.nzb-wrappers".into(),
        poster: "fixture".into(),
        date: "Wed, 01 Jan 2020 00:00:00 +0000".into(),
        complete,
        articles: articles
            .iter()
            .map(|(mid, part)| crate::parser::Article {
                message_id: (*mid).into(),
                subject: format!("\"{FILE}\" yEnc ({part}/2)"),
                filename: Some(FILE.into()),
                part: Some(*part),
                total_parts: Some(2),
                bytes: 64,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn child() {
    let home = crate::paths::app_dir();
    let storage = crate::config::checked_storage_config().unwrap();
    let pool = pg::Pool::new(&storage).unwrap();
    let runtime = pg::indexer_runtime::Runtime::open(&pool).unwrap();
    let (whole, partial) = runtime
        .owner
        .with(|client| {
            let rows = [
                posting(
                    "Native.NZB.Whole",
                    &[
                        ("whole-copy-a@example.invalid", 1),
                        ("whole-copy-b@example.invalid", 1),
                        ("whole-part-two@example.invalid", 2),
                    ],
                    true,
                ),
                posting("Native.NZB.Partial", &[("partial-own@example.invalid", 1)], false),
                posting(
                    "Native.NZB.Neighbor",
                    &[("neighbor-part-one@example.invalid", 1), ("neighbor-part-two@example.invalid", 2)],
                    true,
                ),
            ];
            assert_eq!(
                pg::release_writer::save(client, &rows, 1)?,
                pg::release_writer::SaveStats { releases: 3, files: 3, parts: 6 }
            );
            let whole: i64 = client.query_one("select id from releases where name='Native.NZB.Whole'", &[])?.get(0);
            let partial: i64 = client.query_one("select id from releases where name='Native.NZB.Partial'", &[])?.get(0);
            let neighbor: i64 =
                client.query_one("select id from releases where name='Native.NZB.Neighbor'", &[])?.get(0);
            // Deliberately matching untrusted metadata is an adversarial input.
            // It carries no actual target-bound payload admission.
            let fingerprint = vec![2u8; 21];
            for id in [partial, neighbor] {
                client.execute(
                    "insert into release_fingerprint(release_id,posted) values($1,$2)",
                    &[&id, &fingerprint],
                )?;
            }
            Ok((whole, partial))
        })
        .unwrap();

    let built = build_nzb_checked(whole).unwrap().unwrap();
    let ordinal = build_nzb_file_checked(whole, 1).unwrap().unwrap();
    let named = build_nzb_named_checked(whole, FILE).unwrap().unwrap();
    let listed = file_list(whole).unwrap().unwrap();
    let partial_whole_refused = build_nzb_checked(partial).unwrap().is_none();
    let own = build_nzb_file_checked(partial, 1).unwrap().unwrap();
    let exports = home.join("exports");
    fs::create_dir(&exports).unwrap();
    let export = generate_nzb(whole, Some(&exports)).unwrap();
    let partial_export_refused = generate_nzb(partial, Some(&exports)).is_err();
    let before = fs::read_dir(&exports).unwrap().count();
    let mut observed = serde_json::json!({
        "whole_complete": built.complete(),
        "whole_copy_a": built.xml.matches("whole-copy-a@example.invalid").count(),
        "whole_copy_b": built.xml.matches("whole-copy-b@example.invalid").count(),
        "whole_part_two": built.xml.matches("whole-part-two@example.invalid").count(),
        "ordinal_matches": ordinal.xml == built.xml,
        "named_matches": named.xml == built.xml,
        "listed": listed.len()==1 && listed[0].n==1 && listed[0].name.as_deref()==Some(FILE) && listed[0].articles==3 && listed[0].bytes==192,
        "export_matches": fs::read(&export).unwrap()==built.xml.as_bytes(),
        "partial_whole_refused":partial_whole_refused,
        "partial_export_refused":partial_export_refused,
        "partial_own":own.xml.contains("partial-own@example.invalid"),
        "partial_neighbor":own.xml.contains("neighbor-part-"),
        "partial_missing_two":own.gaps.iter().any(|gap|gap.missing==[2]),
        "partial_marked":own.xml.contains("x-atlas-incomplete"),
        "exports_before_invalid_config":before,
    });
    private_write(&home.join("config.json"), b"{");
    observed["invalid_config_refused"] = serde_json::json!(
        build_nzb(whole).is_err()
            && build_nzb_checked(whole).is_err()
            && build_nzb_file(whole, 1).is_err()
            && build_nzb_file_checked(whole, 1).is_err()
            && build_nzb_named_checked(whole, FILE).is_err()
            && file_list(whole).is_err()
            && generate_nzb(whole, Some(&exports)).is_err()
    );
    observed["exports_after_invalid_config"] = serde_json::json!(fs::read_dir(&exports).unwrap().count());
    observed["legacy_catalog_absent"] = serde_json::json!(
        ["atlas.db", "atlas.raw.db", "atlas.media.db", "atlas.details.db"].iter().all(|name| !home.join(name).exists())
    );
    private_write(&home.join("nzb-wrapper-observed.json"), &serde_json::to_vec(&observed).unwrap());
    runtime.owner.close();
}

#[test]
#[ignore = "requires explicit isolated PG, original-owner child and production spool-space admission"]
fn native_public_nzb_wrappers_preserve_owned_copies_and_refuse_unproven_lending() {
    if std::env::var(CHILD).as_deref() == Ok("1") {
        child();
        return;
    }
    let schema = pg::testing::Schema::native_writer().expect("explicit isolated native PG schema required");
    let home = tempfile::tempdir().unwrap();
    let config = serde_json::json!({"storage":{"backend":"postgres","url":schema.url,"schema":schema.name,"max_connections":4,"allow_plaintext":true},
        "header_store":{"enabled":true,"dir":home.path().join("headers")}});
    private_write(&home.path().join("config.json"), &serde_json::to_vec(&config).unwrap());
    let mut process = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", SELECTOR, "--ignored", "--test-threads=1"])
        .env("ATLAS_HOME", home.path())
        .env("ATLAS_NO_KEYRING", "1")
        .env(CHILD, "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = process.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            process.kill().unwrap();
            process.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.is_some_and(|status| status.success()), "owned native NZB fixture/setup failed");
    let path = home.path().join("nzb-wrapper-observed.json");
    assert!(fs::metadata(&path).unwrap().len() <= 4096);
    let observed: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    for field in [
        "whole_complete",
        "ordinal_matches",
        "named_matches",
        "listed",
        "export_matches",
        "partial_whole_refused",
        "partial_export_refused",
        "partial_own",
        "partial_missing_two",
        "partial_marked",
        "invalid_config_refused",
        "legacy_catalog_absent",
    ] {
        assert_eq!(observed[field], true, "{field}");
    }
    for field in ["whole_copy_a", "whole_copy_b", "whole_part_two"] {
        assert_eq!(observed[field], 1, "{field}");
    }
    assert_eq!(observed["partial_neighbor"], false, "matching metadata must never authorize native lending");
    assert_eq!(observed["exports_before_invalid_config"], 1);
    assert_eq!(observed["exports_after_invalid_config"], 1);
}
