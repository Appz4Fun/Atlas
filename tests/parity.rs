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

#[test]
fn set_ids_main_body_hash_and_creator_set() {
    // PARITY_REQUESTS.md §1-3: the primary set id on the inventory, whether a Main's body hashes
    // to its set id, and the set a Creator packet belongs to.
    let (a, b, set) = two_file_set(2048);
    assert!(!a.is_empty() && !b.is_empty());
    let insp = parity::inspect(&set.index);
    let main = &insp.main[0];
    assert!(main.body_hashes_to_set_id, "a set made by build_set lists exactly the files its id was made from");
    let inv = parity::inventory(&set.index);
    assert_eq!(inv.set_id, Some(main.set_id));
    assert_eq!(inv.set_id.map(|s| s.to_vec()), Some(set.set_id.0.to_vec()));
    assert_eq!(insp.creators.len(), 1);
    assert_eq!(insp.creators[0].set_id, Some(main.set_id));
    assert_eq!(parity::creator(b"ParPar v0.4.6").set_id, None);
    assert_eq!(parity::inventory(b"").set_id, None);
    // Two sets in one buffer: creators and set ids stay apart.
    let other = build_set("other", &[("a.rar", &a)], &opts(1024, 1, VolumeScheme::Single)).unwrap();
    let mut joined = set.index.clone();
    joined.extend_from_slice(&other.index);
    let insp2 = parity::inspect(&joined);
    assert_eq!(insp2.creators.len(), 2);
    assert_ne!(insp2.creators[0].set_id, insp2.creators[1].set_id);
    assert_eq!(parity::inventory_of(&insp2).set_id, Some(main.set_id), "the first Main's set is primary");
}

#[test]
fn ifsc_crcs_on_the_article_grid_localise_damage() {
    // PARITY_REQUESTS.md §4: a file posted in 7000-byte articles, protected with 1000-byte slices.
    let file = data(10 * 7000 * 2 + 4321, 77);
    let (art, bs) = (7000u64, 1000u64);
    let grid = parity::lcm_grid(&[art, bs]).unwrap();
    assert_eq!(grid, 7000);
    let articles: Vec<(u32, u64)> = file.chunks(art as usize).map(|c| (parity::crc32(c), c.len() as u64)).collect();
    let mut padded = file.clone();
    padded.resize(file.len().div_ceil(bs as usize) * bs as usize, 0);
    let slices: Vec<u32> = padded.chunks(bs as usize).map(parity::crc32).collect();
    let from_ifsc = parity::ifsc_interval_crcs(&slices, bs, file.len() as u64, grid).unwrap();
    assert_eq!(from_ifsc, file.chunks(grid as usize).map(parity::crc32).collect::<Vec<_>>());
    assert_eq!(parity::ifsc_intervals(&articles, art, &slices, bs, file.len() as u64), Some(vec![]));
    let mut bad = articles.clone();
    bad[3].0 ^= 1;
    bad[20].0 ^= 0x80;
    assert_eq!(parity::ifsc_intervals(&bad, art, &slices, bs, file.len() as u64), Some(vec![3, 20]));
    // Sizes that do not add up, or a slice count that does not fit, say nothing.
    assert_eq!(parity::ifsc_intervals(&articles, art, &slices, bs, file.len() as u64 + 1), None);
    assert_eq!(parity::ifsc_intervals(&articles, art, &slices[1..], bs, file.len() as u64), None);
    assert_eq!(
        parity::ifsc_interval_crcs(&slices, bs, file.len() as u64, 1500),
        None,
        "grid must be a multiple of the block size"
    );
    // Indices are grid intervals, not articles: 3500-byte articles on a 7000-byte grid put two
    // articles in each interval, so damage in article 7 is interval 3.
    let half: Vec<(u32, u64)> = file.chunks(3500).map(|c| (parity::crc32(c), c.len() as u64)).collect();
    assert_eq!(parity::ifsc_intervals(&half, 3500, &slices, bs, file.len() as u64), Some(vec![]));
    let mut bad_half = half.clone();
    bad_half[7].0 ^= 1;
    assert_eq!(parity::ifsc_intervals(&bad_half, 3500, &slices, bs, file.len() as u64), Some(vec![3]));
    // An article straddling a grid boundary cannot be placed; an empty file agrees everywhere.
    let short: Vec<(u32, u64)> = file.chunks(3000).map(|c| (parity::crc32(c), c.len() as u64)).collect();
    assert_eq!(parity::ifsc_intervals(&short, art, &slices, bs, file.len() as u64), None);
    assert_eq!(parity::ifsc_intervals(&[], art, &[], bs, 0), Some(vec![]));
    assert_eq!(parity::ifsc_intervals(&[], 0, &[], bs, 0), None, "a zero article size has no grid");
    // The shared operator folds the same as the plain call.
    let shift = parity::Crc32Shift::new(bs);
    assert_eq!(parity::fold_ifsc_crcs_with(&shift, &slices, file.len() as u64), Some(parity::crc32(&file)));
}

