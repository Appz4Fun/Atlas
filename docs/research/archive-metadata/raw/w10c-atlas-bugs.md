# W10c: verification of the Atlas archive gap claims (w9a)

Scope: read-only check of the Atlas copy at `/mnt/user-data/uploads/git.xbmc4lyfe/Atlas-to-Rust/src/`:
`probe/archive.rs`, `probe/step.rs`, `archive/rar.rs`, `archive/zip.rs`, `archive/sevenz.rs`,
`archive/steps.rs`, `archive/store.rs`, `archive/mod.rs`, `par2.rs`. Each claim below was checked
against the cited lines and the code around them. Not in the copy and not verified: `archive/names.rs`
(`clean_name`, `best_name`), `ctx.files()` ordering, `duplicates::*`, config validation, `select::plan`.

Verdict key: CONFIRMED = the code does what the claim says and the claim's impact holds.
PARTLY = the code part is right but the stated impact or detail is off, or it depends on something outside the copy.
FALSE = the code does not do what the claim says.

---

## Verdicts (one line each)

1. RAR4 unknown size 0xFFFFFFFF stored as 4 GiB: PARTLY. The code never special-cases 0xFFFFFFFF, but whether it is a size marker is outside the copy, and the stored value is 4 GiB minus 1.
2. Probe RAR4 Unicode names use only the ASCII part: CONFIRMED.
3. Stale encryption columns: CONFIRMED (narrow trigger; a re-read has to flip a release from encrypted to not).
4. Probe zip encrypted flag ignores 0x0040 and 0x2000: CONFIRMED on the mismatch; impact is low.
5. Zip names decoded as Latin-1 instead of CP437 when GP bit 11 is clear: CONFIRMED (spec says CP437; the code uses Latin-1 on purpose, and the probe path uses a third decoding).
6. 7z encoded header supports only LZMA: CONFIRMED.
7. Per-file CRC32 dropped in RAR5, ZIP CD, and 7z substreams: CONFIRMED (RAR5 skips the field without reading it; RAR4 listing also never reads it).
8. PAR2 md5/md5_16k dropped in steps.rs listing_of: PARTLY. Dropped from the stored listing, but the full inventory still goes to `duplicates::twin_fingerprint` at steps.rs:247.
9. SFV candidate selection via take(3) can starve the SFV: CONFIRMED as code; it fires only if three small-text files come before the .sfv in `ctx.files()` order, which is not in the copy.
10. View cap of 256 rows vs 4096 stored: CONFIRMED, and worse than stated: PAR2 rows sort first, so a PAR2 listing over 256 files hides every archive row.

---

## 1. RAR4 unknown size 0xFFFFFFFF stored as 4 GiB: PARTLY

probe/archive.rs:75

```rust
head.unpacked = le32(data, at + 11).map(|low| u64::from(low) | high_word(36));
```

archive/rar.rs:108-110

```rust
if flags & 0x00e0 != 0x00e0 {
    let size = u64::from(unp) | u64::from(high_unp) << 32;
    l.push(&name4(name, flags), Some(size));
}
```

Reasoning: neither site checks for `0xFFFF_FFFF` in the low word without LHD_LARGE. A low word of
0xFFFFFFFF with a zero high word is stored as 4,294,967,295 (4 GiB minus 1, not 4 GiB). That part of
the claim is true. Whether a non-LARGE 0xFFFFFFFF means "unknown" is a format question I cannot settle
from this copy. The stored size is only used for stored archives in `moviehash_of` (probe/step.rs:313-317),
where a wrong size gives a wrong OpenSubtitles hash. So the impact is narrow.

## 2. Probe RAR4 Unicode names use only the ASCII part: CONFIRMED

probe/archive.rs:79-83

