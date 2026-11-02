# w10-a: Atlas integration and dependency cost

Repo: /home/claude/appz4fun/atlas @ a29ee52 (read only). Scratch: scratchpad w10a.46zI (depcheck, hello).

## 1. How bytes reach media::parse

- Entry: `probe/step.rs` `ProbeStep::run` (postprocess step 4, rank 120, needs Bodies, gated by `cfg.archive.enabled`). `inspect()` calls `media::parse(body)` on the main file's front bytes, and on the inner stored file when the archive head (`archive::parse`) points at one.
- Articles fetched per release (all via `ctx.fetch_article`, max 4 MiB each, `archive::ARTICLE_CAP`):
  - up to 3 small text files (NFO/SFV), first segment each;
  - first article of the main file (yEnc head, Xref, headers, `media::parse`);
  - middle article: existence check only (`article_exists`, no body);
  - last article: body read (yEnc tail, CRC, continuity);
  - `read_more_front`: parts 2..=min(total, FRONT_ARTICLES=6), only while `header_complete` is false;
  - moviehash: 1 to 2 more parts for the 64 KiB tail (`file_range`).
  - So roughly 3 to 12 bodies per release, never more than about 24 MiB of front data. MKV tags/Statistics and MP4 tail data are beyond that reach unless a new tail read is added.
- Front bytes are concatenated and parsed each time (`inspect` re-parses the whole buffer).

## 2. Storage and display

- `probe/mod.rs`: `ProbeInfo` (serde Serialize/Deserialize, `media: Option<media::MediaInfo>`) -> `serde_json::to_string` -> `release_probe.info TEXT NOT NULL` (sharded `s{n}`), via `SET_PROBE`. Indexed side columns: crc32, agent, moviehash. Read with `probe::read` (serde_json::from_str).
- `media.rs` structs all `#[serde(default)]`; `header_complete` is `serde(skip)`. Old JSON still parses (test `old_probe_json_still_reads`). Adding fields needs no migration.
- Staleness: `ProbeStep::config_stamp()` returns 1 (bit 1 = chapters, commentary, OpenSubtitles hash). Bump it to 2 so existing releases re-probe after the change.
- Display: `web.rs` line 1116 calls `probe::read`; `probe_html` (line 1193). Video line ~1296: codec, WxH, fps, bit depth, `hdr` list, "Dolby Vision profile N[, enhancement layer]". Audio line ~1317: codec, channels, Hz, language, "Atmos", commentary, name. Lines 285-286 and 689-690 are release-name tags, not probe data.
- The `grep` for `dv_profile|hdr|atmos` in web.rs shows no other consumers.

Gaps seen in media.rs: no CodecPrivate (0x63A2) read, so no HEVC/AVC profile or level from hvcC/avcC/av1C; MP4 ignores hvcC/avcC, colr, mdcv, clli, btrt, pasp, and the DV RPU; MKV Colour mastering ignored; atmos from name only; no bitrate, frame count, encoder, or Tags.

## 3. Dependency cost (scratch crate, 4 cores, fresh target)

