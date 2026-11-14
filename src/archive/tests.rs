use super::testkit::*;
use super::*;
use crate::archive::names::*;
use crate::archive::select::*;
use crate::store::FileInfo;

mod password_budget_tests {
    use super::*;
    use crate::release_info::{PasswordBudgetExhausted, PasswordCandidateBudget, find_verified_password_with_budget};

    fn aes_check() -> PasswordTest {
        // Existing independent WinZip PBKDF2 vector in zip::tests::an_aes_zip_tests_its_password.
        let salt = std::array::from_fn(|i| i as u8);
        PasswordTest::Zip(zip::PasswordCheck::Aes { strength: 3, salt, verifier: [0xa3, 0x36] })
    }

    #[test]
    fn the_twenty_fourth_password_can_succeed_but_rejection_exhausts_the_visit() {
        let check = aes_check();
        let wrong: Vec<String> = (0..24).map(|i| format!("wrong-candidate-{i}")).collect();
        assert!(wrong.iter().all(|candidate| check.accepts(candidate) == Some(false)));
        let mut candidates = wrong[..23].to_vec();
        candidates.push("secret".into());
        let mut budget = PasswordCandidateBudget::default();
        assert_eq!(find_verified_password_with_budget([], &candidates, Some(&check), &mut budget).unwrap().as_deref(), Some("secret"));
        assert_eq!(budget.used(), 24);

        let mut budget = PasswordCandidateBudget::default();
        assert_eq!(find_verified_password_with_budget([], &wrong, Some(&check), &mut budget), Err(PasswordBudgetExhausted));
        assert_eq!(budget.used(), 24);
        assert_eq!(find_verified_password_with_budget([], &["secret".into()], Some(&check), &mut budget), Err(PasswordBudgetExhausted));
        assert_eq!(budget.used(), 24, "a later source gets no fresh allowance");
        let error: crate::postprocess::step::StepError = PasswordBudgetExhausted.into();
        assert!(matches!(error, crate::postprocess::step::StepError::Retry(message) if message == "password candidate budget exhausted"));
    }

    #[test]
    fn split_filename_searches_share_the_budget_and_repeated_candidates_are_free() {
        let check = aes_check();
        let mut budget = PasswordCandidateBudget::default();
        for _ in 0..100 {
            assert_eq!(find_verified_password_with_budget(["{{wrong-candidate-0}}"], &[], Some(&check), &mut budget).unwrap(), None);
        }
        let after_duplicates = budget.used();
        assert!(after_duplicates < 24);
        assert_eq!(find_verified_password_with_budget(["{{secret}}"], &[], Some(&check), &mut budget).unwrap().as_deref(), Some("secret"));
        assert_eq!(budget.used(), after_duplicates + 1, "a late valid candidate still fits after repeated filenames");

        let mut budget = PasswordCandidateBudget::default();
        let mut exhausted_at = None;
        for i in 0..50_000 {
            let name = format!("{{{{wrong-candidate-{i}}}}}");
            if find_verified_password_with_budget([name.as_str()], &[], Some(&check), &mut budget).is_err() {
                exhausted_at = Some(i);
                break;
            }
        }
        assert!(exhausted_at.is_some_and(|i| i < 24));
        assert_eq!(budget.used(), 24, "linked names cannot reset the verifier count");
    }

    #[test]
    fn a_changed_check_does_not_reuse_a_rejection_or_reset_the_allowance() {
        let aes = aes_check();
        let rar = PasswordTest::from_bytes(&[
            0x0f, 0xcc, 0xf5, 0x00, 0x61, 0xff, 0xba, 0xd4, 0x3d, 0xcf, 0x68, 0x42, 0xb1, 0x49, 0xd5, 0xf5, 0xa2, 0xe7,
            0x82, 0x9b, 0x3a, 0xab, 0xe0, 0x37, 0xb0,
        ]).unwrap();
        let mut budget = PasswordCandidateBudget::default();
        assert_eq!(find_verified_password_with_budget([], &["F-UnK-NZBNF".into()], Some(&aes), &mut budget).unwrap(), None);
        let before = budget.used();
        assert_eq!(find_verified_password_with_budget([], &["F-UnK-NZBNF".into()], Some(&rar), &mut budget).unwrap().as_deref(), Some("F-UnK-NZBNF"));
        assert_eq!(budget.used(), before + 1);
    }

    #[test]
    fn uncheckable_explicit_hints_use_no_crypto_and_the_builtin_is_never_a_guess() {
        let unsupported = PasswordTest::Rar5(rar::PasswordCheck { kdf: rar::MAX_KDF + 1, salt: [0; 16], check: [0; 8] });
        let mut budget = PasswordCandidateBudget::default();
        for check in [None, Some(&unsupported)] {
            assert_eq!(find_verified_password_with_budget(["password: explicit-secret"], &[], check, &mut budget).unwrap().as_deref(), Some("explicit-secret"));
            assert_eq!(find_verified_password_with_budget([], &[], check, &mut budget).unwrap(), None);
        }
        assert_eq!(budget.used(), 0);
        let weak = PasswordTest::Zip(zip::PasswordCheck::Crypto { header: [0; 12], check: 0x99 });
        assert!(!weak.certain());
        assert_eq!(find_verified_password_with_budget(["password: wrong"], &[], Some(&weak), &mut budget).unwrap(), None);
        assert!(budget.used() > 0, "a weak ZipCrypto check is supported, not an unsupported-KDF fallback");
    }
}

fn names(l: &Listing) -> Vec<String> {
    l.files.iter().map(|f| f.name.clone()).collect()
}

/// every prefix: no panic, a prefix of the full list, and never complete
fn every_cut(full: &[u8], parse: impl Fn(&[u8]) -> Listing) {
    let whole = names(&parse(full));
    assert!(!whole.is_empty());
    for i in 0..full.len() {
        let part = parse(&full[..i]);
        assert!(whole.starts_with(&names(&part)) && !part.complete, "cut {i}");
    }
}

#[test]
fn identify_goes_by_magic_alone() {
    let table: &[(&[u8], Format)] = &[
        (b"Rar!\x1a\x07\x01\x00", Format::Rar5),
        (b"Rar!\x1a\x07\x00", Format::Rar4),
        (b"7z\xbc\xaf\x27\x1c", Format::SevenZip),
        (b"PK\x03\x04", Format::Zip),
        (b"PK\x07\x08PK\x03\x04", Format::Zip),
        (b"PK00PK\x03\x04", Format::Zip),
        (b"PK\x05\x06", Format::Zip),
        (b"PAR2\0PKT", Format::Par2),
        (b"\xff\xd8\xff", Format::Image),
        (b"\x89PNG\r\n\x1a\n", Format::Image),
        (b"GIF87a", Format::Image),
        (b"GIF89a", Format::Image),
        (b"RIFF\0\0\0\0WEBP", Format::Image),
        (b"BM", Format::Image),
        (b"II*\0", Format::Image),
        (b"MM\0*", Format::Image),
        (b"\x1a\x45\xdf\xa3", Format::Media),
        (b"\0\0\0\x20ftyp", Format::Media),
        (b"RIFF\0\0\0\0AVI ", Format::Media),
        (b"\0\0\x01\xba", Format::Media),
        (b"fLaC", Format::Media),
        (b"OggS", Format::Media),
        (b"ID3", Format::Media),
        (b"Rar!\x1a\x07", Format::Unknown),
        (b"", Format::Unknown),
        (b"PK\x07\x08", Format::Unknown),
    ];
    for (bytes, format) in table {
        assert_eq!(identify(bytes), *format, "{bytes:?}");
    }
}

#[test]
fn clean_name_table() {
    assert_eq!(clean_name("a\x1b[31mb").as_deref(), Some("a[31mb"));
    assert_eq!(clean_name("..\\..\\x").as_deref(), Some("../../x"));
    assert_eq!(clean_name("./a/").as_deref(), Some("a"));
    for gone in ["  ", ".", ".."] {
        assert_eq!(clean_name(gone), None, "{gone}");
    }
    assert_eq!(clean_name(&"a".repeat(2000)).unwrap().len(), 512);
    let four = clean_name(&"😀".repeat(2000)).unwrap();
    assert!(four.len() <= 512 && four.chars().all(|c| c == '😀'));
}

#[test]
fn looks_random_table() {
    for yes in ["a1b2c3d4e5f6g7h8", "1a2b3c4d", "123456", "8f14e45f-ceea-467f-a0e6-a33d1cdf2a10"] {
        assert!(looks_random(yes), "{yes}");
    }
    for no in ["Real.Name", "movie", "Example Movie"] {
        assert!(!looks_random(no), "{no}");
    }
}

