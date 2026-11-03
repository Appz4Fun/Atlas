# w01-c-audio: audio stream metadata, Rust-only. UNVERIFIED = not read from primary source.

- field: AC-3 BSI (fscod, frmsizecod, bsid, bsmod, acmod, lfeon, dialnorm, compr)
  where: syncframe after 0x0B77 + CRC16. fscod 2b, frmsizecod 6b (bitrate index = /2), bsid 5b, bsmod 3b, acmod 3b, lfeon 1b, dialnorm 5b, compr flag+8b, audprodie (mixlevel 5b, roomtyp 2b)
  parse: bdinfo-rs-core ac3.rs
  use: channel layout (acmod+lfeon), bitrate, rate, loudness, bsid 6 = extension
  source: https://docs.rs/crate/bdinfo-rs-core/latest/source/src/codec/ac3.rs

- field: E-AC-3 header (strmtyp, substreamid, frmsiz, fscod/fscod2, numblkscod, acmod, lfeon, bsid 11-16, chanmap)
  where: strmtyp 2b (0 indep, 1 dependent), substreamid 3b, frmsiz 11b (frame bytes = 2*(frmsiz+1)), chanmap 16b when strmtyp=1
  parse: bdinfo-rs-core ac3.rs; MediaArea File_Ac3.cpp
  use: DD+ vs AC-3; 7.1 = indep 5.1 + dependent substream
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Audio/File_Ac3.cpp

- field: DD+ Atmos (JOC)
  where: EMDF sync 0x5838 bit scan; EMDF version 0; payload ID 14 = JOC; joc_num_objects_bits 6b
  parse: bdinfo-rs-core ac3.rs. MediaInfo accepts JOC if the count is present in >= 1/8 of frames
  use: "DD+ with Atmos" label, object count
  source: https://docs.rs/crate/bdinfo-rs-core/latest/source/src/codec/ac3.rs

- field: MP4 dac3 / dec3 boxes; flag_ec3_extension_type_a, complexity_index_type_a
  where: sample entry child boxes. dec3: data_rate 13b, num_ind_sub 3b, per-substream fscod/bsid/bsmod/acmod/lfeon/num_dep_sub/chan_loc. Atmos flags after the substream loop (UNVERIFIED bit layout)
  parse: direct bit read; no crate found
  use: Atmos flag from MP4 header without decoding frames
  source: UNVERIFIED (ETSI TS 102 366 Annex F, Dolby ISOBMFF doc not fetched)

- field: TrueHD major sync (format_sync 0xF8726FBA)
  where: first access unit. ratebits 4b (44.1k if bit 8 else 48k, shifted by low 3 bits; 0xF = unset), channel assignment 13 flags (+2 LFE), peak bitrate 15b, num_substreams 4b
  parse: bdinfo-rs-core src/codec/truehd.rs
  use: layout, rate, peak bitrate, bit depth (24 if peak/ch/rate > 14)
  source: https://docs.rs/crate/bdinfo-rs-core/latest/source/src/codec/truehd.rs

- field: TrueHD Atmos flag (has_extensions)
  where: after major sync: has_extensions 1b, then nonzero 4b content or 8b extension bytes (bdinfo heuristic)
  parse: bdinfo truehd.rs; ffmpeg mlp_parser.c
  use: "TrueHD with Atmos"
  source: https://docs.rs/crate/bdinfo-rs-core/latest/source/src/codec/truehd.rs ; bit widths UNVERIFIED vs spec

- field: DTS core header (sync, AMODE, SFREQ, RATE, LFF, PCMR, NBLKS)
  where: sync 0x7FFE8001 BE, 0xFE7F0180 LE, 0x1FFFE800 / 0xFF1F00E8 (14-bit). AMODE = channel layout index, LFF = LFE, SFREQ = rate index, RATE 29 = open, PCMR bit depth index {16,20,24,24}, samples = NBLKS*32
  parse: File_Dts.cpp; bit widths UNVERIFIED (ETSI TS 102 114)
  use: channels, rate, bitrate, core bit depth
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Audio/File_Dts.cpp

- field: DTS-HD ExSS sync and profile
  where: 0x64582025 starts ExSS. Sub-sync: Core 0x02B09261, LBR 0x0A801921, X96 0x1D95F262, XLL 0x41A29547, XXCH 0x47004A03, XCh 0x5A5A5A5A, XBR 0x655E315E, Aux 0x9A1105A0. Asset descriptor gives channels, speaker mask, bit depth, max rate
  parse: File_Dts.cpp. XLL = MA, LBR = Express
  use: MA vs HRA vs core tier
  source: https://raw.githubusercontent.com/MediaArea/MediaInfoLib/master/Source/MediaInfo/Audio/File_Dts.cpp

