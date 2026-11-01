# TODO

Deliberately not done, with why, so it can be picked up later.

## Per-article obfuscated uploads

Atlas gathers these into one release per upload (`filters::gather_per_article`).

- **Pieces in a slice with too few of them stay as they were.** A slice needs 16 articles of a shape to judge them, and their first and last parts are kept as ordinary releases, because a real upload cut by a slice's edge can look the same. So some pieces become one-article releases and don't join their upload. Fix: when a stored fragment's shape, poster, and day match a gathered upload, move it in.
- **Fragments stored before this change aren't gathered.** They stay where they are. Fix: a batched `--regather` over `filters::stored_fragment`, dry run by default. Their subjects are stored, so it needs no network.
- **Two uploads can share a release.** The key is shape, poster, and day, so same-shaped uploads by one poster on one day merge. A downloader may then fail on that NZB. A poster that changes with nearly every article is left out of the key, which makes this likelier. Fix: split on article number gaps or post time gaps, which needs a look at real uploads.
- **Two same-sized files with no file counter share a file row,** so one `<file>` can list a part number twice. Fix: the same as above, or number duplicates into a second file when building the NZB.
- **They have no real name.** Nothing in the subject gives one. Fix: read a piece's yEnc `name=`, if an upload leaves it unobfuscated, or match PAR2 hashes.
- **`t=get&file=N` isn't advertised.** It isn't in the Newznab spec, and results don't list a release's files. Fix: a files attribute on results, or a `t=files` listing.
- **AI search still drops them** (`src/ai.rs`), since it has no use for a release it can't name.

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

- Some sealed files hold more than `SEAL_MAX_SEGMENTS`, and how they got there isn't known. They're read up to `READ_MAX_SEGMENTS`, and a late article stays a row on them. Fix, if it matters: find them with `blob::count` and leave them, or split them.