type NameCase<'a> = (&'a [(&'a str, u64)], Option<&'a str>);

#[test]
fn best_name_table() {
    let hash = "0123456789abcdef0123456789abcdef.mkv";
    let table: &[NameCase<'_>] = &[
        (&[("small.nfo", 10), ("Real.Movie.2024.mkv", 5_000_000)], Some("Real.Movie.2024.mkv")),
        (
            &[("Real.Name.2024.part01.rar", 100), ("Real.Name.2024.part02.rar", 100), ("Real.Name.2024.nfo", 5)],
            Some("Real.Name.2024"),
        ),
        (&[("a1b2c3d4e5f6a7b8c9d0.part01.rar", 100), ("a1b2c3d4e5f6a7b8c9d0.part02.rar", 100)], None),
        (&[(hash, 4 << 30), ("Real.Movie.2024.1080p-GRP.nfo", 5_000)], Some("Real.Movie.2024.1080p-GRP")),
        (
            &[(hash, 4 << 30), ("Real.Movie.2024.1080p-GRP-sample.mkv", 50 << 20)],
            Some("Real.Movie.2024.1080p-GRP-sample.mkv"),
        ),
        (&[("Real.Movie.2024.sample.mkv", 50), ("Other.Name.2024.mkv", 10)], Some("Other.Name.2024.mkv")),
        (&[("Disc1/Real.Show.S01E01.mkv", 10), ("Disc1/Real.Show.S01E02.mkv", 20)], Some("Real.Show.S01E02.mkv")),
        (&[("cover.jpg", 10), ("readme.txt", 5)], None),
        (&[("Real.Name.7z.001", 10), ("Real.Name.7z.002", 10)], Some("Real.Name")),
        (&[("Real.Name.vol000+01.par2", 10), ("Real.Name.par2", 1)], Some("Real.Name")),
    ];
    for (files, want) in table {
        assert_eq!(best_name(files.iter().copied()).as_deref(), *want, "{files:?}");
    }
}

#[test]
fn collapse_keeps_the_first_volume_and_sums() {
    let file = |name: String, size: u64| InnerFile::new(name, Some(size));
    let mut files: Vec<InnerFile> = (1..=100).map(|i| file(format!("x.part{i:02}.rar"), 10)).collect();
    files.push(file("x.nfo".into(), 1));
    let rows = collapse_volumes(&files);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].name.as_str(), rows[0].size), ("x.part01.rar", Some(1000)));
    let many: Vec<InnerFile> = (0..100).map(|i| file(format!("{i}.mkv"), 1)).collect();
    assert_eq!(collapse_volumes(&many).len(), MAX_STORED);
    assert_eq!(collapse_volumes(&[file("a.mkv".into(), 1), file("a.mkv".into(), 1)]).len(), 1);
    // a set listed with its recovery files first, as an NZB of an upload lists them: named after its data
    let upload = [
        file("x.par2".into(), 1),
        file("x.vol000+01.par2".into(), 2),
        file("x.part001.rar".into(), 100),
        file("x.part002.rar".into(), 100),
    ];
    assert_eq!(collapse_volumes(&upload), [file("x.part001.rar".into(), 203)]);
}

#[test]
fn collapse_names_a_set_after_its_first_volume_in_any_order() {
    let file = |name: &str| InnerFile::new(name.into(), Some(10));
    let as_par2_lists = [file("Show.S01E04.7z.026"), file("Show.S01E04.7z.001"), file("Show.S01E04.7z.097")];
    let rows = collapse_volumes(&as_par2_lists);
    assert_eq!((rows[0].name.as_str(), rows[0].size), ("Show.S01E04.7z.001", Some(30)));
    let rows = collapse_volumes(&[file("x.r00"), file("x.r01"), file("x.rar")]);
    assert_eq!(rows[0].name, "x.rar", "an old-style set starts at its .rar");
}

#[test]
fn password_table() {
    let with = |names: &[&str]| Listing {
        files: names.iter().map(|n| InnerFile::new((*n).into(), None)).collect(),
        ..Default::default()
    };
    assert_eq!(Listing { encrypted: true, ..Default::default() }.password(), Password::Yes);
    assert_eq!(Listing { complete: true, ..Default::default() }.password(), Password::No);
    assert_eq!(Listing { complete: true, damaged: true, ..Default::default() }.password(), Password::Maybe);
    assert_eq!(Listing::default().password(), Password::Maybe);
    assert_eq!(with(&["a.rar", "b.r00"]).password(), Password::Maybe);
    assert_eq!(with(&["a.rar", "b.mkv"]).password(), Password::No);
    for n in 0..3 {
        assert_eq!(Password::from_code(n).unwrap().code(), n);
    }
    assert_eq!(Password::from_code(3), None);
}

mod rar_tests {
    use super::*;
    use crate::archive::rar;

    fn first4() -> Vec<u8> {
        rar4(&[
            r4_main(0),
            r4_file(0, b"Example.Movie.2024.mkv", b"hello world"),
            r4_file(0, b"second.nfo", b"abc"),
            r4_end(),
        ])
    }

    fn first5() -> Vec<u8> {
        rar5(&[
            r5_main(),
            r5_file(2, 4, b"Example.Movie.2024.mkv", &[], b"hello world"),
            r5_file(2, 5, b"folder", &[], b""),
            r5_file(3, 4, b"CMT", &[], b"note"),
            r5_file(2, 4, "Фильм.nfo".as_bytes(), &[], b"abc"),
            r5_end(),
        ])
    }

    #[test]
    fn rar_listing() {
        let l = rar::list(&first4());
        assert_eq!(names(&l), vec!["Example.Movie.2024.mkv", "second.nfo"]);
        assert_eq!((l.files[0].size, l.files[1].size), (Some(11), Some(3)));
        assert!(l.complete);
        assert_eq!(l.password(), Password::No);
        let l = rar::list(&first5());
        assert_eq!(names(&l), vec!["Example.Movie.2024.mkv", "Фильм.nfo"], "directory and service left out");
        assert!(l.complete);
    }

    fn unicode(raw: &[u8]) -> Vec<String> {
        names(&rar::list(&rar4(&[r4_main(0), r4_file(0x0200, raw, b""), r4_end()])))
    }

    #[test]
    fn rar_unicode_names() {
        let packed = [
            &b"____.mkv\0"[..],
            &[0x04, 0b0101_0101, 0x24, 0x38, 0x3b, 0x4c, 0b0100_0000, 0x3c, b'.', b'm', b'k', 0, b'v'],
        ]
        .concat();
        assert_eq!(unicode(&packed), vec!["Фильм.mkv"]);
        assert_eq!(unicode("Фильм2.mkv".as_bytes()), vec!["Фильм2.mkv"]);
        let run = [&b"abcdef.mkv\0"[..], &[0x04, 0b1100_0000, 0x88, 0x00]].concat();
        let want: String =
            String::from_utf16_lossy(&b"abcdef.mkv".iter().map(|b| 0x0400 | u16::from(*b)).collect::<Vec<_>>());
        assert_eq!(unicode(&run), vec![want]);
        assert_eq!(unicode(&[&b"abcdef.mkv\0"[..], &[0x04, 0b1100_0000, 8]].concat()), vec!["abcdef.mkv"]);
        assert_eq!(unicode(&[&b"ab\0"[..], &[0x04, 0b1100_0000, 0x7f]].concat()), vec!["ab"]);
    }

    #[test]
    fn a_real_rar5_password_is_told_from_a_wrong_one() {
        for sample in [
            include_bytes!("../../tests/fixtures/rar5/rar5-psw.rar").as_slice(),
            include_bytes!("../../tests/fixtures/rar5/rar5-hpsw.rar").as_slice(),
        ] {
            let l = rar::list(sample);
            let check = l.password_check.expect("a check value");
            assert_eq!(check.accepts("password"), Some(true));
            assert_eq!(check.accepts("wrong"), Some(false));
            assert_eq!(PasswordTest::from_bytes(&check.to_bytes()), Some(check));
        }
        let many = rar::PasswordCheck { kdf: 30, salt: [0; 16], check: [0; 8] };
        assert_eq!(many.accepts("x"), None, "too slow to try");
    }

    #[test]
    fn rar_encryption() {
        let l = rar::list(&[rar4(&[r4_main(0x0080)]), vec![0xaa; 64]].concat());
        assert!(l.encrypted && l.headers_encrypted && l.files.is_empty());
        assert!(l.encryption.is_some());
        assert_eq!(l.password(), Password::Yes);
        let l = rar::list(&rar4(&[r4_main(0), r4_file(0x0004, b"locked.mkv", b"x"), r4_end()]));
        assert!(l.encrypted && !l.headers_encrypted);
        assert_eq!(names(&l), vec!["locked.mkv"]);
        let l = rar::list(&[rar5(&[r5_crypt_header()]), vec![0xaa; 64]].concat());
        assert!(l.encrypted && l.headers_encrypted && l.files.is_empty());
        assert!(l.encryption.is_some());
        let l = rar::list(&rar5(&[r5_main(), r5_file(2, 4, b"locked.mkv", &r5_crypt_record(), b"x"), r5_end()]));
        assert!(l.encrypted);
        assert_eq!(names(&l), vec!["locked.mkv"]);
    }

    #[test]
    fn rar_old_main_header_with_a_comment() {
        // the CRC covers its first 13 bytes, and 5 comment bytes sit inside HEAD_SIZE
        let mut main = vec![0u8; 18];
        main[2] = 0x73;
        main[3..5].copy_from_slice(&0x0002u16.to_le_bytes());
        main[5..7].copy_from_slice(&18u16.to_le_bytes());
        let c = crc32(&main[2..13]) as u16;
        main[0..2].copy_from_slice(&c.to_le_bytes());
        let l = rar::list(&rar4(&[main, r4_file(0, b"a.mkv", b"x"), r4_end()]));
        assert!(!l.damaged);
        assert_eq!(names(&l), vec!["a.mkv"]);
    }

