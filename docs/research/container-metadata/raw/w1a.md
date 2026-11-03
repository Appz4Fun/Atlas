# w1a: Matroska / WebM metadata catalogue (Atlas, Rust-native)

Scope: every metadata field reachable from Matroska/WebM (EBML) files, with
front-of-file placement and linking value for Atlas's partial-download model
(first 1-4 MB of an article set, often inside a stored RAR).

Confidence key: [spec] = fetched from RFC 9559 / matroska.org text this session.
[mem] = from memory of the spec or mkvmerge behaviour, NOT re-verified this session.
Verify [mem] items against https://github.com/ietf-wg-cellar/matroska-specification
(ebml_matroska.xml) before coding against them.

Sources:
- RFC 9559 (Matroska): https://www.rfc-editor.org/rfc/rfc9559.txt
- RFC 8794 (EBML): https://www.rfc-editor.org/rfc/rfc8794.txt
- Matroska tagging spec: https://www.matroska.org/technical/tagging.html
- Spec XML: https://github.com/ietf-wg-cellar/matroska-specification
- mkvtoolnix docs: https://mkvtoolnix.download/doc/mkvmerge.html

## 0. EBML primitives

- Element ID: VINT with length marker kept (1-4 bytes). [spec, RFC 8794]
- Element size: VINT with marker removed (1-8 bytes). All-ones value = UNKNOWN SIZE
  (live/streamed writes; common in ffmpeg/libwebm live mode and some remuxers). [RFC 8794]
- Master elements contain children; leaf elements are uint, int, float, string,
  utf-8, date, binary. [RFC 8794]
- Void (0xEC) and CRC-32 (0xBF) are padding / integrity elements. Void is how
  mkvmerge reserves space it later overwrites. [mem]

## 1. EBML header (top level, ID 0x1A45DFA3)

- EBMLVersion 0x4286, uint, default 1, [mem]
- EBMLReadVersion 0x42F7, default 1 [mem]
- EBMLMaxIDLength 0x42F2, default 4 [mem]
- EBMLMaxSizeLength 0x42F3, default 8 [mem]
- DocType 0x4282, string: "matroska" or "webm". Only value that tells WebM from MKV. [mem]
- DocTypeVersion 0x4287, uint: 4 for current Matroska (v4 adds CodecDelay, SeekPreRoll,
  BlockAdditionMapping, Colour, Projection). [spec for minver annotations in RFC 9559]
- DocTypeReadVersion 0x4285. [mem]
- DocTypeExtension 0x4281 (+ Name 0x4283, Version 0x4284). [mem]

Front: yes. Tiny. Linking: none (identical across all files of same DocType/version).

## 2. Segment (0x18538067) and top-level children

Top-level IDs. [spec for Segment, SeekHead, Info, Tracks, Cluster; mem for rest]
- SeekHead 0x114D9B74 (max 2, [spec])
- Info 0x1549A966 (1/1, [spec])
- Tracks 0x1654AE6B (max 1, [spec])
- Cluster 0x1F43B675 ([spec])
- Cues 0x1C53BB6B [mem]
- Chapters 0x1043A770 [mem]
- Tags 0x1254C367 [mem]
- Attachments 0x1941A469 [mem]

### 2a. SeekHead (0x114D9B74)
- Seek 0x4DBB (master, min 1) [spec]
  - SeekID 0x53AB, binary(4), the ID of the target element. [spec]
  - SeekPosition 0x53AC, uint, offset from Segment data start. [spec]
- Use: tells you where Cues, Tags, Chapters, Attachments live without a full scan.
  Front: yes (usually). Linking: alt (positions shift if anything differs).

