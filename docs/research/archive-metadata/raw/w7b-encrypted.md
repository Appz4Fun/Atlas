# w7b: Encrypted archives (RAR5 -hp, RAR4 -hp, 7z -mhe=on, ZipCrypto, WinZip AES)

Scope: what survives encryption, how volumes can be grouped, and how to test a candidate password cheaply.
Source tree used for RAR: GitHub mirror `pmachapman/unrar` (unrar source, version not stated in the file; RARLAB is blocked). Local copies are in `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w7b/`.
Source tree used for 7z and WinZip AES: `ip7z/7zip` main branch. ZIP and RAR/7z detection: `libarchive/libarchive` master.
Tags: VERIFIED = read in the cited source. UNVERIFIED = not read in this pass, or an estimate. Cost figures are estimates, not benchmarks.

## 0. Headline findings

1. RAR5 -hp does NOT leave header sizes in the clear. Each encrypted header is `IV(16) || AES-256-CBC(...)`. The header CRC32 and the header size vint are inside the ciphertext, so header lengths, the number of headers and the header positions are not recoverable without the password. Only the HEAD_CRYPT block (the archive encryption header) is plaintext. VERIFIED arcread.cpp:559-620 (IV read, then first 7-byte read through the decrypting Raw), arcread.cpp:725-763.
2. RAR5 salt is not a set-wide twin key in the unrar code. Each archive (each volume file) has its own HEAD_CRYPT salt, and each file header has its own FHEXTRA_CRYPT salt and InitV. Whether rar writes the same HEAD_CRYPT salt into every volume is UNVERIFIED and needs a real multi-volume fixture.
3. RAR4 -hp has a clear 8-byte salt before every encrypted block. The KDF therefore runs once per block (0x40000 SHA-1 rounds each), and the salt cannot link volumes. VERIFIED arcread.cpp:148-164, crypt3.cpp:19-80.
4. RAR4 has no password check value. The password test is "decrypt the next block header and check HEAD_CRC". VERIFIED arcread.cpp:431-432 (HEAD_CRC compare on decrypted headers). The exact CRC15 definition (`GetCRC15`) was not read.
5. 7-Zip's default `-mhe=on` AES-256 uses NO salt (the encoder's salt code is commented out), NumCyclesPower = 19, and a fresh 16-byte IV. The KDF is SHA-256 over `password || counter` for 2^19 rounds. VERIFIED 7zAes.cpp:39-111, 186-236.
6. ZipCrypto has a 1-byte check, so a false-positive rate of 1/256 per candidate. WinZip AES has a 2-byte verifier (1/65536 false positive) plus a per-entry salt. VERIFIED ZipCrypto.cpp:25-54, WzAes.cpp:36-54, libarchive zip.c:3449-3463.
7. libarchive can detect encrypted entries for RAR5, RAR4 and 7z, but cannot decrypt any of them. VERIFIED archive_read_support_format_rar5.c:2541-2547, archive_read_support_format_rar.c:772-773, archive_read_support_format_7zip.c:102, 526-527.

## 1. RAR5 (-hp: archive encryption header, type 4)

### 1.1 HEAD_CRYPT block layout (plaintext)
The HEAD_CRYPT block is read with `Encrypted==false`, so it is not decrypted. VERIFIED arcread.cpp:559 (Decrypt = Encrypted && pos > sig), 725-763. Fields in order:
- Common block header: CRC32, header size vint, header type vint (= 4), header flags vint.
- CryptVersion vint. Must be 0 (`CRYPT_VERSION 0`, crypt.hpp:21). VERIFIED arcread.cpp:728-733.
- EncFlags vint. Bit 0x0001 = CHFL_CRYPT_PSWCHECK (headers.hpp / headers5.hpp:41). VERIFIED arcread.cpp:736.
- Lg2Count: 1 byte. KDF iteration count is 2^Lg2Count. Default 15 (CRYPT5_KDF_LG2_COUNT, crypt.hpp:19). Maximum 24 (crypt.hpp:20). VERIFIED arcread.cpp:737-742.
- Salt: 16 bytes. VERIFIED arcread.cpp:745 (SIZE_SALT50 = 16, crypt.hpp:10).
- If PSWCHECK: PswCheck 8 bytes, then csum 4 bytes. csum must equal the first 4 bytes of SHA-256(PswCheck), otherwise UsePswCheck is cleared. VERIFIED arcread.cpp:746-760.