    #[test]
    fn rar_every_cut() {
        every_cut(&first4(), rar::list);
        every_cut(&first5(), rar::list);
        every_cut(&rar4(&[r4_main(0), r4_file(0x0200, "Фильм2.mkv".as_bytes(), b"x"), r4_end()]), rar::list);
        every_cut(&rar4(&[r4_main(0), r4_file(0x0004, b"locked.mkv", b"x"), r4_end()]), rar::list);
        every_cut(&rar5(&[r5_main(), r5_file(2, 4, b"locked.mkv", &r5_crypt_record(), b"x"), r5_end()]), rar::list);
    }

    fn raw5(body: &[u8]) -> Vec<u8> {
        [R5_MARK, &crc32(body).to_le_bytes(), body].concat()
    }

    #[test]
    fn rar_malicious_table() {
        let l = rar::list(&[rar4(&[r4_main(0)]), b"\0\0\x74\0\0\x06\0".to_vec()].concat());
        assert!(l.damaged);
        let l = rar::list(&[rar4(&[r4_main(0)]), b"\0\0\x74\0\x80\xff\xff\0\0\0\0".to_vec()].concat());
        assert!(l.files.is_empty() && !l.damaged && !l.complete);

        let mut fields = r4_file_fields(b"abc", 3);
        fields[15..17].copy_from_slice(&[0xff, 0xff]);
        assert!(rar::list(&rar4(&[r4_main(0), r4_block(0x74, 0, &fields, Some(3))])).damaged);

        let base = r4_file_fields(b"abc", 0);
        let big = [&base[..21], &[0xff; 8], &base[21..]].concat();
        let l = rar::list(&rar4(&[r4_main(0), r4_block(0x74, 0x0100, &big, Some(u32::MAX))]));
        assert_eq!(names(&l), vec!["abc"]);
        assert_eq!(l.files[0].size, Some(0xffff_ffff_0000_0000));
        assert!(!l.complete);

        let mut flipped = rar4(&[r4_main(0), r4_file(0, b"a", b"")]);
        *flipped.last_mut().unwrap() ^= 0xff;
        assert!(rar::list(&flipped).damaged);

        let old: Vec<Vec<u8>> =
            std::iter::once(r4_main(0)).chain((0..100_000).map(|_| r4_block(0x75, 0, &[], None))).collect();
        let l = rar::list(&rar4(&old));
        assert!(!l.complete);

        let many: Vec<Vec<u8>> =
            std::iter::once(r4_main(0)).chain((0..5_000).map(|i| r4_file(0, format!("{i}").as_bytes(), b""))).collect();
        let l = rar::list(&rar4(&many));
        assert_eq!(l.files.len(), MAX_ENTRIES);
        assert!(!l.complete);

        let l = rar::list(&rar4(&[
            r4_main(0),
            r4_block(0x74, 0x00e0, &r4_file_fields(b"dir", 0), Some(0)),
            r4_file(0, b"f", b""),
        ]));
        assert_eq!(names(&l), vec!["f"]);

        assert!(rar::list(&raw5(&[&[0x80, 0x80, 0x80, 0x01][..], &[0; 8]].concat())).damaged);
        let l = rar::list(&raw5(&[0xff, 0x7f]));
        assert!(l.files.is_empty() && !l.damaged);
        assert!(rar::list(&raw5(&[&[0x80; 11][..], &[0]].concat())).damaged);

        let long_name = [&vint(0)[..], &vint(0), &vint(0x20), &[0, 0], &vint(u64::MAX)].concat();
        assert!(rar::list(&rar5(&[r5_block(2, &long_name, &[], None)])).damaged);

        let fields = r5_file_fields(4, b"a", 0);
        let inner = [&vint(2)[..], &vint(2), &vint(u64::MAX), &fields].concat();
        let body = [vint(inner.len() as u64), inner].concat();
        let l = rar::list(&raw5(&body));
        assert_eq!(names(&l), vec!["a"]);
        assert!(!l.complete && !l.damaged);

        let inner = [&vint(2)[..], &vint(1), &vint(5000), &fields].concat();
        let body = [vint(inner.len() as u64), inner].concat();
        assert!(rar::list(&raw5(&body)).damaged);

        let l = rar::list(&rar5(&[r5_block(2, &r5_file_fields(4, b"a", 0), &[0, 0, 0], None)]));
        assert_eq!(names(&l), vec!["a"]);
        assert!(!l.encrypted);
    }
}

mod zip_tests {
    use super::*;
    use crate::archive::zip;

    fn z() -> Vec<u8> {
        zip_of(&[("Example.Movie.2024.nfo", b"note"), ("Example.Movie.2024.mkv", b"hello world")])
    }

    #[test]
    fn zip_listing() {
        let z = z();
        let head = zip::list_head(&z);
        assert_eq!(names(&head), vec!["Example.Movie.2024.nfo", "Example.Movie.2024.mkv"]);
        assert!(!head.complete);
        let tail = zip::list_tail(&z);
        assert_eq!(names(&tail), names(&head));
        assert_eq!((tail.files[0].size, tail.files[1].size), (Some(4), Some(11)));
        assert!(tail.complete);
        assert!(zip::list(&z[..40], &z).complete);
    }

    #[test]
    fn zip_every_cut() {
        let z = z();
        every_cut(&z, zip::list_head);
        let full = names(&zip::list_tail(&z));
        for i in 0..z.len() {
            let part = names(&zip::list_tail(&z[i..]));
            assert!(full.ends_with(&part), "suffix {i}: {part:?}");
        }
    }

    #[test]
    fn zip_malicious_table() {
        let mut bad = zip_local(0, 0, b"abc", b"");
        bad[26..28].copy_from_slice(&[0xff, 0xff]);
        assert!(zip::list_head(&bad).files.is_empty());
        let mut bad = zip_local(0, 0, b"abc", b"");
        bad[18..22].copy_from_slice(&[0xff; 4]);
        assert_eq!(names(&zip::list_head(&bad)), vec!["abc"]);

        let mut fake = zip_of(&[("a", b"x")]);
        let at = fake.len() - 2;
        fake[at..].copy_from_slice(&26u16.to_le_bytes());
        fake.extend(b"PK\x05\x06");
        fake.extend([0u8; 22]);
        assert_eq!(names(&zip::list_tail(&fake)), vec!["a"]);

        let two = zip_of(&[("a", b"x"), ("b", b"y")]);
        let l = zip::list_tail(&two[70..]);
        assert_eq!(names(&l), vec!["b"]);
        assert!(!l.complete);

        let mut central = zip_central(0, 0, b"a", b"x", 0);
        central[28..30].copy_from_slice(&[0xff, 0xff]);
        let data = [central.clone(), zip_eocd(1, central.len() as u32, 0, b"")].concat();
        assert!(zip::list_tail(&data).files.is_empty());

        for (flags, method) in [(1, 0), (0x40, 0), (0x2000, 0), (0, 99)] {
            let l = zip::list_head(&zip_local(flags, method, b"a", b""));
            assert!(l.encrypted, "{flags} {method}");
            let want = if method == 99 { Encryption::ZipAes } else { Encryption::ZipCrypto };
            assert_eq!(l.encryption, Some(want), "{flags} {method}");
        }
        let data = [zip_local(8, 8, b"a", b""), zip_local(0, 0, b"b", b"")].concat();
        assert_eq!(names(&zip::list_head(&data)), vec!["a"]);
        let data = [&b"PK\x07\x08"[..], &zip_local(0, 0, b"a", b"")].concat();
        assert_eq!(names(&zip::list_head(&data)), vec!["a"]);

        let many: Vec<(String, &[u8])> = (0..5_000).map(|i| (format!("{i}"), &b""[..])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        let l = zip::list_tail(&zip_of(&refs));
        assert_eq!(l.files.len(), MAX_ENTRIES);
        assert!(!l.complete);

        let mut z64 = zip_local(0, 0, b"a", b"");
        z64[18..26].copy_from_slice(&[0xff; 8]);
        z64[28] = 8;
        z64.extend([0x01, 0x00, 0x04, 0x00, 0xaa, 0xbb, 0xcc, 0xdd]);
        let l = zip::list_head(&z64);
        assert_eq!(names(&l), vec!["a"]);
        assert!(!l.complete);
    }
}

mod sevenz_tests {
    use super::*;
    use crate::archive::sevenz;

    fn plain() -> Vec<u8> {
        sz_wrap(b"hello world", &sz_plain_header(11, &[11], &["Example.Movie.2024.mkv"], None))
    }

    /// `NUMBER` in its shortest form
    fn shortest(n: u64) -> Vec<u8> {
        for extra in 0..8u32 {
            let bits = 7 - extra + 8 * extra;
            if n < 1u64 << bits {
                let high = (n >> (8 * extra)) as u8;
                let lead = !(0xffu16 >> extra) as u8;
                let mut o = vec![lead | high];
                o.extend_from_slice(&n.to_le_bytes()[..extra as usize]);
                return o;
            }
        }
        [&[0xff][..], &n.to_le_bytes()].concat()
    }

