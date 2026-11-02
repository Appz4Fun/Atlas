# w7a: DV test files, container-level DV signalling, Atlas parser check

Workdir: /tmp/w7a. Tools: dovi_tool 2.1.2 (GitHub release tarball, cargo has no dovi_tool crate), ffmpeg/ffprobe 6.1.1, mkvmerge v82.0, x265 via ffmpeg libx265.

## 1. Build test files

```
# dovi_tool: cargo install failed (no crate named dovi_tool; dolby_vision is a library only)
curl -sSL -o dovi.tgz https://github.com/quietvoid/dovi_tool/releases/download/2.1.2/dovi_tool-2.1.2-x86_64-unknown-linux-musl.tar.gz
tar xzf dovi.tgz -C /tmp/w7a/dovi_bin          # -> /tmp/w7a/dovi_bin/dovi_tool

# RPU config (rpu_cfg.json): length 240, one shot 0..240, Level1 + Level6 blocks
dovi_tool generate -j rpu_cfg.json -o RPU_81.bin          # Profile 8.1 (default)
dovi_tool generate -j rpu_cfg.json -p 5 -o RPU_5.bin      # Profile 5

# base layer: 10 s, 24 fps, 1920x1080 testsrc2, 10-bit BT.2020/PQ, HDR10 SEI
X="hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,1):max-cll=1000,400:log-level=error"
ffmpeg -y -f lavfi -i testsrc2=size=1920x1080:rate=24 -t 10 -vf format=yuv420p10le \
  -c:v libx265 -preset medium -crf 18 -x265-params "$X" \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -f hevc base.hevc   # 13,516,964 bytes

dovi_tool inject-rpu -i base.hevc --rpu-in RPU_81.bin -o dv81.hevc   # 13,554,887 bytes
dovi_tool inject-rpu -i base.hevc --rpu-in RPU_5.bin  -o dv5.hevc    # 13,554,647 bytes
```
Note: dovi_tool `info -i <hevc>` does not work on an HEVC file (it wants a binary RPU); `extract-rpu` + `info` does.

## 2. Mux

```
mkvmerge -o dv81.mkv dv81.hevc      # 13,565,234 B, rc 0 (output module 'HEVC/H.265 (unframed)')
mkvmerge -o dv5.mkv  dv5.hevc       # 13,564,994 B
ffmpeg -fflags +genpts -r 24 -i dv81.hevc -c copy -strict unofficial dv81_strict.mp4   # hev1, no dvcC
ffmpeg -fflags +genpts -r 24 -i dv81.hevc -c copy dv81_nostrict.mp4                    # hev1, no dvcC (same size as strict)
ffmpeg -fflags +genpts -r 24 -i dv5.hevc  -c copy -strict unofficial dv5_strict.mp4    # hev1, no dvcC
ffmpeg -tag:v dvh1 -i dv81.mkv -c copy dv81_dvh1.mp4                                    # dvh1 sample entry, STILL no dvcC/dvvC
ffmpeg -y -fflags +genpts -r 24 -i dv81.hevc -c copy ff81.mkv
#   -> FAILS: "Can't write packet with unknown timestamp", rc 234, and the output is
#      corrupt (ffmpeg reports duplicate elements / EOF). Raw HEVC into ffmpeg's matroska
#      muxer does not work here.
```

## 3. Container dumps

Script: /tmp/w7a/scripts/dump.py (python3 -I), walks EBML tracks/BlockAdditionMapping and MP4 stsd.

### MKV, mkvmerge (dv81.mkv), Tracks > TrackEntry
```
[0x1549a966] Info:  MuxingApp (0x4D80) = 'libebml v1.4.5 + libmatroska v1.7.1'
                    WritingApp (0x5741) = "mkvmerge v82.0 ('I'm The President') 64-bit"
TrackEntry: CodecID = 'V_MPEGH/ISO/HEVC', Video PixelWidth 1920, PixelHeight 1080
  BlockAdditionMapping (0x41E4)
    BlockAddIDName (0x41A4)  = 'Dolby Vision configuration'
    BlockAddIDType (0x41E7)  = 'dvvC'
    BlockAddIDExtraData (0x41ED) len=24:
      01 00 10 1d 10 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
    (no BlockAddIDValue element: default 1)
  CodecPrivate (0x63A2) len=2491 = hvcC (starts 01 02 20 00 00 00 90 ...), no dvcC inside
  No Colour (0x55B0) master, no BitDepth (0x55B2) element
```
Decode (dv81): version 1.0; byte2 0x10 -> profile = 0x10>>1 = 8, level bit5 = 0; byte3 0x1d -> level = (0x1d>>3)&0x1F = 3, rpu=1, el=0, bl=1; byte4 0x10 -> bl_signal_compatibility_id = 1.

