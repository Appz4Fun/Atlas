# w5c: ISOBMFF derived stats, audio/video config box layouts, iTunes freeform atoms

Tags: [V-FF] verified in FFmpeg libavformat/mov.c (master, fetched this session); [V-MUT] verified in mutagen (quodlibet/mutagen main, mutagen/mp4/__init__.py or _as_entry.py); [M] memory, not verified against source this session; [X] FFmpeg/mutagen disagree with spec, check.

## 1. Derived stats from moov (all computable from moov alone; mdat only needed for payload bytes/bitrate per GOP)

- Track duration: mdhd. v0: timescale u32 @ offset 12 (after ver/flags 4, created 4, modified 4), duration u32 @16. v1: created/modified u64, timescale u32, duration u64. [V-FF mov_read_mdhd; V-MUT load() uses ">2I" at offset 8 for v0, ">IQ" at 16 for v1]. Seconds = mdhd.duration / mdhd.timescale. All-ones duration = unknown (FFmpeg sets 0) [V-FF].
- Movie duration: mvhd (v0 timescale u32 @12, duration u32 @16; v1 timescale @24, duration u64 @28 from payload start incl ver/flags) [V-MUT _parse_mvhd, v0 data[8:16], v1 data[16:28] after ver/flags strip]. Movie timescale != media timescale: convert with x / mvhd.timescale.
- Edit list (elst): v0 entries 12 bytes (segment_duration u32 in movie timescale, media_time i32, media_rate u16.u16); v1 entries 20 bytes (u64 duration, i64 media_time). media_time = -1 means empty edit (leading delay). rate = u32/65536 [V-FF mov_read_elst]. Priming / start offset = first non-empty edit's media_time (media timescale units) when it is >0 (encoder delay). Presentation start = sum of empty-edit durations. Also see iTunSMPB below for AAC priming [M].
- Frame count: stsz.sample_count (entries) [V-FF mov_read_stsz: sc->sample_count = entries]; equals sum of stts sample_count across entries [V-FF stts loop]. stz2 with field_size 4/8/16 compact table [V-FF: field_size in {4,8,16,32}].
- Per-track average bitrate = sum(stsz sizes) * 8 / (mdhd.duration / mdhd.timescale). Use the edit-list-trimmed duration if you want the playable bitrate. btrt box (in stsd sample entry): bufferSizeDB u32, maxBitrate u32, avgBitrate u32 [M]. Prefer btrt/esds values when present, computed as fallback [V-MUT: esds avgBitrate used as bitrate].
- FPS / CFR-VFR: stts entries (sample_count, sample_delta) [V-FF mov_read_stts reads version/flags/entries; each entry is count u32, delta u32]. CFR iff stts has one entry (or all deltas equal) -> fps = timescale / delta. If multiple distinct deltas, VFR (or CFR with dropped frames). Average fps = frame_count * timescale / total_duration_ticks.
- Keyframes / GOP: stss present -> list of sync sample numbers (1-based) [M]. Absent -> all samples are sync. Keyframe interval = diff between consecutive stss sample numbers, convert to time with the stts cumulative DTS. If stss absent and video is AVC/HEVC, all-I (rare).
- B-frames / reorder: ctts entries (sample_count, sample_offset). v0 offsets unsigned, v1 offsets signed (FFmpeg reads entries via avio_rb32; sign handling [M, need to confirm in the ctts loop]). B-frame presence = any negative offset after normalizing, or ctts present and pts order != dts order. Reorder depth = max over i of (number of samples with pts > pts_i that precede i in decode order). cslg (composition to decode shift box, version 0/1: compositionToDTSShift, leastDecodeToDisplayDelta, greatestDecodeToDisplayDelta, compositionStartTime, compositionEndTime) gives reorder bounds directly. FFmpeg does not parse cslg (grep finds none in mov.c) [V-FF].
- Sample-to-chunk interleave: stsc entries (first_chunk u32 1-based, samples_per_chunk u32, sample_description_index u32); chunk offsets from stco (u32) or co64 (u64). Byte offset of sample n = chunk_offset[c] + sum of preceding sample sizes in that chunk [M]. Interleave check: sort all tracks' chunk offsets; if chunks of different tracks alternate, the file is interleaved (per-chunk duration ~ 0.5 to 2 s typical). All computable from moov.
- Bitrate per GOP / peak: sum sizes over a GOP range using stss and stsz [M].

## 2. Fragmented MP4 (no moov sample tables)

