# W3A re-pack provenance (Haiku, ~07:56Z). WebFetch hourly rate limit hit near the end.
| Signal | Proves | Cost | Source |
|---|---|---|---|
| Inner mtime (RAR5 ns / 7z 100ns / RAR4 DOS 2s) + which times are stored (RAR5 HTIME flags 0x02/0x04/0x08/0x10, 7z CTime/ATime presence) | same source copy (provenance), flags = writer setting | free (header) | WinRAR HELPSwTS; php-rar unrar headers5.hpp; zesven 7z timestamps |
| unrar restores archived mtime on extract; SABnzbd only strips with ignore_unrar_dates | re-packs of a downloaded copy inherit the original mtime | - | php-rar unrar extract.cpp; sabnzbd newsunpack.py |
| RAR5 UOWNER 0x06 user/group/uid/gid | same uploader machine | free | headers5.hpp |
| HOST_OS, 7z Unix mode (umask 0644 vs 0664), Windows archive bit | packer OS / machine | free | headers.hpp; zesven |
| RAR5 -ts- (no times), RAR5 authenticity/comment flags, RAR4 comment block 0x75 | writer setting/generation | free | forensics.wiki/rar |
| ngPost: --rar_size -> -v42m (42 MB volumes), -idp, -ep1, -m0, --gen_name 17 chars, --gen_pass 13 chars, 7z -mx0 -mhe=on, --rar_no_root_folder | tool fingerprint | free | ngPost README |
| Nyuu: no RAR support (7z or pre-packed only) | tool exclusion | free | Nyuu README |
| postie par2.redundancy default 1n*1.2, obfuscation_policy | tool fingerprint | free | postie docs |
| Same volume size + same source copy -> identical split offsets | twin-ish provenance | volume headers | pyrescene issue 168 |
Caveat: agent claims volume size changed the stored file CRC/method in pyrescene #168; that's likely the per-volume chunk CRC, not the whole-file CRC. Unverified: -ep paths, scene volume rules (scenerules.org robots-blocked).
