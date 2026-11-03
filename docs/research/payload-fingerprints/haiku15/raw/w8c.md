# W8C partial-buffer parsing (Haiku)
- Hand-written tri-state parser (NeedMore(n) / Invalid / Parsed(v, consumed)) recommended; winnow Partial<I> -> ErrMode::Incomplete(Needed) if a combinator lib is wanted.
- Integrity: RAR4 HEAD_CRC = low 16 of CRC32 from HEAD_TYPE to end of header; RAR5 CRC32 per block from size vint to end of header; 7z StartHeaderCRC over bytes 12..32 (peer W3A pass 2 confirms 0x0C-0x1F); PAR2 MD5 bytes 32..end, length %4==0, >=64; EBML VINT 1-8 octets, all-ones = unknown size (RFC 8794).
- nzbdav-rar: vint drops 10th-byte high bits; read_exact EOF == invalid; vec![0; n] from parsed size (no cap).
- Rules: checked arithmetic; cap header/name sizes before alloc; verify header CRC only when complete; sanity-check sizes before NeedMore; vint max 10 bytes, reject overflow; property-test every prefix; fuzz.
