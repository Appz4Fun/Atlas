# W1B archive headers + posters (Haiku)
- RAR5 main header flags (RR 0x0008, volnum 0x0002) + locator record (QO/RR offsets): packer-settings fingerprint, first article. technote.htm
- RAR5 file header: inner CRC32 + unpacked size = alt/twin key for inner file; mtime, host OS, solid, method, dict size = packer fingerprint. First article.
- RAR4: HOST_OS, salt, ext time, unicode name flags; main header RR/comment/first-vol/new-naming. forensics.wiki/rar
- 7z: first 32 bytes = next-header offset/size/CRC -> strong per-archive fingerprint (needs only first article for the numbers; real header at tail, often EncodedHeader).
- ngPost: 716800 article size, Message-ID uuid@ngPost (-z), random From, UUID subject with -x.
- Nyuu, newsmangler, GoPostStuff, JBinUp, line lengths: not verified yet.
