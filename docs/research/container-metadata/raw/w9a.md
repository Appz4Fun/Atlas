# w9a change log (catalog verification against corpus)

Scope: partial run inside the 10-minute limit. Scripts in /tmp/w9a (ebml_scan.py, mp4_scan.py, apply.py).

## Corpus used
MKV: a_hdr10_lavf, a_hdr10_mkvmerge, b_multitrack, b_truehd, b_video, d_fonts, e_baseline_lavf (/tmp/w5a/corpus); dv5, dv81, ff81 (/tmp/w7a).
MP4: c_mp4_faststart, c_mp4_nofaststart (/tmp/w5a/corpus); dv5_strict, dv81_dvh1, dv81_nostrict (/tmp/w7a).

## Findings
- Lavf writes Tags before the first Cluster (front). mkvmerge writes Tags, Cues at the tail. Cues are at the tail in all files. Tags stay front_or_tail with cost tail.
- Chapters and Attachments (FileData) sit before the first Cluster in b_multitrack (~420 KB) and d_fonts (~2.3 MB). Cost first_4mb is right.
- mkv.tag.encoder: location changed from front to front_or_tail (both writers observed).

## mkv.yaml
- Verified before: spec 132, memory 15, tested 80, source 32.
- Verified after: spec 86, memory 15, tested 126, source 32.
- 46 entries moved from spec to tested with source appended "; corpus:<files>" (EBML ID found in the corpus).
- Memory entries were not promoted. Their IDs did not appear in the corpus (dvcc.compat_id, emsg-like tags, etc.).

## mp4.yaml
- No changes. The walker reached stsd children (avc1, hev1, dvh1, mp4a, ec-3) but no spec/memory stsd entry was present in the corpus besides already-tested ones.
- Not done: moov field checks (mvhd, tkhd, mdhd, ilst, dvcC, hvcC, colr) and sample-entry descent (dvcC/avcC inside avc1/hev1/dvh1). The walker does not yet descend into sample entries.

## disc_other.yaml
- udf.avdp.location and udf.avdp.main_vds_extent: cost first_4mb -> first_article (AVDP at 512 KB, VDS at sector 32).
- dvd.vmg.magic (VIDEO_TS.IFO): cost -> first_article, location_note records genisoimage offset 567296.
- dvd.discid (VTS_01_0.IFO): cost -> targeted, location_note records offset 632832 and that later VTS IFOs are ~21 MB in.
- BDMV entries kept targeted, per instruction.
- Verified counts unchanged (cost-only edits).

## Not done
- Spot-check of 15 verified entries against raw notes: not performed.
- DVD ISO rebuild per raw/w6a.md: not performed.
- The EBML walker counts each element twice (n=2). Presence and position are correct; counts are not used.