### 2b. Info (0x1549A966), Segment Info [spec field IDs]
| path | type | meaning | example | front | linking |
|---|---|---|---|---|---|
| Info/SegmentUUID 0x73A4 | binary16 | unique segment id | 3f2a... | yes | none (regenerated per mux) |
| Info/SegmentFilename 0x7384 | utf8 | original filename | "Movie.mkv" | yes | alt |
| Info/PrevUUID 0x3CB923 / PrevFilename 0x3C83AB | binary16 / utf8 | hard-link chain predecessor | | yes | none |
| Info/NextUUID 0x3EB923 / NextFilename 0x3E83BB | binary16 / utf8 | hard-link chain successor | | yes | none |
| Info/SegmentFamily 0x4444 | binary16 | groups linked segments | | yes | none |
| Info/ChapterTranslate 0x6924 (ChapterTranslateID 0x69A5, Codec 0x69BF, EditionUID 0x69FC) | master | maps DVD/Blu-ray chapter codecs | | yes | alt |
| Info/TimestampScale 0x2AD7B1 | uint | ns per tick, default 1000000 [spec] | 1000000 | yes | same-film |
| Info/Duration 0x4489 | float | duration in TimestampScale ticks; duration_s = Duration * TimestampScale / 1e9 | 5432.117 | yes (rewritten in place at finalise) | alt |
| Info/DateUTC 0x4461 | date | mux time, ns since 2001-01-01 | | yes | none |
| Info/Title 0x7BA9 | utf8 | title tag | "Movie Title (2019)" | yes | alt (usually survives remux) |
| Info/MuxingApp 0x4D80 | utf8 (required) | library that muxed | "libebml v1.4.2 + libmatroska v1.6.4" | yes | same-film |
| Info/WritingApp 0x5741 | utf8 (required) | tool/command that wrote it | "mkvmerge v81.0 ('...') 64bit" | yes | same-film |

Muxer strings (tool + version + build date hints) [mem, verify]:
- mkvmerge: WritingApp "mkvmerge vX.Y.Z ('Codename') 64bit" (optionally "built on <date>"). MuxingApp "libebml v1.x + libmatroska v1.x". Version and codename reveal the mkvtoolnix release.
- ffmpeg/libavformat: MuxingApp and WritingApp "Lavf<major.minor.micro>" (e.g. Lavf58.45.100). Lavf version maps to ffmpeg release.
- HandBrake: usually "HandBrake x.y.z" in WritingApp, libmatroska in MuxingApp. [mem]
- MakeMKV: writes MuxingApp/WritingApp "makemkv" with version/platform string. [mem]
- Live/streaming writers (libwebm) may use UNKNOWN-size Segment/Cluster and omit Duration.

Front: yes for all Info fields. Linking for all Info: same-film (muxer strings identify the pipeline, which is an encode-level fingerprint, not a file id).

### 2c. Tracks (0x1654AE6B) and TrackEntry (0xAE) [spec]
Field IDs and mandatory flags from RFC 9559 (fetched):

