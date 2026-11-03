# w04-a random access into MKV / MP4 / TS (Atlas)

Legend: VERIFIED = read in source or spec this session. UNVERIFIED = memory or not checked.
RFC 9559 and RFC 8794 were NOT fetchable (rfc-editor, ietf.org, datatracker all 403 via proxy). Spec used: ebml_matroska.xml from ietf-wg-cellar.

## Sources read
- ebml_matroska.xml https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/ebml_matroska.xml
- matroska-demuxer 0.8.1 https://static.crates.io/crates/matroska-demuxer/matroska-demuxer-0.8.1.crate (src/lib.rs, block.rs, ebml.rs, element_id.rs)
- matroska 0.30.1 https://static.crates.io/crates/matroska/matroska-0.30.1.crate (metadata only, 0 hits for Cluster)
- mp4 0.14.0 https://static.crates.io/crates/mp4/mp4-0.14.0.crate (src/track.rs, reader.rs, mp4box/)
- mpeg2ts 0.3.1 https://static.crates.io/crates/mpeg2ts/mpeg2ts-0.3.1.crate (src/ts/packet.rs, adaptation_field.rs)

## Methods (YAML)
- method: cues_seek
  where: Cues 0x1C53BB6B (tail, usually). SeekHead 0x114D9B74 at front gives its offset.
  parse: SeekHead>Seek 0x4DBB > SeekID 0x53AB, SeekPosition 0x53AC, relative to Segment data start (VERIFIED demuxer parse_seek_head). CuePoint 0xBB > CueTime 0xB3 (TimestampScale units), CueTrackPositions 0xB7 > CueTrack 0xF7, CueClusterPosition 0xF1 (rel. Segment data; demuxer adds segment_data_offset), CueRelativePosition 0xF0 (v4+, rel. Cluster data start), CueBlockNumber 0x5378. Demuxer binary-searches CueTime, target = cluster_data_offset + relpos (VERIFIED lib.rs seek_broad_phase).
  use: keyframe byte offset for time t in one tail fetch. Cues size UNVERIFIED ~20-30 B per cue.
  source: ebml_matroska.xml; matroska-demuxer lib.rs

- method: cluster_resync
  where: any offset inside Segment body
  parse: scan for 1F 43 B6 75 (Cluster). Size = EBML vint; all data bits set = unknown size (VERIFIED demuxer u64::MAX). First child should be E7 Timestamp (uint, cluster base time), then A3 SimpleBlock or A0 BlockGroup (A1 Block, FB ReferenceBlock). Unknown-size clusters (live): end at next 1F43B675 or segment end (spec allows unknownsize on Cluster, VERIFIED XML).
  validate (false positive): (a) size vint decodes and fits in remaining Segment; (b) child E7 with 1-8 byte size; (c) element after E7 is A3/A0/A7/AB/EC; (d) SimpleBlock track vint is a track number from front Tracks; (e) strongest: next 1F43B675 at offset+size. Keep 3-byte overlap across article boundaries.
  estimate: random 4-byte match rate ~2^-32 per position, so ~0.05 raw hits per 225 MB of 2160p. Checks (b)-(e) make residual near zero. UNVERIFIED estimate.

- method: simpleblock_layout
  parse: track vint (marker removed), i16 BE relative timecode (abs = cluster Timestamp + rel, VERIFIED block.rs), flags byte: 0x80 keyframe (SimpleBlock only), 0x08 invisible, 0x01 discardable, 0x06 lacing (0 none, 1 Xiph, 2 fixed, 3 EBML). Lacing: 1 byte frame-count-1, then sizes (Xiph 255-runs; EBML first vint then signed vint deltas; fixed = equal). VERIFIED block.rs parse_laced_frames.
  BlockGroup keyframe = no ReferenceBlock (UNVERIFIED, from spec knowledge).

- method: mid_file_demux
  parse: matroska-demuxer MatroskaFile::open parses EBML header, Segment, SeekHead, Info, Tracks, Cues, Chapters, Tags, then first cluster (VERIFIED). No public from-offset API; fields private. next_frame() walks flat, entering Cluster/BlockGroup by data offset, so it works only if the reader sits on a Cluster header (VERIFIED). seek(ts) uses Cues fast path, else linear cluster scan (one seek per cluster header: unusable with lazy article fetch unless Cues exist).
  recommendation: own walker, about 150 lines: EBML id/size, Cluster > Timestamp > SimpleBlock; take TrackEntry (codec, CodecPrivate, channels, sample rate) from the front parse.
  matroska 0.30.1: metadata only (Tracks, Tags, Chapters); no clusters.
  webm-iterable 0.7.1 and mkvparser 0.2.0: NOT read.