### 1.2 Encrypted header blocks
After HEAD_CRYPT, `ReadHeader50` (arcread.cpp:559-640):
1. Reads a 16-byte InitV (IV) from the file. VERIFIED arcread.cpp:575-578.
2. Derives the key and, if PSWCHECK is present, compares the derived PswCheck with the stored one before decrypting. VERIFIED arcread.cpp:587-606.
3. Decrypts with AES-256-CBC, key = PBKDF2 output (below), IV = the per-block InitV. VERIFIED crypt5.cpp:185-186 (`rin.Init(Encrypt, Key, 256, InitV)`).
4. Reads the first 7 bytes through the decrypting Raw stream: 4-byte CRC32 plus up to 3 bytes of header size vint (the comment says so). The header size is therefore inside the ciphertext. VERIFIED arcread.cpp:614-620.
5. If the header CRC32 fails while decrypting, the code assumes a wrong key. VERIFIED arcread.cpp:686-700.
Ciphertext length is the padded plaintext length, but the plaintext length is only known after decryption. So header lengths and positions are NOT visible. The only clear-text per-header item is the 16-byte IV.

### 1.3 KDF and password check (exact algorithm, from crypt5.cpp)
Inputs: password as UTF-8 (`WideToUtf`, crypt5.cpp:158-161), salt (16 bytes), c = 2^Lg2Count.
- `pbkdf2(pwd, salt, c)` PBKDF2-HMAC-SHA256, 32-byte output blocks, block index 1 (salt is 16 bytes, so the index is appended at salt||00000001). VERIFIED crypt5.cpp:85-101.
- The function computes the same U-chain three times: Key = T_c, HashKey V1 = T_(c+16), PswCheckValue V2 = T_(c+32), where T_n is the n-th PBKDF2 iterate. VERIFIED crypt5.cpp:104-121 (CurCount = {Count-1, 16, 16}, Count = 1<<Lg2Cnt, called at crypt5.cpp:161 with 1<<Lg2Cnt).
- So V2 = PBKDF2-HMAC-SHA256(pwd, salt, c+32, dkLen=32) (derived from the chain above, since the chain is the standard PBKDF2 chain). UNVERIFIED against a real archive; the check tests the unrar test vectors only (crypt5.cpp:219-233, #if 0).
- PswCheck (8 bytes) = XOR-fold: `PswCheck[i % 8] ^= PswCheckValue[i]` for i = 0..31. VERIFIED crypt5.cpp:177-179.
- Compare with HEAD_CRYPT PswCheck (8 bytes). The stored csum (4 bytes) only guards the stored PswCheck value. VERIFIED arcread.cpp:598-599, 746-760.
- The 16-byte HashKey V1 is only used for -hashmac MACs (`ConvertHashToMAC`, crypt5.cpp:193-212). Not needed for the password test.

Implementation: `pbkdf2_hmac::<Sha256>(pwd_utf8, salt16, (1<<lg2) + 32, &mut v2)` then fold v2 to 8 bytes. Compare to PswCheck. Use `pbkdf2_hmac::<Sha256>(pwd_utf8, salt16, 1<<lg2, &mut key32)` for the header key. Test vector for the derivation: PBKDF2-HMAC-SHA256("password","salt",1) starts with 120fb6cf (crypt5.cpp:224), 4096 iterations starts with c5e478d5 (crypt5.cpp:228).

### 1.4 Cost
- Per guess: about 2^15 + 32 = 32 800 HMAC-SHA256 iterations. unrar caches the ipad/opad contexts (crypt5.cpp:23-76), so each iteration is about 2 SHA-256 compressions. That is about 65 600 compressions per guess.
- Estimate: about 2-4 ms per guess per core with SHA-NI, about 10-15 ms with portable SHA-256. UNVERIFIED benchmark.
- With the PswCheck flag set, a wrong password is rejected after one KDF. Without it, the test is the header CRC32 of a decrypted header, which costs a KDF plus one AES block and a CRC.

### 1.5 Salt and volume grouping
- Per archive: one HEAD_CRYPT salt (arcread.cpp:745).
- Per file: FHEXTRA_CRYPT carries its own Salt, InitV and PswCheck (arcread.cpp:1077-1107). These are inside the encrypted header, so invisible with -hp.
- unrar has no state shared between volumes for keys (KDF5Cache is keyed by password+salt+lg2, crypt5.cpp:131-172, and is process-local).
- Whether rar writes an identical HEAD_CRYPT salt into every volume: UNVERIFIED. Test with a real multi-volume fixture. If it is identical, the 16-byte salt is a cheap grouping key (readable in the clear at byte offset 8 + HEAD_CRYPT fields). If not, grouping has to use the article subject, NZB, sizes and PAR2 (see section 5).

### 1.6 What still shows in RAR5 -hp (without password)
- Signature `Rar!\x1a\x07\x01\x00`.
- HEAD_CRYPT block in full: Lg2Count (so KDF cost is known), Salt, PswCheck, csum. VERIFIED.
- Nothing else: file names, file count, sizes, main-header flags, volume number and the packed-data sizes are all inside ciphertext. UNVERIFIED for the main-header volume fields, which are inside the encrypted header per the code path above.
- The total volume file size (full volumes equal the -v size) and the article count remain visible.

## 2. RAR4 (-hp, MHD_PASSWORD)

### 2.1 Which blocks are encrypted
- Marker block (7 bytes) is plain.
- The main header is read before `Encrypted` is set from its MHD_PASSWORD flag (arcread.cpp:148, 237). So the main header is parsed as plaintext in this code path. UNVERIFIED against a real RAR3 -hp fixture (this depends on whether any earlier state sets `Encrypted`).
- Every later header: `Decrypt = Encrypted && pos > sig+7` (arcread.cpp:148), and each one begins with an 8-byte salt in the clear (arcread.cpp:157-163).

### 2.2 Salt scope
- Per block. `ReadHeader15` reads a fresh 8-byte salt before each encrypted block and calls `SetCryptKeys(false, CRYPT_RAR30, pwd, Salt, ...)` (arcread.cpp:157-164). VERIFIED.
- The file-data salt for each file (LHD_SALT) is inside the encrypted file header (arcread.cpp:373-390). VERIFIED.
- Same across volumes: the format has no archive-wide salt. Each block salt is independent, so volumes cannot be linked by salt. The statement that salts are random per block is UNVERIFIED (the code only reads them).

### 2.3 KDF (crypt3.cpp:SetKey30)
- Raw password = UTF-16 code units, little-endian (`WideToRaw`), followed by the 8-byte salt. VERIFIED crypt3.cpp:9-18 (RawLength = 2*len, Salt appended). LE byte order UNVERIFIED (WideToRaw body not read).
- Loop i = 0 .. 0x40000 - 1 (262 144 rounds): `sha1_process_rar29(c, RawPsw, RawLength); sha1_process(c, [i&0xff, (i>>8)&0xff, (i>>16)&0xff], 3)`. VERIFIED crypt3.cpp:20-35.
- Every 0x40000/16 = 16 384 rounds, copy the SHA-1 state, finalise, take digest[4] low byte as IV byte I/16384. That gives a 16-byte IV. VERIFIED crypt3.cpp:37-42 (AESInit[I/(HashRounds/16)] = (byte)digest[4]).
- Final key: SHA-1 digest of the whole loop, words written little-endian to 16 bytes (AESKey[I*4+J] = digest[I] >> (8*J)). VERIFIED crypt3.cpp:45-49.
- AES-128-CBC with that key and IV (`rin.Init(Encrypt, AESKey, 128, AESInit)`, crypt3.cpp:54). VERIFIED.
- `sha1_process_rar29` is not standard SHA-1 in name. Its definition is in unrar's hash code, not fetched. UNVERIFIED whether it equals plain SHA-1 on the byte stream. Check before implementing. 7-Zip has an independent RAR3 decryptor at `ip7z/7zip/main/CPP/7zip/Crypto/RarAes.cpp` (its CalcKey was not read).

### 2.4 Password test (no check value)
- Decrypt the first encrypted block (its 8-byte salt is in the clear), then check HEAD_CRC. RAR4 HEAD_CRC is `GetCRC15` of the header bytes, compared against the stored value. VERIFIED arcread.cpp:431-432 (`GetCRC15`). The 15-bit/16-bit masking was not read.
- Each block has its own salt, so each test is a full KDF (262 144 SHA-1 rounds plus AES). There is no cache benefit across blocks.

### 2.5 Cost
- Hashed bytes per guess: 262 144 * (2*L + 8 + 3) with L = password length in UTF-16 code units. For L=10: about 8.1 MB, about 127 000 SHA-1 compressions.
- Estimate: about 4 ms per guess with SHA-NI, about 20 ms portable. UNVERIFIED benchmark.

### 2.6 What still shows in RAR4 -hp
- Marker, and (per 2.1, UNVERIFIED) the main header. Every later block starts with a clear 8-byte salt, but the block length is inside the ciphertext, so the next salt position is not known without the key. The block count is not recoverable without a key.
- Volume file sizes and article counts remain visible.

## 3. 7z -mhe=on (AES-256 header encryption)

### 3.1 Coder properties (7zAes.cpp)
- Encoder: NumCyclesPower = 19. Salt code is commented out, so the default SaltSize is 0. VERIFIED 7zAes.cpp:228-236 (`_key.NumCyclesPower = 19`; `// _key.SaltSize = 4 ...`). The default CKeyInfo has SaltSize = 0 (7zAes.h:35-46).
- IV: `ResetInitVector` writes 16 random bytes, `_ivSize = 16`. VERIFIED 7zAes.cpp:199-206.
- Properties blob: byte 0 = NumCyclesPower | (salt ? 0x80 : 0) | (iv ? 0x40 : 0); byte 1 = ((saltSize-1)<<4) | (ivSize-1) when either is present; then salt bytes; then IV bytes. VERIFIED 7zAes.cpp:211-226.
- Decoder: NumCyclesPower = b0 & 0x3F; saltSize = ((b0>>7)&1) + (b1>>4); ivSize = ((b0>>6)&1) + (b1 & 0xF); the blob length must equal 2 + saltSize + ivSize. VERIFIED 7zAes.cpp:247-275.
- Default encoder output (no salt, 16-byte IV): b0 bit 7 = 0, bit 6 = 1, b1 = 0x0F, so the blob is 2 + 16 = 18 bytes. VERIFIED by the code above (7zAes.cpp:211-226, 247-275).

### 3.2 Key derivation (7zAes.cpp:39-111)
- If NumCyclesPower == 0x3F: key is raw salt||password padded to 32 bytes (not hashed). VERIFIED 7zAes.cpp:41-50.
- Otherwise: SHA-256 stream over `salt || password || LE counter` where the counter is written into the 8-byte field at the end of each buffer and increments per round, for 2^NumCyclesPower rounds. The final digest is the 32-byte AES key. VERIFIED 7zAes.cpp:54-111 (numRounds = 1<<NumCyclesPower; counter in `buf + bufSize - 8`; the 8-byte field is zeroed first, so the counter is effectively 32-bit LE for 2^19 rounds). The unroll code is an optimisation of the same byte stream.
- Password bytes: the password comes in as a BSTR (`7zHandlerOut.cpp:722-734`). UTF-16LE byte encoding of the password is UNVERIFIED (the BSTR-to-bytes conversion in 7zAes.cpp was not read).
- Test cost: 2^19 SHA-256 stream updates, each over (salt + password bytes + 8). With no salt and a 10-character password (20 bytes in UTF-16LE), that is about 14.7 MB, about 230 000 compressions. Estimate: about 7 ms with SHA-NI, about 40 ms portable. UNVERIFIED benchmark.

### 3.3 Same IV across .7z.001 ... .7z.00N?
- Yes. The IV is in the coder properties of the (encoded) header folder, written once per archive. VERIFIED 7zAes.cpp:199-226 (one IV per stream). A multi-volume 7z is one continuous byte stream.
- Where the header lives: the encoded header is in the last volume (7z layout). The 32-byte start header is at offset 0 of .001 and holds NextHeaderOffset/Size/CRC in clear. UNVERIFIED that the next header is in the last volume in this source set (standard 7z layout).

### 3.4 Password test
- The header is stored as kEncodedHeader: a packed folder with the AES coder followed by LZMA (7zIn.cpp:1679, 1753). The decrypted output is the real header (kHeader, byte 0x01 in the 7z NID scheme, UNVERIFIED in this source set).
- Test: derive the key (3.2), AES-256-CBC-decrypt the packed header with the IV from the props (no padding check is available from the cipher alone; padding scheme UNVERIFIED), then either LZMA-decode the first bytes and check for the kHeader NID, or check the folder CRC. Folder CRCs are parsed at 7zIn.cpp:906 (`ReadHashDigests(numFolders, folders.FolderCRCs)`). VERIFIED for parsing; UNVERIFIED for the decoded-header check semantics.

### 3.5 What still shows in 7z -mhe=on
- Start header (32 bytes at .001 offset 0): next header offset, size and CRC. Plain.
- The encoded-header folder: its PackInfo and coder props (including the AES props and IV). Plain. VERIFIED 7zIn.cpp:1679/1753 (structure), 7zAes.cpp (props).
- File names, file count, file sizes and the data-folder pack sizes are inside the encrypted header. UNVERIFIED in detail, but the header is encrypted as a whole with -mhe=on.
- Packed file data is AES-encrypted only with -p; with -mhe=on alone, data is not encrypted. UNVERIFIED.

## 4. ZIP

### 4.1 ZipCrypto (traditional PKWARE)
- Key init: key0 = 0x12345678, key1 = 0x23456789, key2 = 0x34567890, then update with each password byte. VERIFIED ZipCrypto.cpp:25-30; libarchive archive_read_support_format_zip.c:~395-400 (keys[1]=591751049 = 0x23456789, keys[2]=878082192 = 0x34567890).
- Update per byte: key0 = crc32_update(key0, b); key1 = (key1 + (key0 & 0xFF)) * 0x08088405 + 1; key2 = crc32_update(key2, key1 >> 24). VERIFIED ZipCrypto.cpp:15-18.
- 12-byte encryption header decrypted with the key stream. libarchive decrypts 12 bytes (zip.c:401, `trad_enc_decrypt_update(ctx, key, 12, header, 12)`). VERIFIED.
- Check byte: 7-Zip's writer puts `crc >> 8` in the last header byte (ZipCrypto.cpp:52-54). libarchive returns `header[11]` as `crcchk` (zip.c:403). The comparison against CRC high byte vs DOS-time high byte (general-purpose bit 3) is UNVERIFIED in this source set. Standard ZIP spec behaviour.
- Test cost: under 1 microsecond per candidate. False positive 1/256; confirm with a second check (decrypt the first data bytes, or full CRC32 of the entry).
- Clear-text: entry names (ZipCrypto does not encrypt names), compressed and uncompressed sizes, CRC32 in local and central headers. Standard ZIP layout, UNVERIFIED in this source set.

### 4.2 WinZip AES (AE-1 / AE-2)
- Strength byte: 1 = AES-128 (salt 8, key 16), 2 = AES-192 (salt 12, key 24), 3 = AES-256 (salt 16, key 32). VERIFIED archive_read_support_format_zip.c:3449-3460.
- Per-entry salt: read at the start of each entry's data, followed by a 2-byte password verifier. VERIFIED zip.c:3463 (`salt_len + 2` read ahead), WzAes.cpp:97-105.
- KDF: PBKDF2-HMAC-SHA1(password, salt, 1000 iterations, dkLen = 2*keyLen + 2). Output split as: AES key = dk[0..keyLen], HMAC key = dk[keyLen..2*keyLen], verifier = dk[2*keyLen..+2]. VERIFIED WzAes.cpp:36-54 (kNumKeyGenIterations = 1000, WzAes.cpp:24; kPwdVerifSize = 2, WzAes.h:41; salt size = 4*KeySizeMode+4, WzAes.h:60; Hmac()->SetKey(dk+keySize, keySize) and verifier copy at WzAes.cpp:52-54).
- Password test: compare the 2-byte verifier (WzAes.cpp:120). False positive 1/65536.
- Cost: 1000 HMAC-SHA1 iterations, about 2000 SHA-1 compressions per guess. Estimate: about 0.1 ms per guess per core. UNVERIFIED benchmark.
- Clear-text: entry names, sizes and the CRC is 0 for AE-2 (standard, UNVERIFIED here), salts and verifiers per entry.

## 5. What remains readable without the password (all formats)

| Item | RAR5 -hp | RAR4 -hp | 7z -mhe=on | ZipCrypto | WinZip AES |
|---|---|---|---|---|---|
| Signature | yes | yes | yes | yes | yes |
| Archive KDF params | HEAD_CRYPT: Lg2Count, Salt, PswCheck (plain) | none (per block salt) | coder props (plain, in encoded-header folder) | none | per-entry salt + verifier |
| Header sizes / count | no | no | no (header inside AES) | n/a | n/a |
| File names | no | no | no | yes | yes |
| Sizes | no (inside header) | no | no (inside header) | yes (local/central) | yes |
| Volume file sizes | yes (full volumes = -v size) | yes | yes | yes | yes |
| Article counts | yes (from NZB) | yes | yes | yes | yes |
| Recovery records | UNVERIFIED (PAR2 is separate) | same | same | n/a | n/a |
| PAR2 FileDesc (names + MD5 of each RAR volume) | yes, never encrypted (general PAR2 knowledge; see background file `w6c-par2-proto.md`, UNVERIFIED here) | same | same | n/a | n/a |

## 6. Grouping volumes and testing a password

### 6.1 Grouping
- RAR5 -hp: if HEAD_CRYPT salts are identical across volumes (UNVERIFIED), group by the 16-byte salt in the clear (cheap). Otherwise group by NZB/subject, size (all full volumes equal), and PAR2 FileDesc names and MD5 (exact per-volume match).
- RAR4 -hp: no archive-wide salt, so grouping is by NZB/subject, sizes and PAR2 only.
- 7z -mhe=on: a multi-volume set is one stream. Group by the start-header only in `.001`; for `.002..N`, use NZB/subject, sizes and PAR2. The IV and KDF params are per archive, so a candidate password is per set.
- ZIP: a ZipCrypto entry's name is clear; group by name. WinZip AES: the salt is per entry, so grouping uses names in the clear.

### 6.2 Password testing (cheapest first)
1. RAR5 with PswCheck: one PBKDF2 (2^lg2 + 32 iterations), fold, compare 8 bytes. Definitive except about 2^-64. Cost about 3-15 ms per guess.
2. RAR5 without PswCheck: derive key (2^lg2 iterations), decrypt one header, check CRC32. Cost about the same plus one AES block.
3. RAR4 -hp: 262 144 SHA-1 rounds with the first block salt, decrypt the first block, check HEAD_CRC. Cost about 4-20 ms per guess.
4. 7z -mhe=on: 2^19 SHA-256 rounds, decrypt the encoded header block, then LZMA-decode a few bytes or check the folder CRC. Cost about 7-40 ms per guess.
5. ZipCrypto: check byte (1/256 false positive), then decrypt-and-CRC to confirm. Cost under 1 microsecond.
6. WinZip AES: 2-byte verifier (1/65536), then HMAC over the data for confirmation. Cost about 0.1 ms.

### 6.3 Rust crates (RustCrypto) and what to use
- `pbkdf2` (`pbkdf2_hmac::<Sha256>`) for RAR5, `pbkdf2_hmac::<Sha1>` for WinZip AES.
- `sha2` (Sha256) for RAR5 PswCheck and 7z KDF; `sha1` for RAR4 KDF (a streaming `Sha1` hasher that can be cloned, to snapshot digest[4] every 16 384 rounds).
- `hmac` for the PBKDF2 path (pbkdf2 crate wraps it).
- `aes` + `cbc` (`cbc::Decryptor<aes::Aes256>` for RAR5 and 7z, `cbc::Decryptor<aes::Aes128>` for RAR4). WinZip AES uses CTR (`ctr` crate), not CBC.
- `zeroize` for key material.
- Estimated cost per guess is above; benchmark with criterion before committing to a budget. UNVERIFIED.
- Background file `w2a-decompressors.md` lists `rars` with an `encryption` feature built on aes/hmac/sha1/sha2. Not checked here.

## 7. Open items (UNVERIFIED, need a real fixture)
- Whether rar writes the same HEAD_CRYPT salt in every RAR5 volume.
- Whether the RAR4 main header is encrypted under -hp.
- `sha1_process_rar29` equivalence to standard SHA-1.
- RAR4 HEAD_CRC masking (GetCRC15) and the WideToRaw byte order.
- 7z padding scheme for AES-CBC and the UTF-16LE password encoding.
- The ZipCrypto check-byte comparison in libarchive (zip.c around 3404-3420).

## Sources
- `pmachapman/unrar` mirror (unrar source), local copies in `/tmp/claude-0/-home-claude/1315c443-c098-58da-9f63-b135a00caf8b/scratchpad/w7b/`:
  - https://raw.githubusercontent.com/pmachapman/unrar/master/crypt5.cpp (hmac_sha256 1-40; pbkdf2 85-128; SetKey50 131-190; ConvertHashToMAC 193-212; test vectors 215-235)
  - https://raw.githubusercontent.com/pmachapman/unrar/master/crypt3.cpp (SetKey30 1-54)
  - https://raw.githubusercontent.com/pmachapman/unrar/master/crypt.hpp (constants, KDF caches, lines 10-21)
  - https://raw.githubusercontent.com/pmachapman/unrar/master/crypt.cpp (SetCryptKeys dispatch, lines 46-90)
  - https://raw.githubusercontent.com/pmachapman/unrar/master/arcread.cpp (ReadHeader15 salt 145-164; main header 228-240; ReadHeader50 decrypt 555-640; FHEXTRA_CRYPT 1065-1112; HEAD_CRYPT 719-763; HEAD_CRC 686-700, 431-432)
  - Local copy `headers5.hpp` (flag definitions 40-41, 82, 101-103) from `/home/claude/scratchpad/w3c/unrar/headers5.hpp`.
- libarchive (master):
  - https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar5.c (encryption detection 1755-1758, 2541-2547)
  - https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_rar.c (caps 772-773; has_encrypted_entries 777-785)
  - https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_zip.c (trad decrypt 385-403; AES strength 3449-3463; 3404-3420 caller)
  - https://raw.githubusercontent.com/libarchive/libarchive/master/libarchive/archive_read_support_format_7zip.c (crypto ids 101-102; caps 526-527)
- 7-Zip (ip7z/7zip, main):
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/7zAes.cpp (CalcKey 39-111; ResetInitVector 199-206; props encode 211-226; encoder ctor 228-236; decoder props 247-275)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/7zAes.h (CKeyInfo 17-46)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/ZipCrypto.cpp (key init 25-30; update 15-18; check byte 44-58)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/WzAes.cpp (PBKDF2 36-54; verifier 97-120)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/WzAes.h (kPwdVerifSize 41; salt size 60)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Crypto/RarAes.cpp (RAR3 AES, not fully read)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zIn.cpp (encoded header 1679, 1753; folder CRCs 906)
  - https://raw.githubusercontent.com/ip7z/7zip/main/CPP/7zip/Archive/7z/7zHandlerOut.cpp (password BSTR 722-734)
- Background (not verified here): `docs/research/archive-metadata/raw/w6c-par2-proto.md`, `w2a-decompressors.md`, `w1b-7z.md`.
