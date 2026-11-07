# par-rs requests

APIs Atlas needs from [par-rs](https://github.com/xbmc4lyfe/par-rs) that the crate doesn't
have yet. `atlas::parity` (`src/parity.rs`) calls the crate and never re-implements PAR2 or
CRC math, so when something is missing it is written here, with the exact signature wanted,
instead of being worked around in Atlas.

Checked against par-rs `main` at `56dcf7a3`. Requests 1-6 below were met at par-rs `8afce2e`
(its reply: `docs/PARITY_REQUESTS_RESPONSE.md`); this file keeps them as the record of what was asked
and how it was answered. Nothing is open.

## Answered (par-rs `8afce2e`, 2026-11-07)

| # | asked | par-rs |
|---|---|---|
| 1 | `Inventory.set_id: Option<[u8; 16]>` | added as written: the primary set (first `Main`, else first `FileDesc`) |
| 2 | `MainInfo.body_hashes_to_set_id: bool` | added as written, for `Main` and `PkdMain` |
| 3 | `CreatorInfo.set_id: Option<[u8; 16]>` | added as written; `None` from `CreatorInfo::parse` |
| 4 | `gf2::ifsc_interval_crcs(slice_crcs, block_size, file_size, grid_size)` | added as written; `parity::ifsc_intervals` compares both sides and names the intervals that differ |
| 5 | `is_base_par2` ends the name right after `.par2` | the table in §5 is now par-rs's test; `atlas::par2::is_base_par2` delegates to it. The name starts after the previous whitespace, quote or *opening* bracket, so `Show (2026).par2`, `[Group] Title [720p].par2` and an obfuscated `".par2"` stay par2 files (par-rs `b44cc55`, `56dcf7a`; `tests/parity.rs` checks the whole naming corpus against the old regexes) |
| 6 | speed | `is_base_par2` 5 → 52 M subjects/s (one allocation-free pass); `inventory` 658 → 2,365 /s on the 200-file index (optimistic single pass, metadata MD5s in one 16-lane batch, resynchronising scanner as the fallback, result identical by test); recovery volume 282 → 1,029 MiB/s; article folding 71 k → 548 k /s (`gf2::Crc32Shift`, a per-block-size operator table). Metadata MD5s are still checked, which is the whole of what remains against Atlas's unchecked scanner on a 2-file index. |

Below, the requests as they were made.

## 1. The primary set id on `Inventory`

The integration brief lists `Inventory { set_id, .. }`; the crate's `Inventory` has no set id,
only one per file. An empty set (a Main with no FileDesc read yet) has no file to take it from.

```rust
// par_rs::par2
pub struct Inventory {
    /// Set id of the primary set (the set of the first `Main` packet, else of the first
    /// `FileDesc`), the same set `block_size` describes.
    pub set_id: Option<[u8; 16]>,
    // ..existing fields
}
```

## 2. Whether a Main packet's body hashes to its set id

PAR2 makes the set id the MD5 of the Main packet's body, so a Main whose body hashes to its
set id lists every file of the set: nobody added a file to it afterwards. `atlas::par2`
records that as `Inventory::sealed` and keys file rows on it (`raw_store`, `store`,
`archive::link`). par-rs's `Inventory::sealed` means something else (a FileDesc plus an IFSC
consistent with the size), and the inspection doesn't expose the check.

```rust
// par_rs::inspect
pub struct MainInfo {
    /// `MD5(body) == set_id`: the recovery set list is the one the set id was made from.
    pub body_hashes_to_set_id: bool,
    // ..existing fields
}
```

## 3. The set a Creator packet belongs to

`Par2Inspection::creators` is a `Vec<CreatorInfo>` without the set id, and a buffer can carry
several sets (Atlas keeps `Inventory::creators: Vec<(set_id, text)>`, one per set). Matching
them back up through `packets` by position would be a workaround.

```rust
// par_rs::inspect
pub struct CreatorInfo {
    /// Set id from the packet header; `None` when built by `CreatorInfo::parse` alone.
    pub set_id: Option<[u8; 16]>,
    // ..existing fields
}
```

## 4. IFSC slice CRCs on an article grid

`gf2::compute_interval_crcs` puts a file's article CRCs on a grid, and `gf2::fold_ifsc_crcs`
folds the IFSC slice CRCs into one whole-file CRC. Nothing puts the IFSC CRCs on the same
grid, so Atlas can say *whether* a file's articles agree with its PAR2 set
(`parity::articles_agree_with_ifsc`, whole-file CRCs compared) but not *which* stretch of the
file disagrees. With this, `lcm_grid(&[article_size, block_size])` intervals from both sides
compare one to one.

```rust
// par_rs::gf2
/// Fold IFSC slice CRCs (each over `block_size` bytes, the last zero padded) onto
/// `grid_size` intervals of the unpadded file: entry `k` is the CRC-32 of file bytes
/// `[k * grid_size, (k + 1) * grid_size)`, the last entry covering the tail. `None` when
/// `grid_size` is not a multiple of `block_size`, `block_size == 0`, or the slice count
/// doesn't fit `file_size`.
pub fn ifsc_interval_crcs(slice_crcs: &[u32], block_size: u64, file_size: u64, grid_size: u64) -> Option<Vec<u32>>;
```