    #[test]
    fn sevenz_plain_and_sizes() {
        let l = sevenz::list(&plain(), &[]);
        assert_eq!(names(&l), vec!["Example.Movie.2024.mkv"]);
        assert_eq!(l.files[0].size, Some(11));
        assert!(l.complete);
        let s =
            sz_wrap(b"hello world", &sz_plain_header(11, &[5, 6], &["a/one.mkv", "a/two.nfo", "a"], Some(0b0010_0000)));
        let l = sevenz::list(&s, &[]);
        assert_eq!(names(&l), vec!["a/one.mkv", "a/two.nfo"]);
        assert_eq!((l.files[0].size, l.files[1].size), (Some(5), Some(6)));
    }

    #[test]
    fn sevenz_number_round_trip() {
        for n in [0, 0x7f, 0x80, 0x3fff, 0x4000, 0x1f_ffff, 0x20_0000, u64::from(u32::MAX), u64::MAX] {
            let b = shortest(n);
            let mut pos = 0;
            assert_eq!(sevenz::number(&b, &mut pos), Some(n), "{n:x}");
            assert_eq!(pos, b.len(), "{n:x}");
        }
    }

    #[test]
    fn sevenz_encoded_by_the_crates_encoder() {
        let s = sz_encoded(&vec![7u8; 100_000], &["Example.Movie.2024.mkv", "Example.Movie.2024.nfo"]);
        assert!(sevenz::list(&s[..4096], &[]).need_more_tail);
        let l = sevenz::list(&s[..4096], &s[s.len() - 2048..]);
        assert_eq!(names(&l), vec!["Example.Movie.2024.mkv", "Example.Movie.2024.nfo"]);
        assert!(sevenz::list(&s[..4096], &s[s.len() - 40..]).need_more_tail);
        let split = s.len() - 12;
        assert!(sevenz::list(&s[..4096], &s[split..]).need_more_tail);
        let joined = [&s[s.len() - 3000..split], &s[split..]].concat();
        assert_eq!(names(&sevenz::list(&s[..4096], &joined)).len(), 2);
    }

    #[test]
    fn sevenz_encoded_by_a_real_writer() {
        let hex = "377abcaf271c0003c146c887d5000000000000002200000000000000ee4de10a003c6abdffffffffe000000000008133\
                   07ae0fce9c06c5092aaed56264db4fa49b7ad638b48e63717ceace6378de0d3a5df9c3539d62c1afb597588e0db18e5b\
                   1c9a761f4ac31cf907f762e08e7dffa163afd8a80a6503b3e11d9bcd7d0865bf93542f8eb455a5e4e9d46ff627f29ea4\
                   e800a40f213d81cae271375a43e8f1ffeebbe95fc9a80d63d598df3cfcad79a84feaff786bc7d45c3ecba6dc230d80da\
                   dd69d17f26390b75f4753563e3ac5f574da7c13de66cb710ccb52a4d827dc02ef9d077db91b92163e31943989d8c5509\
                   ffc3fd300017060c010980c900070b01000123030101055d000080000c83710a01be81fe670000";
        let b: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
        assert_eq!(b.len(), 279);
        let want: Vec<String> = (1..=8).map(|n| format!("Example.Show.2024.S01E0{n}.1080p.mkv")).collect();
        let l = sevenz::list(&b, &[]);
        assert_eq!(names(&l), want);
        assert!(l.files.iter().all(|f| f.size == Some(1)));
        assert_eq!(names(&sevenz::list(&b[..32], &b[44..])), want);
        assert!(sevenz::list(&b[..32], &b[60..]).need_more_tail);
    }

    #[test]
    fn sevenz_encrypted() {
        let l = sevenz::list(&sz_wrap(&[0; 64], &sz_encrypted_header(0, 64)), &[]);
        assert_eq!(l.encryption, Some(Encryption::SevenZipAes256));
        assert!(l.encrypted && l.headers_encrypted && l.files.is_empty());
        assert!(l.encryption.is_some());
        assert_eq!(l.password(), Password::Yes);
        let next = [&[0x01][..], &sz_main_streams(5, &[5], SZ_AES_DATA), &sz_files(&["a.mkv"], None), &[0x00]].concat();
        let l = sevenz::list(&sz_wrap(b"xxxxx", &next), &[]);
        assert!(l.encrypted && !l.headers_encrypted);
        assert_eq!(names(&l), vec!["a.mkv"]);
    }

    #[test]
    fn sevenz_cuts() {
        let s = sz_encoded(&vec![7u8; 5000], &["a.mkv", "b.nfo"]);
        let full = vec!["a.mkv".to_string(), "b.nfo".to_string()];
        for i in 0..32 {
            assert!(sevenz::list(&s[..i], &s).files.is_empty());
        }
        for i in 32..s.len() {
            assert_eq!(names(&sevenz::list(&s[..i], &s)), full, "head {i}");
        }
        let mut any = false;
        for i in 0..s.len() {
            let got = names(&sevenz::list(&s[..32], &s[i..]));
            assert!(got.is_empty() || got == full, "tail {i}");
            any |= got == full;
        }
        assert!(any);
        let p = plain();
        for i in 0..p.len() {
            assert!(sevenz::list(&p[..i], &[]).files.is_empty(), "cut {i}");
        }
    }

    fn wrap(next: &[u8]) -> Listing {
        sevenz::list(&sz_wrap(b"", next), &[])
    }

    #[test]
    fn sevenz_malicious_table() {
        let mut s = plain();
        s[8] ^= 1;
        assert!(sevenz::list(&s, &[]).damaged);
        let mut s = plain();
        *s.last_mut().unwrap() ^= 1;
        assert!(sevenz::list(&s, &[]).damaged);

        let mut start = [0u8; 20];
        start[8..16].copy_from_slice(&(1u64 << 40).to_le_bytes());
        let huge = [&b"7z\xbc\xaf\x27\x1c\x00\x04"[..], &crc32(&start).to_le_bytes(), &start].concat();
        assert!(sevenz::list(&huge, &[]).damaged);

        let max: &[u8] = &[0xff; 9];
        assert!(wrap(&[&[0x01, 0x05][..], max, &[0x00, 0x00]].concat()).damaged);
        assert!(wrap(&[0x01, 0x05, 0x01, 0x11, 0x03, 0x00, 0x61, 0x00, 0x00, 0x00]).damaged);
        assert!(wrap(&[0x01, 0x05, 0x01, 0x11, 0x02, 0x01, 0x00, 0x00, 0x00]).damaged);
        assert!(wrap(&[&[0x01, 0x04, 0x07, 0x0b, 0x01, 0x00][..], max].concat()).damaged);
        assert!(wrap(&[&[0x01, 0x04, 0x07, 0x0b][..], max].concat()).damaged);

        let props = [0x5d, 0, 0, 0x10, 0];
        assert!(sevenz::list(&sz_wrap(&[0; 10], &sz_encoded_header(0, 10, 1 << 40, &props, 0)), &[]).damaged);
        let dict = [0x5d, 0xff, 0xff, 0xff, 0xff];
        assert!(sevenz::list(&sz_wrap(&[0x55; 10], &sz_encoded_header(0, 10, 100, &dict, 0)), &[]).damaged);
        assert!(sevenz::list(&sz_wrap(&[0; 10], &sz_encoded_header(u64::MAX - 1, 10, 100, &props, 0)), &[]).damaged);

        let lzma2 = [
            0x17, 0x06, 0x00, 0x01, 0x09, 0x0a, 0x00, 0x07, 0x0b, 0x01, 0x00, 0x01, 0x21, 0x21, 0x01, 0x18, 0x0c, 0x64,
            0x00, 0x00,
        ];
        let l = sevenz::list(&sz_wrap(&[0; 10], &lzma2), &[]);
        assert!(l.damaged);
        assert_eq!(l.password(), Password::Maybe);

        // packed over and over
        let nest = |times: usize| {
            let mut header = [&[0x01][..], &sz_files(&["a.mkv"], None), &[0x00]].concat();
            let mut packed_all: Vec<u8> = Vec::new();
            for _ in 0..times {
                let (props, packed) = lzma_raw(&header);
                let at = packed_all.len() as u64;
                header = sz_encoded_header(at, packed.len() as u64, header.len() as u64, &props, crc32(&header));
                packed_all.extend(packed);
            }
            sz_wrap(&packed_all, &header)
        };
        let l = sevenz::list(&nest(3), &[]);
        assert_eq!(names(&l), vec!["a.mkv"]);
        assert!(sevenz::list(&nest(6), &[]).damaged);

        let many: Vec<String> = (0..5_000).map(|i| format!("{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let next = [&[0x01][..], &sz_files(&refs, None), &[0x00]].concat();
        let l = wrap(&next);
        assert_eq!(l.files.len(), MAX_ENTRIES);
        assert!(!l.complete);
    }
}

mod select_tests {
    use super::*;

    fn file(name: &str, parts: i64) -> FileInfo {
        FileInfo {
            id: 1,
            filename: name.into(),
            subject: None,
            subject_part: None,
            expected: Some(parts),
            file_total: None,
            parts_seen: parts,
            touched_at: None,
            sealed: false,
        }
    }

    fn heads<'a>(plan: &[Head<'a>]) -> Vec<&'a str> {
        plan.iter().map(|h| h.file.filename.as_str()).collect()
    }

