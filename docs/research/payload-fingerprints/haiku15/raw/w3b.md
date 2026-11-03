# W3B Rust/Go prior art (Haiku), pass 1
- No project links posts across NZBs via PAR2/yEnc/archive headers. New work.
- rarpar/par2-rs 0.11 (Apache-2.0 from 0.10.11): scan_packets_from_path, Par2FileSet, 16k identity, Par2RepairSession; evidence.rs = ContiguousAssemblyProof (CRC-verified articles, no gaps) — repair guard, not linking.
- rust-par2 0.2 parse_par2_reader(reader,size).
- yencoding 0.2 (+yencoding-multi): ybegin/ypart parsing.
- Go mnightingale/rapidyenc Meta type.
- nzbdav-rar (Rust RAR4/5 header parser, m0 only), rar-stream 5.3 (MIT, parseRarHeader ~300B), zesven 7z needs Read+Seek (tail header), sevenz-rust2.
- usenet-dl: NZB SHA-256 baseline. AltMount: nothing.
## pass 2
- nzbdav-rar parse_headers<R: Read+Seek>: DISCARDS inner CRC32, mtime, host OS (_data_crc, _mtime_buf); no dict size. So Atlas must parse those itself.
- par2-rs scan_packets(&[u8], base_offset) works on partial buffers (skips truncated tail packet). FileDescriptionPacket{file_id, hash_full, hash_16k, file_length, filename}.
- sevenz-rust2 read_start_header private; public open needs full length. (But the 32-byte start header is trivial to hand-parse: NextHeaderOffset/Size/CRC are usable keys from article 1.)
- rar-stream / nzbdav app internals unverified.
