//! Releases nobody here wants, gone for good: anything that carries a program
//! (an installer, a script, a `.mkv.exe`, or one inside an archive) and what
//! the categories call PC. Native writers retain the removed release's identity
//! fence so later articles and collection slices cannot save it again.

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

/// Whether a PC match keeps its release: only one with a video file.
pub fn pc_is_junk(filenames: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
    !filenames.into_iter().any(|f| is_video(f.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Article, Release};

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

    fn settled_at() -> i64 {
        let delay = crate::catalog_facts::SETTLE_SECS
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(crate::catalog_facts::COMPLETE_SETTLE_SECS.load(std::sync::atomic::Ordering::Relaxed))
            .max(1);
        1i64.saturating_sub(delay)
    }

    fn schema() -> Option<crate::pg::testing::Schema> {
        let schema = crate::pg::testing::Schema::native_writer();
        if schema.is_none() {
            eprintln!("requires explicit ATLAS_PG_TEST_URL for native junk assertions");
        }
        schema
    }

    fn counts(client: &mut postgres::Client) -> (i64, i64, i64) {
        let row = client
            .query_one(
                "select (select count(*) from releases), (select count(*) from files), (select count(*) from segments)",
                &[],
            )
            .unwrap();
        (row.get(0), row.get(1), row.get(2))
    }

    #[test]
    fn a_post_with_a_program_is_never_saved_and_nothing_after_it_is() {
        let Some(schema) = schema() else { return };
        let mut client = schema.pool.get().unwrap();
        crate::pg::release_writer::save(
            &mut client,
            &[post("Free.Movie.2024", &["Free.Movie.2024.mkv.exe", "Free.Movie.2024.nfo"])],
            1,
        )
        .unwrap();
        assert_eq!(counts(&mut client), (0, 0, 0));
        assert_eq!(client.query_one("select count(*) from release_writer_junk", &[]).unwrap().get::<_, i64>(0), 1);
        crate::pg::release_writer::save(&mut client, &[post("Free.Movie.2024", &["Free.Movie.2024.part01.rar"])], 2)
            .unwrap();
        assert_eq!(counts(&mut client), (0, 0, 0), "later fragments stay fenced");
        crate::pg::release_writer::save(&mut client, &[post("Real.Movie.2024", &["Real.Movie.2024.mkv"])], 3).unwrap();
        assert_eq!(counts(&mut client), (1, 1, 1));
    }

    #[test]
    fn a_program_arriving_late_takes_the_release_and_its_counts() {
        let Some(schema) = schema() else { return };
        let mut client = schema.pool.get().unwrap();
        crate::pg::release_writer::save(
            &mut client,
            &[post("Some.Thing", &["a.part01.rar", "a.part02.rar"]), post("Other.Thing", &["b.mkv"])],
            1,
        )
        .unwrap();
        assert_eq!(counts(&mut client), (2, 3, 3));
        crate::pg::release_writer::save(&mut client, &[post("Some.Thing", &["setup.exe"])], 2).unwrap();
        assert_eq!(counts(&mut client), (1, 1, 1));
        assert_eq!(client.query_one("select name from releases", &[]).unwrap().get::<_, String>(0), "Other.Thing");
        crate::pg::release_writer::save(&mut client, &[post("Some.Thing", &["a.part03.rar"])], 3).unwrap();
        assert_eq!(counts(&mut client), (1, 1, 1));
    }

    #[test]
    fn what_is_stored_tells_a_release_is_junk() {
        use crate::pg::stage_queue::{Queue, Settlement, Visit};
        let Some(schema) = crate::pg::testing::Schema::new() else {
            eprintln!("requires explicit ATLAS_PG_TEST_URL for native junk inventory/settlement");
            return;
        };
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        let (packed, film) = runtime
            .owner
            .with(|client| {
                let mut film = post("Film", &["Film.2024.mkv"]);
                film.collection = Some("film-collection".into());
                crate::pg::release_writer::save(client, &[post("Packed", &["x.part01.rar"]), film], settled_at())?;
                let id = |client: &mut postgres::Client, name: &str| -> i64 {
                    client.query_one("select id from releases where name=$1", &[&name]).unwrap().get(0)
                };
                Ok((id(client, "Packed"), id(client, "Film")))
            })
            .unwrap();
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        assert_eq!(reads.inner_junk(packed, false).unwrap(), None);
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "insert into release_category(release_id,category) values($1,4010),($2,4010)",
                    &[&packed, &film],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(reads.inner_junk(packed, false).unwrap(), Some(PC));
        assert_eq!(reads.inner_junk(film, true).unwrap(), None, "a PC match with video stays");
        assert!(reads.stale_pc(film, true).unwrap());
        assert!(!reads.stale_pc(packed, false).unwrap());
        runtime
            .owner
            .with(|client| {
                client.execute(
                    "insert into archive_inner_files(release_id,seq,name,size,source) values($1,0,'bin/Crack.exe',1,1)",
                    &[&film],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(reads.inner_junk(film, true).unwrap(), Some(EXECUTABLE_INSIDE));
        let queue = Queue::for_owner(&runtime.owner).unwrap();
        let claims = queue.claim(10, 10, 60).unwrap();
        let claim = claims.iter().find(|claim| claim.release_id() == film).unwrap();
        let applied = crate::postprocess::apply::Applied {
            release_id: film,
            junk: Some(EXECUTABLE_INSIDE),
            ..Default::default()
        };
        assert_eq!(
            queue.settle(claim, Visit::prepare(claim, applied, [1; 32]).unwrap(), 11).unwrap(),
            Settlement::Applied
        );
        runtime
            .owner
            .with(|client| {
                assert_eq!(counts(client), (1, 1, 1));
                assert_eq!(
                    client
                        .query_one("select count(*) from archive_inner_files where release_id=$1", &[&film])?
                        .get::<_, i64>(0),
                    0
                );
                let mut renamed = post("Renamed.Film", &["later.part01.rar"]);
                renamed.collection = Some("film-collection".into());
                crate::pg::release_writer::save(client, &[renamed], 12)?;
                assert_eq!(
                    counts(client),
                    (1, 1, 1),
                    "later collection slices keep the original fence under another name"
                );
                Ok(())
            })
            .unwrap();
        runtime.owner.close();
    }

    #[test]
    fn native_junk_retraction_finds_collection_identity_by_index() {
        let Some(schema) = schema() else { return };
        let mut client = schema.pool.get().unwrap();
        let mut release = post("Collection", &["video.mkv"]);
        release.collection = Some("collection-key".into());
        let (_, key) = crate::pg::release_writer::identity(&release);
        crate::pg::release_writer::save(&mut client, &[release], 1).unwrap();
        let mut tx = client.transaction().unwrap();
        tx.batch_execute("set local enable_seqscan=off").unwrap();
        let plan = tx
            .query(
                "explain select id from releases where group_name=$1 and is_collection=true and identity_key=$2",
                &[&G, &&key[..]],
            )
            .unwrap()
            .into_iter()
            .map(|row| row.get::<_, String>(0))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(plan.contains("Index") && plan.contains("identity_key") && !plan.contains("Seq Scan"), "{plan}");
        tx.rollback().unwrap();
    }

    #[test]
    fn a_program_a_par2_set_describes_is_junk() {
        let Some(schema) = crate::pg::testing::Schema::new() else {
            eprintln!("requires explicit ATLAS_PG_TEST_URL for native PAR2 junk inventory");
            return;
        };
        let runtime = crate::pg::stage_queue_tests::runtime(&schema);
        crate::pg::stage_queue_tests::push(&runtime);
        let id=runtime.owner.with(|client| {
            let id:i64=client.query_one("select id from releases",&[])?.get(0);
            client.execute("insert into par2_files(release_id,file_id,set_id,md5,md5_16k,size,name) values($1,$2,$3,$4,$5,5,'Keygen.EXE')", &[&id,&vec![1u8;16],&vec![2u8;16],&vec![3u8;16],&vec![4u8;16]])?;Ok(id)
        }).unwrap();
        let reads = crate::pg::stage_reads::StepReads::for_owner(&runtime.owner).unwrap();
        assert_eq!(reads.inner_junk(id, false).unwrap(), Some(EXECUTABLE_INSIDE));
        runtime.owner.close();
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
    fn a_pc_match_with_a_video_stays() {
        assert!(pc_is_junk(["setup.rar", "x.nfo"]));
        assert!(!pc_is_junk(["Film.2024.mkv", "x.nfo"]));
        assert!(pc_is_junk(Vec::<String>::new()));
    }
    #[test]
    fn pc_and_video_classification_keep_their_boundaries() {
        for category in [4000, 4010, 4999] {
            assert!(is_pc(category), "{category}");
        }
        for category in [-1, 0, 3999, 5000, 8000] {
            assert!(!is_pc(category), "{category}");
        }
        for name in ["film.MKV", "video.m2ts", "clip.webm", "disc.vob\""] {
            assert!(is_video(name), "{name}");
        }
        for name in ["film.mkv.exe", "disc.iso", "video.mkv.part", "video"] {
            assert!(!is_video(name), "{name}");
        }
    }
}