```rust
let name_len = usize::from(le16(data, at + 26).unwrap_or(0));
let high = if flags & 0x0100 != 0 { 8 } else { 0 };
head.name = data
    .get(at + 32 + high..at + 32 + high + name_len)
    .map(|n| String::from_utf8_lossy(n.split(|b| *b == 0).next().unwrap_or(n)).into_owned());
```

Reasoning: with FHD_UNICODE (0x0200) the name field is `ascii\0encoded`. The `split(... == 0).next()`
keeps only the ASCII fallback, so the real Unicode name is lost (for example, `Ünïcode.mkv` comes
back as its ASCII fallback, which may be `Unicode.mkv` or something worse). The listing path decodes the packed Unicode form (rar.rs:115-173, `unicode4`). The probe name is persisted
in `release_probe.info` (ArchiveHead derives Serialize) and compared in `moviehash_of` (probe/step.rs:344).
Both sides of that comparison come from the probe, so the match still works. The impact is the stored
probe name only.

## 3. Stale encryption columns: CONFIRMED

archive/steps.rs:141-152

```rust
writes.push(StepWrite::new(archive_store::UPSERT_RELEASE, vec![id.into(), listing.password().code().into()]));
if let Some(encryption) = listing.encryption {
    writes.push(StepWrite::new(archive_store::SET_ENCRYPTION, vec![ ... ]));
}
```

archive/store.rs:70-71

```rust
pub const UPSERT_RELEASE: &str = "insert into archive_release (release_id, password) values (?1, ?2)
    on conflict(release_id) do update set password = excluded.password";
```

Reasoning: when a later read finds `encryption == None`, `encryption`, `names_hidden` and
`password_check` keep the old values, while `password` is updated. The row can then read
password = No with encryption = RAR4 set. `read_encryption` (store.rs:110-125) reads those columns as-is.
Trigger: a re-read of the same release has to change from encrypted to not encrypted. That is rare, but
it is a real inconsistency. The fix is to write NULL/0 for the three columns on every finish.

## 4. Probe zip encrypted flag ignores 0x0040 and 0x2000: CONFIRMED (low impact)

probe/archive.rs:191

```rust
head.encrypted = flags & 1 != 0 || method == 99;
```

archive/zip.rs:36-38

```rust
fn is_encrypted(flags: usize, method: usize) -> bool {
    flags & (0x0001 | 0x0040 | 0x2000) != 0 || method == 99
}
```

Reasoning: the mismatch is real. From my reading of APPNOTE (not in the copy), strong encryption (0x0040)
also sets bit 0, so in practice the probe still flags it. The effect shows up in `stored_at` (probe
archive.rs:218 gates it on `!encrypted`), so a store-method entry with 0x0040 but no bit 0 would point
`moviehash` at ciphertext. Low, but the two parsers should agree.

## 5. Zip names decoded as Latin-1 instead of CP437 when GP bit 11 is clear: CONFIRMED

archive/zip.rs:25-34

```rust
/// UTF-8 when flagged or valid, otherwise Latin-1
fn name_of(raw: &[u8], flags: usize) -> String {
    if flags & 0x0800 != 0 { return String::from_utf8_lossy(raw).into_owned(); }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|b| char::from(*b)).collect(),
    }
}
```

Reasoning: APPNOTE says the default encoding without bit 11 is IBM CP437. The code uses Latin-1 by
design, and the comment says so. For a DOS-written name, byte 0x81 is `ü` in CP437 but a C1 control in
Latin-1. So the claim is right on the spec, and the code is a deliberate choice that will mis-decode
CP437 names. Also, probe archive.rs:217 uses `String::from_utf8_lossy` with no fallback at all, so the
probe name and the listing name disagree for the same bytes. That inconsistency is the extra point.

## 6. 7z encoded header supports only LZMA: CONFIRMED

archive/sevenz.rs:512-516

```rust
let folder = encoded.folders.first()?;
let [(id, props)] = folder.coders.as_slice() else { return None };
if id != LZMA || props.len() != 5 {
    return None;
}
```

