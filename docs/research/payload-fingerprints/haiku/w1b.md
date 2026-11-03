# W1B RAR/7z headers (Haiku, ~06:59Z)
- RAR4 file header: PACK_SIZE @7, UNP_SIZE @11, HOST_OS @15, FILE_CRC @16 (CRC32 of unpacked data), FTIME @20 (DOS, 2 s), UNP_VER @24, METHOD @25, NAME_SIZE @26, ATTR @28; flag 0x100 adds HIGH sizes (+8 bytes), 0x400 salt, 0x1000 EXT_TIME. Dictionary (LHD_WINDOW) in flags.
- Alt key: unpacked size + inner CRC32 (RAR4 FILE_CRC / RAR5 data CRC32 / 7z SubStreamsInfo CRC).
- ReScene: dictionary, method, -mt thread count, host OS byte, RAR build together pin down packer build+switches; srrDB flags "custom RAR packer". UNP_VER is useless as a version marker (6.24 still writes 29).
- ReScene collisions queue: volume CRCs can be padded to a target (DEADBABE trick) -> match on inner FILE_CRC, not volume SFV CRC.
- RAR5: compression info vint (version, solid, method, dict 128KB*2^N), host OS, optional CRC32, optional mtime; extra area 0x03 = ns-precision mtime/ctime/atime -> strong twin key when preserved.
- RAR5 encrypted headers: archive encryption header with KDF count + 16-byte salt shared by all headers of one archive; each header starts with a 16-byte IV. Locator record offsets for QO/RR. QO block may leak names.
- 7z: 32-byte signature header (start header CRC @8, next header offset @12, size @20, CRC @28); encoded header 0x17; coder IDs LZMA 030101, LZMA2 21, AES 06F10701; times 100 ns FILETIME; Unix mode in attr high bits (0x8000 flag) -> p7zip/py7zr writer.
Leads: RAR5 ns mtime + attrs + comp info as twin signature; volume-CRC padding means don't trust SFV CRCs; 7z AES salt/IV and SubStreamsInfo CRC location.
Sources: rarlab.com/technote.htm; rescene.wikidot.com rar-420-technote, collisions-queue, forum t-16457819; ctf-wiki.mahaloz.re/misc/archive/rar; codedread.github.io/bitjs/docs/unrar.html; py7zr.readthedocs.io archive_format
