# Part 2: Grouping

Depends on: Part 1. Owns `src/collections.rs`, `data/collection_regexes.tsv`, `scripts/collection_regexes_to_tsv.py`. Base commit for every Atlas line cited here: `de598b6`, with Part 1 merged on top, so a line near a Part 1 edit may have moved by a few. Reference paths are in newznab-tmux at the commit pinned in [00-overview.md](00-overview.md).

Every code block in this plan was compiled and run on an isolated copy of `de598b6` with Part 1 applied, using `cargo test --locked` and `cargo clippy --all-targets --locked -- -D warnings`. `cargo fmt` may rewrap it; run `just format` before each commit.

## 1. Goal

A multi-file post becomes one release. Today Atlas keys a release on one file's stripped name, so a post whose files have different names becomes many releases, and a post of 50 numbered images becomes 50. After this part, articles whose subjects say they belong to one post land in one release, across slices, servers and days, using upstream's 602 per-group regexes and its generic cleaner. Existing rows aren't rewritten, and no setting changes.

## 2. Non-goals

- Merging or renaming existing rows. A post straddling the upgrade stays split at the file level, see 4.5.
- Merging posts with no file counter and no group regex. A lone `"IMG_0001.jpg" yEnc (1/1)` stays its own release. Nothing in the subject says otherwise, and such a merge would have no bound.
- Posts where every file has its own random name. Upstream can't group those either.
- Collections across groups. Upstream merges cross-posts through Xref; Atlas's identity is per group today (`src/store.rs:143`) and stays that way.
- Better names. Part 5 owns `display_name`. FTS indexes `name` and `display_name` only (`src/store.rs:146-147`), so a search for a file other than the one named doesn't find a merged release until Part 5 has run.
- Changing completeness rules, or the upstream `totalfiles + 1` tolerance. See decision 11.
- `fancy-regex`. See 4.2.

## 3. Source map

| Upstream | Take | Leave behind |
|---|---|---|
| `database/seeders/CollectionRegexesTableSeeder.php` | All 602 rows through the converter | `description`: example release names nothing reads |
| `app/Services/Binaries/CollectionHandler.php:117-122,215-220` | The key: `$collMatch['name'].$totalFiles`, hashed at `:129,242`. No poster in it, no group | Collections, binaries and parts tables, their upserts, the row lifecycle |
| `app/Services/Binaries/HeaderStorageService.php:428-433,632-640` | File total from `FILE_COUNTER_PATTERN` over the base, 0 when none | FFI batches |
| `app/Services/Binaries/HeaderParser.php:20,25,199-200` | The base: up to ` (n/m)`, plus ` yEnc` when the subject has none. The counter pattern, through Part 1's `COUNTER_RE` | |
| `app/Services/CollectionsCleaningService.php:162-178` | Regexes first, generic after | |
| `CollectionsCleaningService.php:66-78` | The file-extension pattern `e0` | `REGEX_END`, unused by this path |
| `CollectionsCleaningService.php:187-204,220-244,297-324,380-401,452-460,496-502,536-539` | Generic and music cleaners, `normalizeString`. The digit-stripping alternatives run only on numbered files, see 4.1 | `mb_convert_encoding` over every encoding: Atlas's subjects are already UTF-8 |
| `app/Services/RegexService.php:291-315,333,354-378` | Group's rules in `ordinal, group_regex` order, first non-empty output wins, named groups joined in name order, without `reqid` and `parts` names | The 15-minute cache, the admin CRUD |
| `RegexService.php:410-418` | Read for decision 4: the one upstream hash with a poster, used by its regex test tool, not by ingest | |
| `app/Services/Binaries/BinaryHandler.php:568-571,590-594` | Read for decision 12 | File identity by number |
| `CollectionHandler.php:749-751` | Read for decision 11 | |
| `app/Services/ReleaseProcessingService.php:1083-1105` | Read for decision 4 | The stale path: a collection older than `collectionDelayTime` is released as it stands, with `totalfiles` reset to what arrived. A wrong merge from digit stripping lasts hours upstream. Atlas's identity lasts forever and has no such sweep, so it can't lean on this, and keeps a post's digits in its key instead |

## 4. Atlas changes

### 4.1 What decides the collection

For each parsable article, a pure function of its subject, group, poster, date and parsed name. `base` is `subject_base(subject)`, `(n, m)` is the file counter in the base. The first row that applies decides:

| # | The base says | Result |
|---:|---|---|
| 1 | Counter with `m < 2`, `n > m`, or `m > 10,000` | No collection. Stays by its parsed name, as today |
| 2 | Counter, and the file is **numbered** | Key = group, upstream's generic cleaning with the digits gone, `m`, **poster, day** |
| 3 | A regex of the group matches | Key = group, the regex's output, `m` (0 when no counter). Poster and day too when the output has under 16 letters and digits |
| 4 | Counter | Key = group, cleaning with the post's digits left in, `m`. Poster and day too when the cleaned name has under 16 letters and digits |
| 5 | Neither | No collection |

Names in a key are lowercase, and a `yEnc` left in a name isn't counted. Row 4's cleaning is upstream's generic cleaner with its digit-stripping alternatives taken out, or upstream's music cleaner in a `.flac`, `.lossless`, `.mp3`, `.music`, `.sounds` group or for an audio file. That cleaner leaves what's in quotes out, keeping only the first word.

**Numbered** means the digits before a file's extension count the files of a post, rather than tell the post. `numbered(release_name, n, m)` is true when the parsed name ends in 1–5 digits and then a real extension (2–4 letters and digits, at least one a letter), and:

- an image (`jpg`, `jpeg`, `png`, `gif`, `bmp`, `webp`, `tif`, `tiff`) with `m >= 2`, whatever the numbers are, since a camera doesn't start at 1;
- any other file except audio, with `m >= 3` and those digits equal to `n`.

A `.rar`, `.par2`, `.nfo`, `.sfv` or `.0NN` file is never numbered: `RELEASE_SUFFIX_RE` has already taken that extension off the parsed name.

**Why `n` has to equal the digits.** These two subjects can be identical:

- `[1/9] "Show.S01E01.mkv"`, the first of a 9-episode pack;
- `[1/9] "Show.S01E01.mkv"`, an episode's post with 8 par2 files.

No function of one article can tell them apart, and the key mustn't look at neighbours (4.8). But the rest of each post differs. In the pack, `n` tracks the digits on every file. In an episode's post, it almost never does: `[1/9] "Show.S01E02.mkv"` has `n = 1`, so it keeps its digits and joins its own par2 files. So episode posts stay apart and packs merge, and the one overlap is `[1/9] "Show.S01E01.mkv"` in an episode's post, whose mkv then sits apart from its own par2 files. That's what happens today, and it can't reach another upload's files except at the same `n` (decision 6).

The required shapes, each pinned by a test:

| Articles | Releases | How |
|---|---:|---|
| `Name.2019.mkv` + `.nfo`, `Name.2021.mkv` + `.nfo`, `[n/2]`, two posters, two days | 2 | `m < 3`: digits stay |
| One poster, one day, `[n/2]`: `Show.S01E01.mkv` + `.nfo`, `Show.S01E02.mkv` + `.nfo` | 2 | `m < 3`: digits stay |
| One poster, `[n/10]`, `Show.S01E01.mkv` to `Show.S01E10.mkv` | 1 | `n` equals the digits |
| `[n/12]` album tracks | 1 | The music cleaner, by extension, and `[n/12] Album.Name.Track07.flac` too |
| `[n/50]` numbered images, from `IMG_0001` or `IMG_4831` | 1 | An image |
| `Name.part01.rar` to `.part20.rar`, `Name.2019.part01.rar` to `.part20.rar` | 1 | `.part` goes, the year stays |

Cleaned output on synthetic subjects, from the prototype:

| Subject | Row | Cleaned |
|---|---:|---|
| `[1/4] - "Some.Show.S01.1080p-GRP.part1.rar" yEnc (1/2)` | 4 | `- Some.Show.S01.1080p-GRP yEnc` |
| `[3/4] - "Some.Show.S01.1080p-GRP.nfo" yEnc (1/1)` | 4 | The same |
| `[1/2] "Name.2019.mkv" yEnc (1/9)` | 4 | `Name.2019 yEnc` |
| `[1/20] "Name.2019.part01.rar" yEnc (1/9)` | 4 | `Name.2019 yEnc` |
| `[1/10] "Show.S01E01.1080p.WEB-DL.x264-GRP.mkv" yEnc (1/900)` | 4 | `Show.S01E01.1080p.WEB-DL.x264-GRP yEnc`: scene-named episodes stay apart |
| `[01/50] "IMG_0001.jpg" yEnc (1/1)` | 2 | `IMG_ yEnc` |
| `[7/10] "Show.S01E07.mkv" yEnc (1/900)` | 2 | `Show.S01E yEnc` |

These are key material, unfit to show: that's why the key isn't the name, see 4.4.

### 4.2 Lookaround and backreferences: there are none

The brief expected about 6 regexes needing them. Checked with Part 5's tokenizer and a direct scan of the converted file: **0 lookaround, 0 backreferences, 0 atomic groups, 0 recursion in all 602.** The 6 came from a crude check for `[+*?}]\+`: 5 are `\?+`, a literal `?` repeated (rules 13, 304, 423, 478, 488), and 1 is a real possessive, `\d*+` (rule 251).

The converter is Part 5's `convert`, copied so both scripts treat PCRE alike. It raises an error if a rule ever needs backtracking, so a future seeder can't slip one in. It changes 5 rules:

| Rules | PCRE | Becomes | Why |
|---|---|---|---|
| 235, 376 | `[` inside a class | `\[` | A nested class to the crate |
| 358, 400 | `{` not a quantifier | `\{` | An error to the crate |
| 251 | `\d*+` | `\d*` | The crate reads `x*+` as `(x*)+`. The next character is a literal `/`, so greedy and possessive match the same text |

The other 597 compile unchanged. Rust and PCRE also differ on `\<`, `\>` and class operators `&&`, `--`, `~~`; a scan found none of them in this file, and `convert` refuses the class operators.

The generic cleaner has one lookahead, `\d{3}(?=([\s"]))` (`CollectionsCleaningService.php:235`). It becomes `\d{3}[\s"]`: the extra character it takes is a quote or a space, and everything taken becomes a space, which `normalize` collapses.

### 4.3 Identity

`unique(name, group_name)` (`src/store.rs:143`) stays, and so does its upsert. A new table maps a collection to its row:

```sql
create table if not exists collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid;
```

`key` is the shard's keyed hash (`IdKey::hash`, `src/store.rs:707`) of the key string. In a save, a release with a collection looks up its row's name there, then goes through the same upsert as everything else (`:1380-1395`) under that name. A new row's mapping is written right after the upsert. Part 3's `name_source` guard on that upsert therefore covers collections with no second statement.

### 4.4 What `releases.name` holds

Stated plainly, for every later part:

| Row | `releases.name` |
|---|---|
| **Old row**, saved before this part | Unchanged: `parse_subject`'s `release_name`. The file name with `.partN`, `.rNN`, `.volN+M`, `.par2`, `.nfo`, `.sfv`, `.rar` and `.0NN` stripped repeatedly (`src/parser.rs:75,92-99`), with ` [m]` added when two characters or fewer are left (`:118-122`) |
| **New row, no collection** | Exactly the same |
| **New row, with a collection** | One `release_name` of that form: the **shortest** among the collection's articles in the slice that created the row, the first seen on a tie. If another release in the group already has that name, then that name plus the collision suffix below |
| **Article in a file started before this part** | Goes into the old row, under the old row's name (4.5) |

It's never updated afterward (Part 3's rule 4). It never holds upstream's key text: no `yEnc`, no quotes, no counters unless the file name had them. It's always some file's parsed name, which is what every row holds today. So `Title.mkv` is possible, as it is today, and `Title` beats it when both arrive in one slice.

**The collision suffix, exactly:** a space, `[`, exactly 8 ASCII digits, `]`, at the end of the name, so the regex ` \[\d{8}\]$`. The digits are `key.hash(collection).rem_euclid(100_000_000)`, zero-padded, or the next free number when another row has that name. Nothing else in Atlas writes that form. `a [4]`, from `parse_subject`, has fewer digits, and a name like `Movie.Name [2019]` has 4.

What that means for each consumer:

- **Part 3** `judge`: `name` feeds `stored_fragment` only. The upsert key stays `(name, group_name)`.
- **Part 4** `looks_random(stem(release.name))`: same forms. A suffixed name ends in `]`, so `stem` leaves it whole and the test fails, but the row's `is_obfuscated` comes from the unsuffixed name, and Part 4 checks that first.
- **Part 5** `decide`'s posted-file tier reads `files.filename`, not `name`, and gains from this part: one release now holds all of a post's files. `current_name` gets the forms above. Part 5 leaves a posted `Title.mkv` as it is, because its tidy applies only to rows with a `display_name` or the collision suffix (its decision 25). `clean_candidate` strips exactly the suffix above (its decision 26), so a suffixed name is tidied to the posted file's name, which becomes `display_name`. `is_obfuscated` is computed on the unsuffixed name before saving (`group_articles_by`, and `split_old_rows` for a split piece), so the stored flag is right even though Part 5's check skips names with spaces. It also runs its rules on `collections::subject_base`.
- **Part 6** `nominate` keeps a `name =` probe for the title itself, which hits a new row when any file of the post in the first slice parses to it: `T.part01.rar`, `T.nfo` and `T.par2` all give `T`. Hashes go through FTS tokens, and a full title under `T.mkv` or `T [12345678]` is found through its FTS phrase, then `accepts`. What's left is in its "What it still misses": a short title under any form but an exact `name`, and a title only in a file that isn't the row's `name`. That second one is what this part adds to, since a merged post has one `name` for all its files. These are false negatives only, and `find`'s posted-file tier still matches in the step's own run. Old rows are unchanged.
- **Search and API:** show `coalesce(display_name, name)` (`src/search.rs:33`), so a merged post shows one file's parsed name until Part 5 improves it. A merged plain-named pack shows `Show.S01E03.mkv` for the whole pack. Part 5 and 6 leave episode names out when files hold 2+ distinct episodes, so this stays until a better name arrives.

### 4.5 A post straddling the upgrade

A shard records where collections started: `meta.grouping_from` = `max(id) + 1`, written once, when the first indexer of this build runs (`store::start_grouping`, called from `run_servers` before any pass). Only the indexer sets it. Opening a shard doesn't: `migrate_shard` runs from `db::create_db_at` (`src/db.rs:207,314-316`), which the menu calls, and the overview leaves restarting the indexer as a later choice. An older indexer may still be writing by file name while this build's menu has the shard open, and those rows must count as old. Two indexers can't overlap, and `Ids` hands a new process a block above all earlier ones (`src/store.rs:258-272`), so every row below the mark is old and everything that indexer saves is at or above it. Until the mark exists it reads as 1, which means no probes.