| path | ID | type | notes | linking |
|---|---|---|---|---|
| TrackNumber | 0xD7 | uint | required, not 0 | alt |
| TrackUID | 0x73C5 | uint | random per mux; referenced by Tags TagTrackUID | none |
| TrackType | 0x83 | uint | 1 video, 2 audio, 3 complex, 0x10 logo, 0x11 subtitle, 0x12 buttons, 0x20 control | same-film |
| FlagEnabled | 0xB9 | uint | default 1 | alt |
| FlagDefault | 0x88 | uint | default 1 | alt |
| FlagForced | 0x55AA | uint | default 0 | alt |
| FlagHearingImpaired | 0x55AB | uint | SDH subs | alt |
| FlagVisualImpaired | 0x55AC | uint | audio description | alt |
| FlagTextDescriptions | 0x55AD | uint | | alt |
| FlagOriginal | 0x55AE | uint | original-language track | alt |
| FlagCommentary | 0x55AF | uint | commentary track | alt |
| FlagLacing | 0x9C | uint | default 1 | alt |
| DefaultDuration | 0x23E383 | uint, ns per frame/block | fps = 1e9 / DefaultDuration | same-film |
| DefaultDecodedFieldDuration | 0x234E7A | uint | | same-film |
| TrackTimestampScale | 0x23314F | float | deprecated (maxver 3), default 1.0 | none |
| MaxBlockAdditionID | 0x55EE | uint | | same-film |
| BlockAdditionMapping | 0x41E4 | master | see 2f | same-film |
| Name | 0x536E | utf8 | "Director's commentary", "English 5.1" | alt |
| Language | 0x22B59C | string | ISO 639-2, default "eng" if absent | alt |
| LanguageBCP47 | 0x22B59D | string | e.g. "en-US"; overrides Language | alt |
| CodecID | 0x86 | string | V_MPEG4/ISO/AVC, V_MPEGH/ISO/HEVC, V_AV1, V_VP9, A_AC3, A_EAC3, A_DTS, A_DTS/HD MA, A_TRUEHD, A_OPUS, A_FLAC, S_TEXT/UTF8, S_TEXT/ASS, S_HDMV/PGS, S_VOBSUB | same-film |
| CodecPrivate | 0x63A2 | binary | codec init data: hvcC (HEVC SPS/PPS/VPS + profile/level), avcC, av1C, vorbis/opus headers, FLAC STREAMINFO, ASS header for S_TEXT/ASS | same-film |
| CodecName | 0x258688 | utf8 | human codec name | alt |
| AttachmentLink | 0x7446 | uint | maxver 3 | none |
| CodecDelay | 0x56AA | uint ns, default 0 | Opus pre-skip | same-film |
| SeekPreRoll | 0x56BB | uint ns | Opus/Vorbis | same-film |
| TrackTranslate | 0x6624 (TrackTranslateTrackID 0x66A5, Codec 0x66BF, EditionUID 0x66FC) | master | | alt |
| ContentEncodings | 0x6D80 > ContentEncoding 0x6240 (Order 0x5031, Scope 0x5032, Type 0x5033, Compression 0x5034 (Algo 0x4254, Settings 0x4255), Encryption 0x5035) | master | header-stripping compression is common in MKV ripping | same-film |
| TrackOperation 0xE2 (CombinePlanes 0xE3, JoinBlocks 0xE9) | master | 3D / split tracks | | same-film |

Use: codec, resolution, fps, language, flags and CodecPrivate are all in the
front. CodecPrivate hvcC/avcC gives SPS-level resolution, profile and level.

### 2d. Video (TrackEntry/Video 0xE0) [spec]
| path | ID | notes | linking |
|---|---|---|---|
| FlagInterlaced | 0x9A | 0 undetermined, 1 interlaced, 2 progressive | same-film |
| FieldOrder | 0x9D | default 2 (undetermined) | same-film |
| StereoMode | 0x53B8 | 3D layout | same-film |
| AlphaMode | 0x53C0 | | same-film |
| PixelWidth | 0xB0 | coded width, required | same-film |
| PixelHeight | 0xBA | coded height, required | same-film |
| PixelCropBottom/Top/Left/Right | 0x54AA/0x54BB/0x54CC/0x54DD | default 0 | same-film |
| DisplayWidth/Height | 0x54B0/0x54BA | display size, not 0 | same-film |
| DisplayUnit | 0x54B2 | default 0 (pixels) | same-film |
| UncompressedFourCC | 0x2EB524 | only for V_UNCOMPRESSED | same-film |
| Colour 0x55B0 | master | see 2e | same-film |
| Projection 0x7670 | master | ProjectionType 0x7671 (0 rect, 1 equirect, 2 cubemap, 3 mesh), ProjectionPrivate 0x7672, Pose Yaw/Pitch/Roll 0x7673/0x7674/0x7675 | same-film |

Resolution: PixelWidth x PixelHeight is coded size; DisplayWidth/Height is the
display (SAR-adjusted) size. Crop values give the active picture.

