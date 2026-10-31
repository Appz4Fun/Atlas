# Part 5: Name fixing

Every Atlas line number below is at base commit `de598b6`. Parts 1–4 merge first and shift them: find the named function, keep the intent, and record any difference in the PR. Upstream paths are at the commit pinned in `00-overview.md`. Part 3 wins any disagreement with this plan.

The converter, the obfuscation check, the judge and `decide` were prototyped in a scratch crate against the pinned seeder before this plan was written. Numbers marked "measured" come from that run, on synthetic or upstream data and never on live data.

## 1. Goal

A release whose posted name is junk gets a release-like name from what Atlas already knows: 1,154 upstream regexes over its subjects and group, its posted filenames, and whatever Part 4 read from its PAR2 and archives. With a body budget it can also read the NFO. Names only improve: a worse source never overwrites a better one, and a second run changes nothing. Upgrading only names releases that have no `display_name`. Replacing an existing name is opt-in through `atlas --reprocess --step naming`, and `atlas --unname` undoes the names regexes and posted files gave. The obfuscation flag catches dotted, hyphenated and short random names, so they leave search unless they have a real name. All of it is database work with no network, except the NFO step, which needs Part 3's body budget and costs nothing at the default of 0.

## 2. Non-goals

- No PreDB (Part 6). No categories (Part 7). No search or API format change.
- No PAR2 hash donors (`fixNamesWithParHash`), see decision 9.
- No SRR, CRC32, UID or mediainfo naming: Atlas has none of that data.
- No fuzzy matching of any kind.
- No new table, and no column beyond Part 3's `name_source`.
- The stage does not rewrite `is_obfuscated` on rows it never reaches. `atlas --reflag-obfuscated` does, dry run by default.
- Inline lookups stay as Part 4 left them.
- No copy of a replaced name. `--unname` clears names and does not restore old ones (decision 22).

## 3. Source map

