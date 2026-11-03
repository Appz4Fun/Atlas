# w5a: Atlas media probe vs ffprobe on prefix-cut files

Probe under test: `tools/atlas_media_a29ee52.rs` (copy of `src/probe/media.rs` at a29ee52). Harness: `tools/probe-harness/` (Cargo.toml + main.rs; needs `src/media.rs` = a copy of the parser). Prefixes: 700,000 B, 4,000,000 B, full. Oracle: `ffprobe -show_format -show_streams -show_chapters`.
Time-boxed; 7 corpus files produced, not the 8 asked for. Missing: any DV or HDR10+ sample (no dovi_tool or hdr10plus_tool installed, and ffmpeg 6.1 cannot write DV).

## Corpus and commands

Scripts run in /tmp/w5a (`corpus.sh`, then `corpus_fix.sh` for the TrueHD step that failed). Verbatim:

```bash
#!/bin/bash
# Corpus for the Atlas probe measurement (w5a). Run from /tmp/w5a/corpus.
# no set -e: mkvmerge exits 1 on warnings
cd /tmp/w5a/corpus
F="-hide_banner -loglevel error -y"
FONT=/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf
FONTB=/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf

# ---- shared sources
ffmpeg $F -f lavfi -i "mandelbrot=s=1920x1080:r=24" -t 30 -c:v libx264 -preset veryfast \
  -pix_fmt yuv420p -b:v 20M -maxrate 25M -bufsize 40M -an b_video.mkv
ffmpeg $F -f lavfi -i "anoisesrc=d=30:c=pink:r=48000:a=0.2" -ac 6 -c:a eac3 -b:a 640k b_eac3.eac3
ffmpeg $F -f lavfi -i "anoisesrc=d=30:c=pink:r=48000:a=0.2" -ac 8 -channel_layout 7.1 -c:a truehd b_truehd.mkv
ffmpeg $F -f lavfi -i "anoisesrc=d=30:c=pink:r=48000:a=0.2" -ac 2 -strict -2 -c:a dca -b:a 1509k -f dts b_dts.dts
ffmpeg $F -f lavfi -i "anoisesrc=d=30:c=pink:r=48000:a=0.2" -ac 2 -c:a aac -b:a 192k a_aac.m4a
ffmpeg $F -f lavfi -i "color=c=blue:s=600x900" -frames:v 1 cover.jpg
cp "$FONT" font.ttf
cp "$FONTB" font2.ttf
# big fonts for d: pad to >2 MB of attachments
for i in 1 2 3; do cp "$FONT" "bigfont$i.ttf"; done
dd if=/dev/urandom of=bigfont4.ttf bs=1M count=1 2>/dev/null
cat > sub_eng.srt <<'EOF'
1
00:00:01,000 --> 00:00:04,000
Hello there.

2
00:00:05,000 --> 00:00:09,000
General Kenobi.
EOF
cat > sub_forced.srt <<'EOF'
1
00:00:12,000 --> 00:00:15,000
[ speaks Latin ]
EOF
cat > sub_sdh.srt <<'EOF'
1
00:00:01,000 --> 00:00:04,000
[ door creaks ]

2
00:00:20,000 --> 00:00:25,000
(MUSIC PLAYING)
EOF
cat > sub_ass.ass <<'EOF'
[Script Info]
ScriptType: v4.00+
PlayResX: 1920
PlayResY: 1080

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Liberation Sans,60,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,3,1,2,10,10,40,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:02.00,0:00:06.00,Default,,0,0,0,,{\fnLiberation Sans}Styled line
Dialogue: 0,0:00:10.00,0:00:14.00,Default,,0,0,0,,Second line
EOF
cat > ch.xml <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE Chapters SYSTEM "matroskachapters.dtd">
<Chapters>
  <EditionEntry>
    <EditionFlagHidden>1</EditionFlagHidden>
    <EditionFlagDefault>0</EditionFlagDefault>
    <ChapterAtom>
      <ChapterTimeStart>00:00:00.000000000</ChapterTimeStart>
      <ChapterDisplay><ChapterString>HIDDEN-ED Start</ChapterString><ChapterLanguage>eng</ChapterLanguage></ChapterDisplay>
    </ChapterAtom>
    <ChapterAtom>
      <ChapterTimeStart>00:00:07.000000000</ChapterTimeStart>
      <ChapterDisplay><ChapterString>HIDDEN-ED Middle</ChapterString><ChapterLanguage>eng</ChapterLanguage></ChapterDisplay>
    </ChapterAtom>
  </EditionEntry>
  <EditionEntry>
    <EditionFlagDefault>1</EditionFlagDefault>
    <EditionFlagOrdered>1</EditionFlagOrdered>
    <ChapterAtom>
      <ChapterTimeStart>00:00:00.000000000</ChapterTimeStart>
      <ChapterDisplay><ChapterString>Opening</ChapterString><ChapterLanguage>eng</ChapterLanguage></ChapterDisplay>
    </ChapterAtom>
    <ChapterAtom>
      <ChapterTimeStart>00:00:10.000000000</ChapterTimeStart>
      <ChapterDisplay><ChapterString>Act Two</ChapterString><ChapterLanguage>eng</ChapterLanguage></ChapterDisplay>
    </ChapterAtom>
    <ChapterAtom>
      <ChapterTimeStart>00:00:15.000000000</ChapterTimeStart>
      <ChapterFlagHidden>1</ChapterFlagHidden>
      <ChapterDisplay><ChapterString>Hidden atom</ChapterString><ChapterLanguage>eng</ChapterLanguage></ChapterDisplay>
    </ChapterAtom>
    <ChapterAtom>
      <ChapterTimeStart>00:00:20.000000000</ChapterTimeStart>
      <ChapterDisplay><ChapterString>End</ChapterString><ChapterLanguage>eng</ChapterLanguage></ChapterDisplay>
    </ChapterAtom>
  </EditionEntry>
</Chapters>
EOF
cat > tags.xml <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE Tags SYSTEM "matroskatags.dtd">
<Tags>
  <Tag>
    <Targets><TargetTypeValue>50</TargetTypeValue></Targets>
    <Simple><Name>IMDB</Name><String>tt0000001</String></Simple>
    <Simple><Name>TMDB</Name><String>12345</String></Simple>
    <Simple><Name>ENCODER_SETTINGS</Name><String>preset=veryfast</String></Simple>
  </Tag>
</Tags>
EOF

# ---- a) HDR10 2160p HEVC 10-bit, Lavf-muxed, then mkvmerge remux
ffmpeg $F -f lavfi -i "mandelbrot=s=3840x2160:r=24" -t 20 -c:v libx265 -preset veryfast \
  -pix_fmt yuv420p10le -b:v 20M -maxrate 25M -bufsize 50M -an \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv \
  -x265-params "hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400" \
  a_hdr10_lavf.mkv
mkvmerge -q -o a_hdr10_mkvmerge.mkv --title "HDR10 Test (2026)" a_hdr10_lavf.mkv

# ---- b) multi-track MKV: E-AC-3 5.1, TrueHD 7.1, DTS Commentary, 4 subtitle tracks, 2 editions, cover, tags
mkvmerge -q -o b_multitrack.mkv \
  --title "Test Film (2001)" --chapter-language eng --chapters ch.xml --global-tags tags.xml \
  --attachment-name cover.jpg --attachment-mime-type image/jpeg --attach-file cover.jpg \
  --track-name 0:"Main Feature" --language 0:eng b_video.mkv \
  --language 0:eng --track-name 0:"English 5.1" --default-track-flag 0:yes b_eac3.eac3 \
  --language 0:eng --track-name 0:"TrueHD 7.1 Atmos" b_truehd.mkv \
  --language 0:eng --track-name 0:"Director Commentary" --commentary-flag 0:yes b_dts.dts \
  --language 0:eng --track-name 0:"English" sub_eng.srt \
  --language 0:eng --track-name 0:"Styled" --attachment-name font.ttf --attachment-mime-type application/x-truetype-font --attach-file font.ttf sub_ass.ass \
  --language 0:eng --track-name 0:"Forced" --forced-display-flag 0:yes sub_forced.srt \
  --language 0:eng --track-name 0:"English SDH" --hearing-impaired-flag 0:yes sub_sdh.srt

# ---- c) MP4: H.264 + AAC + E-AC-3 with chapters and title, without and with faststart
cat > meta.txt <<'EOF'
;FFMETADATA1
title=Test Film (2001)
[CHAPTER]
TIMEBASE=1/1000
START=0
END=10000
title=Opening
[CHAPTER]
TIMEBASE=1/1000
START=10000
END=30000
title=Act Two
EOF
ffmpeg $F -i b_video.mkv -i b_eac3.eac3 -i a_aac.m4a -i meta.txt -map 0:v -map 1:a -map 2:a \
  -map_metadata 3 -map_chapters 3 -c:v copy -c:a:0 copy -c:a:1 copy -metadata title="Test Film (2001)" \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=eng c_mp4_nofaststart.mp4
ffmpeg $F -i b_video.mkv -i b_eac3.eac3 -i a_aac.m4a -i meta.txt -map 0:v -map 1:a -map 2:a \
  -map_metadata 3 -map_chapters 3 -c:v copy -c:a:0 copy -c:a:1 copy -metadata title="Test Film (2001)" \
  -metadata:s:a:0 language=eng -metadata:s:a:1 language=eng -movflags +faststart c_mp4_faststart.mp4

# ---- d) MKV with >2 MB of font attachments
mkvmerge -q -o d_fonts.mkv \
  --title "Test Film (2001)" --chapter-language eng --chapters ch.xml --global-tags tags.xml \
  --attachment-name cover.jpg --attachment-mime-type image/jpeg --attach-file cover.jpg \
  --attachment-name bigfont1.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont1.ttf \
  --attachment-name bigfont2.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont2.ttf \
  --attachment-name bigfont3.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont3.ttf \
  --attachment-name bigfont4.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont4.ttf \
  --track-name 0:"Main Feature" --language 0:eng b_video.mkv \
  --language 0:eng --track-name 0:"English 5.1" b_eac3.eac3 \
  --language 0:eng --track-name 0:"English" sub_eng.srt \
  --language 0:eng --track-name 0:"Styled" sub_ass.ass

# ---- e) plain baseline MKV from Lavf (AAC stereo, no chapters) for comparison
ffmpeg $F -i b_video.mkv -i a_aac.m4a -map 0:v -map 1:a -c copy -metadata title="Baseline" e_baseline_lavf.mkv
ls -la /tmp/w5a/corpus

```

