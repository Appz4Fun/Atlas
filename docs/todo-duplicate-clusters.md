# TODO: cluster duplicate releases

Group releases that hold the same content, even when they were posted differently. Today, `idx_release_unique` only stops a repeat of the same name in the same group, so two postings of one title are unrelated rows.

## The words

| Word | Meaning | Can one replace the other? |
|---|---|---|
| **Cluster** | Every release known to hold the same content. Its members are linked as pairs or twins. | Only as twins |
| **Pair** | Same content, different packaging. One is RAR5 and the other is 7z, for example. | No. Each one is a separate download, so its articles can't be swapped in. It's a fallback for the whole release. |
| **Twin** | Same content and same packaging: same container, same article count, same article sizes. | Yes |

A different encode of the same title is not the same content, so it's neither a pair nor a twin. RustyGet calls that an alt. Keeping the same meanings for "twin" and "alt" lets Atlas hand its clusters to RustyGet later.

## What Atlas already has

- `archive_inner_files`: name and size of each file inside a release, from PAR2, RAR, 7z or ZIP. `source` says which, so it also shows the container. It lists at most 256 rows per release when read back.
- `par2::Inventory` parses each file's `file_id`, `md5` and `md5_16k`, but the hashes aren't stored.
- `sevenz.rs` reads CRCs from 7z headers. These aren't stored either.
- `segments`: `part` and `bytes` for each article, and `files.expected`, so article counts and sizes can be compared without new data.
- `media_release`: season, episode, year and external ids, which can narrow down candidates.
- `release_ids`: message-id hashes per release, so a true repost of the same articles can be told apart.

## Work

### 1. Store content hashes

- [ ] Add a shard table for inner-file hashes: release, file name, size, PAR2 `md5`, `md5_16k`, and the CRC where the container gives one.
- [ ] Write it from the archive stage, where the inventory is already parsed.
- [ ] Backfill releases that were already inspected. Check the disk cost on real data first: `max_names_mb` already limits how much inner-file data is kept.

### 2. Fingerprints

- [ ] **Content fingerprint:** a hash of the sorted (inner size, hash) list, leaving out PAR2 and sample files. This one finds clusters.
- [ ] **Packaging fingerprint:** container type, file count, article count and each file's article sizes. This one separates twins from pairs.
- [ ] Store both per release, in the shard, and index them.
- [ ] Decide what to do when only sizes are known, with no hashes. See open questions.

### 3. Build clusters

- [ ] Add `dup_cluster` and `dup_member` tables: release, cluster, and kind (`pair` or `twin`), plus what proved the link.
- [ ] Clusters cross shards, because a release's shard follows its group, so matching can't stay inside one shard.
- [ ] Add a post-process stage that matches new releases as they finish, and a one-off pass for the existing backlog.
- [ ] Rerun when a release gains inner-file data or is renamed. Remove members when a release is purged.

### 4. Twin rule

- [ ] Same content fingerprint and same packaging fingerprint makes a **twin candidate**.
- [ ] A candidate becomes a twin only when PAR2 hashes match, or article checks prove the bytes. See open questions.
- [ ] Same content fingerprint with different packaging makes a **pair**.
- [ ] Encrypted archives: the inner hashes may not be readable, and the same files encrypted twice differ on the wire. Never call these twins on size alone.

### 5. Use it

- [ ] Web UI: show "2 twins, 1 pair" on a result, and list the members on the release page.
- [ ] Newznab API: add attributes for the cluster id and kind, without changing existing results.
- [ ] Search: option to collapse a cluster into one row, off by default.
- [ ] NZB download: when a twin has articles a release is missing, build the NZB from both.

### 6. Check it

- [ ] Tests: a RAR5 and 7z of the same files make a pair. Two identical 7z postings make a twin. Same sizes with different hashes make nothing. Purging a member keeps the cluster right.
- [ ] Measure on a snapshot before turning it on: how many clusters, the share of each kind, and a hand check of a sample for false matches.
- [ ] Measure how long the stage takes and how much disk it uses, with the indexer running.

## Open questions

1. **Is matching article size and count enough to call two releases twins?** This draft says no. Equal sizes make a candidate, and a hash has to prove it, because swapping in an article from a release that only looks the same would corrupt the download. It's the same rule RustyGet uses. The cost is fewer twins where a release has no PAR2.
2. **Do two postings with the same message-ids count as twins, or as one release listed twice?** They're the same articles, so nothing is gained by swapping between them.
3. **Should near-matches count as a cluster,** such as the same files with an extra sample or NFO? This draft leaves samples and PAR2 out of the fingerprint and requires everything else to match.
4. **Are size-only matches worth keeping** as "possible" members, kept apart from proven ones? Many releases may have no hashes at all.
