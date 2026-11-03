# W1A PAR2 internals (Haiku)
- RSID = MD5(Main packet body: slice size, count, sorted File IDs). File ID = MD5(md5_16k||len||name) -> RSID is name-bound.
- FileDesc full MD5 + 16k MD5 = content key; survives renames.
- IFSC: per-slice MD5+CRC32 (tail zero-padded): sub-file matching when slice sizes agree.
- SABnzbd deobfuscation: 16k-MD5->name dict, drops duplicate hashes (ambiguity rule).
- UniFileN overrides ASCII name; sabnzbd ignores it.
- Packets any order, vital packets duplicated across vols.
- Creator strings unverified. ParPar -S auto slice size keeps count <=32768, slice multiple 4.
