# Atlas map: posting-tool fingerprints (catalog lines 639-665, ranks 6 and 8)

Repo /home/claude/appz4fun/atlas, main at 55d2e51 (2026-11-02). Read only; no cargo run.
Scope: the catalog's posting-tool table, the Date-string cluster note (catalog-current.md:663-665),
derived_per_article.{message_id_grammar,message_id_timestamp,from_identity,date_shape} (catalog 473-484),
encoding_quirks.{segment_size,control_line_syntax} (368-379), ybegin.line (325-327),
yenc_additions.{encoder_tells,encoder_fingerprint_tuple} (420-423, 452-455),
headers_poster_tool.X-Newsreader/X-Newsposter/X-Mailer (229-231), nntp_additions.server_assigned_message_ids (282-285),
nntp_session.group_metadata and transfer_facts (43-46, 58-60), ranked plan rows 6 and 8 (1155, 1157).

Status vocabulary used here (one rule, applied to every row): `shown` only when the item's OWN value is printed
on the release page or a panel it loads; a raw string that merely contains the value does not make a derived
fact shown. `stored_not_shown`: the value (or the bytes it is a pure function of) sits in SQLite and nothing
prints it. `parsed_not_stored`: the value is in memory at ingest or in the probe and dropped. `not_collected`:
nothing in the code computes or keeps it.

## 0. One-line verdict

Atlas has NO posting-tool classifier and NO encoder fingerprint. The only tool fact it keeps is the raw agent
header string (`release_probe.agent`, probe/mod.rs:28, :107-109), shown as "Posted with" on the release page
(web.rs:1291-1293) and searchable by exact match (field_search.rs:196-198, :215-217). Everything else in the
tool table is either (a) derivable from stored rows with no fetch (message-id string from `segments.local` +
`domains.suffix`, segment and last-article wire bytes from `segments.bytes`, one raw Date/Injection-Date/From,
yEnc line length, total, name, segment_size for probed releases), (b) held in memory by the probe and dropped
(pcrc32 hex case and padding, `=yend part=`, `=ypart size=`, escapes, header order and casing, headers outside
KEPT_HEADERS, trailing space after name=), or (c) never reachable without new fetches or a schema change
(per-article Date and From, `:lines`, article numbers).

## 1. Files read (all of src that the task named, plus what they call)

src/store.rs (schema 123-197, 200-245; alphabet tags 311-331; to_bytes/from_bytes 334-366; pack_local 373-404;
unpack_local 398-416; repack 419-421; split_message_id 425-432; Domains 437-510 incl. SHARED_AFTER 452 and
whole() 455-460; dedup_key 512-518; decode 520-526; add_segment 645-705; IdKey 713-733; index_release 737-765;
SEAL_AGE 788, SEAL_MAX_SEGMENTS 793, READ_MAX_SEGMENTS 797; sealed_segments 815-822; seal_file/seal_rows 826-900;
seg_key 927-936; upsert 1375-1460; split_old_rows 1595-1640; FileInfo/SegmentRef/SegmentPick 1760-1787;
release_files 1836; Picked/file_segments 1803-1835; articles 1938-1943; each_article 1947-2010; tests 2246-2256),
src/blob.rs (whole file: format 1-5, Seg 12-19, encode 56-71, resolved/is_remote 74-84, decode 87-89, count 92-102,
MAX_LOCAL 107, decode_capped 119-129, parse 131-158), src/probe/yenc.rs (whole: YencLines 7-18, value 21-35, hex 37-39,
parse 46-73, RawArticle 76-79, split_article 88-117), src/probe/mod.rs (whole: PROBE 23-47, SET_PROBE 49-50,
ProbeInfo 52-78, KEPT_HEADERS 81-94, keep_headers 96-104, agent_of 107-109, xref_groups 112-114, derive 118-140,
read 142-148), src/probe/step.rs (whole: config_stamp 99-101, run 102-212, FRONT_ARTICLES 217),
src/nntp.rs (Overview 91-100, into_article 102-116, article_raw 614-634, stat 637-640, xover 738-756, body 758-775,
BackgroundRead 1065-1072, plausible_post_time 1189-1195, Ends 255-289, background_read 1750-1804, parse_overview
2712-2728, yenc_decode 2744-2772), src/dates.rs (whole: parse_rfc2822 9-76, parse_zone 78-109, to_iso_date 132-137,
pub_date 148-163, posted_timestamp 173-185), src/release_info.rs (Contents 39-60, contents_of 160-181, Poster/parse_poster
437-465, MADE_UP_DOMAINS 467-492, looks_made_up 494-501), src/filters.rs (register_sql 376-379, PER_ARTICLE_MIN/TOKEN_LEN
384-389, SUBJECT_RE/COUNTER_RE 391-395, has_token/shape 403-425, gather_per_article 520-579), src/collections.rs
(subject_base 76-88, file_counter 91-94), src/field_search.rs (whole), src/groups_menu.rs (whole), src/metrics/report.rs
(whole), src/metrics/server.rs (whole), src/metrics/names.rs (55-80), src/metrics/mod.rs (1-40), src/metrics/catalog.rs
(1-30), src/web.rs (routes 47-72, tag 694-696, search_link_to 800-802, Search::from_args 295-315, More fields 891-900,
release_html 1028-1197, panel 1234-1240, probe_html 1243-1452, copy_card 1468-1530), src/parser.rs (1-70),
src/postprocess/step.rs (whole), src/postprocess/schema.rs (whole), src/postprocess/steps.rs (whole),
src/postprocess/engine.rs (seed 285-305, walk_tick 412-470, seed_shard 1073-1115, WALK_BATCH 28, ROW_COLUMNS 91-94),
src/postprocess/reprocess.rs (whole), src/postprocess/bodies.rs (refusal 84-100, fetch 124-164), src/archive/steps.rs
(1-80: first_article 38-41, tail_articles 44-53), src/archive/mod.rs (1-60: ARTICLE_CAP 28), src/api.rs (release_attrs
285-310, rss 313-345), src/nzb.rs (60-130), src/duplicates.rs (1-120), src/compact.rs (455-510), src/sealed_store.rs (1-60),
src/indexer.rs (on_writer 450-458, save 464-470, WriterJob 224-231, 1780-1787), src/config.rs (596-660), src/search.rs
(ReleaseRow 9-18, ArticleRow 22-31, COLUMNS 33-34), src/ui.rs (fmt_date 531-533), src/db.rs (open 22-23, attach 94-102),
src/stats_dashboard.rs (grep only), src/ai.rs, src/profile.rs, src/atomic.rs, src/lib.rs, src/probe/lzma.rs (headers),
tests/probe_bg.rs (95-140), tests/web_ui.rs (grep), docs/port/03-postprocess-stage.md (grep), docs/superpowers/specs/
2026-10-05-faster-backfill-smaller-storage-design.md (7, 64-82, 86-137), README.md (326-349, 396-410), PR branches
remotes/pr/48, 51, 54, 55, 57, 58 and origin/feat/release-stills-strip (diff --stat and the relevant hunks).

