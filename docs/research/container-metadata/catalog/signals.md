# Container linking signals: Linking Signals Map rows

Container-level signals (MKV/WebM, MP4/MOV, ISO/UDF/BDMV, DVD, TS/M2TS, AVI, ASF, FLV, Ogg) for the Linking Signals Map. Sorted by what the signal proves (twin, alt, same_film, same_disc, same_subs, same_broadcast, tool, uploader), then by cost to read.

Cost units: free, first article, first 4 MB, +1-2 targeted articles, tail article, full. Atlas status is for Atlas a29ee52: the probe JSON stores container, muxing and writing app, title, duration, tracks, first-edition chapters and DV profile.

| Signal | Proves | Cost to read | Atlas today | Notes |
|---|---|---|---|---|
| MKV SegmentUUID (Info 0x73A4) | twin | first article | no | mkvmerge: front; Lavf: front unless bitexact. Regenerated per mux, so same UUID = same mux run. Absent with mkvmerge --engage no_variable_data; then use the alt key. Tested in raw/w2a. |
| MKV DateUTC (Info) and stats/encode date tags | twin | first article | no | DateUTC is ns since 2001-01-01 UTC and changes per mux: equal values = same mux run (twin); different values = separate mux of the same streams (alt candidate). Tags DATE_ENCODED and _STATISTICS_WRITING_DATE_UTC sit in tail. |
| FLAC STREAMINFO MD5 and MP4 dfLa MD5 | twin | first article | no | MD5 of decoded PCM: survives container changes and matches the same audio across encodes (same_film for audio). MKV: first article via CodecPrivate; MP4: dfLa in moov (tail). All-zero MD5 = not computed. |
| ASF File Properties File ID GUID | twin | first article | no | Byte copies and remuxes that keep the header share it; a fresh mux gets a new GUID. Same header gives exact file size and play_duration. |
| MKV ENCODER_SETTINGS (x264/x265 option line) | twin | tail article | no | Tag in Tags (mkvmerge: tail; Lavf: front). Identical option string across releases = same encode settings; a strong same-encode key when present, often absent. |
| MP4 mvhd/tkhd/mdhd creation_time (set vs unset) | twin | tail article | no | ffmpeg writes 0 (unset) by default, so no signal on most files. Only a non-zero value set by a tool or user is a key; compare per-track values too. |
| Remux-proof alt key (codec config hash + first payload hashes) | alt | first article | no | Hash of codec config record, first video frame, first 8 video and first 16 audio payloads. Identical across mkvmerge, Lavf and MKV<->MP4 remuxes; crf28 control differs. Needs ~200 KB at 1080p (budget 700 KB) and video coverage; audio-only keys collide. MP4 without faststart needs targeted reads. |
| Codec config record hash (avcC, hvcC, av1C, vpcC in CodecPrivate / stsd) | alt | first article | buggy | Config bytes equal across MKV and MP4 remuxes (avcC equals MKV CodecPrivate). Atlas bit_depth reads Colour.BitsPerChannel, which is often absent, so 10-bit shows None; hvcC byte 17 gives depth. Hash itself not stored. |
| Track table signature (type, codec ID, channels, rate, default/forced flags, language, track names) | same_film | first article | collected | Tracks element per file. Atlas stores audio and subtitle track names, flags and language; video track name is not stored. Track names are alt-level. Codec ID and channel count are same_film-level. |
| Container runtime (MKV Info Duration; MP4 mvhd/elst; FLAC total samples; Ogg last granule) | same_film | first article | collected | MKV: duration x TimestampScale; mkvmerge float32 gives ms-scale error on long films. MP4 runtime in mvhd (tail). Atlas stores container duration only; playable length (ordered-edition sum, edit lists) is not computed. |
| MKV linked segments (PrevUUID/NextUUID, SegmentFamily, OP/ED chapter links) | same_film | first article | no | Info PrevUUID, NextUUID and SegmentFamily tie CD1/CD2 and multi-file sets. Chapter ChapterSegmentUUID links (OP/ED from another file) are in Chapters, first 4 MB. Atlas ignores both. |
| HDR static metadata (Colour mastering/MaxCLL, MP4 colr/mdcv/clli) and SPS VUI colour | same_film | first article | buggy | mkvmerge writes no Colour element for raw HEVC input, so the SPS VUI in CodecPrivate is the fallback for primaries and transfer. SEI 137/144 sit in hvcC only for ffmpeg muxes. Atlas labels SEI-only HDR10 as PQ and never reads mastering or light-level values. |
| DV config record (dvcC / dvvC BlockAdditionMapping; MP4 box; TS 0xB0 descriptor) | same_film | first article | collected | Profile, level, rpu/el/bl flags and compat id (0 none, 1 HDR10, 2 SDR, 4 HLG). mkvmerge writes it in MKV for DV input (tested); ffmpeg 6.1.1 never writes it into MP4. Atlas stores the DV profile; level and EL are parsed, compat id and rpu flag are not. |
| Legacy header duration and exact size (AVI avih/strh, ASF play_duration, FLV onMetaData) | same_film | first article | no | Duration plus exact byte size is a same-film key; filesize alone is a twin check. AVI frame count and rate from avih/strh (idx1 chunk index is tail). FLV duration and filesize from the onMetaData block. |
| MKV chapter timing signature (start times, count, names) | same_film | first 4 MB | buggy | Chapter start times in ns plus count and titles; hash of the sequence is one key per cut. Atlas reads only the first edition, so a hidden first edition is returned (wave 5). Chapters come before first Cluster in mkvmerge but big fonts push them past 700 KB. MP4 chpl (100 ns units) is in tail. |
| HDR10+ presence (ST 2094-40 SEI, BD hdr_plus flag, AV1 T.35 OBU) | same_film | first 4 MB | no | SEI in first IDR access unit (country B5, provider 003C, code 0001) for MKV/TS/MP4; BD: STN hdr_plus_flag (targeted). Confirms HDR10+ without decoding RPU. Label precedence: DV record, then HDR10+, HDR10, PQ, HLG, SDR. |
| BD STN and MPLS HDR/DV flags (dynamic_range_type, color_space, DV enhancement-layer entry, static HDR ext 3,5; index.bdmv ext 3.1 disc flags) | same_film | +1-2 targeted articles | no | Stream attribute bytes in MPLS (and CLPI, shifted one byte). index.bdmv version 0300 = UHD. Enum values for dynamic_range_type are from libfreemkv and unverified; the field presence is verified in libbluray 1.5.0 source. |
| BD clip set (MPLS PlayItem clip ids, IN/OUT) and CLPI TS recording rate | same_film | +1-2 targeted articles | no | IN/OUT on 45 kHz clock give title runtime; clip ids name STREAM/CLPI files. Recording rate = clip mux bitrate; source packets x 192 x 8 / rate checks duration. Main title: drop playlists under 20 s and looping ones, take the longest. Clip set matches across encodes of one title. |
| DVD PGC runtime, cell map and title-set table (VMG/VTS IFO) | same_film | +1-2 targeted articles | no | PGC playback_time gives title runtime; cell entries give VOB sector ranges. Pick the longest PGC across title sets (decoy title sets exist). IFO for VTS_01 sits in first 700 KB on a 1-title disc; others need targeted reads. |
| Per-track frame count and bitrate (MKV statistics tags; MP4 stsz/stts/stss/btrt) | same_film | tail article | no | MKV: NUMBER_OF_FRAMES, BPS and NUMBER_OF_BYTES in Tags (mkvmerge tail; Lavf rewrites DURATION). MP4: exact sample count, sample sizes, sync table (GOP). Frame count plus byte total is near-exact; declared btrt can be stale after remux. |
| Audio codec config boxes (dac3, dec3 incl. JOC, ddts, dac4, dOps, dmlp, esds) | same_film | tail article | buggy | Gives channel layout, real sample rate (dfLa/dOps beyond 16-bit stsd field), bitrate, DTS profile and TrueHD format. dec3 JOC flag is the Atmos signal. Atlas detects Atmos from track name only; channels and rate come from the track table. |
| ISO9660 PVD and UDF volume id / volume set id (genisoimage: 16 hex) | same_disc | first 4 MB | no | Volume label is reused across releases, so weak alone. UDF volume set id is what DVDUDFVolumeInfo returns. PVD space size (2048-byte sectors) is a twin size check. PVD at 0x8000 and UDF AVDP at 0x80000, both in first 700 KB. |
| DVDDiscID (MD5 over VIDEO_TS.IFO and VTS_nn_0.IFO) | same_disc | +1-2 targeted articles | no | Same disc gives the same ID for every rip regardless of file names; 32 lowercase hex. Confirmed with libdvdread 6.1.3 over whole IFO files; N from VMG title-set count. VTS_02 IFO at ~21 MB on 2-title discs, so walk ISO9660/UDF directories. The DSI sector layout of VOBs is a rip fingerprint. |
| BD id.bdmv disc_id (org_id at byte 40, disc_id at byte 44) | same_disc | +1-2 targeted articles | no | 16-byte disc_id from CERTIFICATE/id.bdmv; org_id is the licensee id (alt-level: same replicator family). Per-pressing versus per-title behaviour NOT confirmed; test on two rips of one release and two of another. META/DL bdmt XML titles and the thumbnail hash (twin) come from the same disc folder. |
| ASS header hash ([Script Info] + [V4+ Styles], garbage section removed) | same_subs | first article | no | MKV S_TEXT/ASS CodecPrivate, about 4 KB, in Tracks. Same translation across releases gives the same hash. Hash only after removing the [Aegisub Project Garbage] section (see uploader row), otherwise local paths enter the key. |
| Track language and flag tables (forced, SDH, commentary, BCP 47; DVD VTS and BD STN languages) | same_subs | first article | collected | MKV Tracks: FlagForced, FlagHearingImpaired, FlagCommentary, LanguageBCP47. Atlas collects these for MKV only. DVD VTS audio/subpicture attribute blocks and BD STN language fields are targeted reads. |
| VobSub .idx palette hash (16-colour palette, size, lang index) | same_subs | first article | no | VobSub idx text in MKV CodecPrivate. Palette hash identifies one DVD rip's subtitles and survives re-muxing. Event count is in subpicture packets (see next row). |
| Attachment font-set hash and cover-art hash | same_subs | first 4 MB | no | Fonts attached to MKV (mkvmerge: before first Cluster). Hash the sorted font file set; sniff magic bytes because MIME can be wrong. Font files can be MB-scale, so FileData must be complete. Cover hash (cover.jpg, MP4 covr) is a weak same-film key. |
| Subtitle event count and MP4 timed-text sample entries (MKV stats; tx3g; DVD subpicture packets) | same_subs | tail article | collected | Event count: MKV stats NUMBER_OF_FRAMES (tail); MP4 stsz samples minus zero-size tx3g filler; DVD subpicture packet count (targeted). MP4 tx3g ftab font table, wvtt vttC and stpp namespace: Atlas stores the codec only. |
| TS SDT service name and provider (descriptor 0x48), NIT network/transponder, ATSC VCT channel | same_broadcast | first article | no | Channel identity of a broadcast capture. ffmpeg -metadata service_name/service_provider writes SDT. NIT (satellite transponder, orbital position) and ATSC VCT channel number narrow the source. Strongest capture identifier; ffmpeg writes only what metadata sets. |
| DVB EIT present/following event (title, start, length) and TDT/TOT capture time | same_broadcast | first article | no | Programme title and UTC start identify the airing; TDT gives UTC capture time (few seconds accuracy), TOT the local offset. Usually inside first 700 KB; ATSC EIT/ETT in first 4 MB. ffmpeg does not write EIT or TDT. |
| MuxingApp, WritingApp and Duration float size | tool | first article | collected | MKV Info strings, e.g. 'libebml v1.4.5 + libmatroska v1.7.1', 'mkvmerge v82.0', 'Lavf60.16.100'. Duration data length 4 = mkvmerge float32; 8 = Lavf float64. Atlas stores both strings; the float-size tell is not read. |
| MKV element layout (SeekHead, Void, Tags/Cues placement) | tool | first article | no | mkvmerge: SeekHead, Void, Info, Tracks, Chapters, Attachments, Clusters, Cues, Tags last. Lavf: Tags at front, per-track DURATION rewritten at trailer. Layout separates muxers before any tag is read; Tags placement needs a tail read. |
| MP4 box order, ftyp brands and free padding | tool | first article | no | ffmpeg default: ftyp, free, mdat, moov (moov at end, so tail fetch needed); +faststart or MP4Box: moov in front. Brands (isom, iso5, mp41), minor_version 0x200 (ffmpeg). dvh1 brand or dby1 alone does not prove DV; the dvcC box does. |
| Legacy writer strings (AVI ISFT and encoder user data, FLV encoder, Ogg vendor, ASF WM/Encoder and EncodingSettings) | tool | first article | no | AVI ISFT and 00dc user data ('Lavc60.31.102', 'XviD0050', 'DivX503b1393p'); FLV onMetaData encoder; Ogg vendor string (Ogg serial is twin-level); ASF WM/EncodingSettings. Build strings and settings are per-tool fingerprints. |
| Mastering tool strings (genisoimage PVD application id, UDF implementation id, DVDAuthor provider id) | tool | first article | no | PVD application id 'GENISOIMAGE ISO 9660/HFS FILESYSTEM CREATOR'; UDF implementation id '*genisoimage'; VMG provider 'DVDAuthor x.y'. Separates home-made ISO from pressed disc. Generic values match many images, so use as a filter. Pressed-disc values not seen. |
| MP4 encoder tags (hdlr name, ©too, compressorname, iTunSMPB priming) | tool | tail article | no | hdlr VideoHandler/SoundHandler (ffmpeg), GPAC/L-SMASH names; ©too 'HandBrake x.y', 'Lavf60.16.100', 'x265'; stsd compressorname 'x264 core'. iTunSMPB priming (2112 typical for Apple AAC) identifies the encoder. Remuxes may keep or drop these. |
| XMP DocumentID and CreatorTool (MP4 uuid box) | tool | tail article | no | DocumentID is an export-lineage key (twin-level within one export chain); CreatorTool gives editor name and version. Packet may also hold author and GPS: scan privacy fields first (see uploader rows). |
| Aegisub project garbage (local video path in ASS header) | uploader | first article | no | PRIVACY: [Aegisub Project Garbage] in ASS CodecPrivate records the encoder's local video path and name (e.g. a Windows user folder). Strip before hashing the ASS header (see same_subs row); never store the raw value. |
| MKV Segment filename and prev/next filename | uploader | first article | no | PRIVACY: SegmentFilename keeps the file name at mux time; prev/next filenames name linked parts. Can leak the uploader's local naming. Alt-level only; mainly a privacy scrub target. |
| ISO PVD publisher/preparer and UDF LVInfo owner fields | uploader | first article | no | PRIVACY: genisoimage -p data_preparer string can name a person or shop; PVD publisher and UDF *UDF LV Info owner, organisation and contact are free text. Blank on many home-made discs. PVD is first article; LVInfo in first 4 MB. |
| iTunes apID, purd, ownr, sfID (purchase account) | uploader | tail article | no | PRIVACY: apID is the purchasing Apple ID (account email); purd the purchase date; ownr the owner name; sfID the storefront country (narrows buyer). apID plus purd identifies one buy event. Hash or drop; never display. |
| MKV Tags ENCODED_BY, COMMENT, PURCHASE_OWNER | uploader | tail article | no | PRIVACY: ENCODED_BY names the person or group that encoded (handle or real name); COMMENT is free text that often names the ripper; PURCHASE_OWNER holds store buyer data. Tags sit at tail for mkvmerge, front for Lavf. |

