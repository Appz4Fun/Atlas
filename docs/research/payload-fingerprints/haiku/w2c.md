# W2C non-video early-byte IDs (Haiku, ~07:25Z)
- Ogg page header bytes 12-15: bitstream serial, random per stream (RFC 3533) -> twin/alt key for Ogg/Opus/Vorbis, first page. Check if remux keeps it. https://xiph.org/ogg/doc/rfc3533.txt
- LAME info tag: "LAME x.yy" @120, music length @148, music CRC16 @152 (PCM CRC = per-encode key), tag CRC16 @154. https://linux.m2osw.com/mp3-info-tag-specifications-rev0-lame-3100
- APE >=3980: MD5 of audio @36 -> same-work/encode key. ffmpeg libavformat/ape.c
- MOBI PalmDB creation date + unique_id_seed; EXTH 113 ASIN (per title), 112 source. docs.rs iepub mobi/common.rs
- ISO9660 PVD @0x8000: volume set id, application id (burner tool), creation/modification dates with centiseconds (per-creation), Application Use 512 bytes. behindthebits.blogspot.com 2012/08 iso9660-pvd
- CHD v5 header: rawsha1 @64, sha1 @84, parentsha1 @104 (content keys, first 124 bytes). git.redump.net mame chd.h
- MSI Revision Number = PackageCode GUID (per build, not at front). learn.microsoft.com msi revision-number-summary
- XCI CardHeader @0x1000: hashes of partition headers, version; no random field found. switchbrew.org/wiki/XCI
- EXIF ImageUniqueID 0xA420.
Unverified: EPUB calibre:uuid, PDF /ID, PE RSDS GUID, PARAM.SFO.