Files the task asked to clear in one line each (no release-page fields):
- src/ai.rs: "find me 4k hdr movies" -> groups + keywords through a local Ollama model for the TUI (ai.rs:1, imports
  11-19); it searches and downloads, it renders no release page and writes no release column.
- src/profile.rs: indexing-time counters behind ATLAS_PROFILE=1 (profile.rs:1-2, counters 33-40); the one-writer note at
  :68 and :126 is the only fact of interest ("one writer per shard, their times add up").
- src/atomic.rs: atomic_replace / tmp_path / write_atomic for files on disk (atomic.rs:11, 26, 33); no data model.
- src/lib.rs: the module list only (lib.rs:3-48).
- src/probe/lzma.rs: an LZMA/LZMA2 front decoder for cut-off streams (lzma.rs:1-3, Props 7, End 26); payload, no fields.
- src/groups_menu.rs and src/metrics/{report,server}.rs: see sections 8 and 9; TUI cache and Prometheus only.

## 2. Data model (who writes, who reads)

### 2.1 Per article: `segments` and `files.blob` (shard tables, store.rs:181-188; blob.rs:1-5)

```
domains  (id INTEGER PRIMARY KEY, suffix TEXT NOT NULL UNIQUE)                       store.rs:180
segments (file_id, local BLOB, domain INTEGER, part INTEGER, bytes INTEGER)
         PRIMARY KEY (file_id, local, domain) WITHOUT ROWID                           store.rs:181-188
files    (id, release_id, filename, subject, subject_part, subject_mid, expected,
          file_total, seen BLOB, touched_at, blob BLOB)                               store.rs:162-178, 200-245
held_back(file_id, message_id TEXT, subject, part, total_parts, file_total)          store.rs:211-219
release_ids (release_id, id_hash, file_id)                                           store.rs:225-231
```

- Writer: `add_segment` (store.rs:645-705) inserts `(file_id, local, domain, part, bytes)` only (689-691): the
  article's Date, From, :lines, References and article number that `Article` carried (parser.rs:12-27, filled by
  `Overview::into_article` nntp.rs:102-116) are dropped here. `part` comes from the SUBJECT parse
  (parser.rs ParsedSubject 29-37), never from yEnc.
- `local` is `pack_local(local_part)` (373-404) and `domain` is the `domains.id` of the suffix `@domain>` (split_message_id
  425-432 keeps the '@' and '>' in the suffix), or `domain = 0` with the WHOLE `<...>` id stored as TEXT (whole(),
  455-460) when the writer has seen the suffix fewer than SHARED_AFTER = 3 times (452, 480-490). The per-writer caches
  are bounded at 100,000 and cleared when full (450, 481-483, 497-500), so a popular suffix can still be stored whole
  in some rows; add_segment dedups across both forms (677-687).
