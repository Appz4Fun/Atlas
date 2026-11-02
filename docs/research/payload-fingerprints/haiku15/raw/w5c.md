# W5C Rust fingerprint index (Haiku)
- Filters: fastbloom 0.17 (incremental, 29.8M dl), xorf 0.13 binary fuse (static; fuse8 ~9 bits/key 1/256, fuse16 ~18 bits 1.5e-5), qfilter 0.3 (deletes), cuckoofilter/sled/sanakirja stale.
- Stores: rust-rocksdb 0.25 (prefix extractor), fjall 3.1 (pure Rust LSM, young), redb 4.4, heed 0.23 (LMDB), rusqlite baseline.
- SQLite WITHOUT ROWID PK(tag,fp,file_id) clustered.
- Design: 17-byte key tag u8 | fp u64 | file_id u64; duplicates = prefix range on (tag,fp); per-sealed-segment fuse16 + tail bloom.
MY NOTE: keys are per FILE (or per interval), not per article, so volume is ~tens of millions; a composite-PK table in Atlas's existing DB is enough to start; filters only if it becomes hot.
## pass 2
- Postgres: btree only (hash = equality only, BRIN useless for random fp). Secondary: 2B rows, lookup ~100 ms when index > RAM.
- SQLite: 1e9 x 17B fine size-wise.
- 1B-key bench (bangnbx Solana post): negative reads RocksDB+bloom 3.4 us vs redb 94.6 us vs LMDB 273 us; writes RocksDB 1.7-4.3 us. fjall: no numbers (~1.25 GB filters / 1e9 keys).
- Droplet: RAM cuckoo table of MD5 fingerprints, 24 B/entry, load <50%.