    #[test]
    fn select_classify_table() {
        use Kind::*;
        let table: &[(&str, Kind)] = &[
            ("a.par2", Par2Base),
            ("A.PAR2", Par2Base),
            ("a.vol000+01.par2", Par2Vol(0)),
            ("a.vol03-07.par2", Par2Vol(3)),
            ("a.part001.rar", Rar(1)),
            ("a.part1.rar", Rar(1)),
            ("a.part12.rar", Rar(12)),
            ("a.rar", Rar(0)),
            ("a.cbr", Rar(0)),
            ("a.r00", Rar(1)),
            ("a.r99", Rar(100)),
            ("a.s00", Rar(1001)),
            ("a.7z", SevenZip(0)),
            ("a.7z.001", SevenZip(1)),
            ("a.7z.012", SevenZip(12)),
            ("a.zip", Zip(u32::MAX)),
            ("a.zipx", Zip(u32::MAX)),
            ("a.cbz", Zip(u32::MAX)),
            ("a.z01", Zip(1)),
            ("a.001", Numbered(1)),
            ("a.mkv.001", Numbered(1)),
            ("a.000", Numbered(0)),
            ("a.nfo", Nfo),
            ("a.txt", Nfo),
            ("a.jpg", Image),
            ("a.JPEG", Image),
            ("a.tif", Image),
            ("a.mkv", NotArchive),
            ("a.sfv", NotArchive),
            ("a.iso", NotArchive),
            ("a1b2c3d4e5f6g7h8", Unidentified),
            ("a.dat", Unidentified),
            ("a.xk29dj3", Unidentified),
            ("", Unidentified),
        ];
        for (name, kind) in table {
            assert_eq!(classify(name), *kind, "{name}");
        }
    }

    #[test]
    fn an_xref_uploads_volumes_are_its_files_of_the_part_count_most_share() {
        let f = [
            file("u-00001", 1),
            file("u-00003", 293),
            file("u-00002", 293),
            file("u-00004", 91),
            file("u-00005", 469),
            file("u-00006", 293),
        ];
        assert_eq!(
            volumes(&f).iter().map(|f| f.filename.as_str()).collect::<Vec<_>>(),
            ["u-00002", "u-00003", "u-00006"]
        );
        // no two alike: the largest
        assert_eq!(volumes(&[file("a", 10), file("b", 30), file("c", 2)])[0].filename, "b");
        assert!(volumes(&[file("a", 1), file("b", 2)]).is_empty());
    }

    #[test]
    fn an_xref_uploads_guesses_go_volume_by_volume_round_by_round() {
        let seg = |name: &str, part: i64| crate::store::SegmentRef {
            message_id: format!("{name}-{part}"),
            part: Some(part),
            bytes: 1,
        };
        // volumes with different counts of leads: every lead is tried once, over the rounds
        let many: Vec<FileInfo> = (0..20).map(|k| file(&format!("v{k:02}"), 293)).collect();
        let vols: Vec<&FileInfo> = many.iter().collect();
        let leads = |v: &FileInfo| {
            let n = if v.filename.ends_with('3') { 12 } else { 6 };
            Ok::<_, ()>((1..=n).map(|p| seg(&v.filename, p)).collect::<Vec<_>>())
        };
        let total = 18 * 6 + 2 * 12;
        let mut tried = Vec::new();
        let mut round = 0;
        loop {
            let (got, left) = super::super::steps::xref_order(&vols, round * crate::xref::HEAD_PROBES, leads).unwrap();
            assert!(got.len() <= crate::xref::HEAD_PROBES);
            tried.extend(got.into_iter().map(|(_, s)| s.message_id));
            round += 1;
            if !left {
                break;
            }
        }
        assert_eq!(tried.len(), total);
        assert_eq!(tried.iter().collect::<std::collections::HashSet<_>>().len(), total, "none twice");
        assert_eq!(tried[0], "v00-1");
    }

    #[test]
    fn select_plan_table() {
        let f = [file("x.part02.rar", 5), file("x.part01.rar", 5), file("x.part03.rar", 5)];
        let p = plan(&f, 2);
        assert_eq!(heads(&p), vec!["x.part01.rar", "x.part02.rar"]);
        assert_eq!(p[0].tail.filename, "x.part03.rar");
        assert_eq!(heads(&plan(&[file("x.r00", 5), file("x.rar", 5), file("x.r01", 5)], 2)), vec!["x.rar", "x.r00"]);

        let f = [file("x.7z.002", 5), file("x.7z.001", 5), file("x.7z.003", 5)];
        let p = plan(&f, 2);
        assert_eq!(heads(&p), vec!["x.7z.001"]);
        assert_eq!(p[0].tail.filename, "x.7z.003");
        let f = [file("x.7z", 5)];
        assert_eq!(plan(&f, 2)[0].tail.filename, "x.7z");
        let f = [file("x.z02", 5), file("x.zip", 5), file("x.z01", 5)];
        let p = plan(&f, 2);
        assert_eq!(heads(&p), vec!["x.z01"]);
        assert_eq!(p[0].tail.filename, "x.zip");

        assert_eq!(
            heads(&plan(&[file("small.rar", 5), file("big.part01.rar", 200)], 2)),
            vec!["big.part01.rar", "small.rar"]
        );
        let three = [file("a.rar", 5), file("b.rar", 6), file("c.rar", 7)];
        assert_eq!(heads(&plan(&three, 2)), vec!["c.rar", "b.rar"]);

        assert_eq!(heads(&plan(&[file("x.part01.rar", 5), file("a1b2c3d4", 500)], 2)), vec!["x.part01.rar"]);

        let f = [file("aaaa1", 500), file("aaaa2", 900), file("aaaa3", 1)];
        let p = plan(&f, 2);
        assert_eq!(heads(&p), vec!["aaaa2", "aaaa1"]);
        assert!(p.iter().all(|h| h.probe && h.tail.filename == h.file.filename));
        assert!(plan(&[file("aaaa1", 2)], 2).is_empty());
        assert_eq!(plan(&[file("aaaa1", 3)], 2).len(), 1);

        assert!(plan(&[file("aaaa1", 500), file("a.jpg", 1), file("b.jpg", 1)], 2).is_empty());
        assert_eq!(plan(&[file("aaaa1", 500), file("a.mkv", 1), file("a.jpg", 1)], 2).len(), 1);
        assert!(plan(&[file("aaaa1", 500)], 0).is_empty());
        assert!(plan(&[file("a.mkv", 500), file("a.par2", 1)], 2).is_empty());
    }

    #[test]
    fn select_par2_source_table() {
        let f = [file("a.vol000+01.par2", 1), file("a.par2", 1)];
        assert_eq!(par2_source(&f).unwrap().filename, "a.par2");
        let f = [file("b.par2", 3), file("a.par2", 1)];
        assert_eq!(par2_source(&f).unwrap().filename, "a.par2");
        let f = [file("a.vol003+04.par2", 1), file("a.vol000+01.par2", 9)];
        assert_eq!(par2_source(&f).unwrap().filename, "a.vol000+01.par2");
        assert!(par2_source(&[file("a.mkv", 1)]).is_none());
        // a whole volume before a base missing an article, the release's own set before its sample's
        let short = FileInfo { parts_seen: 1, ..file("a.par2", 2) };
        let f = [short, file("a.vol003+04.par2", 4), file("a.vol000+01.par2", 1), file("a-sample.vol0+1.par2", 1)];
        let order: Vec<&str> = par2_sources(&f).iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(order, ["a.vol000+01.par2", "a.vol003+04.par2", "a-sample.vol0+1.par2", "a.par2"]);
    }

    #[test]
    fn select_nfo_sources_table() {
        let by_subject = FileInfo { subject: Some("\"x.nfo\" yEnc (1/1)".into()), ..file("aaaa", 1) };
        assert_eq!(nfo_sources(std::slice::from_ref(&by_subject)).len(), 1);
        let f = [file("c.nfo", 3), file("a.nfo", 2), file("b.txt", 1)];
        let got: Vec<&str> = nfo_sources(&f).iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(got, vec!["b.txt", "a.nfo"]);
    }

    #[test]
    fn stage_owns_names_table() {
        use crate::config::{ArchiveConfig, Config, PostprocessConfig};
        let config = |enabled: bool, budget: i64, archive: bool| Config {
            postprocess: PostprocessConfig { enabled, body_fetches_per_minute: budget, ..Default::default() },
            archive: ArchiveConfig { enabled: archive, ..Default::default() },
            ..Config::default()
        };
        assert!(stage_owns_names(&Config::default()), "an existing config.toml");
        assert!(!stage_owns_names(&config(false, 300, true)), "the stage is off");
        assert!(stage_owns_names(&config(true, 300, true)), "a body budget");
        assert!(!stage_owns_names(&config(true, 300, false)), "the budget serves other parts");
        assert!(!stage_owns_names(&config(true, 0, true)), "no budget");
    }

    #[test]
    fn inner_files_ask_for_naming_and_predb() {
        use crate::postprocess::step::{StepId, mask};
        assert_ne!(RERUN_ON_INNER_FILES & mask(StepId(8)), 0);
        assert_ne!(RERUN_ON_INNER_FILES & mask(StepId(16)), 0);
    }

    #[test]
    fn two_digit_numbered_volumes_share_a_set() {
        let f = [file("x.03", 3), file("x.01", 3), file("x.02", 3)];
        let heads = plan(&f, 2);
        assert_eq!(heads.len(), 1, "{heads:?}");
        assert_eq!(heads[0].file.filename, "x.01");
        assert_eq!(heads[0].tail.filename, "x.03");
    }

