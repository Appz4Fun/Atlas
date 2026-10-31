# Part 7: Categories

Every Atlas line number below is at base commit `de598b6`. Parts 1–6 merge first and shift them: find the named function, keep the intent, and record any difference in the PR. Upstream paths are at the commit pinned in `00-overview.md`. Part 3 wins any disagreement.

The rules, the table test and the SQL here were prototyped in a scratch crate before this plan was written. 243 of the 244 regexes extracted from the upstream categorizers compiled as they stand under `regex::bytes` with Unicode off; rule R3 fixes the other. The prototype ported all nine pipes with parts of XXX, music and book trimmed, used Part 5's `is_obfuscated` as its plan gives it, passed every row of the Task 3 table, and took 4–21 µs a release depending on the share of image names. The cursors and queries ran over 8 synthetic shards of 1M rows each with `EXPLAIN QUERY PLAN` output read directly (SQLite 3.53.2, the `rusqlite` 0.40 bundle). No live data was opened. The numbers in section 9 are labelled measured or derived.

## 1. Goal

Atlas stops reporting every release as 7000. A database step gives each settled release a real Newznab category from its final name (`coalesce(display_name, name)`), its group and its poster, and re-runs when the name improves. The API lists the categories in `caps`, reports them on each result, and honours `cat`, including parents: asking for `5000` includes `5040`. A release with no category row reports 8000, the Other root, and matches `cat=8000`. 7000 stays real Books. Results stay newest first and stay fast at 118M rows. The stats dashboard and the terminal search show categories. No network, no article bodies.

## 2. Non-goals

- No TV or movie IDs, title parsing or providers (Part 8).
- No `CategoryRegexesTableSeeder` port: upstream ships one placeholder row (`database/seeders/CategoryRegexesTableSeeder.php`), and Atlas has no per-group regex table.
- No per-category minimum sizes, disabled categories, user roles or menu grouping (`app/Models/Category.php:217-229`).
- No implicit category for a search type. `t=tvsearch` with no `cat` is not limited to TV, unlike upstream (`app/Models/Category.php:350-352`): a row has no category until the walk reaches it, so the limit would empty searches on an upgraded database (hard rule 11).
- No category on a rejected release. `rules::judge` runs before any step (Part 3 section 6), so a blocked group, a fragment, an empty row, a par2-only post or too few files gets no row and reports 8000. `judge` no longer rejects by size, image type or completeness, so small releases, image posts and incomplete releases do reach the step.
- No row for a decision the default already states, section 5.
- No change to `newznab:response total` (`src/api.rs:236`), or to `limit` and `offset` handling when no `cat` is given.
- No category in `src/ai.rs` tables or in `show_release`.

## 3. Source map

In this plan `Categorizers/` and `Pipes/` mean `app/Services/Categorization/Categorizers/` and `app/Services/Categorization/Pipes/`. `XxxCategorizer.php` and its siblings are in `Categorizers/`. `ReleaseContext.php` and `CategorizationResult.php` are in `app/Services/Categorization/`. All upstream paths are repo-relative.

| Upstream | Take | Leave behind |
|---|---|---|
| `database/seeders/RootCategoriesTableSeeder.php`, `database/seeders/CategoriesTableSeeder.php`, `app/Models/Category.php:55-215` | The numbering, see section 4 | Root id `1`, ids `10` and `20`, status and preview columns |
| `database/migrations/2024_09_15_184830_add_xxx_vr_category.php`, `database/migrations/2025_08_28_000000_add_onlyfans_category_to_categories_table.php` | Ids 6046 and 6047 | |
| `docs/newznab_api_specification.txt:264-306` | Ranges (`:267,274,275`): `0000-0999` reserved, `7000-7999` Books, `8000-8999` Other | `7900 Category Not Determined` |
| `app/Services/Categorization/CategorizationPipeline.php:238-251` and each pipe's `PIPE_PRIORITY` | Pipe order: Misc 1, GroupName 5, XXX 10, TV 20, Movie 25, PC 30, Console 35, Music 40, Book 45, safety net 100 | Laravel pipeline, FFI paths, `w*` batch helpers |
| `app/Services/Categorization/CategorizationResult.php:78-90,123-141,178-192` | Success, override and protected-misc rules | Debug payload |
| `app/Services/Categorization/Pipes/CategorizationPassable.php:130-133,168-181`, `Pipes/AbstractCategorizationPipe.php:88-106`, `Pipes/MiscPipe.php:94-135`, `Pipes/MiscSafetyNetPipe.php:118-165` | Stop at confidence 0.95, skip, update best, lock, downgrade | |
| `Categorizers/MiscCategorizer.php:208-247,389-403,406-432`, `app/Traits/DetectsHashedNames.php:66-107,188-192,265-285,287-305,349-367,396-410` | Hash tokens inside a longer name, the punctuation rule, low-signal analysis, archive and dataset rules | The other hash and gibberish rules: Part 5's `is_obfuscated` answers the same question (R7) |
| `Categorizers/GroupNameCategorizer.php:47,140-172` | Group hints | |
| `Categorizers/XxxCategorizer.php:68-83,176-617`, `app/Services/Categorization/ReleaseContext.php:125-170` | XXX rules, adult markers | Per-call `preg_quote` regexes (R9) |
| `Categorizers/TvCategorizer.php:65-73,184-189,213-489`, `app/Services/Categorization/ReleaseContext.php:17` | TV rules | |
| `Categorizers/MovieCategorizer.php:165-195,220-393` | Movie rules | |
| `Categorizers/PcCategorizer.php:67-70,170-181,207-340` | PC rules | |
| `Categorizers/ConsoleCategorizer.php:158-169,195-346` | Console rules | |
| `Categorizers/MusicCategorizer.php:67-69,183-194,220-441` | Music rules | |
| `Categorizers/BookCategorizer.php:159-217,242-379` | Book rules | |
| `database/support/LegacySettingsManifest.php:99-100` | Defaults: `categorize_foreign` on, `categorize_web_dl` off | The settings tables |
| `rust/pipeline-ffi/src/categorize.rs` | Nothing lifted. Read at `:2816` and `:3798` as a cross-check on the list constants | 57K lines matching PCRE byte for byte |
| `app/Services/NameFixing/NzbSplitUnwrapper.php`, `app/Services/NameFixing/Extractors/ObfuscatedSubjectExtractor.php` (called at `app/Services/Categorization/CategorizationPipeline.php:133-138`) | Nothing: Atlas names come from its own parser and Part 5. Upstream's rewrite of `_`, `+` and `.` to spaces is what protects it from the rules R7 and R11 change | Both |

## 4. Atlas changes

**New `src/category/`**: `mod.rs` (re-exports, `categories_of`), `table.rs`, `lists.rs`, `rules.rs`, `step.rs`. `rules.rs` carries a GPLv3 attribution header naming the upstream categorizers. `src/lib.rs` gets `pub mod category;`.

**New `scripts/extract_category_lists.py`, `data/category_lists.tsv`**: see Task 2.

**`src/postprocess/schema.rs`**: `SHARD_TABLES` gains `&crate::category::RELEASE_CATEGORY`. **`src/postprocess/steps.rs`**: `all()` gains `Arc::new(crate::category::CategoryStep::new())`.

**`src/config.rs`**: `Config` (`:134`) gains `pub category: CategoryConfig`, set in `Default` (`:157`) and in `from_value` (`:282`).

**`src/api.rs`**:
- `CAPS` (`:17`): `:24-28` become `supportedParams="q,cat,limit,offset"` on `search`, `movie-search`, `audio-search` and `book-search`, and `q,season,ep,cat,limit,offset` on `tv-search`. `:30-32` become `<categories><!--categories--></categories>`.
- `:137` gains `.replace("<!--categories-->", &crate::category::caps_xml())`.
- `search` (`:185`): reads `cat` and takes the filtered paths below. It then loads `categories_of` for the ids and passes the map to `rss`.
- `rss` (`:204`, as Part 4 left it, taking `passwords`): gains `categories: &HashMap<i64, i64>`. In `:211` `value="7000"` becomes `value="{}"` with `categories.get(&r.id).copied().unwrap_or(category::UNCATEGORIZED)`. `:152` passes an empty map. Part 4's test `rss_reports_a_known_password_only` gains the argument, an empty map.

```rust
let filter = args.get("cat").and_then(|c| category::parse_filter(c));
let releases = match (&filter, q.trim().is_empty()) {
    (None, true) => all_releases(offset / limit, limit),
    (None, false) => search_all_releases(q, offset / limit, limit),
    (Some(f), true) => newest_in_categories(f, offset, limit).map(|p| p.rows),
    (Some(f), false) => search_in_categories(q, f, offset, limit),
};
```

**`src/search.rs`**: after `newest` (`:224`): `CatPage`, `newest_in_categories`, `newest_in_categories_with`, `search_in_categories`, below.

**`src/app.rs`**: `show_results` (`:31`) gains `Column::new("Category").width(20)` after `Size` (`:46`), a cell from `category_cell(&cats, r.id)`, and `saturating_sub(61)` in place of `saturating_sub(40)` at `:52`. `cats` is `category::categories_of(&ids).unwrap_or_default()`. `category_cell` gives `label(id)`, or `-` with no row: the terminal states what is known, and only the API reports the default number.

**`src/stats_dashboard.rs`**:
- `Content` (`:133`) gains `#[serde(default)] categories: Vec<(i64, i64)>`: (root, releases).
- `compute_content` (`:160`), before `c.computed_at` (`:224`): runs the query below and sums by `root_of`.
- `draw_content` (`:881`): `:903` becomes `Layout::horizontal([Constraint::Ratio(1, 4); 4])`, adding a `categories` panel between `names` and `biggest groups`: one `kv` per root present, with count and share, and `kv("no category yet", ...)` for `releases` minus the sum, then `note("those show as 8000 in the API")`.

```sql
-- one covering index scan per shard, only on the page's recount (`r`)
select category, count(*) from s{i}.release_category group by category
```

**Earlier parts' stage tests.** `steps::all()` now includes `category`, which writes for every release that passes the rules. `tests/archive_stage.rs` (Part 4), `tests/naming_stage.rs` (Part 5) and `tests/predb_stage.rs` (Part 6) start the stage with `steps::all()`, so their harnesses set `category.enabled: false` through `set_config`, as Part 5 does for naming. Part 3's `tests/pp_stage.rs`, `pp_bodies.rs` and `pp_auto_compact.rs` pass an explicit list of their recording steps and need no change; a harness there that uses `steps::all()` gets the same line.