### 2e. Colour (Video/Colour 0x55B0) and MasteringMetadata (0x55D0) [spec]
| path | ID | notes |
|---|---|---|
| MatrixCoefficients | 0x55B1 | default 2 (unspecified). 9 = BT.2020 NCL |
| BitsPerChannel | 0x55B2 | default 0 (unspecified). 8, 10, 12 |
| ChromaSubsamplingHorz/Vert | 0x55B3/0x55B4 | 2,2 = 4:2:0 |
| CbSubsamplingHorz/Vert | 0x55B5/0x55B6 | |
| ChromaSitingHorz/Vert | 0x55B7/0x55B8 | |
| Range | 0x55B9 | 1 broadcast/limited, 2 full |
| TransferCharacteristics | 0x55BA | default 2. 16 = SMPTE ST 2084 (PQ, HDR10 and HDR10+ and DV), 18 = ARIB STD-B67 (HLG) |
| Primaries | 0x55BB | default 2. 9 = BT.2020 |
| MaxCLL | 0x55BC | MaxCLL in cd/m2, HDR10 static |
| MaxFALL | 0x55BD | MaxFALL in cd/m2, HDR10 static |
| MasteringMetadata 0x55D0 | master | PrimaryR/G/BChromaticityX/Y 0x55D1..0x55D6, WhitePointChromaticityX/Y 0x55D7/0x55D8, LuminanceMax 0x55D9, LuminanceMin 0x55DA. Mastering display (SMPTE ST 2086 static HDR10). |

Linking for all colour fields: same-film (encode). Treat as the HDR class fingerprint.

HDR classing from the container alone (no frame parse):
- PQ (16) + MasteringMetadata present + MaxCLL/MaxFALL = HDR10 (static).
- PQ + Dolby Vision dvcC/dvvC block addition (2f) = DV.
- HDR10+ (ST 2094-40 dynamic) is NOT signalled in the container header. The
  dynamic metadata is in HEVC SEI (T.35) inside the bitstream. HDR10+ is only
  detectable from the first frames or a block-level signal. [mem, verify]

### 2f. BlockAdditionMapping (TrackEntry 0x41E4) [spec, minver 4]
| path | ID | type | notes |
|---|---|---|---|
| BlockAddIDValue | 0x41F0 | uint | range >= 2 |
| BlockAddIDName | 0x41A4 | string | |
| BlockAddIDType | 0x41E7 | uint (fourcc-like) | default 0 |
| BlockAddIDExtraData | 0x41ED | binary | payload for the type |

Dolby Vision in Matroska [mem, verify]: BlockAddIDType carries the DV config
box. dvcC = 0x64766343 (ASCII "dvcC"), dvvC = 0x64767643 ("dvvC").
The 24-byte DOVIDecoderConfigurationRecord gives dv_version_major/minor,
dv_profile (7 bits), dv_level (6 bits), rpu_present_flag, el_present_flag,
bl_present_flag, dv_bl_signal_compatibility_id. From that:
- profile 5 (IPTPQc2 single-layer), 7 (dual-layer UHD Blu-ray, MEL/FEL), 8.1 / 8.2 / 8.4 (single-layer, HDR10/HLG-compatible).
- dv_level = DV level tier (resolution/fps class, 1-13). This is the "level" the user asked for. The "L2" in the request is ambiguous: it is probably the RPU metadata level (L1, L2, L5, L6...), not the container level.

The per-frame DV RPU NAL (UNSPEC62 in HEVC) carries the L1-L11 metadata. It is in
the video bitstream, not the container header. Reading it needs parsing Blocks
(the first few frames suffice for L1/L2 presence, but the RPU levels can vary per
scene; the first 1-4 MB covers only the opening scenes).

HDR10+ and DV RPU both need in-band NAL/SEI parsing. Container level gives only the flags above.

Linking: BlockAdditionMapping: same-film.

## 3. Cluster (0x1F43B675) and Block layout [spec, partial]
- Timestamp 0xE7 (uint, exactly one per Cluster, [spec])
- Position 0xA7 (maxver 4), PrevSize 0xAB
- SimpleBlock 0xA3 (minver 2), contains track VINT + timecode + flags + frame data
- BlockGroup 0xA0 > Block 0xA1, BlockAdditions 0x75A1 (BlockMore 0xA6, BlockAdditional 0xA5, BlockAddID 0xEE), BlockDuration 0x9B, ReferencePriority 0xFA, ReferenceBlock 0xFB, CodecState 0xA4, DiscardPadding 0x75A2 [spec]
- The first Cluster starts after Info/Tracks/Chapters/Attachments in mkvmerge output.
- Frame count can be obtained from the block count (Blocks/SimpleBlocks per track), but only over the whole file.