| Upstream | Take | Leave behind |
|---|---|---|
| `database/seeders/ReleaseNamingRegexesTableSeeder.php` | All 1,157 rows through the converter | Rows with `status != 1` (ids 1013, 1014), and rule 1131, whose group regex has an unbalanced `)` and can never apply |
| `app/Services/RegexService.php:291-315,333` | Rules for a group in `ordinal, group_regex` order; first output wins upstream | The 15-minute cache, the admin CRUD |
| `app/Services/RegexService.php:354-378` | Named groups joined in name order, skipping names with `reqid` or `parts` | Numbered groups |
| `app/Services/Binaries/HeaderParser.php:18-20,199-200` | Upstream's subject is what comes before ` (n/m)`, plus ` yEnc` when it has none. Part 2's `collections::subject_base` already ports it, and the regexes here run on its output | The rest of the parser |
| `app/Services/ReleaseCleaningService.php:587-600` | `lRap` is `yEnc` in ROT13: the whole subject is scrambled, so its filename is too | |
| `app/Services/ReleaseCleaningService.php:630-660` | Clean-up order: sample markers, archive suffixes, part and volume markers, `Release Name` prefix | `mb_convert_encoding` over every encoding (`BUGS.md` #30) |
| `app/Services/ReleaseCleaningService.php:125-183` | Nothing: request ids need a feed column no source fills | The request-id lookup |
| `app/Services/ReleaseCleaningService.php:205-563` | Nothing: `parse_subject` (`src/parser.rs:83`) already does what `generic`, `teevee` and the town patterns do for a filename | All of them |
| `BUGS.md` #38 | A hashed filename must not become a name: the judge runs on every candidate | |
| `app/Services/NameFixing/FileNameCleaner.php:163-192` | Wrappers come off repeatedly, then one payload extension; a codec word after a wrapper is part of the title | `FFI` paths |
| `app/Services/NameFixing/FileNameCleaner.php:251-303,482-495` | Structural junk (`VTS_01_1`, `audio_ts`, bare numbers), sample and part tails | Leading-track-number removal: it breaks `24.Hours.S01` |
| `app/Services/NameFixing/FileNameCleaner.php:418-462` | The plausibility rules, as `Grade::Good` | |
| `app/Services/NameFixing/FileNameCleaner.php:571-585` | Scene shape plus a technical token, as `Grade::Scene` | |
| `app/Services/NameFixing/NzbSplitUnwrapper.php:13,54-66` | `__NZBSPLIT__<hex>__NZBSPLIT__payload`, `_` to `.`, `+` to space | The trailer loop: Atlas gets clean filenames |
| `app/Services/NameFixing/FilePrioritizer.php:96-115,154-200,299-316` | Skip samples, proofs, `.url` and structural junk; prefer main RAR, then video | SRR tiers |
| `app/Services/NameFixing/FilePrioritizer.php:336-378` | The most common candidate wins ties. Atlas uses summed size, because Part 4 collapses volumes | |
| `app/Services/NameFixing/SeasonPackDetector.php:40-49,83-110` | Episode and season patterns, lookaheads rewritten as consuming groups, since only `is_match` is used | Episode parsing |
| `app/Services/NameFixing/ReleaseUpdateService.php:217-315` | Order of checks: clean, plausibility, same-name, season pack | The locks and snapshots: Part 3's writer owns that |
| `app/Services/NameFixing/ReleaseUpdateService.php:1272-1277` | A season pack is never renamed to one of its episodes | |
| `app/Services/NameFixing/ReleaseUpdateService.php:850-873` | Part 3's `on_rename` covers this | The column list |
| `app/Services/NameFixing/NameCheckerService.php:358-374,395-447` | Token vocabulary, which is how `Patterns/*` routes names | The FFI batch plan |
| `app/Services/NameFixing/Patterns/{Tv,Movie,Game,App}Patterns.php` | Nothing directly: see section 4 | 39 ordered regexes, many with lookaround |
| `app/Services/NameFixing/Extractors/FileNameExtractor.php:128-404` | The generic cases, through `clean_candidate`, `parent_dir` and `judge` | Group-specific patterns (TVP, R&C, NhaNc3, VIDEOOT, SDPORN) and invented tags |
| `app/Services/NameFixing/Extractors/NfoNameExtractor.php:114-140` | `identity_text`: a `Source:` field names another release and must not count | |
| `app/Services/NameFixing/Extractors/NfoNameExtractor.php:267-276` | Literal fields `Filename`, `File Name`, `Complete Name`, `Release Name` | |
| `app/Services/NameFixing/Extractors/NfoNameExtractor.php:350-369,383-407,468-496` | TV line, movie line, `Title (Year)` | `-NoGroup`, appended tags, FM radio, PC games `(c)`, IGUANA |
| `app/Services/NameFixing/NameFixingService.php:979-1040` | `=newz[NZB]=` NFOs are skipped | Literal PreDB lookup (Part 6) |
| `app/Traits/DetectsHashedNames.php:38-56,307-338` | The hex, UUID, case-transition and consonant-run ideas | The exact rules: they flag `TheMatrix2`-shaped real names |
| `app/Services/NameFixing/DonorMatchSelector.php:118-146`, `NameFixingService.php:386-411,825-893`, root to-do file lines 212-220 | Read for decision 9 | All of it |
| `BUGS.md` #18, root to-do file lines 275, 281, 282 | Separate "matched", "checked, no match" and "retry"; database work never waits on downloads | |

`rust/pipeline-ffi/src/name_checkers.rs` and `filename.rs` are byte-matching ports of the same patterns behind a C interface. Nothing is lifted from them.

## 4. Atlas changes

**`src/parser.rs`**: `:77-81` becomes `pub fn is_obfuscated(name: &str) -> bool { crate::naming::obfuscation::is_obfuscated(name) }`. The caller at `:189` is unchanged, and `RELEASE_SUFFIX_RE` (`:75`) still runs first, so the check sees `name.mkv` but never `name.part01.rar`. In the test at `:325-329`, `:327` asserts that the 15-character `a1B2c3D4e5F6g7H` is not flagged. Catching that shape is the point of this change, so the line becomes `assert!(is_obfuscated("a1B2c3D4e5F6g7H"));`, and a new line adds `assert!(!is_obfuscated("Some.Show.S01E01"));`.

**`src/nfo.rs`**: `display_name` (`:19-37`) keeps its signature, used by the inline path (`src/indexer.rs:1642`) and by the new step. New `identity_text` and `name_candidates`, section 6. `valid_name` (`:39-51`) stays.

**`src/search.rs`**: both lines keep Part 1's blocked-group clause. `:231` becomes `"r.is_obfuscated = 1 and r.display_name is null and not atlas_blocked(r.group_name)"`. `:237` becomes `"select count(*) as c from s{i}.releases where is_obfuscated = 1 and display_name is null and not atlas_blocked(group_name)"`. README `:287` already says the menu lists releases "that still only have a random name". Today a release with a recovered name stays listed. `VISIBLE_RELEASE` (`:36-39`) does not change, decision 7.

**`src/stats_dashboard.rs`**: `:170` `coalesce(sum(is_obfuscated), 0)` becomes `coalesce(sum(is_obfuscated = 1 and display_name is null), 0)`, so the Content page's "obfuscated" means the same as the menu. Part 1's `having not atlas_blocked(group_name)` at `:172` stays. `:912` is unchanged.

**`tests/e2e.rs`**: `:84` expects `0`, because its one flagged release has a recovered name.

**`src/nzb.rs`, `src/api.rs`**: no change. `get_release` returns `coalesce(display_name, name)` (`src/search.rs:33`), so `nzb_filename` (`src/nzb.rs:12-22`, `:129`) follows a rename and already sanitizes it. `render_nzb` uses each file's own subject (`:83`), so downloads are unaffected. A test covers it.

**`src/store.rs`**: no change. `releases_au` (`:155-160`) updates FTS on any `display_name` change, and Part 3's upsert keeps a step's name.

**`src/main.rs`**: `--reflag-obfuscated` and `--unname` dispatch at `:22-32`, help at `:15-17`.

**`src/postprocess/reprocess.rs`** (Part 3): when an applied mask includes `NAMING`, the same per-shard transaction also runs `insert or replace into meta (key, value) values ('naming_rename_named', 1)`. That is the opt-in `Naming::run` reads, see "Who gets renamed". `--step all` includes it. A dry run writes nothing.

**`src/config.rs`**: `Config.naming: NamingConfig`, parsed in `from_value` (`:264`).

**`src/postprocess/steps.rs`**: `all()` gains `Arc::new(naming::Naming)` and `Arc::new(naming::NfoName)`, and loses Part 4's `NfoStep` line, as Part 4 asks. Id 3 is never reused.

**`src/archive/steps.rs`**: delete `NfoStep`; `select::nfo_sources` stays. In `finish`, the name `names::best_name` gave is used only if Part 4's existing condition holds and `naming::improves(ctx.release(), &name, source)` is true, with `NameSource::Par2` for PAR2 and `NameSource::ArchiveInner` for the others. Part 4's `looks_random` passes a name like `Xk3Mz9PqL.mkv`, and a junk name stored at source 20 would block every lower source for good. `NameChange.obfuscated` stays `Some(release.is_obfuscated)`: the menu and dashboard now go by `display_name`.

**`tests/archive_stage.rs`** (Part 4): `steps::all()` now includes naming, which would rename its fixtures. Its harness sets `naming.enabled: false` through `set_config`. `inner_files_rerun_naming` keeps its recording step under id 8, which is free with naming off. The two `an_nfo_*` tests stay and now exercise `NfoName`. If a fixture's posted name grades `Good`, change it to an obfuscated one, because the step no longer spends a body on a release with a good name.

**New `src/naming/`**: `mod.rs`, `obfuscation.rs`, `judge.rs`, `clean.rs`, `regexes.rs`, `decide.rs`, `steps.rs`, `reflag.rs`, `unname.rs`. `src/lib.rs` gets `pub mod naming;`. Each file except `reflag.rs`, `unname.rs` and `steps.rs` carries a GPLv3 attribution header naming the upstream files in section 3.

**New `scripts/naming_regexes_to_tsv.py`, `data/naming_regexes.tsv`, `tests/fixtures/naming_samples.tsv`**: Task 3.

### What the seeder really needs

The brief for this part expected about 39 rules needing lookaround or backreferences. A tokenizer over the seeder finds none: 0 lookaround, 0 backreferences, 0 atomic groups, 0 recursion. A crude grep for `[+*?}]\+` does find 39 rules, and they look like group syntax. But 35 of them are `\?+`, usually as `^\(\?+`: a literal `(` then one or more literal `?`. The other 4 are the possessive quantifiers below. So `regex` is enough and no `fancy-regex` is added. The converter makes these rewrites, each keeping PCRE's meaning:

| Rules | PCRE | Becomes | Why |
|---:|---|---|---|
| 4 (34, 35, 342, 1157) | `\d?+`, `\d*+` | `\d?`, `\d*` | The crate reads `x?+` as `(x?)+`. Possessive to greedy can only widen a match, and in all four a `.+` or `yEnc$` follows, so the difference is one digit at a boundary |
| 5 | `[` inside a class | `\[` | A nested class to the crate |
| 2 | `]` first in a class | `\]` | |
| 2 | `{` not a quantifier | `\{` | An error to the crate |
| 1 (1023) | `\E` with no `\Q` | dropped | PCRE ignores it |
| 2 | `status != 1` | dropped | Disabled upstream |
| 1 (1131) | group regex with a stray `)` | dropped | Never applies |

Result, measured: 1,154 rules, all compile. Of the 1,045 subjects in the seeder's own descriptions that Python's `re` matched with the original pattern, all 1,045 give the same name in Rust. Compiling everything takes 2–5 s and 100–320 MB across runs, so per-group laziness matters: the largest group, `alt.binaries.teevee`, has 89 rules, compiles in 0.2–0.5 s, and misses in 12–34 µs per subject.

### Checkers: what is ported and what is dropped

| Upstream | Decision | Reason |
|---|---|---|
| `TvPatterns` (12), `MoviePatterns` (11), `GamePatterns` (9), `AppPatterns` (7) | Drop | 39 regexes that each say "title words, some markers, a group", differing in marker order. `judge` asks the same question order-free. They exist to find a name inside free text; here the candidates are already isolated |
| `FileNameExtractor` literal title, NZBSPLIT | Port | `clean_candidate`, then `Grade::Scene` |
| `FileNameExtractor` archive path, folder name | Port | `parent_dir`: `Title-GRP/grp-title.mkv` names the release in its directory |
| `FileNameExtractor` TV, movie, music, console, ebook, audiobook, technical basename | Merge | All pass `judge` after cleaning |
| `FileNameExtractor` TVP, R&C, NhaNc3, VIDEOOT, SDPORN, LOL, DL, 3DS, NDS | Drop | Group-specific and old. The naming regexes already hold most of these groups |
| `FileNameExtractor` `Movie (2020).avi` to `DVDRip XVID NoGroup` | Drop | Invents tags. A name must come from the evidence |
| `NfoNameExtractor` literal fields, TV, movie, `Title (Year)` | Port | Without appended tags |
| `NfoNameExtractor` music FM, PC games, IGUANA | Drop | Rare and fabricating |
| `ObfuscatedSubjectExtractor` | Drop | Rewrites dots to spaces and title-cases. Lossy, and only useful for search text |
| `FilePrioritizer` | Port | `ignorable` and `rank` |
| `SeasonPackDetector` | Port, reduced | Blocks pack-to-episode and many-episodes-to-one |
| `CRC32`, `SRR`, `UID`, media, XXX file passes | Drop | Data Atlas does not have |
| `PredbMatchSelector`, `ConstrainedPredbMatcher` | Part 6 | |

**The final order.** `decide` looks at evidence in this order and keeps the highest `(grade, source)` that `improves` accepts, stopping as soon as one is `Scene`:

1. PAR2 file names (`archive_inner_files.source = 1`), `NameSource::Par2`.
2. RAR, 7z and ZIP inner names (`source` 2–4), `NameSource::ArchiveInner`. From a passworded archive, only `Good` or better: names in passworded posts are where fakes live.
3. The naming regexes over up to 4 subjects, `NameSource::NamingRegex`, only `Good` or better. Skipped when the current name is `Scene`.
4. Posted filenames, `NameSource::PostedFile`, only `Good` or better. Skipped likewise.

A `Weak` name needs evidence from inside the post. Subject text can be the poster's advert: rule 132 in `alt.binaries.boneless` turns `Brought to you by Uploader [01/35] - "…" yEnc` into `Brought to you by Uploader`, which is `Weak`, and upstream's plausibility rule rejects it too.

Step `nfo` (`NameSource::Nfo`) runs separately in the net lane, because it needs a body. Inside `nfo::name_candidates`, in order: a literal field that is `Scene`; any `Scene` token in the text; `<title>` tags; `Release:`, `Title:` and similar lines; a literal field that is `Good`; a TV line; a movie line; `Title (Year)`.

### `NameSource`

| Name | Value | Written? | Why here |
|---|---:|---|---|
| `Ingest` | 0 | Never: `name_source` stays NULL, which Part 3 reads as 0 | The posted name |
| `PostedFile` | 3 | Yes | Repeats what ingest parsed |
| `NamingRegex` | 5 | Yes | Written per group by people, but from text the poster controls |
| `InlineBody` | 9 | Never: the effective value of a good name with NULL `name_source` | Part 4's inline lookup could not say which source it used |
| `Nfo` | 10 | Yes | From the post, but free text, and it can quote other releases |
| `ArchiveInner` | 15 | Yes | Real, but often a short per-file name (`grp-title.mkv`) |
| `Par2` | 20 | Yes | The set's own name, usually the whole release's |
| `PreDb` | 100 | Part 6 | A catalogue, not the poster |

10, 15 and 20 are the values Part 4 already writes, unchanged. Part 4 assumed other sources would sit above them (its section 6 and decision 15). Regexes and posted files go below instead, because they are weaker evidence than anything read from inside the post. What Part 4 wanted from that, that its names don't lock out better ones, comes from `improves` here and in Part 4's `finish`. Values 21–99 stay free. `naming::tests::values_match_part_4` pins `archive::NAME_SOURCE_*`.

Part 3 applies `coalesce(name_source, 0) <= source` in its update. That guard alone would let a regex overwrite an inline name, because NULL reads as 0. So `improves` uses `effective_source`, and the step sends nothing it should not win. Every `NameChange` from this part has `display_name: Some(..)` and `obfuscated: Some(..)`: Part 3 accepts `None` for either, but every change here comes with a name, and the flag goes with it.

### Who gets renamed

| Row | `naming` |
|---|---|
| `display_name` NULL | Runs, if `worth_running` says it can improve. This is everything the upgrade walk may rename |
| `display_name` set, `name_source` set | A stage step named it after the upgrade, so new evidence may improve it |
| `display_name` set, `name_source` NULL | The inline lookup named it. Left alone: `run` marks the step done and proposes nothing, unless this shard has `meta.naming_rename_named` |

At upgrade every named row is in the last class, because no step has run yet. So restarting on the new build renames no release that has a name. `atlas --reprocess --step naming --apply` clears the done bits and sets the key, and only then do those rows run `decide`. `improves` still applies to them, so regexes and posted files can only replace an inline name that grades `Junk`. The key stays set until `atlas --unname --apply` removes it, so in that shard a late article can also rename an inline-named row.

`atlas --unname [--sample N] [--apply]` clears every name with `name_source` 3 or 5, the two sources that come from subject text, so a bad regex port can be undone. It does not touch names from NFOs, PAR2, inner files or PreDB. After it, those rows stay unnamed until new evidence or a `--reprocess`, because their `naming` done bit stays set.

### What `naming` skips without reading anything

Part 3's `judge` no longer rejects by size or image type, so about 100M rows pass it. `Naming::applies` is `worth_running`, which uses only `ReleaseInfo` and returns false, in this order, when:

1. `name_source` is `PreDb`.
2. The current name is `Scene` with nothing to clean off: no candidate can grade higher. A name ending in Part 2's collision suffix skips rules 2–4, because it has something to tidy.
3. `parts <= 1` and the group has no naming rules. One article is one file, and `name` came from that file's name, so the posted-file tier can't add anything and no regex can fire.
4. `name` has an image extension, and either `parts <= 1` or the group has no rules. A picture has no release name worth a read. This is the regex in Part 3's `fetch_bodies`.

`applies` false means not run and not marked done (Part 3 §6). Part 4 can still read a one-article ZIP in such a row, and its `finish` names it through `improves`, so inner evidence isn't lost.

### `is_obfuscated`: effect on old rows

| | Today | New saves | Old rows, before `--reflag-obfuscated` | After it |
|---|---|---|---|---|
| Flag | `^[A-Za-z0-9]{16,}$` | The new check, at `src/parser.rs:189`; the upsert writes it while `name_source` is NULL (Part 3's change to `src/store.rs:1370`) | Stale until a late article touches the row | Set for every row the new check flags with NULL `name_source` |
| Search (`src/search.rs:38`) | Hides flagged and 16+ alnum names with no `display_name` | Same query, more rows flagged | Unchanged: the newly caught shapes stay visible | They leave search and the API listing unless named |
| Menu and dashboard (`:231`, `src/stats_dashboard.rs:170`) | Count flagged, named or not | Flagged and unnamed | Named rows drop out at once; the count understates, as the overview's table says | Full count |

The new check flags everything the old one did (decision 6), so the update only ever sets 1. A row a step named has `is_obfuscated = 0` from its `NameChange` and is skipped, as is any row with `name_source` set. The Content page refreshes on its own cycle, so its count moves after the command finishes. Compaction and `--convert` copy the flag as stored, so a converted old database needs the command too.

## 5. Schema and migration

None. Part 3's `name_source` is the only column used. `atlas --reflag-obfuscated` keeps a transient integer in `meta` (`reflag_after`), removed at the end. `meta` is copied by compaction (`src/compact.rs:446`), so a stopped run resumes after a compaction.

| Path | What happens | Test |
|---|---|---|
| `--compact` | `display_name`, `is_obfuscated`, `name_source` are `releases` columns, copied by name | Part 3's `every_registered_table_and_column_survives_compaction` |
| `--convert` | Flags copied as they were (`src/convert.rs:353`); `name_source` NULL | `reflag_runs_after_a_convert` |
| Wipe | Nothing outside the shards | Part 3's `a_wipe_leaves_new_empty_tables` |

## 6. Interfaces

### Provided (`src/naming/mod.rs`)

```rust
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum NameSource { Ingest = 0, PostedFile = 3, NamingRegex = 5, InlineBody = 9, Nfo = 10, ArchiveInner = 15, Par2 = 20, PreDb = 100 }
impl NameSource { pub const fn value(self) -> u8 { self as u8 } }

pub const NAMING: StepId = StepId(8);
pub const NFO: StepId = StepId(9);
pub use judge::{Grade, is_episode_name, is_season_pack, judge, renames_pack_to_episode, distinct_episodes};
pub use clean::{basename, clean_candidate, has_collision_suffix, without_collision_suffix, identity_key, parent_dir, strip_file_suffixes};
pub use decide::{best_from_paths, current_name, decide, effective_source, grade_now, improves, is_image_name, worth_running};
```

| Step | Id | Rank | Needs | `reruns` |
|---|---:|---:|---|---|
| `naming` | 8 | 200 | Database | growth |
| `nfo` | 9 | 210 | Bodies | growth |

Ids 10–15 and ranks 220–299 stay free.

### Provided (`src/naming/obfuscation.rs`)

```rust
//! Is this release name a random string rather than a real name? A flagged name leaves search
//! (src/search.rs:36), so a wrong yes costs more than a wrong no.
use std::sync::LazyLock;
use regex::Regex;

/// The old rule. Anything it flagged stays flagged: search hides these names whatever the flag says.
static LONG_ALNUM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9]{16,}$").unwrap());
/// What `parse_subject` leaves on a name (RELEASE_SUFFIX_RE only strips archive and par2 suffixes).
static PAYLOAD_EXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(mkv|mp4|m4v|avi|wmv|mov|ts|m2ts|mpg|mpeg|webm|iso|img|bin|7z|zip|exe|mp3|flac|m4a|epub|mobi|azw3|pdf|cbr|cbz)$").unwrap()
});
/// A token that is release vocabulary is never random.
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^((19|20)\d\d|s\d{1,3}(e\d{1,4})*|e\d{2,4}|\d{1,2}x\d{2,3}|\d{3,4}[pi]|[48]k|uhd|hdr|x26[45]|h26[45]|hevc|avc|xvid|divx|av1|bluray|bdrip|brrip|dvdrip|dvd|dvdr|webrip|webdl|web|hdtv|pdtv|sdtv|remux|flac|mp3|aac|ac3|dts|ddp?\d?|truehd|atmos|proper|repack|internal|retail|german|french|multi|xxx|ps[345p]|psv|xbox|nsw|wiiu?|win(32|64)?|x64|x86|macos|osx|linux|v\d+)$").unwrap()
});
/// The 160 most common letter pairs of a word list plus German, French, Spanish and Polish words and
/// titles. A random lowercase string has about 24% of its pairs in here.
const COMMON_PAIRS: &str = "ab ac ad ae ag ai al am an ap ar as at au ba be bi bl bo br ca ce ch ci ck co cr ct cu da de di do dr ea ec ed ee el em en eo ep er es et fe fi fo ga ge gi gl go gr ha he hi ho hy ia ic id ie if ig il im in io ip ir is it iv iz ke la le li ll lo lu ly ma me mi mo mp na nc nd ne ng ni no ns nt oc od og oi ol om on oo op or os ot ou ov pa pe ph pi pl po pr qu ra rc rd re ri rm ro rr rs rt ru ry sa sc se sh si sm so sp ss st su ta te th ti to tr tu ty ul um un ur us ut ve vi ze";

const MIN_TOKEN: usize = 8;             // shorter single-case or CamelCase tokens are not judged
const MIN_MIXED_TOKEN: usize = 6;       // shorter mixed-case tokens are not judged
const MIN_PAIRS: usize = 5;             // fewer letter pairs than this are not judged
const PLAIN_MIN_COMMON_PCT: usize = 25; // lowercase, UPPERCASE or CamelCase
const MIXED_MIN_COMMON_PCT: usize = 45; // irregular case
const MAX_CONSONANTS: usize = 6;        // y counts as a vowel
const MIN_HEX: usize = 12;              // one hex token with a digit
const MIN_JOINED_HEX: usize = 16;       // a uuid or hash cut up by separators
const MIN_MIXED_CHARS: usize = 12;      // short pieces that each mix letters and digits

fn is_common(a: u8, b: u8) -> bool {
    static TABLE: LazyLock<[bool; 676]> = LazyLock::new(|| {
        let mut t = [false; 676];
        for p in COMMON_PAIRS.split(' ') {
            let p = p.as_bytes();
            t[(p[0] - b'a') as usize * 26 + (p[1] - b'a') as usize] = true;
        }
        t
    });
    TABLE[(a - b'a') as usize * 26 + (b - b'a') as usize]
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Token { Neutral, Readable, Random }

/// One run of ASCII letters and digits.
pub fn classify(t: &str) -> Token {
    let b = t.as_bytes();
    let len = b.len();
    let digits = b.iter().filter(|c| c.is_ascii_digit()).count();
    if digits == len { return if len >= 16 { Token::Random } else { Token::Neutral }; }
    if MARKER.is_match(t) { return Token::Readable; }
    if digits > 0 && len >= MIN_HEX && b.iter().all(u8::is_ascii_hexdigit) { return Token::Random; }
    let upper = b.iter().filter(|c| c.is_ascii_uppercase()).count();
    let lower = b.iter().filter(|c| c.is_ascii_lowercase()).count();
    if len >= MIN_TOKEN && digits > 0 {
        let runs = b.iter().enumerate().filter(|(i, c)| c.is_ascii_digit() && (*i == 0 || !b[*i - 1].is_ascii_digit())).count();
        let interior = runs - usize::from(b[0].is_ascii_digit()) - usize::from(b[len - 1].is_ascii_digit());
        let flips = b.windows(2).filter(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase()).count();
        if interior >= 1 && (runs >= 2 && upper + lower >= 2 || flips >= 2) { return Token::Random; }
    }
    let plain = lower == 0 || upper == 0 || is_camel(b);
    if len < if plain { MIN_TOKEN } else { MIN_MIXED_TOKEN } { return Token::Neutral; }
    let (mut pairs, mut common, mut run, mut longest) = (0, 0, 0, 0);
    for (i, c) in b.iter().map(u8::to_ascii_lowercase).enumerate() {
        if !c.is_ascii_alphabetic() { run = 0; continue; }
        if matches!(c, b'a' | b'e' | b'i' | b'o' | b'u' | b'y') { run = 0 } else { run += 1; longest = longest.max(run) }
        if let Some(next) = b.get(i + 1).map(u8::to_ascii_lowercase).filter(u8::is_ascii_alphabetic) {
            pairs += 1;
            common += usize::from(is_common(c, next));
        }
    }
    if pairs < MIN_PAIRS { return Token::Neutral; }
    let least = if plain { PLAIN_MIN_COMMON_PCT } else { MIXED_MIN_COMMON_PCT };
    if longest > MAX_CONSONANTS || common * 100 < pairs * least { Token::Random } else { Token::Readable }
}

/// Capitalized words of at least 3 letters each, with an optional lowercase start: iPhone, StarCraft.
fn is_camel(b: &[u8]) -> bool {
    let letters: Vec<u8> = b.iter().copied().filter(u8::is_ascii_alphabetic).collect();
    let mut i = letters.iter().take_while(|c| c.is_ascii_lowercase()).count();
    while i < letters.len() {
        if !letters[i].is_ascii_uppercase() { return false; }
        let tail = letters[i + 1..].iter().take_while(|c| c.is_ascii_lowercase()).count();
        if tail < 2 { return false; }
        i += 1 + tail;
    }
    true
}

pub fn is_obfuscated(name: &str) -> bool {
    if LONG_ALNUM.is_match(name) { return true; }
    let stem = PAYLOAD_EXT.replace(name.trim(), "");
    // words with spaces, or letters outside ASCII: a person wrote it, or it can't be judged
    if !stem.is_ascii() || stem.contains(' ') { return false; }
    let tokens: Vec<&str> = stem.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() { return false; }
    let total: usize = tokens.iter().map(|t| t.len()).sum();
    let hex = tokens.iter().all(|t| t.bytes().all(|c| c.is_ascii_hexdigit()));
    if hex && total >= MIN_JOINED_HEX && tokens.iter().any(|t| t.bytes().any(|c| c.is_ascii_digit())) { return true; }
    let (mut random, mut readable, mut mixed, mut mixed_tokens) = (0, 0, 0, 0);
    for t in &tokens {
        let kind = classify(t);
        match kind { Token::Random => random += t.len(), Token::Readable => readable += t.len(), Token::Neutral => {} }
        if kind != Token::Readable && t.bytes().any(|c| c.is_ascii_digit()) && t.bytes().any(|c| c.is_ascii_alphabetic()) {
            mixed += t.len();
            mixed_tokens += 1;
        }
    }
    if mixed_tokens >= 2 && total >= MIN_MIXED_CHARS && mixed * 4 >= total * 3 { return true; }
    // most of the name is random, and what is left isn't release vocabulary
    random > 0 && random * 2 >= total && readable * 2 <= random
}
```

Measured: of 170,415 words of 8–15 letters in `/usr/share/dict/words`, 33 are flagged (0.019%), such as `Krzysztof` and `snuffbox`. Of 29,272 pairs of short words joined in CamelCase, 245 are flagged (0.8%), all nonsense pairs. Random strings flagged, at lengths 8, 10, 12 and 15:

| Alphabet | 8 | 10 | 12 | 15 |
|---|---:|---:|---:|---:|
| Mixed case and digits | 87.5% | 95.6% | 97.3% | 99.2% |
| Mixed-case letters | 88.8% | 92.4% | 88.1% | 95.1% |
| Lowercase and digits | 76.7% | 87.0% | 93.3% | 96.7% |
| Lowercase letters | 52.5% | 67.0% | 60.7% | 69.1% |

Two random mixed tokens joined by a dot: 9+7 characters 86.7%, 8+8 85.5%, 6+6 87.8%, 5+5 0%. A call takes about 300–450 ns.

### Provided (`src/naming/judge.rs`)

```rust
macro_rules! re { ($name:ident, $p:expr) => { static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($p).unwrap()); } }
re!(GENERIC, r"(?i)^(audio[._-]?ts|video[._-]?ts|vts[._-]?\d{1,2}[._-]?\d{1,2}|\d{1,3}|sample|proof|covers?|thumbs?|screens?|readme|index|movie|video|file|track\d*|cd\d*|dvd\d*|dis[ck]\d*|part\d*|vol\d*|image\d*|img\d*)$");
re!(SEGMENT, r"(?i)(?:^|[.\-_ ])(?:part|vol|r)\d+(?:\+\d+)?$");
re!(INSTALLER, r"(?i)^(setup|install|installer|patch|update|crack|keygen)\d*(?:[\s._-]|$)");
re!(WORDS, r"[\p{L}\p{N}]{3,}");
re!(TAIL, r"[-.][A-Za-z0-9]{2,}$");
re!(TV, r"(?i)\bS\d{1,2}[Eex]\d{1,3}\b");
re!(QUALITY, r"(?i)\b(480p|720p|1080p|2160p|4k|webrip|web[ .-]?dl|bluray|bdrip|dvdrip|hdtv|hdrip|xvid|x264|x265|hevc|h\.?264|proper|repack)\b");
re!(XXX, r"(?i)\bXXX\b");
re!(SCENE_SHAPE, r"^[\p{L}\p{N}][\p{L}\p{N}\p{M}._-]*-[A-Za-z0-9]{2,15}$");
re!(SCENE_TAIL_JUNK, r"(?i)[._-](?:sample|proof|thumbs?|part\d+|vol\d+(?:[+-]\d+)?)$");
re!(TECH, r"(?i)(?:^|[._-])(?:480p|576p|720p|1080[pi]|2160p|4320p|[48]k|web[._-]?dl|webrip|bluray|bdrip|brrip|dvdrip|hdtv|sdtv|x264|x265|h[._]?26[45]|hevc|avc|xvid|divx|aac|ac3|eac3|ddp?[0-9](?:[._][0-9])?|dts(?:[._-]?hd)?|flac|mp3|truehd|pal|ntsc)(?:$|[._-])");
re!(EPISODE, r"(?i)(?:^|[^a-z0-9])(?:S\d+[._ ]?E\d+|\d{1,2}x\d{2,}(?:$|[^a-z0-9]|[exs]\d)|Episode[._ ]?\d{1,4}(?:$|\D)|EP?\d{2,3}(?:$|[^0-9a-z]))");
re!(EPISODE_ID, r"(?i)(?:^|[^a-z0-9])(S\d+[._ ]?E\d+|\d{1,2}x\d{2,})");
re!(SEASON_TOKEN, r"(?i)(?:^|[._ \-\[(])S\d{1,4}(?:[._ ]?D(?:is[ck])?[._ ]?\d{1,2})?(?:$|[._ \-\])])");
re!(SEASON_WORD, r"(?i)(?:^|[._ \-\[(])Seasons?[._ ]?\d{1,2}(?:$|[._ \-\])])");
re!(COMPLETE, r"(?i)(?:^|[._ \-\[(])Complete[._ ](?:Series|Seasons?)(?:$|[._ \-\])])");

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Grade { Junk, Weak, Good, Scene }

/// "Is this a good name?"
pub fn judge(name: &str) -> Grade {
    let t = name.trim();
    let chars = t.chars().count();
    if chars == 0 || chars > 255 || t.chars().any(char::is_control) || t.contains(['\\', '/', '|']) { return Grade::Junk; }
    if GENERIC.is_match(t) || SEGMENT.is_match(t) || INSTALLER.is_match(t) || is_obfuscated(t) { return Grade::Junk; }
    let words = WORDS.find_iter(t).count();
    if words == 0 { return Grade::Junk; }
    // at least 4 words: `grp-title-1080p` has the shape but is a per-file short name, so it is only Good
    if words >= 4 && SCENE_SHAPE.is_match(t) && !SCENE_TAIL_JUNK.is_match(t) && TECH.is_match(t) { return Grade::Scene; }
    if t.len() >= 12 && words >= 2 && (TAIL.is_match(t) || TV.is_match(t) || QUALITY.is_match(t) || XXX.is_match(t) || is_season_pack(t)) { return Grade::Good; }
    if t.len() >= 8 && words >= 2 { return Grade::Weak; }
    Grade::Junk
}
pub fn is_episode_name(n: &str) -> bool { EPISODE.is_match(n) }
pub fn is_season_pack(n: &str) -> bool { !is_episode_name(n) && (SEASON_TOKEN.is_match(n) || SEASON_WORD.is_match(n) || COMPLETE.is_match(n)) }
pub fn renames_pack_to_episode(current: &str, new: &str) -> bool { is_season_pack(current) && is_episode_name(new) && !is_season_pack(new) }
/// How many different SxxEyy or NxMM markers these names carry.
pub fn distinct_episodes<'a>(names: impl Iterator<Item = &'a str>) -> usize;
```

### Provided (`src/naming/clean.rs`)

```rust
re!(WRAPPER, r"(?i)\.(?:7z\.\d{2,4}|part\d+\.rar|part\d*|r\d{2,4}|rar|z\d{2}|zip|7z|vol\d+[+-]\d+\.par2?|vol\d+[+-]\d+|par2?|tar|gz|bz2|xz|lz|lzma|cab|arj|ace|arc|zst|tgz|lz4|enc|rev|\d{3}|nfo|sfv|nzb|srr|srs)$");
re!(CODEC_AFTER_WRAPPER, r"(?i)\.(?:mp3|flac|m4a|aac|ogg|wav|wma|ape|opus|mka|ac3|dts|eac3|truehd|mpc|shn|tak|tta|wv|ts|divx|wmv|mpg|mpeg|rm|rmvb|m1v|m2v|mpv|mpgv|mp2|m4b|aiff?|amr|ra|w64)$");
re!(PAYLOAD, r"(?i)\.(?:mkv|avi|mp4|m4v|wmv|mpg|mpeg|mov|ts|m2ts|vob|divx|flv|webm|ogv|3gp|asf|rm|rmvb|f4v|mp3|flac|m4a|aac|ogg|wav|wma|ape|opus|mka|ac3|dts|eac3|truehd|mpc|shn|tak|tta|wv|jpg|jpeg|png|gif|bmp|tiff?|webp|pdf|txt|diz|md5|sha1|cue|log|epub|mobi|azw3?|djvu|cbr|cbz|fb2|lit|prc|opf|iso|bin|mdf|mds|nrg|img|ccd|sub|exe|msi|dmg|pkg|apk|xap|appx|deb|rpm|srt|idx|ass|ssa|vtt|sup|docx?|rtf|xlsx?|pptx?|odt|m3u8?|ifo|mks|ogm)$");
re!(SAMPLE_TAIL, r"(?i)[.\-_](?:sample|proof|subs?|thumbs?|cover|screens?)[.\-_]?$");
re!(SEGMENT_TAIL, r"(?i)[.\-_]?(?:part|vol|cd|dvd|dis[ck])\d+$");
re!(PREFIX, r"(?i)^(?:Release Name\s*:?\s*|sample[-_])");
re!(COLLISION_SUFFIX, r" \[\d{8}\]$");   // Part 2's ` [12345678]` on a taken name
re!(NZBSPLIT, r"(?i)__NZBSPLIT__[a-f0-9]{8,}__NZBSPLIT__(.+)$");
re!(SEPARATORS, r"[._\s]+");
re!(ROT13_YENC, r"(?i)\blRap\b");

pub fn basename(path: &str) -> &str { path.rsplit(['/', '\\']).next().unwrap_or(path) }
/// wrappers repeatedly, then one payload extension; a codec word after a wrapper is part of the title
pub fn strip_file_suffixes(name: &str) -> String {
    let mut s = name.to_string();
    loop { let n = WRAPPER.replace(&s, "").into_owned(); if n == s { break; } s = n; }
    if s != name && CODEC_AFTER_WRAPPER.is_match(&s) { return s; }
    PAYLOAD.replace(&s, "").into_owned()
}
/// The release name in a raw candidate: no path, no control characters, no file suffix, no sample or part tail.
pub fn clean_candidate(raw: &str) -> Option<String>;   // order: drop controls, trim and strip quotes, COLLISION_SUFFIX, basename,
    // NZBSPLIT payload (`_` to `.`, `+` to space), strip_file_suffixes, SAMPLE_TAIL, SEGMENT_TAIL, PREFIX,
    // collapse runs of whitespace, trim ` .-_"'\t`. Empty is None.
/// `Title-GRP/CD1/file.rar` gives `Title-GRP`. Directories named cd*, disc*, disk*, dvd* (up to 8 characters),
/// subs, sub, sample, proof, video_ts, audio_ts are skipped.
pub fn parent_dir(path: &str) -> Option<String>;
/// The same release, whatever the case and whichever of `.`, `_` and space separates its words.
pub fn identity_key(s: &str) -> String { SEPARATORS.replace_all(s.trim().to_lowercase().as_str(), ".").into_owned() }
pub fn has_collision_suffix(name: &str) -> bool { COLLISION_SUFFIX.is_match(name) }
/// `name` without Part 2's collision suffix, which isnt part of what the name says
pub fn without_collision_suffix(name: &str) -> &str { COLLISION_SUFFIX.find(name).map_or(name, |m| &name[..m.start()]) }
pub fn rot13_subject(subject: &str) -> bool { ROT13_YENC.is_match(subject) }
```

### Provided (`src/naming/regexes.rs`)

```rust
const DATA: &str = include_str!("../../data/naming_regexes.tsv");
const MAX_SUBJECT: usize = 1024;
/// id, group index, case flag and pattern text. Parsed once; group regexes (118) compiled once, case-insensitive.
static TABLE: LazyLock<Table>;
type Cell = Arc<OnceLock<Vec<Rule>>>;
/// A cell per group name seen. The map lock is held only to get the cell; a group compiles inside its own cell,
/// so a slow first compile holds up only threads asking for that group.
static BY_GROUP: LazyLock<Mutex<HashMap<String, Cell>>>;
pub struct Rule { pub id: u32, re: Regex, names: Vec<String> }  // names: capture names, sorted, without reqid or parts
impl Rule { pub fn name_from(&self, subject: &str) -> Option<String>; }   // joined captures; empty is None
pub fn rules_for(group: &str) -> Cell;
/// Each rule's output in order: rule order is `ordinal, group_regex, id` from the file. A subject over MAX_SUBJECT gives none.
/// `subject` is `collections::subject_base`'s output (Part 2): the form upstream's rules were written for.
pub fn names(group: &str, subject: &str) -> Vec<String>;
/// Whether any rule's group regex matches. Compiles no rule, and is remembered per group name.
pub fn group_has_rules(group: &str) -> bool;
#[doc(hidden)] pub static COMPILED: AtomicUsize;  // rules compiled so far, for the laziness test
#[doc(hidden)] pub fn rule_by_id(id: u32) -> Option<Rule>;  // compiles one, for the sample test
```

### Provided (`src/naming/decide.rs`)

Pure, so every rule has a table test with no database.

```rust
pub fn current_name(r: &ReleaseInfo) -> &str { r.display_name.as_deref().unwrap_or(&r.name) }
pub fn effective_source(r: &ReleaseInfo) -> i64 {
    match (r.name_source, &r.display_name) {
        (Some(s), _) => s,
        (None, Some(d)) if judge(d) >= Grade::Weak => NameSource::InlineBody as i64,
        _ => NameSource::Ingest as i64,
    }
}
/// What the current name is worth: what it would clean to counts, so `Title-GRP.mkv` is as good as `Title-GRP`.
pub fn grade_now(current: &str) -> Grade {
    let g = judge(current);
    clean_candidate(current).map_or(g, |c| g.max(judge(&c)))
}
/// The whole precedence rule, for every source but PreDB. A candidate needs a source at least as good and a strictly
/// better grade. So a well-named post keeps its name whatever an inner file says, and a second run changes nothing.
/// The one exception is a candidate that is the current name cleaned. That is a tidy, not new evidence: it is allowed for
/// a `display_name` (Part 4 or the inline path left a suffix) or Part 2's collision suffix, and never for a posted `name`
/// whose only flaw is its extension.
pub fn improves(r: &ReleaseInfo, candidate: &str, source: NameSource) -> bool {
    let (grade, current) = (judge(candidate), current_name(r));
    if grade == Grade::Junk || (source as i64) < effective_source(r) || renames_pack_to_episode(current, candidate) { return false; }
    if identity_key(candidate) == identity_key(current) { return false; }
    if clean_candidate(current).is_some_and(|c| identity_key(&c) == identity_key(candidate)) {
        return r.display_name.is_some() || has_collision_suffix(current);
    }
    grade > grade_now(current)
}
static IMAGE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\.(jpe?g|png|gif|webp|bmp|tiff?)$").unwrap());
pub fn is_image_name(name: &str) -> bool { IMAGE_NAME.is_match(name) }
/// `Naming::applies`: could a run improve this name? No I/O. See "What `naming` skips".
pub fn worth_running(r: &ReleaseInfo) -> bool {
    if r.name_source.is_some_and(|s| s >= NameSource::PreDb as i64) { return false; }
    let current = current_name(r);
    if has_collision_suffix(current) { return true; }
    if grade_now(current) == Grade::Scene && clean_candidate(current).as_deref() == Some(current) { return false; }
    let rules = regexes::group_has_rules(&r.group);
    if r.parts <= 1 && !rules { return false; }
    !(is_image_name(&r.name) && (r.parts <= 1 || !rules))
}
fn ignorable(path: &str) -> bool;  // lowercase basename ends .url .lnk .jpg .jpeg .png .gif .bmp .webp .tif .tiff .txt .sfv .md5 .db,
                                   // or contains sample, proof, password, passwort, or starts with www.
fn rank(path: &str) -> u8;         // 0 .rar without .part, or .part01/.part1/.part001.rar; 1 video, iso, img, epub, mobi, pdf, flac, mp3;
                                   // 2 .nfo .7z .zip, other .part or .r0; 3 the rest
const SUBJECTS_TRIED: usize = 4; const FILES_TRIED: usize = 256;
/// Names in these (path, weight) pairs, best first: grade, summed weight, file rank, longer, lower. Each path offers
/// clean_candidate and clean_candidate(parent_dir). With 2 or more distinct episodes, episode names are left out.
pub fn best_from_paths<'a>(paths: impl Iterator<Item = (&'a str, u64)>, min: Grade) -> Vec<String>;
pub fn decide(r: &ReleaseInfo, files: &[FileInfo], inner: &[InnerFileRow], passworded: bool) -> Option<NameChange> {
    let mut best: Option<(Grade, NameSource, String)> = None;
    let mut offer = |name: Option<String>, source: NameSource| -> bool {
        let Some(name) = name else { return false };
        let grade = judge(&name);
        if best.as_ref().is_none_or(|(g, s, _)| (grade, source) > (*g, *s)) { best = Some((grade, source, name)); }
        grade == Grade::Scene
    };
    let pick = |source: NameSource, min: Grade, kinds: &dyn Fn(i64) -> bool| {
        let paths = inner.iter().filter(|f| kinds(f.source)).map(|f| (f.name.as_str(), f.size.unwrap_or(1).max(1) as u64));
        best_from_paths(paths, min).into_iter().find(|n| improves(r, n, source))
    };
    let archive_min = if passworded { Grade::Good } else { Grade::Weak };
    let done = offer(pick(NameSource::Par2, Grade::Weak, &|s| s == archive::store::SOURCE_PAR2), NameSource::Par2)
        || offer(pick(NameSource::ArchiveInner, archive_min, &|s| s != archive::store::SOURCE_PAR2), NameSource::ArchiveInner);
    if !done && grade_now(current_name(r)) != Grade::Scene {
        let from_subject = subjects(files).into_iter().find_map(|subject| {
            regexes::names(&r.group, &collections::subject_base(subject)).iter()
                .filter_map(|n| clean_candidate(n)).find(|n| judge(n) >= Grade::Good && improves(r, n, NameSource::NamingRegex))
        });
        if !offer(from_subject, NameSource::NamingRegex) {
            let posted = files.iter().filter(|f| f.subject.as_deref().is_none_or(|s| !rot13_subject(s)))
                .map(|f| (f.filename.as_str(), f.parts_seen.max(1) as u64));
            offer(best_from_paths(posted, Grade::Good).into_iter().find(|n| improves(r, n, NameSource::PostedFile)), NameSource::PostedFile);
        }
    }
    best.map(|(_, source, name)| NameChange { display_name: Some(name), obfuscated: Some(false), source: source.value() })
}
/// Up to SUBJECTS_TRIED distinct subjects, lowest file rank first, leaving out scrambled ones.
fn subjects(files: &[FileInfo]) -> Vec<&str>;
```

### Provided (`src/nfo.rs`)

```rust
re!(SOURCE_FIELD, r"(?i)^[ \t]*Source[ \t]*:");
re!(CONTINUATION, r"^[ \t]+\S");
re!(FIELD, r"^[ \t]*[A-Za-z][A-Za-z0-9 _-]*[ \t]*:");
re!(WIN_PATH, r"^[ \t]*[A-Za-z]:[\\/]");
re!(LITERAL_FIELD, r"(?im)^[ \t]*(?:Filename|File[ \t]+Name|Complete[ \t]+Name|Release(?:[ \t]+Name)?)[ \t]*[:=][ \t]*([^\r\n]+)$");
re!(SCENE_TOKEN, r"[\p{L}\p{N}][\p{L}\p{N}\p{M}._-]{6,}-[A-Za-z0-9]{2,15}");
re!(TV_LINE, r"(?i):[ \t]+([^\r\n]*?S\d{1,3}[._ -]?[ED]\d{1,3}[^\r\n]*?)(?:[ \t]{2,}|\r|\n|$)");
re!(MOVIE_LINE, r"(?i):[ \t]+([^\r\n]*?(?:19|20)\d\d[^\r\n]*?(?:BDRip|bluray|DVD(?:R|Rip)?|XVID|WEB-?DL|1080p|720p)[^\r\n]*?)(?:[ \t]{2,}|\r|\n|$)");
re!(TITLE_YEAR, r"(?m)^[ \t]*([\p{L}\p{N}][\p{L}\p{N} .,'&:-]{2,80}\s?\((?:19|20)\d\d\))");
const NFO_MAX_TEXT: usize = 262_144; const MAX_TOKENS: usize = 100;

