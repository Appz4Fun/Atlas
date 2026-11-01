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

Built on the `feat/release-details` branch. Instead of cluster tables, each release keeps two fingerprints in its shard, and its copies are looked up in every shard when its page opens. There's nothing to keep in step, and a purged release takes its row with it.

### Done

- [x] **Posted fingerprint:** sizes and MD5s from a complete PAR2 set. Equal ones are byte-identical, which makes a **twin**.
- [x] **Content fingerprint:** exact sizes of the real files, at least 1 MiB, leaving out samples, with names ignored. Equal ones with different posted files make a **pair**.
- [x] Written by the archive and PAR2 steps as releases are read.
- [x] `atlas --fingerprints --apply` fills in content fingerprints from stored data. It finds pairs only, because PAR2 hashes were never stored.
- [x] Web UI: "Other copies" on the release page, with TWIN and PAIR tags, and counts in results.
- [x] Newznab API: `twins` and `pairs` attributes.
- [x] "Hide copies" filter in search, off by default, within a page.
- [x] NZB download: when a twin has articles a release is missing, an NZB that takes them from it. A twin's file counts only with the same name, part count and part sizes.
- [x] Tests: same sizes with different hashes make no twin, another encode makes no pair, packed files make no content fingerprint, copies are found across shards, and a purged release is no one's copy.

### Left

- [ ] **Prod has no archive data yet.** Its body budget is 0, so no archive or PAR2 set has been read, and `archive_inner_files` is empty. Until the budget is turned on, twins, pairs, encryption types and NFO passwords have nothing to show. On a snapshot at 09:46 on 2026-11-01, `--fingerprints` found 0 releases.
- [ ] Once there's data: how many copies, the share of each kind, a hand check of a sample for false matches, and how long lookups take with real fingerprints. Today's timings only cover an empty table: about 30 to 60 ms more per page.
- [ ] Collapse copies across a whole search. "Hide copies" only works within a page.
- [ ] Encrypted archives with hidden names have no listing, so they get no content fingerprint. They can still be twins through PAR2.

## Part 2: release details

Everything below goes on the release page in the web UI. The API has the tags and copy counts.

### About the NZB itself

- [x] **Poster and email.** `releases.poster` already holds the raw From header. Split it into a name and an email, and show both. Say plainly when the address is obviously made up, because posters rarely use a real one.
- [x] **PAR2 file count.** Count them from `files` for each release: the index file and the recovery volumes.
- [x] **Obfuscated or not.** `releases.is_obfuscated` exists. Show it, plus the original name next to the display name, and where the display name came from (NFO, PAR2 or an inner file).
- [x] **Repair capacity,** exact from PAR2 block counts once the set's been read, and otherwise estimated from sizes.
- [x] **Health.** Articles seen against articles expected, from `files.seen` and `files.expected`, so missing articles can be compared with repair capacity: "3% missing, 10% repairable".

### Encryption

- [x] **Encrypted or not, and what type.** The listing already works out `encrypted` and `headers_encrypted`, but only a yes, maybe or no is stored. Store the type too: RAR5 or RAR4 AES, 7z AES-256, ZIP AES or ZipCrypto, and whether file names are hidden.
- [x] **The password,** from the release name, file names and subjects when the page opens, and from an NFO when the NFO step reads one. Shown as "found in the NFO, not tried".
- [ ] NFOs of well-named releases: the NFO step only reads an NFO when the release needs a name, so an encrypted release with a good name doesn't get its NFO read.
- [x] Try a found password against RAR5 archives, which keep a check value in their headers, and show WORKS or WRONG. Tested on real sample archives.
- [ ] RAR4, 7z and ZIP have no such check, so testing a password there means downloading and decrypting data.

### Parsed release tags

- [x] Parse names the way the Kodi add-on does with PTT, in `release_tags.rs`. Compared with PTT on 1,990 real names: resolution, HDR and bit depth match on all of them, other tags on 96% to 99.9%. Some differences are PTT's mistakes, such as reading `READ.NFO` as Netflix.
- [ ] Languages cover only the common names so far.
- [x] Tags to show: resolution, Dolby Vision, HDR10, HDR10+, source (BluRay, WEB-DL, Remux), video and audio codec, channels, release group, year, edition, languages.
- [x] **Where it was ripped from:** the streaming service tag, such as AMZN, NF, DSNP, HMAX or ATVP.
- [x] Quick filters in results: 2160p, 1080p, DV, HDR, Remux, WEB-DL, BluRay. They search the words, then check the parsed tags, so a page can come back short.
- [x] Tags in the Newznab API: `resolution`, `video`, `audio`, `hdr`, `source`, `language`, `team`, plus `poster` and `group`.

### TMDB and TVDB metadata

- [x] Overview, genres, runtime, cast, network, status, and first air or release date, from TMDB when a release is opened, kept for 30 days in `atlas.details.db`. A title known only by IMDb or TheTVDB id is found through TMDB.
- [ ] TheTVDB's own metadata, for shows TMDB doesn't have.
- [x] **Blu-ray release date:** TMDB's release dates list has physical releases by country. Show it next to the theatrical date.
- [x] **Posters** on the release page, served by Atlas from a cache of at most 512 MB, so browsers never call out.
- [x] Thumbnails in results, for posters already kept.
- [ ] Backdrops.
- [x] **Links:** IMDb, TMDB, TheTVDB, Trakt and TVmaze.
- [x] Requests stay inside each provider's rate limit, and only titles someone opens are fetched.

### Ratings

- [x] OMDb as a provider, with its key as `media.omdb.api_key`: IMDb, Rotten Tomatoes and Metacritic scores, for titles someone opens.
- [x] TMDB's own rating.
- [x] Trakt's rating.
- [x] Ratings stored per title with the time they were fetched, and shown on the release page.
- [x] The best kept score in results.
- [ ] Sorting and filtering by ratings. Scores only exist for titles someone has opened, so this needs fetching ahead of time first.

## Open questions on clusters

1. **Do two postings with the same message-ids count as twins, or as one release listed twice?** They're the same articles, so nothing is gained by swapping between them. Today they'd show as twins.
2. **Is matching exact file sizes enough for a pair?** It's what's built, because archive headers don't reliably give a checksum for split files, and two encodes rarely match to the byte. The sample check on real data will say.
