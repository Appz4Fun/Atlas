# w3a: subtitle and attachment metadata in MKV / MP4 / BD / DVD (readable without decoding video)

Test rig: mkvmerge v82.0, ffmpeg 6.1.1, mkvextract (scratch: scratchpad/w3a). Verified = reproduced on a synthetic file in this session. Memory = from prior knowledge, NOT reproduced. Limits: 10-min budget, so no VobSub/PGS/BD/DVD samples were built.

## A. Matroska subtitle CodecIDs and CodecPrivate

- path: Tracks/TrackEntry/CodecID = S_TEXT/ASS; CodecPrivate = full ASS header
  meaning: The entire [Script Info] + [V4+ Styles] head of the .ass file, plus the Aegisub project-garbage section.
  example (verified): Title: Example Fansub Release; Original Script: FooSubs Team; Original Translation: Jane; Original Editing / Original Timing / Script Updated By: Carol; ScriptType: v4.00+; PlayResX 1920 / PlayResY 1080; YCbCr Matrix: TV.709; [Aegisub Project Garbage]: "Video File: C:\Users\jdoe\Videos\Show.S01E01.1080p.mkv", "Audio File", "Active Line", "Video Position"; [V4+ Styles] Fontname lines.
  front_of_file: yes. Tracks element sits at ~4.3 KB of a mkvmerge file (verified), so inside the first 700 KB article.
  linking_value: same-subs (hash of CodecPrivate minus the Aegisub garbage section gives translator/group identity). The Aegisub "Video File" path is a same-source leak (original filename and local user path). Verified.

- path: S_TEXT/ASS track, CodecPrivate length
  meaning: Length of the header block. Sample: 861 bytes. Verified.
  linking_value: twin (identical headers across releases from the same group template).

- path: S_TEXT/UTF8, S_TEXT/WEBVTT, S_TEXT/SSA
  meaning: UTF8 = plain SRT-style, no CodecPrivate. WEBVTT CodecPrivate = "WEBVTT" header (memory). SSA = same as ASS with a different Script Type.
  front_of_file: yes. Verified: none (not tested). Memory.
  linking_value: none for UTF8; twin for WEBVTT/SSA when header is copied.

- path: S_VOBSUB CodecPrivate
  meaning: Text of the .idx file (size:, palette:, langidx:, id: <lang>, timestamp:, "forced subs:" flag, custom colors). Memory only.
  front_of_file: yes. linking_value: same-subs (palette hash), same-source (palette identical across rips from the same DVD).

- path: S_HDMV/PGS, S_HDMV/TEXTST, S_DVBSUB, S_KATE, S_ARIBSUB
  meaning: PGS has no CodecPrivate (language and flags come from the Track entry only). TextST and DVBSUB carry CodecPrivate in some muxers (memory). KATE CodecPrivate = Kate header with language/category. ARIB = memory.
  linking_value: none for PGS (no header); same-subs for KATE/DVBSUB if the headers carry the same ID.

## B. Track flags and naming (Matroska)

- path: TrackEntry/FlagForced, FlagHearingImpaired, FlagCommentary, FlagDefault
  meaning: Disposition bits. mkvmerge flags --forced-display-flag, --hearing-impaired-flag, --commentary-flag, --default-track-flag set and read back via mkvinfo.
  example (verified): "Hearing impaired flag: 0", "Commentary flag 0" printed by mkvinfo. FlagForced not verified in this run (set to 0).
  linking_value: none (disposition only). Forced / SDH is a same-subs qualifier.

- path: TrackEntry/Language (ISO 639-2) and LanguageBCP47
  meaning: mkvmerge maps --language 0:eng to Language eng and LanguageBCP47 en (IETF). mkvmerge -J reported language_ietf 'en' for the eng track (verified). Track with no language gives und/und.
  linking_value: none, but a BCP47 tag such as pt-BR or es-419 is a same-subs qualifier.

- path: TrackEntry/Name
  meaning: Free text name. mkvmerge --track-name. Typical values: SDH, Forced, Signs & Songs, Full, English [CC]. Set on the subtitle track in the test build, but not read back (unverified). Memory for conventions.
  linking_value: alt (naming convention is a group signature).

## C. Subtitle event count and duration (no clusters needed)

- path: Tags/Tag (TargetTypeValue absent or 30/40) with TrackUID target, Simple NUMBER_OF_FRAMES
  meaning: mkvmerge statistics tag. For subtitle tracks, NUMBER_OF_FRAMES = number of events (verified: 2 events in the 2-cue ASS track gave NUMBER_OF_FRAMES 2). Also DURATION (last event end, not file end), BPS, NUMBER_OF_BYTES, _STATISTICS_WRITING_APP ("mkvmerge v82.0 ..."), _STATISTICS_WRITING_DATE_UTC, _STATISTICS_TAGS.
  front_of_file: no. Tags element is after Cues, at file tail (verified: Tags is the last top-level element; Cues @48946, file 50040 bytes). Needs a tail read.
  linking_value: same-subs (event count + duration matches across releases of the same sub).
  note: Only present when the muxer wrote statistics. Not every muxer does this.

