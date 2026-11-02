# W4c: Disc layout for cheap metadata (Blu-ray/UHD ISO, DVD ISO, RAR'd discs, MakeMKV/AnyDVD)

Legend: [V] verified from source/fetched text in this session. [D] single data point from a forum post. [M] memory, unverified.

## 0. Sources
- libudfread 1.2.0 source (archive.ubuntu.com/ubuntu/pool/universe/libu/libudfread/libudfread_1.2.0.orig.tar.xz), src/udfread.c, src/ecma167.h [V]
- libfreemkv docs/udf.md (docs.rs/crate/libfreemkv/0.8.0/source/docs/udf.md), fetched via WebFetch, offsets as quoted there [V-secondary]
- bdinfo-rs-core 4.0.0 src/bdrom/order.rs (docs.rs), fetched via WebFetch [V-secondary]
- ImgBurn forum thread "BDMV and the physical layout of discs" (forum.imgburn.com/topic/26193), one sector listing [D]
- raw.githubusercontent.com UniqProject/BDInfo/master/BDInfo/BDROM.cs and TSPlaylistFile.cs returned 404 at those paths. The GitHub API is blocked for this session. Use the zoffline/BDInfoCLI-ng mirror next time. [V: fetch failed]
- code.videolan.org and the libbluray source were not fetched. [not done]

## 1. Blu-ray / UHD ISO physical layout (UDF 2.50/2.60)

Read-order chain, all LBAs are 2048-byte sectors [V, udfread.c]:
1. Volume recognition: BEA01 / NSR02 or NSR03 descriptors are scanned from LBA 16 to 255 (udfread `_check_volume_recognition`). [V]
2. AVDP (Anchor Volume Descriptor Pointer, tag 2): at LBA 256 (byte 524288). Fallback: last sector minus 256. [V]
   - From AVDP: MVDS (main VDS) extent and RVDS (reserve VDS) extent. [V]
3. VDS (Main VDS): scanned from the MVDS location. Contains PVD (tag 1), LVD (tag 6), PD (tag 5), terminator (tag 8). [V]
   - Partition Descriptor (tag 5): `partitionStartingLocation` at byte 188 (u32), so the physical partition starts there. udfread uses `pd.start_block`. [V-secondary, libfreemkv; matches ECMA-167]
   - LVD (tag 6): `NumberOfPartitionMaps` at byte 268, partition map table at byte 440. [V-secondary]
   - Partition map type 2 with identifier "*UDF Metadata Partition" marks a virtual metadata partition. Its map entry gives the metadata file ICB LBA (partition-relative) and a mirror LBA. [V]
4. Metadata file ICB (Extended File Entry tag 266, or File Entry tag 261): at `pd.start_block + metadata_lba`. The data extents are also partition-relative: `pd.start_block + ad.lba`. [V, udfread lines 524, 545]
   - Mirror ICB (file type 251) is at `pd.start_block + mirror_lba`. [V]
5. Inside the metadata file: FSD (tag 256) at metadata-relative LBA 0. Root ICB long_ad at byte 400 (LBA at bytes 404-407). [V-secondary]
6. Directory FIDs (tag 257): characteristics at byte 18, L_FI at 19, ICB long_ad at 20, L_IU at 36, name at 38+L_IU, entries 4-byte aligned. [V-secondary]
7. File data is read at `partition_start + LBA` (physical partition), not at metadata start. [V-secondary]

Where the BDMV small files sit: one ImgBurn excerpt of a Blu-ray (single disc, truncated) [D]:
- /BDMV/index.bdmv start 4320, size 264
- /BDMV/MovieObject.bdmv start 4321, size 470
- /BDMV/PLAYLIST/00001.mpls start 4323, size 790
- /BDMV/CLIPINF/00001.clpi start 4344, size 11640
- /BDMV/STREAM/00001.m2ts starts at 275232 in 10 extents (first extents 1073676288 bytes each)
- /BDMV/BACKUP/PLAYLIST/00001.mpls at 16127906
Reading: small files are contiguous near the start of the physical partition (sector ~4300), then the m2ts. Sector 4320 is 8.4 MiB into the image. So index.bdmv may fall outside a 4 MB read. Single sample, not a rule. [D]