- Duration: mvhd duration may be 0; use mehd.fragment_duration (movie timescale; v0 u32, v1 u64) if present [M]. Otherwise sum sidx subsegment durations.
- sidx: version u8, flags u24, reference_ID u32, timescale u32, earliest_presentation_time (v0 u32, v1 u64), first_offset (v0 u32, v1 u64), reserved u16, reference_count u16; each reference: reference_type(1)+referenced_size(31) u32, subsegment_duration u32, starts_with_SAP(1)+SAP_type(3)+SAP_delta(28) u32 [M]. Gives per-segment duration in sidx timescale and byte size; no frame count. FFmpeg mov_read_sidx [V-FF exists at line 6562].
- tfra in mfra: time and moof_offset per random-access point, not frame counts [M]. FFmpeg read_tfra at line 11006 [V-FF exists].
- trex defaults: default_sample_description_index, default_sample_duration, default_sample_size, default_sample_flags [M]; FFmpeg mov_read_trex [V-FF at line 6155].
- Frame count: only by walking moofs: each traf/trun has sample_count (trun), and tfhd can override default_sample_duration/size/flags [M]. Read box headers and skip mdat by size; no payload needed. sidx cannot give frame counts. Sample durations via trun sample_duration fields or trex default.
- No mdat walk needed for duration in most fragmented files: sidx or mehd.

## 3. Audio/video config box layouts

### dac3 (AC-3, ETSI TS 102 366 Annex F.4; payload 3 bytes, bitfield MSB first)
Layout (24 bits): fscod(2) bsid(5) bsmod(3) acmod(3) lfeon(1) bit_rate_code(5) reserved(5).
- Verified: [V-MUT _parse_dac3: skip 2+5+3, acmod=bits(3), lfeon=bits(1), bit_rate_code=bits(5), skip 5]. [V-FF mov_read_dac3: ac3info=rb24; bsmod=(>>14)&7; acmod=(>>11)&7; lfeon=(>>10)&1; channel mask from ff_ac3_channel_layout_tab[acmod] | LFE].
- Channels = [2,1,2,3,3,4,4,5][acmod] + lfeon [V-MUT].
- bit_rate_code -> kbps: 0:32 1:40 2:48 3:56 4:64 5:80 6:96 7:112 8:128 9:160 10:192 11:224 12:256 13:320 14:384 15:448 16:512 17:576 18:640 [V-MUT table].
- fscod: 0=48k 1=44.1k 2=32k 3=reserved; sample rate in AudioSampleEntry covers fscod2 (ignore fscod in dac3 per mutagen comment) [V-MUT].
- bsmod 7 with >1 channel = karaoke [V-FF].