/// A Source field and its indented continuation lines become blank lines.
pub fn identity_text(text: &str) -> String;
/// Best first, deduped, each passing valid_name. Empty for an `=newz[NZB]=` NFO. Only the first NFO_MAX_TEXT bytes are read.
pub fn name_candidates(data: &[u8]) -> Vec<String>;
pub fn display_name(data: &[u8]) -> Option<String> { name_candidates(data).into_iter().next() }
```

### Provided (`src/naming/steps.rs`)

```rust
pub struct Naming;
impl Step for Naming {
    fn id(&self) -> StepId { NAMING }
    fn name(&self) -> &'static str { "naming" }
    fn rank(&self) -> u16 { 200 }
    fn needs(&self) -> Needs { Needs::Database }
    fn enabled(&self, cfg: &Config) -> bool { cfg.naming.enabled }
    fn applies(&self, r: &ReleaseInfo) -> bool { worth_running(r) }
    fn reruns(&self) -> Reruns { Reruns { on_rename: false, on_growth: true } }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let retry = |e: rusqlite::Error| StepError::Retry(e.to_string());
        let id = ctx.release().id;
        let inner = archive::store::inner_files(ctx.conn(), id).map_err(retry)?;
        // New inner files reach PreDB matching too, renamed or not. Bit 16 is Part 6's step: unregistered bits are ignored.
        let rerun = if inner.is_empty() { 0 } else { mask(StepId(16)) };
        // An inline name is left alone until the user opts in, see "Who gets renamed"
        let r = ctx.release();
        if r.display_name.is_some() && r.name_source.is_none() && !rename_named_is_on(ctx.conn()).map_err(retry)? {
            return Ok(Outcome { rerun, ..Outcome::default() });
        }
        let files = ctx.files().map_err(retry)?;
        let passworded = archive::store::password(ctx.conn(), id).map_err(retry)? == Some(Password::Yes);
        let name = decide(ctx.release(), &files, &inner, passworded);
        Ok(Outcome { matched: name.is_some(), name, rerun, ..Outcome::default() })
    }
}

