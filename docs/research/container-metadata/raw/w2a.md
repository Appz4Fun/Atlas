# w2a: Matroska layout, IDs and muxer strings (verified where marked)

Method: spec XML and FFmpeg matroskaenc.c fetched and read; mkvmerge v82.0 installed via apt; ffmpeg 6.1.1 (Lavf60.16.100) local; test MKVs built and dumped with a stdlib EBML walker (scratch: /tmp/claude-0/-home-claude/a01cd88b-5624-59c3-a16b-2c70d53b5c29/scratchpad/w2a/ebmldump.py).

Access limits: mkvtoolnix source NOT reachable (github.com/mbunkus/mkvtoolnix raw paths 404, codeberg blocked 403). Every mkvmerge source-level claim below is [MEM] and only its behaviour is [TESTED]. MakeMKV, HandBrake, DVDFab, Tdarr are not installed; their strings are [MEM].

Tags: [SRC] = read in source; [SPEC] = spec XML; [TEST] = observed in a generated file; [MEM] = unverified.

## 1. Top-level layout per tool

Test inputs: testsrc+sine 4 s, mpeg4 + AAC, 2 chapters, title tag.

| Tool / mode | Front (first ~4 KB) | Back (end of file) | Evidence |
|---|---|---|---|
| Lavf 6.1 default (seekable) | SeekHead(52, 80 B; written at trailer into reserved Void) > Void(137) > Info(213) > Tracks(306) > Chapters(538) > Tags(638, incl. per-track DURATION) > first Cluster(953) | Cues(200718) | [SRC] matroskaenc.c L900-912, 2617-2655, 2700-2720, 3296-3420; [TEST] |
| Lavf with -reserve_index_space 5000 | ...Tags(638) > Cues(953, moved in at trailer) > Void(1121, 4823 B) > Cluster(5953) | none | [SRC] L2728-2737, 3329-3393; [TEST] |
| Lavf with -cues_to_front 1 | ...Tags(638) > Cues(953) > Cluster(1121) (data shifted) | none | [SRC] L3346-3375; [TEST] |
| Lavf live (is_live) | Info without Duration; no trailer Duration rewrite | Cues at end if seekable | [SRC] L2632-2655 |
| mkvmerge v82 default (--cues iframes) | SeekHead(52, 77 B: Info + Tracks entries seen) > Void(134, 4014 B reserved) > Info(4151) > Tracks(4298) > Void(4487, 1143 B) > Chapters(5633) > Void(5739, 101 B) > Cluster(5842) | Cues(204777) > Tags(204932, 700 B incl. statistics) | [TEST] |
| mkvmerge --cues 0:none | same front; no Cues | Tags(204555) | [TEST] |
| mkvmerge --disable-track-statistics-tags | same front | Cues(204568) > Tags(204723, 43 B: ENCODER only) | [TEST] |
| mkvmerge --engage no_variable_data | Void(119) > Info(4151, 82 B) > ... | Cues(204494) > Tags(204649, 632 B) | [TEST] |

Duration placement:
- Lavf writes Info/Duration early, but only when duration is known from input metadata. Otherwise it reserves Void(11) and rewrites it at trailer. Every track DURATION string is rewritten at trailer. [SRC L2644-2655, 3415, 3466-3490; TEST: Duration 4023.0 in Info]
- mkvmerge writes Duration as float32 (size 4) inside Info at header time [TEST: 4040.0]. Whether it is rewritten in place is [MEM]; not checked against source.

Statistics tags (mkvmerge only):
- Per track: BPS, DURATION, NUMBER_OF_FRAMES, NUMBER_OF_BYTES. Global: _STATISTICS_WRITING_APP, _STATISTICS_WRITING_DATE_UTC, _STATISTICS_TAGS. [TEST]
- They sit at the end, after Cues. No front reservation was seen for them. [TEST]
- Lavf writes none of these; grep of matroskaenc.c for BPS, NUMBER_OF_FRAMES, _STATISTICS returns nothing. [SRC]

Front-of-file implication for Atlas:
- mkvmerge: BPS/frames/bytes are at the end, outside the first ~700 KB. Front gives Info (apps, Title, DateUTC, Duration), Tracks, Chapters. Do not expect bitrate from front Tags on mkvmerge output.
- Lavf: Tags with ENCODER, HANDLER_NAME, VENDOR_ID, track ENCODER and DURATION are at the front. Cues are at the end unless reserve_index_space or cues_to_front is set.
- Correction to w1a: "Tags at end" is true for mkvmerge only. Lavf puts Tags in the front. Cues at end is true for both default modes.

## 2. Muxing and writing app strings