#[test]
fn base_par2_names_end_right_after_par2() {
    // PARITY_REQUESTS.md §5, the table as written, through both doors.
    for (subject, want) in [
        (r#""a.par2" yEnc"#, true),
        ("a.par2.rar", false),
        ("a.par2-1", false),
        ("(a.par2)", true),
        ("[a.par2]", true),
        (r#""a.vol00+01.par2" and "b.par2""#, true),
        ("a.vol00+01.par2", false),
        (r#"[1/9] - "abc.par2" yEnc (1/1)"#, true),
        ("abc.par2", true),
        (r#""abc.vol00+01.par2" yEnc (1/1)"#, false),
        (r#""abc.rar" yEnc (1/1)"#, false),
        // A closing bracket is part of the name (the rule's first cut dropped these).
        (r#""Show (2026).par2" yEnc (1/1)"#, true),
        ("Movie.2026.1080p[rartv].par2", true),
        ("[HorribleSubs]_AIURA_-_01_[480p].par2", true),
        ("[Chyuu] Nanatsu no Taizai - 12 [720p][D1F49539].par2", true),
        ("Marillion - Live From Cadogan Hall 2011 (DVD 1).par2", true),
        ("Show (2026).vol000+01.par2", false),
        ("Show (2026).par2.rar", false),
        // Any `White_Space` character ends the name, as it did for the old `\\s` regex: a no-break
        // space from a header read as Latin-1, an ideographic space, an ASCII vertical tab.
        // Letters and zero-width characters do not.
        ("a.par2\u{a0}yEnc", true),
        ("a.par2\u{3000}yEnc", true),
        ("a.par2\u{b}yEnc", true),
        ("a.par2\u{e9}", false),
        ("a.par2\u{200b}x", false),
    ] {
        assert_eq!(parity::is_base_par2(subject), want, "{subject:?}");
        assert_eq!(atlas::par2::is_base_par2(subject), want, "{subject:?} via atlas::par2");
    }
}

#[test]
fn base_par2_agrees_with_the_old_regexes_on_the_naming_corpus() {
    // The rule atlas::par2 used before delegating to par-rs, as an oracle over every real
    // subject in the naming corpus. The rules differ where the request asked them to (`(a.par2)`
    // and `[a.par2]` are par2 files now) and in what may follow `.par2` (any closing delimiter
    // `' > ] )` ends the name, not only `"` and whitespace; a name that is both a volume and a
    // base counts). None of those shapes occur in the corpus, and `\u{a0}` after `.par2` is
    // accepted by both (pinned above), so the two agree on every line.
    let base = regex::Regex::new(r#"(?i)\.par2(?:["\s]|$)"#).unwrap();
    let vol = regex::Regex::new(r"(?i)\.vol\d+[+-]\d+\.par2").unwrap();
    let corpus = include_str!("fixtures/naming_samples.tsv");
    let mut seen = 0usize;
    for line in corpus.lines().filter(|l| !l.starts_with('#')) {
        let Some(subject) = line.split('\t').nth(1) else { continue };
        let old = base.is_match(subject) && !vol.is_match(subject);
        assert_eq!(parity::is_base_par2(subject), old, "{subject:?}");
        assert_eq!(atlas::par2::is_base_par2(subject), old, "{subject:?} via atlas::par2");
        seen += usize::from(old);
    }
    assert!(seen > 200, "{seen} base par2 subjects in the corpus");
}