Front: Clusters are everywhere; the first cluster is near the front. Frame counts
and bitrate are NOT derivable from the first MB without scanning the file.

Linking: Cluster internals: alt (frame content identical after remux, block
boundaries may move).

## 4. Cues (0x1C53BB6B) [mem]
- CuePoint 0xBB > CueTime 0xB3 (ticks), CueTrackPositions 0xB7 > CueTrack 0xF7, CueClusterPosition 0xF1, CueRelativePosition 0xF0, CueBlockNumber 0x5378, CueCodecState 0xEA, CueReference 0xDB.
- Index of keyframes/seek points. Useful for duration sanity and for estimating the byte offset of later frames.
- Front: usually NO. mkvmerge and ffmpeg write Cues after the last Cluster. Some writers (certain live/web muxers) put Cues at the front after SeekHead. [mem]
- Linking: none (positional).

## 5. Tags (0x1254C367) [tagging spec fetched, IDs mem]
Structure (fetched from matroska.org/technical/tagging.html):
- Tags > Tag (0x7373) > Targets (0x63C0) + SimpleTag(s) (0x67C8)
- Targets: TargetTypeValue (0x68CA, uint), TargetType (0x63CA, string, optional),
  TagTrackUID (0x63C5), TagEditionUID (0x63C9), TagChapterUID (0x63C4),
  TagAttachmentUID (0x63C6). Repeated same-type UIDs are ORed; different UID types are ANDed. [fetched]
- SimpleTag: TagName (0x45A3, string), TagString (0x4487, utf8), TagLanguage (0x447A),
  TagLanguageBCP47 (0x447B), TagDefault (0x4484), TagBinary (0x4485), nested SimpleTag (attributes). [mem IDs; structure fetched]
- TagName uses A-Z, 0-9, _; no spaces; not starting with _. Custom tags start with _. [fetched]

TargetTypeValue levels [fetched; note: video level names taken from page]:
| value | audio | video |
|---|---|---|
| 70 | COLLECTION | COLLECTION |
| 60 | EDITION/ISSUE/VOLUME/OPUS | SEASON/SEQUEL/VOLUME |
| 50 | ALBUM/OPERA/CONCERT | MOVIE/EPISODE/CONCERT |
| 40 | PART/SESSION | PART/SESSION |
| 30 | TRACK/SONG | CHAPTER |
| 20 | SUBTRACK/PART/MOVEMENT | SCENE |
| 10 | (none) | SHOT |
A tag at a level applies to lower levels; empty string blocks inheritance.

Standard tag names fetched (not exhaustive): TITLE, SUBTITLE, ARTIST, DIRECTOR, ACTOR,
CHARACTER, WRITTEN_BY, PRODUCER, LABEL, IMDB, TMDB, TVDB, TVDB2, ISRC, ISBN, BARCODE,
ENCODER, ENCODER_SETTINGS, BPS, FPS, BPM, TOTAL_PARTS, PART_NUMBER, DATE_RELEASED,
DATE_RECORDED, DATE_STARTED, DATE_ENDED, GENRE, KEYWORDS, MOOD, COMMENT, RATING,
COPYRIGHT, LICENSE, REPLAYGAIN_GAIN, REPLAYGAIN_PEAK, RECORDING_LOCATION, URL.
Note the tagging page does NOT list the mkvmerge statistics tags, so below is [mem].

