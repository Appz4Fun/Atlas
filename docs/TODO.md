# TODO

Deliberately not done, with why, so it can be picked up later.

## Research findings not built yet

The 2026-11-02 metadata and linking research lives in [research/](research/README.md). Its own list of what is left to build (probe fixes, archive and container facts, twin and alt keys, linking split posts) is [research/TODO.md](research/TODO.md).

## Result totals count releases, not rows

The total over each results page (`web::Search::total`) counts releases in the word index, or in the database for field searches. A page shows a post spread over several groups, and an upload linked by its PAR2 set, as one row, soo the total can be more than the rows. With "Hide copies" or "Hide incomplete" on, the total says "Up to": what they leave out is worked out per row as it's shown. Fix: keep in the index whether a release is folded into another (`upload_link` rows that aren't the lead; split posts need their grouping stored first), whether it's a later copy, and whether its whole post is complete, with triggers queueing a release when any of them change, and count without them.

## Lent articles are not yet checked against the set

An NZB's gaps are filled from twins and file twins by name, part count, part sizes and, where both sides were read, each shared part's decoded length and CRC32 (`segment_crc`). A lent part nobody has read is taken on the set's word: the donor's descriptor matches by `(size, md5_16k, md5)`, but nothing proves the donor's articles decode to that file. A poster who copies a set's public FileDesc packets and posts other bytes under the same names and cut would get them into the victim's NZB (the download's PAR2 check catches it, Atlas doesn't). Fix: before lending a part, read it and fold its CRC32 against the set's IFSC slice checksums (`par2_files.crc32` is the whole-file fold; the per-slice CRCs are in the IFSC packet and would have to be kept), or at least read one shared part on each side first. The same-poster rule for set ties (`release_info::linked_post_with`) closes the recovery-volume version of this.

## Linking across shards

The link step (`archive/link.rs`) finds the files of a fully obfuscated upload among the rows of its own shard, in any group. A row in another shard can't be linked from it: a step's writes go to its shard's writer only, and `upload_link` rows for a lead live in the lead's shard. Fix: let a `StepWrite` name the shard it goes to, and have the engine hand it to that shard's writer; then drop the `between ?1 and ?2` id window and look in every shard. Until then, recovery volumes of one set in several shards are tied by `par2_files.set_id` at read time only (`release_info::linked_post_with`), and a data file posted to another shard's group than its set's volumes stays unlinked.

## Nyuu stream linker

`docs/research` has a solver that links every article of one Nyuu upload from two message-ids (Node 12+ xorshift128+). It isn't ported: it's a search over the generator's state that takes seconds per upload, and the tool's clock in the id already orders files well enough for the link step. Fix: port `nyuu-id-solver` to Rust behind the body budget, run it only for uploads the clock can't place.

## Per-article obfuscated uploads

Atlas gathers these into one release per upload (`filters::gather_per_article`).

- **Pieces in a slice with too few of them stay as they were.** A slice needs 16 articles of a shape to judge them, and their first and last parts are kept as ordinary releases, because a real upload cut by a slice's edge can look the same. So some pieces become one-article releases and don't join their upload. Fix: when a stored fragment's shape, poster, and day match a gathered upload, move it in.
- **Fragments stored before this change aren't gathered.** They stay where they are. Fix: a batched `--regather` over `filters::stored_fragment`, dry run by default. Their subjects are stored, so it needs no network.
- **Two uploads can share a release.** The key is shape, poster, and day, so same-shaped uploads by one poster on one day merge. A downloader may then fail on that NZB. A poster that changes with nearly every article is left out of the key, which makes this likelier. Fix: split on article number gaps or post time gaps, which needs a look at real uploads.
- **Two same-sized files with no file counter share a file row,** so one `<file>` can list a part number twice. Fix: the same as above, or number duplicates into a second file when building the NZB.
- **They have no real name.** Nothing in the subject gives one. Fix: read a piece's yEnc `name=`, if an upload leaves it unobfuscated, or match PAR2 hashes.
- **`t=get&file=N` isn't advertised.** It isn't in the Newznab spec, and results don't list a release's files. Fix: a files attribute on results, or a `t=files` listing.
- **AI search still drops them** (`src/ai.rs`), since it has no use for a release it can't name.

## Uploads obfuscated per article, with no counter

`parse_subject` drops articles whose subject has no `(n/m)`, and `gather_per_article` only gathers ones that have it. In a 3,000-header sample of each of several movie groups on 2026-11-04 these were most of the traffic: `alt.binaries.hdtv` 95% of bytes, `movies` 94%, `misc` 71%, `multimedia.aviation` and `sleazemovies` 100% for that window. `atlas_articles_unparsable_total` and `atlas_article_bytes_unparsable_total` now count them per group and subject shape. Two shapes dominate:

- ngPost `-x`: Subject is the Message-ID's local part (32 hex), From `<random13>@ngPost.com`, every article on its own.
- One random token of 26 to 64 letters and digits, From `<random12>@<random8>.<random3>` that stays the same over a run of consecutive articles, Message-ID `<random24>@<random12>.<random3>`.

Articles are 716,800 bytes of payload (about 740 KB on the wire) except each file's last, which is smaller. Without a counter, which file an article belongs to is only in its body's yEnc header, and a downloader places an article by its `=ypart begin`, so an NZB needs the right files but not the right segment order. Next steps, in order: store the second shape as runs keyed by group, From and day (its From repeats), cut runs into files at the short last articles, then name and check them through a PAR2 index body (`docs/research/posting-behavior/FINAL-REPORT.md`, stages 2 to 4). This needs room for about as many more articles again in those groups, and so a storage decision first.

## Posts crossing midnight UTC

A post with a weak name, such as numbered files, has its poster and day in its key, so one crossing midnight UTC becomes two releases. Each part keeps its articles, and nothing is lost. Dropping the day would merge unrelated posts like a poster's daily `IMG_` sets. Fix: join a post to the previous day's release when the counter's file numbers carry on and the gap is small, which needs the release's last post time at save.

## PreDB

- `scene_archive_sql`: needs a local dump file, and none is on hand. `--predb-import` covers plain title lists.
- `predatabase` reads only the newest ten titles a poll. Its history needs a cursor and an API key.
- The WebSocket streams and the IRC scraper. The streams carry predb.club and predb.net, which are polled. IRC needs its own login.
- `predb.ovh`'s host no longer resolves. Its code stays in case it returns.

## Tests

- `tests/stop.rs` `stop_does_not_wait_for_a_stuck_request` fails now and then under heavy load, on "the slices that came back were saved": the stop can arrive before any slice is saved. It passes alone. Fix: wait for a saved slice before stopping.

## Sealed files

- The sealed log dashboard (`atlas-sealed-log`) and the backfill and realtime one (`atlas-progress`) have no screenshot and no entry on the dashboards page (`grafana/index.html`). Take them once the service and Atlas have run the metrics builds for a while, with addresses and host names replaced as for the others, then publish the page.
- Some sealed files hold more than `SEAL_MAX_SEGMENTS`, and how they got there isn't known. They're read up to `READ_MAX_SEGMENTS`, and a late article stays a row on them. Fix, if it matters: find them with `blob::count` and leave them, or split them.
