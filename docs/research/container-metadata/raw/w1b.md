# w1b: MP4/MOV (ISO BMFF) metadata catalog for Atlas (Rust-native, no ffmpeg/mediainfo)

Scope note: items marked [fetched] were checked in this session. Items marked [spec] come from ISO/IEC 14496-12/-14/-15 and Apple QuickTime docs as recalled and were NOT re-verified here; confirm against the spec before coding. Time budget was 10 minutes, so the crate survey is shallow.

## 1. ftyp
- path: ftyp.major_brand, ftyp.compatible_brands[] | type: fourcc[] | meaning: container flavor/muxer intent | examples: isom, iso2, mp41, mp42, M4V (Apple), M4A, qt  (brand list also carries dby1 for Dolby Vision, avc1/hvc1 hints) [spec] | front_of_file: yes (ftyp is always first) | linking_value: alt (brand set changes with muxer, not content)
- Dolby Vision marker brand dby1 in compatible_brands [spec, not fetched]

## 2. moov / mvhd [spec]
- mvhd.creation_time, modification_time | u32/u64 seconds since 1904-01-01 | creation time of encode/mux | 0 is common from some muxers | front_of_file: depends (faststart) | linking_value: twin (same mux run) / alt
- mvhd.timescale | u32 | ticks per second | e.g. 1000 or 90000 | depends | none
- mvhd.duration | u32/u64 in timescale ticks | movie duration | divide by timescale | depends | same-film (strong with frame count)
- mvhd.next_track_ID | u32 | muxer track counter; reveals number of tracks ever allocated (gaps = removed tracks) | depends | alt
- mvhd.rate, volume, matrix | fixed-point | playback defaults, rarely informative

## 3. tkhd (per track) [spec]
- tkhd.track_ID | u32 | track id | 1,2,3... | depends | none
- tkhd.flags | 24-bit | bit0 enabled, bit1 in_movie, bit2 in_preview; 0x000001 default, 0x000007 typical | depends | none
- tkhd.width, height | 16.16 fixed | display size (may differ from coded size, see pasp/clap) | depends | same-film
- tkhd.creation_time, modification_time | as mvhd | per track | depends | twin
- tkhd.alternate_group | i16 | groups alternative tracks (e.g. audio languages, commentary) | depends | none
- tkhd.layer | i16 | z-order | usually 0 | none
- tkhd.volume | 8.8 | 1.0 for audio, 0 for video | depends | none
- tkhd.matrix | 9 x 32-bit | rotation (phone portrait video = 90/270 deg) | depends | none

## 4. mdhd / hdlr [spec]
- mdhd.language | 15-bit packed ISO-639-2/T, e.g. 'eng' = 0x15C7 | per track language | depends | none
- mdhd.timescale, mdhd.duration | per track ticks | exact track duration = duration/timescale | depends | same-film
- hdlr.handler_type | fourcc: vide, soun, text, sbtl, subt, meta, hint, clcp | track kind | depends | none
- hdlr.name | UTF-8 (Mac: Pascal-ish counted) | encoder/muxer fingerprint, e.g. "VideoHandler", "SoundHandler" (ffmpeg/ffmpeg-like), "L-SMASH", "GPAC ISO Video Handler", "Core Media Video" (Apple) | depends | alt, good for muxer fingerprinting

## 5. stsd sample entries [spec + mp4ra]
- stsd.entry.fourcc | video: avc1, avc3, hvc1, hev1, dvh1, dvhe, av01, vp09, mp4v; audio: mp4a, ac-3, ec-3, Opus, fLaC, alac; text: tx3g, wvtt, stpp, c608 | codec identity; dvh1/dvhe are DV | depends | same-film (codec differs across encodes)
- mp4ra registry is the authoritative fourcc list: https://mp4ra.org/registered-types/codecs [fetched URL, content not read]; generated Rust table exists in mp4ra-rust: https://docs.rs/mp4ra-rust/0.6.0/src/mp4ra_rust/generated.rs.html [fetched URL]
- video sample entry fields: width, height, horizresolution/vertresolution (72 dpi typical), frame_count (usually 1), compressorname (32-byte Pascal string: encoder name, e.g. "x265", "HandBrake", "Lavf"), depth (24 or 0x0018) [spec]
- audio sample entry fields: channelcount, samplesize, samplerate (16.16 fixed; 48000 typical; hz above 65535 uses esds/dOps/dfLa) [spec]