| String | Field | Source | Encodes |
|---|---|---|---|
| Lavf60.16.100 | MuxingApp + WritingApp (Lavf 6.1.1) | [TEST] | FFmpeg libavformat major 60. Version string LIBAVFORMAT_IDENT. WritingApp is overridden by the "encoding_tool" metadata if set. [SRC L2617-2621] |
| Lavf | both, under -fflags/ -bitexact | [SRC L2648-2652], [TEST] | Bitexact mode. No DateUTC, no SegmentUUID. |
| libebml v1.4.5 + libmatroska v1.7.1 | MuxingApp (mkvmerge v82) | [TEST] | Library versions. Not the same as the tool version. |
| mkvmerge v82.0 ('I'm The President') 64-bit | WritingApp (mkvmerge v82) | [TEST] | Tool version, release name, bitness. |
| no_variable_data | MuxingApp AND WritingApp (mkvmerge --engage no_variable_data) | [TEST] | Test mode. DateUTC = -978307200000000000 (Unix epoch in ns since 2001), SegmentUUID all zeros. Two runs were byte-identical. |
| HandBrake 1.7.0 2023... | WritingApp (libav-based) | [MEM] | Not installed. Expected to use the Lavf MuxingApp. Unverified. |
| MakeMKV v1.17.5 win(x64-release) | WritingApp | [MEM] | Not installed. Unverified. |
| DVDFab, Tdarr | WritingApp | [MEM] | Not verified. |

Per-mux timestamps (twin/alt key):
- mkvmerge writes DateUTC in Info and _STATISTICS_WRITING_DATE_UTC in Tags [TEST]. Two different runs produced DateUTC 815295378 and 815295393 (ns-scaled), so re-muxes differ.
- Lavf writes DateUTC only when creation_time metadata is present [SRC L2630-2634]. Lavf -fflags +bitexact still gave different bytes across two runs (differs at char 51, cause [MEM]: possibly track/segment UIDs).
- Byte-identical twin: mkvmerge --engage no_variable_data gives reproducible output [TEST]. Otherwise, use DateUTC plus the statistics date as a per-mux fingerprint.

## 3. Element IDs (spec XML, ietf-wg-cellar master, [SPEC])

Source: https://raw.githubusercontent.com/ietf-wg-cellar/matroska-specification/master/ebml_matroska.xml (fetched and parsed; file version=4).

| Name | ID | Type | Path | min/max |
|---|---|---|---|---|
| Segment | 0x18538067 | master | \Segment | 1/1, unknown size allowed |
| SeekHead | 0x114D9B74 | master | \Segment\SeekHead | max 2 |
| Seek | 0x4DBB | master | ...\SeekHead\Seek | min 1 |
| SeekID | 0x53AB | binary(4) | ...\Seek\SeekID | 1/1 |
| SeekPosition | 0x53AC | uint | ...\Seek\SeekPosition | 1/1 |
| Info | 0x1549A966 | master | \Segment\Info | 1/1 |
| SegmentUUID | 0x73A4 | binary | ...\Info\SegmentUUID | 0/1 |
| Duration | 0x4489 | float | ...\Info\Duration | 0/1 |
| DateUTC | 0x4461 | date | ...\Info\DateUTC | 0/1 |
| Tracks | 0x1654AE6B | master | \Segment\Tracks | 0/1 |
| BlockAdditionMapping | 0x41E4 | master | ...\TrackEntry\BlockAdditionMapping | any |
| BlockAddIDValue | 0x41F0 | uint | ...\BlockAdditionMapping\BlockAddIDValue | 0/1 |
| BlockAddIDName | 0x41A4 | string | ...\BlockAdditionMapping\BlockAddIDName | 0/1 |
| BlockAddIDType | 0x41E7 | uint | ...\BlockAdditionMapping\BlockAddIDType | 1/1, default 0 |
| BlockAddIDExtraData | 0x41ED | binary | ...\BlockAdditionMapping\BlockAddIDExtraData | 0/1 |
| Cluster | 0x1F43B675 | master | \Segment\Cluster | unknown size allowed |
| Cues | 0x1C53BB6B | master | \Segment\Cues | 0/1 |
| CuePoint | 0xBB | master | ...\Cues\CuePoint | 1/any |
| CueTime | 0xB3 | uint | ...\CuePoint\CueTime | 1/1 |
| CueTrackPositions | 0xB7 | master | ...\CuePoint\CueTrackPositions | 1/any |
| CueClusterPosition | 0xF1 | uint | ...\CueTrackPositions\CueClusterPosition | 1/1 |
| Chapters | 0x1043A770 | master | \Segment\Chapters | 0/1 |
| EditionEntry | 0x45B9 | master | ...\Chapters\EditionEntry | 1/any |
| ChapterAtom | 0xB6 | master | ...\EditionEntry\+ChapterAtom | 1/any |
| ChapterUID | 0x73C4 | uint | ...\ChapterAtom\ChapterUID | 1/1 |
| ChapterTimeStart | 0x91 | uint | ...\ChapterAtom\ChapterTimeStart | 1/1 |
| ChapterTimeEnd | 0x92 | uint | ...\ChapterAtom\ChapterTimeEnd | 0/1 |
| ChapterDisplay | 0x80 | master | ...\ChapterAtom\ChapterDisplay | [TEST] ID confirmed by parse; path not printed by XML query |
| ChapString | 0x85 | utf-8 | ...\ChapterDisplay\ChapString | 1/1 |
| ChapLanguage | 0x437C | string | ...\ChapterDisplay\ChapLanguage | 1/any, default eng |
| ChapLanguageBCP47 | 0x437D | string | ...\ChapterDisplay\ChapLanguageBCP47 | 0/any |
| ChapCountry | 0x437E | string | ...\ChapterDisplay\ChapCountry | 0/any |
| Tags | 0x1254C367 | master | \Segment\Tags | 0/any |
| Tag | 0x7373 | master | \Segment\Tags\Tag | 1/any |
| Targets | 0x63C0 | master | ...\Tag\Targets | 1/1 |
| TargetTypeValue | 0x68CA | uint | ...\Targets\TargetTypeValue | 1/1, default 50 |
| TargetType | 0x63CA | string | ...\Targets\TargetType | 0/1 |
| TagTrackUID | 0x63C5 | uint | ...\Targets\TagTrackUID | default 0 |
| TagEditionUID | 0x63C9 | uint | ...\Targets\TagEditionUID | default 0 |
| TagChapterUID | 0x63C4 | uint | ...\Targets\TagChapterUID | default 0 |
| TagAttachmentUID | 0x63C6 | uint | ...\Targets\TagAttachmentUID | default 0 |
| SimpleTag | 0x67C8 | master | ...\Tag\+SimpleTag | 1/any |
| TagName | 0x45A3 | utf-8 | ...\SimpleTag\TagName | 1/1 |
| TagLanguage | 0x447A | string | ...\SimpleTag\TagLanguage | 1/1, default und |
| TagLanguageBCP47 | 0x447B | string | ...\SimpleTag\TagLanguageBCP47 | 0/1 |
| TagDefault | 0x4484 | uint | ...\SimpleTag\TagDefault | 1/1, default 1 |
| TagString | 0x4487 | utf-8 | ...\SimpleTag\TagString | 0/1 |
| Attachments | 0x1941A469 | master | \Segment\Attachments | 0/1 |
| AttachedFile | 0x61A7 | master | ...\Attachments\AttachedFile | 1/any |
| FileDescription | 0x467E | utf-8 | ...\AttachedFile\FileDescription | 0/1 |
| FileName | 0x466E | utf-8 | ...\AttachedFile\FileName | 1/1 |
| FileMediaType | 0x4660 | string | ...\AttachedFile\FileMediaType | 1/1 |
| FileData | 0x465C | binary | ...\AttachedFile\FileData | 1/1 |
| FileUID | 0x46AE | uint | ...\AttachedFile\FileUID | 1/1 |
| Void | 0xEC | - | - | [TEST] |
| CRC-32 | 0xBF | - | - | [TEST] (Lavf and mkvmerge both emit it) |

Still [MEM]: EBML header IDs (0x1A45DFA3 and children) are not in this table.

## 4. Source and test cross-check notes

- FFmpeg matroskaenc.c, raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavformat/matroskaenc.c, fetched this session [SRC]. Functions: mkv_start_seekhead (L~900), mkv_write_info (L2607-2655), mkv_write_tags (L2347), mkv_write_header (L2661-2745), mkv_write_trailer (L3296-3420), DURATION simpletag write-back (L3466-3490), DVCC/HVCE BlockAdditionMapping names (L1768-1798, DVCC/DVVC for Dolby Vision config).
- mkvtoolnix source: not fetched. Claims about output_control.cpp, cluster_helper, and Void reserve purposes are [MEM]. The Void(1143) after Tracks and Void(101) after Chapters in mkvmerge output have an unverified purpose.
- MakeMKV, HandBrake, DVDFab, Tdarr: not tested and not in source.

## 5. Summary of what is verified

1. Lavf default: front SeekHead > Info > Tracks > Chapters > Tags; Cues at end. [SRC+TEST]
2. mkvmerge default: front SeekHead > Info > Tracks > Chapters; Cues then Tags at end; statistics Tags live at end. [TEST]
3. Lavf Duration early only when known; per-track DURATION rewritten at trailer. [SRC+TEST]
4. Lavf reserve_index_space and cues_to_front move Cues to the front before the clusters. [SRC+TEST]
5. mkvmerge Duration is float32; Lavf writes float64. [TEST]
6. mkvmerge statistic tag set: BPS, DURATION, NUMBER_OF_FRAMES, NUMBER_OF_BYTES plus three _STATISTICS_* globals; --disable-track-statistics-tags removes them. [TEST]
7. no_variable_data gives byte-identical output across runs. [TEST]
8. Element IDs for Cues, Chapters, Tags, Attachments and their children are confirmed from the spec XML. [SPEC]