### The category table

Leaf ids are upstream's constants. The Other root is the one place they differ, see decision 2. A missing row reads as the Other root, decision 1.

| Root | Sub-categories |
|---:|---|
| 1000 Console | 1010 NDS, 1020 PSP, 1030 Wii, 1040 Xbox, 1050 Xbox 360, 1060 WiiWare VC, 1070 Xbox 360 DLC, 1080 PS3, 1110 3DS, 1120 PS Vita, 1130 WiiU, 1140 Xbox One, 1180 PS4, 1999 Other |
| 2000 Movies | 2010 Foreign, 2030 SD, 2040 HD, 2045 UHD, 2050 3D, 2060 BluRay, 2070 DVD, 2080 WEBDL, 2090 X265, 2999 Other |
| 3000 Audio | 3010 MP3, 3020 Video, 3030 Audiobook, 3040 Lossless, 3050 Podcast, 3060 Foreign, 3999 Other |
| 4000 PC | 4010 0day, 4020 ISO, 4030 Mac, 4040 Phone-Other, 4050 Games, 4060 Phone-IOS, 4070 Phone-Android |
| 5000 TV | 5010 WEB-DL, 5020 Foreign, 5030 SD, 5040 HD, 5045 UHD, 5060 Sport, 5070 Anime, 5080 Documentary, 5090 X265, 5999 Other |
| 6000 XXX | 6010 DVD, 6020 WMV, 6030 XviD, 6040 x264, 6041 HD Clips, 6042 SD Clips, 6045 UHD, 6046 VR, 6047 OnlyFans, 6050 Packs, 6060 Imageset, 6080 SD, 6090 WEBDL, 6999 Other |
| 7000 Books | 7010 Magazines, 7020 Ebook, 7030 Comics, 7040 Technical, 7060 Foreign, 7999 Other |
| 8000 Other | 8010 Misc, 8020 Hashed |

A step stores leaves only; a test enforces it. `UNCATEGORIZED = 8000` is read-time only and nothing stores it.

**`parse_filter(param) -> Option<CatFilter>`**, in this order:

1. Split on `,`, trim, parse each as `i64`. A token that does not parse is dropped.
2. Any token equal to `-1`, or no tokens left: `None`, meaning no filter. Upstream treats `-1` the same way (`app/Models/Category.php:350`).
3. Each token: a root in the table adds its subs, and adds `uncategorized = true` when it equals `UNCATEGORIZED`. A leaf in the table adds itself. An alias adds its target: `1090→1999`, `2020→2999`, `5050→5999`, `6070→6999`, `7050→7999`. These are the same sub-categories under Prowlarr's standard ids (`src/NzbDrone.Core/Indexers/NewznabStandardCategory.cs` in Prowlarr). Anything else is dropped.
4. Sort and dedupe. Every leaf in the table and `uncategorized`: `None`. Otherwise `Some`, and an empty filter matches nothing, so `cat=9999` returns no rows rather than everything.

### The rules

`classify` runs these, in order. Each pipe returns at most one hit, with a confidence in thousandths (`0.85` is `850`).

```rust
pub fn classify(i: &Input<'_>, o: &Options) -> Decision {
    if i.name.trim().is_empty() {
        return Decision { category: HASHED, confidence: 1000, rule: "unnamed" };
    }
    // the same test as VISIBLE_RELEASE (src/search.rs:36), negated
    if is_hidden(i.name, i.has_display_name, i.is_obfuscated) {
        return Decision { category: HASHED, confidence: 1000, rule: "hidden" };
    }
    let c = Ctx::new(i, o);
    let (low_signal, misc) = misc_pipe(&c);
    let mut best = NO_MATCH; // category UNCATEGORIZED, confidence 0, rule "no_match"
    if let Some(hit) = misc {
        if locks(&hit) { return hit; }
        if overrides(&hit, &best) { best = hit; }
    }
    // R12: an image is not a video, a book or a game
    let pipes: &[Pipe] = if R.image.is_match(c.name) { &[group_xxx_pipe, xxx_pipe] } else { &ALL_PIPES };
    for pipe in pipes {
        if best.confidence >= 950 { break; }
        if let Some(hit) = pipe(&c) && overrides(&hit, &best) { best = hit; }
    }
    if low_signal && best.rule.starts_with("group_name_") {
        return Decision { category: MISC, confidence: 650, rule: "group_only_low_signal" };
    }
    best
}

pub(crate) fn is_hidden(name: &str, has_display_name: bool, is_obfuscated: bool) -> bool {
    !has_display_name && (is_obfuscated || (name.chars().count() >= 16 && name.bytes().all(|b| b.is_ascii_alphanumeric())))
}

/// CategorizationResult.php:178-192
fn protected(d: &Decision) -> bool {
    d.category == HASHED || (d.category == MISC && (locks_rule(d.rule) || d.rule == "group_only_low_signal"))
}
fn locks_rule(rule: &str) -> bool { ["hash_", "obfuscated_", "gibberish_"].iter().any(|p| rule.starts_with(p)) }
fn locks(d: &Decision) -> bool { d.category == HASHED || locks_rule(d.rule) }
const ALL_PIPES: [Pipe; 8] = [group_pipe, xxx_pipe, tv_pipe, movie_pipe, pc_pipe, console_pipe, music_pipe, book_pipe];
fn group_xxx_pipe(c: &Ctx<'_>) -> Option<Decision> { group_pipe(c).filter(|d| d.rule == "group_name_xxx") }

/// CategorizationResult.php:123-141
fn overrides(new: &Decision, best: &Decision) -> bool {
    if new.category != MISC && new.category != HASHED && protected(best) && new.confidence < 600 { return false; }
    new.confidence > best.confidence || (new.confidence == best.confidence && new.category != MISC && best.category == MISC)
}
```

Port each pipe from the `*Php` method named here, rule for rule and in source order, with each regex's text, flags, category, confidence and rule name. The upstream text is the rule source. Every regex stays where the source lines put it, apart from R1–R12.

| Pipe | Skip when (source) | Rules (source) | Category constants |
|---|---|---|---|
| Misc | never | R7's list, in that order; `MiscCategorizer.php:208-247` for `lowSignal` | 8020, 8010 |
| Group | empty group | `GroupNameCategorizer.php:140-172` | 5070, 5999, 2999, 6999, 3999, 1999, 4010, 7020 |
| XXX | never (its gate is at `:181`) | `XxxCategorizer.php:176-617`; lists `:68-83` | 6010–6999 |
| TV | `hasAdultMarkers` (`TvCategorizer.php:184`) | `:213-489`; gate `looksLikeTV` `:262`; lists `:65,67,73` | 5010–5999 |
| Movie | `MovieCategorizer.php:165-193` | `:220-393`; gate `looksLikeMovie` `:276` | 2010–2999 |
| PC | `PcCategorizer.php:170-181` | `:207-340`; lists `:67,70` | 4010–4070 |
| Console | `ConsoleCategorizer.php:158-169` | `:195-346` | 1010–1999 |
| Music | `MusicCategorizer.php:183-194` | `:220-441`; lists `:67,69` | 3010–3999 |
| Book | `BookCategorizer.php:159-217` | `:242-379` | 7010–7040 |

`Category::OTHER_MISC` is 8010, `OTHER_HASHED` is 8020, and `PC_PHONE_OTHER` is 4040. Every other constant keeps its number from `app/Models/Category.php`. `hasAdultMarkers`, `hasOnlyFansMarker` and `hasStandaloneSeasonToken` come from `ReleaseContext.php:125-170`. `categorizeForeign` and `catWebDL` come from `Options`.

The deviations, all deliberate:

- **R1. Byte regexes.** `regex::bytes::RegexBuilder` with `.unicode(false)`, so `\b`, `\d`, `\w` and `[^a-z0-9]` mean what they mean in PCRE without `/u`. `.case_insensitive(true)` only for a source regex with `/i`; the rest stay case sensitive, such as `/^[A-Z0-9]{15,}$/`.
- **R2. Lookahead.** `ReleaseContext.php:17` becomes `(?i)(?:^|[._ -])S\d{1,3}(?:$|[._ -])`. It is only ever a yes or no, so consuming the delimiter changes nothing.
- **R3. Non-ASCII quote.** `O['’]?Reilly` (`BookCategorizer.php:283` `$publishers`, `:353`) becomes `O(?:'|\xE2\x80\x99)?Reilly`.
- **R4. `Teen` removed** from the keyword regex in `hasAdultMarkersPhp` (`ReleaseContext.php:163`) and from the marker regex in XXX `checkUHD` (`XxxCategorizer.php:408`). `Teen.Drama.S01E02.1080p` would otherwise skip every video pipe. Studio names stay as listed.
- **R5. `teevee`.** The TV group regex (`GroupNameCategorizer.php:149`) gains it: `alt\.binaries\..*?(tv|teevee|hdtv|tvseries)`. Upstream's own group summary lists that group as TV (`:47`).
- **R6. `CBR` removed** from Book's music skip (`BookCategorizer.php:183`). With it, `\bCBR\b` skips the pipe for every `.cbr` comic before `checkComic` runs.
- **R7. Part 5 answers "is this name random?".** `misc_pipe` runs, in order:
  1. `crate::naming::obfuscation::is_obfuscated(name)`: 8020, confidence 900, rule `obfuscated_name`. This takes the place of `isBase64LikeToken`, `isUuidPattern`, `isPureHexString`, `isZeroVowelLongToken` as a rule, `isObfuscatedUppercaseString`, `isObfuscatedMixedAlphanumeric`, `isObfuscatedUsenetFilename`, `isRandomByCharacterAnalysis`, `hasInsufficientWordStructure`, `isRandomDigitPattern` and `checkLowSignal`. Keeping them would give Atlas two answers to one question, and two of them lock real names: `isRandomByCharacterAnalysis` sends `Plain.Name.Nothing.Else` to Hashed (13 of 55 table rows failed on it), and `isBase64LikeToken` does the same to `Some_Show_S01E02_720p_HDTV_x264-GRP`.
  2. `hash_md5`, `hash_sha1`, `hash_sha256` (`DetectsHashedNames.php:66-88`), 8020, 950. `hash_generic` (`:90-107`), 8020, 950, with `is_obfuscated(token)` in place of `looksLikeRandomString`. These add a hash inside a longer name: `is_obfuscated` doesn't judge a name with a space.
  3. `obfuscated_pattern` (`DetectsHashedNames.php:188-192`, `MiscCategorizer.php:310-325`), under R11. Hash-like is `lowSignal || isZeroVowelLongToken(core, 12)`: 8020 at 750, otherwise 8010 at 500.
  4. `archive`, `dataset`, `data_dump` (`MiscCategorizer.php:406-432`), 8010.

  `lowSignal` stays for the safety net and for step 3, with `hasStrongWordStructure` (`:389-394`) and its helpers.