### Config boxes
- avcC: profile_idc, level_idc, nal lengths, SPS/PPS (decodable profile/level/resolution-independent) | depends | same-film
- hvcC: general_profile_idc, tier, level, chroma/bit depth, parameter sets | depends | same-film
- av1C: seq_profile, seq_level_idx, high_bitdepth, twelve_bit, monochrome, chroma flags | depends
- vpcC: profile, level, bitDepth | depends
- esds: MPEG-4 DecoderConfigDescriptor: objectTypeIndication (0x40 AAC), DecoderSpecificInfo = AudioSpecificConfig (profile, sample rate index, channel config, SBR/PS flags) | depends | same-film
- dac3: AC-3 fscod (sample rate), bsid, bsmod, acmod (channel layout), lfeon, bit_rate_code (bitrate lookup) | depends | same-film
- dec3: E-AC-3 data_rate (kbps), num_ind_sub, channel map, and JOC/Atmos flag (complexity_index_type_a / JOC extension) [spec] | depends | same-film; JOC is the Atmos signal
- dOps: Opus OpusHead: channels, pre_skip, input sample rate, output gain | depends
- dfLa: FLAC metadata block(s): STREAMINFO (sample rate, channels, bits per sample, total samples, MD5) | depends
- btrt: bufferSizeDB, maxBitrate, avgBitrate (bps) | encoder-declared bitrate, sometimes 0 | depends | same-film

### Color and HDR
- colr (type nclx): colour_primaries, transfer_characteristics, matrix_coefficients, full_range_flag | e.g. 9/16/9 = BT.2020/PQ/BT.2020 NCL = HDR10 signal | depends | same-film
- colr (type prof): embedded ICC profile | depends
- colr (type rICC): restricted ICC | depends
- mdcv (SMPTE ST 2086 mastering display): primaries xy, white point xy, max/min luminance | HDR10 mastering | depends
- clli (content light level): maxCLL, maxFALL | HDR10 | depends
- pasp: hSpacing, vSpacing (pixel aspect ratio, e.g. anamorphic 4:3 in 16:9) | depends
- clap: cleanAperture width/height/offset | cropped display area | depends
- Note: HDR10+ (ST 2094-40) and DV RPU are NOT in container boxes. HDR10+ is per-frame T.35 SEI in the HEVC bitstream. DV RPU is HEVC NAL unit type 62 (UNSPEC62) per frame. Both need bitstream parsing of mdat samples (length-prefixed per hvcC), so they are only readable if the sample data is in hand. [spec]

