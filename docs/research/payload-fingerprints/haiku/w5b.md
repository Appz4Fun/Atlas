# W5B anti-linking countermeasures (Haiku, ~09:00Z)
- Takedowns are Message-ID lists per backbone; notices must list every article ID (UvA rp.os3.nl/2014-2015/p82/report.pdf p5); obfuscated subjects still got removed (p28). nzb.is blog dmca-vs-ntd; newsdemon usenet-takedowns.
- ngPost: -x subject obfuscation, --gen_name 17, --gen_pass 13, --gen_from, --auto_compress, -m meta puts password in the NZB, POST_HISTORY CSV. Nyuu-Obfuscation: random subject/From/Message-ID/yEnc name/Date per article, no User-Agent, real name kept in NZB subject. Neither does junk padding or random volume sizes.
- StreamNZB doc on obfuscated releases: match NZB files to PAR2 by MD5-16k + length; fall back to yEnc name=; RAR5 volume number restores order; magic bytes identify type. https://github.com/Gaisberg/streamnzb/blob/main/docs/obfuscated-releases.md
- RAR4 -p keeps names (Elcomsoft 2026-07 blog); RAR5 -p with checksum tweak HMACs the CRC.
Survivability (corrected by me against the tests):
| Countermeasure | Survives | Breaks |
|---|---|---|
| Subject/Message-ID/From randomisation | every payload key | header keys only |
| Random archive (volume) names | inner size+CRC, PAR2 full MD5/16k of volumes, IFSC, exp-0, pcrc32 | PAR2 File ID, Set ID |
| Junk file added | per-file keys (inner CRC, full MD5, IFSC, pcrc32 of real files) | Set ID, exp-0 |
| Different volume size, no encryption | inner size + inner CRC (last volume) | all volume-level hashes, Set ID, exp-0, pcrc32 |
| Per-post password -p (RAR5) | inner size, unpacked sizes; names in RAR4 | inner CRC (RAR5 tweak), every ciphertext hash |
| Header encryption -hp/-mhe + new password | volume sizes / count only (see Opus -hp size math) | everything else |
| Size-preserving edit (e.g. MKV title) | size, untouched IFSC slices / pcrc32 | full CRC/MD5 |
| Cross-group split / fills of same bytes | all part- and file-level keys | none |