Reasoning: `LZMA` is `[0x03, 0x01, 0x01]` (sevenz.rs:17). LZMA2 (`0x21`), a filter chain, or any
multi-coder folder returns None. `run` (sevenz.rs:463-470) then sets `damaged = true` and clears the
files, so the release reads as Password::Maybe with no names. Writers: 7-Zip's default header
compression is LZMA, so it is handled. Headers written with LZMA2 hit this path. I read in the gap
notes that py7zr does this, but that is not verifiable in the copy. Fix: decode LZMA2 (`0x21`, props
1 byte) and accept a single-coder chain.

## 7. Per-file CRC32 dropped: CONFIRMED (three sub-claims)

- RAR5, archive/rar.rs:265-267. The field is skipped, not read:
  ```rust
  if file_flags & 4 != 0 {
      p = p.checked_add(4)?;
  }
  ```
  The probe does read it (probe/archive.rs:145-148), so only the listing path drops it.
- RAR4, archive/rar.rs:90-110 (`file4`). Offset 16 (FILE_CRC) is never read.
- ZIP central directory, archive/zip.rs:304-324 (`list_tail`). Offset 16 of the CD entry is never read.
  The probe reads only the local header's CRC (probe/archive.rs:207).
- 7z, archive/sevenz.rs:299-304:
  ```rust
  let count: u64 = s.folders.iter().zip(&per_folder)....sum();
  r.digests(count)?;
  ```
  The per-substream digests are read and discarded. Folder CRCs are kept (sevenz.rs:254-257), but only to
  verify nested headers (sevenz.rs:534).

## 8. PAR2 md5/md5_16k dropped in steps.rs listing_of: PARTLY

archive/steps.rs:72-76

```rust
fn listing_of(inventory: par2::Inventory) -> Listing {
    let complete = inventory.complete;
    let files = inventory.files.into_iter().map(|f| InnerFile { name: f.name, size: Some(f.size) }).collect();
    Listing { files, complete, ..Default::default() }
}
```

Reasoning: true for the stored listing. `archive_inner_files` holds only name and size. But the same
`inventory.files` (with md5 and md5_16k) is passed whole to `duplicates::twin_fingerprint` at
archive/steps.rs:246-248, before `listing_of` is called. So md5 is not "dropped" from the step. Whether the
fingerprint uses md5 is in `duplicates`, which is not in the copy.

## 9. SFV candidate selection via take(3) can starve the SFV: CONFIRMED (conditional)

probe/step.rs:109-128

```rust
for file in files.iter().filter(|f| is_small_text(&f.filename)).take(3) {
    ...
    if file.filename.to_ascii_lowercase().ends_with(".sfv") {
        info.sfv = parse_sfv(&text);
    } else if info.nfo.is_none() {
        ...
    }
}
```

`is_small_text` (probe/step.rs:33-36) matches `.nfo`, `.sfv` and `.txt`. `take(3)` runs before the
`.sfv` test, so three `.nfo`/`.txt` files ahead of the `.sfv` in `ctx.files()` order means the SFV is
never fetched. The copy does not include `ctx.files()`, so the ordering is unverified. A second
aggravation: any `.txt` after the first NFO is fetched (one article, budget spent) and then discarded by
the `else if info.nfo.is_none()` guard. Fix: filter `.sfv` first, then take NFO/TXT separately.

## 10. View cap of 256 rows vs 4096 stored: CONFIRMED (worse than stated)

archive/store.rs:91-94

```rust
"select seq, name, size, source from {schema}.archive_inner_files where release_id = ? order by seq limit 256"
```

Storage: archive/mod.rs:25 (`MAX_ENTRIES = 4_096`), capped in `Listing::push` (mod.rs:247-255) and in
steps.rs:101-113 (every row of the listing is written). Ordering: `seq = source * 65_536 + i` (steps.rs:105)
and the view comment says "PAR2 rows first". So a release with a PAR2 listing of more than 256 files
shows only PAR2 rows and never its archive rows, even when they are stored. Fix: a per-source limit, or
order by source then seq, or raise the cap to `MAX_ENTRIES`.

