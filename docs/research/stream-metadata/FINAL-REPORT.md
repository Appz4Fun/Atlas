# Stream metadata for releases: video, HDR, Dolby Vision and audio

Nov 2, 2026. Exported from the Claude Doc https://claude.ai/artifact/ByUTRbbX7sHy2H78JK9iVo

## Summary and ranking

Everything Atlas needs to describe a release's video, HDR, Dolby Vision and audio is already in the front articles it downloads today. It just isn't parsed past the container. Every key below runs in pure Rust with no ffmpeg; frame and audio decode included, all tested against ffmpeg on generated files and, for Dolby Vision and TrueHD, on real encoder output. A combined prototype (`research/stream-metadata/code/streamprobe/`) prints all the phase 1 fields as JSON and matches ffprobe on 12 test files.

| Rank | Idea | Value | Effort | Extra articles | CPU per release | Tested |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | HDR format, bit depth, profile, level, fps and colour from the SPS in CodecPrivate, plus SEI 137/144 | High: fixes wrong HDR labels today | Low | None | Under 1 ms | Yes |
| 2 | Dolby Vision from the first RPU: profile, FEL/MEL, CM version, L5, L6, L9, L11 | High for 4K | Low | None | Under 5 ms | Yes, 27 real RPUs |
| 3 | x264/x265 settings string as encoder tag and same-encode key | High for twins and alts | Low | None | Negligible | Yes |
| 4 | Atmos and DTS:X from audio frames, plus dialnorm, service, bitrate, HE-AAC | Medium to high | Low | None | Under 1 ms | TrueHD Atmos yes; DD+ JOC and DTS:X from source only |
| 5 | HDR10+ present and profile | Medium | Low | None | Under 1 ms | Yes |
| 6 | Captions, film grain, interlace, 3D, HLG compat tags | Low to medium | Low | None | Negligible | MBAFF yes, rest from source |
| 7 | Remux-proof keys: first-keyframe hash, AC-3 crc2 run, FLAC MD5 | Medium for alts | Low | None | Negligible | Yes |
| 8 | Keyframe thumbnail and pHash at 25/50/75% | Medium: same-film links, release page frames | Medium | About 36 (1080p) to 120 (2160p) | 10 to 90 ms per frame | Yes |
| 9 | Audio fingerprint (chromaprint) | Low to medium: same film across codecs | Medium | 16 (1080p) to 116 (2160p remux) per 10 s | About 50 ms | Yes, AC-3/AAC/FLAC |

Ranks 1 to 7 cost no bandwidth. Value scores are judgment calls. Bandwidth and CPU numbers come from the test runs.

## What Atlas does today

Atlas already fetches the right bytes but reads only the container around them. ProbeStep (postprocess step 4) hands `src/probe/media.rs` the main file's first article plus up to 6 more front parts. It stores a `ProbeInfo` JSON in `release_probe.info`, and `web.rs` (`probe_html`, about line 1193) shows it. Every media struct is `serde(default)`, so new fields need no migration. Bumping `ProbeStep::config_stamp` from 1 to 2 re-probes the catalog.

| Field | Atlas reads it from | What the tests showed |
| --- | --- | --- |
| HDR label, transfer, primaries | MKV Colour element only | Empty in ffmpeg and mkvmerge x265 files, so they show no HDR or just "PQ" |
| Bit depth | MKV Colour BitsPerChannel | Empty in the same files, so 10-bit shows as unknown |
| Mastering display, MaxCLL | Only notes that MasteringMetadata exists | The values live in SEI 137/144 |
| DV profile, level, EL flag | dvcC/dvvC in BlockAdditionMapping (bit layout correct) | ffmpeg writes none, so DV in ffmpeg-made files is missed; no compat id (8.1 vs 8.4), no FEL/MEL, no CM version |
| Atmos | Track name contains "atmos" | The bitstream flag sits in the first audio frames already fetched |
| Frame rate | MKV DefaultDuration | Fine for MKV; VUI timing is exact |
| Profile, level, encoder, bitrate, CC, interlace, 3D | Not read | All in CodecPrivate or the first keyframe |

The cheapest fix is the first row group: parse the SPS VUI from the hvcC/avcC/av1C already in CodecPrivate, then walk the first keyframe's NALs for SEI and the RPU. No extra articles are needed.