For a release with a collection, on a shard with `grouping_from > 1`, each distinct `release_name` among its articles is probed:

```sql
select 1 from releases where name = ? and group_name = ? and id < ? and poster = ?
```

- **A hit:** those articles go into that old row through the plain upsert, exactly as today, and their file keeps growing there.
- **No hit:** they go to the collection.

Why this and not adopting the old row into the collection:

- **The file stays whole.** If half a file's parts are in an old row and half in a new one, neither is usable.
- **A reindexed article is still a duplicate.** Dedupe is per file (`add_segment`, `src/store.rs:628`). A sweep or second server re-reading a range indexed before the upgrade would otherwise store the whole post a second time under a new release.
- **No rewrite.** Nothing moves. Adopting would still leave the other 19 old rows of a 20-file post behind.
- **The poster is checked** so that a new post doesn't join an old stranger's `IMG_0001.jpg`.

What stays split, accepted: a 50-file post with 20 files indexed before the upgrade stays 20 old rows and one new release of 30 files. It's the same as today, for those 20. Each file is whole, and new posts are not affected.

### 4.6 What the rest of the code does

| Code | Change |
|---|---|
| `parse_subject`, `tests/parser_parity.rs`, `tests/fixtures/subjects.*` | **None.** The parser's output doesn't change, so the parity test and its fixtures stay as they are, untouched |
| `parser::group_articles` (`src/parser.rs:176`) | Same behavior, now `group_articles_by(articles, \|_, _\| None)`. The old path stays reachable: `examples/bench_writes.rs --by-name` and the tests in `src/parser.rs` use it |
| `remap_bracket_parts` (`:129`) | None. It runs per release per slice, by filename. A file's articles stay in one release, since the counter's digits are stripped from the key. Test: `bracket_parts_still_become_parts_inside_a_collection` |
| `is_complete` (`:216`) | None. It splits by filename and compares the file count to `file_total`. `m` is in the key, so one collection has one `m` |
| `files`, `unique(release_id, filename)` (`src/store.rs:175`) | None. A merged release has more rows. Two uploads under one weak key with the same filename would share a row, which is why a weak key has poster and day |
| `release_complete` (`:753`) | None. It scans all of a release's files per save. `MAX_COLLECTION_FILES` bounds that scan at 10,000 rows for a collection |
| Sealing (`src/store.rs:805-902`, `src/blob.rs`) | None. It's per file, and a merged release just has more files |
| `release_ids` (`:218`) | None, but more of it. A release is indexed once it has a second file, so a newly merged post pays 1 lookup and 1 row per article, about 30 bytes. See section 9 |
| `by_file` (`:1405-1412`) | **Fixed.** A list searched per article is quadratic in a release's files, and a collection makes that reachable. It becomes an `IndexMap` |
| `name_sources` (`src/indexer.rs:1640`) | **Capped at 8.** `first_names` tries one job's sources one after another (`src/nntp.rs:2098`). 5,000 nfos in one release would otherwise stall a slice, where as separate releases they ran concurrently. It can only lower body load |
| Purge (`store::purge_incomplete`, `:1652`) | Also clears `collections`. Part 3's `delete_release` needn't: see 5 |

### 4.7 Posters and uploads

**What upstream's hash holds.** `CollectionHandler.php:122,129`: `sha1(cleaned name . total files)`. No poster, no group, no date. Upstream's collection rows are short-lived: complete ones become releases and go away, and stale ones are released as they stand (`ReleaseProcessingService.php:1083-1105`), so a key is reused safely later. Atlas's identity lasts forever, so a key that's fine upstream would here gather every `DSC` post ever made, and every `Name.2019` and `Name.2021`.

**Why the poster isn't always in the key.** Random per-article `From` is real: upstream measured 199 distinct in 202 rows (`BUGS.md:480`). With the poster in every key, each such article of a real file would be its own release. For a long name, the name already tells the post, and two posters with one long name and one file count are a repost of the same thing, which Atlas merges today.

**Why it's in the key for short names and numbered files.** `DSC`, `IMG_` and a music cleaner's few characters don't tell a post, and a long name with its digits gone can't either. Poster and day keep them apart. A file numbered within its post is always under this rule, whatever its length:

| Case | Result | Test |
|---|---|---|
| 50 posters, each `[n/5] "DSC000n.jpg"`, same minute | 50 releases of 5 | `unrelated_posters_stay_apart` |
| One poster, two names | 2 | `one_posters_two_posts_stay_apart_where_the_subject_tells` |
| One poster, one name, `[1/5]` and `[1/7]` | 2, `m` differs | Same |
| One poster, `[1/5] "DSC.."` on two days | 2, the day differs | Same |
| Two posters, `[n/5] "Holiday.Album.Spain.000n.jpg"` | 2: numbered, so the length doesn't excuse them | Same |
| One poster, `[n/5] "DSC.."` twice in one day | **1, with 10 files, never complete.** The subject doesn't tell them apart | `what_stays_merged_or_split_on_purpose`, decision 5 |
| A long name, a new `From` on every article | 1 | `a_long_name_with_a_new_poster_per_article_stays_together` |
| The same post on two servers | 1: no server-specific value is in the key | `the_key_is_the_same_on_any_server_and_slice` |

The day is the first 10 bytes of `Article.date`, the poster's wall clock (`to_iso_date`, `src/dates.rs`). It's cheap, and the same on every server.

### 4.8 Slice boundaries

The key depends on one article and nothing near it, so it's the same in any slice, on any server, in any order. Slices meet through the `collections` row, including two slices of one post in one write transaction. The one thing that looks at a slice is the row's first name, 4.4, which doesn't affect identity. `remap_bracket_parts` keeps its existing per-slice behavior.

### 4.9 The code

Create `src/collections.rs`; add `pub mod collections;` to `src/lib.rs` after `chunks`.