- **R8. `4040`.** `PC_PHONE_OTHER` is 4040 in `Category.php` and 4999 in the seeder. The categorizer emits the constant, so Atlas lists 4040.
- **R9. No compiling per release.** In `looksLikeXxx` (`XxxCategorizer.php:256-278`) the `preg_quote` tests become: the studio regex's leftmost match starts at 0, the next byte is one of `.-_ `, and `^(?:[A-Za-z]+[.\-_ ])*(?:19|20)\d{2}[.\-_ ](?:720p|1080p|2160p|4K|UHD)` matches what follows it. The date branch at `:273` returns true either way and goes.
- **R10. Compile once.** Every regex is a field of one `LazyLock<Rules>` built by a `rules!` macro, so a bad pattern fails the first test that classifies anything, not a production release that reaches a rare branch.

```rust
fn ci(p: &str) -> Regex { build(p, true) }
fn cs(p: &str) -> Regex { build(p, false) }
fn build(p: &str, insensitive: bool) -> Regex {
    RegexBuilder::new(p).case_insensitive(insensitive).unicode(false).size_limit(32 << 20).build()
        .unwrap_or_else(|e| panic!("bad category regex {p}: {e}"))
}
macro_rules! rules {
    ($($field:ident = $kind:ident($pat:expr);)*) => {
        struct Rules { $($field: Regex,)* }
        static R: LazyLock<Rules> = LazyLock::new(|| Rules { $($field: $kind($pat),)* });
    };
}
```

- **R11. Punctuation rule gate.** `obfuscated_pattern` also requires `countAlphabeticWordTokens(name) < 2` (`MiscCategorizer.php:396-403`). Without it, `SOME.MOVIE.2019.1080P.BLURAY.X264-GRP` matches `^[^a-zA-Z]*[A-Z0-9._\-]{5,}[^a-zA-Z]*$` and locks to 8010. Upstream avoids this only because it rewrites dots to spaces first, and that rewrite would change the `.ext$` rules, so Atlas gates the rule instead.
- **R12. Image names.** A name matching `(?i)\.(jpe?g|png|gif|webp|bmp|tiff?)$`, Part 3's image regex, runs only Misc, the XXX pipe and the group hint `group_name_xxx`. `judge` no longer rejects image posts, and the overview's sample has 58 of 100 single images. Without this, `cover.jpg` in `alt.binaries.e-book` is 7020 and `Some.Movie.2019.1080p.jpg` is 2040. An image in an erotica group stays XXX rather than falling into Other.

## 5. Schema and migration

In each shard, registered through `schema::SHARD_TABLES`. Nothing in `atlas.db`.

```sql
create table if not exists release_category (
    release_id INTEGER PRIMARY KEY,
    category   INTEGER NOT NULL
);
create index if not exists category_release on release_category(category, release_id);
```

```rust
pub const RELEASE_CATEGORY: ShardTable = ShardTable {
    name: "release_category",
    create: "create table if not exists release_category (release_id INTEGER PRIMARY KEY, category INTEGER NOT NULL)",
    indexes: &[("category_release", "create index if not exists category_release on release_category(category, release_id)")],
    release_key: Some("release_id"),
    added: &[],
};
```

A row is written only when it says more than the default. A missing row already reads as 8000, so the step stores nothing for three kinds of decision:

| Rule | Decision | Why no row |
|---|---|---|
| `no_match` | 8000 | It equals the default |
| `hidden`, `unnamed` | 8020 | `VISIBLE_RELEASE` hides them from every API result, so the row would never be served. A real name re-runs the step |
| `obfuscated_name` | 8020 | The same once `atlas --reflag-obfuscated` runs. Until then it reads as Other, which is true |

```rust
pub fn stores(d: &Decision) -> bool { !matches!(d.rule, "no_match" | "hidden" | "unnamed" | "obfuscated_name") }
```

The writes, each a `StepWrite`:

```sql
insert or replace into release_category (release_id, category) values (?1, ?2)  -- stores(d)
delete from release_category where release_id = ?1                              -- !stores(d), and a row exists
```

The second is only sent after `select 1 from release_category where release_id = ?1` on `ctx.conn()` finds a row, so a rename from `Some.Show.S01E02` to something unrecognizable can't leave a stale category, and the 60–85M rows with nothing to store cost no write.

| Path | What happens | Test |
|---|---|---|
| Open | `schema::migrate` creates an empty table and index. No row is rewritten and nothing is indexed on `releases`. | Part 3's `an_old_shard_gets_the_new_tables_without_a_rewrite`, `no_index_is_built_on_an_existing_table` |
| `--compact` | The registry copies rows by name, drops the index for the copy, `build_shard` rebuilds it, and `check` counts the table. | Part 3's `every_registered_table_and_column_survives_compaction`, and `a_category_filter_works_after_a_compaction` here |
| `--convert` | Fresh shards have the empty table. The backlog walk fills it. | Part 3's `converted_shards_have_empty_stage_tables` iterates the registry |
| Wipe | The shard files go (`src/db.rs:174-201`), and the next `create_db` makes the table again. | Part 3's `a_wipe_leaves_new_empty_tables` |
| Delete and purge | `schema::migrate` generates the `gc_release_category` trigger from `release_key`. | Part 3's `deleting_a_release_clears_it_from_every_registered_table` |

## 6. Interfaces

### Provided

```rust
// src/category/table.rs
pub const UNCATEGORIZED: i64 = 8000;   // read-time only, never stored; what no_match decides
pub const MISC: i64 = 8010;
pub const HASHED: i64 = 8020;
pub struct Sub { pub id: i64, pub name: &'static str }
pub struct Root { pub id: i64, pub name: &'static str, pub subs: &'static [Sub] }
pub const TABLE: &[Root];
pub const fn root_of(id: i64) -> i64 { id - id.rem_euclid(1000) }
pub const fn is_tv(id: i64) -> bool { root_of(id) == 5000 }
pub const fn is_movie(id: i64) -> bool { root_of(id) == 2000 }
/// "TV/HD", "Other/Hashed". None for an id outside the table.
pub fn label(id: i64) -> Option<String>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatFilter { pub leaves: Vec<i64>, pub uncategorized: bool }
/// None: no filter. Some with nothing in it: matches nothing.
pub fn parse_filter(param: &str) -> Option<CatFilter>;
/// `<category id=".." name=".."><subcat id=".." name=".."/>...</category>` for each root.
pub fn caps_xml() -> String;

// src/category/rules.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options { pub foreign: bool, pub web_dl: bool }   // Default: true, false
pub struct Input<'a> { pub name: &'a str, pub has_display_name: bool, pub group: &'a str, pub poster: &'a str, pub is_obfuscated: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision { pub category: i64, pub confidence: u16, pub rule: &'static str }
pub fn classify(input: &Input<'_>, options: &Options) -> Decision;
/// Pure, no I/O, so Part 8's `Step::applies` can call it. `name` is coalesce(display_name, name).
pub fn classify_release(release: &ReleaseInfo, options: &Options) -> Decision;
/// False when a missing row already says it, section 5.
pub fn stores(decision: &Decision) -> bool;

// src/category/mod.rs
/// Per shard primary key reads. A release with no row is not in the map.
pub fn categories_of(ids: &[i64]) -> db::Result<HashMap<i64, i64>>;
pub fn categories_of_with(conn: &Connection, ids: &[i64]) -> db::Result<HashMap<i64, i64>>;
// ids grouped by store::shard_of_id, one statement per shard present:
//   select release_id, category from s{i}.release_category where release_id in (1,2,3)

// src/search.rs
pub struct CatPage { pub rows: Vec<ReleaseRow>, pub scanned: usize }
pub fn newest_in_categories(filter: &CatFilter, offset: i64, limit: i64) -> Result<CatPage>;
pub fn newest_in_categories_with(conn: &Connection, filter: &CatFilter, offset: i64, limit: i64, bounds: &Bounds) -> Result<CatPage>;
pub fn search_in_categories(query: &str, filter: &CatFilter, offset: i64, limit: i64) -> Result<Vec<ReleaseRow>>;
pub fn search_in_categories_with(conn: &Connection, query: &str, filter: &CatFilter, offset: i64, limit: i64, bounds: &Bounds) -> Result<Vec<ReleaseRow>>;
```

The step:

```rust
pub struct CategoryStep { options: RwLock<Options> }
impl Step for CategoryStep {
    fn id(&self) -> StepId { StepId(24) }
    fn name(&self) -> &'static str { "category" }
    fn rank(&self) -> u16 { 400 }
    fn needs(&self) -> Needs { Needs::Database }
    fn enabled(&self, cfg: &Config) -> bool {
        *self.options.write().unwrap() = cfg.category.options();
        cfg.category.enabled
    }
    // `parts` growing means a save may have set a name without a NameChange (src/store.rs:1369)
    fn reruns(&self) -> Reruns { Reruns { on_rename: true, on_growth: true } }
    fn run(&self, ctx: &mut StepCtx<'_>) -> Result<Outcome, StepError> {
        let id = ctx.release().id;
        let d = classify_release(ctx.release(), &self.options.read().unwrap());
        let writes = if stores(&d) {
            vec![StepWrite { sql: UPSERT, params: vec![id.into(), d.category.into()] }]
        } else if has_row(ctx.conn(), id).map_err(|e| StepError::Retry(e.to_string()))? {
            vec![StepWrite { sql: DELETE, params: vec![id.into()] }]
        } else {
            Vec::new()
        };
        Ok(Outcome {
            matched: stores(&d),
            writes,
            facts: vec![("category", d.category.to_string()), ("category_rule", d.rule.to_string())],
            ..Outcome::default()
        })
    }
}
```