- Clean `cargo build --release` of all 12 candidates (symphonia with default-features=false, aac+flac): 48 s wall, 2 m 49 s CPU.
- Resolved graph (normal+build): 85 crate versions, 84 names (bitflags twice). Largest adds: image 0.25.10, rustfft, realfft, rubato, symphonia 0.5.5 family (6 crates), moxcms, pxfm.
- Licences: 41 MIT OR Apache-2.0, 16 MIT, and others all permissive (Apache, BSD, Zlib, Unlicense, Unicode-3.0). Only copyleft: 6 MPL-2.0 (symphonia, symphonia-core, -metadata, -bundle-flac, -codec-aac, -utils-xiph). File-level copyleft, GPLv3-compatible, so OK for GPL-3.0-only Atlas. Flag for review.
- No -sys crates, no `links`, no `cc` in the graph. 13 build.rs crates, all common (serde, proc-macro2, quote, thiserror, anyhow, ...). `scuffle-workspace-hack` is a unification shim, harmless.
- Unsafe mentions (not audited): rust_h264 62 (incl. simd x86/neon), dolby_vision 109, rust_h265 41, hdr10plus 17, image 11, rustfft 1432 (SIMD), others 0-2.
- Binary: hello 342,520 B; depcheck (one call into each crate, release, strip) 1,575,768 B; delta about 1.23 MB.
- Overlap with Atlas Cargo.lock (416 packages): already present at the same version: serde 1.0.229, serde_core, serde_derive, anyhow, base64 0.22.1, bytes, memchr, num-traits, thiserror 2.0.21, proc-macro2, quote, zmij, bitflags 2.13.2, rustversion, and others (28 total). Same name, different version: log, serde_json (1.0.154 vs 1.0.151), syn (Atlas has 3), unicode-ident. New names: 53.
- Lean picks: scuffle-h265 and h264-reader (parse only, 0 unsafe), scuffle-av1, oxideav-ac3, dolby_vision (unsafe, check). Full decoders (rust_h264, rust_h265, rusty_h265) are heavy and only needed for frame-level RPU work. image_hasher: use default-features=false to avoid pulling image codecs (not measured).

## 4. Struct changes (serde(default), additive)

```rust
// VideoTrack: keep every existing field. New:
pub profile: Option<String>,        // "Main 10"
pub level: Option<String>,          // "5.1"
pub chroma: Option<String>,         // "4:2:0"
pub full_range: Option<bool>,
pub matrix: Option<u64>,            // colour matrix code
pub max_cll: Option<u64>, pub max_fall: Option<u64>,
pub mastering: Option<Mastering>,
pub hdr_formats: Vec<String>,       // HDR10, HDR10+, HLG, PQ, DV
pub dv: Option<DolbyVision>,
pub hdr10plus_profile: Option<String>,
pub encoder: Option<String>,        // x264 / x265 SEI or Tags ENCODER
pub cc: Option<String>,             // "CEA-708", "CEA-608"
pub film_grain: Option<bool>, pub interlaced: Option<bool>, pub stereo3d: Option<u64>,
pub bitrate: Option<u64>, pub frames: Option<u64>,

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)] #[serde(default)]
pub struct Mastering { pub max_nits: Option<f64>, pub min_nits: Option<f64>, pub primaries_xy: Option<[f64; 8]> }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)] #[serde(default)]
pub struct DolbyVision {
    pub profile: u8, pub level: u8, pub compat_id: Option<u8>,
    pub el: Option<String>,         // "None" | "Mel" | "Fel"
    pub cm_version: Option<String>, // "CM v2.9" / "CM v4.0"
    pub l6: Option<[u16; 2]>,       // max_content_nits, max_frame_avg_nits
    pub l5: Option<[u16; 4]>,       // active area offsets
    pub rpu_seen: bool,
}

// AudioTrack: keep every existing field. New:
pub atmos_source: Option<String>,   // "name" | "bitstream"
pub dtsx: bool,
pub dialnorm: Option<i8>,
pub bsmod: Option<u8>, pub service: Option<String>,   // AC-3 / E-AC-3
pub bitrate: Option<u64>,
pub flac_md5: Option<String>,
```

Meaning changes:
- `VideoTrack.hdr`: today only HDR10/HLG/PQ from transfer+mastering. Becomes the primary label; full list goes to `hdr_formats`.
- `dv_profile`, `dv_level`, `dv_enhancement_layer`: today from dvcC/dvvC only. Keep them as mirrors of `dv`; `dv_enhancement_layer` should be derived from `dv.el`.
- `codec`: MKV uses the Matroska ID (`V_MPEGH/ISO/HEVC`), MP4 uses the fourcc (`hvc1`). Normalise to one vocabulary and say so in the UI.
- `AudioTrack.atmos`: today name heuristic only (false positives). Set true also from bitstream; `atmos_source` says which.
- `header_complete` and `config_stamp` (see section 2) decide whether the new fields need more bytes.