## Video fields

Every field below came out of the first 1 MiB of a test MKV in Rust and matched ffprobe (wave 5), unless the Tested column says otherwise. The codec config in CodecPrivate (hvcC, avcC, av1C) carries the SPS, so most of these need no frame at all.

| Field | Where it lives | Rust parse | Tested | Atlas today |
| --- | --- | --- | --- | --- |
| Profile, tier, level | hvcC bytes 1-12, avcC 1-3, av1C | own (hvcC), scuffle-av1 | Yes | No |
| Bit depth, chroma | hvcC bytes 17-18, SPS, av1C | own, scuffle-h265 | Yes | Only from MKV Colour, which is usually empty |
| Coded and cropped size | SPS conformance window (1088 to 1080) | scuffle-h265, h264-reader | Yes | MKV PixelWidth/Height |
| Frame rate | VUI time\_scale / num\_units\_in\_tick (AVC divides by 2) | scuffle-h265, h264-reader | Yes | MKV DefaultDuration |
| Primaries, transfer, matrix, full range | SPS VUI | scuffle-h265 | Yes | Only from MKV Colour |
| x264 settings | SEI 5, UUID dc45e9bd-e6d9-48b7-962c-d820d923eeef, first frame only | own | Yes | No |
| x265 settings | SEI 5, UUID 2ca2de09-b517-47db-bb55-a4fe7fc2fc4e, first frame and the hvcC SEI array | own | Yes | No |
| Interlaced (PAFF, MBAFF) | AVC frame\_mbs\_only\_flag, mb\_adaptive\_frame\_field\_flag; HEVC field\_seq\_flag, pic\_struct | h264-reader | MBAFF yes | No |
| Closed captions | SEI 4, T.35 0xB5 / 0x0031 "GA94", cc\_count above 0 | own | Source only | No |
| Film grain | AV1 film\_grain\_params\_present; SEI 19 | own | Source only | No |
| 3D | SEI 45 (3 = SBS, 4 = TB), MVC NAL 15/20, MV-HEVC layer above 0 | own | Source only | No |
| HLG compatibility | SEI 147 | own | Source only | No |

- The x264 and x265 strings name the encoder build and every setting (crf, preset, aq, psy). The same string at the same file size is very likely the same encode. NVENC, libaom, SVT-AV1 and AMF write no such string.
- MuxingApp and WritingApp (mkvmerge version) and SegmentUID are container fields that the container thread catalogues.

## HDR10, HDR10+ and Dolby Vision

Read HDR from the bitstream, not the container. In the tests, x265 files muxed by ffmpeg and by mkvmerge had no Matroska mastering metadata or bit depth. ffmpeg writes no dvcC box at all, in MKV or MP4. The SPS VUI, SEI 137/144, the HDR10+ SEI and the DV RPU are always there, in the first keyframe.

| Field | Where it lives | Rust parse | Reach | Tested |
| --- | --- | --- | --- | --- |
| HDR10 mastering display | SEI 137 (AV1: metadata OBU) | own | First keyframe; ffmpeg muxes also copy it into hvcC, mkvmerge strips it | Yes |
| MaxCLL, MaxFALL | SEI 144 | own | Same as above | Yes |
| HDR10+ present and profile A/B | SEI 4, T.35 0xB5 / 0x003C / 0x0001, app 4 v1 | hdr10plus 2.1.5 `Hdr10PlusMetadata::parse` | First keyframe | Yes, profile B, windows, percentiles, bezier |
| DV config: profile, level, rpu/el/bl, compat id | MKV BlockAdditionMapping dvcC/dvvC, MP4 dvcC, TS descriptor 0xB0 | own: u8 major, u8 minor, u16 \[profile 7b, level 6b, rpu, el, bl\], u8 \[compat 4b, md\_compression 2b\] | Header; missing in ffmpeg-made files | Layout from ffmpeg source |
| DV present, profile 4/5/7/8 | RPU, HEVC NAL 62 (AV1: T.35 provider 0x003B) | dolby\_vision 3.4.0 `DoviRpu::parse_unspec62_nalu` | First RPU | Yes, 27 real dovi\_tool fixtures |
| FEL or MEL | RPU NLQ block; MEL = all-zero offsets and deadzones | dolby\_vision `el_type` | First RPU (carry forward when use\_prev\_vdr\_rpu is set) | Yes, 11 of 11 real P7 fixtures |
| CM version 2.9 or 4.0 | v4.0 if any of L3, L8, L9, L10, L11, L254 is present | dolby\_vision `vdr_dm_data` | First RPU | Yes, 27 of 27 |
| L5 letterbox, L6 MaxCLL/FALL, L9 primaries, L11 content type, L254 | RPU extension blocks | dolby\_vision | First RPU, fixed for the stream | Yes |
| L1, L2 and L8 trim sets | RPU, change per shot | dolby\_vision | Whole file for the full set | Yes |
| 8.1 vs 8.4 | Only the dvcC compat id (1 vs 4) or VUI transfer 18 | own | Header or SPS | Yes, RPUs are identical |