Ids 25–31 and ranks 401–499 stay free. The `category` fact is the decided id as decimal and `category_rule` is the rule name, and both are set even when no row is stored. They reach later Database steps in the same run. A net-lane step never sees them, because facts live for one run: Part 8 calls `classify_release` there.

### Consumed

`Step`, `StepCtx`, `Outcome`, `StepWrite`, `Reruns`, `Needs`, `StepId`, `ReleaseInfo` (Part 3 `step.rs`); `ShardTable`, `SHARD_TABLES` (Part 3 `schema.rs`); `steps::all`. `naming::obfuscation::is_obfuscated` and the final name through `ReleaseInfo` (Part 5). `filters::register_sql`, in one test (Part 1).

### The filtered queries

`newest_in_categories` opens one cursor per leaf per shard, plus one per shard when `uncategorized` is set, and merges them lazily. Only rows that reach the top of the merge are read.

```rust
#[derive(Clone, Copy)]
pub struct Bounds {
    pub rejects: usize, // rows from leaf cursors a query may look at and reject
    pub no_row: i64,    // releases per shard the no-row cursor walks, at least
    pub fts: i64,       // newest matches per shard
    pub like: i64,      // newest releases per shard
}
impl Bounds { pub const DEFAULT: Bounds = Bounds { rejects: 2_000, no_row: 20_000, fts: 20_000, like: 100_000 }; }
pub const MAX_FILTER_OFFSET: i64 = 10_000;
```

```sql
-- leaf cursor, one per (shard, leaf). Plan: SEARCH c USING COVERING INDEX category_release (category=?)
-- There is no temp b-tree: the index walks backwards and each step is one index row.
select c.release_id from s{i}.release_category c where c.category = ?1 order by c.release_id desc

-- no-row cursor, one per shard when asked. Plan: CO-ROUTINE r / SCAN releases /
-- CORRELATED SCALAR SUBQUERY / SEARCH k USING INTEGER PRIMARY KEY.
-- ?1 is max(bounds.no_row, 4 * (offset + limit)).
select r.id from (select id, name, display_name, group_name, is_obfuscated from s{i}.releases order by id desc limit ?1) r
 where {VISIBLE_RELEASE}
   and not exists (select 1 from s{i}.release_category k where k.release_id = r.id)

-- probe, one prepared per shard. Plan: SEARCH r USING INTEGER PRIMARY KEY (rowid=?)
select {COLUMNS} from s{i}.releases r where r.id = ?1 and {VISIBLE_RELEASE}
```

```rust
pub fn newest_in_categories_with(conn: &Connection, f: &CatFilter, offset: i64, limit: i64, bounds: &Bounds) -> Result<CatPage> {
    if offset > MAX_FILTER_OFFSET || (f.leaves.is_empty() && !f.uncategorized) {
        return Ok(CatPage { rows: Vec::new(), scanned: 0 });
    }
    let want = (offset + limit) as usize;
    let no_row = bounds.no_row.max(4 * want as i64);
    let mut stmts: Vec<(rusqlite::Statement<'_>, i64)> = Vec::new();
    for s in 0..store::SHARDS {
        for &leaf in &f.leaves {
            stmts.push((conn.prepare(&leaf_cursor_sql(s))?, leaf));
        }
        if f.uncategorized {
            stmts.push((conn.prepare(&no_row_cursor_sql(s))?, no_row));
        }
    }
    let mut cursors = Vec::with_capacity(stmts.len());
    for (stmt, value) in stmts.iter_mut() {
        cursors.push(stmt.query([*value])?);
    }
    let mut heap = std::collections::BinaryHeap::new();
    for (i, c) in cursors.iter_mut().enumerate() {
        if let Some(row) = c.next()? { heap.push((row.get::<_, i64>(0)?, i)); }
    }
    let mut probes = (0..store::SHARDS).map(|s| conn.prepare(&probe_sql(s))).collect::<Result<Vec<_>>>()?;
    let (mut rows, mut seen, mut scanned, mut rejected, mut last) = (Vec::new(), 0usize, 0usize, 0usize, None);
    while let Some((id, i)) = heap.pop() {
        if let Some(row) = cursors[i].next()? { heap.push((row.get::<_, i64>(0)?, i)); }
        if last == Some(id) { continue; }  // a row written between two cursors' reads
        last = Some(id);
        scanned += 1;
        // the no-row cursor filters on VISIBLE_RELEASE itself, so only leaf rows get rejected here
        let Some(hit) = probes[store::shard_of_id(id)].query_row([id], release_row).optional()? else {
            rejected += 1;
            if rejected > bounds.rejects { break; }
            continue;
        };
        seen += 1;
        if seen > offset as usize {
            rows.push(hit);
            if rows.len() == limit as usize { break; }
        }
    }
    Ok(CatPage { rows, scanned })
}
```

With `q`, the existing rank order stays, over the newest `bounds.fts` matches per shard. The query goes first; on an error the LIKE form runs, as `fts_or_like_rows` does (`src/search.rs:76`):

```sql
select * from (
  -- one per shard, union all. `order by rowid desc` is native to FTS5 and needs no sorter.
  -- Plan: CO-ROUTINE ranked / SCAN releases_fts VIRTUAL TABLE / SCAN ranked /
  --       SEARCH r USING INTEGER PRIMARY KEY / SEARCH c USING INTEGER PRIMARY KEY LEFT-JOIN
  select {COLUMNS}, ranked.rank as rank
    from (select rowid, bm25(releases_fts) as rank from s{i}.releases_fts
           where releases_fts match ?1 order by rowid desc limit ?2) ranked
    cross join s{i}.releases r on r.id = ranked.rowid
    left join s{i}.release_category c on c.release_id = r.id
   where {VISIBLE_RELEASE} and {cat_clause}
) order by rank limit ?3 offset ?4
```

The LIKE form, used only when FTS rejects the query. `?2` is `bounds.like`:

```sql
select * from (
  select {COLUMNS}, r.name as sort_name
    from (select * from s{i}.releases order by id desc limit ?2) r
    left join s{i}.release_category c on c.release_id = r.id
   where (r.name like ?1 escape '\' or r.display_name like ?1 escape '\')
     and {VISIBLE_RELEASE} and {cat_clause}
) order by sort_name limit ?3 offset ?4
```

```rust
fn cat_clause(f: &CatFilter) -> String {
    // ids come from TABLE, never from the request, so inlining them is safe
    let list = f.leaves.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    match (list.is_empty(), f.uncategorized) {
        (true, false) => "0".into(),
        (true, true) => "c.release_id is null".into(),
        (false, false) => format!("c.category in ({list})"),
        (false, true) => format!("(c.category in ({list}) or c.release_id is null)"),
    }
}
```

## 7. Config

A `category` object in `config.json`, parsed leniently like `from_value` (`src/config.rs:264`). An absent object means all defaults. No key is written back unless the user adds it; `save_config` keeps it (`:447-450`). No key changes network load.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Run the step. Off: no new rows, and existing rows stay and are served. |
| `foreign` | `true` | Foreign rules on. Upstream's default. A new value applies to releases processed after it; `atlas --reprocess --step category` applies it to old ones. |
| `web_dl` | `false` | WEB-DL goes to 5010 and 2080. Off: 5040 and 2040, the HD categories clients search by default. Upstream's default. Same note on old releases. |

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryConfig { pub enabled: bool, pub foreign: bool, pub web_dl: bool }
impl CategoryConfig { pub fn options(&self) -> Options; }
```

## 8. Tasks

Each task: write the test first and see it fail for the stated reason, implement, run, commit. Append whatever attribution the executing session requires to commit messages. All tests use `tempfile::tempdir()`, and no fixture holds a name from the live database.

### Task 1: The table and the `cat` parser

**Files:** create `src/category/mod.rs`, `src/category/table.rs`; modify `src/lib.rs`.

- [ ] **Tests first**, in `table.rs`:
  - `every_id_is_unique_and_under_its_root`, `no_sub_is_a_root`, `the_table_has_70_leaves`.
  - `the_ids_clients_rely_on_are_there`: 2040, 2045, 2999, 5030, 5040, 5045, 5999, 7020, 8010, 8020.
  - `a_parent_expands_to_its_own_subs`: `parse_filter("2000")` gives exactly the ten Movies ids, not including `2000` itself. `parse_filter("5000,5040")` has no duplicates.
  - `eight_thousand_includes_rows_with_no_category`: `parse_filter("8000")` has `uncategorized: true`, 8010 and 8020. `parse_filter("8010")` has `uncategorized: false`. `parse_filter("7000")` has `uncategorized: false` and the six Books ids, and `caps_xml` calls 7000 `Books` and 8000 `Other`.
  - `parse_filter_table`: `""`, `"abc"`, `"-1"`, `"5000,-1"`, `" , "` give `None`. `"9999"` gives `Some` with no leaves and no `uncategorized`. `"abc,5040"` gives only 5040. `" 5030 , 5040 "` gives both. `"2020"` gives 2999 and `"5050"` gives 5999. A list of every root gives `None`.
  - `labels`: `label(5040)` is `TV/HD`, `label(8020)` is `Other/Hashed`, `label(5000)` is `TV`, `label(9999)` is `None`.
  - `caps_xml_lists_every_root_and_sub`, parsed by string search: `<category id="5000" name="TV">` and `<subcat id="5040" name="HD"/>`.
- [ ] **Implement** section 4's table and rules.
- [ ] **Run:** `cargo test --lib category::table`. **Expected:** pass. Before: no module.
- [ ] **Commit:** `Add the Newznab category table and the cat parameter parser`

### Task 2: Lists and the hidden and hash rules

**Files:** create `scripts/extract_category_lists.py`, `data/category_lists.tsv`, `src/category/lists.rs`, `src/category/rules.rs`; test in `rules.rs` and `src/search.rs`.

The script, committed. It reads upstream as text and is run once with `SRC` at the pinned commit, as `00-overview.md` says:

```python
#!/usr/bin/env python3
"""Writes data/category_lists.tsv from a newznab-tmux checkout. Reads text only. GPLv3, see README credits."""
import pathlib, re, sys

