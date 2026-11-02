use super::testkit::*;
use super::*;
use crate::archive::names::*;
use crate::archive::select::*;
use crate::store::FileInfo;

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
    let file = |name: String, size: u64| InnerFile { name, size: Some(size) };
    let mut files: Vec<InnerFile> = (1..=100).map(|i| file(format!("x.part{i:02}.rar"), 10)).collect();
    files.push(file("x.nfo".into(), 1));
    let rows = collapse_volumes(&files);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].name.as_str(), rows[0].size), ("x.part01.rar", Some(1000)));
    let many: Vec<InnerFile> = (0..100).map(|i| file(format!("{i}.mkv"), 1)).collect();
    assert_eq!(collapse_volumes(&many).len(), MAX_STORED);
    assert_eq!(collapse_volumes(&[file("a.mkv".into(), 1), file("a.mkv".into(), 1)]).len(), 1);
}

#[test]
fn password_table() {
    let with = |names: &[&str]| Listing {
        files: names.iter().map(|n| InnerFile { name: (*n).into(), size: None }).collect(),
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
}
