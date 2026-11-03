# W6B test vectors + fuzzing (Haiku)
- yEnc 1.3 spec text: sources.debian.org/data/main/p/python-yenc/0.4.0-8/doc/yenc-draft.1.3.txt
- libarchive RAR fixtures (.uu, BSD-2): test_read_format_rar_*.rar.uu, test_read_format_rar5.c
- sabctools tests/test_decoder.py; tcllib yencode.test; par2cmdline tests/ (GPL-2, oracle only); ParPar test (MIT?)
- zlib-rs crc32/combine.rs (Zlib license) reference.
- CVE-2025-11579 rardecode: unbounded dictionary size DoS -> cap sizes before alloc. CVE-2023-50628 is NOT par2 (libming).
- Don't vendor unrar code (license).
- Test plan: yEnc round trip; truncate fixtures at every offset (no panic, NeedMore); length fields 0/1/MAX rejected before alloc; caps; cargo-fuzz per parser; combine property test vs zlib+crc32fast; differential PAR2 vs par2cmdline; libarchive fixtures; bit-flip tests.
