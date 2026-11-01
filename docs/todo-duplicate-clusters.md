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

Built on the \`feat/release-details\` branch. Instead of cluster tables, each release keeps two fingerprints in its shard, and its copies are looked up in every shard when its page opens. There's nothing to keep in step, and a purged release takes its row with it.

### Done

- [x] **Posted fingerprint:** sizes and MD5s from a complete PAR2 set. Equal ones are byte-identical, which makes a **twin**.
- [x] **Content fingerprint:** exact sizes of the real files, at least 1 MiB, leaving out samples, with names ignored. Equal ones with different posted files make a **pair**.
- [x] Written by the archive and PAR2 steps as releases are read.
- [x] \`atlas --fingerprints --apply\` fills in content fingerprints from stored data. It finds pairs only, because PAR2 hashes were never stored.
- [x] Web UI: "Other copies" on the release page, with TWIN and PAIR tags.
- [x] Tests: same sizes with different hashes make no twin, another encode makes no pair, packed files make no content fingerprint, copies are found across shards, and a purged release is no one's copy.

### Left

- [ ] Run \`--fingerprints\` on the real data, then measure: how many copies, the share of each kind, a hand check of a sample for false matches, and how long it takes with the indexer running.
- [ ] Twins for existing releases: their PAR2 sets have to be read again, which costs a body fetch per release. Decide whether that's worth a walk.
- [ ] Web UI: show "2 twins, 1 pair" in results. It's about 400 lookups a page, so it needs measuring first.
- [ ] Newznab API: add attributes for the kind of copy, without changing existing results.
- [ ] Search: option to collapse copies into one row, off by default.
- [ ] NZB download: when a twin has articles a release is missing, build the NZB from both.
- [ ] Encrypted archives with hidden names have no listing, so they get no content fingerprint. They can still be twins through PAR2.

## Part 2: release details

Everything below goes on the release page in the web UI. None of it is in the Newznab API yet, which is still to do.

### About the NZB itself

- [x] **Poster and email.** `releases.poster` already holds the raw From header. Split it into a name and an email, and show both. Say plainly when the address is obviously made up, because posters rarely use a real one.
- [x] **PAR2 file count.** Count them from `files` for each release: the index file and the recovery volumes.
- [x] **Obfuscated or not.** `releases.is_obfuscated` exists. Show it, plus the original name next to the display name, and where the display name came from (NFO, PAR2 or an inner file).
- [x] **Repair capacity,** as an estimate from the size of the recovery volumes against the size of the data.
- [ ] Exact repair capacity from PAR2 block counts: read the block size from the main packet.
- [x] **Health.** Articles seen against articles expected, from `files.seen` and `files.expected`, so missing articles can be compared with repair capacity: "3% missing, 10% repairable".

### Encryption

- [x] **Encrypted or not, and what type.** The listing already works out `encrypted` and `headers_encrypted`, but only a yes, maybe or no is stored. Store the type too: RAR5 or RAR4 AES, 7z AES-256, ZIP AES or ZipCrypto, and whether file names are hidden.
- [x] **The password,** from the release name, file names and subjects when the page opens, and from an NFO when the NFO step reads one. Shown as "found in the NFO, not tried".
- [ ] NFOs of well-named releases: the NFO step only reads an NFO when the release needs a name, so an encrypted release with a good name doesn't get its NFO read.
- [ ] Try a found password against the archive header, which proves it without downloading the release. Show "verified" only then.

### Parsed release tags

- [x] Parse names the way the Kodi add-on does with PTT, in `release_tags.rs`. Compared with PTT on 1,990 real names: resolution, HDR and bit depth match on all of them, other tags on 96% to 99.9%. Some differences are PTT's mistakes, such as reading `READ.NFO` as Netflix.
- [ ] Languages cover only the common names so far.
- [x] Tags to show: resolution, Dolby Vision, HDR10, HDR10+, source (BluRay, WEB-DL, Remux), video and audio codec, channels, release group, year, edition, languages.
- [x] **Where it was ripped from:** the streaming service tag, such as AMZN, NF, DSNP, HMAX or ATVP.
- [ ] Filter and search by these tags, so "2160p DV Remux" works as filters instead of words in a name.

### TMDB and TVDB metadata

- [x] Overview, genres, runtime, cast, network, status, and first air or release date, from TMDB when a release is opened, kept for 30 days in `atlas.details.db`. A title known only by IMDb or TheTVDB id is found through TMDB.
- [ ] TheTVDB's own metadata, for shows TMDB doesn't have.
- [x] **Blu-ray release date:** TMDB's release dates list has physical releases by country. Show it next to the theatrical date.
- [x] **Posters** on the release page, served by Atlas from a cache of at most 512 MB, so browsers never call out.
- [ ] Thumbnails in results, and backdrops.
- [x] **Links:** IMDb, TMDB, TheTVDB, Trakt and TVmaze.
- [x] Requests stay inside each provider's rate limit, and only titles someone opens are fetched.

### Ratings

- [x] OMDb as a provider, with its key as `media.omdb.api_key`: IMDb, Rotten Tomatoes and Metacritic scores, for titles someone opens.
- [x] TMDB's own rating.
- [ ] Trakt's rating.
- [x] Ratings stored per title with the time they were fetched, and shown on the release page.
- [ ] Ratings in results, and sorting and filtering by them.

## Open questions on clusters

1. **Do two postings with the same message-ids count as twins, or as one release listed twice?** They're the same articles, so nothing is gained by swapping between them. Today they'd show as twins.
2. **Is matching exact file sizes enough for a pair?** It's what's built, because archive headers don't reliably give a checksum for split files, and two encodes rarely match to the byte. The sample check on real data will say.