mkvmerge statistics tags [mem, verify]: per track, TargetTypeValue 30 with TagTrackUID.
- BPS (average bitrate, bits/s)
- DURATION (HH:MM:SS.nnnnnnnnn)
- NUMBER_OF_FRAMES
- NUMBER_OF_BYTES
- _STATISTICS_WRITING_APP (writing tool that produced the stats)
- _STATISTICS_WRITING_DATE_UTC
- _STATISTICS_TAGS (lists which stats tags are present)
These are written at finalise, i.e. at the END of the file with Tags. They are the
only source of exact frame count and bitrate in the container, and they are
absent in files that were not made by mkvmerge (or with --disable-track-statistics-tags). [mem]

Front: usually NO. Tags are written at the end of the file (after last Cluster and Cues). Some
muxers write a Tags element in the front (e.g. Title-only tags in a pre-Cluster
block) but statistics tags are end-of-file. [mem]
Linking: tags with TargetTypeValue 50/70 (titles, IMDB/TMDB): alt. Statistics tags: alt
(frame count same across remux, bytes/BPS differ). ENCODER/ENCODER_SETTINGS: same-film.

## 6. Chapters (0x1043A770) [mem IDs]
- EditionEntry 0x45B9 > EditionUID 0x45BC, EditionFlagHidden 0x45BD, EditionFlagDefault 0x45DB, EditionFlagOrdered 0x45DD
- ChapterAtom 0xB6 (recursive) > ChapterUID 0x73C4, ChapterStringUID 0x5654, ChapterTimeStart 0x91 (ns), ChapterTimeEnd 0x92, ChapterFlagHidden 0x98, ChapterFlagEnabled 0x4598, ChapterSegmentUUID 0x6E67, ChapterSegmentEditionUID 0x6EBC, ChapterPhysicalEquiv 0x63C3, ChapterTrack 0x8F, ChapterDisplay 0x80 (ChapString 0x85, ChapLanguage 0x437C, ChapLanguageBCP47 0x437D, ChapCountry 0x437E), ChapProcess 0x6944
- Ordered chapters (EditionFlagOrdered) + ChapterSegmentUUID = linked-segment chapters (e.g. DVD/BD multi-part). Important for "sub names / chapter names".
- Front: YES when mkvmerge is used with chapters (written before first Cluster). [mem]. Linking: alt (chapter titles and times survive remux).

## 7. Attachments (0x1941A469) [mem IDs]
- AttachedFile 0x61A7 > FileDescription 0x467E, FileName 0x466E, FileMediaType 0x4660 (e.g. "image/jpeg", "application/x-truetype-font", "font/otf"), FileData 0x465C (binary), FileUID 0x46AE
- Typical content: cover.jpg / poster.jpg, font files for ASS subs (Arial.ttf, etc.).
- Front: YES when attached with mkvmerge (before first Cluster). [mem]
- FileData is large (fonts 100 KB-several MB). The first 1-4 MB may hold them.
- Linking: alt (FileName/FileMediaType/FileData hash stable across remux; FileUID is random: none).

## 8. Subtitles (track level) [spec + CodecID mem]
- S_TEXT/UTF8, S_TEXT/ASS, S_TEXT/SSA, S_TEXT/WEBVTT, S_HDMV/PGS, S_VOBSUB, S_KATE, S_TEXT/USF. [mem]
- Sub name = TrackEntry/Name (0x536E). Language from Language/LanguageBCP47. Flags Forced/HearingImpaired.
- Front: YES (track entries). Subtitle payloads are blocks, so spread across clusters.
- Linking: alt.

## 9. Audio (TrackEntry/Audio 0xE1) [spec]
| path | ID | notes |
|---|---|---|
| SamplingFrequency | 0xB5 | float, required. Default 8000 |
| OutputSamplingFrequency | 0x78B5 | float, SBR/HE-AAC output |
| Channels | 0x9F | uint, required, default 1 |
| ChannelPositions | 0x7D7B | binary |
| BitDepth | 0x6264 | uint |
| Emphasis | 0x52F1 | |
Sample rate (Hz) and channel count from here. Audio bitrate is NOT in Audio master. It
must come from the BPS statistics tag, or from the codec frame headers (AC-3, E-AC-3,
DTS, TrueHD, Opus, AAC all carry bitrate in-band, or CodecPrivate for some).
Front: YES. Linking: same-film.