- method: mp4_stbl_jump
  where: moov (tail when faststart is off). Fragmented: moof.
  parse: stss (sync sample numbers), stsc (sample-to-chunk), stco/co64 (chunk offsets), stsz (sizes). mp4 0.14.0 provides sample_offset (track.rs:437), is_sync_sample (track.rs:520), read_sample (reader.rs:168) (VERIFIED).
  use: nearest stss sample <= t, then byte offset = chunk offset + sizes before it in chunk. Tail moov for a 2 h film ~1-2 MB UNVERIFIED (about 3 articles).
  fragmented: mp4 0.14.0 has moof.rs but NO mfra/tfra/sidx parsing (0 grep hits, VERIFIED). Own mfra/tfra parser needed (tfra layout UNVERIFIED, ISO 14496-12 not read).

- method: ts_resync
  parse: mpeg2ts 0.3.1 asserts 0x47 at the current position, with no resync (VERIFIED packet.rs:96-97). Own resync: 0x47 at k, k+188, k+376, k+564 (188-byte TS). For M2TS (192-byte, 4-byte TP_extra_header), 0x47 at k+4 with 192 spacing (UNVERIFIED convention). PAT (PID 0) repeats often enough to read PMT PID after resync. PES start: payload_unit_start_indicator = byte1 & 0x40, then 00 00 01 start code. PTS 33-bit in 5 bytes (UNVERIFIED layout; mpeg2ts pes.rs not checked).
  keyframe: adaptation_field.random_access_indicator (VERIFIED adaptation_field.rs:17), set by some muxers only (UNVERIFIED).
  NAL start codes: AVC IDR type 5, AUD type 9; HEVC IDR 19/20, AUD 35, RPU NAL 62 (UNVERIFIED, from memory).

## Article budget (700 KB article, decimal; computed)
| stream | rate | articles/s | s per article | 1 GOP 2 s / 5 s (articles) |
|---|---|---|---|---|
| 1080p x264 8 Mb/s | 1000 KB/s | 1.43 | 0.70 | 3 / 8 |
| 2160p HEVC remux 60 Mb/s | 7500 KB/s | 10.7 | 0.093 | 22 / 54 |
| TrueHD 5 Mb/s | 625 KB/s | 0.89 | 1.12 | audio 10 s = 9; 30 s = 27 |
| DD+ 768 kb/s | 96 KB/s | 0.14 | 7.3 | audio 10 s = 2; 30 s = 4 |

Key constraint: in MKV/MP4 remuxes, audio is interleaved with video in the same clusters or moofs. So a 10 s audio window costs total bitrate x 10 s, not audio bitrate. 2160p remux + TrueHD = 65 Mb/s, so 10 s = ~116 articles (~81 MB), 30 s = ~348. 1080p + DD+ = ~9 Mb/s, so 10 s = ~16 articles. Audio-only fetch needs separate tracks or per-track Cues, which are UNVERIFIED for typical remuxes.

## Sampling plan
1. Front: 6 articles (already fetched). Duration from Info; SeekHead; Tracks.
2. Tail: 3-5 articles, covering Cues, Tags, and moov.
3. Three windows at 25/50/75% of duration. Target time t, then Cues CueTime <= t (video track), then cluster offset, then fetch W articles.
   - 1080p: W = 12 (~8.4 MB, ~12 s, covers 2+ GOPs).
   - 2160p remux: W = 40 (~28 MB, ~3.4 s). Covers one keyframe (I-frame size UNVERIFIED, ~1-3 MB) plus ~3 s of TrueHD.
4. Per file: ~46 (1080p) to ~130 (2160p) articles, ~32-91 MB.
5. Per-track bitrate and frame counts: check Tags at tail first. mkvmerge writes per-track statistics tags (BPS, DURATION, NUMBER_OF_FRAMES, NUMBER_OF_BYTES) UNVERIFIED, which would avoid most window fetches.
6. MP4: tail moov, then stss nearest t, then sample_offset, then fetch.

## Leads
- Get RFC 9559 text via another mirror (raw path on master was 404 for matroska.md).
- Check FFmpeg matroskadec.c (raw.githubusercontent.com/FFmpeg/FFmpeg) for cues/unknown-size handling and the cues_to_front muxer option (UNVERIFIED from memory).
- Check MKVToolNix statistics-tag source (gitlab is 403; try GitHub mirror).
- Read webm-iterable 0.7.1 and mkvparser 0.2.0 for resync code.
- mkvmerge cues strategy flags (default Cues at end, UNVERIFIED).

## Dead ends
- rfc-editor.org, ietf.org, datatracker.ietf.org: 403 via proxy.
- matroska-demuxer open() and seek(): not from-offset; linear seek is unusable with lazy fetch.
- matroska 0.30.1: no cluster or frame access.
- mp4 0.14.0: no mfra/tfra/sidx.
- mpeg2ts 0.3.1: no sync resync.