### MKV, mkvmerge (dv5.mkv)
```
  BlockAddIDName = 'Dolby Vision configuration'
  BlockAddIDType = 'dvcC'
  BlockAddIDExtraData len=24: 01 00 0a 1d 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
```
Decode (dv5): profile = 0x0a>>1 = 5, level 3, rpu=1, el=0, bl=1, compat id 0.
So mkvmerge uses dvvC for profile 8 and dvcC for profile 5 (as the record types differ by profile).

### MKV, ffmpeg 6.1.1 ffprobe view of the same dv81.mkv
```
side_data_type=DOVI configuration record, dv_version 1.0, dv_profile=8, dv_level=3, rpu_present=1, el_present=0, bl_present=1, dv_bl_signal_compatibility_id=1
```
Matches the EBML record. ffprobe on dv5.mkv: dv_profile=5, compat 0.

### MP4 (ffmpeg output)
```
dv81_strict.mp4 / dv81_nostrict.mp4 / dv5_strict.mp4: sample entry 'hev1' size=266
   children: hvcC (125 B), colr (nclx, 6e 63 6c 78 00 09 00 10 00 09 00), pasp, btrt
   NO dvcC, NO dvvC
dv81_dvh1.mp4 (-tag:v dvh1): sample entry 'dvh1' size=2640
   children: hvcC (2499 B), colr, pasp, btrt
   NO dvcC, NO dvvC  (dvh1 tag without config box)
dv81_frommkv.mp4 (ffmpeg -i dv81.mkv -c copy): 'hev1', hvcC 2499 B, no dvcC
```
ffprobe on the MP4 outputs: no DOVI side data at all.
So ffmpeg 6.1.1's mov muxer never writes dvcC/dvvC here, even when the input MKV has DOVI config side data, and -strict unofficial makes no difference. ffmpeg's raw HEVC demuxer also attaches no DOVI side data.

Not tested: a real MP4 with dvcC (e.g. from MP4Box/gpac, not installed). So the MP4 side of the Atlas check below uses only ffmpeg output, which lacks dvcC.

## 4. Atlas harness (atlas_media_a29ee52.rs as src/media.rs, probe-harness main.rs, release build)

Build: /tmp/w7a/harness, `cargo build --release` OK (serde, serde_json from crates.io).
Run: `probe-harness dv81.mkv dv5.mkv dv81_dvh1.mp4 dv81_strict.mp4` -> /tmp/w7a/harness_out.jsonl
Prefixes: 700k, 4m, full.

| file | prefix | header_complete | dv_profile | dv_level | dv_enhancement_layer | hdr | bit_depth | primaries | transfer | fps | duration |
|---|---|---|---|---|---|---|---|---|---|---|---|
| dv81.mkv | 700k | true | 8 | 3 | false | null | null | null | null | 24.0 | 10.0 |
| dv81.mkv | 4m | true | 8 | 3 | false | null | null | null | null | 24.0 | 10.0 |
| dv81.mkv | full | true | 8 | 3 | false | null | null | null | null | 24.0 | 10.0 |
| dv5.mkv | 700k/4m/full | true | 5 | 3 | false | null | null | null | null | 24.0 | 10.0 |
| dv81_dvh1.mp4 | 700k, 4m | false | null | null | null | null | ... | | | | |
| dv81_dvh1.mp4 | full | true | null | null | null | null | null | null | null | null | 9.999 |
| dv81_strict.mp4 | full | true | null | null | null | null | null | null | null | null | 9.917 |
| dv81.hevc (raw) | all | info=null (not a container, expected) |

Full MP4 video entry (dv81_dvh1.mp4, full): codec "dvh1", width 1920, height 1080, all other fields null.

Reading the table: DV profile and level are correct for MKV. enhancement_layer false is correct (EL absent). HDR is wrong: the stream is PQ/BT.2020 (ffprobe: color_transfer=smpte2084, color_primaries=bt2020, color_space=bt2020nc), but Atlas returns hdr=null. The cause is that mkvmerge writes no Colour master, so the MKV path has nothing to read (hdr_name only fires on transfer 16 from 0x55BA). For the MP4 path Atlas never reads colr/hvcC at all, so hdr and bit depth stay null even though colr nclx carries transfer 16 in the file. bit_depth is null in MKV for the same reason (no 0x55B2).

No PANIC on any prefix. Atlas's code for the MP4 DV path reads only dvcC/dvvC (line ~548), so dvh1 without the box correctly gives dv_profile=null (not a parser bug, since the box is missing).

## 5. Notes
- MuxingApp from mkvmerge is 'libebml v1.4.5 + libmatroska v1.7.1' and WritingApp is "mkvmerge v82.0 ('I'm The President') 64-bit".
- mkvmerge detects DV and writes BlockAdditionMapping on the video track. Not tested: whether mkvmerge needs the RPU in the first frames (both files had RPU on every frame, so this is open).
- The MKV `ff81.mkv` attempt from raw HEVC failed (see section 2), so there is no ffmpeg-written MKV DV sample.
- Not done (time limit): MP4 with dvcC (needs gpac or mp4box); profile 7 / EL; HDR10+; the Atlas HDR path for HEVC SPS VUI.
