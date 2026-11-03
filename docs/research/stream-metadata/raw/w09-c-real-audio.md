# W09-C: real encoder output for TrueHD/MLP Atmos, E-AC-3 JOC, DTS-HD MA, DTS:X

Status: partial. Real TrueHD/MLP Atmos files found and run. No real E-AC-3 JOC, DTS-HD MA or DTS:X file was reachable. The DTS:X sync question is not resolved.

## Blocked (not retried)
samples.ffmpeg.org (the FFmpeg archive has the only real TrueHD and MLP files in encspect's manifest), fate-suite, archive.org, web.archive.org, and crates.io API (403). static.crates.io tarballs and raw GitHub clones work.

## Crate tarballs checked
- truehd 0.7.2 (Apache-2.0): no binary fixtures in the tarball. Its major-sync CRC is poly 0x2D, init 0, computed with a bit reader over the major sync info (src/structs/sync.rs:539, src/utils/crc.rs:28).
- bdinfo-rs-core 4.0.0, oxideav-ac3 0.0.1 (one stereo AC-3 sine, no value here), symphonia-codec-aac 0.5.4: no useful fixtures.
- oxideav-dts: not published (no tarball).

## Git repos (shallow clone)
- truehdd/truehdd (Apache-2.0), truehd/tests/assets: fba_atmos_obj.mlp (4.6 KB), fba_atmos_cbi.mlp, fba_atmos_dimtrim.mlp, fba_2ch.mlp, fba_192k.mlp, fba_192k_8ch.mlp, fba_spliced.mlp, fbb_6ch.mlp, fbb_copy.mlp. Source URL: https://github.com/truehdd/truehdd/tree/main/truehd/tests/assets. The repo does not say which encoder made them; CHANGELOG calls fba_atmos_dimtrim "a stream whose object metadata carries the dimensional trim", so it is treated as real. Not verified as Dolby output.
- smares/encspect (tests/fixtures): ffmpeg- and mkvmerge-made files plus synthetic "built-" and "joc-" files. Real Atmos, DTS-HD and DTS:X files are not committed (its docs say they are downloaded from samples.ffmpeg.org, which is blocked).
- domyd/mlp, iainchesworthlabs/iclforge (containers fuzz seeds, Apache/own licence, provenance unknown): no Atmos audio found.

## ffprobe (ground truth) on truehdd assets
- fba_atmos_obj, fba_atmos_cbi, fba_atmos_dimtrim, fba_192k_8ch: codec mlp, 8 ch, layout 7.1, 48 kHz (192 kHz for the 192k one), bits 24. Warning on each: "MLP only supports up to 2 substreams". ffprobe does not name Atmos.
- fba_2ch, fba_spliced: mlp stereo. fbb_6ch, fbb_copy: mlp, no major sync found by our parser (FBB form).

## Our parser runs (w06b_main.rs, major sync and CRC)
- Signature 0xB752 ok on every real file.
- Stream type byte is 0xBA (MLP sync F8726FBA), not 0xBB (TrueHD). Atmos files carry 3+ substreams (6ch mask 0x0F, 8ch mask 0x004F).
- Sync CRC: w06b reports "sync CRC ok: false" on all 9 real syncs in the Atmos and 2-ch files.
- Brute-force (w09c_main.rs, byte-aligned starts 0/4/8, ends 4..40, field after range, BE and LE): zero matches with poly 0x2D init 0 MSB-first. So the CRC byte order and range are unresolved. The truehd crate does the check with a bit-serial reader and may use a different range or bit alignment. Next step is to port the crate's exact range.

## Not resolved
- TrueHD Atmos rule disagreement (ffmpeg vs MediaInfo): not tested. The fba files are MLP, and ffprobe only warns.
- E-AC-3 JOC addbsi vs EMDF: no real JOC file. encspect's joc-*.eac3 and joc-*.mp4 are built, not encoder output.
- E-AC-3 7.1 with dependent substream: no real file.
- DTS-HD MA, DTS:X sync 0x02000850 and 0xF14000D0: no real file reachable. encspect's dtshd-*.dtshd are its own.

## Code
docs/research/stream-metadata/code/w09c_main.rs is the CRC brute-force probe (rustc -O, no deps). Usage: w09c_main <file.mlp> ...