/// `select value from meta where key = 'naming_rename_named'`: a row means opted in.
fn rename_named_is_on(conn: &Connection) -> rusqlite::Result<bool>;

pub struct NfoName;
impl Step for NfoName {
    fn id(&self) -> StepId { NFO }
    fn name(&self) -> &'static str { "nfo" }
    fn rank(&self) -> u16 { 210 }
    fn needs(&self) -> Needs { Needs::Bodies }
    // not tied to naming.enabled: with a budget, `save_slice` no longer reads NFOs (Part 4), so this is the only reader
    fn enabled(&self, cfg: &Config) -> bool { cfg.archive.enabled && cfg.naming.nfo_step }
    fn applies(&self, r: &ReleaseInfo) -> bool { effective_source(r) <= NameSource::Nfo as i64 && judge(current_name(r)) < Grade::Good }
    fn reruns(&self) -> Reruns { Reruns { on_rename: false, on_growth: true } }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError>;
}
```

`NfoName::run`: `archive::select::nfo_sources(&files)` (at most 2). For each, `ctx.segments(file.id, &SegmentPick::First(1))`; a row with `part > 1` means the first article was never indexed, so skip it. `fetch_bodies` with `max_bytes: Some(archive::ARTICLE_CAP)` and `file` set to the filename. `Got::Missing`, `TooBig` and `Refused` skip the candidate, and a `StepError` from `fetch_bodies` propagates. The first of `nfo::name_candidates(&body)` for which `improves(release, name, NameSource::Nfo)` is the result, with `obfuscated: Some(false)`. No NFO file: done with no fetch.

### Provided (`src/naming/reflag.rs`)

```rust
/// `atlas --reflag-obfuscated [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed.
pub fn run(args: &[String]) -> i32;
```

### Provided (`src/naming/unname.rs`)

```rust
/// `atlas --unname [--sample N] [--apply]`. Exit 0 on success, 1 when refused or failed. Shares its batched
/// per-shard walk, its refusals and its WAL rule with `reflag.rs`.
pub fn run(args: &[String]) -> i32;
```

### Consumed

`Step`, `StepCtx`, `Outcome`, `NameChange`, `ReleaseInfo`, `store::FileInfo`, `store::SegmentPick`, `Got`, `StepError`, `Needs`, `Reruns`, `StepId`, `mask` (Part 3). `archive::store::{inner_files, password, InnerFileRow, SOURCE_PAR2}`, `archive::select::nfo_sources`, `archive::{Password, ARTICLE_CAP, NAME_SOURCE_*}` (Part 4). `collections::subject_base` (Part 2). `compact::Lock`, `compact::refuse_unresolved_backups`, `convert::needed`, `procs::indexer_alive`, `db::{open_shard, tune_for_writing}`, `store::{shard_paths, set_meta}`. `archive::select` must be a public module, so Part 4 declares `pub mod select` and `nfo_sources` returns `Vec<&FileInfo>`; adapt to what merged.

The table names differ from the ones first pinned for this work (`release_inner_files`, `release_archive`), because Part 4 used Part 3's `archive_` prefix. This plan reads only through Part 4's functions.

### What Parts 6, 7 and 8 can rely on

- A rename clears the done bit of every higher-ranked step whose `reruns()` has `on_rename: true`, and in the same run `ctx.release()` shows the new name. Nothing else is needed to learn of a rename.
- `ctx.release().name_source` says how far to trust the name, with values above.
- `naming::judge`, `is_season_pack` and `is_episode_name` are public.
- `display_name` holds no path and no file suffix when `naming` or `nfo` set it, and `naming` tidies one Part 4 left at an equal source. A name with NULL `name_source` from the inline path may still carry `.mkv`.
- `naming::improves` is the one rule for proposing a name. Only Part 6's PreDB match goes around it.
- `atlas --unname` changes names without going through a step. It re-queues every higher-ranked step with `on_rename`, so they see the release again as they would after a rename.

## 7. Config

A top-level `naming` object, parsed into `Config.naming: NamingConfig`. Absent keys take these defaults, no key is written back, and a bad value falls back as `from_value` does elsewhere (`src/config.rs:631`).

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Run the `naming` step. Database work only, no network |
| `nfo_step` | `true` | Run `nfo`, while `archive.enabled` is also true. It fetches nothing while `postprocess.body_fetches_per_minute` is 0 |

Part 3 starts a walk when a step turns on, so upgrading starts one over the releases that pass the rules. It names only rows with no `display_name` ("Who gets renamed") and needs no network. Set `naming.enabled: false` before restarting to hold it back. Replacing existing names needs `atlas --reprocess --step naming --apply`, not a config key.

## 8. Tasks

Each task: write the test first and see it fail for the stated reason, implement, run, commit. Append whatever attribution the executing session requires to commit messages. All tests use `tempfile::tempdir()`. No test may read a release name from a live database. Every fixture name here is invented, including the look-alikes of the shapes this part targets.

### Task 1: The obfuscation check

**Files:** create `src/naming/mod.rs`, `src/naming/obfuscation.rs`; modify `src/lib.rs`, `src/parser.rs:77-81` and its test at `:327`.

- [ ] **Test first**, in `obfuscation.rs`:

```rust
#[test]
fn flags_random_shapes() {
    for n in ["a1B2c3D4e5F6g7H8", "a1B2c3D4e5F6g7H8i9J0", "kP93xQ7rL.ZMtwBfa", "hQ72vB9nK.XWdrLpz", "Xk3Mz9PqL", "nQzw7RtKjd",
        "qT4mVx8LpW", "pK2xQ9mZ", "550e8400-e29b-41d4-a716-446655440000", "d41d8cd98f00b204e9800998ecf8427e",
        "D41D8CD98F00B204E9800998ECF8427E", "da39a3ee5e6b4b0d3255bfef95601890afd80709", "aB3dE-fG7hJ-kL9mN", "x7Kp2QmZ-t9Rw4LsV",
        "Gq7YGEWLy8wAA2NhbZx5LukEa", "abcd1234efgh5678", "zXcVbNmAsDfGhJkL", "kjhgfdsapoiuytre", "QWERTYUIOPASDFGH", "qzxkjwvbnm",
        "xkcdqwrtplmn", "Qw3Er5Ty7.mkv", "nMhb2KxQd7.rar.bin", "tZ9wL2xV_pR5nK8q"] {
        assert!(is_obfuscated(n), "{n}");
    }
}