Practical answer to "can 1 MB tell us the metadata location?": Yes, for the chain in steps 1-4. The VDS and LVD fit inside the first 1 MB. The metadata file ICB is at partition_start + small LBA, so one extra article covering roughly the first 0.1-10 MB of the image is likely enough to list the BDMV tree. The exact placement is not verified. Check it with a real ISO: print the LBAs above and compare. [M for "one extra article", needs test]

Layers: a dual-layer disc has no logical layer break (per Lightning UK quoted in the ImgBurn thread). This is a claim, not checked against the spec. [D]

## 2. DVD ISO (UDF 1.02 + ISO9660 bridge) [M, not verified this session]
- ISO9660 Primary Volume Descriptor at sector 16 (byte 32768). Root directory record in the PVD (offset 156, 34 bytes). Directory records give name, extent LBA, and size. This means the whole ISO9660 tree is readable from the first ~100 KB if the directories are at low LBAs. [M]
- UDF 1.02 AVDP at 256 with the same VDS logic as Blu-ray. The DVD layout puts VIDEO_TS/ at the start of the physical partition, with VIDEO_TS.IFO and VIDEO_TS.BUP before VTS_01_0.IFO. Each VTS_xx_0.IFO precedes its VTS_xx_n.VOB files. [M]
- To list every file with sizes from the first ~1 MB: parse the ISO9660 directory extents for VIDEO_TS/. These records give all names and sizes. [M]

## 3. Main-title selection heuristics

BDInfo (via bdinfo-rs-core, a port of BDInfo's playlist ordering) [V-secondary]:
- Sort key: total length descending, then name ordinal ascending.
- Short-playlist filter: drop playlists strictly shorter than 20.0 s (default on). Exactly 20.0 is kept. Threshold valid range 0..86400, and 0 disables it.
- Looping filter: drop playlists that repeat a clip (has_loops), default on.
- Clip-set grouping: playlists sharing any clip join a group; groups are ordered by creation, longest first.
- Named selection ignores filters: normalize to upper case, append .MPLS if no dot.
- Note: this module does not choose a main title and has no chapter logic. Build the chapter count and the mapping from CLPI EP maps yourself.

Heuristic for Atlas (memory-based, to test):
1. Drop playlists under 20 s and any playlist with a repeated clip.
2. Take the longest remaining playlist. Obfuscated discs (Disney, Lionsgate) add hundreds of fake mpls with shared clips; they fall in one group, and the longest in that group is usually the film. [M]
3. Confirm with chapter count (MARKs in PlayList) and a known-runtime match if available. [M]
DVD: pick the longest PGC across title sets (VTS_xx IFO, title PGC playback time). Fake title sets (ARccOS) add decoy VTS. [M]

## 4. RAR'd BDMV folders
- RAR5 has no central directory. Each volume carries headers only for the file parts it contains. So volume 1 lists only the files that start there. Split files also have continuation headers. [M]
- File order from rar: depends on the command line and directory walk order. It is not guaranteed alphabetical. Store mode (-m0) keeps the data uncompressed, so volume 1 starts with the first files in walk order. [M]
- Scene full-BD rules (COMPLETE.BLURAY) require BDMV and CERTIFICATE folders. Not checked this session. [M]
- Implication: the first ~700 KB volume may hold BDMV/index.bdmv and a few PLAYLIST and CLIPINF files, but not all. Fetch extra later articles for the rest. Do not assume the order. Look for file headers. [M]

## 5. MakeMKV backup, AnyDVD HD, encryption detection
- Not verified. Forum threads found, not read: forum.makemkv.com viewtopic.php?p=51474, p=51449, p=51452, p=75401, p=75380 (returned by search).
- Detection idea: check for an AACS/ folder at the root (encrypted discs). In m2ts, TP_extra_header copy_permission_indicator (2 bits, top bits of the 4-byte header) identifies copy control. Decrypted m2ts shows copy_permission 0. Treat as [M] until tested.
- AnyDVD HD UDF implementation ID: not verified.

## Next steps
1. Build a test ISO (genisoimage/xorriso with -udf) and check the LBA chain above.
2. Fetch the zoffline/BDInfoCLI-ng BDInfo source for the exact filters.
3. Read the makemkv forum threads listed above for backup layout.