```rust
//! Which articles belong to one post, whatever its files are called: the
//! collection a subject belongs to, from per group regexes with a generic
//! fallback. Ported from newznab-tmux (`CollectionsCleaningService`,
//! `CollectionHandler`, `RegexService`, the `collection_regexes` seed), GPLv3.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use indexmap::IndexMap;
use regex::{Regex, RegexBuilder};

use crate::filters::COUNTER_RE;
use crate::parser::{Article, ParsedSubject, Release, group_articles_by};

/// a post claiming more files than this isnt merged: its files stay releases of
/// their own, as a forged counter cant make one release of a million files
pub const MAX_COLLECTION_FILES: i64 = 10_000;

/// letters and digits a cleaned name needs to tell a post by itself; under
/// this the poster and the day have to help (`IMG_`, `DSC`)
const STRONG_NAME: usize = 16;

const IMAGE_EXT: [&str; 8] = ["jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff"];
const AUDIO_EXT: [&str; 9] = ["mp3", "flac", "m4a", "ogg", "wav", "wma", "ape", "aac", "opus"];

/// id, ordinal, group_regex, flags, pattern, in the order they are tried. Made by
/// scripts/collection_regexes_to_tsv.py.
const TABLE: &str = include_str!("../data/collection_regexes.tsv");

struct Row {
    /// its line in the table: upstream's order, over every group_regex that matches
    order: usize,
    id: i32,
    case_insensitive: bool,
    regex: &'static str,
}

/// each distinct group_regex with the rows under it
static BY_GROUP_REGEX: LazyLock<Vec<(Regex, Vec<Row>)>> = LazyLock::new(|| {
    let mut by: IndexMap<&str, Vec<Row>> = IndexMap::new();
    for (order, line) in TABLE.lines().filter(|l| !l.starts_with('#')).enumerate() {
        let mut cols = line.split('\t');
        let (Some(id), Some(_ordinal), Some(group), Some(flags), Some(regex)) =
            (cols.next(), cols.next(), cols.next(), cols.next(), cols.next())
        else {
            continue;
        };
        let row = Row { order, id: id.parse().unwrap_or(0), case_insensitive: flags.contains('i'), regex };
        by.entry(group).or_default().push(row);
    }
    by.into_iter().filter_map(|(group, rows)| Some((Regex::new(group).ok()?, rows))).collect()
});

/// One group's regexes in upstream's order, compiled the first time it comes up.
type Compiled = Arc<Vec<Regex>>;
static COMPILED: LazyLock<Mutex<HashMap<String, Compiled>>> = LazyLock::new(Default::default);

fn compiled(group: &str) -> Compiled {
    if let Some(found) = COMPILED.lock().unwrap().get(group) {
        return found.clone();
    }
    let mut rows: Vec<&Row> =
        BY_GROUP_REGEX.iter().filter(|(g, _)| g.is_match(group)).flat_map(|(_, rows)| rows).collect();
    rows.sort_by_key(|r| r.order);
    // outside the lock: two threads compiling one group at once is only waste
    let regexes = rows.iter().filter_map(|row| {
        let built = RegexBuilder::new(row.regex).case_insensitive(row.case_insensitive).build();
        built.inspect_err(|e| println!("collection regex {} doesnt compile: {e}", row.id)).ok()
    });
    let list: Compiled = Arc::new(regexes.collect());
    COMPILED.lock().unwrap().insert(group.to_string(), list.clone());
    list
}

/// `<base> (12/45)`
static BASE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\(\d+\s*/\s*\d+\)").unwrap());

/// What upstream's regexes run on: the subject up to its part counter, with a
/// ` yEnc` when the subject has none. A subject with no part counter is its own base.
pub fn subject_base(subject: &str) -> Cow<'_, str> {
    let base = BASE_RE.captures(subject).and_then(|c| c.get(1)).map_or(subject.trim(), |m| m.as_str());
    if subject.as_bytes().windows(4).any(|w| w.eq_ignore_ascii_case(b"yenc")) {
        Cow::Borrowed(base)
    } else {
        Cow::Owned(format!("{base} yEnc"))
    }
}

/// (file number, files) of a base's counter
fn file_counter(base: &str) -> Option<(i64, i64)> {
    let c = COUNTER_RE.captures(base)?;
    Some((c[1].parse().ok()?, c[2].parse().ok()?))
}

const FILE_SIZES: &str = r#"\d{1,3}([,.\/])\d{1,3}\s([kmg])b|(])?\s\d+KB\s(yENC)?|"?\s\d+\sbytes?|[- ]?\d+([.,])?\d+\s([gkm])?B\s-?(\s?yenc)?|\s\(d{1,3},\d{1,3}\s\{K,M,G\}B\)\s|yEnc \d+k$|\{\d+ yEnc bytes\}|yEnc \d+ |\(\d+ ?([kmg])?b(ytes)?\) yEnc$"#;
const MUSIC_FILE_SIZES: &str = r#"\d{1,3}([,.\/])\d{1,3}\s([kmg])b|(])?\s\d+KB\s(yENC)?|"?\s\d+\sbytes?|[- ]?\d+[.,]?\d+\s([gkm])?B\s-?(\s?yenc)?|\s\(d{1,3},\d{1,3}\s\{K,M,G\}B\)\s|yEnc \d+k$|\{\d+ yEnc bytes\}|yEnc \d+ |\(\d+ ?([kmg])?b(ytes)?\) yEnc$"#;
const MARKERS: &str =
    r#"AutoRarPar\d{1,5}|\(\d+\)\s?yEnc|\d+(Amateur|Classic)| \d{4,}[a-z]{4,} |.vol\d+\+\d+|.part\d+"#;
const NUMBERING: &str = r#"((( \(\d\d\) -|(\d\d)? - \d\d\.|\d{4} \d\d -) | - \d\d-| \d\d\. [a-z]).+| \d\d of \d\d| \dof\d)\.mp3"?|([)(\[\s])\d{1,5}(\/|([\s_])of([\s_])|-)\d{1,5}([)\]\s$:])|\(\d{1,3}\|\d{1,3}\)|[^\d]{4}-\d{1,3}-\d{1,3}\.|\s\d{1,3}\sof\s\d{1,3}\.|\s\d{1,3}\/\d{1,3}|\d{1,3}of\d{1,3}\.|^\d{1,3}\/\d{1,3}\s|\d{1,3} - of \d{1,3}"#;
const MUSIC_NUMBERING: &str = r#"((( \(\d\d\) -|(\d\d)? - \d\d\.|\d{4} \d\d -) | - \d\d-| \d\d\. [a-z]).+| \d\d of \d\d| \dof\d)\.mp3"?|([(\[\s])\d{1,4}(\/|([\s_])of([\s_])|-)\d{1,4}([)\]\s$:])|\(\d{1,3}\|\d{1,3}\)|-\d{1,3}-\d{1,3}\.|\s\d{1,3}\sof\s\d{1,3}\.|\s\d{1,3}\/\d{1,3}|\d{1,3}of\d{1,3}\.|^\d{1,3}\/\d{1,3}\s|\d{1,3} - of \d{1,3}"#;
// upstream has `\d{3}(?=([\s"]))` where this has `\d{3}[\s"]`: the character
// it now takes is a quote or a space, and what is taken becomes a space
const EXTENSIONS: &str = r#"(-? [a-z0-9]+-?|\(?\d{4}\)?([_-])[a-z0-9]+)\.jpg"?| [a-z0-9]+\.mu3"?|((\d{1,3})?\.part(\d{1,5})?|\d{1,5} ?|sample|- Partie \d+)?\.(7z|\d{3}[\s"]|avi|diz|docx?|epub|idx|iso|jpg|m3u|m4a|mds|mkv|mobi|mp4|nfo|nzb|par(\s?2|")|pdf|rar|rev|rtf|r\d\d|sfv|srs|srr|sub|txt|vol.+(par2)|xls|zip|z{2,3})"?|(\s|(\d{2,3})?-)\d{2,3}\.mp3|\d{2,3}\.pdf|\.part\d{1,4}\."#;
// EXTENSIONS without what takes the digits before an extension: `\d{1,5} ?`,
// `(\d{1,3})?` before `.part`, and the `.mp3` and `.pdf` ones. Upstream's row
// is gone within hours, Atlas's stays: `Name.2019` and `Name.2021` must stay two posts
const EXTENSIONS_KEEPING_DIGITS: &str = r#"(-? [a-z0-9]+-?|\(?\d{4}\)?([_-])[a-z0-9]+)\.jpg"?| [a-z0-9]+\.mu3"?|(\.part(\d{1,5})?|sample|- Partie \d+)?\.(7z|\d{3}[\s"]|avi|diz|docx?|epub|idx|iso|jpg|m3u|m4a|mds|mkv|mobi|mp4|nfo|nzb|par(\s?2|")|pdf|rar|rev|rtf|r\d\d|sfv|srs|srr|sub|txt|vol.+(par2)|xls|zip|z{2,3})"?|\.part\d{1,4}\."#;
const LEFTOVER_EXTENSION: &str = r#"([\-_](proof|sample|thumbs?))*(\.part\d*(\.rar)?|\.rar|\.7z|\.par2)?(\d{1,3}\.rev"|\.vol\d+\+\d+.+?"|\.[A-Za-z0-9]{2,4}"|")"#;

fn insensitive(p: &str) -> Regex {
    RegexBuilder::new(p).case_insensitive(true).build().unwrap()
}

/// upstream's generic cleaner, for files numbered within a post (`numbered`)
static GENERIC_NUMBERED: LazyLock<[Regex; 5]> =
    LazyLock::new(|| [FILE_SIZES, MARKERS, NUMBERING, EXTENSIONS, LEFTOVER_EXTENSION].map(insensitive));
/// the same with a post's own digits left in
static GENERIC: LazyLock<[Regex; 5]> =
    LazyLock::new(|| [FILE_SIZES, MARKERS, NUMBERING, EXTENSIONS_KEEPING_DIGITS, LEFTOVER_EXTENSION].map(insensitive));
static MUSIC: LazyLock<[Regex; 5]> =
    LazyLock::new(|| [MUSIC_NUMBERING, r#"".+""#, EXTENSIONS, MUSIC_FILE_SIZES, MARKERS].map(insensitive));
static MUSIC_GROUP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(flac|lossless|mp3|music|sounds)").unwrap());
static MUSIC_SUBJECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(\w{10,}-[a-zA-Z0-9]+ ")\d\d-.+?" yEnc$"#).unwrap());
static SHORT_MUSIC: LazyLock<Regex> = LazyLock::new(|| insensitive(r"^[\-a-z0-9$ ]{1,7}yEnc$"));
static FIRST_QUOTED_WORD: LazyLock<Regex> = LazyLock::new(|| insensitive(r#".*("[A-Z0-9]+).*?""#));
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"".+?""#).unwrap());
static LETTERS_OR_EXTENSION: LazyLock<Regex> = LazyLock::new(|| insensitive(&format!("[a-z0-9]|{LEFTOVER_EXTENSION}")));
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s\s+").unwrap());

fn normalize(s: &str) -> String {
    SPACES.replace_all(s, " ").trim().to_string()
}

fn replace_each(patterns: &[Regex], base: &str) -> String {
    let mut out = Cow::Borrowed(base);
    for p in patterns {
        if let Cow::Owned(s) = p.replace_all(&out, " ") {
            out = Cow::Owned(s);
        }
    }
    normalize(&out)
}

/// the extension of a parsed name, lowercase. `RELEASE_SUFFIX_RE` already took
/// `.rar`, `.par2`, `.nfo` and the like off, and a year or `S01E01` isnt one.
fn extension(release_name: &str) -> Option<(&str, String)> {
    let (stem, ext) = release_name.rsplit_once('.')?;
    let fits = (2..=4).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic());
    fits.then(|| (stem, ext.to_ascii_lowercase()))
}

fn is_audio(release_name: &str) -> bool {
    extension(release_name).is_some_and(|(_, ext)| AUDIO_EXT.contains(&ext.as_str()))
}

/// The digits before a file's extension number the files of a post, rather than
/// tell the post: `IMG_0007.jpg`. An image always. Another file only as the
/// `n`th of 3 or more with `n` as its number: `[7/10] Show.S01E07.mkv`, since
/// `[1/2] Show.S01E02.mkv` and `[1/9] Show.S01E02.mkv` are one episode's
/// post. A subject alone cant say more: `[1/9] Show.S01E01.mkv` is the same
/// text in a pack and in a post with 8 par2 files.
fn numbered(release_name: &str, file: i64, files: i64) -> bool {
    let Some((stem, ext)) = extension(release_name) else { return false };
    let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || digits > 5 || AUDIO_EXT.contains(&ext.as_str()) {
        return false;
    }
    if IMAGE_EXT.contains(&ext.as_str()) {
        return files >= 2;
    }
    files >= 3 && stem[stem.len() - digits..].parse() == Ok(file)
}

/// `CollectionsCleaningService::generic()`, no regex of the group matched: the
/// music cleaner in a music group or for an audio file, which leaves what is
/// in quotes out, otherwise the generic one
fn clean_generic(base: &str, group: &str, release_name: &str) -> String {
    if !MUSIC_GROUP.is_match(group) && !is_audio(release_name) {
        return replace_each(&*GENERIC, base);
    }
    if let Some(c) = MUSIC_SUBJECT.captures(base) {
        return c[1].to_string();
    }
    let clean = replace_each(&*MUSIC, base);
    if clean.len() > 10 && !SHORT_MUSIC.is_match(&clean) {
        return clean;
    }
    // too little left: what is outside the quotes, and the first word inside
    let mut extra = FIRST_QUOTED_WORD.captures(base).map_or(String::new(), |c| c[1].to_string());
    extra.extend(base.chars().filter(|c| !c.is_ascii_alphanumeric()).take(3));
    let rest = QUOTED.replace_all(base, "");
    let rest = LETTERS_OR_EXTENSION.replace_all(&rest, "");
    format!("{clean}{rest}{extra}")
}

/// `RegexService::_matchRegex`: the named groups in name order, without reqid
/// and parts ones. None when nothing, or `0`, comes of it (upstream's falsy).
fn from_regex(re: &Regex, base: &str) -> Option<String> {
    let c = re.captures(base)?;
    let mut names: Vec<&str> =
        re.capture_names().flatten().filter(|n| !n.to_ascii_lowercase().contains("reqid")).collect();
    names.retain(|n| !n.to_ascii_lowercase().contains("parts"));
    names.sort_unstable();
    let out: String = names.iter().filter_map(|n| c.name(n)).map(|m| m.as_str()).collect();
    (!out.is_empty() && out != "0").then_some(out)
}

/// `CollectionsCleaningService::collectionsCleaner`: what stays the same over a
/// post's articles, and whether a regex of the group said it
fn clean(base: &str, group: &str, release_name: &str, regexes: &[Regex]) -> (String, bool) {
    match regexes.iter().find_map(|r| from_regex(r, base)) {
        Some(name) => (name, true),
        None => (clean_generic(base, group, release_name), false),
    }
}

/// letters and digits in a cleaned name, a `yEnc` left in it not counted
fn name_strength(clean: &str) -> usize {
    let lower = clean.to_ascii_lowercase();
    lower.replace("yenc", "").bytes().filter(u8::is_ascii_alphanumeric).count()
}

/// One slice's cleaned names: a post's articles share a base, soo the regexes
/// run once per file.
pub struct Collections<'g> {
    group: &'g str,
    regexes: Compiled,
    memo: HashMap<String, Option<Cleaned>>,
}

#[derive(Clone)]
struct Cleaned {
    /// lowercased: `Name.part1.rar` and `name.nfo` are one post
    name: String,
    files: i64,
    strong: bool,
}

impl<'g> Collections<'g> {
    pub fn new(group: &'g str) -> Self {
        Collections { group, regexes: compiled(group), memo: HashMap::new() }
    }

    /// The collection `article` belongs to, as a string only it and the rest
    /// of its post give. None when it stays a release by its own file name: the
    /// subject doesnt say it's part of a multi-file post, says it's the only
    /// file, or says something a real post cant.
    ///
    /// Like upstream's: the cleaned name and the file total. Article numbers
    /// and anything else a server decides are not in it, soo every server
    /// and every slice gets the same one. The group is, a collection doesnt
    /// cross groups here. A name that doesnt tell a post by itself, short or
    /// with its digits gone, has the poster and the day in it too.
    pub fn of(&mut self, article: &Article, parsed: &ParsedSubject) -> Option<String> {
        let base = subject_base(&article.subject);
        let cleaned = match self.memo.get(base.as_ref()) {
            Some(found) => found.clone(),
            None => {
                let cleaned = self.cleaned(&base, &parsed.release_name);
                self.memo.insert(base.clone().into_owned(), cleaned.clone());
                cleaned
            }
        }?;
        let (group, name, files) = (self.group, &cleaned.name, cleaned.files);
        if cleaned.strong {
            return Some(format!("{group}\0{name}\0{files}"));
        }
        let day = article.date.get(..10).unwrap_or(&article.date);
        Some(format!("{group}\0{name}\0{files}\0{}\0{day}", article.author))
    }

    fn cleaned(&self, base: &str, release_name: &str) -> Option<Cleaned> {
        let counter = file_counter(base);
        let files = counter.map_or(0, |(_, files)| files);
        if counter.is_some_and(|(n, files)| files < 2 || n > files || files > MAX_COLLECTION_FILES) {
            return None;
        }
        if counter.is_some_and(|(n, files)| numbered(release_name, n, files)) {
            let name = replace_each(&*GENERIC_NUMBERED, base);
            return Some(Cleaned { name: name.to_lowercase(), files, strong: false });
        }
        let (name, from_regex) = clean(base, self.group, release_name, &self.regexes);
        // no word of more files and no regex for this shape: its file name is all there is
        if counter.is_none() && !from_regex {
            return None;
        }
        let strong = name_strength(&name) >= STRONG_NAME;
        Some(Cleaned { name: name.to_lowercase(), files, strong })
    }
}

/// Bucket a slice's articles of `group` into releases by their collection,
/// or their parsed name when they have none (`Collections::of`), keeping
/// first-seen order.
pub fn group_articles(group: &str, articles: Vec<Article>) -> IndexMap<String, Release> {
    let mut collections = Collections::new(group);
    group_articles_by(articles, |article, parsed| collections.of(article, parsed))
}
```

`src/parser.rs`:

```rust
// Release (:39-50) gains:
    /// the post it belongs to when that says more than its name (`collections.rs`)
    pub collection: Option<String>,

/// Bucket articles into releases by their parsed release name, keeping
/// first-seen order. What was there before collections, and what they fall back on.
pub fn group_articles(articles: Vec<Article>) -> IndexMap<String, Release> {
    group_articles_by(articles, |_, _| None)
}

/// Bucket articles into releases by what `collection_of` says, or their parsed
/// release name when it says nothing, keeping first-seen order.
pub fn group_articles_by(
    articles: Vec<Article>,
    mut collection_of: impl FnMut(&Article, &ParsedSubject) -> Option<String>,
) -> IndexMap<String, Release> {
    // the body of the old group_articles, with :187-191 replaced by:
        let collection = collection_of(&article, &parsed);
        let key = collection.clone().unwrap_or_else(|| parsed.release_name.clone());
        let release = releases.entry(key).or_insert_with(|| Release {
            name: parsed.release_name.clone(),
            is_obfuscated: is_obfuscated(&parsed.release_name),
            collection,
            ..Default::default()
        });

        // a collection goes by its shortest name in the slice: `T` over `T.mkv` or `T.sample.mkv`
        if release.collection.is_some() && parsed.release_name.len() < release.name.len() {
            release.is_obfuscated = is_obfuscated(&parsed.release_name);
            release.name = parsed.release_name.clone();
        }
    // the rest as it was
}
```

A key holds `\0` and a parsed name can't, so the two kinds share the map safely.

`src/store.rs`:

```rust
// migrate_shard (:199), in the execute_batch at :200, after release_ids:
//     create table if not exists collections (key INTEGER PRIMARY KEY, release_id INTEGER NOT NULL) without rowid;

// save_in_transaction: after IdKey::of (:1359)
let grouping_from = grouping_from(&tx)?;

// :1376, as Part 1 left it, becomes
// nothing is ever saved for a blocked group, whichever path it came by
let mut releases: Vec<Cow<'_, Release>> = Vec::new();
for release in batches.into_iter().flatten().filter(|r| !crate::filters::group_blocked(&r.group)) {
    releases.extend(split_old_rows(&tx, grouping_from, release)?);
}

for release in releases.iter().map(Cow::as_ref) {
    let t = Instant::now();
    let collection = release.collection.as_deref().map(|c| collection_row(&tx, &key, c, release)).transpose()?;
    let name = collection.as_ref().map_or(release.name.as_str(), |c| c.name.as_str());
    let id = global_id(ids.next()?, shard);
    // the upsert as it was, with `name` where it had `release.name`. Move `type Saved` to module level.

// after `if release_id == id { new_releases += 1; }` (:1399-1401)
    if let Some(c) = collection.as_ref().filter(|c| !c.known) {
        tx.prepare_cached("insert or replace into collections (key, release_id) values (?, ?)")?
            .execute(params![c.key, release_id])?;
    }

// :1405-1412
    let mut by_file: IndexMap<&str, Vec<&Article>> = IndexMap::new();
    for a in release.articles.iter().filter(|a| !a.message_id.is_empty()) {
        by_file.entry(a.filename.as_deref().unwrap_or("")).or_default().push(a);
    }
```

The loop body keeps its indentation, so the diff stays small and Part 3's line references to the upsert and the update stay close. New functions, before `add_totals` (`:1509`), and `use std::borrow::Cow;`, `use indexmap::IndexMap;`:

```rust
/// id, size, parts and file total of the row a release is saved into
type Saved = (i64, Option<i64>, Option<i64>, Option<i64>);

/// the id collections started from in this shard, see `start_grouping`. 1, so
/// nothing is an old row, until an indexer of this build has run
pub(crate) fn grouping_from(conn: &Connection) -> Result<i64> {
    let from = conn.prepare_cached("select value from meta where key = 'grouping_from'")?.query_row([], |r| r.get(0));
    Ok(from.optional()?.unwrap_or(1))
}

/// Note where collections start in each shard, the first time an indexer of
/// this build runs: what is there by then was saved by file name, whoever
/// saved it, and its files stay in their rows (`split_old_rows`). Not on
/// open: a menu of this build can open a shard while an older indexer is
/// still writing by file name.
pub fn start_grouping(main: &Path) -> Result<()> {
    for path in shard_paths(main) {
        db::open_shard(&path)?.execute(
            "insert or ignore into meta (key, value) select 'grouping_from', coalesce(max(id), 0) + 1 from releases",
            [],
        )?;
    }
    Ok(())
}

/// A collection's articles that were saved, or started, under their own file
/// name before collections: the file stays in its row, same poster, soo it
/// doesnt split and a reindexed article is still seen as a duplicate. What
/// is left, if any, goes to the collection. A release with no such file comes
/// back as it is.
fn split_old_rows<'a>(tx: &Connection, grouping_from: i64, release: &'a Release) -> Result<Vec<Cow<'a, Release>>> {
    if release.collection.is_none() || grouping_from <= 1 {
        return Ok(vec![Cow::Borrowed(release)]);
    }
    let mut old =
        tx.prepare_cached("select 1 from releases where name = ? and group_name = ? and id < ? and poster = ?")?;
    let mut known: HashMap<&str, bool> = HashMap::new();
    for a in &release.articles {
        let Some(name) = a.release_name.as_deref() else { continue };
        if !known.contains_key(name) {
            known.insert(name, old.exists(params![name, release.group, grouping_from, a.author])?);
        }
    }
    if !known.values().any(|in_old_row| *in_old_row) {
        return Ok(vec![Cow::Borrowed(release)]);
    }
    let piece = |name: &str, collection: Option<String>, articles: Vec<Article>| Release {
        name: name.to_string(),
        size: articles.iter().map(|a| a.bytes).sum(),
        is_obfuscated: crate::parser::is_obfuscated(name),
        collection,
        articles,
        display_name: release.display_name.clone(),
        complete: false,
        group: release.group.clone(),
        poster: release.poster.clone(),
        date: release.date.clone(),
    };
    let mut rest = Vec::new();
    let mut in_old: IndexMap<&str, Vec<Article>> = IndexMap::new();
    for a in &release.articles {
        match a.release_name.as_deref().filter(|name| known.get(name).copied().unwrap_or(false)) {
            Some(name) => in_old.entry(name).or_default().push(a.clone()),
            None => rest.push(a.clone()),
        }
    }
    let mut pieces: Vec<Cow<'a, Release>> =
        in_old.into_iter().map(|(name, articles)| Cow::Owned(piece(name, None, articles))).collect();
    // the shortest name may have been of a file that stayed behind
    if let Some(name) = rest.iter().filter_map(|a| a.release_name.as_deref()).min_by_key(|n| n.len()) {
        pieces.push(Cow::Owned(piece(name, release.collection.clone(), rest.clone())));
    }
    Ok(pieces)
}

/// A collection in a save: its hash, the name its release is saved under,
/// and whether `collections` already points at it.
struct CollectionRow {
    key: i64,
    name: String,
    known: bool,
}

/// The name a collection's release is saved under: its row's when it has one.
/// A new one takes the slice's name; when another release in the group has
/// that, with 8 digits of the key's hash after it, as `a [4]` has its count.
fn collection_row(tx: &Connection, key: &IdKey, collection: &str, release: &Release) -> Result<CollectionRow> {
    let hash = key.hash(collection);
    // a purge can leave a row here, whose release is gone: it finds nothing
    let known: Option<String> = tx
        .prepare_cached("select r.name from collections c join releases r on r.id = c.release_id where c.key = ?")?
        .query_row([hash], |r| r.get(0))
        .optional()?;
    if let Some(name) = known {
        return Ok(CollectionRow { key: hash, name, known: true });
    }
    let mut taken = tx.prepare_cached("select 1 from releases where name = ? and group_name = ?")?;
    let mut name = release.name.clone();
    // the next number when 8 digits of two hashes meet: the upsert would merge the posts
    let mut digits = hash.rem_euclid(100_000_000);
    while taken.exists(params![name, release.group])? {
        name = format!("{} [{digits:08}]", release.name);
        digits = (digits + 1) % 100_000_000;
    }
    Ok(CollectionRow { key: hash, name, known: false })
}
```

## 5. Schema and migration

```sql
-- in the shard, created empty by migrate_shard: no row is rewritten and no table is scanned
create table if not exists collections (
    key INTEGER PRIMARY KEY,      -- IdKey::hash of the collection string, the shard's keyed SipHash
    release_id INTEGER NOT NULL
) without rowid;

-- meta, a key not a column, by store::start_grouping, from run_servers only. O(log n): max(id) is the rowid's last entry
insert or ignore into meta (key, value) select 'grouping_from', coalesce(max(id), 0) + 1 from releases;
```

There's no index on `release_id`, deliberately: it would cost as much as the table, for deletes that don't need it. A row whose release is gone is harmless. `collection_row` joins to `releases`, so it finds nothing and makes a new release, and ids are never reused, so such a row can't point at the wrong one.

| Path | What happens | Test |
|---|---|---|
| Open | `create_shard` → `build_shard` → `migrate_shard` (`src/store.rs:121,192`) makes the table. `db::create_shards` runs it for every shard (`src/db.rs:314-316`). It doesn't touch `grouping_from` | `opening_a_shard_doesnt_say_where_collections_start` |
| First indexer | `run_servers` calls `store::start_grouping` before any pass | `tests/grouping_start.rs` |
| `--compact` | `store::migrate_shard` on the original (`src/compact.rs:418`), the copy made with the table (`:424`), `meta` copied whole (`:446`), so `grouping_from` and the hash keys `id_key0` and `id_key1` survive. Rows copied next to `release_ids` (`:704`), dropping those with no release | `compacting_keeps_collections_and_where_they_started` |
| `--convert` | Nothing to do. `collections` stays empty and `grouping_from` unset, so the first indexer's mark falls above every converted row | Extension of `tests/convert.rs` |
| Wipe | `db::wipe` removes whole shard files. A new shard has an empty table and no `grouping_from` | Extension of `a_wipe_clears_every_shard_and_restarts_the_ids` |
| Menu purge | `store::purge_incomplete` deletes `collections` rows of what it deletes | `the_menu_purge_clears_a_collections_row` |
| Part 3's purge | `delete_release` doesn't touch `collections`, and needn't. Compaction drops what's left | `a_collection_whose_release_is_gone_gets_a_new_one` |

Compaction, in `src/compact.rs:703-706`:

```rust
tx.execute_batch(
    "insert into release_ids select * from old.release_ids
     where file_id = 0 or file_id in (select id from files);
     insert into collections select * from old.collections
     where release_id in (select id from releases);",
)?;
```

`purge_incomplete`, in the batch at `src/store.rs:1671-1677`, after the `release_ids` line:

```sql
delete from collections where release_id in (select id from releases where complete = 0);
```

## 6. Interfaces

**Provided:**

```rust
// src/collections.rs
pub const MAX_COLLECTION_FILES: i64 = 10_000;
pub fn subject_base(subject: &str) -> Cow<'_, str>;   // Part 5 runs its rules on this
pub struct Collections<'g>;
impl<'g> Collections<'g> {
    pub fn new(group: &'g str) -> Self;
    pub fn of(&mut self, article: &Article, parsed: &ParsedSubject) -> Option<String>;
}
pub fn group_articles(group: &str, articles: Vec<Article>) -> IndexMap<String, Release>;

// src/parser.rs
pub struct Release { /* ... */ pub collection: Option<String> }
pub fn group_articles(articles: Vec<Article>) -> IndexMap<String, Release>;   // unchanged
pub fn group_articles_by(articles: Vec<Article>, collection_of: impl FnMut(&Article, &ParsedSubject) -> Option<String>) -> IndexMap<String, Release>;

// src/store.rs
pub fn start_grouping(main: &Path) -> Result<()>;
pub(crate) fn grouping_from(conn: &Connection) -> Result<i64>;
```

`Release` is built with `..Default::default()` everywhere (`grep -rn "Release {" src tests examples`), so `collection` defaults to `None` and existing tests keep the old path.

**Consumed:** `filters::group_blocked`, `drop_per_article_obfuscated` and `COUNTER_RE`, which is `pub(crate)` for this (Part 1), `IdKey::hash`, `parser::parse_subject`.

**Requirements on Part 3:**
1. Its `refuse_what_wouldnt_be_copied` (its Task 3) must list `collections` as a core table. Without that, every shard refuses to compact, since `migrate_shard` creates the table on all of them. It's copied here, with a filter, so it must not also be in `SHARD_TABLES`.
2. Nothing else. Its upsert edit applies as written, since collections use the same statement. `meta` gains a key, not a column.

## 7. Config

None. Grouping changes for every new article on upgrade, with no key, because the old behavior is the bug. It adds no NNTP load, so hard rule 8 isn't in play.

## 8. Tasks

Work in `.worktrees/port-02` on `port/02-grouping`, per the overview.

### Task 1: The regexes as data

**Files:** create `scripts/collection_regexes_to_tsv.py`, `data/collection_regexes.tsv`, `src/collections.rs` (`TABLE`, `BY_GROUP_REGEX`, `compiled`); modify `src/lib.rs`.

- [ ] **Create the script.** Standard library only, reading the seeder as text. `unphp`, `split_delimiters` and `convert` are exactly as in Part 5's `scripts/naming_regexes_to_tsv.py`:

```python
#!/usr/bin/env python3
"""newznab-tmux CollectionRegexesTableSeeder.php -> data/collection_regexes.tsv.

Usage: scripts/collection_regexes_to_tsv.py SEEDER.php OUT.tsv
Source: newznab-tmux at f39acb5ed7fe90a77ab01b7f92e96b473ffe1628, GPLv3. It reads the seeder as text and runs nothing
from it. `convert` is the same as in scripts/naming_regexes_to_tsv.py (Part 5). Descriptions are dropped: they hold
example release names and nothing reads them.
"""
import re
import sys

STR = r"'((?:[^'\\]|\\.)*)'"
ROW = re.compile(
    r"'id'\s*=>\s*(\d+),\s*'group_regex'\s*=>\s*" + STR + r",\s*'regex'\s*=>\s*" + STR
    + r",\s*'status'\s*=>\s*(\d+),\s*'description'\s*=>\s*" + STR + r",\s*'ordinal'\s*=>\s*(-?\d+),",
    re.S,
)
QUANTIFIER = re.compile(r"\{\d+(?:,\d*)?\}")
NEEDS_BACKTRACKING = re.compile(r"\(\?(?:=|!|<=|<!|>|\(|R|\d|&|P=|P>)")


def unphp(s):
    """A PHP single-quoted literal: only \\\\ and \\' are escapes."""
    return re.sub(r"\\(['\\])", r"\1", s)


def split_delimiters(raw):
    raw = raw.strip()
    d, end = raw[0], raw.rfind(raw[0])
    flags = raw[end + 1 :].replace(" ", "")  # PHP ignores spaces among the flags
    if d not in "/#" or end == 0 or set(flags) - set("iu"):
        raise ValueError(f"unexpected delimiters or flags: {raw[:60]}")
    return raw[1:end], flags


def convert(b):
    """PCRE syntax to the regex crate's, keeping PCRE's meaning. Returns (pattern, what changed)."""
    out, changes, i, n, in_class, class_start, after_quantifier = [], [], 0, len(b), False, 0, False
    while i < n:
        c = b[i]
        if c == "\\":
            e = b[i + 1]
            if not in_class and e == "E":  # \E with no \Q means nothing to PCRE, the crate refuses it
                changes.append("dropped \\E")
                i += 2
                continue
            if e in "QGXRKgk" or (not in_class and e in "123456789"):
                raise ValueError(f"unsupported \\{e} in {b[:60]}")
            out.append(b[i : i + 2])
            i, after_quantifier = i + 2, False
        elif in_class:
            if c == "]" and i > class_start:
                in_class = False
                out.append(c)
            elif c == "]":  # first in a class: a literal to PCRE
                out.append("\\]")
                changes.append("escaped leading ] in a class")
            elif c == "[" and b[i + 1 : i + 2] != ":":  # a literal to PCRE, a nested class to the crate
                out.append("\\[")
                changes.append("escaped [ in a class")
            elif b[i : i + 2] in ("&&", "--", "~~"):
                raise ValueError(f"class set operator in {b[:60]}")
            else:
                out.append(c)
            i += 1
        elif c == "[":
            in_class, class_start = True, i + 1
            out.append(c)
            i += 1
            if b[i : i + 1] == "^":
                out.append("^")
                i, class_start = i + 1, i + 1
            after_quantifier = False
        elif c == "(" and NEEDS_BACKTRACKING.match(b, i):
            raise ValueError(f"needs backtracking: {b[:60]}")
        elif c in "+*?":
            if after_quantifier and c == "+":  # possessive: the crate would read x?+ as (x?)+
                changes.append("possessive quantifier made greedy")
                after_quantifier = False
            else:
                out.append(c)
                after_quantifier = not (after_quantifier and c == "?")
            i += 1
        elif c == "{":
            m = QUANTIFIER.match(b, i)
            if m:
                out.append(m.group(0))
                i, after_quantifier = m.end(), True
            else:  # a literal to PCRE, an error to the crate
                out.append("\\{")
                changes.append("escaped literal {")
                i, after_quantifier = i + 1, False
        else:
            out.append(c)
            i, after_quantifier = i + 1, False
    return "".join(out), changes


def main(seeder, tsv):
    rows, stats = [], {}
    for m in ROW.finditer(open(seeder, encoding="utf-8").read()):
        rid, group, raw, status, ordinal = int(m[1]), unphp(m[2]), unphp(m[3]), int(m[4]), int(m[6])
        if status != 1:
            stats["disabled"] = stats.get("disabled", 0) + 1
            continue
        re.compile(group)
        body, flags = split_delimiters(raw)
        pattern, changes = convert(body)
        for c in changes:
            stats[c] = stats.get(c, 0) + 1
        if any(ch in pattern + group for ch in "\t\r\n"):
            raise ValueError(f"tab or newline in rule {rid}")
        rows.append((ordinal, group, rid, "i" if "i" in flags else "", pattern))
    rows.sort()  # upstream: ORDER BY ordinal, group_regex
    with open(tsv, "w", encoding="utf-8", newline="\n") as f:
        f.write("# generated by scripts/collection_regexes_to_tsv.py, do not edit\n")
        f.write("# newznab-tmux database/seeders/CollectionRegexesTableSeeder.php at f39acb5, GPLv3\n")
        f.write("# id\tordinal\tgroup_regex\tflags\tpattern\n")
        f.writelines(f"{r}\t{o}\t{g}\t{fl}\t{p}\n" for o, g, r, fl, p in rows)
    print(f"{len(rows)} rules, {dict(sorted(stats.items()))}")


if __name__ == "__main__":
    main(*sys.argv[1:])
```

- [ ] **Run**, on the reference clone from the overview:

```bash
python3 -I scripts/collection_regexes_to_tsv.py "$SRC/database/seeders/CollectionRegexesTableSeeder.php" data/collection_regexes.tsv
```

  **Expected:** `602 rules, {'escaped [ in a class': 2, 'escaped literal {': 2, 'possessive quantifier made greedy': 1}`. The file is 605 lines and 109,462 bytes, `shasum -a 256` gives `6905c905057b4399ae7166a3dd1cbde9fb12c1e30c88e26b5ac7bde510d8d1a2`. A different result means the seeder or the script changed; fix that first. Commit both.
- [ ] **Test first**, in `src/collections.rs`:

```rust
#[test]
fn every_regex_compiles() {
    let mut count = 0;
    for (_, rows) in BY_GROUP_REGEX.iter() {
        for row in rows {
            RegexBuilder::new(row.regex).case_insensitive(row.case_insensitive).build()
                .unwrap_or_else(|e| panic!("regex {}: {e}", row.id));
            assert!(row.regex.contains("(?P<match"), "regex {} has no named group: nothing would come of it", row.id);
            count += 1;
        }
    }
    assert_eq!(count, 602);
    assert_eq!(BY_GROUP_REGEX.len(), 101);
}

#[test]
fn group_regexes_run_in_upstreams_order_lazily() {
    assert_eq!(compiled("alt.binaries.teevee").len(), 22);
    assert_eq!(compiled("alt.binaries.boneless").len(), 26);
    assert!(compiled("alt.binaries.no.such").is_empty());
    assert!(Arc::ptr_eq(&compiled("alt.binaries.teevee"), &compiled("alt.binaries.teevee")));
}

#[test]
fn upstreams_order_holds_over_group_regexes() {
    // `alt.binaries.ath` has rows under two group_regexes
    let table: Vec<&str> = TABLE.lines().filter(|l| !l.starts_with('#'))
        .map(|l| l.split('\t').collect::<Vec<_>>())
        .filter(|c| c[2] == r"^alt\.binaries\.ath$" || c[2] == r"^alt\.binaries\.(cores|ath)$")
        .map(|c| c[4]).collect();
    let got = compiled("alt.binaries.ath");
    assert!(table.len() >= 2);
    assert_eq!(got.iter().map(|r| r.as_str()).collect::<Vec<_>>(), table);
}

#[test]
fn no_group_of_the_seed_is_blocked() {
    let names = BY_GROUP_REGEX.iter().map(|(g, _)| g.as_str().replace('\\', ""));
    let hits: Vec<String> = names.filter(|g| crate::filters::group_blocked(g)).collect();
    assert!(hits.is_empty(), "{hits:?}");
}
```

- [ ] **Run:** `cargo test --locked --lib collections`. **Expected:** compile error first; pass once `TABLE`, `BY_GROUP_REGEX` and `compiled` are in per 4.9.
- [ ] **Commit:** `Add the collection regexes as data, with their converter`

### Task 2: The cleaners

**Files:** `src/collections.rs` (`subject_base`, `file_counter`, the constants, `extension`, `numbered`, `clean_generic`, `from_regex`, `clean`); `src/filters.rs` (`COUNTER_RE` is `pub(crate)` from Part 1).

- [ ] **Test first:**

```rust
const G: &str = "alt.binaries.test";

#[test]
fn subject_base_is_what_upstream_passes_to_its_regexes() {
    assert_eq!(subject_base(r#"[41/62] "abc" yEnc (316/375)"#), r#"[41/62] "abc" yEnc"#);
    assert_eq!(subject_base(r#"[41/62] "abc" (316/375)"#), r#"[41/62] "abc" yEnc"#);
    assert_eq!(subject_base("[3/7] - file.name.mkv yEnc"), "[3/7] - file.name.mkv yEnc");
    assert_eq!(subject_base(r#"[1/2] "abc def""#), r#"[1/2] "abc def" yEnc"#);
}

#[test]
fn generic_cleaning() {
    let clean = |s: &str, name: &str| clean_generic(&subject_base(s), G, name);
    let first = clean(r#"[1/4] - "Some.Show.S01.1080p-GRP.part1.rar" yEnc (1/2)"#, "Some.Show.S01.1080p-GRP");
    assert_eq!(first, "- Some.Show.S01.1080p-GRP yEnc");
    assert_eq!(clean(r#"[2/4] - "Some.Show.S01.1080p-GRP.part2.rar" yEnc (1/1)"#, ""), first);
    assert_eq!(clean(r#"[3/4] - "Some.Show.S01.1080p-GRP.nfo" yEnc (1/1)"#, ""), first);
    assert_eq!(clean(r#"[4/4] - "Some.Show.S01.1080p-GRP.vol00+1.par2" yEnc (1/1)"#, ""), first);
    // a post's own digits stay
    assert_eq!(clean(r#"[1/2] "Name.2019.mkv" yEnc (1/9)"#, ""), "Name.2019 yEnc");
    assert_eq!(clean(r#"[2/2] "Name.2019.nfo" yEnc (1/1)"#, ""), "Name.2019 yEnc");
    assert_eq!(clean(r#"[1/20] "Name.2019.part01.rar" yEnc (1/9)"#, ""), "Name.2019 yEnc");
    assert_eq!(clean(r#"[1/2] "Name 2019.pdf" yEnc (1/9)"#, ""), "Name 2019 yEnc");
}

#[test]
fn what_numbers_a_post() {
    // images, whatever the numbering starts at
    assert!(numbered("IMG_0007.jpg", 7, 50));
    assert!(numbered("DSC4831.JPG", 1, 2));
    // others only as their own position, in 3 or more
    assert!(numbered("Show.S01E07.mkv", 7, 10));
    assert!(!numbered("Show.S01E07.mkv", 1, 10));
    assert!(!numbered("Show.S01E01.mkv", 1, 2));
    assert!(!numbered("Name.2019.mkv", 1, 2));
    // nothing to go on
    assert!(!numbered("Name.mkv", 1, 10));
    assert!(!numbered("Name.2019", 1, 10), "a year isnt an extension");
    assert!(!numbered("Show.S01E01", 1, 10), "what a rar or nfo leaves");
    assert!(!numbered("Album.Track01.mp3", 1, 12), "audio goes by the music cleaner");
}

#[test]
fn music_groups_and_audio_files_clean_their_own_way() {
    let music = "alt.binaries.sounds.mp3";
    let clean = |s: &str, g: &str| clean_generic(&subject_base(s), g, "x.mp3");
    let a = clean(r#"[01/12] "Artist Name - Song One.mp3" yEnc (1/8)"#, music);
    assert_eq!(a, clean(r#"[02/12] "Artist Name - Song Two.mp3" yEnc (1/8)"#, music), "what is in quotes varies");
    assert!(a.contains(r#""Artist"#), "a short result takes the first quoted word");
    assert_eq!(a, clean(r#"[01/12] "Artist Name - Song One.mp3" yEnc (1/8)"#, G), "by extension too");
    let hit = clean(r#"Some_Artist_Name-404 "00-some_artist-unknown-2009.sfv" yEnc"#, music);
    assert_eq!(hit, r#"Some_Artist_Name-404 ""#);
}

#[test]
fn group_regexes_are_used() {
    // rule 6: `^(?P<match1>[a-zA-Z0-9]+)\[\d+\/\d+\] - ".+?" yEnc$` in teevee only. A made up token.
    let subject = r#"t2EI3CdWdF0hi5b8L9tkx[08/52] - "t2EI3CdWdF0hi5b8L9tkx.part07.rar" yEnc (1/3)"#;
    let group = "alt.binaries.teevee";
    let (name, by_regex) = clean(&subject_base(subject), group, "", &compiled(group));
    assert!(by_regex);
    assert_eq!(name, "t2EI3CdWdF0hi5b8L9tkx");
    let (_, by_regex) = clean(&subject_base(subject), G, "", &compiled(G));
    assert!(!by_regex);
}
```

- [ ] **Run:** `cargo test --locked --lib collections`. **Expected:** compile error.
- [ ] **Implement** per 4.9.
- [ ] **Run:** the same. **Expected:** pass.
- [ ] **Commit:** `Port the collection cleaners`

### Task 3: The collection key, and grouping by it

**Files:** `src/collections.rs` (`Collections`, `group_articles`), `src/parser.rs`.

- [ ] **Test first**, with:

```rust
const DAY: &str = "2026-10-02 10:11:12";

fn art(subject: &str, author: &str) -> Article {
    art_on(subject, author, DAY)
}

fn art_on(subject: &str, author: &str, date: &str) -> Article {
    Article {
        subject: subject.into(),
        author: author.into(),
        date: date.into(),
        message_id: format!("<{subject}{author}{date}>"),
        bytes: 10,
        ..Default::default()
    }
}

fn names(group: &str, subjects: &[&str]) -> Vec<(String, usize)> {
    group_articles(group, subjects.iter().map(|s| art(s, "p <p@x>")).collect())
        .into_values()
        .map(|r| (r.name, r.articles.len()))
        .collect()
}

fn numbered_set(files: i64, subject: impl Fn(i64) -> String) -> Vec<Article> {
    (1..=files).map(|n| art(&subject(n), "p <p@x>")).collect()
}
```

  `use crate::parser::is_complete;`, and these tests:

```rust
#[test]
fn a_multi_file_post_is_one_release() {
    let got = names(
        G,
        &[
            r#"[1/4] - "Some.Show.S01.1080p-GRP.part1.rar" yEnc (1/2)"#,
            r#"[1/4] - "Some.Show.S01.1080p-GRP.part1.rar" yEnc (2/2)"#,
            r#"[2/4] - "Some.Show.S01.1080p-GRP.part2.rar" yEnc (1/1)"#,
            r#"[3/4] - "Some.Show.S01.1080p-GRP.nfo" yEnc (1/1)"#,
            r#"[4/4] - "Some.Show.S01.1080p-GRP.vol00+1.par2" yEnc (1/1)"#,
        ],
    );
    assert_eq!(got, vec![("Some.Show.S01.1080p-GRP".to_string(), 5)]);
}

#[test]
fn different_file_names_of_one_post_are_one_release() {
    let got = names(
        G,
        &[
            r#"[1/3] "Holiday.Film.2019.WEBRip.mkv" yEnc (1/9)"#,
            r#"[2/3] "Holiday.Film.2019.WEBRip.nfo" yEnc (1/1)"#,
            r#"[3/3] "Holiday.Film.2019.WEBRip.par2" yEnc (1/1)"#,
        ],
    );
    assert_eq!(got, vec![("Holiday.Film.2019.WEBRip".to_string(), 3)], "the shortest name, not the first");
}

/// the six shapes the key has to get right. A cleaner that drops digits
/// merges the first two; one that keeps them splits the rest.
#[test]
fn uploads_differing_by_a_year_stay_apart_across_posters_and_days() {
    let (p, q) = ("p <p@x>", "q <q@x>");
    let (d1, d2) = ("2026-01-02 10:00:00", "2026-09-30 10:00:00");
    for stem in ["Name", "The.Lion.King.Adventures"] {
        let got = group_articles(
            G,
            vec![
                art_on(&format!(r#"[1/2] "{stem}.2019.mkv" yEnc (1/1)"#), p, d1),
                art_on(&format!(r#"[2/2] "{stem}.2019.nfo" yEnc (1/1)"#), p, d1),
                art_on(&format!(r#"[1/2] "{stem}.2021.mkv" yEnc (1/1)"#), q, d2),
                art_on(&format!(r#"[2/2] "{stem}.2021.nfo" yEnc (1/1)"#), q, d2),
            ],
        );
        assert_eq!(got.len(), 2, "{stem}: {:?}", got.keys());
        assert!(got.values().all(|r| r.articles.len() == 2));
    }
}

#[test]
fn one_posters_episodes_with_their_own_files_stay_apart() {
    let got = names(
        G,
        &[
            r#"[1/2] "Show.S01E01.mkv" yEnc (1/1)"#, r#"[2/2] "Show.S01E01.nfo" yEnc (1/1)"#,
            r#"[1/2] "Show.S01E02.mkv" yEnc (1/1)"#, r#"[2/2] "Show.S01E02.nfo" yEnc (1/1)"#,
        ],
    );
    assert_eq!(got, vec![("Show.S01E01".to_string(), 2), ("Show.S01E02".to_string(), 2)]);
}

#[test]
fn a_pack_of_episodes_is_one_release() {
    let got = group_articles(G, numbered_set(10, |n| format!(r#"[{n}/10] "Show.S01E{n:02}.mkv" yEnc (1/1)"#)));
    assert_eq!(got.len(), 1, "{:?}", got.keys());
}

#[test]
fn album_tracks_are_one_release() {
    for group in [G, "alt.binaries.sounds.mp3"] {
        let named = numbered_set(12, |n| format!(r#"[{n:02}/12] "Artist Name - Song {n}.mp3" yEnc (1/8)"#));
        assert_eq!(group_articles(group, named).len(), 1, "{group}: titled tracks");
        let numbered = numbered_set(12, |n| format!(r#"[{n:02}/12] "Album.Name.Track{n:02}.flac" yEnc (1/8)"#));
        assert_eq!(group_articles(group, numbered).len(), 1, "{group}: numbered tracks");
    }
    // two artists, same count, same poster, same day
    let mut two = numbered_set(3, |n| format!(r#"[{n}/3] "Beatles - Song {n}.mp3" yEnc (1/8)"#));
    two.extend(numbered_set(3, |n| format!(r#"[{n}/3] "Queen - Song {n}.mp3" yEnc (1/8)"#)));
    assert_eq!(group_articles(G, two).len(), 2);
}

#[test]
fn numbered_images_are_one_release() {
    let got = names(
        G,
        &(1..=50)
            .map(|i| format!(r#"[{i:02}/50] "IMG_{i:04}.jpg" yEnc (1/1)"#))
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_eq!(got, vec![("IMG_0001.jpg".to_string(), 50)], "first seen on a tie");
    // a camera's own numbering
    let got =
        group_articles(G, numbered_set(50, |n| format!(r#"[{n:02}/50] "IMG_{:04}.jpg" yEnc (1/1)"#, 4830 + n)));
    assert_eq!(got.len(), 1);
}

#[test]
fn rar_volumes_are_one_release() {
    let got = group_articles(G, numbered_set(20, |n| format!(r#"[{n:02}/20] "Name.part{n:02}.rar" yEnc (1/50)"#)));
    assert_eq!(got.len(), 1);
    let got =
        group_articles(G, numbered_set(20, |n| format!(r#"[{n:02}/20] "Name.2019.part{n:02}.rar" yEnc (1/50)"#)));
    assert_eq!(got.len(), 1, "with digits before them");
}

/// `Some.Longer...` and the `[01/20]` pair: the rest of the reviewer's probe
#[test]
fn nine_articles_from_different_uploads_dont_merge() {
    let (p, q) = ("p", "q");
    let (d1, d2) = ("2026-01-02 10:00:00", "2026-09-30 10:00:00");
    let got = group_articles(
        G,
        vec![
            art_on(r#"[1/2] "The.Lion.King.Adventures.2019.mkv" yEnc (1/1)"#, p, d1),
            art_on(r#"[2/2] "The.Lion.King.Adventures.2019.nfo" yEnc (1/1)"#, p, d1),
            art_on(r#"[1/2] "The.Lion.King.Adventures.2021.mkv" yEnc (1/1)"#, q, d2),
            art_on(r#"[2/2] "The.Lion.King.Adventures.2021.nfo" yEnc (1/1)"#, q, d2),
            art_on(r#"[1/2] "Some.Longer.Show.Name.S01E01.mkv" yEnc (1/1)"#, p, d1),
            art_on(r#"[1/2] "Some.Longer.Show.Name.S01E07.mkv" yEnc (1/1)"#, q, d2),
            art_on(r#"[2/2] "Some.Longer.Show.Name.S01E07.nfo" yEnc (1/1)"#, q, d2),
            art_on(r#"[01/20] "Movie.Name.Of.Some.Length.2019.part01.rar" yEnc (1/50)"#, p, d1),
            art_on(r#"[01/20] "Movie.Name.Of.Some.Length.2021.part01.rar" yEnc (1/50)"#, q, d2),
        ],
    );
    assert_eq!(got.len(), 6, "{:?}", got.keys());
}

/// What the subject cant tell, pinned so a change to it is a decision.
#[test]
fn what_stays_merged_or_split_on_purpose() {
    // one poster, one day, two sets of one shape and count: one release
    let mut two = numbered_set(5, |n| format!(r#"[{n}/5] "DSC{n:04}.jpg" yEnc (1/1)"#));
    two.extend(numbered_set(5, |n| format!(r#"[{n}/5] "DSC{:04}.jpg" yEnc (1/1)"#, 100 + n)));
    assert_eq!(group_articles(G, two).len(), 1);
    // the same text in a pack and in an episode's post: its first file goes with the pack's
    let got = names(
        G,
        &[
            r#"[1/3] "Show.S01E01.mkv" yEnc (1/1)"#, r#"[2/3] "Show.S01E01.nfo" yEnc (1/1)"#,
            r#"[3/3] "Show.S01E01.par2" yEnc (1/1)"#, r#"[1/3] "Show.S01E02.mkv" yEnc (1/1)"#,
            r#"[2/3] "Show.S01E02.nfo" yEnc (1/1)"#, r#"[3/3] "Show.S01E02.par2" yEnc (1/1)"#,
        ],
    );
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[1], ("Show.S01E01".to_string(), 2));
    assert_eq!(got[2], ("Show.S01E02".to_string(), 3));
    // a pack with its files off by one: no merge, as today
    let off = numbered_set(10, |n| format!(r#"[{}/11] "Show.S01E{n:02}.mkv" yEnc (1/1)"#, n + 1));
    assert_eq!(group_articles(G, off).len(), 10);
    // track numbers first: the first word differs, no merge, as today
    let tracks = numbered_set(12, |n| format!(r#"[{n:02}/12] "{n:02} - Song.mp3" yEnc (1/8)"#));
    assert_eq!(group_articles(G, tracks).len(), 12);
}

#[test]
fn bracket_parts_still_become_parts_inside_a_collection() {
    let got = group_articles(
        G,
        vec![
            art(r#"[1/3] - "same.bin" yEnc (1/1)"#, "p"),
            art(r#"[2/3] - "same.bin" yEnc (1/1)"#, "p"),
            art(r#"[3/3] - "same.bin" yEnc (1/1)"#, "p"),
        ],
    );
    assert_eq!(got.len(), 1);
    let r = got.values().next().unwrap();
    let parts: Vec<_> = r.articles.iter().map(|a| (a.part, a.total_parts, a.file_total)).collect();
    assert_eq!(parts, vec![(Some(1), Some(3), None), (Some(2), Some(3), None), (Some(3), Some(3), None)]);
    assert!(is_complete(&r.articles));
}

#[test]
fn nothing_in_the_subject_says_more_files_no_merge() {
    let got = names(
        G,
        &[
            r#""IMG_0001.jpg" yEnc (1/1)"#, r#""IMG_0002.jpg" yEnc (1/1)"#, r#"[1/1] "IMG_0003.jpg" yEnc (1/1)"#,
            r#"[1/1] "IMG_0004.jpg" yEnc (1/1)"#,
        ],
    );
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(got[2].0, "IMG_0003.jpg", "the same names as today");
}

#[test]
fn a_forged_counter_doesnt_merge() {
    let subjects = [r#"[1/99999] "a0001.jpg" yEnc (1/1)"#, r#"[2/99999] "a0002.jpg" yEnc (1/1)"#];
    assert_eq!(names(G, &subjects).len(), 2, "past MAX_COLLECTION_FILES");
    let subjects = [r#"[9/5] "b0001.jpg" yEnc (1/1)"#, r#"[8/5] "b0002.jpg" yEnc (1/1)"#];
    assert_eq!(names(G, &subjects).len(), 2, "a number past the total");
}

#[test]
fn unrelated_posters_stay_apart() {
    let mut articles = Vec::new();
    for poster in 0..50 {
        for i in 1..=5 {
            articles
                .push(art(&format!(r#"[{i}/5] "DSC{i:04}.jpg" yEnc (1/1)"#), &format!("p{poster} <p{poster}@x>")));
        }
    }
    let got = group_articles(G, articles);
    assert_eq!(got.len(), 50);
    assert!(got.values().all(|r| r.articles.len() == 5));
}

#[test]
fn one_posters_two_posts_stay_apart_where_the_subject_tells() {
    let two = |a: &str, b: &str| assert_eq!(names(G, &[a, b]).len(), 2, "{a} {b}");
    two(r#"[1/2] "Other.Good.Release.Name.mkv" yEnc (1/1)"#, r#"[1/2] "First.Good.Release.Name.mkv" yEnc (1/1)"#);
    two(r#"[1/5] "DSC0001.jpg" yEnc (1/1)"#, r#"[1/7] "DSC0101.jpg" yEnc (1/1)"#);
    two(r#"[1/5] "Holiday.Album.Spain.0001.jpg" yEnc (1/1)"#, r#"[1/7] "Holiday.Album.Spain.0001.jpg" yEnc (1/1)"#);
    // another day, for a name that cant tell
    let a = art_on(r#"[1/5] "DSC0001.jpg" yEnc (1/1)"#, "p", "2026-10-02 23:59:59");
    let b = art_on(r#"[2/5] "DSC0002.jpg" yEnc (1/1)"#, "p", "2026-10-09 00:00:01");
    assert_eq!(group_articles(G, vec![a, b]).len(), 2);
    // a long name with its digits gone cant either
    let a = art(r#"[1/5] "Holiday.Album.Spain.0001.jpg" yEnc (1/1)"#, "p");
    let b = art(r#"[2/5] "Holiday.Album.Spain.0002.jpg" yEnc (1/1)"#, "q");
    assert_eq!(group_articles(G, vec![a, b]).len(), 2);
}

#[test]
fn a_long_name_with_a_new_poster_per_article_stays_together() {
    let articles: Vec<Article> = (1..=20)
        .map(|i| art(&format!(r#"[1/2] "Some.Long.Release.Name.2024.bin" yEnc ({i}/20)"#), &format!("{i}@random")))
        .collect();
    let got = group_articles(G, articles);
    assert_eq!(got.len(), 1);
    assert!(!is_complete(&got.values().next().unwrap().articles), "one of two files");
}

#[test]
fn the_key_is_the_same_on_any_server_and_slice() {
    let key = |group: &str, number: u64, poster: &str| {
        let mut a = art(r#"[3/9] "Same.Post.Over.Servers.rar" yEnc (2/9)"#, poster);
        a.number = number;
        let parsed = crate::parser::parse_subject(&a.subject).unwrap();
        Collections::new(group).of(&a, &parsed).unwrap()
    };
    assert_eq!(key(G, 100, "p"), key(G, 99_000_000, "p"), "article numbers arent in it");
    assert_eq!(key(G, 100, "p"), key(G, 100, "q"), "a long name doesnt need the poster");
    assert_ne!(key(G, 100, "p"), key("alt.binaries.other", 100, "p"), "a collection doesnt cross groups");
}
```

- [ ] **Run:** `cargo test --locked --lib collections`. **Expected:** compile error.
- [ ] **Implement** per 4.9. `parser::group_articles` keeps its signature and its tests.
- [ ] **Run:** `cargo test --locked --lib && cargo test --locked --test parser_parity`. **Expected:** pass. No `src/parser.rs` test and no fixture changed.
- [ ] **Commit:** `Group a slice's articles by collection`

### Task 4: Saving by collection

**Files:** `src/store.rs`.

- [ ] **Test first**, in `mod tests`. `save` is `store::save`:

```rust
/// a collection's slice: one article of file `file` in release `name`
fn in_collection(collection: &str, name: &str, file: &str, id: &str, poster: &str) -> Release {
    Release {
        name: name.into(),
        group: "alt.binaries.t".into(),
        poster: poster.into(),
        collection: Some(collection.into()),
        articles: vec![Article {
            message_id: id.into(),
            author: poster.into(),
            subject: format!("\"{file}\" yEnc (1/1)"),
            filename: Some(file.into()),
            release_name: Some(file.into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn fresh_shard_db() -> (tempfile::TempDir, PathBuf, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    let conn = db::open_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
    (dir, main, conn)
}

fn rows_of(conn: &Connection) -> Vec<(String, i64, i64)> {
    conn.prepare(
        "select r.name, r.parts, (select count(*) from files f where f.release_id = r.id) from releases r order by id",
    )
    .unwrap()
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
    .unwrap()
    .map(|r| r.unwrap())
    .collect()
}

/// a row saved before collections: name is its file's parsed name
fn before_collections(main: &Path, name: &str, id: &str, poster: &str) {
    let mut old = in_collection("unused", name, name, id, poster);
    old.collection = None;
    save(main, &[old]).unwrap();
    start_grouping(main).unwrap();
}

#[test]
fn a_collection_stays_one_release_over_slices() {
    let (_dir, main, conn) = fresh_shard_db();
    save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p")]).unwrap();
    // another slice or server: its first file is another one
    save(&main, &[in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p")]).unwrap();
    assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 2, 2)]);
    // the same article again adds nothing
    save(&main, &[in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p")]).unwrap();
    assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 2, 2)]);
    let totals: i64 = conn.query_row("select value from meta where key = 'releases'", [], |r| r.get(0)).unwrap();
    assert_eq!(totals, 1, "new releases are counted once");
}

#[test]
fn two_slices_of_a_collection_in_one_transaction_are_one_release() {
    let (_dir, main, conn) = fresh_shard_db();
    let (a, b) =
        (in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p"), in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p"));
    save(&main, &[a, b]).unwrap();
    assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 2, 2)]);
}

#[test]
fn a_name_another_release_has_gets_a_part_of_the_hash() {
    let (_dir, main, conn) = fresh_shard_db();
    save(&main, &[in_collection("one", "same.jpg", "same.jpg", "<a@x>", "p")]).unwrap();
    save(&main, &[in_collection("two", "same.jpg", "same.jpg", "<b@x>", "q")]).unwrap();
    let rows = rows_of(&conn);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "same.jpg");
    assert!(rows[1].0.starts_with("same.jpg [") && rows[1].0.ends_with(']'), "{rows:?}");
    assert!(rows[1].0[10..rows[1].0.len() - 1].bytes().all(|b| b.is_ascii_digit()), "{rows:?}");
    // and it stays that row
    save(&main, &[in_collection("two", "other.jpg", "other.jpg", "<c@x>", "q")]).unwrap();
    assert_eq!(rows_of(&conn)[1].1, 2);
}

#[test]
fn a_suffix_another_release_has_moves_to_the_next_number() {
    let (_dir, main, conn) = fresh_shard_db();
    save(&main, &[in_collection("one", "same.jpg", "same.jpg", "<a@x>", "p")]).unwrap();
    let digits = IdKey::of(&conn).unwrap().hash("two").rem_euclid(100_000_000);
    save(&main, &[in_collection("other", &format!("same.jpg [{digits:08}]"), "x.jpg", "<b@x>", "q")]).unwrap();
    save(&main, &[in_collection("two", "same.jpg", "same.jpg", "<c@x>", "r")]).unwrap();
    let rows = rows_of(&conn);
    assert_eq!(rows.len(), 3, "the posts stay apart: {rows:?}");
    assert_eq!(rows[2].0, format!("same.jpg [{:08}]", (digits + 1) % 100_000_000));
}

#[test]
fn a_collection_whose_release_is_gone_gets_a_new_one() {
    let (_dir, main, conn) = fresh_shard_db();
    save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p")]).unwrap();
    // what a purge leaves: `collections` keeps its row
    conn.execute_batch("delete from segments; delete from files; delete from releases;").unwrap();
    save(&main, &[in_collection("k", "b.jpg", "b.jpg", "<b@x>", "p")]).unwrap();
    assert_eq!(rows_of(&conn), vec![("b.jpg".to_string(), 1, 1)]);
    let points: i64 = conn
        .query_row("select count(*) from collections c join releases r on r.id = c.release_id", [], |r| r.get(0))
        .unwrap();
    assert_eq!(points, 1);
}

#[test]
fn a_file_started_before_collections_stays_in_its_row() {
    let (_dir, main, conn) = fresh_shard_db();
    before_collections(&main, "a.jpg", "<a@x>", "p");
    // a new part of it, a copy of what it has, and a new file of the same post
    let mut late = in_collection("k", "a.jpg", "a.jpg", "<a2@x>", "p");
    late.articles.extend(in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p").articles);
    late.articles.extend(in_collection("k", "a.jpg", "b.jpg", "<b@x>", "p").articles);
    save(&main, &[late]).unwrap();
    let rows = rows_of(&conn);
    assert_eq!(rows[0], ("a.jpg".to_string(), 2, 1), "its new part went in, its copy didnt: {rows:?}");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1].1, 1, "the new file starts the collection: {rows:?}");
    assert_eq!(rows[1].0, "b.jpg", "named after what it holds: {rows:?}");
}

#[test]
fn all_of_a_post_in_old_rows_makes_no_new_row() {
    let (_dir, main, conn) = fresh_shard_db();
    before_collections(&main, "a.jpg", "<a@x>", "p");
    save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p")]).unwrap();
    assert_eq!(rows_of(&conn), vec![("a.jpg".to_string(), 1, 1)]);
}

#[test]
fn another_posters_file_doesnt_join_an_old_row() {
    let (_dir, main, conn) = fresh_shard_db();
    before_collections(&main, "a.jpg", "<a@x>", "p");
    save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<b@x>", "q")]).unwrap();
    let rows = rows_of(&conn);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].1, 1);
}

#[test]
fn rows_saved_since_are_not_old_rows() {
    let (_dir, main, conn) = fresh_shard_db();
    assert_eq!(grouping_from(&conn).unwrap(), 1, "a new shard");
    let mut plain = in_collection("unused", "a.jpg", "a.jpg", "<a@x>", "p");
    plain.collection = None;
    save(&main, &[plain]).unwrap();
    save(&main, &[in_collection("k", "a.jpg", "a.jpg", "<b@x>", "p")]).unwrap();
    assert_eq!(rows_of(&conn).len(), 2, "not continued: it was saved under this build");
}

#[test]
fn opening_a_shard_doesnt_say_where_collections_start() {
    let (_dir, main, conn) = fresh_shard_db();
    let stored = |conn: &Connection| -> Option<i64> {
        conn.query_row("select value from meta where key = 'grouping_from'", [], |r| r.get(0)).optional().unwrap()
    };
    assert_eq!(stored(&conn), None, "made and opened by this build, no indexer yet");
    let mut plain = in_collection("unused", "a.jpg", "a.jpg", "<a@x>", "p");
    plain.collection = None;
    save(&main, &[plain]).unwrap();
    create_shard(&shard_path(&main, shard_of("alt.binaries.t"))).unwrap();
    assert_eq!(stored(&conn), None, "nor does opening it again");
    let max: i64 = conn.query_row("select max(id) from releases", [], |r| r.get(0)).unwrap();
    start_grouping(&main).unwrap();
    assert_eq!(grouping_from(&conn).unwrap(), max + 1);
    let mut more = in_collection("unused", "b.jpg", "b.jpg", "<b@x>", "p");
    more.collection = None;
    save(&main, &[more]).unwrap();
    start_grouping(&main).unwrap();
    assert_eq!(grouping_from(&conn).unwrap(), max + 1, "the next run doesnt move it");
}

/// `by_file` was a list searched per article: quadratic in a release's
/// files, which a collection makes many of. `cargo test --release -- --ignored`
#[test]
#[ignore]
fn many_files_in_one_release_save_in_about_linear_time() {
    let time = |files: usize| {
        let (_dir, main, _conn) = fresh_shard_db();
        let mut release = in_collection("k", "f0", "f0", "<0@x>", "p");
        release.articles = (0..files)
            .map(|i| in_collection("k", "f", &format!("f{i}"), &format!("<{i}@x>"), "p").articles.remove(0))
            .collect();
        let t = Instant::now();
        save(&main, &[release]).unwrap();
        t.elapsed().as_secs_f64()
    };
    let (small, big) = (time(5_000), time(40_000));
    println!("5,000 files {small:.2}s, 40,000 files {big:.2}s");
    assert!(big / small < 16.0, "8 times the files took {:.1} times as long", big / small);
}
```

- [ ] **Run:** `cargo test --locked --lib store`. **Expected:** compile error.
- [ ] **Implement** per 4.9. `src/store.rs` isn't reached by real articles yet, so `just test` stays as it was.
- [ ] **Run:** `cargo test --locked --lib store`. **Expected:** pass, every existing store test included.
- [ ] **Run, for information:** `cargo test --release --locked --lib many_files -- --ignored --nocapture`. The prototype's ratio was 8.6. Put the number in the PR description; per the overview, an `#[ignore]` timing test never stops a part.
- [ ] **Commit:** `Save a collection into one release, keeping files already in old rows`

### Task 5: It survives everything

**Files:** `src/compact.rs`, `src/store.rs`, `src/db.rs`, `tests/convert.rs`.

- [ ] **Tests first:**
  - In `src/store.rs`:

```rust
#[test]
fn the_menu_purge_clears_a_collections_row() {
    let (_dir, main, conn) = fresh_shard_db();
    let mut broken = in_collection("k", "a.jpg", "a.jpg", "<a@x>", "p");
    broken.articles[0].total_parts = Some(2);
    save(&main, &[broken]).unwrap();
    assert_eq!(conn.query_row("select count(*) from collections", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    purge_incomplete(&main).unwrap();
    assert_eq!(conn.query_row("select count(*) from collections", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}
```

  - In `src/compact.rs` `mod tests`:

```rust
#[test]
fn compacting_keeps_collections_and_where_they_started() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("atlas.db");
    db::create_db_at(&main).unwrap();
    let group = "alt.binaries.g0";
    let path = store::shard_path(&main, store::shard_of(group));
    let release = |file: &str, id: &str| Release {
        name: file.into(),
        group: group.into(),
        collection: Some("k".into()),
        articles: vec![Article {
            message_id: id.into(),
            subject: format!("\"{file}\" yEnc (1/1)"),
            filename: Some(file.into()),
            release_name: Some(file.into()),
            part: Some(1),
            total_parts: Some(1),
            bytes: 10,
            ..Default::default()
        }],
        ..Default::default()
    };
    store::save(&main, &[release("a.jpg", "<a@x>")]).unwrap();
    store::start_grouping(&main).unwrap();
    // left behind by a purge: compaction drops it
    db::open_at(&path)
        .unwrap()
        .execute("insert into collections (key, release_id) values (7, 999999)", [])
        .unwrap();
    let from = store::grouping_from(&db::open_at(&path).unwrap()).unwrap();
    assert!(from > 1);

    run(&main, &|_| {}, &Default::default()).unwrap();

    let conn = db::open_at(&path).unwrap();
    assert_eq!(store::grouping_from(&conn).unwrap(), from);
    let rows: i64 = conn.query_row("select count(*) from collections", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1, "the row with a release, not the one left behind");
    // the hash key came with meta: the collection finds its release
    store::save(&main, &[release("b.jpg", "<b@x>")]).unwrap();
    let (releases, files): (i64, i64) = conn
        .query_row("select (select count(*) from releases), (select count(*) from files)", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((releases, files), (1, 2));
}
```

  - In `src/db.rs`, at the end of `a_wipe_clears_every_shard_and_restarts_the_ids`, where `shard` is that test's `store::shard_path(&main, 2)`:

```rust
let conn = open_at(&shard).unwrap();
let (collections, from): (i64, Option<i64>) = conn.query_row(
    "select (select count(*) from collections), (select value from meta where key = 'grouping_from')",
    [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
assert_eq!((collections, from), (0, None), "a new shard has no collections, and no indexer has run on it");
```

  - In `tests/convert.rs`, at the end of the test ending at `assert_eq!(newest, "Brand.New");` (`:220`):

```rust
// no collections yet, and it's the first indexer's to say where they start
for path in store::shard_paths(&main) {
    let conn = db::open_shard(&path).unwrap();
    let (collections, from): (i64, Option<i64>) = conn.query_row(
        "select (select count(*) from collections), (select value from meta where key = 'grouping_from')",
        [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((collections, from), (0, None));
}
```

- [ ] **Run:** `cargo test --locked --lib compact && cargo test --locked --test convert`. **Expected:** the compaction test fails, its row is lost. The convert and wipe extensions pass already, and pin that nothing sets the mark early.
- [ ] **Implement** the two edits in section 5.
- [ ] **Run:** the same, then `cargo test --locked --lib store && cargo test --locked --lib db`. **Expected:** pass.
- [ ] **Commit:** `Carry collections through compaction, conversion, wipe and purge`

### Task 6: Turn it on

**Files:** `src/indexer.rs`, `src/ai.rs`, `src/bg_indexer.rs`, `tests/common/mod.rs`, `tests/save_memory.rs`; create `tests/collection_paths.rs`, `tests/grouping_start.rs`.

- [ ] **Test first.**
  - `tests/common/mod.rs`: `Post` gains `pub author: String`. `post()` (`:150`) sets `"poster <p@mock>".into()`. The XOVER row (`:249`) writes `p.author` instead of the literal. `tests/save_memory.rs:44` has the one struct literal: add `author: "poster <p@mock>".into(),`.
  - `tests/collection_paths.rs`, a single test, with the env set as `tests/e2e.rs:16-19` does and `db::create_db().unwrap()`:

```rust
fn index(port: u16, slice: i64) {
    let client = BlockingPool::new(&[mock(port, "secret", 2, 1)]);
    client.connect().unwrap();
    let mut indexer = Indexer::new(client, "backfill", db::open().unwrap());
    indexer.request_size = slice as u64;
    index_until_idle(&mut indexer);
}

// in the test:
// 50 images, each its own file name, from one poster
let album = |first: u64, poster: &str| -> Vec<Post> {
    (1..=50u64)
        .map(|i| Post {
            author: poster.into(),
            // a post's message-ids are the same on every server, its numbers arent
            message_id: format!("<{i}.{}@x>", &poster[..1]),
            ..post(first + i, &format!(r#"[{i:02}/50] "DSC{i:04}.jpg" yEnc (1/1)"#), 10, vec![])
        })
        .collect()
};
// server A carries the first half of a's album, B all of it: same post, other numbers
let mut on_a = album(0, "a <a@x>");
on_a.truncate(25);
let mut on_b = album(1000, "a <a@x>");
// another poster's album of the same names, and a post without a counter
on_b.extend(album(2000, "b <b@x>"));
on_b.push(post(3000, r#""Lone.Picture.jpg" yEnc (1/1)"#, 10, vec![]));

// small slices, soo the post is cut up
index(spawn_server(Server::new(on_a)), 7);
// the other server's own article numbers, from its own cursors
db::open().unwrap().execute("delete from groups", []).unwrap();
index(spawn_server(Server::new(on_b)), 11);

let conn = db::open().unwrap();
let rows: Vec<(String, i64, i64)> = conn
    .prepare("select name, parts, file_total from releases order by parts desc, name").unwrap()
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2).unwrap_or(0)))).unwrap()
    .map(|r| r.unwrap()).collect();
assert_eq!(rows.len(), 3, "{rows:?}");
assert_eq!((rows[0].1, rows[0].2), (50, 50), "{rows:?}");
assert_eq!((rows[1].1, rows[1].2), (50, 50), "{rows:?}");
assert_eq!(rows[2].0, "Lone.Picture.jpg");

// complete, and its nzb has every file
let all = search::all_releases(0, 10).unwrap();
let album = all.iter().find(|r| r.parts == Some(50)).unwrap();
assert!(album.complete, "{album:?}");
let xml = nzb::build_nzb(album.id).unwrap().unwrap();
assert_eq!(xml.matches("<file ").count(), 50);
```

  - `tests/grouping_start.rs`, with `mod common;` and its imports (`std::sync::Arc`, `std::sync::atomic::{AtomicBool, Ordering}`, `std::time::{Duration, Instant}`, `atlas::parser::{Article, Release}`, `atlas::{bg_indexer, config, db, paths, store}`, `common::{GROUP, Server, post, spawn_server}`, `rusqlite::OptionalExtension`):

```rust
#[test]
fn only_the_indexer_says_where_collections_start() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: only test in this binary, set before any thread is spawned
    unsafe {
        std::env::set_var("ATLAS_HOME", home.path());
        std::env::set_var("ATLAS_NO_KEYRING", "1");
        std::env::set_var("ATLAS_SAB_DIR", home.path().join("no-sabnzbd"));
    }
    let port = spawn_server(Server::new(vec![post(1, r#"[1/2] "Straddling.Post.bin" yEnc (2/2)"#, 10, vec![])]));
    let config = serde_json::json!({
        "usenet_servers": [{"host": "127.0.0.1", "username": "bob", "password": "secret", "port": port, "ssl": false, "connections": 2}],
        "groups": [GROUP],
        "index_mode": "backfill",
    });
    std::fs::write(home.path().join("config.json"), config.to_string()).unwrap();

    db::create_db().unwrap();
    let main = paths::database();
    let shard = db::open_shard(&store::shard_path(&main, store::shard_of(GROUP))).unwrap();
    let from = || -> Option<i64> {
        shard.query_row("select value from meta where key = 'grouping_from'", [], |r| r.get(0)).optional().unwrap()
    };
    assert_eq!(from(), None, "opening a shard doesnt start it");

    // an older indexer goes on writing, with this build's menu open
    let old = Release {
        name: "Straddling.Post.bin".into(),
        group: GROUP.into(),
        poster: "poster <p@mock>".into(),
        articles: vec![Article {
            subject: r#"[1/2] "Straddling.Post.bin" yEnc (1/2)"#.into(),
            filename: Some("Straddling.Post.bin".into()),
            part: Some(1),
            total_parts: Some(2),
            file_index: Some(1),
            file_total: Some(2),
            message_id: "<old@x>".into(),
            bytes: 10,
            ..Default::default()
        }],
        ..Default::default()
    };
    store::save(&main, &[old]).unwrap();
    db::create_db().unwrap();
    assert_eq!(from(), None, "nor does opening it again");
    let max: i64 = shard.query_row("select max(id) from releases", [], |r| r.get(0)).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let cfg = config::load_config().unwrap();
    let runner = {
        let stop = stop.clone();
        std::thread::spawn(move || bg_indexer::run_until(cfg, stop))
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let parts = || -> i64 { shard.query_row("select parts from releases", [], |r| r.get(0)).unwrap_or(0) };
    while parts() < 2 {
        assert!(Instant::now() < deadline, "the post never arrived");
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    assert_eq!(runner.join().unwrap(), 0);

    assert_eq!(from(), Some(max + 1), "what the older indexer saved is below it");
    let rows: i64 = shard.query_row("select count(*) from releases", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1, "the file went on in the row it started in");
    let collections: i64 = shard.query_row("select count(*) from collections", [], |r| r.get(0)).unwrap();
    assert_eq!(collections, 0);

    // a later run leaves it where it is
    store::start_grouping(&main).unwrap();
    assert_eq!(from(), Some(max + 1));
}
```

  - `src/indexer.rs` `mod tests`:

```rust
#[test]
fn a_release_looks_at_a_few_bodies_for_its_name() {
    let article = |i: usize, name: &str| crate::parser::Article {
        subject: format!(r#""{name}" yEnc (1/1)"#),
        message_id: format!("<{i}@x>"),
        ..Default::default()
    };
    let mut articles: Vec<_> = (0..40).map(|i| article(i, &format!("n{i}.nfo"))).collect();
    articles.push(article(99, "base.par2"));
    let sources = name_sources(&Release { articles, ..Default::default() });
    assert_eq!(sources.len(), NAME_SOURCES_MAX);
    assert_eq!(sources[0].0, "<99@x>", "the par2 first");
}
```

- [ ] **Run:** `cargo test --locked --test collection_paths --test grouping_start`. **Expected:** the first fails with 101 or more releases rather than 3, the second with no `grouping_from`.
- [ ] **Implement.**
  - `save_slice`: Part 1's `parsing` closure becomes the following, and `group_articles` leaves the `crate::parser` import at `src/indexer.rs:14`:

```rust
let parsing = {
    let group = group.to_string();
    move || {
        let (articles, skipped) = crate::filters::drop_per_article_obfuscated(headers_to_articles(headers));
        let releases: Vec<Release> = crate::collections::group_articles(&group, articles).into_values().collect();
        (releases, skipped)
    }
};
```

  - `src/indexer.rs`, before `name_sources` (`:1640`), `const NAME_SOURCES_MAX: usize = 8;`, and `.take(NAME_SOURCES_MAX)` before `.collect()` at `:1649`.
  - `src/ai.rs:172`: `crate::collections::group_articles(grp, articles)`; remove the parser import.
  - `src/bg_indexer.rs`, `run_servers`, first thing in the `Ok(conn)` branch at `:736`:

```rust
// before any pass: what is there so far was saved by file name
if let Err(e) = crate::store::start_grouping(&paths::database()) {
    ui::warn(&format!("couldnt note where collections start: {e:#}"));
}
```

- [ ] **Run:** `cargo test --locked --test collection_paths --test grouping_start && just test`. **Expected:** pass, `tests/e2e.rs` and `tests/parser_parity.rs` unchanged: e2e's `Cool.Movie.2024.1080p` is 3 files and 4 parts under the name it had, its obfuscated post still counts as obfuscated.
- [ ] **Commit:** `Index by collection, so a multi-file post is one release`

### Task 7: Measure it

**Files:** create `examples/bench_collections.rs`; modify `examples/bench_writes.rs`.

- [ ] **Implement `examples/bench_collections.rs`.** 20 slices of 10,000 articles with author `poster <p@x>`, date `2026-10-03 10:00:00` and made-up ids. Half are `[{k%20+1:02}/20] - "Some.Show.S01E{set%90:02}.1080p-GRP{set}.part{k%20+1:02}.rar" yEnc ({k%20+1}/20)` with `k` the running number and `set = k / 400`, the other half `[{k%200+1:03}/200] "Album{k/2000}_{k%200+1:04}.jpg" yEnc (1/1)`. For each of `alt.binaries.test`, `alt.binaries.boneless`, `alt.binaries.e-book` and `alt.binaries.sounds.mp3`, it prints articles/s and µs/article for `parser::group_articles`, then the time of `collections::group_articles(group, Vec::new())` as the first use of that group's regexes, then articles/s and the release count for `collections::group_articles`. Then a worst case for `alt.binaries.test` and `alt.binaries.e-book`: every article its own file, `[{i%9000+1}/9000] "Some.Name.{12 random}.{i}.bin" yEnc (1/1)`.
- [ ] **In `examples/bench_writes.rs`**, `slice` (`:47`):

```rust
// `--by-name`: the file name grouping from before collections
let articles = headers_to_articles(headers);
let grouped = if std::env::args().any(|a| a == "--by-name") {
    group_articles(articles)
} else {
    atlas::collections::group_articles(group, articles)
};
let mut releases: Vec<Release> = grouped.into_values().collect();
```

  and after `db::create_db_at(main)` in `main`:

```rust
// `--old-rows`: a shard from before collections, each file name looked up
if std::env::args().any(|a| a == "--old-rows") {
    for i in 0..store::SHARDS {
        db::open_at(&store::shard_path(main, i)).unwrap()
            .execute("update meta set value = 2 where key = 'grouping_from'", []).unwrap();
    }
}
```

- [ ] **Test:** none: these are measurements. `cargo clippy --all-targets --locked -- -D warnings` compiles them, which is the check.
- [ ] **Run, for information:**

```bash
cargo run --release --locked --example bench_collections
for flags in "--by-name" "" "--old-rows"; do for i in 1 2 3; do
  rm -rf "$(pwd)/target/bw"; mkdir -p "$(pwd)/target/bw"
  cargo run --release --locked --example bench_writes -- --db "$(pwd)/target/bw/atlas.db" --slices 300 --slice-size 2000 --groups 30 $flags | tail -1
done; done
rm -rf "$(pwd)/target/bw"
```

  Put the numbers in the PR description. Per the overview, a benchmark never stops a part. Run these only in the worktree: `--db` writes to the path it's given, and must never point at the repo root's live database.
- [ ] **Commit:** `Add benchmarks for collection grouping`

### Task 8: Docs and the full check

**Files:** `README.md`.

- [ ] **Edit** the sections in section 12. **Test:** `grep -c "collection" README.md` is at least 3.
- [ ] **Run:** `just format-check && just lint && just test`, and every check in section 11. **Expected:** pass.
- [ ] **Commit:** `Document collection grouping`

## 9. Performance and load

Measured on the user's Mac at `nice 15`, release profile without LTO, on a scratch copy. The low end is a quiet machine, the high end `atlas --compact` running. They're informational: no number here is an acceptance check, and Task 7 measures the real thing.

| Cost | Measured | At 30,000 articles/s |
|---|---|---|
| Grouping by name, today | 0.9–6 µs/article | 3–18% of a core |
| Grouping by collection | 1.9–11 µs/article, about 1–5 µs more on the same run | 6–33% of a core |
| Worst case: every article its own file, 5 generic regexes each | 3.2–16 µs/article, over 60,000 articles/s on one core | 10–49% of a core |
| A group's regexes, first use in a process | 2–38 ms, once | Per group |
| All 602 compiled, if every group in the table is tracked | 250–540 ms in total, +53 MB of memory | Lazy: only groups in use |
| 28 regexes all missing, one base | 8 µs. The memo runs this once per distinct base in a slice | |
| Per-slice memo | At most one entry per distinct base, about 2 MB at 10,000, freed with the slice | Bounded by `request_size` |
| Writer, best of 5 under load, 100 new releases per 2,000 headers, before the key's digit rule, which doesn't touch the save path | By name 169,800 headers/s; by collection 138,700 (−18%); with old-row probes 123,100 (−27%) | One writer of 8. Each is over 4× the target alone |
| 40,000 files in one release, one save | 0.19 s, 8.6× a 5,000-file save: linear | |

**At 118M releases:** nothing is scanned or rewritten. On first open, `create table` and `select max(id)` take under a millisecond a shard. The probe and `collections` lookups are point reads on existing indexes. When a post's files were separate releases, each had an upsert; now one upsert plus `collections` plus one probe per distinct name, so a slice's index touches stay about level, or fall.

**Disk.** `collections` is about 25 bytes a row, new rows only: 2.5 GB over 100M new collections across 8 shards, against the `releases` and FTS rows it saves. `release_ids` adds about 30 bytes per article for a post that's newly multi-file. A 50-image post costs 1.5 KB there and saves 49 `releases` rows with their FTS entries, which is more. RAR sets are multi-file already, so they change nothing.

**NNTP:** none added. `NAME_SOURCES_MAX` can only reduce body fetches.

**Release count.** The overview's 58 of 100 sampled releases that are single images keep their own rows when they have no counter, and merge when they have one. How many merge isn't known without reading live data, and isn't needed for any decision here.

## 10. Failure handling

- **Bad input.** A subject `parse_subject` rejects is dropped as today, before any of this. A counter with digits past 5 doesn't match. A forged counter (`n > m`, `m > 10,000`) means no collection. An empty cleaned name still gives a key with that group and `m`; under 16 characters, it gets poster and day.
- **A regex that fails to compile at runtime:** logged, skipped. `every_regex_compiles` means it can't happen with the committed file.
- **A forged post of 10,000 files:** one release, a bounded `release_complete` scan, 8 body lookups at most.
- **Server error:** not involved. A failed slice saves nothing and is redone.
- **Crash partway through a save:** the mapping and the row commit in one transaction. A `collections` row can't exist without its release from a crash.
- **Re-run or second server:** the key is the same, the row is found, and articles dedupe by message-id, within a file and across a multi-file release (`add_segment`). Re-reading a range from before the upgrade routes into the old rows.
- **Hash collision:** 64-bit keyed SipHash. Across 100M collections in one shard, about 3 in 10,000 of one collision ever, and the two posts would share a row with separate `files`. The key is secret, so a post can't aim for one. When only 8 digits of a suffix collide, the second post takes the next free number and they stay apart.
- **`meta.grouping_from` missing:** treated as 1, so no probes. It's missing until the first indexer of this build runs. A release an AI search saves before that falls below the mark once it's set, and counts as an old row, which is harmless: a later file of it goes in by name, as today. If `start_grouping` fails, the indexer warns and goes on without the probes, and the writer will be hitting the same fault.
- **Downgrade to an older build:** it ignores `collections`, and its compaction would drop the table's rows. Those posts' next articles would then upsert by name. Nothing crashes.

## 11. Acceptance checks

In the worktree, with no live data and no network:

```bash
cargo test --locked --lib collections          # regexes compile, the six required shapes, posters, days, forged counters
cargo test --locked --lib store                # identity, old rows, name collisions, upgrade, menu purge
cargo test --locked --lib compact              # collections and grouping_from survive, dangling rows go
cargo test --locked --test convert             # no collection rows and no mark yet: the first indexer sets it
cargo test --locked --test collection_paths    # one release over slices and servers, posters apart, full NZB
cargo test --locked --test grouping_start      # only the indexer sets grouping_from, and a straddling file stays in its row
cargo test --locked --test parser_parity       # unchanged, and its fixtures untouched
just format-check && just lint && just test
git diff de598b6 --stat -- tests/fixtures/ tests/parser_parity.rs   # empty
python3 -I scripts/collection_regexes_to_tsv.py "$SRC/database/seeders/CollectionRegexesTableSeeder.php" "${TMPDIR:-/tmp}/c.tsv" \
  && cmp "${TMPDIR:-/tmp}/c.tsv" data/collection_regexes.tsv     # the committed file is what the script makes
grep -c fancy-regex Cargo.toml                       # 0
```

## 12. Docs

- **`README.md`:**
  - "How the database is stored": the `collections` table, and that a post's files share a release.
  - "Index": a multi-file post is one release, and a post straddling the upgrade stays split by file.
  - "Search": a merged release is named after one of its files until a better name is found.
  - "Limitations":
    - No merge without a counter or a group regex.
    - Two same-day posts by one poster of one shape and file count merge, and such a row, with more files than its `file_total`, stays incomplete. Part 3's rules decide what happens to it.
    - A pack merges only when each file's `[n/m]` equals its number, so a pack with an nfo first stays by file, as today.
    - Album tracks named with the track number first stay by file, as today.
  - "Source layout": `src/collections.rs`, `data/`, `scripts/`, `examples/bench_collections.rs`.
  - "Credits": the collection regexes and cleaner are from newznab-tmux, GPLv3.
- **Dashboard pages:** none. Release counts simply grow more slowly.

## 13. Decisions

1. **A side table holds identity, and `unique(name, group_name)` stays.** No rewrite, no change to `releases`, and a save still has one upsert. A nullable column with a unique index would mean building that index on 15M rows per shard at open, which hard rule 7 forbids. Keying `name` itself on the collection would mean showing upstream's key text or a tidied copy, and tidying it can merge unrelated posts.
2. **`name` is the shortest parsed file name in the first slice.** It's a real name in the form every row already has, so nothing downstream needs a new case. Shortest beats first because `T` is better than `T.sample.mkv`, and it makes the `name =` probe of Part 6 work for the common `T.mkv` plus `T.par2` post. Part 5 improves on it through `display_name`, never `name`.
3. **A taken name gets ` [` 8 decimal digits `]`.** `a [4]` is already a form like it, and Part 5 strips exactly this one. Digits classify as neutral in Part 5's token test, where hex would read as random and could change how a real name is judged.
4. **The key is upstream's, plus the group, with two changes for identity that lasts forever.** Upstream's cleaner strips any digit run before an extension, and its hash has no poster (its one poster hash, `RegexService.php:417`, serves a test tool). That's safe upstream because a collection is short-lived and a stale one is released as it stands (`ReleaseProcessingService.php:1083-1105`). In Atlas it would merge `Name.2019` with `Name.2021` from any poster on any day, forever. So digits stay in the key unless the file is numbered within its post, and poster and day join the key for numbered files and for names under 16 alphanumerics. Not for other long names, because random per-article `From` would shatter real files.
5. **Two same-day posts by one poster of one shape and file count merge,** for example two `[n/5] "DSC000n.jpg"` sets, or two 12-track albums whose first word matches. The subject can't tell them apart. The merged row has more files than `file_total`, so it's never complete, and Part 3's rules deal with it. The review offers narrowing the window to the hour. That splits every numbered post that crosses an hour, which is far more common than this. It's pinned by `what_stays_merged_or_split_on_purpose`.
6. **The numbered rule, and why these numbers.** A single subject can't separate a pack's first file from an episode post's mkv, so the rule goes by what differs across each post's files, as 4.1 explains.
   - `m >= 3` for non-images, because at `m = 2` an episode and its nfo would otherwise split at E01 and E02. A 2-file numbered set staying as 2 releases costs little.
   - Images have no `n` test, because cameras don't number from 1. A cover in an episode's post (`[3/5] "Show.S01E01.jpg"`) is then numbered and sits apart from its post, which is what happens today.
   - **The accepted cost:** `[1/9] "Show.S01E01.mkv"` in an episode's post has `n` equal to its digits, so that mkv is apart from its 8 par2 files, as it is today. A wrong merge needs a second upload by the same poster on the same day with the same cleaned prefix and `m`, at a file whose `n` equals its digits. Across a season's episodes that happens only at E01, because each later episode's mkv has `n` unequal to its digits.
   - **Regexes come after this rule.** Group regexes mostly capture the name with its digits, so numbered sets would otherwise stay split in groups that have them. Generic cleaning keeps any text outside the quotes, so the post's identity isn't lost.
7. **Only counters with `m >= 2`, or a group regex, merge.** Without one, nothing says the files belong together. Upstream's generic cleaner would happily reduce `photo123.jpg` and `photo456.jpg` to one name, but upstream's row is short-lived. Here that would be one row forever.
8. **`MAX_COLLECTION_FILES` is 10,000.** Above any real post, and it bounds `release_complete`'s per-save scan and an NZB's size against a forged counter.
9. **Old rows are continued by file, not adopted or rewritten, and only the indexer sets the mark.** See 4.5. The probe stays on for as long as `grouping_from > 1`, since it's what keeps a late sweep from storing an old post twice. Its cost is a point read that replaces the upsert each of those files had anyway. Setting the mark on open would let a menu of this build set it while an older indexer goes on writing by file name, and those rows would then look new.
10. **`regex` only, `&str` matching, Unicode classes kept.** Zero of 602 need more, per 4.2. PCRE treats rules without `u` as ASCII, so `\w` here matches more on a non-ASCII subject. That can only let a rule match a little more often, and it keeps one code path. Byte mode can't compile rules 15 and 21, which have non-ASCII literals and no `u` flag. Part 5 made the same call, its decision 2.
11. **Completeness stays `files == file_total`.** Upstream accepts `total + 1` (`CollectionHandler.php:750`), for an extra `[00/50]` file. `src/db.rs:662-664` pins the opposite in a test: a third file when the post said 2 makes it incomplete. It's today's behavior for those posts too, not a regression, and Part 3's completion percent treats them as done.
12. **Files stay identified by filename, not file number.** Upstream uses `md5('file:'.n)` (`BinaryHandler.php:570`). Atlas's `files_key` and every NZB go by filename, and `remap_bracket_parts` shows that a `[n/m]` isn't always a file number.
13. **The old `group_articles` stays public, and parity tests stay as they are.** `parse_subject` doesn't change, so the parity fixtures stay meaningful and need no update. `--by-name` gives a benchmark baseline.
14. **Lazily compiled per group, memoized per slice, no `RegexSet`.** One slice's articles mostly share few bases, so the memo removes nearly all regex work; measured, the memo's slowest case, every base distinct, still runs at 61,000 articles/s. A set would add 100s of KB per group for a miss path the memo has already made rare.
15. **Keys are lowercased.** A set's `.nfo` is often lowercase while its RARs aren't, and upstream's regexes capture the name as posted. A false merge needs two posts differing only by case.
16. **Converter copied from Part 5, not shared.** Part 2 runs first, and one dev-only script for each seeder is simpler than a module for two scripts. Its `convert` is word for word Part 5's.
17. **`name_sources` capped at 8.** A release needs one name. With merging, one job can hold what used to be thousands of jobs, and `first_names` runs a job's sources in sequence. Part 4 owns this function afterward.
18. **Audio goes by the music cleaner whatever its group.** Titled tracks have nothing in common but the text outside the quotes and the first word inside, so generic cleaning can't merge them. Upstream only does this in music groups, where its regexes and posters behave. It stays per-poster and per-day through its short key. Tracks named with the number first (`01 - Song.mp3`) have a different first word each, so they stay by file, as today, rather than merge on nothing but the poster and count.
19. **`COUNTER_RE` and `subject_base` have one home.** Part 1 owns the counter pattern, so the two rules can't drift. Part 5 runs its regexes on `subject_base`, so it stays public.