    #[test]
    fn rar5_header_and_stream_decryption_with_password() {
        let hpsw = std::fs::read("tests/fixtures/rar5/rar5-hpsw.rar").unwrap();
        let without = rar::list(&hpsw);
        assert!(without.encrypted && without.headers_encrypted && without.files.is_empty());
        let wrong = rar::list_with_password(&hpsw, Some("wrong"));
        assert!(wrong.encrypted && wrong.headers_encrypted && wrong.files.is_empty() && !wrong.damaged);
        let psw = std::fs::read("tests/fixtures/rar5/rar5-psw.rar").unwrap();
        let right = rar::list_with_password(&hpsw, Some("password"));
        assert!(right.encrypted && right.headers_encrypted && !right.damaged);
        assert_eq!(right.files, rar::list(&psw).files);

        let nest_no_pw = crate::probe::nest::unwrap(&psw);
        assert_eq!(nest_no_pw.layers[0].stop.as_deref(), Some("encrypted"));
        let nest_wrong = crate::probe::nest::unwrap_with_password(&psw, Some("wrong"));
        assert_eq!(nest_wrong.layers[0].stop.as_deref(), Some("encrypted"));
        let nest_right = crate::probe::nest::unwrap_with_password(&psw, Some("password"));
        assert!(nest_right.layers[0].stop.is_none(), "{:?}", nest_right.layers);
        assert!(!nest_right.inner.is_empty());

        let nest_hp = crate::probe::nest::unwrap_with_password(&hpsw, Some("password"));
        assert!(nest_hp.layers[0].stop.is_none(), "{:?}", nest_hp.layers);
        assert_eq!(nest_hp.inner.as_ref(), nest_right.inner.as_ref());
    }

    #[test]
    fn release_161580499_password_check_and_verified_harvester() {
        // Exact password_check blob from Release 161580499 (6GTX0B5E4O2GARHV9KPW on shard 3)
        let raw_check: [u8; 25] = [
            0x0f, 0xcc, 0xf5, 0x00, 0x61, 0xff, 0xba, 0xd4, 0x3d, 0xcf, 0x68, 0x42, 0xb1, 0x49, 0xd5, 0xf5, 0xa2, 0xe7,
            0x82, 0x9b, 0x3a, 0xab, 0xe0, 0x37, 0xb0,
        ];
        let test = super::PasswordTest::from_bytes(&raw_check).unwrap();
        assert_eq!(test.accepts("F-UnK-NZBNF"), Some(true));
        assert_eq!(test.accepts("wrong"), Some(false));

        // NFO with false positives before the real password, or bare line password verified by PasswordTest
        let noisy_nfo =
            "Encoding settings: cabac=1 / pass=2 / crf=18\nPasswort: keins\nTemplate {{password}}\n\nF-UnK-NZBNF\n";
        assert_eq!(crate::release_info::find_password("Encoding settings: pass=2"), None);
        assert_eq!(crate::release_info::find_password("Template {{password}}"), None);
        assert_eq!(crate::release_info::find_password("Passwort: keins"), None);
        assert_eq!(crate::release_info::find_password("Passwort: Datum"), None);
        assert_eq!(
            crate::release_info::find_verified_password([noisy_nfo], &[], Some(&test)).as_deref(),
            Some("F-UnK-NZBNF")
        );

        // Release 652163000 (VariousPics-Part19): "RAR Passwurd: variouspics" in a 114-byte subject
        let raw_652163000: [u8; 25] = [
            0x0f, 0xae, 0x2e, 0x03, 0xb4, 0xb8, 0x07, 0x34, 0x5a, 0xb4, 0xa1, 0x35, 0x2a, 0xb8, 0x0f, 0xee, 0x6b,
            0xd5, 0xb1, 0x8b, 0xbd, 0xd4, 0x27, 0x06, 0x0a,
        ];
        let check_652163000 = super::PasswordTest::from_bytes(&raw_652163000).unwrap();
        let subj = r#"Various pictures - Part 19 - RAR Passwurd: variouspics [003/371] "VariousPics-Part19.part001.rar" yEnc (0001/1093)"#;
        assert_eq!(
            crate::release_info::find_verified_password([subj], &[], Some(&check_652163000)).as_deref(),
            Some("variouspics")
        );
        // Even an arbitrary unrecognized label before ':' in a long subject is verified via the broad token scan
        let unlabeled_subj = r#"Various pictures - Part 19 - RAR Key: variouspics [003/371] "VariousPics-Part19.part001.rar" yEnc (0001/1093)"#;
        assert_eq!(
            crate::release_info::find_verified_password([unlabeled_subj], &[], Some(&check_652163000)).as_deref(),
            Some("variouspics")
        );
    }