src = pathlib.Path(sys.argv[1]) / "app/Services/Categorization/Categorizers"
WANTED = {
    "XxxCategorizer.php": ["JACQUIE_ET_MICHEL", "KNOWN_STUDIOS", "ADULT_KEYWORDS", "VR_SITES", "GENERIC_VR_SITE", "VR_DEVICES"],
    "PcCategorizer.php": ["PC_GROUPS", "PC_KEYWORDS"],
    "MusicCategorizer.php": ["FOREIGN_LANGUAGES", "LANGUAGE_CODES"],
    "TvCategorizer.php": ["SPORTS_MARKER_REGEX", "SPORTS_EVENT_CONTEXT_REGEX", "ANIME_GROUPS_REGEX"],
}
rows = {}
for name, consts in WANTED.items():
    text = (src / name).read_text(encoding="utf-8")
    for const in consts:
        m = re.search(r"const (?:string )?" + const + r" = '((?:[^'\\]|\\.)*)'", text)
        value = m.group(1).replace("\\'", "'")
        m2 = re.fullmatch(r"/(.*)/i", value, re.S)
        rows[const] = m2.group(1) if m2 else value
# the PHP constant ends `|'.self::JACQUIE_ET_MICHEL`, which the match above stops short of
assert rows["KNOWN_STUDIOS"].endswith("|")
rows["KNOWN_STUDIOS"] += rows["JACQUIE_ET_MICHEL"]
out = pathlib.Path("data/category_lists.tsv")
out.write_text("# generated by scripts/extract_category_lists.py\n" + "".join(f"{k}\t{v}\n" for k, v in rows.items()), encoding="utf-8")
```

- [ ] **Tests first:**
  - In `lists.rs`: `every_list_is_there_and_has_no_empty_alternative`: each of the 13 names loads; none starts or ends with `|` or holds `||`. An empty alternative makes `\b(...)\b` match any word, which is how a bad extraction shows up. `KNOWN_STUDIOS` holds `Jacquie`.
  - In `rules.rs`: `an_obfuscated_or_unnamed_release_stays_hashed`, using `classify` with group `alt.binaries.teevee`:

| name | display name | `is_obfuscated` | category | rule |
|---|---|---:|---:|---|
| (empty) | no | false | 8020 | `unnamed` |
| `   ` | no | false | 8020 | `unnamed` |
| `Some.Show.S01E02.720p` | no | true | 8020 | `hidden` |
| `abcdefghij0123456789` | no | false | 8020 | `hidden` |
| `d41d8cd98f00b204e9800998ecf8427e` | no | false | 8020 | `hidden` |

  A name with a display name isn't hidden whatever the flag says; Task 3's `a_display_name_clears_hidden` checks the category it gets.

  - `misc_rules`, with `misc_pipe` directly: `Xq7.Zr2.Tk9.Wp4.Vn6.Ly8`, `q7Zr2Tk9-Wp4Vn6Ly8.part01.rar`, `kP93xQ7rL.ZMtwBfa`, `123e4567-e89b-12d3-a456-426614174000` and `aGVsbG8+d29ybGQ/Zm9vYmFyYmF6cXV4==` are 8020 with `obfuscated_name`. `Some Name - d41d8cd98f00b204e9800998ecf8427e` is 8020 with `hash_md5`, and `[abc] 7fG2kP9qLm3XwZ8vN4bR6tY1.rar` is 8020 with `hash_generic`. `2019-01-01_0001` is 8010 with `obfuscated_pattern`. `files.part01.rar` and `Some.Archive.Name.7z` are 8010 with `archive`, `backup_2019_full.sql` is 8010 with `dataset`. `Plain.Name.Nothing.Else`, `IMG_20190101_120000.jpg`, `SOME.MOVIE.2019.1080P.BLURAY.X264-GRP` and `Some_Show_S01E02_720p_HDTV_x264-GRP` return no hit (R7, R11).
  - `stores_table`: `no_match`, `hidden`, `unnamed` and `obfuscated_name` are false; `hash_md5`, `archive`, `hd_resolution` and `group_name_tv` are true.
  - In `src/search.rs` tests: `the_hidden_rule_matches_visible_release`: for 12 names and flags, `rules::is_hidden(name, display, flag)` equals `not VISIBLE_RELEASE` evaluated by SQLite on an in-memory `releases` table with the same row. The connection calls `filters::register_sql(&conn)` and each row has a `group_name` that isn't blocked, because Part 1's `VISIBLE_RELEASE` reads `atlas_blocked(r.group_name)`. `is_hidden` ignores the group on purpose: `rules::judge` rejects a blocked group before any step. Names include a 15 and a 16 character alnum name, a name with a dot, and one with `é`. Fails first: no function.
- [ ] **Implement:** `Rules`, `is_hidden`, `stores`, `misc_pipe` per R7 and R11, its helpers, and `classify` with the Misc pipe only.
- [ ] **Run:** `cargo test --lib category && cargo test --lib search`. **Expected:** pass.
- [ ] **Commit:** `Use Part 5's check for random names, and port the Misc rules it doesn't cover`

### Task 3: The pipes

**Files:** `src/category/rules.rs`.

- [ ] **Test first:** `the_rules_table`, every row with `Options { foreign: true, web_dl: false }`, an empty poster and `is_obfuscated: false`. Fails first: only Misc exists.

| Name | Group `alt.binaries.` | Category |
|---|---|---:|
| `Some.Show.S01E02.720p.HDTV.x264-GRP` | `teevee` | 5040 |
| `Some.Show.S01E02.2160p.WEB.H265-GRP` | `teevee` | 5045 |
| `Some.Show.S01E02.WEB-DL.AAC2.0.H.264-GRP` | `tv` | 5040 |
| `Some.Show.S01E02.HDTV.XviD-GRP` | `tv` | 5030 |
| `Some.Show.S02.1080p.BluRay.x264-GRP` | `tv` | 5040 |
| `Some.Show.S02.COMPLETE.GERMAN.DUBBED.1080p.WEB-GRP` | `tv` | 5020 |
| `Some.Show.S03E04.German.720p.WEB.x264-GRP` | `tv` | 5020 |
| `Some.Docs.S01E01.Documentary.1080p.WEB.x264-GRP` | `tv` | 5080 |
| `[SubsPlease] Some Show - 08 (1080p) [ABCD1234].mkv` | `anime` | 5070 |
| `Some Show - 08 [1080p]` | `multimedia.anime` | 5070 |
| `NFL.2024.Week.03.Team.vs.Team.1080p.WEB.h264-GRP` | `sports` | 5060 |
| `Some.Daily.Show.2024.03.15.Guest.Name.720p.HDTV.x264-GRP` | `teevee` | 5040 |
| `Some.Show.S01E02.1080p.mkv` | `misc` | 5040 |
| `Teen.Drama.S01E02.1080p.WEB-DL.x264-GRP` | `teevee` | 5040 |
| `SOME.SHOW.S01E02.720P.HDTV.X264-GRP` | `teevee` | 5040 |
| `NFL.2024.WEEK.03.1080P.WEB.H264-GRP` | `sports` | 5060 |
| `Some_Show_S01E02_720p_HDTV_x264-GRP` | `teevee` | 5040 |
| `Some-Show-S01E02-720p-HDTV-x264-GRP` | `teevee` | 5040 |
| `Some.Movie.2019.1080p.BluRay.x264-GRP` | `movies` | 2040 |
| `Some.Movie.2019.2160p.UHD.BluRay.x265-GRP` | `movies` | 2045 |
| `Some.Movie.2019.COMPLETE.BLURAY-GRP` | `bluray` | 2060 |
| `Some.Movie.2019.BD50.Untouched-GRP` | `bluray` | 2060 |
| `Some.Movie.2019.3D.1080p.BluRay.SBS.x264-GRP` | `movies` | 2050 |
| `Some.Movie.2019.DVDRip.XviD-GRP` | `movies` | 2030 |
| `Some.Movie.2019.DVDR-GRP` | `dvd` | 2070 |
| `Some.Movie.2019.CAM.XviD-GRP` | `movies` | 2030 |
| `Some.Movie.2019.GERMAN.1080p.BluRay.x264-GRP` | `movies` | 2010 |
| `Some.Movie.2019.1080p.WEB-DL.DD5.1.H264-GRP` | `movies` | 2040 |
| `Wicked.2024.1080p.WEB-DL.DD5.1.x264-GRP` | `movies` | 2040 |
| `SOME.MOVIE.2019.1080P.BLURAY.X264-GRP` | `movies` | 2040 |
| `Artist-Album-2019-GRP` | `mp3` | 3010 |
| `Artist - Album (2019) [FLAC]` | `sounds.flac` | 3040 |
| `Artist-Album-WEB-2019-320kbps-MP3` | `sounds.mp3` | 3010 |
| `Artist-Album-WEB-320kbps-MP3` | `sounds.mp3` | 3010 |
| `Some.Author.Book.Name.Unabridged.Audiobook.M4B` | `audiobooks` | 3030 |
| `Some.Podcast.Episode.123` | `sounds` | 3050 |
| `Some Concert Live 2019 1080p BluRay x264-GRP` | `music.videos` | 3020 |
| `Some Game-CODEX` | `games` | 4050 |
| `Some.Game.Update.1-FLT` | `games` | 4050 |
| `Some.App.v1.2.3.Incl.Keygen-GRP` | `warez` | 4010 |
| `Some.App.v2.MacOSX-GRP` | `mac` | 4030 |
| `Some.Tool.Android-GRP` | `warez` | 4070 |
| `Some.Game.PS4-DUPLEX` | `games` | 1180 |
| `Some.Game.XBOX360-GRP` | `games.xbox360` | 1050 |
| `Some.Game.USA.NSW-GRP` | `games` | 1999 |
| `Some.Book.Title.2020.RETAIL.EPUB-GRP` | `ebook` | 7020 |
| `Some Book Title (2020).epub` | `e-book` | 7020 |
| `Some Comic 012 (2020) (Digital).cbr` | `comics` | 7030 |
| `Learning Rust, 2nd Edition - OReilly.pdf` | `e-book` | 7040 |
| `Forbes.USA.March.2024.PDF` | `magazines` | 7010 |
| `Some.Title.XXX.1080p.WEB.x264-GRP` | `erotica` | 6041 |
| `Some.Title.XXX.2160p.MP4-GRP` | `erotica` | 6045 |
| `Some.Title.XXX.DVDRip.x264-GRP` | `erotica` | 6040 |
| `Some.Title.XXX.IMAGESET-GRP` | `erotica` | 6060 |
| `Some.Title.XXX.IMAGESET.jpg` | `erotica` | 6060 |
| `photo_001.jpg` | `pictures.erotica` | 6999 |
| `Some.Movie.2019.1080p.jpg` | `movies` | 8000 |
| `cover.jpg` | `e-book` | 8000 |
| `cover.jpg` | `anime` | 8000 |
| `IMG_20190101_120000.jpg` | `pictures` | 8000 |
| `holiday_photos_2019` | `pictures` | 8000 |
| `Plain Name Nothing Else` | `misc` | 8000 |
| `Plain Name Nothing Else` | `teevee` | 5999 |
| `Plain Name Nothing Else` | `movies` | 2999 |
| `Plain Name Nothing Else` | `mp3` | 3999 |
| `Plain Name Nothing Else` | `warez` | 4010 |
| `Plain Name Nothing Else` | `e-book` | 7020 |
| `Kw3Rt7Yp9Zs2` | `teevee` | 8020 |
| `mountainlake` | `teevee` | 8010 |

- [ ] **Tests first**, also:
  - `a_display_name_clears_hidden`: `Some.Show.S01E02.720p`, display name yes, `is_obfuscated: true`, group `alt.binaries.teevee`: 5040 with `hd_resolution`.
  - `options_change_the_sub_not_the_root`: with `web_dl: true`, `Some.Show.S01E02.WEB-DL.AAC2.0.H.264-GRP` is 5010 and `Some.Movie.2019.1080p.WEB-DL.DD5.1.H264-GRP` is 2080. With `foreign: false`, `Some.Show.S03E04.German.720p.WEB.x264-GRP` is 5040. For every table row and all four option values, `root_of` of the result is the same.
  - `a_poster_can_decide`: `Some.Show.08.mkv` in `alt.binaries.misc` with poster `anon <x@animetosho.org>` is 5070.
  - `a_group_hint_loses_to_a_name_and_never_beats_a_hash`: conf 600 from `alt.binaries.teevee` loses to `Some.Movie.2019.1080p.BluRay.x264-GRP` at 850, and `d41d8cd98f00b204e9800998ecf8427e` stays 8020.
  - `only_leaves_are_stored`: every decision over the table with `stores(&d)` true is a sub in `TABLE`.
  - `#[ignore] classify_is_cheap`: 200,000 calls over the table's names on one thread in release. Prints µs per call for the PR description and asserts nothing, as the overview says of benchmarks. The prototype took 4–21 µs.
