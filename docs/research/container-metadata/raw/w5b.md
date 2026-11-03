# w5b: Advanced Matroska/WebM structures for a robust metadata extractor

Scope: chapters/editions, tags, unknown-size/lacing/CRC/Void, track fields, linking, WebM restrictions, version differences.
Verification legend: [V-XML] = read from ebml_matroska.xml (fetched from raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/ebml_matroska.xml, 2026-11-02); [V-LOCAL] = observed with local mkvmerge v82.0 / ffprobe; [MEM] = from memory, not checked against a fetched source.
Not reachable in this session: chapters.md, ordering.md, tagging.md, notes.md, codec_specs.md (404 on master root); RFC 9559 / RFC 8794 (rfc-editor, datatracker, ietf.org all 403 via proxy); matroska.org not tried; GitHub API/gh blocked for this repo (add_repo not requested).

## 1. Editions and chapters

- element: EditionEntry | ID 0x45B9 | master | path \Segment\Chapters\EditionEntry | webm=yes [V-XML]
  parser_action: iterate all editions; one Chapters element may hold several.
- element: EditionFlagDefault | 0x45DB | uint 0-1, default 0 | [V-XML] | parser_action: pick the default edition; if none set, use the first.
- element: EditionFlagHidden | 0x45BD | uint 0-1, default 0 | [V-XML] | parser_action: hidden editions should not be shown to users; still usable for control tracks.
- element: EditionFlagOrdered | 0x45DD | uint 0-1, default 0 | [V-XML] | parser_action: if 1, chapters may repeat and playback order is defined by the atom tree (see duration below).
- element: EditionUID | 0x45BC | uint, non-zero | [V-XML] | linking_value: target for TagEditionUID and ChapterSegmentEditionUID.
- element: EditionDisplay | 0x4520 | master, minver 5 | [V-XML] | parser_action: localized edition names; report them as edition titles.
- element: ChapterAtom | 0xB6 | master, nestable | [V-XML] | parser_action: recurse; nested atoms are sub-chapters.
- element: ChapterUID | 0x73C4 | uint, non-zero | [V-XML] | linking_value: TagChapterUID target.
- element: ChapterStringUID | 0x5654 | utf-8, minver 3 | [V-XML] | parser_action: store as an opaque string ID.
- element: ChapterTimeStart | 0x91 | uint, ns | [V-XML] | parser_action: convert with TimestampScale-independent ticks (nanoseconds).
- element: ChapterTimeEnd | 0x92 | uint, ns, optional | [V-XML] | note: end is exclusive; must be >= start when present. If absent, the span runs to the next atom or end of segment.
- element: ChapterFlagHidden | 0x98 | uint 0-1, default 0 | [V-XML] | parser_action: hidden chapters are not shown but still count for playback.
- element: ChapterFlagEnabled | 0x4598 | uint 0-1, default 1 | [V-XML] | parser_action: 0 means the player should skip the span (usually control-track driven). Report as disabled.
- element: ChapterSegmentUUID | 0x6E67 | binary (16 bytes) | [V-XML] | linking_value: segment-linking. This chapter plays a span from another file with this SegmentUUID. Anime OP/ED in separate files use this.
- element: ChapterSegmentEditionUID | 0x6EBC | uint | [V-XML] | parser_action: which edition of the linked segment to use. If absent, no edition of the linked segment is used [V-XML wording].
- element: ChapterSkipType | 0x4588 | uint, minver 5 | [V-XML] | enum: 0 No Skipping, 1 Opening Credits, 2 End Credits, 3 Recap, 4 Next Preview, 5 Preview, 6 Advertisement, 7 Intermission (values 8+ not checked). Rules: child must not repeat parent's type. Without ChapterTimeEnd, the type applies until the next ChapterSkipType atom or end of file. This is the skip-OP/ED signal to report.
- element: ChapterTrack / ChapterTrackUID | 0x8F / 0x89 | master / uint | [V-XML] | parser_action: chapter applies only to these tracks; absence means all tracks.
- element: ChapterPhysicalEquiv | 0x63C3 | uint | [V-XML] | e.g. 60 = DVD, 50 = SIDE. Report as a physical-media hint.
- element: ChapterDisplay | 0x80 | master, webm | [V-XML] | parser_action: may contain several titles per chapter, one per language.
- element: ChapString | 0x85 | utf-8 | [V-XML] | the chapter title text.
- element: ChapLanguage | 0x437C | string, default eng | [V-XML] | ignored if ChapLanguageBCP47 is present in the same ChapterDisplay.
- element: ChapLanguageBCP47 | 0x437D | string, minver 4 | [V-XML] | preferred over ChapLanguage. Matroska only, not WebM.
- element: ChapCountry | 0x437E | string | [V-XML] | ignored if ChapLanguageBCP47 is present.
- element: ChapProcess | 0x6944 | master | [V-XML] | DVD-style menu commands.
  - ChapProcessCodecID 0x6955 | uint, default 0 | [V-XML] | 0 = Matroska Script, 1 = DVD menu [MEM].
  - ChapProcessPrivate 0x450D | binary | [V-XML] | for codec 1, the DVD level.
  - ChapProcessCommand 0x6911 | master | [V-XML]; ChapProcessTime 0x6922 uint, ChapProcessData 0x6933 binary | [V-XML] | DVD pre/post commands. Store as raw bytes; do not interpret.