- Alphabet tags, the first byte of `local` (store.rs:311-319): 0 TEXT, 1 HEX_LOWER, 2 HEX_UPPER, 3 DIGITS, 4 BASE36_LOWER,
  5 BASE36_UPPER, 6 BASE62, 7 BASE64_URL. Rules (373-404): hex only when the length is even (375-384); otherwise the
  FIRST alphabet in the order DIGITS, BASE36_LOWER, BASE36_UPPER, BASE62, BASE64_URL that covers every byte, for lengths
  1..=255, packed as `[tag][len][base-N big number]` (386-397); anything else `[0][bytes]` (399-403). The repo's own
  test (store.rs:2246-2256) shows the classes: ngPost 32 hex -> 1; JBinUp `hotfTpetaZRIbOYuTuQ31` -> 6; a Nyuu local
  WITHOUT the hyphen `ZjPsQyLgOjNtQvXbHeKaXyCm1730295651235` -> 6; `nnd$009a5634$43be2fd7` (an INN/Highwinds style id
  with '$') -> 0; odd-length hex `abc` -> 4. A real Nyuu local `([A-Z][a-z]){12}-\d{13}` carries '-', so it packs as 7
  (BASE64_URL alphabet is `0-9A-Za-z-_`, 326). A Giganews base64 id with '+' or '/', a PowerPost `part1of45.<rand>` or a
  newsmangler `epoch.5dp.part` with '.', and every Highwinds `T.N$K.R` id fall to TEXT. Legacy rows from before alphabet
  packing hold the same locals as TEXT (add_segment's legacy check 680-687; `--compact` re-encodes, README.md:349).
  => The tag is a size optimisation, not a class: it separates "even-length hex" from "letters/digits" from "has
  punctuation"; it does not separate Nyuu from JBinUp from postie from a Usenet.Farm server id.
- Sealing: a file complete, or untouched for SEAL_AGE = 3 days (store.rs:788), is sealed into `files.blob`
  (seal_file 826-835, seal_rows 836-900), at most SEAL_MAX_SEGMENTS = 50,000 segments (793), locals <= MAX_LOCAL 512
  (blob.rs:107). Blob v1 (blob.rs:1-5, VERSION 1 at :9, parse 131-158): zstd level 6 over `u8 version, varint count,
  per segment: varint part+1, zigzag varint bytes delta, varint domain, varint local len, local bytes`. It holds nothing
  but part, bytes, domain and local: no room for a tool, a Date, or a From. `parse` rejects any other version (132-134).
  With `sealed_log.offload` on, blobs of 1 KiB or more of files untouched for a day move to the sealed-log service and
  `files.blob` holds a Locator instead (sealed_store.rs:1-7, 25-37; blob.rs:74-84 `resolved` fetches it; README.md:330).

### 2.2 Per release: `releases` (store.rs:133-145) and `release_probe` (probe/mod.rs:23-47)

```
releases (id, name, group_name, poster TEXT, posted_date TEXT, size, complete, parts, file_total,
          display_name, is_obfuscated, name_source)                                   store.rs:133-145; schema.rs:96
release_probe (release_id PK, crc32 INTEGER, agent TEXT, info TEXT NOT NULL, moviehash TEXT [added])
   indexes: release_probe_crc32 (where crc32 is not null), release_probe_agent (where agent is not null),
            release_probe_moviehash                                                     probe/mod.rs:23-47
```

- `releases.poster` and `posted_date`: the From and the ISO wall-clock Date of the FIRST article of the slice
  (indexer.rs:1785-1786), with the zone dropped by `to_iso_date` (nntp.rs:108; dates.rs:132-137) and overwritten by every
  later save of the same (name, group) (upsert `on conflict ... do update set poster = excluded.poster, posted_date =
  excluded.posted_date`, store.rs:1392-1397). In `split_old_rows` the poster is the first article's author again
  (1624). Read by: release_html facts (web.rs:1067, 1079-1086), copy_card (1497-1504), the API `poster` attr and pubDate
  (api.rs:296, 341), the NZB file poster/date (nzb.rs:85-87 via `ArticleRow.poster/posted_date` joined from releases in
  each_article, store.rs:1954-1971), the stage's ReleaseInfo (postprocess/step.rs:74-75; engine ROW_COLUMNS 91).
- `release_probe` is written only by ProbeStep via SET_PROBE (probe/mod.rs:49-50; step.rs:200-207) as `insert or
  replace` of all five columns. `agent = agent_of(info.headers)` (step.rs:201; mod.rs:107-109: User-Agent, then
  X-Newsreader, then X-Newsposter). `info` is the whole `ProbeInfo` as JSON (step.rs:203). Read by `probe::read`
  (mod.rs:142-148, whole row deserialised) for the page (web.rs:1163-1169), by the field search for `crc32` and `agent`
  (field_search.rs:191-198, 215-217), by tests (tests/probe_bg.rs:112-137: agent "Nyuu/0.4.2").
- `ProbeInfo` (probe/mod.rs:52-78): file, yenc: YencLines {name, file_size, line_len, part, total, begin, end, yend_size,
  pcrc32, crc32} (yenc.rs:7-18), file_crc32, segment_size, overhead_percent, continuity_ok, part_size_ok, part_crc_ok,
  health {first, middle, last}, headers BTreeMap<String,String>, crossposted_to, archive, layers, media, nfo, imdb_ids,
  sfv, moviehash. All of it is about ONE file (main_file, step.rs:40-46: most parts, first volume by name) and the
  headers are from its FIRST article only (step.rs:140-147); the last article's control lines are parsed (178) and
  used by derive (182) then dropped except what derive copies (file_crc32, continuity).
- `KEPT_HEADERS` (mod.rs:81-94): Injection-Date, NNTP-Posting-Date, Date, User-Agent, X-Newsreader, X-Newsposter, Path,
  Injection-Info, Organization, Xref, NNTP-Posting-Host, X-Complaints-To; values cut to 512 chars (100); names
  normalised to this spelling (99-100); arrival order lost in the BTreeMap (97). Not kept: Message-ID, From, Subject,
  Newsgroups, Lines, X-Mailer, X-Trace, X-Nxg, X-Ufk, X-Ufhash, X-Received-*, Content-Type, and every other header
  `split_article` read (yenc.rs:88-117, MAX_HEADERS 256).
- Probe coverage: ProbeStep is `Needs::Bodies` (step.rs:83-85), enabled only with `archive.enabled` (86-89), skipped for
  `.jpg` releases (90-92), and its fetches obey the body gates (bodies.rs:84-100: group blocked, `body_min_size_bytes`
  1 MiB default / `body_max_size_bytes`, image names, `bodies_per_release` 8; config.rs:629-638). README.md:405 puts
  this at "about 15 million of 118 million releases pass the body gates". So at most ~15M releases ever get a
  `release_probe` row; the other ~103M have only segments/files/releases rows.

### 2.3 What the probe holds in memory and drops (the parsed_not_stored set)

In `ProbeStep::run` (step.rs:140-160) the first article is `article_raw` (nntp.rs:614-634: ARTICLE, undecoded, dot-
unstuffed, LF-joined) split by `yenc::split_article` into `headers: Vec<(String, String)>` in arrival order and
`lines: Vec<Vec<u8>>` (yenc.rs:76-79, 88-117). From there: `keep_headers` keeps 12 names (mod.rs:96-104), `yenc::parse`
reads the control lines (yenc.rs:46-73) and `yenc_decode` (nntp.rs:2744-2772) decodes the body and records nothing.
Dropped on the way:
- pcrc32/crc32 hex case and zero padding: `hex()` parses to u32 (yenc.rs:37-39).
- `=yend part=`: not parsed at all (yenc.rs:65-69 reads size, pcrc32, crc32 from =yend; `part` comes from =ybegin, :59).
- `=ypart size=` (nxg-upper): not parsed (yenc.rs:61-64 reads begin and end only).
- Trailing space after `name=` (yEnc32): `.trim()` (yenc.rs:54).
- Keyword order, spacing, `size=` before or after `part=`: `value()` finds each key anywhere (yenc.rs:21-35).
- `total=` absent (Rust yenc crate): PARSED and STORED as `info.yenc.total = null` (yenc.rs:60) for probed releases.
- `crc32=` absent on the last part (ngPost, PowerPost 2004, NewsUP): stored as `file_crc32 = null` (mod.rs:139).
- Escapes (TAB/SPACE at line edges, '.' at column 0, 0x7F, `=` at line end), an empty line before `=yend`, line length
  consistency: all in `article.lines`, nothing records them; yenc_decode strips them (nntp.rs:2756-2770).
- Header order, header name casing (Farm's `Message-Id`), LF-only headers (nxg-upper): in `article.headers`, lost at
  keep_headers (mod.rs:97-101; split_article trims and splits on '\n' only, yenc.rs:92, 107-110).
- Headers outside KEPT_HEADERS (X-Mailer, X-Trace, X-Nxg, X-Ufk/X-Ufhash, X-Received-Body-CRC/Bytes, the Message-ID
  header itself, From, Newsgroups, Subject, Lines, Content-Type): read, dropped.
- The last article's `YencLines` (begin/end/yend_size/pcrc32 of the tail): parsed (step.rs:178), used by derive and
  last_part (182-183, 253-260), dropped.
- At ingest, per article (parser.rs:12-27): `author`, `date` (ISO), `references`, `lines`, `number` exist in memory for
  every article of a slice; `gather_per_article` even counts (subject shape, author) pairs to decide whether a From is
  random per article (filters.rs:544-552, `lasting`, 555-556) and then stores "" as the poster for once-seen Froms; none
  of that reaches a table.

## 3. From stored rows back to a message-id string, and the cost

- Rows: `each_article` (store.rs:1947-1975) runs one query joining `files f` x `segments s` x `releases r` with
  `left join domains d on d.id = s.domain where f.release_id = ?` (1953-1958) and builds the id with
  `decode(local, suffix)` (520-526): `suffix = None` (domain 0) -> `unpack_local(local)` gives the whole `<...>`
  string; `Some(suffix)` -> `format!("<{}{suffix}", unpack_local(local))`. `unpack_local` (398-416) reverses each
  packing: hex bytes -> `{:02x}`/`{:02X}`, tags 3-7 -> `from_bytes` (353-366, O(len^2) small-int arithmetic), tag 0 -> the
  bytes as UTF-8. `dedup_key` (512-518) is the same string, or `\0domain\0hex` when a domain has no suffix row.
- Sealed: the second statement in each_article (1977-2008) reads `f.blob` per file, `blob::decode` (blob.rs:87-89:
  `resolved` then `zstd::stream::decode_all` then `parse`) and looks each `seg.domain` up once per blob with a cached
  `select suffix from domains where id = ?` (1983-1995). A step uses `file_segments` (store.rs:1803-1835): the whole
  blob is decoded with `decode_capped(READ_MAX_SEGMENTS = 1,000,000)` (815-822) even for `SegmentPick::First(1)`
  (1813-1817); `Picked` then keeps only what was asked (1778-1800). `SegmentRef` carries `message_id` (decoded),
  `part`, `bytes` (1771-1775).
- Cost per release: one indexed read of the release's segment rows (PK (file_id, local, domain); `files_key
  (release_id, filename)` store.rs:178) plus one zstd decode per sealed file touched. The storage spec measured 15-27
  bytes per article in a blob (design spec :137) and a file is <= 50,000 segments (store.rs:793), so a main-file blob is
  tens to hundreds of KB and decodes in well under a millisecond to a few ms. A tool classifier needs ONE id
  (First(1)) plus the tail bytes (Last(2), archive/steps.rs:44-53) of the main file, so per release: 1 blob decode (or a
  few-row scan) + 1-2 domain lookups + regex work on one string. With the sealed log offloaded, each blob of a settled
  file is a network fetch to the service (blob.rs:74-79; sealed_store.rs:3-7, 25-37) and the service must be up.
- Nothing prints a message-id on any UI page (grep `message_id` in src/web.rs: no hits); they leave Atlas only in NZBs
  (nzb.rs:101-106, `trim_matches(['<','>'])`).

## 4. UI surface today

- Release page `release_html` (web.rs:1028-1197), rendered whole on GET /ui/release (routes 47-72; only the title
  details load over htmx, 1189). Facts (1048-1134): Category, Title, Size, **Posted** (`fmt_date` = first 10 chars of
  `posted_date`, ui.rs:531-533; web.rs:1067), Group(s) (1068-1078), **Poster** (parse_poster name -> search link
  `poster=<raw From>`, 1079-1081), **Email** + `MADE UP` tag from looks_made_up (1082-1085), Posted as (1087-1090), Name
  from, Year, Ripped from, Site, Video, Audio, Languages, Edition, Release group, Parts, Encryption, Password, Files,
  PAR2 files, Articles, Repair, Links. Then `{copies}{probe}{files}` (1194-1196).
- `probe_html` (1243-1452), called with `probe::read` (1163-1169): panels in order Tracks, Chapters, **yEnc**, Packing,
  **How it was posted**, then NFO and SFV (1445-1451; `panel` 1234-1240 drops an empty one).
  - yEnc panel (1252-1288): File name, File size, **Parts** (= `yenc.total`), **Line length** (`yenc.line_len`,
    :1264), **Article size** (`segment_size`, :1265), CRC32 (search link, :1266-1268), Overhead %, Checks (continuity,
    part size, part CRC verdicts, 1270-1276), On the server (health tags, 1277-1288).
  - How it was posted (1290-1302): **Posted with** = `agent_of(headers)` as a search link on `agent` (1291-1293);
    then every kept header except User-Agent, X-Newsreader, X-Newsposter and Xref, name and value verbatim in BTreeMap
    (alphabetical) order (1294-1298): Date, Injection-Date, Injection-Info, NNTP-Posting-Date, NNTP-Posting-Host,
    Organization, Path, X-Complaints-To; then Crossposted to (1299-1302). A second agent header (X-Newsposter next to
    User-Agent) is stored in `info.headers` but never printed.
- Copy cards (`copy_card`, 1468-1530) repeat Poster/Email per copy (1497-1504).
- Search "More fields" (web.rs:891-900; `Fields` field_search.rs:22-32): group, poster, email, size, team, crc,
  **agent ("Posted with")**. Plan: `r.poster = ?` exact (168), `@email:` LIKE (170-171), `@probe:agent:` exact
  `r.id in (select release_id from s{s}.release_probe where agent = ?)` (196-198, 215-217), indexed (255-261).
- API (api.rs:285-310): `poster`, `group`, resolution, video, audio, hdr, source, language, team, twins, alts; no agent,
  no tool, no id domain. NZB (nzb.rs:60-120): poster and date per file from the release row.
- TUI/stats: nothing on agent or poster (grep stats_dashboard.rs: none). groups_menu and metrics: section 8/9.

## 5. Catalog coverage, item by item

### 5.1 The posting-tool table (catalog 643-661): the TOOL LABEL is not_collected for every row

No code names a tool. grep over src for nyuu|ngpost|powerpost|jbinup|newsmangler|gopoststuff|nxg|sanguinews|newsup
hits only test fixtures (store.rs:2246-2256, 2304-2311; compact.rs:1159-1167, 1316-1456; probe/mod.rs:241-245;
filters.rs:452 a comment). `agent_of` forwards the header string (mod.rs:107-109) and the page prints it (web.rs:1291-
1293). Per row, what is reachable today:

| Row | Tells stored with no fetch | Tells shown | Tells dropped (parsed_not_stored) | Needs new fetch / impossible |
| --- | --- | --- | --- | --- |
| Nyuu | id `([A-Z][a-z]){12}-\d{13}` + `@nyuu>` suffix (segments.local tag 7 + domains row); 13-digit ms in the id; 716,800 segment (info.segment_size; segments.bytes approx); User-Agent (release_probe.agent); default From user@host (releases.poster); `total=` present (info.yenc.total) | Posted with; Poster/Email; Date (raw GMT form, info.headers); Article size; Parts | header order; pcrc32 8 lower-case hex; crc32= on last part only (file_crc32 is kept, its presence per part is not); Date seconds == id ms/1000 is checkable from stored strings but nothing checks it | repost with SAME id (dedup swallows it, add_segment returns false 645-705); `--date now` same Date on every part (per-article Date not stored); monotone Date (same) |
| Nyuu-Obfuscation | `{rand32}@{rand8}.{rand3}`: stored WHOLE (domain 0, TEXT) because the random domain never reaches SHARED_AFTER (store.rs:480-490) | none beyond Poster | random yEnc name per article (only the first article's name is kept, info.yenc.name); random Date per article | header order, Date suffix split |
| ngPost | `<32 hex>@ngPost>`: tag 1 + the `@ngPost>` domain row (the compact tests use exactly this, compact.rs:1757, 2178); 716,800; From random per article but the DOMAIN stays (the suffix row); Subject == id local for `-x` (files.subject) | Date (server arrival, printed raw: indistinguishable from a poster Date) | pcrc32 unpadded; no `part=`/`crc32=` on =yend (the absence of crc32 IS stored as file_crc32 null); trailing TAB/SPACE unescaped; empty line before =yend; header order | "sends NO Date" cannot be told from XOVER or from the stored header (the server fills Date) |
| Postie | `32@8.3` random: stored whole, TEXT, domain 0; From `14 <14@5.3>` in releases.poster (looks_made_up does not flag a dotted 3-letter TLD, release_info.rs:494-501); 768,000/750,000 | Poster/Email; Date raw (`UTC` or `+0000` zone readable in the printed string) | rapidyenc =ypart without size (not parsed anyway), =yend `size part pcrc32` lower-case padded; random yEnc name= per article; random header order (Go map) | Date backdated 0-6 h vs arrival (no arrival time stored; Injection-Date raw IS stored for probed releases, so the gap is derivable there and only there); md5(subject) partial mode (files.subject holds the first subject; a predb dictionary reverse is not built) |
| NewsUP | fixed From `NewsUP <NewsUP@somewhere.cbr>` in releases.poster (exact match possible); 768,000; its own `sec+usec.rand@time.newsup` (TEXT: has '.'); fake grammars @gpoststuff/@JBinUp.local/@camelsystem-powerpost.local as domain rows | Poster | `=yend size= pcrc32=` without part= (the one tell that names it; =yend part= is not parsed, yenc.rs:65-69); header order From, Newsgroups, Subject, X-* | shuffled group pool with empty From (per-article From/group not stored) |
| GoPostStuff | `UnixNano$gps@gopoststuff>` (TEXT, '$'); nanosecond time in the id | (none) | pcrc32 UPPER-CASE `%08X` (hex() loses case); header order From, Newsgroups, Message-ID, X-Newsposter, Subject | no Date (server fills it) |
| nxg-upper | `40 hex@21 hex.3 hex`: tag 1 local, domain stored whole (random, never shared) so in fact TEXT whole; Subject = sha256(id) checkable from files.subject + the id; 768,000 | Date raw (local zone abbreviation visible) | =ypart size= (not parsed), UPPER-CASE pcrc32 + crc32= last part, 0x7F escaped, LF-only headers, X-Nxg header (not in KEPT_HEADERS), missing Newsgroups header (not kept) | packs -hp: archive headers_encrypted IS stored and shown (web.rs:1308-1314), a strong indirect tell |
| newsmangler | `unixtime.5dp.partnum@<NNTP host>` (TEXT, '.'); the provider host as the domain row; the part number in the id checkable against segments.part | (none) | dot-stuffs leading '.' (dropped by the dot-unstuffing in article_raw nntp.rs:625-627 before anything sees it: never observable) | server-suggested id (see Server-made) |
| Rust `yenc` crate posters | `total=` absent -> info.yenc.total null (stored; "Parts" dd omitted on the page, web.rs:1263) | Parts missing (an absence, not printed as such) | never escapes TAB/SPACE | |
| PowerPost / YencPowerPost | `part1of45.<rand>@powerpost2000AA.local>` (TEXT: '.'; domain row); X-Newsposter (agent); 500,000/640,000; Subject without `yEnc` (files.subject; subject_base appends ` yEnc` for regexes, collections.rs:82-87) | Posted with (X-Newsposter) | no total=/crc32= on =yend (stored as nulls) | PowerPost 2004 sends no Message-ID: see Server-made |
| JBinUp | `{19-20}_{file}o{total}@JBinUp.local>` (tag 7 when '_' present, else 6; domain row); file/total counters checkable against files.file_total and expected; X-Newsposter | Posted with | | |
| Fill and repair tools | nzbrefresh re-posts the SAME id: swallowed by add_segment dedup (store.rs:645-705, index_release 737-765); nzb-repair `part>1 begin=1`: begin only kept for the first article (info.yenc.begin), `- ""` subject in files.subject | | stripped extra headers (nothing compares header sets) | juicenet next-run reposts: no per-article Date |
| Server-made ids | INN `<hex$0$n$hex@host>`, Highwinds `T.N$K.R@fxNN.site>`, Giganews base64, Diablo `hexunix$seq$pid$hosthash@cluster>`, Supernews base32: all TEXT tag 0 (contain '$', '.', '+', '/'); Farm `[A-Za-z]{32}@UFK>`: tag 6 + `@UFK>` row. The domain row names the injecting provider, not a poster (risk 10.6). None decoded: no counter, time, pid or K read (catalog 282-285 confirmed: "stored as any message-id; not decoded") | Path / Injection-Info / X-Complaints-To raw strings for one article (web.rs:1294-1298) | X-Ufk/X-Ufhash, X-Trace, X-Received-* (not kept) | the T/N/K decode needs code only; the SAME-SESSION join needs every id of a post (stored) plus arrival order (article numbers: not stored) |
| yEnc32 and newspost | server-made id as above | | yEnc32 trailing space after name= (trim, yenc.rs:54); escapes every '.' | |
| newzNZB | `unixtime.SESSION.part@PRiVATE>` (TEXT); session token and time in the id | | | |
| sanguinews | X-Newsposter 'sanguinews v.. (ruby ..)' (agent); server-made id; Date numeric zone raw | Posted with; Date raw | | |
| nntpPoster | server-suggested id; Date ends '+0000 (UTC)' raw (the comment is stripped by dates.rs:13-21 for posted_date, kept verbatim in info.headers.Date) | Date raw | | |

Status per row for the structured object: `not_collected` (the tool name), with the evidence above.

### 5.2 The Date-string cluster note (catalog 663-665)

- Raw Date of the first article of the main file: `shown` verbatim (web.rs:1294-1298) for probed releases; the zone
  token (`-0000`, `+0000`, `GMT`, `UTC`, local abbreviation) is in the printed string. Injection-Date and
  NNTP-Posting-Date likewise.
- The cluster itself (zone -> tool): `not_collected`. `dates::parse_zone` (dates.rs:78-109) already tells `-0000`
  (returns Some(None), :81-83), numeric offsets (85-93), `UT|UTC|GMT|Z` (96) and US abbreviations (97-104) apart, but
  only to compute a timestamp; `to_iso_date` drops the result (132-137) and `releases.posted_date` has no zone.
  Farm's 2-digit-year rewrite is parsed as a year (dates.rs:55-60) and lost.
- Date seconds == Nyuu id ms/1000: both strings are stored (info.headers.Date and the id in segments); nothing compares.

### 5.3 derived_per_article and friends

- message_id_grammar (473-475): `stored_not_shown`. Every id is recoverable (section 3); `segments.local[0]` gives the
  alphabet class and `local[1]` the length for tags 3-7 (store.rs:393-394); `domains.suffix` gives the domain. No
  classification, no regex, no search on domain (field_search has no such field, 22-32); nothing prints an id.
- message_id_timestamp (476-478): `stored_not_shown` (inside the stored id; nothing parses digits out of a local:
  grep for the tool names and for timestamp parsing of ids finds nothing outside dates.rs, which only reads Date
  headers).
- from_identity (479-481): `parsed_not_stored` for the item proper (constant vs random From per article): the ingest
  computes exactly this in `gather_per_article` (`posters` counts per (shape, author), `lasting` = seen twice,
  filters.rs:544-552) and keeps only "" or the author as the release poster (555). The ONE From that is stored
  (releases.poster) is `shown` as Poster/Email with the MADE UP flag (web.rs:1079-1086; release_info.rs:443-501:
  `.local`, `.invalid`, no dot, and the MADE_UP_DOMAINS list 467-492). This resolves the storage.md/probe.md/ingest.md
  disagreement: shown is true only of the single From string, not of the identity property.
- date_shape (482-484): `parsed_not_stored` for the item (Date absent vs present cannot be told at all: the server
  fills the XOVER Date when the poster sent none, and parse_overview takes column 4 as is, nntp.rs:2712-2728; same
  Date on every part / backdating / monotone order need per-article Dates, which `Article.date` holds in memory
  (parser.rs:16) and add_segment drops). The raw Date, Injection-Date and NNTP-Posting-Date of one article are `shown`
  (web.rs:1294-1298), and the Date minus Injection-Date gap is derivable from those two stored strings but not
  computed. Resolution of the three-way disagreement: "shown" applies to the raw strings only; the shape facts are
  parsed_not_stored at ingest.
- encoding_quirks.segment_size (368-370): `shown` as "Article size" (web.rs:1265) from `info.segment_size` =
  `end + 1 - begin` of the first article (mod.rs:120-122), main file, probed releases only. For every release the wire
  size per article is in `segments.bytes` (`stored_not_shown`): it includes headers (overview :bytes), so it is an
  approximation about 1-3 % above the yEnc segment (info.overhead_percent, mod.rs:127-130); the tail article's bytes
  (the odd-sized last part, catalog 618) is there too, unparsed.
- encoding_quirks.control_line_syntax (377-379): `parsed_not_stored` (yenc.rs:21-73 tolerant `value()` parser: key
  order, spacing, upper-case hex, trailing space all gone; `=yend part=` and `=ypart size=` not even read).
- ybegin.line (325-327): `shown` as "Line length" (web.rs:1264) from `info.yenc.line_len` (yenc.rs:58), main file only;
  the overview `:lines` that would cross-check it is parsed (nntp.rs:2725) and dropped at add_segment.
- yenc_additions.encoder_tells (452-455): `parsed_not_stored`: unpadded/upper-case pcrc32 (hex(), yenc.rs:37-39),
  rustnzb's =yend without part= (not parsed), xHarbour trailing space (trim). The escape rules are in `article.lines`
  until yenc_decode (step.rs:151-158; nntp.rs:2744-2772) and never recorded.
- yenc_additions.encoder_fingerprint_tuple (420-423): `parsed_not_stored` overall; sub-fields field order, TAB before
  name=, crc key spelling, hex case/padding, size= on =yend (parsed: yenc.rs:67), part= on =yend (not parsed), crc32=
  per part vs last only (only the first and last articles are read; the first's crc32 would land in info.yenc.crc32
  and derive prefers the last's, mod.rs:139), total= present (stored), begin=0 (first article's begin stored:
  info.yenc.begin), column-0 escapes, trailing TAB/SPACE, '=n' vs dot-stuffing (dot-unstuffing happens in article_raw
  nntp.rs:625-627 BEFORE split_article, so the distinction is destroyed on the wire read), empty line before =yend,
  lines ending in '=' (PR #55's push_yenc_line strips an odd trailing '=' silently, nntp.rs diff: `trailing % 2`).
- headers_poster_tool X-Newsreader / X-Newsposter (229-231): `shown` as "Posted with" when they are the first of
  User-Agent, X-Newsreader, X-Newsposter present (mod.rs:107-109; web.rs:1291-1293); a second agent header is
  `stored_not_shown` (kept in info.headers, excluded from the verbatim loop at 1295). X-Mailer: `parsed_not_stored`
  (in article.headers, not in KEPT_HEADERS mod.rs:81-94).
- headers_optional_netnews.User-Agent (126-128): `shown` ("Posted with"), stored in release_probe.agent, indexed
  (mod.rs:36-40), searchable by exact string (field_search.rs:196-198) so "Nyuu/0.4.2" and "Nyuu/0.4.3" are two values.
- nntp_additions.server_assigned_message_ids (282-285): `stored_not_shown` (as any id, TEXT tag, not decoded; the
  injecting host sits in the `domains.suffix` row for Highwinds/Farm ids; Xref's host token is dropped by xref_groups
  mod.rs:112-114).
- poster_profile.stable_keys / habit_keys / linked_identities (522-530): `not_collected` as profiles (no per-poster
  aggregation anywhere: no table keyed by poster or account; the only poster index is the optional
  `idx_release_poster` on releases.poster, field_search.rs:18, 376-383). Their per-release components: From, agent
  shown; Path, Injection-Info, NNTP-Posting-Host, X-Complaints-To, Organization shown raw for one article; id domain
  stored_not_shown; header order parsed_not_stored; segment size shown; line length shown; PAR2 block size stored
  (release_fingerprint.par2_block, duplicates.rs:40, shown as Repair facts via Contents.par2_block release_info.rs:58).
- Rank 6, tool classifier (catalog 1155): `not_collected`. Rank 8, encoder fingerprint (1157): `not_collected`.
- nntp_session.group_metadata (43-46): `parsed_not_stored`: groups_menu.rs:48-49 `list_groups` -> `.map(|(name, _)|
  name)` drops the LIST ACTIVE flag/bounds, kept 10 minutes in a process-local HashMap (CACHE_TTL 600 s, :10, :15-16,
  :42-56), TUI only; nothing reaches SQLite or a page.
- nntp_session.transfer_facts (58-60): `parsed_not_stored`, Prometheus only: NNTP_* families (metrics/names.rs:60-71),
  filled by report_servers (report.rs:82-125) and the command/byte spans in nntp.rs, rendered by `scrape` on
  /metrics (server.rs:54-60, 74-83); no TLS details, nothing per release, nothing in SQLite (metrics/mod.rs:1-7:
  counters start over with the process; only `catalog.rs` persists, and it counts releases by category/group/size,
  catalog.rs:1-6).
- overview_row.subject (contradiction 1): `stored_not_shown` confirmed from the code: web.rs:1087-1090 prints
  `n.posted` from `release_info::names_with` (release_info.rs:136-143, `releases.name`), and `files.subject` is read only
  by the NZB writer (nzb.rs:85-88); no `subject` read in src/web.rs. Relevant here because files.subject carries three
  tool tells (nxg sha256 subject, NewsUP 1-32 lowercase letters, Postie md5 subject) for the FIRST article of each file.

## 6. Extension points

6.1 A `tool` column on `release_probe`: add `("tool", "TEXT")` to `PROBE.added` (probe/mod.rs:46; the `added` columns
are created by `add_missing` on open, schema.rs:102-110, called from `migrate` 114-131, no rewrite) and a partial
index like `release_probe_agent` (mod.rs:36-40: `where tool is not null`). SET_PROBE (49-50) must list the new column:
it is `insert or replace`, so any column NOT in its list is reset to NULL on every re-probe (risk 10.8). ProbeInfo
(52-78) gets `tool: Option<ToolGuess>` (name, confidence, tells) in the JSON; a missing field deserialises as None for
Option, Vec/BTreeMap fields need `#[serde(default)]` as `layers` has (70). Search: a `tool` field follows `agent`
through field_search.rs:22-32, 41-53, 56-68, 83-96, 196-198, 255-261 and the form at web.rs:302-310, 891-900.

6.2 Where the tells are computed: inside `ProbeStep::run` between `split_article` and `yenc_decode`
(step.rs:146-158 for the first article, 177-180 for the last): `article.headers` (ordered Vec, raw names) and
`article.lines` (raw control lines and body lines) exist only there. A `probe/tool.rs` (or `probe/yenc.rs` growing a
`Tells` struct next to YencLines) taking `&RawArticle`, the message-id string (`first.message_id`, step.rs:136-141),
`segment_size`, the tail `SegmentRef.bytes`, `agent_of`, the raw Date and `releases.poster` (ctx.release().poster,
postprocess/step.rs:74) is the natural shape. This is rank 8's home; it does not touch nntp.rs::yenc_decode, which
PR #55 rewrites (risk 10.4).

6.3 DB-only backfill (rank 6 for the ~103M unprobed releases and a cheap first pass for the rest): a new
`Needs::Database` step (postprocess/step.rs:25-29; trait 37-65; register in steps.rs:10-26 with an unused id and a
rank inside the ranges the test enforces, steps.rs:38). It reads `ctx.segments(main_file.id, &SegmentPick::First(1))`
and `Last(2)` (step.rs:106-108 -> store.rs:1803-1835; `archive::steps::first_article`/`tail_articles` 38-53 are the
existing helpers) for the id string, segment bytes and tail bytes, and `ctx.conn()` (step.rs:112-114) for
`release_probe.agent` and `json_extract(info, '$.yenc.line_len')` etc. (rusqlite 0.40 `bundled`, Cargo.toml:28, ships
SQLite with JSON built in; no json_extract is used in src today, grep: none). It writes through `Outcome.writes`
(step.rs:209-211: "your own tables only, or your own releases column") either an `UPDATE release_probe SET tool = ?`
(only where a row exists) or rows of its own table `release_tool (release_id PK, tool, confidence, tells)` registered
in SHARD_TABLES (schema.rs:78-92) so the gc trigger drops them with the release (123-128). Triggers: a `config_stamp`
bit on the step (postprocess/step.rs:58-62) makes `seed_shard` queue a `steps` walk with that redo bit
(engine.rs:1103-1111) on the next start; or `atlas --reprocess --step <name> --apply` (reprocess.rs:25-40, 137-162;
the dry run prints `total / backlog_per_second` seconds, 110-135).

6.4 Pace and writer: walks take `backlog_per_second / SHARDS` releases per second per shard (engine.rs:421-431;
default 2000, config.rs:628; SHARDS 8, store.rs:35) in batches of WALK_BATCH 256 (engine.rs:28) and stop while
`writer_queued > 2 * SHARDS` (418-420). 118M releases / 2000 per s = 59,000 s, about 16.4 h of walking at the default
with nothing else; each visit adds a blob decode (section 3) and a `release_probe` read. Every write goes to the shard's
single writer thread (indexer.rs:450-458 `on_writer`, WriterJob 224-227; profile.rs:68, :126) which gives stage work
10 ms after each save and 100 ms when idle (indexer.rs:230-231), so 118M small updates land between saves and stretch
the wall time well past the walk's own; the lane never blocks saves. A regex per id over 118M ids is CPU-trivial next
to the zstd decodes.

6.5 SQL function for ad-hoc analysis: `filters::register_sql` (filters.rs:376-379) registers `atlas_blocked` with
`create_scalar_function` (rusqlite `functions` feature, Cargo.toml:28); an `atlas_msgid(local, suffix)` and
`atlas_tool(...)` would let `select ... from segments join domains` answer "how many ids per grammar" without a walk.

6.6 UI: in `probe_html` append to `posted` (web.rs:1290-1302) before the header loop: `dd(&mut posted, "Posting tool",
format!("{} {}", search_link_to("tool", name, name), tag(class, "LIKELY"|"GUESS")))` and `dd(&mut posted, "Tells",
list.join("<br>"))`; `tag` (694-696) and `search_link_to` (800-802) exist. For unprobed releases (no release_probe row)
the panel does not exist: `probe::read` returns None (mod.rs:142-148) and `release_html` writes nothing (1163-1170), so a
tool guess stored in its own table needs its own small panel or a fact in the top `facts` dl (1048-1134, e.g. after
Poster/Email at 1086) to appear on EVERY release page. Copy cards (1468-1530) are the other place a tool label helps.

6.7 Per-article Date/From/number (for from_identity constancy, date_shape, Highwinds session stitching): not
storable without a new table or blob v2 (blob.rs:1-5, VERSION 1 at :9, parse 132-134 rejects others; segments has no
columns for them, store.rs:181-188; the catalog's own storage section says the same at 1181). Ingest has them in
memory (parser.rs:12-27) at add_segment time.

## 7. Backfill facts worth copying into the plan

- Population: 118M releases, about 15M pass the body gates (README.md:405); one shard holds about 9.8M files and 221M
  segments (design spec :7). release_probe rows exist only for probed releases (section 2.2), so a tool column there
  covers at most ~15M; a "tool from id + sizes" for the rest needs its own rows.
- The ProbeStep re-run path is NOT DB-only: it is `Needs::Bodies` (step.rs:83-85) and refetches two articles per
  release; a stamp bit on it (99-101) would re-probe every probed release over the network (bodies at
  `body_fetches_per_minute` 600 default, config.rs:630; README.md:405 estimates 78 days at 300/min for the backlog).
- What is derivable for ALL releases with no fetch: id string (grammar, domain, embedded time), per-article wire bytes
  (segment size +-3 %, tail bytes, count), parts from the subject, first subject per file (files.subject), one From and
  one ISO Date (releases.poster/posted_date), file names and PAR2 block sizes. What needs the probe row: exact yEnc
  segment size, line length, total, pcrc32 presence, agent, raw Date/Injection-Date/Path/Injection-Info.

## 8. groups_menu.rs (catalog nntp_session.group_metadata)

TUI only. `load_groups` (groups_menu.rs:39-57) calls `BlockingPool::list_groups(pattern)` (LIST ACTIVE wildmat) and
keeps the names only (`map(|(name, _)| name)`, :49), in `GROUPS_CACHE` keyed (host, pattern) for CACHE_TTL = 600 s
(:10, :12-16, :42-56); `page_nonempty` (:20-37) GROUPs each shown group for `last <= first` and caches that in
`EMPTY`. The chosen group is written to config (:216-225), never to SQLite. No release-page field; the LIST ACTIVE
flag (y/n/m/j/x/=) is dropped at :49. Status: parsed_not_stored.

## 9. metrics/report.rs and metrics/server.rs (catalog nntp_session.transfer_facts)

`report_servers` (report.rs:82-125) folds `ServerStat` per server into gauges NNTP_CONNECTIONS/WAITING/SERVER_STATE/
SERVER_INDEXING and counters NNTP_HEADERS/NNTP_TRAFFIC (wire vs text bytes) and seeds NNTP_FAILURES per kind;
`report_indexer` (141-160) copies stats.json numbers (workers, groups, load, stage, predb, media titles). `server.rs`
serves `/metrics` and `/healthz` on its own port (63-88, 91-103) and renders `super::render()` (54-60). All families
are in-memory (metrics/mod.rs:1-7 "A counter starts over with its process"); the only persisted metrics are the
catalog walk's counts (catalog.rs:1-6), none about transfers or tools. Confirmed: transfer facts stay Prometheus-only
and reach no release page. The names list (names.rs:60-71) has commands/bytes/duration per server and command, no TLS.

## 10. Risks

10.1 The alphabet class is a storage optimisation (store.rs:368-371 doc; 373-404): it must never be used as a tool
label. Classify on the decoded string (section 3), and expect legacy TEXT rows for packed-able locals (680-687).
10.2 domain 0 vs a domains row is a per-writer frequency decision (SHARED_AFTER 3, bounded caches cleared when full,
store.rs:450-452, 480-500) and compaction re-decides it (compact.rs:468-495 keeps only shared domains): "has a domain
row" means nothing about the poster; always rebuild the full id.
10.3 Sealed blobs cost a zstd decode per file per visit (blob.rs:87-89; file_segments decodes the whole blob for
First(1), store.rs:1813-1817) and may be remote (blob.rs:74-84, sealed_store.rs:25-37; README.md:330: compaction then
refused, compact.rs:462-466). READ_MAX_SEGMENTS 1M (store.rs:797) can make a corrupt blob an error for the whole
release (`blob_error`, 810-812).
10.4 PR #55 (remotes/pr/55, one commit 6a0ff6d, src/nntp.rs +205, Cargo.toml +1 `nntmux-yenc` git dep) replaces the body
of `yenc_decode` with `push_yenc_line` + `nntmux_yenc::decoder::decode` over one buffer and keeps the old scalar decoder
only as a test oracle. It does not touch probe/yenc.rs or probe/step.rs, so tells computed from `article.lines`
before decoding do not conflict; tells hooked inside yenc_decode would. Its line normalisation (odd trailing '='
dropped, CR/LF inside a line unescaped) removes two catalog tells from the decoder's view.
10.5 Open PRs on the same code: #48 (remotes/pr/48 head 06847b2; no local branch named feat/release-nzb-files or
feat/release-page-media-cover exists, only the PR ref) changes `probe_html`'s signature (adds `cover: Option<i64>`),
adds hunks inside Tracks (Muxed, display aspect, primaries/transfer, DV level, MaxCLL/MaxFALL), an Attachments table
and a Cover panel, and rewrites the final `format!` footer (main web.rs:1445-1451) to
`"{cover}{}{}{attachments}{}{}{}{nfo}{sfv}"`; it adds `COVER` to SHARD_TABLES (schema.rs:84-85) and bumps
`config_stamp` 1 -> 3 (step.rs:96-101), whose bit 2 collides with main's #52 bit (`1 | 2` on main). A tool
classifier's stamp bit must be agreed after #48 rebases (next free bit 4 on main today, 8 if #48 takes 4). The `posted`
block (1290-1302) itself is untouched by #48, #51, #54, #55, #57, #58 and the stills-strip branch; #51/#54 touch
probe/mod.rs by one line (a `pub mod stills`), step.rs by 11 lines and steps.rs/schema.rs by one line each, so a new
step registration and a new SHARD_TABLES entry will merge-conflict textually with them (trivial). The stills-strip
branch adds 68 lines to web.rs in release_html's panel assembly (1194-1196 region). #57 rewrites `Search`
(web.rs hunks 5-730: struct Search, from_args, results sorting), so a `tool` search field conflicts with it; #58
touches probe/nest.rs and probe/mod.rs (+1).
10.6 Highwinds `@fxNN.site`, Farm `@UFK`, Giganews, Astraweb/Diablo ids name the PROVIDER's injector (catalog 657):
a classifier must emit "server-made id (provider X)" and never a tool; Nyuu adopts a 340-suggested id and
newsmangler/sanguinews/nntpPoster/yEnc32/PowerPost 2004/newspost always get one, so those posts are only separable by
agent, From, Date zone, segment size and yEnc tells. NewsUP fakes seven grammars (catalog 646): id alone never names a
tool; weight the id below `=yend` shape, agent and From.
10.7 Coverage: only one article's headers per release, only probed releases (~15M), only with `archive.enabled`
(step.rs:86-89), so "Posted with" is absent on most pages today; a `tool` on `release_probe` inherits that. The exact-
match agent search (field_search.rs:168, 197) splits versions; a normalised `tool` value fixes it.
10.8 `SET_PROBE` is `insert or replace` of five named columns (mod.rs:49-50): a `tool` column kept outside that list is
NULLed on every re-probe (growth reruns, step.rs:93-95). Either add it to SET_PROBE and compute it in the probe, or keep
it in a separate table. Also `info` is rewritten whole each time, so a DB-only backfill that writes into `info` JSON
would race the probe's replace (the writer serialises, but the later write wins and may drop the other's field).
10.9 Backfill size: 118M releases at `backlog_per_second` 2000 is ~16 h of walk plus 118M writer jobs on 8 single
writers (section 6.4); `atlas --reprocess` dry run gives the exact number per shard (reprocess.rs:110-135). The walk
visits newest first (engine walk_tick `order by r.id desc`, 433-447), so the oldest ~100M get their tool last.
10.10 Privacy: Nyuu's default From leaks `user@host` and Injection-Info posting-account/host are account keys (catalog
1221); they are already printed verbatim (web.rs:1082-1085, 1294-1298). A tells list must not add hashes or ids that make
them searchable in new ways without the owner's call.
10.11 The Finish web UI thread's twin grouping lives in `copies_html` / clusters (web.rs:1455-1620) and
release_fingerprint (duplicates.rs:21-40); a tool label shown on copy cards (copy_card 1468-1530) is the only overlap.
Session stitching keys (Highwinds host+K+N ranges) would need article numbers that segments never stored (10.3 of the
catalog's own storage note).

## 11. Open questions

- Should the tool for the ~103M unprobed releases be guessed from id grammar + segments.bytes alone (segment size is
  only +-3 % there) and shown with a lower confidence tag, or left blank until a probe runs?
- Is `release_tool` (own table, every release) or `release_probe.tool` (probed releases only, SET_PROBE change) the
  owner's preference? The former gives "every release page" today; the latter is one column.
- Which stamp bit is free after #48 and #52 reconcile (step.rs:99-101 vs #48's `3`)?
- Does the sealed log run in the deployment that would backfill (sealed_store.rs offload)? It changes the per-release
  cost from a local zstd decode to a network fetch.
- Date-zone classification needs the raw Date, which only probed releases keep; is adding the first article's raw Date
  zone token at ingest (one TEXT column on releases, or a byte in files) acceptable, given per-article data must not go
  on segments?