JSON (same rows):

```json
[
  {
    "signal": "MKV SegmentUUID (Info 0x73A4)",
    "proves": "twin",
    "cost": "first article",
    "atlas": "no",
    "notes": "mkvmerge: front; Lavf: front unless bitexact. Regenerated per mux, so same UUID = same mux run. Absent with mkvmerge --engage no_variable_data; then use the alt key. Tested in raw/w2a."
  },
  {
    "signal": "MKV DateUTC (Info) and stats/encode date tags",
    "proves": "twin",
    "cost": "first article",
    "atlas": "no",
    "notes": "DateUTC is ns since 2001-01-01 UTC and changes per mux: equal values = same mux run (twin); different values = separate mux of the same streams (alt candidate). Tags DATE_ENCODED and _STATISTICS_WRITING_DATE_UTC sit in tail."
  },
  {
    "signal": "FLAC STREAMINFO MD5 and MP4 dfLa MD5",
    "proves": "twin",
    "cost": "first article",
    "atlas": "no",
    "notes": "MD5 of decoded PCM: survives container changes and matches the same audio across encodes (same_film for audio). MKV: first article via CodecPrivate; MP4: dfLa in moov (tail). All-zero MD5 = not computed."
  },
  {
    "signal": "ASF File Properties File ID GUID",
    "proves": "twin",
    "cost": "first article",
    "atlas": "no",
    "notes": "Byte copies and remuxes that keep the header share it; a fresh mux gets a new GUID. Same header gives exact file size and play_duration."
  },
  {
    "signal": "MKV ENCODER_SETTINGS (x264/x265 option line)",
    "proves": "twin",
    "cost": "tail article",
    "atlas": "no",
    "notes": "Tag in Tags (mkvmerge: tail; Lavf: front). Identical option string across releases = same encode settings; a strong same-encode key when present, often absent."
  },
  {
    "signal": "MP4 mvhd/tkhd/mdhd creation_time (set vs unset)",
    "proves": "twin",
    "cost": "tail article",
    "atlas": "no",
    "notes": "ffmpeg writes 0 (unset) by default, so no signal on most files. Only a non-zero value set by a tool or user is a key; compare per-track values too."
  },
  {
    "signal": "Remux-proof alt key (codec config hash + first payload hashes)",
    "proves": "alt",
    "cost": "first article",
    "atlas": "no",
    "notes": "Hash of codec config record, first video frame, first 8 video and first 16 audio payloads. Identical across mkvmerge, Lavf and MKV<->MP4 remuxes; crf28 control differs. Needs ~200 KB at 1080p (budget 700 KB) and video coverage; audio-only keys collide. MP4 without faststart needs targeted reads."
  },
  {
    "signal": "Codec config record hash (avcC, hvcC, av1C, vpcC in CodecPrivate / stsd)",
    "proves": "alt",
    "cost": "first article",
    "atlas": "buggy",
    "notes": "Config bytes equal across MKV and MP4 remuxes (avcC equals MKV CodecPrivate). Atlas bit_depth reads Colour.BitsPerChannel, which is often absent, so 10-bit shows None; hvcC byte 17 gives depth. Hash itself not stored."
  },
  {
    "signal": "Track table signature (type, codec ID, channels, rate, default/forced flags, language, track names)",
    "proves": "same_film",
    "cost": "first article",
    "atlas": "collected",
    "notes": "Tracks element per file. Atlas stores audio and subtitle track names, flags and language; video track name is not stored. Track names are alt-level. Codec ID and channel count are same_film-level."
  },
  {
    "signal": "Container runtime (MKV Info Duration; MP4 mvhd/elst; FLAC total samples; Ogg last granule)",
    "proves": "same_film",
    "cost": "first article",
    "atlas": "collected",
    "notes": "MKV: duration x TimestampScale; mkvmerge float32 gives ms-scale error on long films. MP4 runtime in mvhd (tail). Atlas stores container duration only; playable length (ordered-edition sum, edit lists) is not computed."
  },
  {
    "signal": "MKV linked segments (PrevUUID/NextUUID, SegmentFamily, OP/ED chapter links)",
    "proves": "same_film",
    "cost": "first article",
    "atlas": "no",
    "notes": "Info PrevUUID, NextUUID and SegmentFamily tie CD1/CD2 and multi-file sets. Chapter ChapterSegmentUUID links (OP/ED from another file) are in Chapters, first 4 MB. Atlas ignores both."
  },
  {
    "signal": "HDR static metadata (Colour mastering/MaxCLL, MP4 colr/mdcv/clli) and SPS VUI colour",
    "proves": "same_film",
    "cost": "first article",
    "atlas": "buggy",
    "notes": "mkvmerge writes no Colour element for raw HEVC input, so the SPS VUI in CodecPrivate is the fallback for primaries and transfer. SEI 137/144 sit in hvcC only for ffmpeg muxes. Atlas labels SEI-only HDR10 as PQ and never reads mastering or light-level values."
  },
  {
    "signal": "DV config record (dvcC / dvvC BlockAdditionMapping; MP4 box; TS 0xB0 descriptor)",
    "proves": "same_film",
    "cost": "first article",
    "atlas": "collected",
    "notes": "Profile, level, rpu/el/bl flags and compat id (0 none, 1 HDR10, 2 SDR, 4 HLG). mkvmerge writes it in MKV for DV input (tested); ffmpeg 6.1.1 never writes it into MP4. Atlas stores the DV profile; level and EL are parsed, compat id and rpu flag are not."
  },
  {
    "signal": "Legacy header duration and exact size (AVI avih/strh, ASF play_duration, FLV onMetaData)",
    "proves": "same_film",
    "cost": "first article",
    "atlas": "no",
    "notes": "Duration plus exact byte size is a same-film key; filesize alone is a twin check. AVI frame count and rate from avih/strh (idx1 chunk index is tail). FLV duration and filesize from the onMetaData block."
  },
  {
    "signal": "MKV chapter timing signature (start times, count, names)",
    "proves": "same_film",
    "cost": "first 4 MB",
    "atlas": "buggy",
    "notes": "Chapter start times in ns plus count and titles; hash of the sequence is one key per cut. Atlas reads only the first edition, so a hidden first edition is returned (wave 5). Chapters come before first Cluster in mkvmerge but big fonts push them past 700 KB. MP4 chpl (100 ns units) is in tail."
  },
  {
    "signal": "HDR10+ presence (ST 2094-40 SEI, BD hdr_plus flag, AV1 T.35 OBU)",
    "proves": "same_film",
    "cost": "first 4 MB",
    "atlas": "no",
    "notes": "SEI in first IDR access unit (country B5, provider 003C, code 0001) for MKV/TS/MP4; BD: STN hdr_plus_flag (targeted). Confirms HDR10+ without decoding RPU. Label precedence: DV record, then HDR10+, HDR10, PQ, HLG, SDR."
  },
  {
    "signal": "BD STN and MPLS HDR/DV flags (dynamic_range_type, color_space, DV enhancement-layer entry, static HDR ext 3,5; index.bdmv ext 3.1 disc flags)",
    "proves": "same_film",
    "cost": "+1-2 targeted articles",
    "atlas": "no",
    "notes": "Stream attribute bytes in MPLS (and CLPI, shifted one byte). index.bdmv version 0300 = UHD. Enum values for dynamic_range_type are from libfreemkv and unverified; the field presence is verified in libbluray 1.5.0 source."
  },
  {
    "signal": "BD clip set (MPLS PlayItem clip ids, IN/OUT) and CLPI TS recording rate",
    "proves": "same_film",
    "cost": "+1-2 targeted articles",
    "atlas": "no",
    "notes": "IN/OUT on 45 kHz clock give title runtime; clip ids name STREAM/CLPI files. Recording rate = clip mux bitrate; source packets x 192 x 8 / rate checks duration. Main title: drop playlists under 20 s and looping ones, take the longest. Clip set matches across encodes of one title."
  },
  {
    "signal": "DVD PGC runtime, cell map and title-set table (VMG/VTS IFO)",
    "proves": "same_film",
    "cost": "+1-2 targeted articles",
    "atlas": "no",
    "notes": "PGC playback_time gives title runtime; cell entries give VOB sector ranges. Pick the longest PGC across title sets (decoy title sets exist). IFO for VTS_01 sits in first 700 KB on a 1-title disc; others need targeted reads."
  },
  {
    "signal": "Per-track frame count and bitrate (MKV statistics tags; MP4 stsz/stts/stss/btrt)",
    "proves": "same_film",
    "cost": "tail article",
    "atlas": "no",
    "notes": "MKV: NUMBER_OF_FRAMES, BPS and NUMBER_OF_BYTES in Tags (mkvmerge tail; Lavf rewrites DURATION). MP4: exact sample count, sample sizes, sync table (GOP). Frame count plus byte total is near-exact; declared btrt can be stale after remux."
  },
  {
    "signal": "Audio codec config boxes (dac3, dec3 incl. JOC, ddts, dac4, dOps, dmlp, esds)",
    "proves": "same_film",
    "cost": "tail article",
    "atlas": "buggy",
    "notes": "Gives channel layout, real sample rate (dfLa/dOps beyond 16-bit stsd field), bitrate, DTS profile and TrueHD format. dec3 JOC flag is the Atmos signal. Atlas detects Atmos from track name only; channels and rate come from the track table."
  },
  {
    "signal": "ISO9660 PVD and UDF volume id / volume set id (genisoimage: 16 hex)",
    "proves": "same_disc",
    "cost": "first 4 MB",
    "atlas": "no",
    "notes": "Volume label is reused across releases, so weak alone. UDF volume set id is what DVDUDFVolumeInfo returns. PVD space size (2048-byte sectors) is a twin size check. PVD at 0x8000 and UDF AVDP at 0x80000, both in first 700 KB."
  },
  {
    "signal": "DVDDiscID (MD5 over VIDEO_TS.IFO and VTS_nn_0.IFO)",
    "proves": "same_disc",
    "cost": "+1-2 targeted articles",
    "atlas": "no",
    "notes": "Same disc gives the same ID for every rip regardless of file names; 32 lowercase hex. Confirmed with libdvdread 6.1.3 over whole IFO files; N from VMG title-set count. VTS_02 IFO at ~21 MB on 2-title discs, so walk ISO9660/UDF directories. The DSI sector layout of VOBs is a rip fingerprint."
  },
  {
    "signal": "BD id.bdmv disc_id (org_id at byte 40, disc_id at byte 44)",
    "proves": "same_disc",
    "cost": "+1-2 targeted articles",
    "atlas": "no",
    "notes": "16-byte disc_id from CERTIFICATE/id.bdmv; org_id is the licensee id (alt-level: same replicator family). Per-pressing versus per-title behaviour NOT confirmed; test on two rips of one release and two of another. META/DL bdmt XML titles and the thumbnail hash (twin) come from the same disc folder."
  },
  {
    "signal": "ASS header hash ([Script Info] + [V4+ Styles], garbage section removed)",
    "proves": "same_subs",
    "cost": "first article",
    "atlas": "no",
    "notes": "MKV S_TEXT/ASS CodecPrivate, about 4 KB, in Tracks. Same translation across releases gives the same hash. Hash only after removing the [Aegisub Project Garbage] section (see uploader row), otherwise local paths enter the key."
  },
  {
    "signal": "Track language and flag tables (forced, SDH, commentary, BCP 47; DVD VTS and BD STN languages)",
    "proves": "same_subs",
    "cost": "first article",
    "atlas": "collected",
    "notes": "MKV Tracks: FlagForced, FlagHearingImpaired, FlagCommentary, LanguageBCP47. Atlas collects these for MKV only. DVD VTS audio/subpicture attribute blocks and BD STN language fields are targeted reads."
  },
  {
    "signal": "VobSub .idx palette hash (16-colour palette, size, lang index)",
    "proves": "same_subs",
    "cost": "first article",
    "atlas": "no",
    "notes": "VobSub idx text in MKV CodecPrivate. Palette hash identifies one DVD rip's subtitles and survives re-muxing. Event count is in subpicture packets (see next row)."
  },
  {
    "signal": "Attachment font-set hash and cover-art hash",
    "proves": "same_subs",
    "cost": "first 4 MB",
    "atlas": "no",
    "notes": "Fonts attached to MKV (mkvmerge: before first Cluster). Hash the sorted font file set; sniff magic bytes because MIME can be wrong. Font files can be MB-scale, so FileData must be complete. Cover hash (cover.jpg, MP4 covr) is a weak same-film key."
  },
  {
    "signal": "Subtitle event count and MP4 timed-text sample entries (MKV stats; tx3g; DVD subpicture packets)",
    "proves": "same_subs",
    "cost": "tail article",
    "atlas": "collected",
    "notes": "Event count: MKV stats NUMBER_OF_FRAMES (tail); MP4 stsz samples minus zero-size tx3g filler; DVD subpicture packet count (targeted). MP4 tx3g ftab font table, wvtt vttC and stpp namespace: Atlas stores the codec only."
  },
  {
    "signal": "TS SDT service name and provider (descriptor 0x48), NIT network/transponder, ATSC VCT channel",
    "proves": "same_broadcast",
    "cost": "first article",
    "atlas": "no",
    "notes": "Channel identity of a broadcast capture. ffmpeg -metadata service_name/service_provider writes SDT. NIT (satellite transponder, orbital position) and ATSC VCT channel number narrow the source. Strongest capture identifier; ffmpeg writes only what metadata sets."
  },
  {
    "signal": "DVB EIT present/following event (title, start, length) and TDT/TOT capture time",
    "proves": "same_broadcast",
    "cost": "first article",
    "atlas": "no",
    "notes": "Programme title and UTC start identify the airing; TDT gives UTC capture time (few seconds accuracy), TOT the local offset. Usually inside first 700 KB; ATSC EIT/ETT in first 4 MB. ffmpeg does not write EIT or TDT."
  },
  {
    "signal": "MuxingApp, WritingApp and Duration float size",
    "proves": "tool",
    "cost": "first article",
    "atlas": "collected",
    "notes": "MKV Info strings, e.g. 'libebml v1.4.5 + libmatroska v1.7.1', 'mkvmerge v82.0', 'Lavf60.16.100'. Duration data length 4 = mkvmerge float32; 8 = Lavf float64. Atlas stores both strings; the float-size tell is not read."
  },
  {
    "signal": "MKV element layout (SeekHead, Void, Tags/Cues placement)",
    "proves": "tool",
    "cost": "first article",
    "atlas": "no",
    "notes": "mkvmerge: SeekHead, Void, Info, Tracks, Chapters, Attachments, Clusters, Cues, Tags last. Lavf: Tags at front, per-track DURATION rewritten at trailer. Layout separates muxers before any tag is read; Tags placement needs a tail read."
  },
  {
    "signal": "MP4 box order, ftyp brands and free padding",
    "proves": "tool",
    "cost": "first article",
    "atlas": "no",
    "notes": "ffmpeg default: ftyp, free, mdat, moov (moov at end, so tail fetch needed); +faststart or MP4Box: moov in front. Brands (isom, iso5, mp41), minor_version 0x200 (ffmpeg). dvh1 brand or dby1 alone does not prove DV; the dvcC box does."
  },
  {
    "signal": "Legacy writer strings (AVI ISFT and encoder user data, FLV encoder, Ogg vendor, ASF WM/Encoder and EncodingSettings)",
    "proves": "tool",
    "cost": "first article",
    "atlas": "no",
    "notes": "AVI ISFT and 00dc user data ('Lavc60.31.102', 'XviD0050', 'DivX503b1393p'); FLV onMetaData encoder; Ogg vendor string (Ogg serial is twin-level); ASF WM/EncodingSettings. Build strings and settings are per-tool fingerprints."
  },
  {
    "signal": "Mastering tool strings (genisoimage PVD application id, UDF implementation id, DVDAuthor provider id)",
    "proves": "tool",
    "cost": "first article",
    "atlas": "no",
    "notes": "PVD application id 'GENISOIMAGE ISO 9660/HFS FILESYSTEM CREATOR'; UDF implementation id '*genisoimage'; VMG provider 'DVDAuthor x.y'. Separates home-made ISO from pressed disc. Generic values match many images, so use as a filter. Pressed-disc values not seen."
  },
  {
    "signal": "MP4 encoder tags (hdlr name, ©too, compressorname, iTunSMPB priming)",
    "proves": "tool",
    "cost": "tail article",
    "atlas": "no",
    "notes": "hdlr VideoHandler/SoundHandler (ffmpeg), GPAC/L-SMASH names; ©too 'HandBrake x.y', 'Lavf60.16.100', 'x265'; stsd compressorname 'x264 core'. iTunSMPB priming (2112 typical for Apple AAC) identifies the encoder. Remuxes may keep or drop these."
  },
  {
    "signal": "XMP DocumentID and CreatorTool (MP4 uuid box)",
    "proves": "tool",
    "cost": "tail article",
    "atlas": "no",
    "notes": "DocumentID is an export-lineage key (twin-level within one export chain); CreatorTool gives editor name and version. Packet may also hold author and GPS: scan privacy fields first (see uploader rows)."
  },
  {
    "signal": "Aegisub project garbage (local video path in ASS header)",
    "proves": "uploader",
    "cost": "first article",
    "atlas": "no",
    "notes": "PRIVACY: [Aegisub Project Garbage] in ASS CodecPrivate records the encoder's local video path and name (e.g. a Windows user folder). Strip before hashing the ASS header (see same_subs row); never store the raw value."
  },
  {
    "signal": "MKV Segment filename and prev/next filename",
    "proves": "uploader",
    "cost": "first article",
    "atlas": "no",
    "notes": "PRIVACY: SegmentFilename keeps the file name at mux time; prev/next filenames name linked parts. Can leak the uploader's local naming. Alt-level only; mainly a privacy scrub target."
  },
  {
    "signal": "ISO PVD publisher/preparer and UDF LVInfo owner fields",
    "proves": "uploader",
    "cost": "first article",
    "atlas": "no",
    "notes": "PRIVACY: genisoimage -p data_preparer string can name a person or shop; PVD publisher and UDF *UDF LV Info owner, organisation and contact are free text. Blank on many home-made discs. PVD is first article; LVInfo in first 4 MB."
  },
  {
    "signal": "iTunes apID, purd, ownr, sfID (purchase account)",
    "proves": "uploader",
    "cost": "tail article",
    "atlas": "no",
    "notes": "PRIVACY: apID is the purchasing Apple ID (account email); purd the purchase date; ownr the owner name; sfID the storefront country (narrows buyer). apID plus purd identifies one buy event. Hash or drop; never display."
  },
  {
    "signal": "MKV Tags ENCODED_BY, COMMENT, PURCHASE_OWNER",
    "proves": "uploader",
    "cost": "tail article",
    "atlas": "no",
    "notes": "PRIVACY: ENCODED_BY names the person or group that encoded (handle or real name); COMMENT is free text that often names the ripper; PURCHASE_OWNER holds store buyer data. Tags sit at tail for mkvmerge, front for Lavf."
  }
]
```