- element: ChapterTranslate | 0x6924 (under Segment/Info) | master | [V-XML] | maps this segment to a codec segment ID (ChapterTranslateID 0x69A5, ChapterTranslateCodec 0x69BF, ChapterTranslateEditionUID 0x69FC). Rarely used. Parser: ignore unless it is DVD menu data.

**Playback duration with ordered chapters (EditionFlagOrdered=1)** [MEM; semantics not in fetched XML]:
- Walk the atoms in the order given. Each atom contributes (ChapterTimeEnd - ChapterTimeStart), skipping disabled or hidden ones. Atoms that link to an external segment (ChapterSegmentUUID) have an unknown duration unless the linked file is present. Mark the total as partial.
- If ChapterTimeEnd is absent, use the next sibling's start (or the Segment Duration).
- Sum of spans is only the true playback length when the edition is ordered and has no external-segment atoms. Otherwise report "partial". Anime fansub files usually have all chapters in-file.

**Frequency** [MEM]: ordered editions and ChapterSegmentUUID are rare outside anime (OP/ED segment links in some fansub remuxes). ChapterSkipType is increasingly present in recent remuxes from mkvtoolnix and some encoders. Not measured in this session.

**Local test** [V-LOCAL]: mkvmerge v82.0 with a simple XML chapter file produced a file that ffprobe parses (2 chapters, nanosecond time base, titles from ChapString).

## 2. Tags and Targets

- element: Tags | 0x1254C367 | master, webm | [V-XML] | top-level.
- element: Tag | 0x7373 | master | [V-XML] | one metadata descriptor.
- element: Targets | 0x63C0 | master | [V-XML] | empty or absent means the whole Segment.
- element: TargetTypeValue | 0x68CA | uint, default 50 | [V-XML] | logical level: 70 COLLECTION, 60 EDITION/ISSUE/VOLUME, 50 ALBUM/MOVIE/EPISODE, 40 PART/SESSION, 30 TRACK/SONG/CHAPTER, 20 SUBTRACK/PART, 10 SUBTRACK/SCENE (values from memory; the XML only states the default 50).
- element: TargetType | 0x63CA | string | [V-XML] | informational label ("ALBUM", "TRACK", "MOVIE", "CHAPTER").
- element: TagTrackUID | 0x63C5 | uint, default 0 | [V-XML] | scopes a tag to a track. Map stats tags (BPS, DURATION, NUMBER_OF_FRAMES, etc.) to the track whose TrackUID matches.
- element: TagEditionUID | 0x63C9 | uint, default 0 | [V-XML] | scopes to an edition.
- element: TagChapterUID | 0x63C4 | uint, default 0 | [V-XML] | scopes to a chapter.
- element: TagAttachmentUID | 0x63C6 | uint, default 0 | [V-XML] | scopes to an attachment.
  parser_action: a value of 0 is "not set". A Tag with all four at 0 applies to the Segment.
- element: SimpleTag | 0x67C8 | master, recursive | [V-XML] | nested SimpleTags are allowed.
- element: TagName | 0x45A3 | utf-8 | [V-XML] | key name, e.g. BPS, DURATION, NUMBER_OF_FRAMES, ENCODER, ENCODING.
- element: TagString | 0x4487 | utf-8 | [V-XML] | value.
- element: TagBinary | 0x4485 | binary | [V-XML] | binary value. Must not coexist with TagString in the same SimpleTag.
- element: TagLanguage | 0x447A | string, default und | [V-XML] | ignored if TagLanguageBCP47 present.
- element: TagLanguageBCP47 | 0x447B | string, minver 4 | [V-XML] | preferred.
- element: TagDefault | 0x4484 | uint 0-1, default 1 | [V-XML] | default/original language value.
- element: TagDefaultBogus | 0x44B4 | minver 0, maxver 0 | [V-XML] | legacy ID for TagDefault. Accept both IDs.
- parser_action: mkvmerge writes stats tags with track-scoped TargetTypeValue and TagTrackUID. Use TagTrackUID to bind BPS/DURATION/FRAMES to the track.