### dec3 (E-AC-3, TS 102 366 Annex F.6; full EC3SpecificBox payload)
Header (16 bits): data_rate(13) num_ind_sub(3). data_rate in kbps, num_ind_sub = number of independent substreams minus 1.
Per independent substream i (24 bits for each): fscod(2) bsid(5) reserved(1) asvc(1) bsmod(3) acmod(3) lfeon(1) reserved(3) num_dep_sub(4) then chan_loc(9) if num_dep_sub>0 else reserved(1).
- Verified byte offsets: [V-FF mov_read_dec3: keeps the whole payload as extradata (ff_get_extradata), then eac3info = AV_RB24(extradata+2) i.e. the first substream's 24 bits; bsmod=(>>12)&0x1f; acmod=(>>9)&7; lfeon=(>>8)&1]. 
- [X] FFmpeg masks bsmod with 0x1f (5 bits, bits 16..12), which would include reserved+asvc. Per my memory of the spec bsmod is 3 bits at 14..12 and asvc is bit 15. Verify against the spec before relying on it.
- Channels: mask from acmod and lfeon as in dac3 [V-FF].
- JOC/Atmos extension [M]: after the independent substreams (and dependent-substream chan_loc, padded to a byte), an optional extension: flag_ec3_extension_type_a(1) and complexity_index_type_a(8) when present. Presence = payload size exceeds the base length. Dolby Atmos (JOC) streams signal this extension; FFmpeg keeps the raw bytes so you must parse them yourself.
- data_rate kbps is the stream bitrate [M].

### dac4 (AC-4, ETSI TS 103 190-2 Annex) [M]
Not parsed by FFmpeg mov.c (grep for dac4 found nothing) [V-FF]. Layout in memory: ac4_dsi_version(3) bitstream_version(7) fs_index(1) frame_rate_index(4) n_presentations(9) ... the rest is the presentation/substream table. Parse per spec; no verified bit table in this session.

### ddts (DTS, DTS-HD in ISOBMFF; 20 bytes)
Verified by [V-FF mov_read_ddts, DDTS_SIZE 20, bit by bit]:
- DTSSamplingFrequency u32 (offset 0)
- maxBitrate u32 (offset 4, skipped by FFmpeg)
- avgBitrate u32 (offset 8)
- pcmSampleDepth u8 (offset 12)
- FrameDuration 2 bits: 0=512, 1=1024, 2=2048, 3=4096 samples (FFmpeg frame_size)
- StreamConstruction 5 bits (Table E-2 profile source; FFmpeg maps to profile)
- CoreLFEPresent 1
- CoreLayout 6
- CoreSize 14
- StereoDownmix 1
- RepresentationType 3 (values 2 or 3 -> FFmpeg defaults to stereo layout when channel mask missing)
- ChannelLayout 16 (FFmpeg mov_dts_channel_layout_to_mask)
- reserved 8 (to make 20 bytes) [M for the reserved byte].
DTS-HD MA/X detection: StreamConstruction values distinguish core-only, core+extension, and HD MA/HRA profiles (Table E-2 in spec) [M for table values]. FFmpeg publishes it as codecpar->profile via mov_dts_stream_construction_to_profile.

### dmlp (Dolby TrueHD / MLP, 10+ bytes)
Verified [V-FF mov_read_dmlp]:
- format_info u32 at offset 0.
- ratebits = (fi >> 28) & 0xF -> sample rate via mlp_samplerate(ratebits); frame_size = 40 << (ratebits & 7).
- channel_assignment1 = (fi >> 15) & 0x1F; channel_assignment2 = fi & 0x1FFF; use CA2 if nonzero else CA1 -> truehd_layout(...).
- Peak data rate: 15 bits + 1 reserved after format_info [M, not parsed by FFmpeg]. Bitrate = peak_data_rate kbps.
- Atmos over TrueHD is not signaled here [M].

### dOps (Opus-in-ISOBMFF; payload is big-endian OpusSpecificBox)
Verified layout [V-FF mov_read_dops; reconstructs OpusHead by prepending 'OpusHead' and version 1]:
- Version u8 = 0 (must be 0)
- OutputChannelCount u8 (offset 1)
- PreSkip u16 (offset 2; FFmpeg sets initial_padding = pre_skip)
- InputSampleRate u32 (offset 4)
- OutputGain i16 (offset 8)
- ChannelMappingFamily u8 (offset 10)
- If family != 0: StreamCount u8, CoupledCount u8, ChannelMapping[OutputChannelCount] u8 each.
Seek preroll = 80 ms [V-FF].

### dfLa (FLAC-in-ISOBMFF)
Verified [V-FF mov_read_dfla]: payload = version u8 (must be 0), flags u24, then one FLAC metadata block header (4 bytes: last-flag+type(7)+length u24), type must be 0 (STREAMINFO) and length 34.
STREAMINFO (34 bytes, 272 bits) [M, FLAC spec, consistent with FFmpeg's size check]:
min_blocksize u16 | max_blocksize u16 | min_framesize u24 | max_framesize u24 | sample_rate u20 | channels-1 u3 | bps-1 u5 | total_samples u36 | MD5 u128.
- Total samples in u36 gives exact frame count (sample count) without walking data. MD5 = the 16-byte signature of decoded PCM (use it to compare audio content). Both are inside STREAMINFO, which FFmpeg copies verbatim to extradata.

### esds (MPEG-4 AAC / MP3 etc.)
Verified [V-MUT _parse_esds + ES_Descriptor + DecoderSpecificInfo]:
- version u8 (must be 0) + flags u24.
- ES_Descriptor tag 0x03 (length-prefixed descriptor; each descriptor: tag u8, size with 7-bit vlq continuation bytes).
- DecoderConfigDescriptor tag 0x04: objectTypeIndication u8 (0x40 = MPEG-4 AAC, 0x66/0x67/0x68 MPEG-2 AAC, 0x6B MP3), streamType u6 (0x05 = audio), upStream u1, reserved u1, bufferSizeDB u24, maxBitrate u32, avgBitrate u32. Optional DecoderSpecificInfo tag 0x05 follows.
- AudioSpecificConfig: audioObjectType u5 (31 = escape, then u6 + 32); samplingFrequencyIndex u4 (0xF = escape, then u24 explicit Hz); channelConfiguration u4.
  - AOT 5 or 29 = SBR/PS: extensionAudioObjectType=5, sbrPresentFlag=1, extensionSamplingFrequency index u4 (+escape), then AOT again. AOT 29 = PS (psPresentFlag=1).
  - Stereo handling: channelConfiguration 1 with PS -> 2 channels. conf 7 -> 8 channels.
  - Sync extension 0x2b7 (11 bits) signals SBR in non-HE-AAC-tagged streams; 0x548 signals PS [V-MUT _parse].
- AAC AOT 2 = LC, 5 = HE-AAC v1 (SBR), 29 = HE-AAC v2 (SBR+PS).
- Sample rate: if SBR present, the output rate is extensionSamplingFrequency; else samplingFrequency. [V-MUT sample_rate property].
- Bitrate: esds avgBitrate (u32 in DecoderConfigDescriptor) [V-MUT].

### dOps, dfLa, alac, chnl, pasp, colr
- alac magic cookie (ALACSpecificConfig, 24 bytes) [M]: frameLength u32, compatibleVersion u8, bitDepth u8, pb u8, mb u8, kb u8, numChannels u8, maxRun u16, maxFrameBytes u32, avgBitRate u32, sampleRate u32. FFmpeg mov_read_alac at line 2397 [V-FF exists].
- chnl (ISO 14496-12 channel layout) [V-FF ff_mov_read_chnl in mov_chan.c]: stream_structure u8 (version 1 splits high nibble as stream_structure and next byte is base_channel_count u8). If stream_structure & 1: layout u8; if layout == 0: custom: channel count (v1 extra byte), then one speaker_position u8 per channel; position 126 = explicit with 3 extra bytes (azimuth, elevation); mapped through iso_channel_position table. Layout != 0 = a predefined ISO layout id. Note the mapping table is in mov_chan.c.
- colr/pasp/fiel parsers exist in mov.c at lines 2211, 1483, 2300 [V-FF exists, not read in detail].

### Dolby Vision (dvcC / dvvC / dvwC)
- FFmpeg dispatches to ff_isom_parse_dvcc_dvvc (mov.c line 9134) [V-FF], but the function is not in the isom.c I downloaded (it lives elsewhere in libavcodec/libavformat). Not read.
- Layout from memory [M]: dv_version_major u8, dv_version_minor u8, dv_profile u7, dv_level u6, rpu_present_flag u1, el_present_flag u1, bl_present_flag u1, dv_bl_signal_compatibility_id u4, reserved u28 (total 24 bytes). Profile 5 = IPT-PQ no HDR10 base; 8.1 HDR10 base; 8.4 HLG base. Verify before relying on the field positions.

## 4. iTunes freeform and data atoms

Verified [V-MUT, mutagen/mp4/__init__.py]:
- Freeform key layout: ----, then 'mean' atom (len u32, 'mean', version/flags u32, then the namespace string, e.g. com.apple.iTunes), then 'name' atom (len, 'name', version/flags, the name, e.g. iTunEXTC), then one or more 'data' atoms. Key string is "----:mean:name" (__parse_freeform, __render_freeform).
- Each data atom: length u32, 'data', version u8 + flags u24 (flags = data type code), reserved u32, payload. (__parse_data: flags is 24-bit type code, payload at pos+16.)
- Data type codes [V-MUT AtomDataType]: 0 implicit, 1 UTF-8, 2 UTF-16BE, 3 SJIS, 6 HTML, 7 XML, 8 UUID/GUID, 9 ISRC, 10 MI3P, 12 GIF, 13 JPEG, 14 PNG, 15 URL, 16 DURATION (u32 ms), 17 DATETIME (u32/u64 seconds since 1904 UTC), 18 GENRES, 21 INTEGER (signed big-endian, 1/2/3/4/8 bytes), 24 RIAA_PA (-1/0/1, u8), 25 UPC (UTF-8), 27 BMP.
- Freeform values: MP4FreeForm.dataformat = flags, so UTF-8 text is type 1; mutagen defaults to UTF-8 when rendering [V-MUT __render_freeform].
- Covers: JPEG 13, PNG 14 [V-MUT MP4Cover].

[M] iTunes freeform atoms (from knowledge of AtomicParsley/ffmpeg conventions, not read in this session):
- ----:com.apple.iTunes:iTunEXTC  value text 'mpaa|R|400|' (system|rating|points|) -> content advisory.
- ----:com.apple.iTunes:iTunMOVI  value is an XML plist (cast, directors, producers, screenwriters, studio, copy-warning, ...). Parse as plist; key 'cast' array of {name: ...}.
- ----:com.apple.iTunes:xid   'xid ' atom (text) gives the content ID (store). Not verified.
- cnID, sfID, geID, plID, atID, cmID, akID are four-char tags whose int value is the Apple ID [V-MUT lists them in MP4Tags docstring, line ~315].
- Chapters: chpl (Nero): version u8, flags u24, reserved u8, count u8, then entries: start u64 in 100 ns units (mutagen divides by 10,000,000), title length u8, title UTF-8 [V-MUT _parse_chpl]. Chapter time = start / 1e7 seconds.

## 5. Things I did not verify
- ff_isom_parse_dvcc_dvvc source (DV RPU/profile parse).
- dac4 table (TS 103 190-2).
- dec3 bsmod bit width vs FFmpeg mask (possible FFmpeg mismatch).
- ctts v0/v1 signedness handling in mov_read_ctts loop.
- alac, colr, pasp, esds in FFmpeg (only function locations).
- iTunSMPB field layout (AAC priming 2112 typical for Apple encoder) [M].

Sources fetched this session: FFmpeg master libavformat/mov.c (13311 lines), movenc.c, mov_chan.c, isom.c, isom.h; mutagen main mutagen/mp4/__init__.py and _as_entry.py. Local copies under /tmp/w5c_src.