---

## Extra findings (not in w9a, each with code)

E1. Listing with no names property reads as complete (archive/sevenz.rs:335-396). `read` stays empty when there is no
0x11 property, and the final check only tests `read.len() > MAX_ENTRIES`, so `l.complete = true` with zero files
and `Listing::password` (archive/mod.rs:234-244) returns No. Needs a CRC-valid but malformed header, so low.

E2. `zip::list` merges `encrypted` but not `encryption` from the head (archive/zip.rs:340-349):
```rust
from_tail.encrypted |= from_head.encrypted;
from_tail.password_check = from_tail.password_check.or(from_head.password_check);
```
If the head is encrypted and the tail has entries that are not flagged, `encrypted` is true with
`encryption == None`, and steps.rs:142 then skips `SET_ENCRYPTION`. Latent; a normal zip flags the CD too.

E3. RAR4 legacy-code-page names decoded as lossy UTF-8 (probe/archive.rs:83, archive/rar.rs:121-122):
`String::from_utf8_lossy(raw)` when FHD_UNICODE is clear. Bytes of 0x80 and up in an OEM/ANSI name become
U+FFFD. Same for ZIP in the probe (archive.rs:217). The listing for ZIP uses Latin-1 (see 5), so the
two parsers disagree.

E4. RAR vint in the probe accepts a 10th byte above 1 (probe/archive.rs:102-113). The shift on
the 10th byte drops the high bits silently; `archive/rar.rs:176-190` rejects it. Low, since a value this
large is only a malformed header.

E5. `Duration / u32` truncation (archive/steps.rs:391):
```rust
let gap = Duration::from_secs(60) / limits.probes_per_minute.max(1) as u32;
```
`probes_per_minute` is i64. A value that is a multiple of 2^32 becomes 0 after `as u32`, and
`Duration / 0` panics. Depends on config bounds, which are not in the copy. Low.

E6. 7z tail window assumes the header ends at the end of the tail (archive/sevenz.rs:481-487):
`let Some(next_at) = window.len().checked_sub(size)` in the tail case. Fine for normal 7z (the header
is last). A file with bytes after the header reads the wrong bytes, fails the CRC, and is marked
damaged. Not a bug for normal archives.

---

## Checked and correct (no finding)

- 7z NextHeaderOffset: `len = 32 + offset + size` (sevenz.rs:475). Correct.
- 7z NUMBER decode (sevenz.rs:47-60): the leading-ones rule and the 8-byte case are right.
- 7z StartHeaderCRC covers bytes 12..32 (sevenz.rs:40). Correct.
- RAR5 vint in `rar.rs` rejects a 10th byte above 1 (rar.rs:181). Correct, no overflow.
- RAR5 block flags: split-before is 0x08 and split-after is 0x10 (probe/archive.rs:138 uses 0x10). Correct.
- RAR4 header offsets for LARGE (32/36 high words, name at 40) match `file4` and the probe.
- ZIP64 extra field order (uncompressed then compressed, only the saturated ones) in `zip64_sizes`
  (zip.rs:49-72) and its CD call site (zip.rs:324). Correct.
- ZIP64 EOCD locator at eocd-20 and the record at eocd-76 (zip.rs:276-288). Correct.
- ZipCrypto check byte (zip.rs:203-204) and the ZipCrypto key schedule (zip.rs:128-152). Correct.
- AES strength to salt length and key length (zip.rs:117-118, 182-191). Correct.
- PAR2 packet offsets (set id 32, type 48, FileDesc body 0/16/32/48/56), Main body (block size 0, count 8, ids 12),
  UNICODE name at 16 (par2.rs:89-117). Correct.