## 5. `is_base_par2` stops at the end of the name

`par_rs::is_base_par2` ends the name at a word boundary after `.par2`, so it takes names that
only contain `.par2` for a par2 index. `atlas::par2::is_base_par2` (the one in use) requires a
quote, whitespace or the end of the subject right after `.par2`. Same signature, wanted
behaviour:

| subject | atlas::par2 | par_rs now | wanted |
|---|---|---|---|
| `"a.par2" yEnc` | true | true | true |
| `a.par2.rar` | false | true | **false** |
| `a.par2-1` | false | true | **false** |
| `(a.par2)` | false | true | true |
| `[a.par2]` | false | true | true |
| `"a.vol00+01.par2" and "b.par2"` | false | true | true |
| `a.vol00+01.par2` | false | false | false |

```rust
// par_rs::par2 (unchanged signature)
pub fn is_base_par2(subject: &str) -> bool;
```

## 6. Speed: `is_base_par2` and `inventory`

`examples/bench_parity.rs` runs Atlas's own code and par-rs on the same inputs (Xeon @ 2.3 GHz,
x86_64 with pclmulqdq and AVX-512, release build, best of 5). par-rs's CRC-32 is far faster;
the folding algebra is the same speed; its PAR2 reading and subject test are slower:

| | Atlas | par-rs | par-rs vs Atlas |
|---|---|---|---|
| `crc32`, 64 MiB buffer | 484 MiB/s | 24,720 MiB/s | **51x faster** |
| `crc32`, 700 KiB articles | 709 /s | 125,181 /s | **177x faster** |
| fold 94 article CRCs into a file CRC | 56,146 /s | 56,816 /s | same |
| `crc32_combine` / `crc32_unzero` | 2.3 / 0.9 M/s | 2.5 / 1.0 M/s | same |
| `inventory`, 200-file index (431 KiB) | 821 /s | 509 /s | 0.6x |
| same, par-rs with `VerifyMode::None` | 821 /s | 770 /s | 0.9x |
| `inventory`, 2-file index | 179,453 /s | 123,010 /s | 0.7x |
| `inventory`, 1.3 MiB recovery volume | 917 MiB/s | 336 MiB/s | 0.4x |
| same, par-rs with `VerifyMode::None` | 917 MiB/s | 787 MiB/s | 0.9x |
| `is_base_par2`, 100k subjects | 18.8 M/s | 3.3 M/s | **0.2x** |

Most of the `inventory` gap is the packet MD5s par-rs checks and Atlas doesn't (`VerifyMode::Metadata`).
That check is worth its cost, because a corrupt FileDesc is never believed. The 10% left over is the
full inspection built before the summary. `is_base_par2` matters more: the indexer runs it on every
subject it reads, and par-rs's version lowercases a copy of every `.par2` match before looking
at it. Wanted, same signatures:

```rust
// par_rs::par2: no allocation per subject; at least as fast as a single anchored regex match.
pub fn is_base_par2(subject: &str) -> bool;
// par_rs::par2: inventory straight from the packets, without building a full `Par2Inspection`
// first (same result as `inventory`, metadata MD5s still checked).
pub fn inventory(data: &[u8]) -> Inventory;
```

## Atlas code that still does PAR2 / CRC math itself

Left as it is in the change that added `atlas::parity`, one change at a time. Each is a
follow-up that moves its callers onto `atlas::parity`. `src/gf2.rs` was the first: it is now
only re-exports of `par_rs::gf2`, so every CRC-32 postprocess takes of a BODY runs par-rs's
hardware path (the bench rows above are from before that change). Still to go:

- `src/par2.rs`: its own PAR2 packet scanner and `Inventory` / `Par2File`. The crate's types
  have the same fields except Atlas's `creators` and the meaning of `sealed`; par-rs now carries
  what both need (`CreatorInfo::set_id`, `MainInfo::body_hashes_to_set_id`), and
  `is_base_par2` already delegates to par-rs. Its inventory also stops reading IFSC packets
  after 16,384 slices in total (`MAX_TOTAL_IFSC_SLICES`), so files of a big set lose their
  whole-file CRC: in the benchmark's 200-file, 19,600-slice set Atlas folds a CRC for 167
  files and par-rs for all 200. A 50 GB remux with 1 MiB slices has about 50,000 slices. `creator_text` (drops e-mail addresses from
  Creator strings) is Atlas's privacy rule, not PAR2 math, and stays in Atlas.
- `src/merging.rs`: `Par2Bridge` and `parse_rar5_volume_num`, a second obfuscated-name
  resolver and RAR5 header reader (only tests call them today); `parity::resolve` /
  `parity::rar5_volume` cover both.
