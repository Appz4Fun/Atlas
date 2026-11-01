# TODO: duplicate clusters and release details

Two pieces of work: group releases that hold the same content, and show everything known about a release on its page.

## Part 1: duplicate clusters

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
- [ ] A candidate becomes a twin only when PAR2 hashes match, or article checks prove the bytes. Swapping in an article from a release that only looks the same would corrupt the download. RustyGet uses the same rule. The cost is fewer twins where a release has no PAR2.
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

## Part 2: release details

Everything below goes on the release page in the web UI, and into the Newznab API where a client can use it.

### About the NZB itself

- [ ] **Poster and email.** `releases.poster` already holds the raw From header. Split it into a name and an email, and show both. Say plainly when the address is obviously made up, because posters rarely use a real one.
- [ ] **PAR2 file count.** Count them from `files` for each release: the index file and the recovery volumes.
- [ ] **Obfuscated or not.** `releases.is_obfuscated` exists. Show it, plus the original name next to the display name, and where the display name came from (NFO, PAR2 or an inner file).
- [ ] **Repair capacity.** How much can be repaired if the download is bad: recovery blocks against total blocks, as a percentage and a size. `par2.rs` doesn't read block counts yet, so read the block size from the PAR2 main packet and count blocks from the `volNNN+MM` file names, or from the recovery packets.
- [ ] **Health.** Articles seen against articles expected, from `files.seen` and `files.expected`, so missing articles can be compared with repair capacity: "3% missing, 10% repairable".

### Encryption

- [ ] **Encrypted or not, and what type.** The listing already works out `encrypted` and `headers_encrypted`, but only a yes, maybe or no is stored. Store the type too: RAR5 or RAR4 AES, 7z AES-256, ZIP AES or ZipCrypto, and whether file names are hidden.
- [ ] **The password.** Atlas only knows whether there is one, not what it is. Look for it where posters leave it: the NFO, the subject, the file name (`{{password}}` style) and the release name. Store where it came from, and show it as "found in the NFO", because it's unverified until something tries it.
- [ ] Try a found password against the archive header, which proves it without downloading the release. Show "verified" only then.

### Parsed release tags

- [ ] Parse names the way the Kodi plugin does with PTT (parsett), so both read a name the same way. `media/parse.rs` only gets title, year, season, episode, air date and a quality score today. Either port PTT's rules to Rust or run the same test names through both and compare.
- [ ] Tags to show: resolution, Dolby Vision, HDR10, HDR10+, source (BluRay, WEB-DL, Remux), video and audio codec, channels, release group, year, edition, languages.
- [ ] **Where it was ripped from:** the streaming service tag, such as AMZN, NF, DSNP, HMAX or ATVP.
- [ ] Filter and search by these tags, so "2160p DV Remux" works as filters instead of words in a name.

### TMDB and TVDB metadata

- [ ] `media_titles` only keeps ids and a found title. Store the metadata as well: overview, genres, runtime, cast, network, status, and first air or release date.
- [ ] **Blu-ray release date:** TMDB's release dates list has physical releases by country. Show it next to the theatrical date.
- [ ] **Posters and backdrops.** Keep the image paths and show them on the release page and as thumbnails in results. The page's CSP needs `img-src` for the image host, or Atlas has to serve the images itself. Serving them keeps the UI working with no internet and stops viewers' browsers calling out, so prefer that, with a size-limited cache.
- [ ] **Links:** IMDb, TMDB, TheTVDB, Trakt and TVmaze.
- [ ] Keep requests inside each provider's rate limits, and refresh on a schedule through the existing `next_check`.

### Ratings

- [ ] Add OMDb as a provider, with its key in `config.toml`. It gives IMDb, Rotten Tomatoes and Metacritic scores from an IMDb id. Its free tier is limited to about 1,000 requests a day, so fetch per title rather than per release, and only for titles someone opens or that have recent releases.
- [ ] Add TMDB's and Trakt's own ratings, which come with calls Atlas already makes.
- [ ] Store ratings per title, with the time they were fetched. Show them on the release page and in results, and allow sorting and filtering by them.

## Open questions on clusters

1. **Do two postings with the same message-ids count as twins, or as one release listed twice?** They're the same articles, so nothing is gained by swapping between them.
2. **Should near-matches count as a cluster,** such as the same files with an extra sample or NFO? This draft leaves samples and PAR2 out of the fingerprint and requires everything else to match.
3. **Are size-only matches worth keeping** as "possible" members, kept apart from proven ones? Many releases may have no hashes at all.