HDR10+ can also sit outside the HEVC stream. In MKV it may be a BlockAdditional per frame, flagged by a BlockAdditionMapping of type 4 (ITU-T T.35), with the same 0xB5 / 0x003C payload. So check both places. This one comes from the container thread's reading of ffmpeg's matroskadec.c and hasn't been tested here.

Things to handle:

- `cmv29_metadata` is set even on CM v4.0 RPUs, so decide the CM version by which levels are present.
- An L6 of 4000/50 with CLL and FALL of 0 is common. Store it as unknown, not 0.
- dolby\_vision 3.4.0 on crates.io rejects the whole RPU when it holds a Level 253 block. dovi\_tool main already handles it, so pin the git version or keep the dvcC result when the RPU parse fails.
- MaxCLL and scene counts the way dovi\_tool `info` reports them need every RPU. From the front they are lower bounds, so label them that way or leave them out.
- The DV level table (level to resolution and fps) and compat id names are still unverified, because the Dolby profiles PDF was out of reach.

## Audio fields

Audio metadata comes from the first frame of each track, which is already inside the front articles. MKV CodecPrivate is usually empty for A\_AC3, A\_EAC3, A\_TRUEHD and A\_DTS, so parse frames, not the track header. symphonia has no AC-3, E-AC-3, DTS or TrueHD support, so these are small std-only parsers.

| Codec | Fields | Atmos / X rule | Tested |
| --- | --- | --- | --- |
| AC-3 | Rate, channels (acmod, lfeon), bitrate, dialnorm, bsmod (service), cmixlev, surmixlev, mixlevel, roomtyp, dsurmod (Dolby Surround) | n/a | Matches encoder settings and ffprobe |
| E-AC-3 | strmtyp, substreamid, acmod, lfeon, dialnorm, chanmap (7.1 needs the dependent substream) | addbsi extension flag plus complexity index (ffmpeg), or EMDF 0x5838 payload 14 JOC in at least 1/8 of frames (MediaInfo); MP4 dec3 tail flag | 5.1 tested; 7.1 and JOC untested, no sample reachable |
| TrueHD / MLP | Rate, 6ch/8ch layout, VBR, peak rate, substreams, CRC | Atmos = FBA sync and substream\_info bit 7; flag it if substreams isn't 4 | Rule and CRC verified on 52 files including 3 Atmos (truehdd fixtures) |
| DTS | Core: amode, rate, bitrate, LFE, bit depth; ExSS 0x64582025, XLL 0x41A29547 (MA), XBR 0x655E315E (HRA) | DTS:X = XLL extradata sync 0x02000850, IMAX 0xF14000D0 | Core tested; HD and X from ffmpeg source only |
| AAC | Object type (LC, HE, HEv2), rate, channel config, SBR/PS | n/a | From ffmpeg source |
| FLAC | Rate, channels, bits, total samples, MD5 of the decoded audio | n/a | MD5 equals the decoded PCM MD5 |
| Opus | Channels, pre-skip, input rate, mapping family | n/a | From RFC 7845 |

Two cautions. A DTS:X sync word announces object metadata but doesn't prove objects are present, as MediaInfo's own source notes. The TrueHD Atmos object count isn't in the major sync, so store Atmos as yes/no with objects unknown.

## Frames and fingerprints

Frame thumbnails, frame hashes and audio fingerprints all work in pure Rust, with no ffmpeg. Three kinds of key come out of them.