    #[test]
    fn nzb_meta_password_and_rar4_and_zip_decryption() {
        let nzb_xml = br#"<?xml version="1.0" encoding="utf-8"?>
            <nzb><head><meta type="password">JFB9hbdCrue7m4XA391gNYM50</meta></head>
            <file poster="a@b" date="1" subject="zDwd4n3PFKu1gLc5RL4M.part01.rar (1/2)">
              <groups><group>alt.binaries.boneless</group></groups>
              <segments><segment bytes="100" number="1">m1@x</segment></segments>
            </file></nzb>"#;
        assert_eq!(super::nzb::meta_password(nzb_xml).as_deref(), Some("JFB9hbdCrue7m4XA391gNYM50"));

        // RAR4 encrypted stored file decryption
        let plain = b"RAR4 secret payload for probe test!";
        let salt = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let (key, iv) = rar::derive_rar4_key_iv("rar4pass", &salt);
        // Encrypt `plain` (padded to 48 bytes) with AES-128-CBC:
        let aes = super::aes::Aes::new(&key).unwrap();
        let mut padded = plain.to_vec();
        padded.resize(48, 0);
        let mut ct = Vec::new();
        let mut prev = iv;
        for chunk in padded.as_chunks::<16>().0 {
            let mut blk = [0u8; 16];
            for i in 0..16 {
                blk[i] = chunk[i] ^ prev[i];
            }
            aes.encrypt_block(&mut blk);
            prev = blk;
            ct.extend_from_slice(&blk);
        }
        // Build RAR4 file block with LHD_PASSWORD (0x04) and LHD_SALT (0x400), method 0x30 (stored)
        let mut body = Vec::new();
        body.extend_from_slice(&(plain.len() as u32).to_le_bytes()); // unp_size
        body.push(2); // host_os
        body.extend_from_slice(&super::crc32(plain).to_le_bytes()); // file_crc
        body.extend_from_slice(&0u32.to_le_bytes()); // ftime
        body.push(29); // unp_ver
        body.push(0x30); // method = m0 (stored)
        let fname = b"secret.bin";
        body.extend_from_slice(&(fname.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes()); // attr
        body.extend_from_slice(fname);
        body.extend_from_slice(&salt);
        let file_blk = [super::testkit::r4_block(0x74, 0x0004 | 0x0400, &body, Some(ct.len() as u32)), ct].concat();
        let r4 = super::testkit::rar4(&[super::testkit::r4_main(0), file_blk]);
        assert_eq!(crate::probe::nest::unwrap(&r4).layers[0].stop.as_deref(), Some("encrypted"));
        assert_eq!(
            crate::probe::nest::unwrap_with_password(&r4, Some("wrong")).layers[0].stop.as_deref(),
            Some("encrypted")
        );
        let dec_r4 = crate::probe::nest::unwrap_with_password(&r4, Some("rar4pass"));
        assert!(dec_r4.layers[0].stop.is_none());
        assert_eq!(dec_r4.inner.as_ref(), plain);

        let nzb_ent = br#"<nzb><head><meta type="password">abc&amp;123&#33;</meta></head></nzb>"#;
        assert_eq!(super::nzb::meta_password(nzb_ent).as_deref(), Some("abc&123!"));
        assert_eq!(crate::release_info::find_password("Movie.{{!@#$}}.nzb").as_deref(), Some("!@#$"));
        let z_chk = super::PasswordTest::Zip(super::zip::PasswordCheck::Crypto { header: [0; 12], check: 0x99 });
        assert_eq!(crate::release_info::find_verified_password(["password: wrong"], &[], Some(&z_chk)), None);
    }

    #[test]
    fn par2_name_map_keeps_verified_names_and_bounded_wire_size_matching() {
        let files = vec![
            FileInfo {
                id: 1,
                filename: "a.part01.rar".into(),
                subject: None,
                subject_part: None,
                expected: Some(50),
                file_total: None,
                parts_seen: 50,
                touched_at: None,
                sealed: false,
            },
            FileInfo {
                id: 2,
                filename: "obf_nfo_999".into(),
                subject: None,
                subject_part: None,
                expected: Some(1),
                file_total: None,
                parts_seen: 1,
                touched_at: None,
                sealed: false,
            },
            FileInfo {
                id: 3,
                filename: "obf_nzb_888".into(),
                subject: None,
                subject_part: None,
                expected: Some(1),
                file_total: None,
                parts_seen: 1,
                touched_at: None,
                sealed: false,
            },
        ];
        let mut out = std::collections::HashMap::new();
        let rows = vec![
            ("a.part01.rar".to_string(), 50_000_000),
            ("secret.nfo".to_string(), 850),
            ("pointer-{{pw}}.nzb".to_string(), 40_000),
        ];
        let (unmatched_files, unmatched_par2) = select::prepare_par2_names(&mut out, &files, rows);
        let wire_sizes = [(2, 900), (3, 42_000)].into_iter().collect();
        let map = select::finish_par2_names(out, unmatched_files, unmatched_par2, &wire_sizes);
        assert_eq!(map.get("obf_nfo_999").map(String::as_str), Some("secret.nfo"));
        assert_eq!(map.get("obf_nzb_888").map(String::as_str), Some("pointer-{{pw}}.nzb"));
        assert_eq!(select::nfo_sources_with_par2(&files, &map)[0].filename, "obf_nfo_999");
        assert_eq!(select::nzb_sources_with_par2(&files, &map)[0].filename, "obf_nzb_888");
    }

}

// The former par2_reruns_go_to_incomplete_movies_and_tv_with_par2_files_and_no_set fixture
// exercised the one-time pp_par2_from_volumes SQLite upgrade repair, retired for fresh native catalogs.
// Current volume fallback/inventory remain in Par2Step; native queue redo and receipt assertions remain
// in pg::stage_queue::walk_tests and archive::link::native_shipping_tests.

// ---------------------------------------------------------------- header facts

/// A RAR4 file block's fields with every fact set: unpacked size, host, CRC, DOS time, version, method, name.
fn r4_fields_with(name: &[u8], host: u8, crc: u32, ftime: u32) -> Vec<u8> {
    [
        &1_000_000u32.to_le_bytes()[..],
        &[host],
        &crc.to_le_bytes(),
        &ftime.to_le_bytes(),
        &[29, 0x33],
        &(name.len() as u16).to_le_bytes(),
        &0x20u32.to_le_bytes(),
        name,
    ]
    .concat()
}

#[test]
fn rar4_keeps_each_files_crc_and_host_and_how_it_was_packed() {
    // 2024-03-10 14:30:20, DOS
    let ftime = ((2024 - 1980) << 9 | 3 << 5 | 10) << 16 | (14 << 11 | 30 << 5 | 10);
    let whole = r4_block(0x74, 0x0020, &r4_fields_with(b"a.mkv", 3, 0x1122_3344, ftime), Some(0));
    // split after: the CRC is only this part's
    let split = r4_block(0x74, 0x0002, &r4_fields_with(b"b.mkv", 2, 0x5566_7788, ftime), Some(0));
    let l = rar::list(&rar4(&[r4_main(0x0008), whole, split, r4_end()]));
    assert_eq!(l.files.len(), 2);
    assert_eq!(
        (l.files[0].crc32, l.files[0].host_os, l.files[0].mtime),
        (Some(0x1122_3344), Some(facts::HOST_UNIX), None)
    );
    assert_eq!((l.files[1].crc32, l.files[1].host_os), (None, Some(facts::HOST_WINDOWS)));
    assert_eq!(l.packer.as_deref(), Some("RAR4 · unpack 2.9 · m3 · dict 128k · Unix · solid"));
    assert_eq!(l.local_wall, Some(1_710_081_020), "the local time is kept for the zone guess only");
    assert_eq!(l.owner, None);
}

#[test]
fn rar4_reads_its_unix_owner_subblock_and_never_prints_it() {
    let mut fields = r4_fields_with(b"UOW", 3, 0, 0);
    fields.extend(b"someone\0staff");
    let uow = r4_block(0x7a, 0, &fields, Some(0));
    let l = rar::list(&rar4(&[r4_main(0), r4_file(0, b"a.mkv", b"x"), uow, r4_end()]));
    let owner = l.owner.clone().expect("an owner");
    assert!(owner.0.contains("someone") && owner.0.contains("staff"));
    assert!(!format!("{l:?}").contains("someone"), "a listing's Debug never shows the owner");
}

#[test]
fn rar4_hp_keeps_its_salt_as_the_archives_twin() {
    let mut data = rar4(&[r4_main(0x0080)]);
    data.extend([9u8; 8]);
    data.extend([0u8; 16]);
    let l = rar::list(&data);
    assert!(l.headers_encrypted);
    assert_eq!(l.twin, Some(facts::Twin::new("rar4hp", &[9; 8])));
}

/// RAR5 file fields with a CRC, a host and a compression word.
fn r5_fields_with(name: &[u8], crc: u32, compression: u64, host: u64) -> Vec<u8> {
    [
        &vint(4)[..],
        &vint(5_000_000),
        &vint(0x20),
        &crc.to_le_bytes(),
        &vint(compression),
        &vint(host),
        &vint(name.len() as u64),
        name,
    ]
    .concat()
}

fn r5_record(ty: u64, body: &[u8]) -> Vec<u8> {
    let b = [vint(ty), body.to_vec()].concat();
    [vint(b.len() as u64), b].concat()
}

#[test]
fn rar5_keeps_each_files_crc_time_and_host_and_its_owner() {
    // HTIME: Unix times with nanoseconds, mtime only
    let htime = r5_record(3, &[&vint(0x13)[..], &1_700_000_000u32.to_le_bytes(), &5u32.to_le_bytes()].concat());
    let owner =
        r5_record(6, &[&vint(0x0f)[..], &vint(4), b"user", &vint(5), b"group", &vint(1000), &vint(100)].concat());
    // stored (method 0), dictionary 2^2 * 128k, RAR 5.0
    let compression = 2 << 10;
    let file =
        r5_block(2, &r5_fields_with(b"a.mkv", 0xcafe_f00d, compression, 1), &[htime, owner].concat(), Some(b"x"));
    let l = rar::list(&rar5(&[r5_main(), file, r5_end()]));
    let f = &l.files[0];
    assert_eq!((f.crc32, f.mtime, f.host_os), (Some(0xcafe_f00d), Some(1_700_000_000), Some(facts::HOST_UNIX)));
    assert_eq!(l.packer.as_deref(), Some("RAR5 · v0 · m0 · dict 512k · Unix · times unix-ns"));
    let owner = l.owner.expect("an owner");
    assert!(owner.0.contains("u=user:g=group:uid=1000:gid=100"));
}

#[test]
fn a_split_files_whole_crc_comes_from_its_last_volume() {
    // RAR4: the first volume's header says split after (0x02), the last's split before (0x01) with the file's CRC
    let first = r4_block(0x74, 0x0002, &r4_fields_with(b"a.mkv", 2, 0x0bad_0bad, 0), Some(0));
    let last = r4_block(0x74, 0x0001, &r4_fields_with(b"a.mkv", 2, 0x1122_3344, 0), Some(0));
    let mut listing = rar::list(&rar4(&[r4_main(0x0001), first]));
    assert_eq!(listing.files[0].crc32, None);
    let closing = rar::list(&rar4(&[r4_main(0x0001), last]));
    assert_eq!(closing.files[0].crc32, Some(0x1122_3344));
    super::steps::close_split_files(&mut listing, &closing);
    assert_eq!(listing.files[0].crc32, Some(0x1122_3344));
    // another file of the same name and size in another set is no match: only `close_split_files`' caller pairs
    // sets, and a different size never closes
    let split = r4_block(0x74, 0x0002, &r4_fields_with(b"a.mkv", 2, 0, 0), Some(0));
    let mut other = rar::list(&rar4(&[r4_main(0x0001), split]));
    other.files[0].size = other.files[0].size.map(|s| s + 1);
    super::steps::close_split_files(&mut other, &closing);
    assert_eq!(other.files[0].crc32, None);
    // RAR5: header flags 0x08 alone, the last part, keep the CRC; 0x10, a part with more after it, don't
    let r5 = |flags: u64| {
        let fields = r5_fields_with(b"a.mkv", 0xcafe_f00d, 0, 0);
        let inner = [&vint(2)[..], &vint(flags | 2), &vint(1), &fields].concat();
        let body = [vint(inner.len() as u64), inner].concat();
        let block = [&crc32(&body).to_le_bytes()[..], &body, b"x"].concat();
        rar::list(&rar5(&[r5_main(), block])).files[0].crc32
    };
    assert_eq!((r5(0x08), r5(0x18), r5(0x10)), (Some(0xcafe_f00d), None, None));
}

#[test]
fn a_split_rar5_file_keeps_no_crc() {
    // header flags 0x10: the data goes on in the next volume
    let fields = r5_fields_with(b"a.mkv", 0xcafe_f00d, 0, 0);
    let inner = [&vint(2)[..], &vint(0x10 | 2), &vint(1), &fields].concat();
    let body = [vint(inner.len() as u64), inner].concat();
    let block = [&crc32(&body).to_le_bytes()[..], &body, b"x"].concat();
    let l = rar::list(&rar5(&[r5_main(), block, r5_end()]));
    assert_eq!(l.files[0].crc32, None);
    assert_eq!(l.files[0].host_os, Some(facts::HOST_WINDOWS));
}

#[test]
fn rar5_hp_keeps_its_salt_and_check_as_the_twin() {
    let l = rar::list(&rar5(&[r5_crypt_header()]));
    assert!(l.headers_encrypted);
    let twin = l.twin.expect("a twin");
    assert_eq!(twin.tag, "rar5hp");
    assert_eq!(twin.bytes[..16], [0x53; 16]);
    // the test header's check doesn't sum right, soo only the salt is there
    assert!(twin.bytes.len() == 16 || twin.bytes.len() == 24);
    // a -p file's salt and IV when the headers aren't hidden
    let file = r5_file(2, 4, b"a.mkv", &r5_crypt_record(), b"x");
    let l = rar::list(&rar5(&[r5_main(), file, r5_end()]));
    assert_eq!(l.twin, Some(facts::Twin::new("rar5", &[[0x53; 16], [0x49; 16]].concat())));
}

/// A stored ZIP entry with a DOS time and the given extras, local header first, its directory entry second.
fn zip_with(name: &[u8], data: &[u8], time: u16, date: u16, extra: &[u8], made_by: u16) -> (Vec<u8>, Vec<u8>) {
    let crc = crc32(data);
    let local = [
        &b"PK\x03\x04\x14\x00\x00\x00\x00\x00"[..],
        &time.to_le_bytes(),
        &date.to_le_bytes(),
        &crc.to_le_bytes(),
        &(data.len() as u32).to_le_bytes(),
        &(data.len() as u32).to_le_bytes(),
        &(name.len() as u16).to_le_bytes(),
        &(extra.len() as u16).to_le_bytes(),
        name,
        extra,
        data,
    ]
    .concat();
    let central = [
        &b"PK\x01\x02"[..],
        &made_by.to_le_bytes(),
        &b"\x14\x00\x00\x00\x00\x00"[..],
        &time.to_le_bytes(),
        &date.to_le_bytes(),
        &crc.to_le_bytes(),
        &(data.len() as u32).to_le_bytes(),
        &(data.len() as u32).to_le_bytes(),
        &(name.len() as u16).to_le_bytes(),
        &(extra.len() as u16).to_le_bytes(),
        &[0; 10],
        &0u32.to_le_bytes(),
        name,
        extra,
    ]
    .concat();
    (local, central)
}

#[test]
fn zip_gives_the_packers_zone_owner_and_host() {
    // 14:30:20 local, 13:30:20 UTC: the packer was an hour east
    let (time, date) = ((14 << 11 | 30 << 5 | 10) as u16, ((2024 - 1980) << 9 | 3 << 5 | 10) as u16);
    let utc = 1_710_081_020 - 3600;
    let ut = [&0x5455u16.to_le_bytes()[..], &5u16.to_le_bytes(), &[1], &(utc as i32).to_le_bytes()].concat();
    let ux = [
        &0x7875u16.to_le_bytes()[..],
        &11u16.to_le_bytes(),
        &[1, 4],
        &1000u32.to_le_bytes(),
        &[4],
        &100u32.to_le_bytes(),
    ]
    .concat();
    let (local, central) = zip_with(b"a.txt", b"hello", time, date, &[ut, ux].concat(), 3 << 8 | 30);
    let eocd = zip_eocd(1, central.len() as u32, local.len() as u32, b"");
    let whole = [local.clone(), central, eocd].concat();
    let l = zip::list(&local, &whole);
    let f = &l.files[0];
    assert_eq!((f.crc32, f.mtime, f.host_os), (Some(crc32(b"hello")), Some(utc), Some(facts::HOST_UNIX)));
    assert_eq!(l.zone, Some(3600));
    assert!(l.owner.expect("an owner").0.contains("uid=1000:gid=100"));
    assert_eq!(l.packer.as_deref(), Some("ZIP · made by Unix 3.0 · needs 2.0 · store · extras 5455,7875"));
    // the head alone: no host, the same zone
    let head = zip::list_head(&local);
    assert_eq!((head.files[0].host_os, head.zone), (None, Some(3600)));
}

#[test]
fn a_zip_without_a_utc_time_keeps_only_its_local_time_for_the_guess() {
    let (time, date) = ((14 << 11 | 30 << 5 | 10) as u16, ((2024 - 1980) << 9 | 3 << 5 | 10) as u16);
    let (local, _) = zip_with(b"a.txt", b"hello", time, date, &[], 0);
    let l = zip::list_head(&local);
    assert_eq!((l.files[0].mtime, l.zone, l.local_wall), (None, None, Some(1_710_081_020)));
}

#[test]
fn seven_zip_keeps_crcs_times_coders_and_its_twin() {
    let payload = b"hello";
    // one folder, one stream, its CRC in the unpack info
    let streams = [
        &[0x04, 0x06, 0x00, 0x01, 0x09, 0x05, 0x00, 0x07, 0x0b, 0x01, 0x00][..],
        SZ_COPY,
        &[0x0c, 0x05, 0x0a, 0x01],
        &crc32(payload).to_le_bytes(),
        &[0x00, 0x00],
    ]
    .concat();
    let name: Vec<u8> = "a.txt".encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    // 2023-11-14 22:13:20 UTC as a FILETIME
    let ft = (1_700_000_000u64 + 11_644_473_600) * 10_000_000;
    let files = [
        &[0x05, 0x01, 0x11][..],
        &sz_num(name.len() as u64 + 1),
        &[0x00],
        &name,
        &[0x14, 0x0a, 0x01, 0x00],
        &ft.to_le_bytes(),
        &[0x15, 0x06, 0x01, 0x00],
        &(0x8000u32 | 0o644 << 16).to_le_bytes(),
        &[0x00],
    ]
    .concat();
    let header = [&[0x01][..], &streams, &files, &[0x00]].concat();
    let archive = sz_wrap(payload, &header);
    let l = sevenz::list(&archive, &[]);
    assert!(l.complete, "{l:?}");
    let f = &l.files[0];
    assert_eq!((f.crc32, f.mtime, f.host_os), (Some(crc32(payload)), Some(1_700_000_000), Some(facts::HOST_UNIX)));
    assert_eq!(l.packer.as_deref(), Some("7z · v0.4 · coders Copy"));
    assert_eq!(l.twin.as_ref().map(|t| t.tag), Some("7z"), "the start header, from article 1 alone");
    assert_eq!(l.twin.as_ref().map(|t| &t.bytes[..]), archive.get(12..32));
}

#[test]
fn seven_zip_aes_gives_its_iv_as_the_twin() {
    let mut props = vec![0x40 | 19, 0x0f];
    props.extend(1..=16u8);
    // AES coder with properties: flags 0x24 (4-byte id, props)
    let coder = [&[0x01, 0x24, 0x06, 0xf1, 0x07, 0x01][..], &[props.len() as u8], &props].concat();
    let header = [&[0x01][..], &sz_main_streams(5, &[5], &coder), &sz_files(&["a.txt"], None), &[0x00]].concat();
    let l = sevenz::list(&sz_wrap(b"xxxxx", &header), &[]);
    assert!(l.encrypted);
    assert_eq!(l.twin, Some(facts::Twin::new("7zaes", &(1..=16u8).collect::<Vec<_>>())));
    assert!(l.packer.as_deref().is_some_and(|p| p.ends_with("coders AES")), "{:?}", l.packer);
}

#[test]
fn archive_keys_salt_the_uploaders_facts_and_digest_the_twin() {
    use crate::archive::store::ArchiveKeys;
    let listing = Listing {
        packer: Some("RAR5 · v0".into()),
        twin: Some(facts::Twin::new("rar5hp", &[1; 24])),
        owner: facts::owner_text(Some("user"), None, Some(1000), None),
        zone: Some(3600),
        ..Default::default()
    };
    let a = ArchiveKeys::of(&listing, Some(7), None);
    let b = ArchiveKeys::of(&listing, Some(8), None);
    assert!(a.owner_key.is_some() && a.zone_key.is_some() && a.twin_key.is_some());
    assert_ne!(a.owner_key, b.owner_key, "another install, another salt, another key");
    assert_eq!(a.twin_key, b.twin_key, "the twin is the archive's, not the uploader's");
    let unsalted = ArchiveKeys::of(&listing, None, None);
    assert_eq!((unsalted.owner_key, unsalted.zone_key), (None, None), "no salt, no uploader key");
    // a local time only: guessed against the post's clock
    let local = Listing { local_wall: Some(1_000_000 + 3600 - 30), ..Default::default() };
    assert!(ArchiveKeys::of(&local, Some(7), Some(1_000_000)).zone_key.is_some());
    assert!(ArchiveKeys::of(&local, Some(7), None).zone_key.is_none());
}

#[test]
fn collapsed_volumes_keep_no_parts_crc() {
    let file = |name: &str, crc| InnerFile {
        name: name.into(),
        size: Some(10),
        crc32: Some(crc),
        mtime: Some(5),
        host_os: Some(1),
    };
    let rows = collapse_volumes(&[file("a.part01.rar", 1), file("a.part02.rar", 2), file("b.mkv", 3)]);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].crc32, rows[0].mtime, rows[0].host_os), (None, None, Some(1)));
    assert_eq!(rows[1].crc32, Some(3));
}

#[test]
fn garbage_after_a_marker_reads_without_a_panic() {
    for seed in 0u32..200 {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let noise: Vec<u8> = (0..300)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let _ = rar::list(&[R4_MARK, &noise].concat());
        let _ = rar::list(&[R5_MARK, &noise].concat());
        let _ = zip::list(&[b"PK\x03\x04".as_slice(), &noise].concat(), &noise);
        let _ = sevenz::list(&sz_wrap(&noise, &noise), &noise);
    }
}
