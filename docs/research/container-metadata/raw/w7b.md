# W7b: libdvdread DVDDiscID and volume info, multi-VTS, home-made ISO fingerprint (Atlas)

Scope: verify with the real libdvdread 6.1.3 (apt `libdvdread-dev`, built from C) the DVDDiscID algorithm, the volume-id fields, and multi-title-set behaviour on locally authored DVD-Video ISOs. Also the UDF and ISO9660 identifiers genisoimage writes. Blu-ray: not done (see section 4).
Work dir: /tmp/w7b (C in /tmp/w7b/discid.c, binary /tmp/w7b/discid; Python stdlib only, run with `python3 -I`, scripts in /tmp/w7b/py/).

## Verdict

| Item | Status |
|---|---|
| DVDDiscID = MD5 over VIDEO_TS.IFO then VTS_nn_0.IFO (nn = 1..nr_of_title_sets), whole files, no separators | CONFIRMED with libdvdread on 1-VTS and 2-VTS discs |
| w6a candidate `918d26bab2dcf09adf08a37da9267d21` for disc_gen.iso | CONFIRMED (libdvdread returns the same value) |
| Multi-VTS: second VTS IFO is included in the hash | CONFIRMED |
| DVDUDFVolumeInfo / DVDISOVolumeInfo volume id and volume set id | CONFIRMED (fields below) |
| Preparer | NOT exposed by libdvdread; present only as a raw ISO9660 PVD field, empty on these discs |
| Blu-ray (tsmuxer authoring, MPLS/CLPI parse) | NOT DONE: tsmuxer is not in apt |
| Pressed-disc fingerprint | NOT DONE: no pressed disc here |

## 0. Environment

- `apt-get install -y libdvdread-dev` installed libdvdread-dev 6.1.3-1.1build1 (headers in /usr/include/dvdread). gcc, dvdauthor, genisoimage, xorriso and ffmpeg were already present.
- libdvdread prints a banner "Encrypted DVD support unavailable" (no libdvdcss). It is harmless for unencrypted samples, and it means CSS discs were not tested.
- Two-title-set disc: /tmp/w7b/dvd2/dvd.xml has two `<titleset>` blocks (12 s and 30 s PAL MPEG-2, 2 x AC-3 en/fr each). Built with `dvdauthor -o dvd_out -x dvd.xml` then `genisoimage -quiet -dvd-video -V ATLAS_DVD2 -udf -o disc2.iso dvd_out` (29,952,000 bytes).

## 1. DVDDiscID (libdvdread DVDDiscID, C test in /tmp/w7b/discid.c)

| Image | libdvdread DVDDiscID | Python MD5 (order: VMG, VTS_01..n) |
|---|---|---|
| disc_gen.iso (1 title set, from w6a) | 918d26bab2dcf09adf08a37da9267d21 | 918d26bab2dcf09adf08a37da9267d21 (match) |
| disc2.iso (2 title sets) | 15dcd45ff5b2267ca94f1547f02a7f33 | 15dcd45ff5b2267ca94f1547f02a7f33 (match) |
| disc2 dvd_out/ directory (not ISO) | 15dcd45ff5b2267ca94f1547f02a7f33 | n/a (same value, so DVDOpen on a directory and on an ISO agree) |

Python check (/tmp/w7b/py/discid_check.py) on disc2, alternatives that do NOT match:
- reversed order (VTS_02, VTS_01, VMG): `9ac7f683...`
- VMG only: `f7935120...`
- VTS_01..n only (no VMG): `bb958681...`
- VMG + VTS_01 only (ignoring VTS_02): `e4144642...`

Algorithm, as confirmed (matches the w3b §6 summary of libdvdread):
1. VMG nr_of_title_sets = N (2 for disc2, 1 for disc_gen).
2. MD5 over `VIDEO_TS.IFO || VTS_01_0.IFO || ... || VTS_0N_0.IFO`, each file whole, in that order, no separators. No BUP, no VOB.
3. Output is the 16-byte digest, printed as lowercase hex.

Caveats still open:
- Every IFO here is exactly 12,288 bytes (a multiple of 2048). libdvdread reads each file in whole 2048-byte blocks, so a non-sector-aligned IFO could hash padding bytes. Not tested.
- The 10-title-set cap in w3b was not tested (only 1 and 2 title sets).
- Not tested on CSS-encrypted discs, real pressed discs, or discs with menus other than the VMG menu PGC.

Where the IFOs sit (matters for the parser, since a fixed-LBA parser would miss them):

| File | disc_gen.iso LBA (byte) | disc2.iso LBA (byte) |
|---|---|---|
| VIDEO_TS/VIDEO_TS.IFO | 277 (567,296) | 280 (573,440) |
| VIDEO_TS/VTS_01_0.IFO | 309 (632,832) | 312 (638,976) |
| VIDEO_TS/VTS_02_0.IFO | n/a | 10413 (21,325,824) |
| VIDEO_TS/VTS_01_1.VOB | 315 (645,120) | 318 (651,264) |
| VIDEO_TS/VTS_02_1.VOB | n/a | 10419 (21,338,112) |