Local test [V-LOCAL]: mkvmerge -i on the sample reported "Global tags: 1 entry" and "Tags for track ID 0/1: 1 entry" each, confirming track-scoped tag blocks.

## 3. Unknown size, lacing, CRC, Void, header

- element: Segment | 0x18538067 | master | unknownsizeallowed=1 [V-XML].
- element: Cluster | 0x1F43B675 | master | unknownsizeallowed=1 [V-XML]. Live recordings use unknown-size clusters.
  parser_action: an unknown size is encoded as all value bits set in the VINT (e.g. 0x01 FF FF FF FF FF FF FF for 8 bytes). When unknown, treat the element as extending until the next element with a parent-level ID (e.g. the next Cluster, Cues, Tags). Do not read to EOF.
- element: Lacing (FlagLacing | 0x9C | uint 0-1, default 1) [V-XML]. Lacing bits in the block header: 00 none, 01 Xiph, 11 EBML, 10 fixed-size [MEM]. parser_action: for header parsing, only Tracks/CodecPrivate/Info/Tags/Chapters need to be read; lacing matters only for block payload. Skip blocks on lacing errors.
- element: CRC-32 | ID 0xBF | binary, 4 bytes | not in the Matroska XML; defined in RFC 8794 (EBML) [MEM]. Usually the first child of a Cluster/Segment. parser_action: optional; may verify with IEEE CRC-32 over the rest of the parent's payload, or ignore.
  [V-LOCAL] mkvmerge v82 output has 4 byte pairs 0xBF 0x84 (length 4) in a 146 KB file, first at offset 13744. Not structurally validated: the hex search alone could match payload bytes.
- element: Void | ID 0xEC | binary, any length | [MEM; not in the Matroska XML] | parser_action: skip. mkvmerge writes many Void elements (428 0xEC bytes observed in the test; not all are Void elements).
- element: EBMLMaxIDLength | 0x42F2 | default 4, must be 4 | [V-XML] | parser_action: reject larger.
- element: EBMLMaxSizeLength | 0x42F3 | default 8, range 1-8 | [V-XML] | parser_action: reject VINTs wider than this.
- element: EBMLVersion 0x4286 (=1), EBMLReadVersion 0x42F7 (=1), DocType 0x4282 ("matroska"/"webm"), DocTypeVersion 0x4287, DocTypeReadVersion 0x4285 [MEM for IDs; EBML header bytes observed in the local file: 1a45dfa3 a3 4286 81 01 42f7 81 01 42f2 81 04, consistent].
  parser_action: DocTypeReadVersion > the parser's supported version => refuse. Accept DocTypeVersion up to the maximum supported.

## 4. Track fields often missed (all [V-XML] unless noted)

- FlagOriginal 0x55AE (minver 4) | original-language track. Good for picking the "main" audio.
- FlagCommentary 0x55AF (minver 4) | commentary track.
- FlagHearingImpaired 0x55AB (minver 4), FlagVisualImpaired 0x55AC (minver 4), FlagTextDescriptions 0x55AD (minver 4) | accessibility tracks.
- FlagForced 0x55AA | forced-subtitle auto-select.
- LanguageBCP47 0x22B59D (minver 4) | preferred; Language 0x22B59C default eng, ignored when BCP47 present.
- TrackOperation 0xE2 (minver 3) > TrackCombinePlanes 0xE3 > TrackPlane 0xE4 > {TrackPlaneUID 0xE5, TrackPlaneType 0xE6} | 3D virtual tracks. TrackJoinBlocks 0xE9 > TrackJoinUID 0xED | joined tracks.
- TrackOverlay 0x6FAB, maxver 0 | deprecated.
- TrickTrackUID 0xC0, TrickTrackSegmentUID 0xC1, TrickTrackFlag 0xC6, TrickMasterTrackUID 0xC7 (all maxver 0) | DivX trick tracks, deprecated.
- ContentEncodings 0x6D80 > ContentEncoding 0x6240 > {ContentEncodingOrder 0x5031 default 0, ContentEncodingScope 0x5032 default 1, ContentEncodingType 0x5033 default 0}.
  - ContentCompression 0x5034 (required when type=0): ContentCompAlgo 0x4254 (0 zlib, 1 bzlib, 2 lzo1x, 3 header stripping) [algo values MEM], ContentCompSettings 0x4255 (for algo 3: bytes removed from the start of each frame). parser_action: header stripping means CodecPrivate and block payloads are missing a prefix. Prepend ContentCompSettings to each frame before probing, or the codec parser breaks.
  - ContentEncryption 0x5035 (type=1): ContentEncAlgo 0x47E1, ContentEncKeyID 0x47E2, ContentEncAESSettings 0x47E7 (minver 4). parser_action: flag encrypted tracks; no decode.