- [ ] **Implement** the pipes as section 4 says, each with its skip and gate, in source order. Each `check*` is a function returning `Option<Decision>`.
- [ ] **Run:** `cargo test --lib category && cargo test --release --lib classify_is_cheap -- --ignored --nocapture`. **Expected:** pass. If a table row fails, read the source lines of the rule that fired: the table was checked against a prototype with the same rules, so a failure means the port differs from the source. Do not edit a row to pass.
- [ ] **Commit:** `Port the category rules for TV, movies, audio, books, PC, console and XXX`

### Task 4: The table, the step and config

**Files:** create `src/category/step.rs`; modify `src/category/mod.rs`, `src/postprocess/schema.rs`, `src/postprocess/steps.rs`, `src/config.rs`, `config.example.json`, `tests/archive_stage.rs`, `tests/naming_stage.rs`, `tests/predb_stage.rs`; test `tests/category_stage.rs` (no env; start the stage as `tests/pp_stage.rs` does, with this step in its list, settle 1 s).

- [ ] **Tests first:**
  - In `src/config.rs`: `category_defaults_when_absent`, `category_keys_parse`, `a_bad_category_value_falls_back`. Fails first: no field.
  - In `src/postprocess/steps.rs`: Part 3's `steps_have_unique_ids_ranks_names_within_their_ranges` passes with the step added.
  - `a_settled_release_gets_its_category`: save a complete `Some.Movie.2019.1080p.BluRay.x264-GRP` of 2 MiB in `alt.binaries.movies`. Its `release_category` row reads 2040 and `pp_state.done` has bit 24. Fails first: no row.
  - `a_rename_reruns_the_step`: a release named with a hash shape has no row, and the `category` fact is 8020. A test step at rank 250 returns a `NameChange` to `Some.Show.S01E02.720p.HDTV.x264-GRP` on its second run. The row reads 5040.
  - `a_rename_to_nothing_recognizable_deletes_the_row`: the other way: 5040, then a `NameChange` to `Plain Name Nothing Else` in `alt.binaries.misc`. No row, and the API reports 8000.
  - `a_no_match_or_hidden_release_writes_no_row`: `Plain Name Nothing Else` and a hash shape: no row, with `pp_state.done` bit 24 set.
  - `a_small_release_and_an_image_post_reach_the_step`: a 10 KB `Some.Book.Title.epub` gets 7020, a 700 KB `cover.jpg` runs the step and writes no row. This holds Part 3's `judge` to its revision.
  - `a_name_from_a_late_save_reruns_it`: save with no `display_name`, then a later article with `display_name: Some("Some.Show.S01E02.720p.HDTV.x264-GRP")`, then let it settle. The row follows. Fails if `on_growth` is false.
  - `the_fact_reaches_a_later_step`: a recording test step at rank 450 sees `ctx.fact("category") == Some("2040")`.
  - `a_rejected_release_gets_no_row`: a release whose files are all par2, under `reject_par2_only`. No row, no `pp_state` row, and the API reports 8000.
  - `the_step_does_no_network`: the stage over a mock server with the default config sends no BODY: `bodies_sent == 0`.
  - `category_off_writes_nothing`: `"category": {"enabled": false}`. No row, and the release is otherwise processed.
- [ ] **Test first** in `src/compact.rs`, on `legacy_fixture`: `a_category_filter_works_after_a_compaction`: insert a row for a visible release, `run`, then `newest_in_categories_with` finds it and `category_release` exists. Fails first: no table.
- [ ] **Implement** sections 5, 6 and 7. Add the `category` block to `config.example.json`, and `category.enabled: false` to the three harnesses section 4 names.
- [ ] **Run:** `cargo test --test category_stage && cargo test --lib config && cargo test --lib compact && cargo test --test pp_schema && cargo test --test convert && cargo test --test archive_stage && cargo test --test naming_stage && cargo test --test predb_stage`. **Expected:** pass, and Part 3's registry tests pick the table up with no change to them.
- [ ] **Commit:** `Store a category per release through the processing stage`

### Task 5: Filtered reads, bounded

**Files:** `src/search.rs`, `src/category/mod.rs`; tests in `src/search.rs`, on a fixture of real shards under a tempdir, using `db::create_db_at` and `db::open_with_shards`.

- [ ] **Tests first:**
  - `the_leaf_cursor_needs_no_sort`: `explain query plan` of the leaf cursor holds `USING COVERING INDEX category_release` and not `TEMP B-TREE`. The no-row cursor holds `SEARCH k USING INTEGER PRIMARY KEY`. Fails first: the functions building them don't exist.
  - `newest_first_across_shards_with_exact_paging`: 40 releases across 4 groups in different shards, half in 5040 and half in 2040. `cat=5040`, `limit=7`, `offset=3` gives the 4th to 10th newest 5040 ids, in id order descending. Pages join with no gap and no repeat.
  - `a_parent_merges_its_leaves`: rows in 5030, 5040, 5999; the `5000` filter gives all three in id order.
  - `a_rare_category_is_found_past_many_others`: 5,000 rows in 5040 and 3 in 7020, scattered. The `7020` filter gives the 3, with `scanned <= 3`. This is the point of the index: it fails if the code walks `releases`.
  - `no_row_means_eight_thousand`: rows with no category match the `8000` filter merged with 8010 rows in id order, and do not match `5000` or `7000`.
  - `hidden_rows_are_skipped_and_the_scan_is_bounded`: 2,500 hidden rows in 8020 plus one visible row in 8020, older than them all. The result is empty and `scanned` is `Bounds::DEFAULT.rejects + 1`. With 100 hidden rows the visible one is found. A hidden row gets a category row only if it was flagged after being categorized, so the fixture writes them directly.
  - `the_no_row_cursor_prepares_with_the_whole_visibility_rule`: on an empty shard, `no_row_cursor_sql` with `VISIBLE_RELEASE` as Part 1 left it prepares and returns nothing; a blocked group's row with no category is left out.
  - `the_no_row_cursor_is_bounded`: 500 categorized rows on top of one uncategorized row, in one shard. `Bounds { no_row: 100, ..Bounds::DEFAULT }` with `limit` 10: empty. `Bounds::DEFAULT`: found.
  - `an_offset_past_the_limit_is_empty`: `offset = MAX_FILTER_OFFSET + 1`, no query run.
  - `an_empty_filter_matches_nothing`.
  - `search_in_categories_filters_ranks_and_keeps_no_row_rows`: `q` plus `5040`; `q` plus `8000` includes a no-row match; `q` plus `7000` doesn't; a hidden match never shows.
  - `search_in_categories_looks_at_the_newest_matches_only`: 300 matches in one shard, `Bounds { fts: 100, ..Bounds::DEFAULT }`: an old row in the right category is not returned, a new one is.
  - `the_like_fallback_filters_too`: drop `releases_fts` from every shard of the fixture, so the FTS form errors. `search_in_categories_with` still answers and still honours `cat`.
  - `categories_of_reads_each_shard`: ids across 8 shards, one with no row: the map has the others. Empty input does no query.
- [ ] **Implement** section 6.
- [ ] **Run:** `cargo test --lib search && cargo test --lib category`. **Expected:** pass.
- [ ] **Commit:** `Filter by category newest first, with bounded scans`

### Task 6: The API

**Files:** `src/api.rs`; test `tests/category_api.rs` (one test, env set first, as `tests/e2e.rs`).

- [ ] **Tests first**, in `src/api.rs`:
  - `caps_lists_real_categories`: `<category id="5000" name="TV">`, `<subcat id="5040" name="HD"/>`, `<category id="7000" name="Books">` and `<category id="8000" name="Other">`. Also `cat` in every `supportedParams`.
  - `rss_reports_the_category_and_defaults_to_8000`: two `ReleaseRow`s, one id in the map with 5040: `value="5040"` on one and `value="8000"` on the other, both asserted as raw strings (`r#"name="category" value="8000""#`), so section 11's grep can't match the test. Fails first: `rss` takes no map.
