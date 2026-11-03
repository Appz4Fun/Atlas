# w3c: AVI/OpenDML, MPEG-TS/M2TS, Ogg/OGM, FLV, ASF/WMV, MPEG-PS/VOB metadata

Legend. verified = fetched source this session. search = seen in search results, not fetched. memory = from training, unchecked.
Crate availability: crates.io API returned 403 from the sandbox, so no crate listing is verified. mpeg2ts-reader existence is from search (docs.rs and GitHub dholroyd/mpeg2ts-reader).

Sources
- FFmpeg libavformat/mpegts.c (6.1): https://ffmpeg.org/doxygen/6.1/mpegts_8c_source.html (verified)
- ASF GUID table: http://drang.s4.xrea.com/program/tips/id3tag/wmp/10_asf_guids.html (verified; mirrors ASF spec GUIDs)
- ASF spec draft: https://datatracker.ietf.org/doc/html/draft-fleischman-asf-01 (search)
- OpenDML AVI 1.02: http://www.jmcgowan.com/odmlff2.pdf (search, not fetched)
- Microsoft AVI RIFF reference: https://learn.microsoft.com/en-us/windows/win32/directshow/avi-riff-file-reference (search)
- Dolby Vision in MPEG-2 TS spec: not retrieved (memory)
- FFmpeg AVDOVIDecoderConfigurationRecord: https://ffmpeg.org/doxygen/trunk/structAVDOVIDecoderConfigurationRecord.html (search)
- mpeg2ts-reader descriptors: https://docs.rs/mpeg2ts-reader/latest/mpeg2ts_reader/descriptor/index.html (search)

---

## 1. AVI / OpenDML

- path: RIFF 'AVI ' > LIST hdrl > avih (MainAVIHeader)
  meaning: dwMicroSecPerFrame, dwMaxBytesPerSec, dwTotalFrames, dwStreams, dwWidth, dwHeight, dwFlags (HASINDEX, ISINTERLEAVED, WASCAPTUREFILE)
  example: 40000 us/frame (25 fps), 1920x800, 170000 frames
  front_of_file: yes (first ~200 bytes after RIFF header)
  linking_value: same-film (dims + dwTotalFrames + duration). weak alone.
  status: memory; Microsoft RIFF reference (search)

- path: LIST hdrl > LIST strl > strh (AVIStreamHeader), one per stream
  meaning: fccType ('vids','auds','txts'), fccHandler (XVID, DX50, DIVX, H264, MJPG, or 0 for PCM), dwScale/dwRate (fps = dwRate/dwScale), dwLength (frames or samples), wPriority, wLanguage
  example: fccType=vids fccHandler=XVID dwRate=24000 dwScale=1001 dwLength=142000
  front_of_file: yes
  linking_value: same-film (dwLength + rate). handler alone: none
  status: memory

- path: strl > strf (BITMAPINFOHEADER for vids, WAVEFORMATEX for auds)
  meaning: video: biCompression (FourCC), biWidth, biHeight, biBitCount. audio: wFormatTag, nChannels, nSamplesPerSec, nAvgBytesPerSec, nBlockAlign, cbSize
  example: wFormatTag=0x0055 (MP3), 0x2000 (AC-3), 0x2001 (DTS), 0x0001 (PCM)
  front_of_file: yes
  linking_value: none (codec-level, shared by many encodes)
  status: wFormatTag values memory; BITMAPINFOHEADER per Microsoft reference (search)

- path: strl > strn (stream name, NUL-terminated)
  meaning: per-stream human name, often a language or track title
  front_of_file: yes
  linking_value: alt (distinctive custom names carry across re-muxes of one rip)
  status: memory

- path: hdrl > LIST odml > dmlh (ODML extended header), dwTotalFrames (true count past 1 GB)
  meaning: total frames across all AVIX RIFFs. avih.dwTotalFrames may be capped at 32-bit legacy value.
  front_of_file: yes
  linking_value: same-film
  status: memory; OpenDML 1.02 (search)