#[test]
fn leaves_real_names_alone() {
    for n in ["Movie.2024.1080p.WEB", "Some.Show.S01E01.720p.HDTV.x264-GRP", "Avatar", "Inception", "Dune", "Se7en", "Thor2", "U2", "Blink182",
        "MacOS", "iPhone15", "WinRAR", "XMen2", "LaLaLand", "Dune2021", "GTA5", "Fallout4", "McDonald", "YouTube", "PlayStation5", "eBay", "JavaScript",
        "SpongeBob", "StarCraft2", "NaNoWriMo", "Windows10", "Office2019", "Photoshop", "Transformers", "Schwarzenegger", "Strengths", "Rammstein",
        "Metallica", "Kraftwerk", "Motorhead", "Bjork", "Xzibit", "Minecraft", "Cyberpunk2077", "Battlefield1942", "Spider-Man", "Wu-Tang-Clan",
        "Foo-Bar-GRP", "The.Matrix", "the_matrix_1999", "abc def", "broken upload", "Some.Thing", "Rel", "a [4]", "no part", "file.name.mkv", "weird.bin",
        "just.a.file", "ab.0001", "Ünïcödé.Fïlé.mkv", "Тестовый.Релиз", "Die.Hard.4.0", "Ocean11", "Apollo13", "Area51", "R2D2", "C3PO", "1984", "20240115",
        "Weihnachtsmann", "Schlafzimmer", "Fruehstueck", "Bibliotheque", "Chevalier", "Manana", "Corazon", "Przygoda", "Warszawa", "Kobenhavn", "Yokohama",
        "Shinjuku", "Dragonball", "Evangelion", "Adobe.Photoshop.2024.v25.0-GRP", "Microsoft.Office.2019.x64", "VA-Best_Of_Rock-2CD-2019-GRP",
        "Artist-Album-WEB-2024-GRP", "Game.Name-CODEX", "Foo.Bar.2024.1080p.WEB-DL.DDP5.1.H.264-x7Kp2QmZ", "TheMatrix", "MyFavoriteMovie", "GameOfThrones",
        "01-track_name", "Track01", "CD1", "IMG_1234", "DSC01234", "P1010123"] {
        assert!(!is_obfuscated(n), "{n}");
    }
}

/// decision 6
#[test]
fn everything_the_old_rule_flagged_stays_flagged() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let alpha = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for i in 0..20_000 {
        seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
        let s: String = (0..16 + i % 20).map(|k| alpha[(seed >> (k % 40)) as usize % 62] as char).collect();
        assert!(is_obfuscated(&s), "{s}");
    }
    assert!(is_obfuscated("TheMatrixReloaded"), "17 letters, as today");
}

#[test]
fn most_random_short_names_are_caught() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let alpha = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut next = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; alpha[seed as usize % 62] as char };
    let hits = (0..20_000).filter(|_| { let s: String = (0..10).map(|_| next()).collect(); is_obfuscated(&s) }).count();
    assert!(hits >= 18_000, "{hits} of 20000");
}
```

- [ ] **Run:** `cargo test --lib obfuscation`. **Expected before:** `flags_random_shapes` panics at `kP93xQ7rL.ZMtwBfa`, because the old regex misses it.
- [ ] **Implement** section 6, and `src/parser.rs:77-81` and `:327` as in section 4. Do not change `tests/fixtures/subjects.python.jsonl`: its one flagged name stays flagged and nothing else in it changes, so `tests/parser_parity.rs` still passes.
- [ ] **Run:** `cargo test --lib obfuscation && cargo test --test parser_parity && just test`. **Expected:** pass.
- [ ] **Commit:** `Catch dotted, hyphenated and short random names in the obfuscation check`

### Task 2: The judge, cleaning and name sources

**Files:** create `src/naming/judge.rs`, `src/naming/clean.rs`; modify `src/naming/mod.rs`.

- [ ] **Tests first:**
  - `judge_table`: `Scene`: `Some.Show.S01E01.720p.HDTV.x264-GRP`, `Movie.2024.1080p.WEB-DL.DDP5.1.H.264-GRP`, `Artist-Album-WEB-2024-FLAC-GRP`. `Good`: `grp-sometitle-1080p`, `VA-Best_Of_Rock-2CD-2019-GRP`, `Game.Name-CODEX`, `Book.Title.2020.RETAIL.EPUB-GRP`, `Cool Movie 2024 1080p`, `Some Show S01E01`, `Some.Show.S02.Complete`, `Cool.Movie.2024`. `Weak`: `Real Name`, `Some Book Title`. `Junk`: `a1B2c3D4e5F6g7H8`, `kP93xQ7rL.ZMtwBfa`, `Xk3Mz9PqL`, `Inception`, `sample`, `setup.exe`, `setup`, `track01`, `VTS_01_1`, `Foo.Bar.part01`, `Foo.Bar.vol03+04`, `abc def`, `123`, `---`, `a/b/c.mkv`, empty.
  - `clean_table`: `Some.Movie.2024.1080p.x264-GRP.mkv` and `.part01.rar` give `Some.Movie.2024.1080p.x264-GRP`; `Some.Movie-GRP.vol03+04.par2` and `.7z.003` give `Some.Movie-GRP`; `Movie.2020.1080p.DTS.rar` and `.mkv` give `Movie.2020.1080p.DTS`; `dir\sub/Some.Show.S01E01-GRP-sample.mkv` gives `Some.Show.S01E01-GRP`; `Release Name: Foo.Bar-GRP` gives `Foo.Bar-GRP`; `__NZBSPLIT__0123abcd__NZBSPLIT__Foo_Bar_2024+x264-GRP.rar` gives `Foo.Bar.2024 x264-GRP`; `Foo.Bar.CD1.rar` gives `Foo.Bar`; `  spaced   out   name.mp4  ` gives `spaced out name`; `"quoted.name.nfo"` gives `quoted.name`; `Title-GRP [12345678]` gives `Title-GRP`; `T.mkv [12345678]` gives `T`; `Movie.Name [2019]` is unchanged, because only Part 2's 8 digits are stripped.
  - `parent_table`: `A.Movie-GRP/grp-a.mkv` and `A.Movie-GRP\CD1\a.rar` give `A.Movie-GRP`; `a.mkv` and `Sample/a.mkv` give none.
  - `season_rules`: `renames_pack_to_episode("Show.S02.1080p.WEB-GRP", "Show.S02E03.1080p.WEB-GRP")` true; from an episode, or from a random name, false. `distinct_episodes` of `a.S01E01.x`, `a.S01E02.x`, `a.S01E01.y` is 2.
  - `values_match_part_4`: `NameSource::Nfo.value() == archive::NAME_SOURCE_NFO`, the same for `ArchiveInner` and `Par2`. `all_values_fit_part_3`: every written value is in `1..=127`.
- [ ] **Run:** `cargo test --lib naming`. **Expected before:** does not compile: no module.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib naming && just lint`. **Expected:** pass.
- [ ] **Commit:** `Add a judgement of release names, name cleaning and the source order`

### Task 3: Naming regexes

**Files:** create `scripts/naming_regexes_to_tsv.py`, `data/naming_regexes.tsv`, `tests/fixtures/naming_samples.tsv`, `src/naming/regexes.rs`; test `tests/naming_regexes.rs`, one test per concern. The binary must not share `COMPILED` with a test that touches other groups, so `rules_compile_lazily_by_group` is a separate binary, `tests/naming_regexes_lazy.rs`.

The script, standard library only:

```python
#!/usr/bin/env python3
"""newznab-tmux ReleaseNamingRegexesTableSeeder.php -> data/naming_regexes.tsv and tests/fixtures/naming_samples.tsv.

Usage: scripts/naming_regexes_to_tsv.py SEEDER.php OUT.tsv SAMPLES.tsv
Source: newznab-tmux at f39acb5ed7fe90a77ab01b7f92e96b473ffe1628, GPLv3. Python 3.11 or newer.
"""
import re
import sys
import warnings

warnings.simplefilter("ignore", FutureWarning)  # re warns about the "[[" PCRE takes literally
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


def sample_of(description, py):
    """(subject, expected) from the examples in a seeder description, by RegexService::_matchRegex's rule."""
    text = re.sub(r"^//\s*", "", description.split(":::")[-1].strip())
    for subject in re.split(r"\s+or\s+|\s+//\s*", text):
        m = py.search(subject.strip()) if "\t" not in subject else None
        if m:
            name = "".join(v or "" for k, v in sorted(m.groupdict().items()) if not re.search("reqid|parts", k, re.I))
            if name:
                return subject.strip(), name
    return None


def main(seeder, tsv, samples):
    rows, sample_rows, stats = [], [], {}
    for m in ROW.finditer(open(seeder, encoding="utf-8").read()):
        rid, group, raw, status, desc, ordinal = int(m[1]), unphp(m[2]), unphp(m[3]), int(m[4]), unphp(m[5]), int(m[6])
        if status != 1:
            stats["disabled"] = stats.get("disabled", 0) + 1
            continue
        try:
            re.compile(group)
        except re.error:  # rule 1131 has an unbalanced ")" in its group regex and can never apply
            stats["bad group regex"] = stats.get("bad group regex", 0) + 1
            continue
        body, flags = split_delimiters(raw)
        pattern, changes = convert(body)
        for c in changes:
            stats[c] = stats.get(c, 0) + 1
        if any(ch in pattern + group for ch in "\t\r\n"):
            raise ValueError(f"tab or newline in rule {rid}")
        rows.append((ordinal, group, rid, "i" if "i" in flags else "", pattern))
        try:
            py = re.compile(body.replace("\\E", ""), (re.I if "i" in flags else 0) | (0 if "u" in flags else re.A))
        except re.error:
            continue
        found = sample_of(desc, py)
        if found:
            sample_rows.append((rid, *found))
    rows.sort()
    with open(tsv, "w", encoding="utf-8", newline="\n") as f:
        f.write("# generated by scripts/naming_regexes_to_tsv.py, do not edit\n")
        f.write("# newznab-tmux database/seeders/ReleaseNamingRegexesTableSeeder.php at f39acb5, GPLv3\n")
        f.write("# id\tordinal\tgroup_regex\tflags\tpattern\n")
        f.writelines(f"{r}\t{o}\t{g}\t{fl}\t{p}\n" for o, g, r, fl, p in rows)
    with open(samples, "w", encoding="utf-8", newline="\n") as f:
        f.write("# generated by scripts/naming_regexes_to_tsv.py from the seeder's own descriptions\n")
        f.write("# id\tsubject\texpected name\n")
        f.writelines(f"{r}\t{s}\t{n}\n" for r, s, n in sorted(sample_rows))
    print(f"{len(rows)} rules, {len(sample_rows)} samples, {dict(sorted(stats.items()))}")


if __name__ == "__main__":
    main(*sys.argv[1:])
```