- [ ] **Test first:** `tests/category_api.rs`, `the_cat_filter_end_to_end`. Set `ATLAS_HOME` and `ATLAS_NO_KEYRING`, save 7 releases with hand-written names, and set rows by direct SQL on their shards: movie 2040, TV 5040, TV 5030, book 7020, visible no-row, hidden no-row, Other 8010. Through `api::handle`:

| Query | Result |
|---|---|
| `t=search` | every visible release, newest first, as today |
| `t=search&cat=2000` | the movie, `value="2040"` |
| `t=search&cat=5040` | TV HD only |
| `t=search&cat=5000` | both TV, newest first |
| `t=search&cat=5030,5040` | both TV |
| `t=tvsearch&cat=2040` | the movie: `cat` decides, not `t` |
| `t=search&cat=9999` | no items, `<newznab:response offset="0" total="0"/>` |
| `t=search&cat=abc` | as `t=search` |
| `t=search&cat=-1` | as `t=search` |
| `t=search&cat=7000` | book `value="7020"` only |
| `t=search&cat=7020` | book only |
| `t=search&cat=8000` | the 8010 row and the visible no-row `value="8000"`, not the hidden one |
| `t=search&cat=8010` | the 8010 row only |
| `t=search&q=WORD&cat=5040` | that TV release only |
| `t=search&cat=5000&limit=1&offset=1` | the older TV release |
| `t=search&limit=1` | the newest visible release, its `value` from its row, or `8000` with none |

  Fails first: `cat` is ignored.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --lib api && cargo test --test category_api && cargo test --test e2e`. **Expected:** pass. `e2e` shows search and `t=get` unchanged.
- [ ] **Commit:** `Serve real categories in caps and results, and honour cat`

### Task 7: Terminal search and the dashboard

**Files:** `src/app.rs`, `src/stats_dashboard.rs`.

- [ ] **Tests first:**
  - In `src/app.rs`: `category_cell_shows_a_label_or_a_dash`: 5040 gives `TV/HD`, no row gives `-`.
  - In `src/stats_dashboard.rs`: extend `pages_render` (`:1471`) with `categories: vec![(5000, 4)]` in the fixture `Content` and `2 => ... && text.contains("no category yet") && text.contains("TV")`. Fails first: no panel. `old_cache_still_loads` (`:1459`) stays as it is and `c.categories` is empty.
  - `content_categories_sums_by_root`: `content_categories(conn) -> rusqlite::Result<Vec<(i64, i64)>>` on a tempdir database through `db::open_with_shards`, because `compute_content` reads `ATLAS_HOME`: rows in 5030, 5040 and 2040 give `[(2000, 1), (5000, 2)]`. Fails first: no function.
- [ ] **Implement** section 4.
- [ ] **Run:** `cargo test --lib stats_dashboard && cargo test --lib app`. **Expected:** pass.
- [ ] **Commit:** `Show categories in search results and on the Content page`

### Task 8: Docs and full verification

**Files:** `README.md`, `config.example.json`; test in `src/config.rs`.

- [ ] **Test first:** `the_docs_describe_categories`, on `include_str!("../README.md")` and `include_str!("../config.example.json")`: the README holds `### Categories`, `cat=8000` and `7000` as Books, and no longer holds `Every release is in category` or `Atlas reports every release as category`. The example parses, and `Config::from_value` gives its `category` values. Fails first: the old lines are there.
- [ ] **Implement** section 12.
- [ ] **Run:** `just format-check && just lint && just test`, then section 11. **Expected:** all pass.
- [ ] **Commit:** `Document categories`

## 9. Performance and load

No network and no article bodies. Numbers are measured on the prototype or derived, as marked. None come from live data.

**What reaches the step.** Part 3's `judge` no longer rejects by size or image type, so about 100M of the 118M rows run this step, not about 16M. What's still rejected, and every row until the walk or its settle time reaches it, reads as 8000.

**The step.** Classify: 4–21 µs a release on one thread (measured on the prototype, with image names at the cheap end because they skip most pipes). `classify_is_cheap` prints the figure for the PR. A stored row costs about 10 µs on the shard's writer (derived from Part 3's 266 µs per article at peak and two b-tree inserts), and a decision with nothing to store costs a primary key read of under 5 µs and no write. At the walk's 2,000 rows/s, about 1,700 runs/s: 4% of a core at the measured top end, and 9% if a real name mix reaches 50 µs.

**Whether to store the default.** A row for every run would be about 100M rows, 2.5 GB and 17 minutes of writer time, and most would say what a missing row says or describe a release the API never shows. So nothing is stored for those, section 5. Stored rows can't be counted without live data. These cases bracket it:

| Rows with a stored category | Size at 25 B a row | Share of 300 GB | Writer time in the walk |
|---:|---:|---:|---:|
| 15M: names that say something, roughly the old estimate | 375 MB | 0.13% | 2.5 min |
| 40M: every multi-article release and some single ones | 1 GB | 0.33% | 7 min |
| 100M: the ceiling, every row reaching the step | 2.5 GB | 0.83% | 17 min |

Group hints decide where in that range it lands: a release in a hinted group is stored whatever its name says, unless it's an image (R12). Part 3 accounts for its own `pp_state` row per run. The sizes are compacted sizes: the first-start walk fills this table newest first, which leaves its pages about half full (Part 3 measures about double for `pp_state`), so until the next `--compact` plan on about twice these figures.

**The existing 118M.** Part 3's single first-start walk reaches them newest first:

| Phase | Rate | Time at 118M |
|---|---|---|
| Walk and judge (`backlog_per_second` 2,000) | 2,000 rows/s | 16.4 h |
| Same, at `backlog_per_second` 8,000 | 8,000 rows/s | 4.1 h, about 6,800 runs/s, with classify at 14–34% of a core across the DB lane's threads |
| Rows classified | About 85% of rows looked at | About 100M |

Newest rows come first, so what clients search most is categorized in the first hour: 7.2M rows looked at, about 6.1M classified. The walk pauses whenever `writer_queued` exceeds `2 * SHARDS`.

**What reads as 8000.** A missing row means any of: not reached yet, still settling (a new complete release takes `complete_settle_secs`, 15 min, an incomplete one `settle_secs`, 2 h), rejected by `judge`, no rule matched, or a name the API hides. That's probably most rows. They all live under `cat=8000`, so no other category is diluted: `cat=7000` and `cat=7020` hold only what the rules or a book group's hint put there, with no default rows and no images (R12). A hint can still file a non-book posted to a book group, at confidence 500, the lowest of any rule.

**Filtered reads at 118M.** A shard holds 14.8M releases and at most about 12.5M category rows. The plans below were read from `EXPLAIN QUERY PLAN`; "hot" is measured on 8 shards of 1M rows with 13% categorized and 12% hidden, best of 3, on a laptop's NVMe.

| Query | Cursors | Rows probed | Hot | Cold, derived |
|---|---:|---:|---:|---:|
| `cat=5000` | 80 | 100 | 0.6 ms | 40 ms |
| `cat=5040` | 8 | 100 | 0.2 ms | 25 ms |
| `cat=7020`, 0.1% of rows | 8 | 100 | 0.2 ms | 25 ms |
| `cat=7000` | 48 | 100 | 0.4 ms | 30 ms |
| `cat=2040,5040` | 16 | 100 | 0.2 ms | 25 ms |
| `cat=5000&offset=2900`, Sonarr's last page | 80 | 3,000 | 2.6 ms | 600 ms |
| `cat=8000`, 2 leaves and the no-row cursor | 24 | 114 | 0.4 ms | 30 ms |
| `cat=8000&offset=2900` | 24 | 3,475 | 3.4 ms | 700 ms |
| 18 leaves | 144 | 100 | 0.9 ms | 50 ms |
| `cat=8020`, all hidden: the bound | 8 | 2,001 | 1.5 ms | 400 ms |
| Unfiltered, today's path, for comparison | | | 0.7 ms | |

"Cold" takes a random read at 100 µs and 2 uncached pages per seek or probe. Going from 1M to 14.8M rows a shard adds at most one b-tree level, one page per seek.

Why this shape and not the obvious ones:

- `order by c.release_id desc` with `category in (...)` makes SQLite read every matching row into a sorter, because an `IN` over an index is not ordered by the next column. At 5000 that is millions of rows per query. One cursor per leaf has no sorter at all.
- Walking `releases` newest first with a `release_category` probe is linear in the rows skipped. A category holding 0.1% of rows skips 1,000 rows per hit, so 100,000 probes per page.
- A SQL `union all` of one per-leaf subquery per shard reads `limit + offset` rows from each, 80 × 3,000 at Sonarr's last page, and hits SQLite's 500 compound terms at 63 leaves across 8 shards. The lazy merge reads `limit + offset` in all, and `prepare` is the only per-cursor cost, about 30 µs.

**Every scan is bounded:**

| Scan | Bound | Reached when |
|---|---|---|
| Leaf rows rejected per request | 2,000, so the answer is "nothing recent" | A category holding rows hidden after they were categorized. Those are rare, since hidden rows aren't stored |
| No-row cursor per shard | `max(20,000, 4 × (offset + limit))` releases | Nearly every recent row has a category |
| `offset` | 10,000; past it, an empty page | A client pages far, Sonarr and Radarr stop at 2,900 |
| FTS matches per shard, with `cat` | 20,000 newest | A common word. Measured at 1M matches in one shard: 84 ms bounded, 1,090 ms for today's unbounded shape |
| LIKE fallback per shard, with `cat` | 100,000 newest | FTS rejects the query. Measured at 1M rows: 198 ms |
| Attribute lookup | 100 ids, one primary key read each | Every result page |

Without `cat`, every path is exactly as it is today.

**Memory.** Per request: one statement per cursor, at most 560 with every leaf, about 2 KB each, and 100 rows. `Rules` builds lazily in the indexer and in any process that classifies: the whole prototype peaked at 13 MB. The API process never builds it.

## 10. Failure handling

