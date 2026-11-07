//! `atlas::parity` against sets made by `par_rs::create::build_set`, so CI never needs a
//! .par2 file from outside.

use atlas::parity::{self, Candidate, CreatorTool, ParFormat, ResolveMethod, SliceStatus};
use par_rs::create::{Par2Set, SetOptions, VolumeScheme, build_set};
use par_rs::{Isa, Kernel};

/// Bytes that are never a shifted copy of another seed's.
fn data(n: usize, seed: u32) -> Vec<u8> {
    (0..n as u32).map(|i| ((i ^ seed.wrapping_mul(0x9E37_79B9)).wrapping_mul(2_654_435_761) >> 13) as u8).collect()
}

fn opts(slice_size: usize, recovery_blocks: u32, volumes: VolumeScheme) -> SetOptions {
    SetOptions {
        slice_size,
        recovery_blocks,
        volumes,
        creator: "par-rs test 0.2.0 (x86_64)".into(),
        // the scalar kernel gives the same bytes on every CI machine
        kernel: Kernel::with_isa(Isa::Scalar).unwrap_or_else(Kernel::detect),
        ..SetOptions::default()
    }
}

/// A RAR5 file: signature, a main archive header (with a volume number after the first
/// volume) and a body.
fn rar5(volume: Option<u64>, body: &[u8]) -> Vec<u8> {
    let mut h = parity::RAR5_SIGNATURE.to_vec();
    h.extend_from_slice(&[0, 0, 0, 0]);
    let mut hdr = vec![1u8, 0];
    match volume {
        Some(v) => {
            hdr.push(0x03);
            hdr.push(v as u8);
        }
        None => hdr.push(0x01),
    }
    h.push(hdr.len() as u8);
    h.extend_from_slice(&hdr);
    h.extend_from_slice(body);
    h
}

fn two_file_set(slice_size: usize) -> (Vec<u8>, Vec<u8>, Par2Set) {
    let a = data(20_000, 11);
    let b = data(9_000, 12);
    let set =
        build_set("Some.Movie.2026", &[("a.mkv", &a), ("b.nfo", &b)], &opts(slice_size, 6, VolumeScheme::Exponential))
            .unwrap();
    (a, b, set)
}