## 10. Video frame and bitrate summary
- fps from DefaultDuration (2d). Exact frame count only from Tags NUMBER_OF_FRAMES (end) or by counting blocks (full scan).
- bitrate: BPS statistics tag (end of file); otherwise NUMBER_OF_BYTES / DURATION from the tag, or from total file size / Duration (rough, [mem]).
- Resolution: PixelWidth x PixelHeight (2d), also in SPS of CodecPrivate hvcC/avcC/av1C.

## 11. Front-of-file summary for Atlas (first 1-4 MB)

Readable from the first 1-4 MB (in order of reliability):
1. EBML header: DocType (matroska vs webm), DocTypeVersion. (spec)
2. Segment header (size may be unknown). SeekHead positions. (spec)
3. Info: TimestampScale, Duration (if written in place at finalise), Title, DateUTC, MuxingApp, WritingApp. (spec)
4. Tracks: every TrackEntry (codec, language, names, flags, DefaultDuration, CodecPrivate, Video/Audio/Colour/Mastering/Projection, BlockAdditionMapping incl. DV dvcC/dvvC). (spec)
5. Chapters (EditionEntry, ChapterAtom, names, times). Usually before first Cluster for mkvmerge. [mem]
6. Attachments (cover art and fonts). Usually before first Cluster for mkvmerge. [mem]
7. Cluster header and first blocks: first frames for HEVC/AV1 SPS/VPS and DV RPU L1/L2 presence, HDR10+ T.35 SEI in frame 0 or first GOP. [mem]

NOT readable from the first 1-4 MB (end of file or a full scan):
- Tags with statistics: NUMBER_OF_FRAMES, NUMBER_OF_BYTES, BPS, DURATION. [mem; written at finalise, end]
- Cues. [mem; end]
- Exact frame count, true bitrate, exact per-track bytes. Full scan only.
- Full DV RPU level set across the whole film (only the opening scenes).
- Any Tags written by a post-process tool (e.g. mkvpropedit, Matroska Tag Editor).

Atlas practical rule: the front gives duration (Info), codecs, resolution, colour/HDR class
(PQ + mastering + MaxCLL), DV profile/level (dvcC), languages, chapters and
attachments, and the muxer identity. Frame count and bitrate need a tail read or a Cues-based
seek (SeekHead gives the Tags/Cues offset, so a second ranged read to that offset is cheap if the
article set allows it).

Caveat for Atlas: Matroska files stored inside RAR (store mode) are contiguous, so SeekHead
offsets still work once you have the byte range.

## 12. Rust crates (crates.io metadata fetched this session, 2026-11-02)

| crate | version | total dl | recent dl | updated | scope | notes |
|---|---|---|---|---|---|---|
| matroska | 0.30.1 | 338,231 | 70,636 | 2026-04-15 | metadata parse (tuffy/matroska) | Targets metadata reading. Likely seeks to Cues/Tags at the end. Needs Read+Seek. |
| matroska-demuxer | 0.8.1 | 211,729 | 94,763 | 2026-08-07 | demux (hasenbanck) | Parses tracks and blocks. Reads the whole structure; needs Read+Seek. Maintained. |
| webm-iterable | 0.7.1 | 585,948 | 256,618 | 2026-08-28 | WebM/Matroska schema iterator (austinleroy) | Built on ebml-iterable. Schema-driven, generic. Maintained. |
| ebml-iterable | 0.7.1 | 586,619 | 256,383 | 2026-08-28 | generic EBML iterator (austinleroy) | Generic EBML. Schema-driven. Maintained. Best base if we want a schema. |
| mkv-element | 0.4.0 | 4,090 | 1,408 | 2026-03-02 | element reader/writer (b01o) | Low adoption. |
| symphonia-format-mkv | 0.6.1 | 9,956,034 | 5,051,624 | 2026-08-13 | Pure Rust demuxer (Symphonia) | Very widely used. Probes and demuxes. Returns track info and metadata. Needs seekable input. |
| ebml | 0.0.0 | 2,032 | 53 | 2019-07-07 | ebml parser | Unmaintained. Do not use. |
| matroska-parser | n/a | n/a | n/a | n/a | n/a | Not on crates.io. |