| Case | What happens |
|---|---|
| A name that isn't UTF-8 clean | `ReleaseInfo.name` is a `String`, and bytes regexes take it as is. No panic path: slices come from `str` only. |
| A name of 1 MB | `regex` is linear in input. The step finishes within the 120 s deadline. |
| A bad regex in the port | `Rules` panics at first build, so the first test fails. In production a panic would be caught per release by the engine and retried, then the failed bit. |
| Part 5's check flags a real name | The release stops at `obfuscated_name`: no row, reads 8000. Part 5 measured 0.019% of dictionary words flagged. A later rename re-runs the step. |
| `insert` fails | Savepoint rollback for that release, `db_attempts + 1`, backoff, per Part 3. The release reports what it had, or 8000, in the meantime. |
| Crash during a batch | Nothing was written, the queue rows remain, and the write is idempotent. |
| Rename while a step runs | The later run wins: on_rename cleared the bit, and it writes or deletes the row. |
| Rules change between versions | Old rows keep their value until `atlas --reprocess --step category`. |
| `categories_of` fails | `search` logs it and uses an empty map: results show 8000 and the search still answers. |
| A filtered query fails | Error 900, `Search failed`, as `src/api.rs:199`. |
| `cat` with garbage | Dropped, see `parse_filter`. Never an error, since clients send odd values. |
| A category id later removed from `TABLE` | Its rows match no filter and `label` returns `None`, until a reprocess. `rss` still reports the stored number. |
| Stage disabled, or `category.enabled` false | No new rows. The API works, and new releases report 8000. |
| Compaction | The stage is stopped and the table is copied. |
| Re-run | Idempotent. |

## 11. Acceptance checks

Run in the part's worktree. None touch live data or a live server.

```bash
just format-check && just lint && just test
# expected: all pass, no warnings

cargo test --release --lib classify_is_cheap -- --ignored --nocapture
# expected: prints microseconds per call; record it in the PR description, not a gate

cargo test --lib category::rules::tests::the_rules_table
# expected: pass, every row of the table in Task 3

cargo test --lib search::tests::a_rare_category_is_found_past_many_others
# expected: pass, scanned <= 3

cargo test --lib search::tests::hidden_rows_are_skipped_and_the_scan_is_bounded
# expected: pass

cargo test --test category_stage the_step_does_no_network
# expected: pass, bodies_sent == 0

cargo test --test category_api
# expected: pass, including no-row releases at 8000 and 7000 holding only books

! grep -n 'category\\" value=\\"[0-9]' src/api.rs
# expected: no output: non-test code takes the number from category::UNCATEGORIZED, and tests use raw strings

cargo test --lib api::tests::caps_lists_real_categories
# expected: pass, every root and sub in t=caps
```

## 12. Docs

- `README.md`, Features (`:52`): real categories.
- "Top-level keys" (`:127`): a `category` row and a new table.
- "Operations" (`:383-384`) stays. "Parameters" (`:398`): `cat` takes a comma list, a parent includes its subs, and an unknown id matches nothing.
- New `### Categories` under "Newznab API": the table from section 4; how a release gets one; that it arrives after the release settles, 15 min if complete and 2 h otherwise; that a release with no stored category reports 8000 and matches `cat=8000`, which covers everything not yet reached, rejected, unrecognized or hidden; that **7000 is Books, not Other as Atlas reported it before**, so a saved Other (7000) in a client must become 8000; that `cat=8020` holds only visible names with a hash in them, as other hashed names are hidden or stay under 8000; that `foreign` and `web_dl` change only what is processed next.
- "Add Atlas to Prowlarr" (`:424`): replace `Other (7000)` with the categories to pick, for example `TV (5000)`, `Movies (2000)`, and `Other (8000)` to see everything else.
- "Search results" (`:435`): `<newznab:attr name="category" value="5040"/>`.
- "Limitations" (`:513`): remove the line.
- "Stats dashboard", "Content" (`:326`): the categories panel. "Search" (`:283`): the column.
- "Credits" (`:516`): newznab-tmux, GPLv3, for the category rules and lists.
- `config.example.json`: the block.

## 13. Decisions

1. **A release with no row reports 8000, and 7000 stays Books.** The coordinator decided this after the first draft flagged that upstream's seeders, its spec (`docs/newznab_api_specification.txt:274`) and Prowlarr's standard list all make 7000 Books (`app/Models/Category.php:209`), and that Atlas's `name="Other"` at `src/api.rs:31` is the older layout. `UNCATEGORIZED` is one read-time constant, so nothing stored depends on it. A client configured with Other (7000) must change to 8000, and the README says so. Moving Books to 8000 instead was rejected: Prowlarr maps by id, so Readarr asking for `7020` would get Hashed.
2. **Other is 8000, 8010, 8020, not upstream's 1, 10, 20.** Upstream's seeder and its own spec disagree. The spec reserves `0000-0999` and lists 8000 and 8010; Prowlarr maps 8000, 8010 and 8020. `root_of` arithmetic also holds. Only these three ids differ from upstream's constants.
3. **Other leaf numbers stay upstream's even where Prowlarr's standard differs:** 2999 against 2020, 5999 against 5050, 2060 BluRay against 2050. The brief asks for upstream's numbering. Five aliases in `parse_filter` make a standard "other" id find the same rows. The 2050/2060 swap and 3050 have no safe alias, as each number means something else here.
4. **All nine categorizers are ported, console included.** The brief lists "PC/games", and upstream's console pipe is 150 lines of simple rules. Dropping it would send console releases to `PC/Games` or Other.
5. **Port the `*Php` methods, not `rust/pipeline-ffi/src/categorize.rs`.** They are the readable statement of the rules. The Rust file exists to match PCRE byte for byte across 57K lines, which Part 3's overview says to leave. 243 of 244 regexes compiled unchanged, so nothing needs translating by hand except R2 and R3.
6. **Hand-written Rust, with only long lists in `data/`.** Rules combine regexes with `&&`, `!` and nesting, and a table format would be a small language to maintain. The lists are data, generated by a committed script, as the overview requires for regex data.
7. **`regex::bytes` with Unicode off.** The rules were written for PCRE without `/u`, so this keeps their meaning. Non-ASCII titles match on their ASCII tokens. Part 8's title parser uses Unicode because it extracts text.
8. **Hidden releases are decided first, match `VISIBLE_RELEASE` exactly, and aren't stored.** The brief says obfuscated and unnamed releases stay Other/hashed: the decision and the `category` fact say 8020, and the API can't show them anyway, so the row adds nothing and they count under Other on the dashboard. This also keeps hidden rows out of the category index, so walking any category meets almost none. A group hint cannot override it, and a real name from Part 5 clears it, because `has_display_name` is checked and the rename re-runs the step.
9. **`on_growth` as well as `on_rename`.** `src/store.rs:1369` lets a save set `display_name` with no `NameChange` while `name_source` is NULL, as the inline par2 and nfo lookups do. Without it those names never reclassify.
10. **Rules run on the final name, with no subject or file evidence.** The brief defines the final name as `coalesce(display_name, name)`. Part 5 already feeds file evidence into that name.
11. **One cursor per leaf, merged lazily in Rust.** Section 9 has the alternatives and their costs. It costs 40 lines and no new index beyond the pinned one.
12. **No filter is applied by `t=`.** See Non-goals. A client that wants TV sends `cat=5000`, and Sonarr and Radarr always do.
13. **`cat=9999` returns nothing, and `cat=abc` returns everything.** A valid-looking id the table lacks must not widen a search. A token that isn't a number is noise, and upstream ignores it too.
14. **Bounds return a short page, not an error.** A client asking for a category of hidden rows gets nothing, which is correct.
15. **`offset` past 10,000 is empty on filtered paths.** Sonarr and Radarr stop at 30 pages of 100 (`MaxPages`, `PageSize` in their `NewznabRequestGenerator`). Past that the cost is all rejected work.
16. **The FTS scan keeps rank order over the newest 20,000 matches per shard.** Sorting by newest would change what `q` returns for clients that expect relevance. The bound means an old exact match can lose to a newer one under a very common word only, which also made it cheaper by a factor of 13 in the prototype.
17. **`ReleaseRow` is unchanged, and attributes load in a second query.** It has four construction sites (`src/search.rs:42`, `src/convert.rs:641`, `src/nzb.rs:157`, `tests/convert.rs:102`), and `rss` is the only consumer of the category. 100 primary key reads cost under 1 ms.
18. **Table name `release_category`, as pinned.** Part 3 suggests a `category_` prefix per part. No test enforces prefixes, Part 3 section 5 uses this shape as its example, and the brief pins the name. The index is `category_release`. This is the one name that departs from Part 3's prefix list.
19. **The terminal shows `-` for no row, and the API shows 8000.** A person reading a table is better told nothing is known; a client protocol needs a number.
20. **Category on by default.** No network and no bodies, so hard rule 8 is met, and every client benefits at once.
21. **`foreign` on and `web_dl` off, upstream's defaults.** Sonarr's defaults search 5030 and 5040, so WEB-DL under 5010 would vanish from them.
22. **A config change does not re-run old rows.** `atlas --reprocess --step category` does, with the dry run Part 3 gives it. A silent 16 h walk on a config edit would surprise the user.
23. **A decision that says nothing more than the default gets no row.** Part 3's revision sends about 100M rows through the step. Storing every one is 2.5 GB and 17 minutes of writer time to record `no_match`, which a missing row already reads as, and names the API hides. The cost: `cat=8020` no longer lists every hashed name, only visible names holding a hash. Nothing else could list them, as `VISIBLE_RELEASE` hides the rest.
24. **`misc_pipe` asks Part 5.** Two random-name detectors would disagree on the same name, and Part 5's was measured against 170,415 words and 20,000 random strings. The rules kept are the ones it doesn't cover: a hash inside a longer name, the punctuation rule, archive and dataset labels, and `lowSignal` for the group safety net. Part 7 already depends on Part 5.
25. **The `obfuscated_pattern` gate rather than upstream's name rewrite.** Rewriting `.` and `_` to spaces, as `ObfuscatedSubjectExtractor` does, would change every `.ext$` rule and all their table rows. Counting word tokens is the helper upstream itself uses for this distinction.
26. **Image names skip all but XXX.** `judge` stopped rejecting them, and they are most of the sample. A jpg is not the movie its name mentions, and hints exist to place unnamed content, not cover art. Keeping them out also makes 58% of rows the cheapest case.
27. **2,000 rejected rows, not 20,000.** Leaf cursors arrive newest first, so 2,000 hidden rows in a row means nothing recent, and the bound drops a repeatable worst case from 4 s to 400 ms cold (review A). The no-row cursor has its own bound, because it filters on visibility in SQL.