- [ ] **Run** the script, on the reference clone from `00-overview.md`. It only reads the seeder as text:

```bash
python3 scripts/naming_regexes_to_tsv.py "$SRC/database/seeders/ReleaseNamingRegexesTableSeeder.php" data/naming_regexes.tsv tests/fixtures/naming_samples.tsv
```

**Expected:** `1154 rules, 1045 samples, {'bad group regex': 1, 'disabled': 2, 'dropped \\E': 1, 'escaped [ in a class': 5, 'escaped leading ] in a class': 2, 'escaped literal {': 2, 'possessive quantifier made greedy': 4}`. Commit both outputs. A different count means the seeder or the script changed; fix that before going on.

- [ ] **Tests first:**
  - `every_rule_compiles`: all 1,154 lines through `rule_by_id`, and the 118 group regexes.
  - `the_seeders_own_samples_agree`: for each line of `naming_samples.tsv`, `rule_by_id(id).name_from(subject) == expected`. 1,045 lines, no exceptions.
  - `possessive_became_greedy`: rule 1157 on `[AFL 2017 Round 8 NMFC vs SYD 720p] - "AFL 2017 Round 8 NMFC vs SYD 720p par2" yEnc` in `alt.binaries.multimedia.sports` gives `AFL 2017 Round 8 NMFC vs SYD 720p`, and it is the sample for that rule.
  - `rules_come_in_file_order`: for `alt.binaries.teevee`, `rules_for` ids follow ascending `(ordinal, id)` from the TSV.
  - `names_run_on_part_2s_subject_base`: `names("alt.binaries.boneless", &subject_base(r#"Brought to you by Uploader [01/35] - "x.rar" yEnc (1/50)"#))` contains `Brought to you by Uploader`. Without the base, rule 132's `yEnc$` can't match.
  - `a_long_subject_is_skipped`: 2,000 bytes gives no names.
  - `group_has_rules_table`: `alt.binaries.teevee` and `alt.binaries.test` (3 rules upstream) yes, `ALT.BINARIES.TEEVEE` yes, `alt.binaries.no.rules` no.
  - `rules_compile_lazily_by_group` (own binary): `names("alt.binaries.no.rules", "x")` and `group_has_rules("alt.binaries.teevee")` leave `COMPILED` at 0. `names("alt.binaries.teevee", "x")` makes it 89. Calling again leaves it 89.
- [ ] **Implement** section 6. A rule whose pattern fails to build at run time is skipped with `eprintln!` once; `every_rule_compiles` is what keeps that from happening.
- [ ] **Run:** `cargo test --test naming_regexes && cargo test --test naming_regexes_lazy`. **Expected:** pass.
- [ ] **Commit:** `Convert upstream's naming regexes and compile them per group`

### Task 4: `decide`

**Files:** create `src/naming/decide.rs`; tests in it.

- [ ] **Tests first**, table style, with `rel(name)` building a `ReleaseInfo` in `alt.binaries.no.rules` and `Xk3Mz9PqL` as the junk name:
  - `a_regex_name_replaces_junk`: group `alt.binaries.teevee`, subject `[#####]-[FULL]-[#a.b.teevee@EFNet]-[Batman.The.Animated.Series.S04E01.DVDRiP.XviD-PyRo]-[00/35] "Xk3Mz9PqL.nfo" yEnc (1/1)`: `NamingRegex`, that name.
  - `inner_names_beat_a_regex`: the same plus inner `Real.Movie.2024.1080p.WEB-DL.x264-GRP/grp-real.mkv`, source 2: that directory, `ArchiveInner`.
  - `a_worse_source_never_overwrites`: `Other Film Title` at `Nfo`: the regex gives `None`. The same name at `Par2`: inner `Scene` gives `None`.
  - `a_better_source_and_grade_replaces`: `Other Film Title` at `NamingRegex`: inner `Scene` replaces it.
  - `an_equal_grade_never_replaces`: a different `Scene` name at `NamingRegex`, at `ArchiveInner` and at ingest: inner `Scene` gives `None`.
  - `a_well_named_post_is_not_renamed`: `Some Film 2024 1080p` as the posted name: inner `grp-sometitle-1080p.mkv` gives `None`. With `Xk3Mz9PqL` as the posted name, it is taken.
  - `a_higher_source_with_a_worse_grade_does_not_replace`: `Scene` at `NamingRegex`: PAR2 `Some Book Title.pdf` gives `None`.
  - `grade_comes_first_within_a_run`: PAR2 `Some Book Title.pdf` and inner `Real.Movie.2024.1080p.WEB-DL.x264-GRP.mkv`: the inner one.
  - `predb_is_never_replaced`: `name_source` 100: always `None`.
  - `a_good_inline_name_counts_as_a_body_source`: `display_name` `Inline Film 2020 720p`, NULL source: the regex gives `None`, inner `Scene` replaces it.
  - `a_junk_inline_name_is_replaced`: `display_name` `aB3dE9Xk.mkv`, NULL source: the regex replaces it.
  - `a_file_suffix_is_tidied`: `display_name` `Real.Movie.2024.mkv` at `Par2` with that PAR2 row: `Real.Movie.2024`, and then `None`.
  - `a_posted_suffix_is_not_a_rename`: `name` `Hidden.Show.S01E01.mkv`, `display_name` NULL, with that posted file: `None`. The same for `Some.Movie.2024.1080p.WEB-DL.x264-GRP.mkv`, whose cleaned form is `Scene`.
  - `a_collision_suffix_is_tidied`: `name` `Some.Show.S01E02 [12345678]`, `display_name` NULL, posted file `Some.Show.S01E02.mkv`: `Some.Show.S01E02`, `PostedFile`, and then `None`.
  - `an_ad_prefix_is_not_a_name`: group `alt.binaries.boneless`, subject `Brought to you by Uploader [01/35] - "kP93xQ7rL.ZMtwBfa.part01.rar" yEnc (1/50)`: rule 132 gives that text, it is `Weak`, so `None`.
  - `a_weak_posted_name_is_not_taken`: posted `Some Book Title.pdf` on a junk name: `None`. The same name from an inner file is taken, see below.
  - `a_second_run_changes_nothing`: feed `decide`'s result back into the release: `None`, for each table row above.
  - `a_season_pack_keeps_its_name`: `Show.Name.S02.1080p.WEB-DL.x264-GRP` with an inner `S02E03` file: `None`.
  - `many_episodes_name_nothing`: `Xk3Mz9PqL` with `S02E03` and `S02E04` inner: `None`.
  - `samples_and_junk_are_ignored`: `…-GRP-sample.mkv`, `abc123Xy9.mkv`: `None`.
  - `passworded_needs_a_good_name`: inner `Some Book Title.pdf`, `Weak`: taken, unless passworded.
  - `sizes_pick_the_set`: PAR2 `Rel.Name.2024.x264-GRP.part01.rar` size 900 and `grp-rn.nfo` size 5: `Rel.Name.2024.x264-GRP`, `Par2`.
  - `separators_are_not_a_rename`: `Some.Movie.2024.1080p.WEB-DL.x264-GRP` with a matching posted file: `None`.
  - `scrambled_subjects_are_skipped`: subject ending `lRap (1/2)`: `None`.
  - `worth_running_table`, group `alt.binaries.no.rules` unless stated. False: `name_source` 100; `Some.Movie.2024.1080p.WEB-DL.x264-GRP` with 50 parts; `Xk3Mz9PqL` with 1 part; `DSC_0001.jpg` with 1 part, also in `alt.binaries.teevee`; `DSC_0001.jpg` with 7 parts. True: `Xk3Mz9PqL` with 50 parts; `Xk3Mz9PqL` with 1 part in `alt.binaries.teevee`; `DSC_0001.jpg` with 7 parts in `alt.binaries.teevee`; `Some.Movie.2024.1080p.WEB-DL.x264-GRP.mkv` with 50 parts; `Some.Show.S01E02 [12345678]` with 1 part.
- [ ] **Run:** `cargo test --lib decide`. **Expected before:** does not compile.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib decide`. **Expected:** pass.
- [ ] **Commit:** `Decide a release's best name, and never let a worse source win`

### Task 5: NFO text

**Files:** `src/nfo.rs`.

- [ ] **Tests first:** the five assertions at `:65-74` stay, and:
  - `a_source_field_is_not_the_release`: `Source : Other.Movie.2019.1080p.BluRay.x264-OTHER\n  Other.Movie.2019.720p.BluRay.x264-OTHER\nRelease: Real.Movie.2024.1080p.WEB-DL.x264-GRP\n` gives the last, and `identity_text` blanks both `Source` lines. Fails first: no `identity_text`.
  - `a_scene_name_anywhere_beats_a_weak_field`: `Title: Some Film\n  Real.Movie.2024.1080p.WEB-DL.x264-GRP  \n` gives the scene name.
  - `an_indexer_nfo_gives_nothing`: starts `=newz[NZB]=`.
  - `a_complete_name_field_loses_its_path`: `Complete name : /x/Real.Movie.2024.1080p.WEB-DL.x264-GRP.mkv`.
  - `title_and_year`: `Nice Film (2019)\nplot blah\n` gives `Nice Film (2019)`, with no invented tags.
  - `the_e2e_nfo_still_reads`: `some ascii art\n.leading dot line\nRelease Name: Cool Movie 2024 1080p` gives `Cool Movie 2024 1080p`.
  - `only_the_head_is_read`: a scene token past 262,144 bytes is not found.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib nfo && cargo test --test e2e && cargo test --test multi_server`. **Expected:** pass, and the e2e name unchanged.
- [ ] **Commit:** `Read real names from NFOs and ignore their Source fields`

### Task 6: The naming step

**Files:** create `src/naming/steps.rs`; modify `src/postprocess/steps.rs`, `src/postprocess/reprocess.rs`, `src/config.rs`, `config.example.json`, `src/archive/steps.rs`, `tests/archive_stage.rs`; test `tests/naming_stage.rs`.

Harness: Part 3's `tests/pp_stage.rs` harness, with `Stage::start` and `steps::all()`, settle 1 s, `min_size_bytes: 0`.

- [ ] **Tests first:**
  - `a_junk_release_gets_a_regex_name`: save with `store::save` in `alt.binaries.teevee`, with the subject above. `display_name` set, `name_source = 5`, `is_obfuscated = 0`, `pp_state` naming bit set. Fails first: no step.
  - `the_upgrade_walk_leaves_named_rows_alone`: rows saved before the stage starts, as an upgrade finds them: one unnamed, one with an inline `display_name` that grades `Good`, one with an inline `display_name` that grades `Junk`, each with evidence that `decide` would use. After the walk, only the unnamed row has changed. The other two have their done bit set and the same `display_name`.
  - `reprocess_opts_in`: on that database, `atlas --reprocess --step naming --apply`'s statements through `reprocess::apply`: `meta.naming_rename_named` is 1, the done bits clear, and the junk inline name is now replaced. The `Good` inline name stays, by `improves`, unless inner evidence grades better.
  - `a_skipped_row_gets_no_state_row`: `DSC_0001.jpg`, 1 part, with naming the only step: no `pp_state` row and no queue row left.
  - `inner_files_rename_and_a_rerun_is_stable`: insert `archive_inner_files` rows, `Stage::rerun(id, mask(NAMING))`: `name_source = 15`, and `releases_fts match` finds the new name. Rerun again: `display_name` does not change, and a recording step at rank 220 with `on_rename` ran once only.
  - `a_rename_reruns_later_steps`: the same recording step runs after each real rename.
  - `new_inner_files_rerun_predb_matching_even_with_no_rename`: a recording step `StepId(16)`, rank 300: runs again after inner rows arrive that do not rename.
  - `part_4s_steps_pass_the_same_rule`: with naming on, a PAR2 listing whose best name is `Xk3Mz9PqL.mkv`: rows stored, no name set, `name_source` NULL.
  - `a_late_article_does_not_undo_a_name`: after a rename, save another article with a `display_name` in the `Release`: unchanged, by Part 3's upsert.
  - `naming_off_means_no_step`: `naming.enabled: false` through `set_config`: no rename, no bit.
  - `no_network_at_the_default`: `bodies_sent == 0` on a mock with `.nfo` posts, a default config and the stage running.
  - Config: `naming_defaults_when_absent`, `naming_keys_parse`, `a_bad_naming_value_falls_back`.
- [ ] **Implement** `Naming`, `NamingConfig`, the block in `config.example.json` and the registration. Add the `naming::improves` condition to `finish` in `src/archive/steps.rs`, and set `naming.enabled: false` in the harness of `tests/archive_stage.rs`, as section 4 says.
- [ ] **Run:** `cargo test --test naming_stage && cargo test --test archive_stage && cargo test --lib steps_have_unique && just test`. **Expected:** pass.
- [ ] **Commit:** `Add the naming step`

### Task 7: The NFO step

**Files:** `src/naming/steps.rs`, `src/postprocess/steps.rs`, `src/archive/steps.rs`, `tests/naming_stage.rs`, `tests/archive_stage.rs`.

- [ ] **Tests first**, on the mock server:
  - The two `an_nfo_*` tests stay in `tests/archive_stage.rs`, and pass with `NfoName` in place of `NfoStep`.
  - `nfo_runs_with_naming_off`: `naming.enabled: false`, budget set: the NFO still names the release, `name_source = 10`.
  - `no_budget_no_nfo_body`: default config: `bodies_sent == 0`, the step never runs.
  - `a_missing_nfo_is_done_not_failed`: `Got::Missing`: done bit set, `matched` false, `net_attempts = 0`.
  - `an_nfo_never_beats_a_better_source`: name at `Par2`: `applies` is false.
  - `archive_nfo_is_gone`: `steps::all()` holds no `archive_nfo`, and `atlas --reprocess --step archive_nfo` is refused with the list. An old done bit 3 does nothing.
- [ ] **Implement** `NfoName`; remove `NfoStep` and its line.
- [ ] **Run:** `cargo test --test naming_stage && cargo test --test archive_stage && cargo test --test archive_inline && cargo test --test archive_bg && cargo test --test archive_bg_default`. **Expected:** pass; `archive_bg` still ends at `name_source = 20`. Part 4's `finish` sets it when `improves` agrees, and `naming` may tidy a suffix at that same source, so assert the source and the stem rather than `.mkv`.
- [ ] **Commit:** `Move the NFO step here, and let it pass the name judge`

### Task 8: What a rename reaches

**Files:** `src/search.rs:231,237`, `src/stats_dashboard.rs:170`, `tests/e2e.rs:84`; test `tests/naming_effects.rs`, one test, env set first, as `tests/e2e.rs`.

- [ ] **Test first:** save an obfuscated release with an `a.rar` file and a good one. Then apply through `Db::on_writer` the same `update` Part 3's apply uses, with `display_name = 'Real.Movie.2024.1080p.WEB-DL.x264-GRP'`, `is_obfuscated = 0`, `name_source = 15`.
  - `search_all_releases("Real.Movie")` finds it, and the old name's token finds it too, since `name` stays.
  - `all_releases` lists the new name. `get_release` returns it.
  - `api::handle` `t=get` has `Real.Movie.2024.1080p.WEB-DL.x264-GRP.{id}.nzb` in its header, and the body is byte-identical to the NZB from before the rename.
  - `count_obfuscated` falls by 1 and `search_obfuscated` stops listing it. Before that, a flagged release with a `display_name` and no `name_source` is not listed either. Fails first: it is.
  - A flagged release with no `display_name` stays hidden from `search_all_releases`, and appears in `search_obfuscated`.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --test naming_effects && cargo test --test e2e && cargo test --lib stats_dashboard`. **Expected:** pass.