Not verified this session (README/source not read): partial-input behaviour, unknown-size handling, whether
any of them exposes the Tags/statistics. The crates.io listing did not state it.

Recommendation [analysis, not verified against source]:
- Project rule is widespread crates only. symphonia-format-mkv (~10M downloads, Symphonia project) is
  the widespread one. It handles tracks and metadata, but it expects a seekable stream and
  will fail if the tail (Cues/Tags) is missing; check its behaviour on truncated input before use.
- ebml-iterable (~587k downloads) is the widely used generic EBML reader. Use it with a hand-written
  Matroska schema for the front-of-file walk. It is the best fit for truncated input because it iterates
  elements and lets you stop at any byte offset.
- matroska and matroska-demuxer are widespread enough, but both need Read+Seek to the file's tail
  for Cues and Tags. For Atlas's first-N-MB input, expect them to error out or return
  partial data. Verify before adopting.
- A small hand-written EBML walker (~300 lines: VINT read, unknown-size handling, Master
  descent, skip Void/CRC, stop at first Cluster or at end of buffer) fits the first-1-4-MB
  requirement best and avoids truncation failure. Recommended as the primary path, with symphonia
  kept as an optional fallback when the full file is available.

## 13. Surprises
1. The DV RPU and HDR10+ dynamic metadata are NOT container metadata. They are in the video bitstream
   (HEVC NAL / SEI). The container only carries dvcC/dvvC config flags. Atlas needs NAL parsing to see
   L1/L2/L5 etc. [mem, verify]
2. Duration is in TimestampScale ticks, not seconds, and is a float. Default 1 ms ticks. [spec]
3. Language defaults to "eng" if the element is absent, which will mis-tag files with no language. [spec]
4. FlagDefault defaults to 1. A track with no flags set counts as default. [spec]
5. Statistics tags (BPS, NUMBER_OF_FRAMES, NUMBER_OF_BYTES) are written at the end, not the front. [mem]
6. Matroska tags are per-target. A TITLE at TargetTypeValue 50 is a movie title, at 30 a track name. Read
   Targets before trusting a tag. [spec]
7. DocTypeVersion 4 added CodecDelay, SeekPreRoll, Colour, Projection, BlockAdditionMapping. Older
   files (v2) lack HDR and DV fields entirely. [spec]
8. Colour TransferCharacteristics 16 (PQ) is the strongest in-header HDR signal. Mastering metadata and
   MaxCLL/MaxFALL are optional and often absent even for HDR10 sources. [spec]
9. UNKNOWN-size elements (all VINT data bits set) mean the Segment or Cluster size is not written. The
   walker must treat these as "to end of file" or "to next top-level ID". [spec, RFC 8794]
10. MuxingApp and WritingApp are required. They are the best muxer fingerprint, but they are often
    spoofed or left default by repackers. [spec for required; mem for spoofing]

## 14. Linking value summary (per Atlas definition)
- twin (byte-identical post): every field matches, including SegmentUUID, TrackUIDs, Cluster bytes.
- alt (same inner file, differently packed): content fields survive; regenerated IDs change. Good keys:
  Duration (approx), DefaultDuration, PixelWidth/Height, CodecPrivate hash, Colour, DV dvcC, Title,
  Chapters (times and names), Attachments (FileData hash), Statistics NUMBER_OF_FRAMES.
- same-film (same content, different encode): video/audio codec, resolution, HDR class, DV profile,
  MuxingApp/WritingApp (only if same tool), Language, track names may match loosely.
- none: SegmentUUID, TrackUID, FileUID, Cluster timestamps, Cues positions, SeekHead offsets,
  DateUTC, statistics BPS/NUMBER_OF_BYTES (change on any re-encode or remux).
