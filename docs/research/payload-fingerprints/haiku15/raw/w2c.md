# W2C obfuscation (Haiku)
- sabnzbd is_probably_obfuscated(): ^[a-f0-9]{32}$, ^[a-f0-9.]{40,}$, 30+hex+2 tags, abc.xyz prefix. deobfuscate(): biggest file >3x second. par2file.py FileDesc offsets: fileid 32:48, 16k 64:80, size 80:88, name 88:.
- nzbget par-renamer groups par sets by content; bug splits sets with different obfuscated base names (issue 313).
- ngPost -x: subject=UUID=msgid, --gen_name 17, --gen_pass 13, -m password in NZB meta.
- Newsbin pattern: one random subject per file with trailing number, no extensions.
- Password sources: NZB filename {{PW}}, NZB meta password, x-dnzb-password header.
- RAR5 -hp hides everything except crypt header (KDF salt/params + check value) -> salt is a per-creation twin key.