- MaxBlockAdditionID 0x55EE, default 0 | 0 means no BlockAdditions.
- BlockAdditionMapping 0x41E4 (minver 4) > BlockAddIDValue 0x41F0 (>=2), BlockAddIDName 0x41A4, BlockAddIDType 0x41E7 (default 0), BlockAddIDExtraData 0x41ED.
- CodecDelay 0x56AA (minver 4, ns, default 0) | subtract from frame timestamps (Opus pre-skip). SeekPreRoll 0x56BB (minver 4, ns, default 0) | Opus pre-roll.
- DefaultDecodedFieldDuration 0x234E7A (minver 4, ns) | interlaced field rate.
- Video (under Tracks > TrackEntry > Video):
  - FieldOrder 0x9D (minver 4, default 2) | interlace.
  - StereoMode 0x53B8 (minver 3, default 0) | 3D.
  - AlphaMode 0x53C0 (minver 3, default 0) | alpha via BlockAddID 1.
  - PixelCropBottom 0x54AA (default 0), DisplayUnit 0x54B2 (default 0).
  - UncompressedFourCC 0x2EB524 (binary) | uncompressed pixel format.
  - Colour 0x55B0 (minver 4) > MatrixCoefficients 0x55B1 (default 2), ChromaSitingHorz 0x55B7 (default 0), Range 0x55B9 (default 0). Also Projection (ProjectionType 0x7671, ProjectionPoseYaw 0x7673, range -180..180, default 0) [V-XML].

## 5. Info-level linking

- SegmentUUID 0x73A4 (binary, 128 bit, UUID-v4-shaped) [V-XML].
- SegmentFilename 0x7384 (utf-8) [V-XML].
- PrevUUID 0x3CB923 / PrevFilename 0x3C83AB; NextUUID 0x3EB923 / NextFilename 0x3E83BB [V-XML].
- SegmentFamily 0x4444 (binary, 128 bit, shared by all segments of a linked set) [V-XML].
- Duration 0x4489 (float, Segment ticks, webm=yes) [V-XML]. Multiply by TimestampScale (0x2AD7B1, default 1000000 ns) for real seconds.
  parser_action: for multi-part CD1/CD2 files, read PrevUUID/NextUUID to chain them. Use SegmentFamily to confirm they belong together.

## 6. WebM restrictions and version differences

- webm=yes in the XML marks elements allowed in WebM (verified per element above). WebM excludes: ChapLanguageBCP47, FlagHearingImpaired/VisualImpaired/TextDescriptions/Original/Commentary, TagLanguageBCP47, TagDefaultBogus, ChapterSkipType, ChapterSegmentUUID, ChapterSegmentEditionUID, TagEditionUID, TagChapterUID, TagAttachmentUID, Colour/Projection subtree, ContentCompression, ContentEncryption AESSettings, CRC-32 etc. [V-XML webm flags for each element above]
- DocTypeVersion 4 introduced BlockAdditionMapping, FlagHearingImpaired/VisualImpaired/TextDescriptions/Original/Commentary, LanguageBCP47, ChapLanguageBCP47, TagLanguageBCP47, Colour, Projection, CodecDelay/SeekPreRoll, FieldOrder [V-XML minver=4 on each].
- DocTypeVersion 5 introduced EditionDisplay and ChapterSkipType [V-XML minver=5].
- DocTypeVersion 3 introduced TrackOperation, StereoMode, AlphaMode, ChapterStringUID is minver 3 [V-XML].

## Parser checklist (short)
1. Accept DocType matroska or webm; reject DocTypeReadVersion above the supported max.
2. Skip Void (0xEC); verify or skip CRC-32 (0xBF).
3. Handle unknown-size Segment/Cluster by searching for the next sibling ID.
4. Read Tracks > ContentEncodings first. Header stripping (algo 3) changes frame bytes.
5. Read Chapters, prefer ChapLanguageBCP47 over ChapLanguage.
6. Bind Tag values to tracks via TagTrackUID; treat 0 as segment-level.

## Gaps
- chapters.md, ordering.md, tagging.md, notes.md, RFC 9559, RFC 8794: not reached. Items marked [MEM] need checking against those.
- Frequency of ChapterSkipType, ordered editions, and segment links in anime fansub corpora: not measured.