- [ ] **Commit:** `Have the obfuscated menu and count mean still unnamed`

### Task 9: `atlas --reflag-obfuscated`

**Files:** create `src/naming/reflag.rs`; modify `src/main.rs`; test `tests/naming_reflag.rs` through `CARGO_BIN_EXE_atlas`, as `tests/compact_command.rs`.

Usage: `atlas --reflag-obfuscated [--sample N] [--apply]`.

- [ ] **Tests first:**
  - `a_dry_run_changes_nothing_and_counts`: rows `a1B2c3D4e5F6g7H8` (flagged), `kP93xQ7rL.ZMtwBfa`, `Xk3Mz9PqL` (not flagged, as an old build left them), `Good.Name.2024`. Stdout says `2 would be flagged` and, with `--sample 1`, shows a name. Every row and `meta` unchanged. It works while another handle holds the shared guard.
  - `apply_sets_the_flag_and_search_follows`: both flagged. `search_all_releases` no longer finds them. `Good.Name.2024` is untouched.
  - `a_named_row_stays_visible`: the same shape with a `display_name` and NULL source: flagged, still in search.
  - `a_step_named_row_is_left_alone`: `name_source = 15`, `is_obfuscated = 0`: unchanged.
  - `a_second_run_finds_nothing`: `0 would be flagged`.
  - `a_stopped_run_resumes`: test env `ATLAS_REFLAG_STOP_AFTER=1` commits one batch and exits. A second run finishes, and `reflag_after` is gone.
  - `apply_is_refused_while_anything_holds_the_lock`: `hold_off_compaction` held: exit 1, nothing changed. `it_is_refused_with_an_indexer_alive`, using Part 3's `common::fake_indexer(home)` from `tests/common/mod.rs`, which runs a process with `--bg-indexer` in its arguments and writes `bg_indexer.pid`. A pid file alone doesn't satisfy `procs::indexer_alive`.
  - `a_reader_holding_the_wal_stops_it_cleanly`: test env `ATLAS_REFLAG_CHECK_EVERY=1`, `ATLAS_REFLAG_WAL_LIMIT=1`, and a second connection holding a read snapshot: exit 1, the message says a reader is holding the WAL, and the committed cursor is there. With the reader gone, a second run finishes.
  - `reflag_runs_after_a_convert`: on the `old_database` fixture of `tests/convert.rs`.
  - `help_lists_it`.
- [ ] **Implement:**
  - Dry run: `compact::try_hold_off_compaction`, where `None` refuses; drop the guard; read-only connections.
  - Apply: refuse on `procs::indexer_alive()` ("stop indexing first"), then `compact::Lock::take` (message on `Busy`), `refuse_unresolved_backups`, refuse when `convert::needed`, signal flag as `src/bg_indexer.rs:968-972`.
  - Per shard, 4 at once (`std::thread::scope`, as `src/compact.rs:345`). `db::open_shard`, `tune_for_writing`.

```sql
-- read outside the transaction; rowid order, so it never revisits a page
select id, name from releases where id > :cursor and name_source is null and is_obfuscated = 0 order by id limit 2000;
-- for the rows parser::is_obfuscated flags, then the cursor, in one BEGIN IMMEDIATE:
update releases set is_obfuscated = 1 where id = ?;
insert or replace into meta (key, value) values ('reflag_after', ?);   -- the highest id read
```

  **The WAL.** `tune_for_writing` turns auto-checkpointing off, and a passive checkpoint never resets the WAL while a reader such as the API holds a snapshot. So every 64 commits: `pragma wal_checkpoint(restart)`, up to 8 tries 250 ms apart. Then, if the `-wal` file is over 4 GiB, stop with exit 1 and `a reader is holding the WAL; close the API or the menu and run this again`. The cursor is committed, so the next run resumes. This is the rule Part 3's purge uses: call its helper, `postprocess::batch::checkpoint_or_stop(conn, path, limit)` with `batch::COMMITS_PER_CHECKPOINT` and `batch::WAL_LIMIT`, rather than a second copy, and adapt if it merged under another name.

  Progress every 1M rows; at the end `delete from meta where key = 'reflag_after'` and `pragma wal_checkpoint(truncate)`. `name_source is null` needs no index: the filter runs on rows the rowid walk already reads. No trigger fires, because `releases_au` is `after update of name, display_name`.
- [ ] **Run:** `cargo test --test naming_reflag && just test`. **Expected:** pass.
- [ ] **Commit:** `Add atlas --reflag-obfuscated, dry run by default`

### Task 10: `atlas --unname`

**Files:** create `src/naming/unname.rs`; modify `src/main.rs`, `src/naming/reflag.rs` (share the walk); test `tests/naming_unname.rs` through `CARGO_BIN_EXE_atlas`.

Usage: `atlas --unname [--sample N] [--apply]`.

- [ ] **Tests first:** rows with `name_source` 3, 5, 10, 15, 20 and 100, one with NULL and an inline `display_name`, and one with NULL and none.
  - `a_dry_run_changes_nothing_and_counts`: stdout gives a count per source, `2 would be cleared`, and `--sample 1` shows a name. Every row and `meta` unchanged. It works while another handle holds the shared guard.
  - `apply_clears_only_regex_and_posted_names`: 3 and 5 have `display_name` and `name_source` NULL. All others are untouched, including the inline name.
  - `search_follows`: `releases_fts` no longer finds the cleared name's tokens, and still finds `name`. The `is_obfuscated` flag is `parser::is_obfuscated(name)` again, so a random `name` leaves search and a real one stays. A random `name` ending in Part 2's collision suffix leaves search too.
  - `later_steps_are_queued`: a registered test step at rank 400 with `on_rename` has its done bit cleared and a `pp_queue` row; another at `failed` has that bit and its attempts cleared and runs again on the restored name. `naming`'s own bit stays set, and a stage run leaves the row unnamed.
  - `it_removes_the_opt_in`: `meta.naming_rename_named` is gone afterward, and a dry run keeps it.
  - `a_stopped_run_resumes`: `ATLAS_UNNAME_STOP_AFTER=1`, then a second run finishes, and `unname_after` is gone.
  - `apply_is_refused_while_anything_holds_the_lock`, `it_is_refused_with_an_indexer_alive`, as in Task 9. `a_second_run_finds_nothing`. `help_lists_it`.
- [ ] **Implement:** the same refusals, shards and WAL rule as Task 9. `:later` is the OR of `mask(s.id())` over `steps::all()` with `s.rank() > 200` and `s.reruns().on_rename`.

```sql
-- read outside the transaction
select id, name from releases where id > :cursor and name_source in (3, 5) order by id limit 2000;
-- per row, then the cursor, in one BEGIN IMMEDIATE. ?2 is parser::is_obfuscated(without_collision_suffix(name)), as
-- Part 2 worked the flag out before adding it. releases_au updates FTS
update releases set display_name = null, name_source = null, is_obfuscated = ?2 where id = ?1;
update pp_state set done = done & ~:later, failed = failed & ~:later, db_attempts = 0, net_attempts = 0, net_next_at = 0 where release_id = ?1;
insert into pp_queue (release_id, due_at) values (?1, 0) on conflict(release_id) do nothing;  -- when :later != 0
insert or replace into meta (key, value) values ('unname_after', ?);
-- at the end
delete from meta where key in ('unname_after', 'naming_rename_named');
```

- [ ] **Run:** `cargo test --test naming_unname && cargo test --test naming_reflag`. **Expected:** pass.
- [ ] **Commit:** `Add atlas --unname to undo names taken from subjects, dry run by default`

### Task 11: Docs and full verification

**Files:** `README.md`, `src/main.rs` help, `config.example.json`.

- [ ] **Implement** section 12.
- [ ] **Run:** `just format-check && just lint && just test`, then section 11. **Expected:** all pass.
- [ ] **Commit:** `Document name fixing`

## 9. Performance and load

The estimates here are derived, not measured on live data, which no test may touch. Timings were measured in a scratch crate, with an unrelated compaction taking CPU on the same machine, so they are on the slow side. Nothing here is a gate.

**`is_obfuscated` at ingest.** 300–450 ns a call, once per new release per slice (`src/parser.rs:187-191`). Peak 30,000 articles/s, all new releases: 13 ms per second, 1.3% of one core. In practice far less, and on a parse thread.

**Reach.** Part 3's `judge` now passes about 100M rows, and `worth_running` decides which run. These shares come from the overview's sample of 100 scaled to 118M, so treat them as rough. The split of images assumes the one-article rows that aren't fragments are images.

| Rows | Share | Count | `naming` |
|---|---:|---:|---|
| One-article fragments | 29% | 34M | Rejected by Part 3 |
| Images, one article | about 39% | 46M | Skipped, under 1 µs |
| Images, several articles | about 19% | 22M | Skipped unless the group has rules |
| The rest | about 13% | 15M | Runs, unless one article in a group with no rules |

So `run` reaches about 15M rows, and at most about 37M if every multi-article image is in a group with rules. Named rows are in those counts but return after one key read, until the user opts in.

**Cost per row, measured.**

| What | Time |
|---|---|
| `worth_running` | 0.4–2 µs |
| `decide`, 10 files, group with no rules | 11 µs |
| `decide`, 10 files, `alt.binaries.boneless` (30 rules) | 23 µs |
| `decide`, 10 files, `alt.binaries.teevee` (89 rules) | 40 µs |
| `ctx.files()` and `inner_files` (estimate) | 20–50 µs warm, up to about 1 ms cold |

**Duration.** The walk's rate is Part 3's budget, not this step's cost: 118M rows at 2,000 rows/s is 16.4 h, however many pass. Skipped rows cost 2 ms of CPU per thousand. If the sample is badly wrong and nothing is skipped, 2,000 rows/s × 100 µs is 20% of one core, and the 16.4 h still holds.

**Writes.** A rename is one `update releases` plus an FTS delete and insert on the shard's writer: about 30–60 µs and 2–4 KB of WAL. Old rows hold one file's name per row, so the posted-file tier adds nothing on them, and only a regex in one of the 118 groups with rules can rename one. The flagged share is 14.2M, so about 15M renames is a fair ceiling, and 37M the hard one.

| Renames | Writer time, all shards | WAL through the walk | FTS growth until `--compact` |
|---:|---:|---:|---:|
| 15M | 8–15 min | 30–60 GB, about 1 MB/s | 0.9–1.8 GB |
| 37M | 18–37 min | 74–148 GB, about 2.5 MB/s | 2.2–4.4 GB |