Implication: for a 2-title-set disc the second VTS IFO is at about 21 MB, far outside the first 700 KB or 4 MiB. The DVDDiscID input can only be read by walking ISO9660 directory records (or UDF file entries), not from fixed offsets. The helper `find()` in /tmp/w7b/py/isoread.py does that.

## 2. Volume identifiers (libdvdread DVDISOVolumeInfo and DVDUDFVolumeInfo)

Both calls return the volume id on both ISOs.

| Field | disc_gen.iso | disc2.iso | Where in the image |
|---|---|---|---|
| Volume id (ISO side, DVDISOVolumeInfo) | ATLAS_DVD | ATLAS_DVD2 | ISO9660 PVD bytes 40..71 |
| Volume set id (ISO side) | "" (blank, 128 spaces) | "" (blank) | ISO9660 PVD bytes 190..317 |
| Volume id (UDF, DVDUDFVolumeInfo) | ATLAS_DVD | ATLAS_DVD2 | UDF PVD volumeIdentifier, LVD logicalVolumeIdentifier |
| Volume set id (UDF, DVDUDFVolumeInfo) | 6AE8392A00001C38 | 6AE83AA3000018AA | UDF PVD volumeSetIdentifier (128 bytes, dstring) |
| Preparer | "" | "" | ISO9660 PVD bytes 446..573. No UDF field and no libdvdread accessor |
| Publisher | "" | "" | ISO9660 PVD bytes 318..445 |

Notes:
- DVDISOVolumeInfo returns a blank volume set id on these discs, while DVDUDFVolumeInfo returns a non-empty 16-hex-digit value that genisoimage writes into the UDF PVD. For this reason, a parser that reads only ISO9660 bytes 190..317 gets an empty value. The UDF value is the one libdvdread prefers. How genisoimage derives the 16-hex-digit string was not checked.
- The UDF LVInfo1..3 fields are empty. genisoimage did not get a `-p` preparer, so the preparer field stays empty. A preparer test needs a build with `-p` (not done).

## 3. ISO9660 and UDF identifiers written by genisoimage (home-made ISO fingerprint)

ISO9660 PVD, same on both discs:
- Application identifier (bytes 574..701): `GENISOIMAGE ISO 9660/HFS FILESYSTEM CREATOR (C) 1993 E.YOUNGDALE (C) 1997-2006 J.PEARSON/J.SCHILLING (C) 2006-2007 CDRKIT TEAM`
- Publisher and preparer: empty.
- Volume space size: 10,561 sectors (disc_gen), 14,625 sectors (disc2), matching the file sizes.
- Creation timestamp (bytes 813..829): 17 chars of date, time and hundredths, then a GMT offset byte 0x00 (e.g. `2026110207180200`).

UDF (ECMA-167 / OSTA), same pattern on both discs:
- AVDP at sector 256, MVDS at LBA 32 (length 32,768 bytes). Terminator at offset 10,240 in the VDS.
- Primary VD: implementation id `*genisoimage` (regid suffix all zeros). Recording time: 2026-11-02 in both (from the same run).
- IUVD implementation id `*UDF LV Info`, with LVI implementation id `*genisoimage`.
- Partition descriptor: partition 0, start LBA 257 (both discs), length 10,154 (disc_gen) / 14,218 (disc2). Contents regid `+NSR02`; implementation id `*genisoimage`.
- LVD: logical block size 2048; domain id `*OSTA UDF Compliant` with suffix bytes `02 01 03 00 00 00 00 00` (raw, not decoded against the spec); implementation id `*genisoimage`.

Fingerprint for "made here with genisoimage/cdrkit": UDF implementation ids `*genisoimage` in PVD, LVI, PD and LVD, plus the ISO9660 application id starting `GENISOIMAGE`, plus a blank publisher and preparer.

Pressed-disc differences: NOT verified. We have no pressed disc, so we cannot say what the mastering houses write (publisher, preparer, implementation ids). Treat any such assumption as a hypothesis.

## 4. Blu-ray

- `apt-cache policy tsmuxer` and `apt-cache search tsmuxer` returned nothing in this distro. tsmuxer is not available from apt, so no BD folder was authored (the task rule was not to download binaries).
- apt does offer libbluray-bin and libbluray-dev, but no BD sample was authored, so index.bdmv, PLAYLIST/*.mpls and CLIPINF/*.clpi were not parsed. The Blu-ray step is open.

## Files

- /tmp/w7b/discid.c (libdvdread test: DVDOpen, DVDDiscID, DVDISOVolumeInfo, DVDUDFVolumeInfo)
- /tmp/w7b/py/isoread.py (ISO9660 path lookup and PVD fields, stdlib)
- /tmp/w7b/py/discid_check.py (MD5 ordering check)
- /tmp/w7b/py/udf_vds.py (UDF VDS dump: PVD, IUVD, PD, LVD, implementation ids)
- /tmp/w7b/dvd2/dvd.xml (two-title-set authoring source), /tmp/w7b/dvd2/disc2.iso, /tmp/w7b/dvd2/dvd_out/