- field: DTS:X / IMAX Enhanced
  where: probe for 0x02000850 and 0xF14000D0-D4 (4-ch set, CRC checked); 0xF14000D3 + type-241 header gives object count (4b + 1)
  parse: File_Dts.cpp probe. IMAX source not shown in excerpt
  use: DTS:X / IMAX label, object count
  source: File_Dts.cpp (URL above). Literal "XLL X" label NOT found (UNVERIFIED how mediainfo names it)

- field: AAC AudioSpecificConfig; esds bitrates
  where: MP4 esds DecoderSpecificInfo or MKV CodecPrivate. audioObjectType 5b (2 LC, 5 SBR, 29 PS, 31 escape +6b), samplingFrequencyIndex 4b (0xF = explicit 24b), channelConfiguration 4b. esds carries maxBitrate, avgBitrate
  parse: manual (ISO/IEC 14496-3, not fetched, UNVERIFIED). symphonia AAC-LC only
  use: HE-AAC vs LC, PS = HEv2, esds bitrates
  source: UNVERIFIED (ISO/IEC 14496-3)

- field: FLAC STREAMINFO (first metadata block, 34 bytes)
  where: min/max block 16b each, min/max frame 24b each, sample rate 20b, channels-1 3b, bps-1 5b, total samples 36b, MD5 128b (0 = unknown)
  parse: manual
  use: rate, channels, depth, duration. MD5 covers unencoded interleaved signed LE PCM, so it is a rename-proof audio key for FLAC only
  source: https://www.rfc-editor.org/rfc/rfc9639 (section 8.2)

- field: Opus OpusHead (RFC 7845 5.1)
  where: first packet / MKV A_OPUS CodecPrivate. magic 0-7, version 8, channels 9, pre-skip 10-11 LE, input rate 12-15 LE (metadata only), output gain 16-17, mapping family 18 (0 mono/stereo, 1 surround, 255 discrete), stream count 19, coupled 20, mapping 21+
  parse: manual
  use: channels, pre-skip, surround layout
  source: https://datatracker.ietf.org/doc/html/rfc7845

- field: PCM (MKV A_PCM/INT/LIT, A_PCM/INT/BIG, A_PCM/FLOAT/IEEE)
  where: CodecID string; BitDepth 0x6264; Channels 0x9F; SamplingFrequency 0xB5
  parse: container fields
  use: depth, endianness, float vs int
  source: https://www.matroska.org/technical/elements.html ; codec ID strings UNVERIFIED

- field: MKV audio/track fields
  where: Audio 0xE1: SamplingFrequency 0xB5, OutputSamplingFrequency 0x78B5 (SBR), Channels 0x9F, BitDepth 0x6264, ChannelPositions 0x7D7B. TrackEntry: Language 0x22B59C, Name 0x536E, CodecID 0x86, CodecPrivate 0x63A2, CodecDelay 0x56AA, FlagDefault 0x88, FlagForced 0x55AA, FlagHearingImpaired 0x55AB, FlagVisualImpaired 0x55AC, FlagTextDescriptions 0x55AD, FlagOriginal 0x55AE, FlagCommentary 0x55AF
  use: Commentary/AD/SDH flags, language, name, SBR
  source: https://www.matroska.org/technical/elements.html

# Crates
- symphonia: AAC-LC (Great, feature aac), ALAC, FLAC (Excellent), MP1-3, PCM, Vorbis, ADPCM, WavPack, Opus (no status). HE-AAC no status. Demux: MP4 Great, MKV Good. NO AC-3, E-AC-3, DTS, TrueHD. https://raw.githubusercontent.com/pdeljanov/Symphonia/master/README.md
- bdinfo-rs-core 2.0.0: ac3.rs, truehd.rs, DTS. License UNVERIFIED. https://docs.rs/crate/bdinfo-rs-core/latest
- libfreemkv 0.31.x: mux/codec ac3.rs, dts.rs (license UNVERIFIED).

# Leads for next wave
- ETSI TS 102 366 (full AC-3/E-AC-3, Annex F dec3/dac3); ETSI TS 103 420 (JOC complexity_index).
- ffmpeg libavcodec mlp_parser.c, mlp_parse.h; dca_core.c, dca_xll.c, dca_exss.c (DTS:X).
- crates.io license checks.

# Dead ends
- Symphonia has no AC-3, E-AC-3, DTS, TrueHD.
- xiph.org FLAC page lacks STREAMINFO table; use RFC 9639.
- File_Mlp.cpp unreachable (client_error); File_Dts.cpp excerpt lacks core bit widths and "XLL X" label.
- File_Ac3.cpp excerpt lacks flag_ec3_extension_type_a
