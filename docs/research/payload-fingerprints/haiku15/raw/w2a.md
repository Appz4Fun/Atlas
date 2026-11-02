# W2A indexer prior art (Haiku)
- NNTmux par_hashes(releases_id, hash = PAR2 hash_16k), written in ReleaseFile.php via Par2Processor; NameFixingService::hashCheck matches other releases with predb_id>0 within ±5% size (proc_hash16k flag). Name-fix only, no twin table.
- release_files.crc32 -> crcCheck (±5%), media_infos.unique_id -> uidCheck (±10%).
- Predb has no hash columns; PredbHash model unverified.
- usenet-dl duplicates.rs: SHA-256 of NZB, job name.
- r/newznab 2013 "a worthwhile modification": per-file hash index so clients ask "which posts contain this file".
- github.com/scryer-media/rarpar crates/par2-rs/src/evidence.rs "ContiguousAssembly" w/ hash_16k: unread.