- path: hdrl > strl > indx (OpenDML super index, AVI 2.0) and movi > ix00/ix01 (standard index per AVIX segment)
  meaning: super index lists ix chunks (offsets, entries, duration, dwDuration) per stream. Standard index holds per-chunk offsets within each RIFF segment.
  example: indx nEntriesInUse=12 for a 4 GB file
  front_of_file: indx is at front (inside hdrl). ix chunks are inside movi, spread through file.
  linking_value: none
  status: memory; OpenDML 1.02 (search)

- path: RIFF 'AVIX' segments (after first 1 GB RIFF 'AVI ')
  meaning: continuation RIFF chunks for files over 1 GB. Atlas partial reads see only the first segment unless it reads the tail.
  front_of_file: no (follows first segment)
  linking_value: none
  status: memory; OpenDML 1.02 (search)

- path: idx1 (legacy index, 16-byte entries: ckid FourCC, dwFlags, dwChunkOffset, dwChunkLength)
  meaning: flat index of all chunks. Entry count = dwTotalFrames-ish, and can be read to estimate frame count and key-frame count (AVIIF_KEYFRAME flag 0x10).
  front_of_file: no (end of file, after movi)
  linking_value: none. Tail read only.
  status: memory

- path: LIST INFO > ISFT (software that wrote the file)
  meaning: writer identity. Examples seen in the wild: 'VirtualDubMod 1.5.10.2', 'MEncoder', 'Lavf', 'Lavf58.29.100', 'AVI Mux', 'Nero'.
  example: ISFT = 'Lavf58.45.100'
  front_of_file: yes (LIST INFO is in the header area, and usually within the first 1 KB to 4 KB)
  linking_value: alt (same writer and version across batches). ISFT + creation date is closer to twin.
  status: memory (example strings are memory)

- path: LIST INFO > INAM (title), IART (artist/author), ICMT (comment), ICRD (creation date), IENG, ISRC, ISBJ
  meaning: standard RIFF INFO chunks, often carry rip tool text or release info
  front_of_file: yes
  linking_value: alt (ICRD + ISFT together = twin candidate; ICMT often copies release info)
  status: memory

- path: JUNK chunk (padding), also used by writers for alignment and as a placeholder for reserved header space
  meaning: padding. Some writers put a signature or leftover text inside JUNK (for example 'ODML' or 'AVI-Mux' placeholders). Scan JUNK payloads for ASCII.
  front_of_file: yes
  linking_value: alt (writer-specific JUNK sizes and text identify a writer build)
  status: memory

- path: movi > '00dc' / '01wb' etc. (chunk ids: stream number, dc = compressed video, wb = audio)
  meaning: data chunks. First '00dc' payload carries the codec user data: XviD user-data string 'XviD0050' or DivX 'DivX503b1393p' (MPEG-4 Part 2 user_data start code 0x000001B2).
  example: 'XviD0050' in the first frame of an XVID encode, 'DivX503b1393p' in a DivX 5.03 encode
  front_of_file: yes (first video chunk, usually within the first ~700 KB)
  linking_value: alt (same encoder build + build date string); twin if the string is identical and the frame count matches
  status: memory; this is the one that Atlas's 700 KB prefix reads reliably

---

## 2. MPEG-TS / M2TS

- path: TS packet size detection (188, 192 for M2TS, 204 for RS-coded)
  meaning: sync byte 0x47 at offsets 0, N, 2N, ... For M2TS, 192-byte packets carry a 4-byte TP_extra_header before each 188-byte TS packet, sync byte at offset 4.
  front_of_file: yes
  linking_value: none
  status: memory

- path: TS packet PID 0x0000 > PAT (table_id 0x00) > PMT PID
  meaning: program map PID.
  front_of_file: yes (within first few packets; first PMT typically within first 100 packets)
  linking_value: none
  status: memory