### Dolby Vision boxes
- dvcC (profiles 0-7) / dvvC (profile 8+ per spec) / dvwC (profile 10 AV1, FFmpeg patch 2021) [fetched: https://lists.mplayerhq.hu/pipermail/ffmpeg-cvslog/2020-April/121746.html ; https://lists.mplayerhq.hu/pipermail/ffmpeg-cvslog/2021-October/129625.html]
- Layout [fetched, from FFmpeg mov_read_dvcc_dvvc]: dv_version_major u8, dv_version_minor u8, then a 16-bit field: dv_profile 7 bits, dv_level 6 bits, rpu_present_flag 1, el_present_flag 1, bl_present_flag 1; then dv_bl_signal_compatibility_id 4 bits from the next byte (only if box size >= 24, else 0).
- Meaning of compat id [spec]: 0 none, 1 = HDR10 base, 2 = SDR base, 4 = HLG base. Profile 5 = IPT-PQ no backward-compatible base; profile 7 = dual layer BL+EL (MEL/FEL, found on UHD Blu-ray rips); profile 8.1/8.4 = single layer with compat id.
- dv level 1-13, RPU/EL/BL presence flags reveal whether an enhancement layer exists (FEL vs MEL is decided by EL content, not the box). [spec]
- front_of_file: yes if moov is first. linking_value: same-film (profile/level/compat id same across encodes), twin only if same mux run.

## 6. Sample tables (stbl) [spec]
- stts: decoding time deltas | derived fps = timescale / delta (or sample count / duration) | depends | same-film
- ctts: composition offsets (B-frame reorder) | depends
- stss: sync sample list | keyframe interval = gaps between entries | depends
- stsz: sample sizes (sample_count is exact frame count for video; per-sample bytes gives exact bitrate per track) | depends | same-film
- stsc, stco/co64: chunk layout (co64 = 64-bit offsets, large files). Chunk offsets point into mdat, so when moov is at end, offsets are past the first MB. [spec]
- Derived: frame count, avg bitrate = sum(stsz)*8/duration, max bitrate windows, keyframe interval. These need moov only; they are good same-film fingerprints.
- elst (edts): edit list. media_time -1 = empty edit (start delay / audio priming); audio priming offsets (~1024 samples AAC) show as elst media_time. Affects true start time and duration. [spec]

## 7. udta / meta / ilst (iTunes/QuickTime tags) [spec]
- udta.©nam (title), ©ART (artist), ©alb, ©day (year), ©gen, ©cmt, ©too (encoder: "HandBrake 1.x", "Lavf", "x265", "GPAC", "L-SMASH"), desc, ldes (long description), tvsh (show), tven (episode id), tvsn (season), tves (episode), stik (media type), hdvd (HD flag), covr (cover art, JPEG/PNG bytes), ©xyz (location string ISO 6709), ©wrt, ©grp, ©lyr, cprt, rtng, tmpo, trkn/disk, gnre
- ----:com.apple.iTunes:* freeform atoms: iTunEXTC (content rating, e.g. "mpaa|R|..."), iTunMOVI (XML plist: cast, directors, codirectors, producers, screenwriters), iTunSMPB (AAC priming: encoder delay/padding samples), iTunNORM, ISRC, etc. [spec]
- meta (full box) with hdlr 'mdta' + keys + ilst: QuickTime style metadata. keys entries map to keys like com.apple.quicktime.* [spec]
- uuid box XMP: uuid = BE7ACFCB-97A9-42E8-9C71-999491E3AFAC, payload = XMP packet (XML) [spec]
- Nero chpl (inside udta): chapter list, count + (time 100ns units, title). [spec]
- QuickTime chapters: tref/chap track reference pointing to a text track with chapter names and per-sample times. [spec]
- udta.name (QuickTime 'name' box) [spec]
- front_of_file: depends on moov position. linking_value: alt (same inner file, different tag set = different uploader or tool).

## 8. Fragmented MP4 [spec]
- mvex/mehd: fragment_duration. trex: default sample flags. moof/mfhd (sequence_number), traf/tfhd, tfdt, trun. sidx: segment index. ftyp often 'iso5', 'dash', 'msdh'.
- In fragmented files, moov has no sample tables (stts/stsz empty); duration and frame counts come only from moof/trun walking. Readable only if the full sequence is present.

## 9. Faststart / moov placement
- Faststart = moov before mdat. Check: top-level box order. FFmpeg wiki: https://trac.ffmpeg.org/wiki/HowToCheckIfFaststartIsEnabledForPlayback [fetched URL, not read in full]
- Measured frequency for release MP4s: NOT FOUND in this session (search returned only qt-faststart mailing list threads). Do not rely on a percentage. Suggest Atlas measure on its own corpus: count faststart among the first-MB-readable files.
- If moov is at the end: the first ~700 KB article chunks show only ftyp + free/wide + mdat header and a huge mdat size. Readable: ftyp (brands, major brand), and nothing else. Nothing about codecs, resolution, duration, tags, chapters, DV. Only the tail (last article(s)) holds moov, which Atlas would need to fetch explicitly. In a stored RAR, the MP4 bytes are contiguous, so the tail article of the RAR volume set is needed.
- Also: some muxers write moov at front but with mdat first and a 'free' box; check for mdat size 1 (largesize) case.
- If mdat has size 0 (box extends to EOF), the file may be a live/stream write; moov can be missing entirely.

## 10. Rust crates (survey, shallow)
- mp4parse (Mozilla): https://github.com/mozilla/mp4parse-rust [fetched] - MPL-2.0. Track-metadata parser, the Firefox demuxer. Does not list udta/ilst, fragmented MP4, or specific boxes on its page. Not an ilst reader. Good reference for track-level fields. Maintenance: 1,174 commits, 28 open issues, 13 open PRs [fetched].
- mp4 (alfg/mp4-rust): https://github.com/alfg/mp4-rust [fetched] - MIT, 139 commits, 353 stars, 12 open issues, 26 open PRs. Example API covers ftyp, moov/mvhd only. Handbrake-free, read/write. Does not document tkhd/hdlr/stsd/colr/dvcC/ilst/moof support on its page [fetched; could not confirm box list].
- mp4ameta: https://lib.rs/crates/mp4ameta [fetched] - iTunes ilst reader/writer, freeform (----) atoms supported, no chapter support mentioned. lib.rs shows 0.11.0, last update 2021-06-15, about 12k downloads per month. docs.rs search snippet shows 0.13.0 exists, so lib.rs page may be stale. Covers ilst only.
- symphonia-format-isomp4: https://docs.rs/symphonia-format-isomp4 [search only, not fetched] - demuxer for audio/video track tables, not a metadata dump. Check box coverage before using.
- mp4ra-rust: https://docs.rs/mp4ra-rust/0.6.0/src/mp4ra_rust/lib.rs.html [search result] - generated fourcc registry (SampleEntryCode). Useful as a static table, not a parser.
- NOT checked: mp4-atom, re_mp4, isobmff (maintenance state unknown).
- Recommendation under project rule: write a small box reader (32-bit size + 4cc, 64-bit largesize, uuid boxes, recursion into moov/trak/mdia/minf/stbl/udta/meta/ilst). Use mp4ameta or mp4parse only as test oracles or for ilst edge cases. ffprobe exists in this container at /usr/bin/ffprobe; useful only as an oracle in tests, not a dependency.

## 11. Linking value summary
- Muxer fingerprint (strong, cross-post): ftyp brands, hdlr.name, ©too/compressorname, moov/mvhd timescale, tkhd creation_time, udta order, meta key order.
- Same-film (content identity across encodes): duration (mvhd and mdhd in ticks), frame count (stsz count), width/height, DV profile/level/compat id, colr nclx values, audio channel layout (dac3/dec3 acmod), sample rate, chapter count and titles.
- Twin: identical moov bytes (mvhd creation_time same) across posts.

## 12. Surprises
- DV profile and HDR10+ are not container boxes for HDR10+; only the RPU/T.35 in-bitstream payload.
- dvcC/dvvC bit layout is 7/6/1/1/1 across 16 bits, not byte aligned; compat id sits in the next byte.
- mp4parse-rust (Mozilla) does not read ilst; the common crate for ilst (mp4ameta) is 2021 on lib.rs. Expect to write your own udta/ilst reader.
- Moov-at-end makes the first 700 KB article chunks nearly useless beyond ftyp. Atlas needs a tail fetch to read most metadata.
- mvhd creation_time is often 0 or a placeholder; tkhd creation_time may differ from mvhd.
- Track-level duration can differ from movie duration (elst empty edits shift the start).
- mp4ra registry is the authority for sample entry fourccs (mp4ra.org).

## Sources
- https://lists.mplayerhq.hu/pipermail/ffmpeg-cvslog/2020-April/121746.html (dvcC/dvvC layout)
- https://lists.mplayerhq.hu/pipermail/ffmpeg-cvslog/2021-October/129625.html (dvwC)
- https://mp4ra.org/registered-types/codecs
- https://docs.rs/mp4ra-rust/0.6.0/src/mp4ra_rust/generated.rs.html
- https://github.com/mozilla/mp4parse-rust
- https://github.com/alfg/mp4-rust
- https://lib.rs/crates/mp4ameta
- https://docs.rs/symphonia-format-isomp4
- https://trac.ffmpeg.org/wiki/HowToCheckIfFaststartIsEnabledForPlayback
- https://cobalt.googlesource.com/cobalt/+/HEAD/media/formats/mp4/dolby_vision.h (listed, not read)