| Key | Proves | How | Measured |
| --- | --- | --- | --- |
| Hash of the first keyframe's NAL bytes | Same encode (an alt), even across MKV, MP4 and TS | Hash the IRAP payload | Keyframe bytes identical across MKV, MP4 (3 layouts) and TS |
| AC-3 / E-AC-3 crc2 sequence | Same audio track, remuxed | Last 2 bytes of each syncframe, skip silent runs | Identical in MKV and MP4 |
| FLAC STREAMINFO MD5 | Same decoded audio | Header field | Equals the MD5 of the decoded PCM |
| Keyframe pHash | Same film, any encode | Decode, crop letterbox, equalise, 64-bit DCT hash | Same frame max 4 bits apart, other shot of the same film min 24 |
| Audio fingerprint | Same film, any codec | Decode to PCM, rusty-chromaprint | FLAC vs AAC 0.992, AC-3 vs FLAC 0.99 from 5 s, unrelated about 0 |

Decoders tested bit-exact against ffmpeg:

- HEVC 8 and 10-bit: rust\_h265 0.4.0 and rusty\_h265 0.6.2. About 10 ms per 720p keyframe and 90 ms per 2160p keyframe on one core, about 250 MB peak at 2160p. Open-GOP CRA starts work.
- H.264 8-bit, including MBAFF interlaced: rust\_h264 0.6.0, about 11 ms at 720p.
- MPEG-2 (DVD): oxideav-mpeg12video 0.0.13 gets 76 dB, fine for a thumbnail.
- AV1: rav1d (pure Rust dav1d port) is the path but wasn't run.
- AC-3 audio: oxideav-ac3 0.0.11. AAC and FLAC: symphonia. No pure-Rust DTS or TrueHD decoder exists, so those tracks get headers and no fingerprint.

Frame hash recipe, from wave 9: crop rows and columns with mean luma under 20, histogram-equalise, then a 32 to 8 DCT pHash. Call it a match at 10 bits or less out of 64, and require 2 of 3 frames to match. Without the crop, a 2.39:1 letterboxed copy is 19 bits away. A PQ re-encode stayed within 2 bits. A real HDR grade against its SDR grade is still untested.

## Fetching just enough

The front articles Atlas already reads hold nearly every field above. Only thumbnails, frame hashes and audio fingerprints need more. A 700 KB article maps 1:1 to inner-file bytes in a store-mode RAR.

| What | Articles | Measured on |
| --- | --- | --- |
| Codec config, SPS, encoder SEI, HDR10 SEI, HDR10+, first DV RPU, audio frame headers | The front 6 Atlas already fetches | All test files |
| MKV mid-file keyframe plus 10 s of audio, cluster resync from 50% | 16 beyond the front | 60 s 720p x264 + AC-3 |
| The same via Cues (tail Cues / front Cues) | 14 / 13 beyond the front | Same file |
| TS or M2TS keyframe after a resync at any offset | 1, at most 2 | 20 s HEVC + AC-3 |
| MP4 keyframe at 50% (moov at end, faststart, fragmented) | 2 | 3 HEVC layouts |
| 10 s audio window on a 2160p remux with TrueHD | About 116 (audio is interleaved) | Computed, not run |

The container thread measured keyframe sizes on grainy synthetic video. The first keyframe was 576 KB at 1080p x264 12 Mb/s and 970 KB at 2160p HEVC 40 Mb/s. That is 1 or 2 articles per keyframe once the walker lands on a cluster.

Walker rules the tests turned up:

- MKV: scan for Cluster 1F 43 B6 75, skip CRC-32 (0xBF) and Void (0xEC), then require Timestamp (0xE7), a SimpleBlock or BlockGroup, a known track number and the next Cluster at offset plus size. matroska-demuxer can't start mid-file, so this is about 150 lines of our own.
- MKV header stripping (ContentEncoding algorithm 3): put the stripped bytes back on every frame before looking for 0x0B77 or NAL lengths.
- TS: 0x47 five times at stride 188, or 192 with a 4-byte prefix, then PAT/PMT and PES. Accept CRA (NAL 21) as a start point, because x265 only makes an IDR at the start.
- Compressed archives: the archive thread got prefix decode working for 7z LZMA2 and RAR5 in pure Rust, so front fields work there too. Mid-file windows need store mode.