At the peak, 2,000 applies/s over 8 writers is 250/s per writer, about 1.5% of a writer, and 8 MB/s of WAL. With no rename, Part 3 writes only `pp_state`, 30 B a row, and a skipped row writes nothing. The 1.6M rows named today are untouched. FTS growth is about 60–120 B a name, and `--compact` rebuilds the index (`src/compact.rs:715`).

**Memory.** Measured 100–320 MB to compile all 1,154 rules, about 90–280 KB each. Typical, a handful of tracked groups with rules: under 30 MB. The worst case is every one of the 118 groups tracked. The TSV is 226 KB in the binary, parsed once, with 118 small group regexes. First touch of a group costs at most 0.5 s on one stage thread, and other groups are not held up.

**Network.** None for `naming`. `nfo` uses Part 3's budget, default 0: at most 2 bodies per release, and only for releases with an NFO file and no good name.

**`--reflag-obfuscated`.** Reads 2,000 rows a batch by rowid: sequential, 2–5 µs a row plus 0.5 µs for the check. 118M rows over 4 shards at once: about 2–10 min. Update cost depends on how many rows change. An in-place integer update dirties its page once per batch, and at about 40 rows per 4 KB page the worst case is 3M pages, about 12 GB of WAL over all shards. The WAL rule in Task 9 keeps that from piling up behind a reader: at most 4 GiB per shard before it stops. Memory: 4 connections at 256 MB cache. Dry run: the same reads, no writes, beside a running indexer.

**`--unname`.** The same read, then per cleared row an update, an FTS delete and insert, a `pp_state` update and a queue insert. At 15M rows and 4 shards at once: about 5–10 min, and 30–60 GB through the WALs, with the same rule.

## 10. Failure handling

| Case | What happens |
|---|---|
| A group regex or rule fails to compile at run time | Skipped with one line on stderr. `every_rule_compiles` keeps this from shipping |
| Non-UTF-8 subject or name | Rusqlite gives an error for that row: `StepError::Retry`, then the failed bit after `max_attempts`. Atlas stores subjects as text, so this needs a damaged shard |
| A very long subject | Over 1,024 bytes skips the regex tier. The rest of `decide` still runs |
| Regex blow-up | Not possible: the crate is linear time |
| A corrupt NFO | `from_utf8_lossy`, capped at 256 KiB. No candidate means done with `matched: false` |
| `Got::Missing`, `TooBig`, `Refused` on an NFO | Skips the candidate. Done with `matched: false` |
| Server down during the NFO step | `StepError` from `fetch_bodies` propagates. Part 3 gives the attempt back |
| A wrong name got through, or a bad regex port | `atlas --unname --apply` clears names from regexes and posted files, the only sources an upgrade writes on its own. A later `--reprocess --step naming` on a fixed build names them again. Names from a body or PreDB are not cleared: a better source with a better grade replaces them, and PreDB always does |
| Crash during the DB lane | Nothing written, per Part 3. `decide` is pure, so the rerun gives the same answer |
| Re-run | No `NameChange` when nothing improves. A second pass is a no-op by test |
| New regexes in a later release | `atlas --reprocess --step naming --apply`. Only names that grade better than the current one are taken |
| `--reflag-obfuscated` or `--unname` crash or Ctrl-C | The cursor commits with each batch. Run it again. Both are idempotent |
| Either after a compaction | `reflag_after`, `unname_after` and `naming_rename_named` are copied with `meta`, so it resumes |
| A reader holds the WAL during either | Restart checkpoints fail, and past 4 GiB it stops with a message and a committed cursor. Close the reader and run it again |

## 11. Acceptance checks

Run in the part's worktree. None touch live data or a live server.

```bash
just format-check && just lint && just test
# expected: all pass, no warnings

python3 scripts/naming_regexes_to_tsv.py "$SRC/database/seeders/ReleaseNamingRegexesTableSeeder.php" /tmp/n.tsv /tmp/s.tsv \
  && cmp /tmp/n.tsv data/naming_regexes.tsv && cmp /tmp/s.tsv tests/fixtures/naming_samples.tsv
# expected: prints 1154 rules, 1045 samples; cmp is silent

grep -vc '^#' data/naming_regexes.tsv
# expected: 1154

cargo test --lib obfuscation && cargo test --test naming_regexes && cargo test --test naming_regexes_lazy
# expected: pass, including the 1,045 samples and 89 rules compiled for one group

cargo test --lib decide
# expected: pass, including a_second_run_changes_nothing

cargo test --test naming_stage no_network_at_the_default
# expected: pass, bodies_sent == 0

cargo test --test naming_effects && cargo test --test naming_reflag
# expected: pass

cargo test --test naming_stage the_upgrade_walk_leaves_named_rows_alone && cargo test --test naming_unname
# expected: pass: a restart renames no named release, and --unname clears sources 3 and 5 only

HOME_DIR=$(mktemp -d)
ATLAS_HOME=$HOME_DIR cargo run --quiet -- --help | grep -E -- '--reflag-obfuscated|--unname'
# expected: both listed

! grep -rn 'fancy' Cargo.toml src/naming
# expected: no output, exit 0
```

## 12. Docs

- `README.md`, "Top-level keys" (`:127`): a `naming` row.
- New `### Name fixing` under Usage: what evidence is used and in what order; what a good name is; that names only improve; that it needs no network, apart from the NFO step and its budget; that upgrading only names unnamed releases; `atlas --reprocess --step naming` to go further, and `atlas --unname` to undo.
- "Search" (`:283-287`): obfuscated posts covers dotted, hyphenated and short random names. A flagged name with no real name stays out of search.
- New paragraph in `### Purging and reprocessing` (Part 3): `atlas --reflag-obfuscated` and `atlas --unname`, their dry runs, that the first hides newly flagged old rows, and the estimates from section 9.
- "Stats dashboard", Content (`:326-331`): "obfuscated" means flagged and with no recovered name.
- "Credits" (`:516`): newznab-tmux's naming regexes and name fixing, GPLv3.
- "Source layout" (`:491`): `src/naming/`, `data/`, `scripts/`.

## 13. Decisions

1. **`regex` only, no `fancy-regex`.** The seeder has no lookaround or backreference, and the four possessive quantifiers lose nothing as greedy. Part 2's plan did not exist when this was written. If it adds `fancy-regex` for its own seeder, this part still does not use it. A scan of `CollectionRegexesTableSeeder.php` also finds no lookaround or backreference, so Part 2 likely needs none either.
2. **Unicode classes stay in rules without upstream's `u` flag.** PCRE treats those as ASCII. Rewriting 766 rules to ASCII classes cut memory by a third in a trial (324 to 202 MB) but adds a converter pass and stops a rule matching a non-ASCII title. Laziness already bounds the common case.
3. **A token judge replaces 39 ordered regexes.** They find a name inside free text. Atlas's candidates are already isolated by `parse_subject`, Part 4 and the NFO's own extraction. The judge also gives a grade, which the regexes do not.
4. **Grade before source within a run, source as the hard limit across runs.** A `Scene` name from a regex beats a `Weak` one from PAR2 at first naming. After that, no lower source replaces anything.
5. **A candidate must grade strictly better than the current name, whatever its source.** A first version let a better source replace at an equal grade, and that renamed well-named posts to short inner names: `grp-title-1080p` over a real scene name, the case Part 4's decision 15 warns of. Strict grading also makes re-runs stable by construction, and is stricter than Part 3's "equal replaces", which remains the writer's backstop. The cost: a `Good` name at ingest is not swapped for a different `Good` one from PAR2. The aim of this part is a release-like name from a junk one, so that is the right way round. A name is graded by what it would clean to, so `Title-GRP.mkv` counts as `Scene`.
6. **The new check flags a superset of the old one, and `VISIBLE_RELEASE` stays.** Search's own `length(name) < 16 or glob` clause hides 16+ alnum names whatever the flag says. Un-flagging `TheMatrixReloaded` would either do nothing or need that clause gone, which would expose very old rows whose flag defaulted to 0 (`src/db.rs:373-374`). A readable name of that shape stays flagged, as today, and a step can still give it a `display_name`.
7. **Bias towards missing a random name rather than hiding a real one.** A flag removes a release from search, so a false positive loses it. The thresholds give 0.019% on a word list and let through about a third of all-lowercase random strings.
8. **`--reflag-obfuscated` is a command, not part of the stage.** `naming` skips most rows without running (`worth_running`: one article in a group with no rules, or an image name), and a step that proposes no name writes no flag, so the stage would leave most stale flags as they are. And it makes rows vanish from search, which hard rule 6 and "nothing changes until the user chooses" say the user decides. It only ever sets the flag, so re-running is safe.
9. **PAR2 hash donors are not ported.** `fixNamesWithParHash` finds another release with the same file MD5 and copies its name. It needs each file's MD5 stored and indexed across all 8 shards, since donors live in other groups. Part 4 parses `md5` and `md5_16k` (`Par2File`) and stores neither. Roughly 40 B × 5 files × 37.6M releases is 7.5 GB, with a cross-shard lookup per release. Upstream only accepts a donor that is itself PreDB-confirmed (`DonorMatchSelector.php:118-146`), and still lists eligibility problems with shared support files (root to-do file lines 212-220). Bodies are budgeted, so at 300 per minute the hash index reaches 37.6M releases in 261 days. PreDB matching in Part 6 gets most of the same names for far less. It can be added later from a new `archive_` table without touching this part.
10. **`PostedFile` and `NamingRegex` sit below `Nfo`, against Part 4's assumption.** Part 4's three values do not change. Text from the subject is the weakest evidence, whoever wrote the regex. Part 4's section 6 and decision 15 say other sources sit above its own, so those two sentences need updating.
11. **`Par2` outranks `ArchiveInner`, Part 4's order.** PAR2 names the set; inner names are per file. A season pack's PAR2 says `Show.S01-GRP.part01.rar`, its RAR says `S01E01`.
12. **`InlineBody` is an effective value, never stored.** Old rows can't say whether their name came from a PAR2 or an NFO. A good one is protected from regexes and open to any body source. A junk one, which the old "biggest file" rule produced, is open to all.
13. **Literal names are kept as posted.** Upstream's `formatSearchName` turns spaces to dots when a name looks like a scene release. Part 6 compares titles literally and needs the original spelling, and a person reads either.
14. **No invented tags.** `DVDRip XVID NoGroup`, `-NoGroup` and `(GAMES)` state things nobody checked, and would mislead Part 7's categories.
15. **`Naming` asks for PreDB matching when inner files exist.** Part 4's rerun mask only names step 8, and a release may have useful inner names without a rename. `rerun` costs one more DB-lane pass. Bits of unregistered steps are harmless.
16. **The menu and dashboard mean "still unnamed".** README `:287` already promises it. `tests/e2e.rs:84` changes to match.
17. **Inner files are read at most 256 per release, through Part 4's function.** `MAX_STORED` caps them already, and the file-level cap in `best_from_paths` guards posted files.
18. **The converter is Python.** It parses PHP source text once, its output is committed, and no build needs it. Rust would add a binary for a one-time job.
19. **Part 4's steps pass `improves` too.** Its `looks_random` lets 8–15 character random stems through, and a junk name at source 20 would block regexes and NFOs for good, because Part 3's apply refuses a lower source. One rule in one place, on top of Part 4's own conditions.
20. **`nfo` is not switched off by `naming.enabled`.** With a budget set, Part 4 stops the inline lookup for good, so turning naming off must not also lose NFO names.
21. **A step always sends `Some` for both `display_name` and `obfuscated`.** Part 3's `NameChange` accepts `None` for either, so a flag-only change is legal, but this part never needs one: every change here comes with a name, and `--reflag-obfuscated` covers old rows.
22. **An upgrade renames only unnamed rows, and there is no copy of old names.** A restart is not consent to rewrite searchable names. `run` leaves inline names alone until `meta.naming_rename_named` is set, by `--reprocess --step naming --apply`. `applies` can't do this: it does no I/O, and false never sets the done bit, so `--reprocess` could not reach the row. A `naming_previous` table would cost a row per replacement for an undo that `--unname` mostly gives. What it can't give back is an inline name replaced after opting in; only one that graded `Junk` can be replaced by a regex, so little is lost.
23. **`--unname` clears sources 3 and 5 only.** Those are all an upgrade writes on its own. A body source needs a budget the user set, and PreDB needs `predb.enabled`. It re-queues later steps, because their results rest on a name that has gone, and leaves `naming`'s bit set so the name stays cleared until the user asks.
24. **`NamingRegex` and `PostedFile` need `Good`.** Rule 132 shows subject text can be an advert, and `Weak` means no release marker at all. Upstream's plausibility rule, which `Good` ports, would reject it too. Inner files and NFOs keep `Weak`: those are the post's own names, and a book title has no marker.
25. **A tidy is only for a `display_name` or Part 2's collision suffix.** Stripping `.mkv` from a posted `name` is no new information, and would be tens of millions of FTS writes. The suffix is different: Part 2 made it up, and it must not stay on show.
26. **Only Part 2's exact 8 digits are stripped.** ` [\d+]` would also take `Movie.Name [2019]`. `a [4]`, the other form, cleans to a name too short to be anything but `Junk` with or without it.
27. **`subject_base` is Part 2's, not a second copy.** Part 2 merges first and exports it, and its tests already pin the form the regexes expect.
28. **`applies` skips by what the row can't gain, not by what it looks like.** A one-article post's filename is its name, so with no regex for the group nothing can change. A picture isn't worth a read. Everything else runs, because the cost is small and bounded by the walk's rate. `Xk3Mz9PqL` with many articles has to run: that's the release this part exists for.