- path: PMT (table_id 0x02) stream loop, stream_type values
  meaning: 0x02 MPEG-2 video, 0x1B AVC, 0x24 HEVC, 0x42 AVS, 0x80 SCTE-35 or DCII (memory), 0x81 AC-3 (BD), 0x83 TrueHD (BD, with HDMV), 0x84 E-AC-3 (BD), 0x85 DTS-HD (BD), 0x86 DTS-HD MA or SCTE-35 (depends on CUEI registration), 0x87 E-AC-3 (BD), 0x06 private PES (carries DVB descriptors)
  verified: 0x83 HDMV TrueHD, 0x86 SCTE-35 when CUEI, 0x85/0x86 DTS-HD in HDMV_types (verified, ffmpeg mpegts.c)
  front_of_file: yes
  linking_value: none (generic)
  status: stream type table mostly memory; ffmpeg values verified

- path: PMT descriptor 0x0A ISO_639_language
  meaning: 3-character language code + audio_type byte
  front_of_file: yes
  linking_value: none
  status: memory

- path: PMT descriptor 0x05 registration_descriptor (format_identifier, 4 bytes)
  meaning: identifies the format. Program level values in BD streams: 'HDMV' or 'HDPR'. Stream level values verified in ffmpeg REGD_types: 'AC-3', 'EAC3', 'DTS1','DTS2','DTS3', 'HEVC', 'VC-1', 'Opus', 'BSSD', 'KLVA', 'VANC', 'drac', 'AC-4', 'ID3 '. 'CUEI' marks SCTE-35 on stream_type 0x86.
  verified: ffmpeg mpegts.c (https://ffmpeg.org/doxygen/6.1/mpegts_8c_source.html)
  front_of_file: yes
  linking_value: none
  status: verified

- path: PMT descriptor 0xB0 DOVI video stream descriptor
  meaning: Dolby Vision signaling in TS. Layout (verified from ffmpeg, the 'case 0xb0' branch):
    byte 0: dv_version_major (8)
    byte 1: dv_version_minor (8)
    bytes 2-3, big-endian 16-bit:
      dv_profile = (buf >> 9) & 0x7f  (7 bits)
      dv_level = (buf >> 3) & 0x3f  (6 bits)
      rpu_present_flag = (buf >> 2) & 1
      el_present_flag = (buf >> 1) & 1
      bl_present_flag = buf & 1
    if bl_present_flag == 0: dependency_pid = (bytes 4-5 BE16) >> 3  (13 bits)
    next byte, high nibble: dv_bl_signal_compatibility_id (4 bits), 0 if absent
    minimum descriptor length: 4 bytes
  example: profile 8, level 6, rpu=1 el=0 bl=1, compat id 1 (HDR10)
  front_of_file: yes (in PMT, within first PAT/PMT, first ~2 packets after PAT)
  linking_value: alt (profile+level+compat id shared across encodes of one source). Not unique to film.
  status: verified (ffmpeg). Parser note: ffmpeg traces dependency_pid even when bl_present_flag==1, which is uninitialised in that case. Do not read dependency_pid unless bl_present_flag==0.
  note: the Dolby spec was not retrieved, so the reserved-bit layout beyond this is unverified.

- path: PMT descriptor 0x6A (DVB AC-3) and 0x7A (DVB E-AC-3), and 0x7F extension (DVB extension tag, ffmpeg excerpt)
  meaning: DVB audio codec signaling (ETSI EN 300 468). 0x6A AC-3 descriptor flags: component_type, bsid, mainid, asvc. 0x7A E-AC-3 has more flags.
  front_of_file: yes
  linking_value: none
  status: tags verified present in ffmpeg; field layout memory

- path: PMT descriptor 0x59 subtitling_descriptor
  meaning: per-subtitle-stream: ISO_639 language (3 bytes), subtitling_type (1 byte, e.g. 0x10 DVB normal, 0x20 hard-of-hearing), composition_page_id (16), ancillary_page_id (16)
  front_of_file: yes
  linking_value: none
  status: memory

- path: PMT descriptor 0x56 teletext_descriptor
  meaning: 3-byte language, then teletext_type (5 bits) and magazine_number (3 bits), page number as 2 BCD bytes. Sub type 0x02 = subtitles page.
  front_of_file: yes
  linking_value: none
  status: memory

- path: PMT descriptor 0x52 stream_identifier_descriptor
  meaning: component_tag (1 byte), links to component_tag in SDT/EIT
  front_of_file: yes
  linking_value: none
  status: memory

- path: PMT descriptor 0x86 caption_service_descriptor (ATSC A/65)
  meaning: number_of_services, then per service: language (3), digital_cc flag, cc_type, caption_service_number. Gives CEA-608 field and CEA-708 service 1 to 63 names.
  front_of_file: yes
  linking_value: none
  status: memory

- path: DVB SI SDT (PID 0x0011, table_id 0x42 actual), service_descriptor 0x48
  meaning: service_type, service_provider_name, service_name (DVB text encoding with leading byte for charset)
  front_of_file: sometimes in first 1 MB, often needs a few SDT repetitions (every ~0.5 to 1 s)
  linking_value: same-broadcast (service_name + provider = same channel capture)
  status: memory

- path: DVB SI EIT p/f (PID 0x0012, table_id 0x4E present/following), short_event_descriptor 0x4D; extended_event 0x4E; content_descriptor 0x54 (genre nibbles); parental_rating 0x55 (country + rating)
  meaning: event name, short text, start time (40-bit MJD + BCD HHMMSS), duration (BCD 24-bit), content genre nibbles, rating
  front_of_file: EIT p/f arrives within the first few seconds, so a 700 KB prefix often holds it
  linking_value: same-broadcast (event name + start time). Strongest TS-only linking key.
  status: memory

- path: DVB SI TDT (PID 0x0014, table_id 0x70) and TOT (table_id 0x73)
  meaning: UTC time (MJD + BCD) of capture, TOT adds local offset descriptor 0x58
  front_of_file: yes, within a few seconds
  linking_value: same-broadcast (capture time)
  status: memory

- path: DVB SI NIT (PID 0x0010, table_id 0x40), network_name_descriptor 0x40, satellite_delivery_system_descriptor 0x43 (orbital_position, west/east flag, polarization, frequency), terrestrial 0x5A, cable 0x44
  meaning: network name and delivery parameters. Orbital position is the satellite identity.
  front_of_file: yes
  linking_value: same-broadcast (network + orbital position + transponder frequency)
  status: memory

- path: ATSC PSIP base PID 0x1FFB, VCT (table_id 0xC8 terrestrial, 0xC9 cable), short_name (7 UTF-16 chars), major/minor channel number
  meaning: channel name and number. ETT (0xCC) and EIT (0xCB) carry event titles.
  front_of_file: yes, VCT repeats every few hundred ms
  linking_value: same-broadcast (short name + major.minor)
  status: memory

- path: M2TS TP_extra_header (4 bytes before each 188-byte packet)
  meaning: copy_permission_indicator (2 bits) + arrival_time_stamp (30 bits, 27 MHz clock, wraps about every 2^30 / 27e6 ~= 39.7 s)
  front_of_file: yes, every packet
  linking_value: none directly. Arrival timestamp deltas give mux bitrate and can reveal re-mux twins from the same disc rip (identical ATS sequences).
  status: memory

- path: PES headers (PTS/DTS, 33-bit at 90 kHz)
  meaning: frame presentation timing. Sum of PTS gaps gives fps; first to last PTS gives duration within the sample.
  front_of_file: yes, first PES
  linking_value: none directly
  status: memory

---

## 3. Ogg / OGM

- path: Ogg page header 'OggS' (version 0, header_type BOS 0x02, granule_position (64 bit), serial number, page sequence, CRC)
  meaning: page framing. Granule position on the last page gives total samples (for Vorbis, Opus: samples at 48 kHz for Opus; 'pre-skip' affects it).
  front_of_file: first 2 pages (BOS) at front
  tail: last page holds final granule position (duration) -> tail read needed for duration
  linking_value: none (per-stream serial number is random per mux, so it is a per-mux identity; byte-identical copies share it = twin)
  status: memory

- path: Vorbis identification header packet ('\x01vorbis'), comment header packet ('\x03vorbis'), setup ('\x05vorbis')
  meaning: ident: channels, blocksize, rate, bitrate hints. Comment: vendor string (e.g. 'Xiph.Org libVorbis I 20070622') + KEY=VALUE pairs (TITLE, ARTIST, ALBUM, DATE, ENCODER, TRACKNUMBER, ...), UTF-8.
  front_of_file: yes (comment header on page 2 to 3, small)
  linking_value: alt (vendor string + ENCODER tag). Same tags across rips of one album track = same-film/same-broadcast for audio.
  status: memory

- path: Opus 'OpusHead' and 'OpusTags' packets
  meaning: OpusHead: channel count, pre-skip, input sample rate, output gain, mapping family. OpusTags: vendor string + Vorbis-style comments.
  front_of_file: yes
  linking_value: alt (vendor string)
  status: memory

- path: Theora '\x80theora' identification and comment header
  meaning: frame size (FMBW, FMBH), picture size, frame rate numerator/denominator, pixel aspect ratio, colorspace, keyframe granule shift
  front_of_file: yes
  linking_value: none
  status: memory

- path: OGM stream header packet ('\x01video' / '\x01audio' / '\x01text' ...)
  meaning: 'video' stream header: subtype FourCC at byte offset 8 (e.g. DIV3, DIVX, XVID, MP42, DX50). Then time_unit (100 ns units per frame, e.g. 400000 for 25 fps), samples_per_unit, default_len, buffer_size, bits_per_sample, then width (int32) and height (int32) for video. Audio header uses wFormatTag-style layout.
  front_of_file: yes (first 1 to 2 pages)
  linking_value: alt (fourcc + time_unit + dimensions)
  status: memory (OGM layout from the old OGM/Ogg-Video spec, memory; not fetched)

- path: OGM 'comment' packet ('\x03...' with KEY=VALUE tags, same as Vorbis comment)
  front_of_file: yes
  linking_value: alt
  status: memory

---

## 4. FLV

- path: FLV header: 'FLV' + version 0x01 + flags (0x04 audio present, 0x01 video present) + data offset (9)
  front_of_file: yes
  linking_value: none
  status: memory

- path: first tag, type 18 (script data), body AMF0 'onMetaData' with ECMA array
  meaning: duration (double, seconds), width, height, videodatarate (kbps), framerate, videocodecid (2 = Sorenson H.263, 4 = VP6, 7 = AVC), audiocodecid (2 = MP3, 10 = AAC), audiosamplerate, audiosamplesize, audiochannels, audiodatarate, stereo, filesize (bytes), encoder / metadatacreator (strings, e.g. 'Lavf58.x', 'Adobe Flash Media Server', 'Ffmpeg'), lasttimestamp, lastkeyframetime, keyframes { filepositions[], times[] } (large array on long files; can exceed the 700 KB prefix)
  example: duration=5432.1, width=1280, height=720, framerate=23.976, videocodecid=7, encoder='Lavf57.83.100'
  front_of_file: yes (first tag after 13 bytes). Keyframe table can be large.
  tail: final PreviousTagSize + last tag (lasttimestamp)
  linking_value: alt (encoder + metadatacreator string). duration + filesize = same-film (weak to strong when both match). filesize unique to byte copy = twin.
  status: memory

- path: 'LAST' time: last video tag timestamp (lasttimestamp field), keyframe positions
  front_of_file: no (tail)
  linking_value: same-film

---

## 5. ASF / WMV / WMA

Verified GUIDs (fetched from ASF GUID table, http://drang.s4.xrea.com/program/tips/id3tag/wmp/10_asf_guids.html, mirrors the ASF spec):
- Header Object 75B22630-668E-11CF-A6D9-00AA0062CE6C (file starts with this)
- File Properties 8CABDCA1-A947-11CF-8EE4-00C00C205365
- Stream Properties B7DC0791-A9B7-11CF-8EE6-00C00C205365
- Header Extension 5FBF03B5-A92E-11CF-8EE3-00C00C205365
- Content Description 75B22633-668E-11CF-A6D9-00AA0062CE6C
- Extended Content Description D2D0A440-E307-11D2-97F0-00A0C95EA850
- Codec List 86D15240-311D-11D0-A3A4-00A0C90348F6
- Script Command 1EFB1A30-0B62-11D0-A39B-00A0C90348F6
- Marker F487CD01-A951-11CF-8EE6-00C00C205365
- Padding 1806D474-CADF-4509-A4BA-9AABCB96AAE8
- Metadata Object C5F8CBEA-5BAF-4877-8467-AA8C44FA4CCA
- Metadata Library 44231C94-9498-49D1-A141-1D134E457054
- Data Object 75B22636-668E-11CF-A6D9-00AA0062CE6C
- Index Object D6E229D3-35DA-11D1-9034-00A0C90349BE
- Simple Index 33000890-E5B1-11CF-89F4-00A0C90349CB
- Audio Media F8699E40-5B4D-11CF-A8FD-00805F5C442B
- Video Media BC19EFC0-5B4D-11CF-A8FD-00805F5C442B
- Command Media 59DACFC0-59E6-11D0-A3AC-00A0C90348F6

- path: Header Object children, File Properties (8CABDCA1...)
  meaning: File ID GUID (16 bytes), file size, creation date (100 ns since 1601-01-01), data packet count, play duration (100 ns), send duration, preroll, flags (broadcast, seekable), min/max data packet size, max bitrate
  front_of_file: yes (Header Object is at offset 0, all children in the first KB to tens of KB; Padding can be large)
  linking_value: File ID GUID = twin (byte copies and re-muxes that keep the ID share it; a fresh mux from the same source gets a new ID). creation date + play duration + file size = same-film/twin.
  status: GUID verified; field layout memory

- path: Stream Properties (B7DC0791...), one per stream
  meaning: stream type GUID (Audio F8699E40, Video BC19EFC0), error correction type, time offset, stream number (7 bits), type-specific data: video = BITMAPINFOHEADER (width, height, biCompression FourCC), audio = WAVEFORMATEX (wFormatTag, channels, sample rate, bitrate)
  front_of_file: yes
  linking_value: none (codec)
  status: GUID verified; layout memory

- path: Content Description (75B22633...)
  meaning: 5 length-prefixed UTF-16LE strings: Title, Author, Copyright, Description, Rating
  front_of_file: yes
  linking_value: alt (Title + Author). Rating, Copyright often unset.
  status: GUID verified; layout memory

- path: Extended Content Description (D2D0A440...)
  meaning: name/value descriptor list. Typical names: 'WM/Genre', 'WM/Year', 'WM/AlbumArtist', 'WM/Picture', 'WM/EncodingSettings' (encoder build and settings string, e.g. 'Lavf', 'Windows Media Encoder 9 Series'), 'WM/Encoder', 'WMFSDKVersion'. Value types: UNICODE, BOOL, DWORD, QWORD, WORD, BINARY.
  front_of_file: yes
  linking_value: alt (WM/EncodingSettings + WM/Encoder = same encode settings and build, strong alt). WM/Picture binary may be large.
  status: GUID verified; names memory

- path: Codec List (86D15240...)
  meaning: codec entries with type (video / audio / unknown), name (UTF-16) and description, e.g. 'Windows Media Video 9.1', 'Windows Media Audio 9.2', 'VC-1', 'MPEG-4' (memory)
  front_of_file: yes
  linking_value: alt (codec name+version)
  status: GUID verified; contents memory

- path: Padding Object (1806D474...)
  meaning: zero padding placed after header so the header can be rewritten in place. Size is a fingerprint of the writer tool (fixed sizes like 4096 or 8192 bytes per tool).
  front_of_file: yes
  linking_value: alt (padding size per writer)
  status: GUID verified; size pattern memory

- path: Data Object (75B22636...) and packet size in File Properties
  meaning: fixed-size data packets (typically 2048 or 4096 bytes)
  front_of_file: data starts after header
  linking_value: none
  status: GUID verified

- path: Simple Index Object (33000890...) and Index Object (D6E229D3...)
  meaning: seek tables. Simple index at end (tail read), legacy index in some files.
  front_of_file: no (end of file, tail read)
  linking_value: none
  status: GUID verified

---

## 6. MPEG-PS / VOB (DVD-Video)

- path: pack header start code 0x000001BA (MPEG-2 program stream pack header)
  meaning: SCR (33-bit base + 9-bit extension, 27 MHz), program_mux_rate (22 bits, 50-byte units)
  front_of_file: yes, every ~2 KB pack
  linking_value: none
  status: memory

- path: system header 0x000001BB
  meaning: rate_bound, audio_bound, video_bound, fixed_flag, CSPS flag, per-stream P-STD buffer sizes (stream_id + buffer scale/size)
  front_of_file: yes (after first pack header, usually first VOBU)
  linking_value: alt (buffer sizes per stream = muxer fingerprint)
  status: memory

- path: PES stream IDs: video 0xE0 to 0xEF, audio 0xC0 to 0xDF, private_stream_1 0xBD, private_stream_2 0xBF (nav packs)
  front_of_file: yes
  status: memory

- path: private_stream_1 sub_stream_id (first byte after PES header)
  meaning: 0x20 to 0x3F subpicture, 0x80 to 0x87 AC-3, 0x88 to 0x8F DTS, 0xA0 to 0xA7 LPCM. (DVD-Video, memory)
  front_of_file: yes
  linking_value: none
  status: memory

- path: nav packs in private_stream_2 (0xBF): PCI (sub_id 0x00) and DSI (sub_id 0x01), at the start of each VOBU
  meaning: PCI: button/highlight info, VOBU start/end PTM and sector addresses. DSI: VOBU sector addresses (NV_PCK_LBN, VOBU_EA), SRI navigation links, seek info. The DSI's NV_PCK_LBN / cell start PTMs identify the disc position.
  front_of_file: first nav pack at VOB start; then one per VOBU (so PCI/DSI appear throughout the file)
  linking_value: twin (same disc rip: identical VOBU sector layout), same-film (cell timings, PGC duration)
  status: memory

- path: VIDEO_TS.IFO / VTS_xx_0.IFO (sibling files, not inside VOB)
  meaning: title/PGC tables, cell durations, audio and subtitle stream tables with language codes. Strongest disc-level metadata for DVD rips.
  front_of_file: n/a (separate file; if present in the same release it links the VOBs)
  linking_value: same-film (title set + PGC duration), twin (same disc)
  status: memory

---

## Summary of linking strength by field (for twin/alt matching)

- twin (byte-identical or re-mux identity): ASF File ID GUID; Ogg per-stream serial and page checksums; DVD DSI sector layout; FLV exact filesize.
- alt (same encoder build or settings): AVI ISFT + ICRD + JUNK text; WM/EncodingSettings + WM/Encoder; FLV encoder + metadatacreator; Vorbis vendor string; Dolby DOVI profile+level+compat id (weak, per source); padding size per writer.
- same-film (duration, dims, frames): AVI dwTotalFrames + dwRate/dwScale + dims; FLV duration + width/height; ASF play duration; DVD PGC duration.
- same-broadcast (TS only): DVB EIT p/f event name + start time; SDT service name; TDT/TOT capture time; NIT orbital position + frequency; ATSC VCT short name + channel number.
- none: stream-type and codec tables; per-stream language codes; wFormatTag / fourcc alone.

## Open items
- Dolby Vision spec not retrieved; DOVI beyond ffmpeg's parse unverified.
- crates.io blocked from sandbox: crate coverage (mpeg2ts-reader feature list, a DVB SI crate, an AVI or FLV crate) not checked.
- OpenDML 1.02 and Microsoft AVI reference not fetched; AVI field offsets are memory.
- OGM layout from memory; no fetched source.