```bash
#!/bin/bash
# Re-run for the steps that failed in corpus.sh: b_truehd.mkv was empty (ffmpeg 6.1 needs -strict -2 and an explicit 7.1 pan; -ac 8 -channel_layout 7.1 from mono fails with Invalid argument).
cd /tmp/w5a/corpus
F="-hide_banner -loglevel error -y"
ffmpeg $F -f lavfi -i "anoisesrc=d=30:c=pink:r=48000:a=0.2" -af "pan=7.1|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0|c6=c0|c7=c0" -strict -2 -c:a truehd b_truehd.mkv
# ---- b) multi-track MKV: E-AC-3 5.1, TrueHD 7.1, DTS Commentary, 4 subtitle tracks, 2 editions, cover, tags
mkvmerge -q -o b_multitrack.mkv \
  --title "Test Film (2001)" --chapter-language eng --chapters ch.xml --global-tags tags.xml \
  --attachment-name cover.jpg --attachment-mime-type image/jpeg --attach-file cover.jpg \
  --track-name 0:"Main Feature" --language 0:eng b_video.mkv \
  --language 0:eng --track-name 0:"English 5.1" --default-track-flag 0:yes b_eac3.eac3 \
  --language 0:eng --track-name 0:"TrueHD 7.1 Atmos" b_truehd.mkv \
  --language 0:eng --track-name 0:"Director Commentary" --commentary-flag 0:yes b_dts.dts \
  --language 0:eng --track-name 0:"English" sub_eng.srt \
  --language 0:eng --track-name 0:"Styled" --attachment-name font.ttf --attachment-mime-type application/x-truetype-font --attach-file font.ttf sub_ass.ass \
  --language 0:eng --track-name 0:"Forced" --forced-display-flag 0:yes sub_forced.srt \
  --language 0:eng --track-name 0:"English SDH" --hearing-impaired-flag 0:yes sub_sdh.srt

# ---- d) MKV with >2 MB of font attachments
mkvmerge -q -o d_fonts.mkv \
  --title "Test Film (2001)" --chapter-language eng --chapters ch.xml --global-tags tags.xml \
  --attachment-name cover.jpg --attachment-mime-type image/jpeg --attach-file cover.jpg \
  --attachment-name bigfont1.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont1.ttf \
  --attachment-name bigfont2.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont2.ttf \
  --attachment-name bigfont3.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont3.ttf \
  --attachment-name bigfont4.ttf --attachment-mime-type application/x-truetype-font --attach-file bigfont4.ttf \
  --track-name 0:"Main Feature" --language 0:eng b_video.mkv \
  --language 0:eng --track-name 0:"English 5.1" b_eac3.eac3 \
  --language 0:eng --track-name 0:"English" sub_eng.srt \
  --language 0:eng --track-name 0:"Styled" sub_ass.ass


```