#[test]
fn sniffs_formats_and_names() {
    let (_, _, set) = two_file_set(2048);
    assert_eq!(parity::format_of(&set.index), Some(ParFormat::Par2));
    for (_, vol) in &set.volumes {
        assert_eq!(parity::format_of(vol), Some(ParFormat::Par2));
    }
    assert_eq!(parity::format_of(b"Rar!\x1a\x07\x01\x00"), None);
    assert_eq!(parity::format_of(b""), None);

    let index = parity::par2_name("Some.Movie.2026.par2").unwrap();
    assert!(index.is_index);
    assert_eq!(index.base, "Some.Movie.2026");
    // Exponential volumes: 1, 2, then the remaining 3 of 6.
    let names: Vec<_> = set.volumes.iter().map(|(n, _)| parity::par2_name(n).unwrap()).collect();
    assert!(names.iter().all(|n| !n.is_index && n.base == "Some.Movie.2026"));
    let blocks: Vec<_> = names.iter().map(|n| (n.first_block.unwrap(), n.block_count.unwrap())).collect();
    assert_eq!(blocks.iter().map(|(_, c)| c).sum::<u32>(), 6);
    assert!(blocks.windows(2).all(|w| w[0].0 + w[0].1 == w[1].0), "volumes are contiguous: {blocks:?}");
    assert_eq!(parity::par2_name("Some.Movie.2026.vol003-005.par2").map(|n| n.block_count), Some(Some(3)));
    assert!(parity::par2_name("Some.Movie.2026.rar").is_none());

    assert!(parity::is_base_par2(r#"[1/9] - "Some.Movie.2026.par2" yEnc (1/1) 1024"#));
    assert!(!parity::is_base_par2(r#"[2/9] - "Some.Movie.2026.vol000+01.par2" yEnc (1/1)"#));
    assert!(!parity::is_base_par2(r#"[3/9] - "Some.Movie.2026.part1.rar" yEnc (1/50)"#));
}

#[test]
fn inspects_packets_creator_and_resync() {
    let (_, _, set) = two_file_set(2048);
    let insp = parity::inspect(&set.index);
    assert_eq!(insp.format, Some(ParFormat::Par2));
    assert_eq!(insp.main.len(), 1);
    assert_eq!(insp.main[0].slice_size, 2048);
    assert_eq!(insp.main[0].recovery_set.len(), 2);
    assert_eq!(insp.files.len(), 2);
    assert_eq!(insp.ifsc.len(), 2);
    assert_eq!(insp.bad_md5, 0);
    assert_eq!(insp.resyncs, 0);
    assert!(!insp.truncated);
    assert!(insp.recovery.is_empty(), "the index carries no recovery data");
    assert_eq!(insp.creators.len(), 1);
    let creator = &insp.creators[0];
    assert_eq!(creator.tool, CreatorTool::ParRs);
    assert_eq!(creator.version.as_deref(), Some("0.2.0"));
    assert_eq!(creator.arch.as_deref(), Some("x86_64"));
    assert!(insp.packets.iter().all(|p| p.kind != parity::PacketKind::Unknown));

    // Creator strings of other tools, as their packet bodies carry them.
    let parpar = parity::creator(b"ParPar v0.4.1 x64 [https://github.com/animetosho/parpar]\0\0\0");
    assert_eq!(
        (parpar.tool, parpar.version.as_deref(), parpar.arch.as_deref()),
        (CreatorTool::ParPar, Some("0.4.1"), Some("x64"))
    );
    assert_eq!(parity::creator(b"Created by par2cmdline version 0.8.1.").tool, CreatorTool::Par2Cmdline);
    assert_eq!(parity::creator(b"\xff\xfe garbage").tool, CreatorTool::Other);

    // Garbage before, between and after the packets: every packet is still found.
    let cut = insp.packets[insp.packets.len() / 2].offset;
    let mut dirty = b"junk before the first magic".to_vec();
    dirty.extend_from_slice(&set.index[..cut]);
    dirty.extend_from_slice(b"PAR2\0PKT but not a header");
    dirty.extend_from_slice(&set.index[cut..]);
    dirty.extend_from_slice(b"trailing junk");
    let messy = parity::inspect(&dirty);
    assert!(messy.resyncs > 0);
    assert_eq!(messy.files.len(), 2);
    assert_eq!(messy.ifsc.len(), 2);
    assert_eq!(parity::inventory_of(&messy).files, parity::inventory(&set.index).files);

    // A FileDesc with one byte flipped fails its MD5 and is not believed.
    let fd = insp.packets.iter().find(|p| p.kind == parity::PacketKind::FileDesc).unwrap();
    let mut flipped = set.index.clone();
    flipped[fd.offset + 64 + 40] ^= 0x01;
    let bad = parity::inspect(&flipped);
    assert_eq!(bad.bad_md5, 1);
    assert_eq!(bad.files.len(), 1);
    let inv = parity::inventory(&flipped);
    assert_eq!(inv.files.len(), 1);
    assert!(!inv.complete, "a Main lists a file no FileDesc describes");
}

#[test]
fn inventory_lists_files_in_libpar2_order_with_hashes() {
    let files: Vec<(String, Vec<u8>)> =
        (0..6).map(|i| (format!("Some.Movie.2026.part{}.rar", i + 1), data(5_000 + i * 777, 20 + i as u32))).collect();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
    let set = build_set("Some.Movie.2026", &refs, &opts(1024, 3, VolumeScheme::Single)).unwrap();
    let inv = parity::inventory(&set.index);
    assert_eq!(inv.block_size, Some(1024));
    assert!(inv.complete);
    assert!(!inv.truncated);
    assert_eq!(inv.files.len(), 6);
    assert!(
        inv.files.windows(2).all(|w| u128::from_le_bytes(w[0].file_id) < u128::from_le_bytes(w[1].file_id)),
        "libpar2 order: file id as a little-endian u128"
    );
    assert_eq!(inv.sealed.len(), 6);
    let set_id = inv.files[0].set_id;
    assert!(inv.files.iter().all(|f| f.set_id == set_id && f.bound));
    for (name, bytes) in &files {
        let f = inv.files.iter().find(|f| &f.name == name).unwrap();
        assert_eq!(f.size, bytes.len() as u64);
        assert_eq!(f.md5, <[u8; 16]>::from(<md5::Md5 as md5::Digest>::digest(bytes)));
        assert_eq!(f.md5_16k, par_rs::hasher::md5_16k(bytes));
        assert_eq!(f.crc32, Some(parity::crc32(bytes)), "{name}: whole-file CRC folded from IFSC");
        assert!(inv.sealed.contains(&(set_id, f.file_id)));
    }
    // A volume repeats the metadata: the same files, the same order.
    assert_eq!(parity::inventory(&set.volumes[0].1).files, inv.files);
    // Not a par2 at all.
    let none = parity::inventory(b"not a par2 file");
    assert!(none.files.is_empty());
    assert!(!none.complete);
}

#[test]
fn resolves_obfuscated_rar_volumes() {
    let v1 = rar5(None, &data(50_000 - 20, 1));
    let v2 = rar5(Some(1), &data(50_000 - 21, 2));
    let v3 = rar5(Some(2), &data(50_000 - 21, 3));
    let v4 = rar5(Some(3), &data(50_000 - 21, 4));
    let v5 = rar5(Some(4), &data(10_000, 5));
    let nfo = data(777, 6);
    let files: Vec<(&str, &[u8])> = vec![
        ("Rel.part1.rar", &v1),
        ("Rel.part2.rar", &v2),
        ("Rel.part3.rar", &v3),
        ("Rel.part4.rar", &v4),
        ("Rel.part5.rar", &v5),
        ("Rel.nfo", &nfo),
    ];
    let set = build_set("Rel", &files, &opts(4096, 6, VolumeScheme::Single)).unwrap();
    let inv = parity::inventory(&set.index);
    assert_eq!(parity::rar5_volume(&v1[..64]), Some(0));
    assert_eq!(parity::rar5_volume(&v4[..64]), Some(3));
    assert_eq!(parity::rar5_volume(&nfo), None);
    assert_eq!(parity::rar5_volume(&v4[..10]), None, "a cut header gives nothing");

    // What an indexer knows of each obfuscated file: the NZB size always, article 1's 16 KiB
    // MD5 or the folded yEnc CRC for some, the first bytes for others.
    let articles = |d: &[u8]| d.chunks(7_000).map(|c| (parity::crc32(c), c.len())).collect::<Vec<_>>();
    let candidates = vec![
        Candidate { name: "a1b2".into(), size: v5.len() as u64, ..Default::default() },
        Candidate {
            name: "c3d4".into(),
            size: v2.len() as u64,
            md5_16k: Some(par_rs::hasher::md5_16k(&v2)),
            ..Default::default()
        },
        Candidate {
            name: "e5f6".into(),
            size: v3.len() as u64,
            crc32: Some(parity::file_crc(&articles(&v3)).unwrap()),
            ..Default::default()
        },
        Candidate { name: "g7h8".into(), size: v4.len() as u64, head: Some(v4[..64].to_vec()), ..Default::default() },
        Candidate { name: "i9j0".into(), size: v1.len() as u64, head: Some(v1[..64].to_vec()), ..Default::default() },
        Candidate { name: "k1l2".into(), size: nfo.len() as u64, ..Default::default() },
        Candidate { name: "stray".into(), size: 123, ..Default::default() },
    ];
    let report = parity::resolve(&inv, &candidates);
    let named = parity::real_names(&inv, &report);
    assert_eq!(
        named,
        vec![
            (0, "Rel.part5.rar", ResolveMethod::SizeChain),
            (1, "Rel.part2.rar", ResolveMethod::Md5_16k),
            (2, "Rel.part3.rar", ResolveMethod::Crc32),
            (3, "Rel.part4.rar", ResolveMethod::Rar5Volume),
            (4, "Rel.part1.rar", ResolveMethod::Rar5Volume),
            (5, "Rel.nfo", ResolveMethod::SizeChain),
        ]
    );
    assert_eq!(report.unresolved_candidates, vec![6]);
    assert!(report.unmatched_files.is_empty());

    // Same-size volumes with nothing else known stay unresolved rather than guessed.
    let blind: Vec<Candidate> =
        [&v2, &v3].iter().map(|v| Candidate { name: "x".into(), size: v.len() as u64, ..Default::default() }).collect();
    let blind_report = parity::resolve(&inv, &blind);
    assert!(blind_report.resolved.is_empty());
    // A report from another inventory names nothing out of range.
    assert!(parity::real_names(&parity::inventory(b""), &report).is_empty());
}

#[test]
fn crc_algebra_from_article_metadata() {
    let file = data(3_000_000, 3);
    let whole = parity::crc32(&file);
    // yEnc articles of 716_800 decoded bytes, as an NZB/yEnc trailer reports them.
    let parts: Vec<(u32, usize)> = file.chunks(716_800).map(|c| (parity::crc32(c), c.len())).collect();
    assert_eq!(parity::file_crc(&parts), Ok(whole));
    let mut indexed: Vec<(usize, u32, usize)> = parts.iter().enumerate().map(|(i, &(c, l))| (i, c, l)).collect();
    indexed.reverse();
    assert_eq!(parity::file_crc_indexed(parts.len(), &indexed), Ok(whole));
    assert_eq!(
        parity::file_crc_indexed(parts.len(), &indexed[1..]),
        Err(parity::FoldError::MissingPart(parts.len() - 1))
    );
    assert_eq!(parity::file_crc(&[]), Err(parity::FoldError::Empty));

    // combine / zeros / unzero / uncombine round trips
    let (a, b) = file.split_at(1_234_567);
    let (ca, cb) = (parity::crc32(a), parity::crc32(b));
    assert_eq!(parity::crc32_combine(ca, cb, b.len()), whole);
    assert_eq!(parity::crc32_uncombine_suffix(whole, cb, b.len() as u64), ca);
    assert_eq!(parity::crc32_uncombine_prefix(whole, ca, b.len() as u64), cb);
    assert_eq!(parity::crc32_zeros(4096), parity::crc32(&[0u8; 4096]));
    let mut padded = a.to_vec();
    padded.resize(a.len() + 999, 0);
    assert_eq!(parity::crc32_unzero(parity::crc32(&padded), 999), ca);

    // LCM grid of two posters' article sizes and the interval CRCs on it.
    let grid = parity::lcm_grid(&[716_800, 768_000]).unwrap();
    assert_eq!(grid, 10_752_000);
    let big = data(2 * grid as usize + 12_345, 4);
    for art in [716_800usize, 768_000] {
        let p: Vec<(u32, u64)> = big.chunks(art).map(|c| (parity::crc32(c), c.len() as u64)).collect();
        let iv = parity::compute_interval_crcs(&p, grid).unwrap();
        assert_eq!(iv.len(), 3);
        for (k, c) in iv.iter().enumerate() {
            let end = ((k + 1) * grid as usize).min(big.len());
            assert_eq!(*c, parity::crc32(&big[k * grid as usize..end]));
        }
    }
    // An article straddling the grid can't be split.
    assert_eq!(parity::compute_interval_crcs(&[(0, 10), (0, 10)], 15), None);
}

#[test]
fn articles_checked_against_ifsc_without_bodies() {
    let file = data(70_001, 9);
    let set = build_set("Rel", &[("Rel.mkv", &file)], &opts(4096, 2, VolumeScheme::Single)).unwrap();
    let insp = parity::inspect(&set.index);
    let slices: Vec<u32> = insp.ifsc[0].slices.iter().map(|&(_, crc)| crc).collect();
    let size = file.len() as u64;
    assert_eq!(parity::file_crc_from_ifsc(&slices, 4096, size), Some(parity::crc32(&file)));
    assert_eq!(parity::file_crc_from_ifsc(&slices, 4096, size + 4096), None, "slice count must fit the size");

    let articles: Vec<(u32, usize)> = file.chunks(6_000).map(|c| (parity::crc32(c), c.len())).collect();
    assert_eq!(parity::articles_agree_with_ifsc(&articles, &slices, 4096, size), Some(true));
    // One article posted with other bytes.
    let mut wrong = articles.clone();
    wrong[3].0 ^= 0x8000_0000;
    assert_eq!(parity::articles_agree_with_ifsc(&wrong, &slices, 4096, size), Some(false));
    // An article missing: the lengths don't add up, so nothing can be said.
    assert_eq!(parity::articles_agree_with_ifsc(&articles[1..], &slices, 4096, size), None);
    assert_eq!(parity::articles_agree_with_ifsc(&[], &slices, 4096, size), None);
}

#[test]
fn verifies_slices_of_bytes_at_hand() {
    let (a, b, set) = two_file_set(2048);
    let v = parity::verifier(&set.index).unwrap();
    assert_eq!(v.block_size(), 2048);
    assert_eq!(v.total_slices(), 10 + 5);
    let insp = parity::inspect(&set.index);
    let id_of = |name: &str| insp.files.iter().find(|f| f.name == name).unwrap().file_id;
    let fa = v.file_index(&id_of("a.mkv")).unwrap();
    let fb = v.file_index(&id_of("b.nfo")).unwrap();
    assert!(v.verify_file(fa, &a).is_intact());
    assert!(v.verify_file(fb, &b).is_intact());

    // Two flipped bytes in a, 100 bytes cut out of b's third slice.
    let mut da = a.clone();
    da[100] ^= 1;
    da[5_000] ^= 1;
    let mut db = b.clone();
    db.drain(4_096..4_196);
    let report = v.verify_files(&[(fa, &da), (fb, &db)]);
    assert_eq!(report.files[0].slices.iter().filter(|s| **s == SliceStatus::Damaged).count(), 2);
    assert!(!report.files[1].size_ok);
    assert_eq!(report.files[1].slices[3], SliceStatus::Misplaced { offset: 6_144 - 100 });
    assert_eq!(report.missing.len(), 3);

    // Recovery slices from the volumes: 6 made, all whole.
    let set_id = insp.main[0].set_id;
    let volumes: Vec<_> = set.volumes.iter().map(|(_, bytes)| parity::inspect(bytes)).collect();
    assert_eq!(parity::usable_recovery(&volumes, &set_id), 6);
    // A volume seen twice adds nothing; another set's id counts nothing.
    let mut twice = volumes.clone();
    twice.push(volumes[0].clone());
    assert_eq!(parity::usable_recovery(&twice, &set_id), 6);
    assert_eq!(parity::usable_recovery(&volumes, &[0u8; 16]), 0);
    // The last volume cut inside its last recovery slice: that slice isn't whole.
    let last = &set.volumes.last().unwrap().1;
    let last_slice = volumes.last().unwrap().recovery.last().unwrap().data_offset;
    let mut partial = volumes[..volumes.len() - 1].to_vec();
    partial.push(parity::inspect(&last[..last_slice + 10]));
    assert_eq!(parity::usable_recovery(&partial, &set_id), 6 - 1);

    let enough = v.assess(&report, parity::usable_recovery(&volumes, &set_id));
    assert!(enough.repairable);
    assert_eq!(enough.shortfall, 0);
    let short = v.assess(&report, 2);
    assert!(!short.repairable);
    assert_eq!(short.shortfall, 1);

    // b handed in as a: b's slices are found anyway.
    let swapped = v.verify_files(&[(fa, &b)]);
    assert!(swapped.located.iter().filter(|l| l.is_some()).count() >= 5);
    // No Main packet, no verifier.
    assert!(parity::verifier(b"not a par2").is_none());
}

#[test]
fn every_prefix_is_data_not_a_panic() {
    let (_, _, set) = two_file_set(1024);
    let full = parity::inventory(&set.index);
    let mut seen = 0usize;
    for cut in 0..=set.index.len() {
        let prefix = &set.index[..cut];
        let _ = parity::format_of(prefix);
        let insp = parity::inspect(prefix);
        let inv = parity::inventory_of(&insp);
        assert!(inv.files.len() >= seen, "prefix {cut}: an inventory never loses a file a shorter prefix had");
        seen = inv.files.len();
        // a file may show before the Main that binds it or the IFSC that gives its CRC, but
        // what it says of itself never changes
        for f in &inv.files {
            assert!(
                full.files.iter().any(|g| (g.file_id, &g.name, g.size, g.md5) == (f.file_id, &f.name, f.size, f.md5)),
                "prefix {cut}: {} isn't in the full set",
                f.name
            );
        }
        let _ = parity::verifier(prefix);
    }
    assert_eq!(seen, full.files.len());
    for (_, vol) in &set.volumes {
        for cut in (0..=vol.len()).step_by(97) {
            let insp = parity::inspect(&vol[..cut]);
            let _ = parity::usable_recovery(&[insp], &[0u8; 16]);
        }
    }
}