## D. Attachments (fonts, cover art)

- path: Segment/Attachments/AttachedFile/FileName, FileMediaType, FileDescription, FileUID, FileData
  meaning: Fonts and cover art. mkvmerge writes them before the first Cluster.
  example (verified): Attachments @6616 precedes first Cluster @6747 in a mkvmerge file with 2 attachments. Read back: font.ttf (font/ttf, description "Arial Narrow"), cover.jpg (image/jpeg).
  front_of_file: yes for mkvmerge output (verified for mkvmerge v82). Inference: if font files are multi-MB, FileData is truncated in a 700 KB read, but FileName, FileMediaType, FileUID sit before FileData within each AttachedFile (not verified order).
  linking_value: same-subs (font set hash = group release fingerprint); same-source (cover.jpg hash).
  warning (verified): MIME type can be wrong. My first build set font/ttf on cover.jpg because the MIME flag order was wrong. Trust magic bytes over FileMediaType.

## E. MP4 subtitle tracks (tx3g via ffmpeg mov_text)

- path: moov/trak/mdia/hdlr handler_type
  meaning: 'sbtl' for tx3g subtitles written by ffmpeg (verified, name "SubtitleHandler"). 'subt' is the Apple/QuickTime name (memory).
  linking_value: none.

- path: moov/trak/mdia/mdhd language (packed ISO-639-2) and timescale/duration
  meaning: Language and duration of the subtitle track. Verified: fre and spa decoded from packed values; timescale 1000000 and duration 3000000 = 3.0 s.
  front_of_file: yes if faststart (verified: moov first in ffmpeg +faststart output).
  linking_value: none.

- path: moov/trak/mdia/minf/stbl/stsd/tx3g entry + ftab box
  meaning: tx3g sample entry. ftab holds the font table; verified: ftab font name 'Arial' in ffmpeg output. Also the default style and display flags. Verified as present.
  linking_value: same-subs (font name table matches across releases of one sub).

- path: moov/trak/mdia/minf/stbl/stsz sample_count
  meaning: Sample count. Verified: 5 samples for a 2-event track, because ffmpeg writes filler samples (empty terminator samples). stsz count is NOT the event count. Subtract empty samples (sample_size 0 and text length 0) to get events. Also stsz count is in the same stbl, so it needs the stbl walk.
  linking_value: twin (only after the empty-sample fix).

- path: moov/trak/tkhd flags
  meaning: enabled bit (flag 1). Verified: ffmpeg set enabled=1 only on the first subtitle track (default disposition). enabled=0 on other tracks. The forced disposition I set on track 2 did NOT show in tkhd flags (memory: forced is usually signaled by track name or a separate forced flag, not by tkhd).
  linking_value: none.

- path: moov/trak/udta/name
  meaning: Track title written from ffmpeg title metadata. Verified: name box 'Forced' inside the subtitle trak's udta. Free text, a same-subs qualifier.
  linking_value: alt.

- path: moov/trak/stsd/wvtt
  meaning: WebVTT in MP4 (vttC config). Not testable: ffmpeg 6.1 mp4 muxer rejected -c:s webvtt ("Invalid argument"). Memory only.

## F. Blu-ray and DVD (memory only, not reproduced)

- path: BDMV/PLAYLIST/*.mpls STN_table, PG/TextST streams: language code (3 letters), TextST character_code byte
  meaning: Subtitle languages per playlist. The TextST character code tells UTF-8 / UTF-16 / Shift-JIS. Memory.
  linking_value: same-disc (playlist STN plus disc ID in id.bdmv).

- path: VTS IFO VTS_PGCI / VTSI_MAT subpicture attributes: language code (2 bytes), code extension (1 normal, 2 large, 3 children, 5 closed caption, 9 forced, 13 director's comment, others)
  meaning: DVD subpicture language and kind. Memory; exact IFO offsets not checked.
  linking_value: same-disc (IFO bytes identical across rips of one disc).

## Recommendations for Atlas
1. Matroska: parse Tracks + CodecPrivate (ASS header) + Attachments from the first 700 KB. Hash CodecPrivate minus the Aegisub garbage section for a same-subs key, and keep the Video File line as same-source evidence.
2. Attachments are before the first Cluster in mkvmerge output, so the first article is enough for font-set and cover hashes.
3. Event count: use Tags NUMBER_OF_FRAMES (verified) from the tail read. For MP4 do not trust stsz count (filler samples).
4. Do not trust FileMediaType. Use magic bytes.