Also run by hand for the MP4 files (`meta.txt` = the FFMETADATA block inside corpus.sh, written once; the c-step ffmpeg commands were re-run after corpus.sh had not reached them yet, with identical arguments). Probe + oracle: `probe-harness FILE...` and `ffprobe -v error -show_format -show_streams -show_chapters -of json FILE`.

## Results (what Atlas reports at 700K / 4M / full)

| Field | ffprobe | Atlas 700K | Atlas 4M | Atlas full | Verdict |
|---|---|---|---|---|---|
| Title (Segment Info / MP4 udta) | Test Film (2001) / title tag | yes (MKV) / no (MP4 title) | same | same | MKV ok; MP4 title missing |
| Duration | 30.016 s | 30.016 | 30.016 | 30.016 | ok (MP4 non-faststart: None until full) |
| Video codec, WxH | h264 1920x1080 | ok | ok | ok | ok |
| Frame rate | 24 | 24.000000384 | same | same | ok (MP4: None, not parsed) |
| Bit depth (HEVC 10-bit) | yuv420p10le | None | None | None | MISS: Lavf and mkvmerge write no BitsPerChannel |
| HDR10 label (PQ + bt2020 + master-display SEI) | smpte2084 | `PQ` | `PQ` | `PQ` | WRONG: no MasteringMetadata element in container, so HDR10 becomes PQ |
| MaxCLL / MaxFALL, mastering values | in SEI only | not read | not read | not read | MISS (parser ignores 0x55BC/0x55BD and 0x55D1..) |
| Dolby Vision / HDR10+ | none in corpus | n/a | n/a | n/a | UNTESTED |
| E-AC-3 channels, rate | 6 ch, 48 kHz, 640 kb/s | 6, 48000 | same | same | ok; bitrate MISS |
| TrueHD name "Atmos" | Atmos only in name | atmos=true | same | same | name-based only (bitstream JOC not read) |
| DTS flagged commentary | disposition.comment=1 | commentary=true | same | same | ok |
| Audio default flag | 1 on E-AC-3 | MISS | MISS | MISS | parser has no FlagDefault (0x88) |
| Video track name ("Main Feature") | yes | MISS | MISS | MISS | no name on VideoTrack |
| SRT/ASS names, forced, SDH | yes | ok | ok | ok | ok (SDH from name or flag) |
| Subtitle default / language | ok | ok | ok | ok | ok |
| Attachments (4 TTF fonts, cover.jpg) | attachment streams | MISS | MISS | MISS | no Attachments parsing |
| Cover art (attached_pic) | mjpeg 600x900 | MISS | MISS | MISS | same as above |
| Chapters, 2 editions (ed 1 hidden, ed 2 default+ordered) | ffprobe lists all 5 | `HIDDEN-ED Start`, `HIDDEN-ED Middle` | same | same | BUG: takes the first edition with chapters, ignores EditionFlagHidden / EditionFlagDefault; hidden atom ChapterFlagHidden is correctly dropped |
| Global tags (IMDB, TMDB, ENCODER_SETTINGS) | tt0000001, 12345 | MISS | MISS | MISS | no Tags element parsing |
| Stats tags (mkvmerge `_STATISTICS_*`) | present in a_hdr10_mkvmerge | MISS | MISS | MISS | no Tags parsing |
| Writing app / muxing app | mkvmerge / Lavf | ok | ok | ok | ok |
| MP4 chapters (2) | Opening, Act Two | MISS | MISS | MISS | no chpl or chapter-track parsing |
| MP4 tracks (faststart) | avc1, ec-3, mp4a | ok | ok | ok | ok |
| MP4 tracks (non-faststart) | same | NONE (empty, header_complete=false) | NONE | ok | moov at file end: needs the whole file |

Prefix findings:
- `b_multitrack.mkv` (109 MB) is header-complete at 700K: all 3 audio, 4 subtitle tracks, and chapters are there.
- `d_fonts.mkv` (87 MB, 4 fonts about 2.2 MB, placed before Chapters): at 700K, header_complete=false, no chapters. Chapters first appear at 4M.
- `c_mp4_nofaststart.mp4` (86 MB): 700K and 4M give no tracks at all. Only the full file parses.
- Panics: none across 18 probe runs (harness `catch_unwind` also reports PANIC; none seen).

## Notes and caveats
- TrueHD sample: ffmpeg 6.1 needs `-strict -2`, and `-ac 8 -channel_layout 7.1` from a mono source fails with "Invalid argument". A `pan=7.1` filter works, but both ffprobe and Atlas report 5 channels for the result, so the 7.1 layout in that file is not reliable.
- The 2160p HEVC HDR10 encode (20 s, 20 Mb/s, veryfast) was the slowest step on 4 cores.
- ffprobe lists every edition's chapters merged; the Atlas comparison is against the first edition only, by design.