Suggested sampling: front 6 articles for everything, then for frame hashes three windows at 25, 50 and 75% (about 12 articles each at 1080p, 40 at 2160p). Spend the audio-fingerprint budget only on 1080p encodes or on releases nothing else identified.

## Crates, licences and build cost

All twelve candidate crates together build clean in 48 s wall on 4 cores and add about 1.2 MB to a stripped binary. There are no -sys crates and no C build steps. The licences are MIT, Apache, BSD, Zlib and Unicode, plus MPL-2.0 for the symphonia family, which is file-level copyleft. 28 of the 85 transitive crates are already in Atlas's Cargo.lock.

| Crate | Version | Licence | Use | Phase |
| --- | --- | --- | --- | --- |
| scuffle-h265 | 0.2.2 | MIT/Apache | HEVC SPS and VUI | 1 |
| h264-reader | 0.9.0 | MIT/Apache | AVC SPS, PPS, SEI | 1 |
| scuffle-av1 | 0.1.4 | MIT/Apache | av1C and sequence header | 1 |
| dolby\_vision | 3.4.0 | MIT | DV RPU | 1 |
| hdr10plus | 2.1.5 | MIT | HDR10+ SEI | 1 |
| rust\_h265 or rusty\_h265 | 0.4.0 / 0.6.2 | MIT/Apache / Apache | HEVC keyframe decode | 2 |
| rust\_h264 | 0.6.0 | MIT/Apache | AVC keyframe decode | 2 |
| image\_hasher | 3.x | MIT/Apache | Frame hash (or own 40-line DCT) | 2 |
| rusty-chromaprint | 0.3.0 | MIT | Audio fingerprint | 3 |
| oxideav-ac3 | 0.0.11 | MIT | AC-3 / E-AC-3 decode | 3 |
| symphonia | 0.5 | MPL-2.0 | AAC and FLAC decode | 3 |

Audio frame headers (AC-3, E-AC-3, TrueHD, DTS, AAC, FLAC, Opus) need no crate: the std-only parsers in `research/stream-metadata/code/` are a few hundred lines. The young crates (oxideav-ac3, rusty\_h265, scuffle-h265 marked unstable) should be pinned exactly. rust\_h265 has about 501k downloads; the others are much smaller.

## Suggested order and open questions

1. Phase 1, no new bandwidth: in `src/probe/media.rs`, parse hvcC/avcC/av1C and the SPS VUI, then walk the first keyframe for SEI 5/137/144/HDR10+ and the DV RPU, then the first frame of each audio track. Keep Atlas's own EBML walker: the streamprobe test showed matroska-demuxer follows SeekHead to the file's end and fails on a partial file. Add the new `serde(default)` fields and bump `config_stamp` to 2.
2. Show the new fields on the release page and as search tags (hdr10, hdr10plus, dv, dvp7, fel, mel, atmos, dtsx, the encoder name).
3. Store the remux-proof keys (first-keyframe hash, crc2 run, FLAC MD5) next to the twin and alt keys.
4. Phase 2: the mid-file walker and keyframe decode for thumbnails and pHash at 25/50/75%, sharing work with the release-page frames thread.
5. Phase 3, only for releases nothing else linked: audio fingerprints.

Open questions:

- Are the new crates OK? Phase 1 needs scuffle-h265, h264-reader, scuffle-av1, dolby\_vision and hdr10plus, all MIT or Apache. Phase 2 adds rust\_h265 and rust\_h264. Phase 3 adds rusty-chromaprint, oxideav-ac3 and symphonia (MPL-2.0).
- For Dolby Vision, pin dolby\_vision from git to get the Level 253 fix, or stay on crates.io 3.4.0 and fall back to dvcC when the RPU parse fails?
- Real samples for E-AC-3 Atmos (JOC), E-AC-3 7.1, DTS-HD MA and DTS:X weren't reachable from this sandbox. The sample archives are blocked. A run of streamprobe on a few real releases on the Mac would close these.
- The Dolby DV level table and compat id names are still unverified.
- A real HDR grade against its SDR grade hasn't been hashed yet. The synthetic PQ test passed.

Raw notes, code and the YAML field catalog are in `docs/research/stream-metadata/`. The catalog file is `catalog.yaml`.
