# Atlas map — area: probe (src/probe/*.rs, src/nntp.rs, release_probe, probe_html)

Repo: /home/claude/appz4fun/atlas, branch main at 55d2e51 (read-only; nothing built or run). Remote `origin/main` has moved
one commit past it to 67504d6 "Decode bzip2, deflate64, compress and lz4 layers (#56)", which adds `src/probe/codec/*` and
one `pub mod codec;` line to `src/probe/mod.rs` (git diff 55d2e51 origin/main). Everything below cites 55d2e51 unless
marked "origin/main" or "PR #n".

Catalog: /tmp/claude-0/-home-claude/b80d34bc-6432-5cb9-a471-5c997487e01d/scratchpad/catalog-current.md. The `atlas:` legend
there says whether Atlas collects a field; this file says whether the WEB UI shows it, from the code.

Status vocabulary used here: **shown** (release page or a panel it loads), **stored_not_shown** (SQLite column or inside
the `release_probe.info` JSON, no UI), **parsed_not_stored** (in memory during the probe, dropped), **not_collected**,
**unclear**. "In hand" below means: the bytes are already fetched by the probe for that release, so storing the field costs
no extra NNTP request.

---------------------------------------------------------------------------------------------------------------------------

## 1. Exactly what the probe fetches, per release

Step: `ProbeStep`, `StepId(4)`, name "probe", rank 120, `Needs::Bodies` (src/probe/step.rs:19, 73-85). Enabled only when
`cfg.archive.enabled` (step.rs:86-89; default true, src/config.rs:546-563). Skips releases whose name ends in `.jpg`
(step.rs:90-92). Reruns on growth, not on rename (step.rs:93-95). `config_stamp() = 1 | 2` (step.rs:96-101): a new bit
re-probes every existing release (see §9).

Order of requests in `run` (step.rs:102-212), every one through `fetch_raw` (step.rs:55-62) = `ctx.fetch_article` =
`BackgroundRead::Raw` = NNTP `ARTICLE <message-id>` read whole and undecoded (src/postprocess/step.rs:152-159;
src/nntp.rs:1792-1794, 614-635), capped at `ARTICLE_CAP` = 4 MiB (src/archive/mod.rs:28) and at `body_max_bytes`
(16 MiB default; src/postprocess/bodies.rs:143-146, src/config.rs:633):

1. Up to 3 small text files (`.nfo`, `.sfv`, `.txt`; step.rs:33-36, 110): the FIRST indexed segment of each
   (`SegmentPick::First(1)`, step.rs:111). ARTICLE. Headers parsed by `split_article` and then DROPPED (step.rs:115 uses
   only `article.lines`). Body decoded (`yenc_decode`, falling back to raw lines joined, step.rs:116), CP437 fallback
   (`nfo_text`, mod.rs:150-161). SFV → `info.sfv` (mod.rs:187-203). First NFO → `info.nfo` (≤ 256 KiB chars,
   step.rs:22, 128), `info.imdb_ids` (mod.rs:163-185), and a password write to `release_nfo_password`
   (step.rs:122-127, `archive::store::SET_NFO_PASSWORD`, `release_info::find_password` src/release_info.rs:517).
2. The main file (`main_file`, step.rs:38-53: most parts, not PAR2/NFO/SFV/TXT, `.rar` first among ties):
   - **first article** = `archive::steps::first_article` (src/archive/steps.rs:37-40: `SegmentPick::First(1)` and only if
     its part ≤ 1). ARTICLE whole. Its `bytes` from the overview row becomes `wire_bytes` (step.rs:138). From it:
     `info.headers = keep_headers(&article.headers)` (step.rs:147), `crossposted_to` from the kept Xref (148-150),
     `info.yenc = yenc::parse(lines)` (151), body decoded, `part_crc_ok = crc32(body) == pcrc32` (152-155),
     `inspect(front)` → `info.archive`, `info.layers`, `info.media` (157, 396-402). `health["first"]` from
     `there(&got)` (142-144).
   - **middle article**: only when the file has > 2 parts: part `(total+1)/2` by `SegmentPick::Parts` (step.rs:161-169),
     `ctx.article_exists` = `BackgroundRead::Stat` = NNTP `STAT` (postprocess/step.rs:162-174; nntp.rs:637-642,
     1795). Nothing transferred; `health["middle"]`.
   - **last article** = `archive::steps::tail_articles(...).pop()` (archive/steps.rs:43-52: `SegmentPick::Last(2)`, only
     when the last indexed part == `files.expected`), skipped when it is the same message-id as the first (step.rs:171).
     ARTICLE whole. `last_lines = yenc::parse`, `last_body = yenc_decode` (177-179). Its HEADERS are parsed by
     `split_article` and never kept (177: only `.lines` used). Its pcrc32 is NOT checked. `health["last"]`.
   - `derive(&mut info, wire_bytes, file.expected, last_lines)` (step.rs:182; mod.rs:116-140): segment_size,
     part_size_ok, overhead_percent, continuity_ok, file_crc32 (last's crc32, else first's).
   - **moviehash** (step.rs:184, 292-394): needs the last 64 KiB of the video; uses `last_body` when it is the right part
     (`last_part`, 252-260), else `file_range`/`read_part` fetch 1–2 more articles with ARTICLE (263-290, 241-250), or
     for a stored RAR set the FIRST article of the LAST volume plus its tail part (331-360).
   - **read_more_front** (step.rs:186-193, 217-238): while the media header is incomplete, parts 2..=min(total, 6)
     via `read_part` (ARTICLE each); `inspect` again each time.
3. Every one of these calls goes through `BodyFetcher::fetch` (bodies.rs:124-177): refusal when the group is blocked, the
   release is outside `body_min_size_bytes`..`body_max_size_bytes` (1 MiB..100 GiB defaults, config.rs:631-632), the
   file name is an image, or `used >= bodies_per_release` (8 default, config.rs:634) (bodies.rs:85-100); one token per
   request from `body_fetches_per_minute` (600 default, config.rs:629; bodies.rs:102-122); `used += 1` for EVERY
   request, STAT included (bodies.rs:142). So a release costs 2 (first+last) to 8 requests; past 8 the rest come back
   `Got::Refused("past bodies_per_release")` and `there()` yields None (step.rs:64-71), i.e. no health entry.
4. Server choice: `Pool::background_read` tries the indexing tier least-busy first, then the other servers in priority
   order; 423/430 means "this server doesn't have it", anything else is "unreachable" (nntp.rs:1750-1823, 1504-1518,
   2494-2499). Health is therefore "some server still has it", not per provider, and undated.
5. Empty result (no yenc, no nfo, no sfv, no health) writes nothing to `release_probe` (step.rs:196-199) but still writes
   the NFO password.
6. Write: `SET_PROBE` = `insert or replace into release_probe (release_id, crc32, agent, info, moviehash)` with
   `crc32 = file_crc32`, `agent = agent_of(headers)` (User-Agent, else X-Newsreader, else X-Newsposter; mod.rs:106-109),
   `info = serde_json::to_string(&ProbeInfo)` (step.rs:200-207; mod.rs:49-50). `Outcome.matched = true`, a `moviehash`
   fact and a rerun of the OpenSubtitles step when a hash was found (step.rs:208-211).

Lanes/gates before the probe even runs (src/postprocess/engine.rs:585-700, src/postprocess/rules.rs:158-209): the release
must pass `judge_files` (not empty, settled, not a fragment, ≥ `min_files`, not PAR2-only) and `body_gate`
(size within bounds, not all images, completion ≥ `min_completion_percent` 95%; "NotYet" waits). The probe's `applies`
also excludes `*.jpg`. Releases that fail these NEVER get a `release_probe` row, and the page then shows no probe panels
at all, silently (`unwrap_or_default`, src/web.rs:1163-1170).

Test coverage of the above: tests/probe_bg.rs:57-149 (yenc lines, derive flags, agent, crossposts, health, media, NFO,
password), tests/probe_moviehash.rs:105-189 (stored RAR set hash + OpenSubtitles match + chapters past article 1). The
mock server's ARTICLE answer carries exactly From, User-Agent, Path, Xref (tests/common/mod.rs:311-316), so no test
exercises Date/Injection-Date/Injection-Info/Organization/NNTP-Posting-* rendering. No test calls `probe_html` or
`release_html` directly (src/web.rs: only the definitions at 1028 and 1243 and the one call at 1168); tests/web_ui.rs
fetches `/ui/release?id=` (161, 206, 284, 369) but asserts nothing about the probe panels.

## 2. The NNTP layer (src/nntp.rs)

Commands ever sent (grep of `command(`): `AUTHINFO USER/PASS` (385-387), `XFEATURE COMPRESS GZIP` (397), `GROUP`
(720), `XOVER` (746, 814), `LIST ACTIVE` (840), `BODY` (586, 653, 766), `ARTICLE` (615), `STAT` (638), `QUIT` (706).
Never: CAPABILITIES, HEAD, HDR/XHDR/XPAT, DATE, LISTGROUP, NEWNEWS/NEWGROUPS, LIST OVERVIEW.FMT/NEWSGROUPS/ACTIVE.TIMES,
OVER, XZVER/XZHDR.

- Login: the greeting's code is checked (200/201) and its text discarded (378-381). `XFEATURE COMPRESS GZIP` is sent when
  the server's `compress` setting is on (default true, config.rs:49 `Option<bool>`; `Server::compress`, 1094-1098);
  a 290 sets the per-connection `compressed` flag (396-399, 297). Nothing about capabilities is stored anywhere.
- Multi-line replies: `each_line` inflates when the status line says `COMPRESS=GZIP` (776-799, 431-491, `Inflater`
  905-993), otherwise plain. BODY/ARTICLE refuse a compressed answer (591-593, 657-659).
- XOVER rows: `parse_overview` (2712-2728) splits on tabs and keeps exactly fields 0..7 (number, subject, from, date,
  message_id, references, bytes, lines) with `from_utf8_lossy`; column 9+ (Xref:full, anything extra) is ignored.
  `Overview::into_article` (102-116) keeps all eight but converts `date` with `to_iso_date` (src/dates.rs:132-137: RFC
  2822 → `%Y-%m-%d %H:%M:%S`, offset DROPPED, unparseable kept raw). `Ends::add` (265-290) reads only number and date for
  date searches. What reaches the store per article is `(file_id, local, domain, part, bytes)` only (src/store.rs:181-188
  schema, 687-690 insert): number, references, lines and the per-article Date and From are gone at save time. releases
  keeps one `poster` and one `posted_date`, overwritten on every re-save (store.rs:1391-1397 upsert).
- Message-id storage: `Domains::encode` splits `<local@suffix>` into a packed local blob and a `domains.suffix` id, or
  stores the id whole under domain 0 until a suffix repeats (store.rs:464-498; `decode` 528-533). Sealed files move
  segments into `files.blob`, format v1: per segment part+1, bytes delta, domain, local (src/blob.rs:1-5, 12-19).
- Article reads: `article_raw` (614-635) = ARTICLE, dot-unstuffed, lines joined with `\n`, CR/LF stripped per line by
  `read_line_capped` (548-576: `while buf.last() is \n or \r pop`), None past `max_bytes` (connection then thrown away,
  `Lease.busy`). `body_capped` (578-611) / `body_prefix` (645-675) = BODY, `yenc_decode` or raw lines joined. `stat`
  (637-642) = STAT, 223 → Ok. Status texts of 430/423/451 etc. are not kept: `failure_kind` folds 420|423|430 into one
  metric label (166-183) and `background_read` turns 423|430 into "not here" and anything else into an error
  (1810-1818). A 451 "removed" would surface as `StepError::Transient` text in `pp_state.last_error`, not as data.
- `yenc_decode` (2744-2772): finds `=ybegin `, skips `=ypart `, stops at `=yend`, decodes `=`-escapes and `-42`; records
  nothing about escapes, line lengths, control-line syntax or trailing text. PR #55 replaces its inner loop with the
  `nntmux-yenc` crate (see §10).
- Metrics only (no SQLite): commands by server/command/result, seconds, bytes by direction, failures by kind
  (331-349, 141-147, 149-163; src/metrics/names.rs:65-68).

## 3. What the probe extracts: `ProbeInfo` (src/probe/mod.rs:52-78) and `YencLines` (src/probe/yenc.rs:7-18)

```
ProbeInfo {
  file: Option<String>                 // main file's filename (from the Subject-derived files.filename), step.rs:134
  yenc: Option<YencLines>              // FIRST article of the main file only
     { name, file_size, line_len, part, total,   // =ybegin  (yenc.rs:50-60; name = everything after " name=")
       begin, end,                               // =ypart   (61-64)
       yend_size, pcrc32, crc32 }                // =yend    (65-70)  -- =yend part= is NOT read
  file_crc32: Option<u32>              // last article's crc32=, else first's (mod.rs:139)
  segment_size: Option<u64>            // end+1-begin of the first part (mod.rs:120-122)
  overhead_percent: Option<f64>        // (wire bytes of first article - segment)/segment (mod.rs:127-130)
  continuity_ok: Option<bool>          // Subject total == yEnc total, first begin==1, last begin/end tile (mod.rs:131-137)
  part_size_ok: Option<bool>           // first =yend size == end+1-begin (mod.rs:123-125)
  part_crc_ok: Option<bool>            // crc32(decoded first body) == first pcrc32 (step.rs:152-155)
  health: BTreeMap<String,bool>        // "first","middle","last" → still on some server (step.rs:142-144,164-168,172-175)
  headers: BTreeMap<String,String>     // KEPT_HEADERS of the FIRST main-file article, values cut to 512 chars (mod.rs:81-104)
  crossposted_to: Vec<String>          // groups parsed from the kept Xref (mod.rs:111-114)
  archive: Option<ArchiveHead>         // outermost archive header (probe/archive.rs; not this area's catalog)
  layers: Vec<nest::Layer>  #[serde(default, skip_serializing_if = is_empty)]   // nested archives (nest.rs:23-51)
  media: Option<MediaInfo>             // MKV/MP4 (probe/media.rs; another area)
  nfo: Option<String>, imdb_ids: Vec<String>, sfv: BTreeMap<String,u32>
  moviehash: Option<String>            // also a column
}
```
`KEPT_HEADERS` (mod.rs:81-94): Injection-Date, NNTP-Posting-Date, Date, User-Agent, X-Newsreader, X-Newsposter, Path,
Injection-Info, Organization, Xref, NNTP-Posting-Host, X-Complaints-To. Matching is case-insensitive and the stored key
is the canonical spelling (mod.rs:99-100), so the poster's header casing is lost. `split_article` (yenc.rs:88-117)
keeps every header in order in a `Vec<(name, value)>` (max 256 headers, folded continuation lines joined with one space,
names and values trimmed, `from_utf8_lossy`), so header count, order and all other names are IN HAND and dropped by
`keep_headers`. Body lines are kept as `Vec<Vec<u8>>` (max 262,144 lines) with CR/LF already stripped at the NNTP layer.

Serde facts that matter for extension: `ProbeInfo` derives `Default + Serialize + Deserialize` but has NO struct-level
`#[serde(default)]`; only `layers` has a field-level default (mod.rs:70-71). `read()` does
`serde_json::from_str(&text).ok()` (mod.rs:142-148) — a new non-`Option` field without `#[serde(default)]` makes every
existing row fail to parse and the whole probe section of the page disappears for old releases. `Option` fields are
safe (serde treats missing as None). `YencLines` fields are all `Option`, so adding one there is safe.

## 4. Storage

Table `release_probe` per shard (8 shards, src/store.rs:35; `ShardTable` registered in
src/postprocess/schema.rs:78-92, created/migrated on open by `schema::migrate` 114-131, GC trigger on release delete):

| column | type | written by | read by |
|---|---|---|---|
| release_id | INTEGER PK | step.rs:204-207 `SET_PROBE` | mod.rs:142-148 `probe::read` (web.rs:1166) |
| crc32 | INTEGER, partial index `release_probe_crc32` (mod.rs:32-35) | `info.file_crc32` | field_search.rs:190-193 → `@probe:crc32:` (213-217); web form "CRC32" web.rs:899 |
| agent | TEXT, partial index `release_probe_agent` (36-39) | `agent_of(headers)` | field_search.rs:195-197 → `@probe:agent:`; web form "Posted with" web.rs:900; link `search_link_to("agent")` web.rs:1292 |
| info | TEXT NOT NULL (JSON of ProbeInfo) | step.rs:203 | web.rs:1166 only ("only ever read whole", mod.rs:22) |
| moviehash | TEXT, added later via `added` (mod.rs:46), partial index (40-43) | `info.moviehash` | opensubtitles.rs:137-148 `hash_of` (fallback when no fact) |

`insert or replace` (mod.rs:49-50): a re-probe replaces the row wholesale; nothing accumulates (health has no history).
Column additions use `ShardTable.added` → `alter table add column` on open (schema.rs:102-110), indexes are created
if missing (116-121); `sample_row` for tests (133-159). Related tables: `release_opensubtitles` (opensubtitles.rs:26-48,
written 214-229, read 285-309 → web.rs:1167, 1391-1403), `release_nfo_password` (archive store), `release_fingerprint`
(duplicates.rs:21-39: posted/content SHA-1 + par2_block(s), the twin/alt keys the "Copies" panel uses; copies_with
224-251, can_fill 522-524).

The API (src/api.rs) never reads `release_probe` (no match for probe/field_search/agent/crc in api.rs), so every probe
field is HTML-only. rusqlite 0.40 is `bundled` (Cargo.toml:28), so SQLite's built-in JSON functions (`json_extract`) are
available for a JSON→column backfill, though nothing in src uses them today (grep json_extract: none).

## 5. UI surface: `probe_html` (src/web.rs:1243-1453) on `/ui/release?id=` (route web.rs:60; `release_page` 989-1017;
`release_html` 1028-1202 calls `probe::read` + `opensubtitles::read` at 1163-1170 and places `{probe}` after the Copies
panel, 1193)

Panels, in render order (web.rs:1445-1452): Tracks, Chapters, **yEnc**, Packing, **How it was posted**, then NFO (h2 +
imdb links + `<pre>`), SFV table. `panel()` omits an empty panel (1235-1241); `dd` omits empty values (1244-1248).

yEnc panel (1250-1290): File name (`yenc.name`), File size (`yenc.file_size`), Parts (`yenc.total`), Line length
(`yenc.line_len`), Article size (`segment_size`), CRC32 (`file_crc32`, link to `?crc=`), Overhead (`overhead_percent`),
Checks (continuity_ok → ALL BYTES COVERED/BYTES MISSING, part_size_ok → PART SIZE OK/WRONG, part_crc_ok → PART CRC
OK/WRONG; `verdict` 1227-1233 hides None), On the server (FIRST/MIDDLE/LAST THERE|GONE).
NOT rendered although in the JSON: `file`, `yenc.part`, `yenc.begin`, `yenc.end`, `yenc.yend_size`, `yenc.pcrc32`,
`yenc.crc32` as its own value (only via file_crc32).

How it was posted (1291-1303): "Posted with" = `agent_of(headers)` (the first of User-Agent, X-Newsreader, X-Newsposter;
the other two are then hidden by the exclusion at 1295); every other kept header as raw `<dt>name</dt><dd>value</dd>`
(Date, Injection-Date, NNTP-Posting-Date, Path, Injection-Info, Organization, NNTP-Posting-Host, X-Complaints-To);
Xref is hidden and replaced by "Crossposted to" group links (1299-1302; the server host and article numbers inside
Xref are therefore stored, not shown).

Other probe-derived UI: Tracks/Chapters/Packing/Layers (1305-1387, `nested_chain` 1840-1854), OpenSubtitles hash and
match (1388-1403), NFO (1411-1420), SFV (1421-1436). Search: "More fields" form has CRC32 and "Posted with" inputs
(884-912) feeding `Fields.crc/agent` (field_search.rs:29-31, 89-90).

Release facts above the probe (not from the probe, but the nntp-catalog items land here): Size (releases.size, 1061-1066),
Posted (`fmt_date(posted_date)`, 1067), Group(s) (1068-1078), Poster/Email with MADE UP (`parse_poster`,
`looks_made_up`; 1079-1085; release_info.rs:451, 501), Posted as / Name from (1086-1093).

## 6. Catalog coverage for this area (status · in hand? · evidence)

### nntp_session
- capabilities: parsed_not_stored (only XFEATURE COMPRESS GZIP, per-connection bool; nntp.rs:396-399, 297). Needs a
  CAPABILITIES command to collect; one per connection, not per release.
- greeting: parsed_not_stored (code only; text dropped; nntp.rs:378-381).
- server_clock (DATE): not_collected (no DATE command).
- group_counts: stored_not_shown at group level (db.rs:605-606 `save_group_bounds`; shown in the TUI, not on a release
  page). Not per release.
- group_metadata: parsed_not_stored (LIST ACTIVE → group picker; nntp.rs:832-880).
- overview_format: not_collected (fixed 8 columns; nntp.rs:2712-2728).
- article_presence: shown (health first/middle/last; step.rs:142-175; web.rs:1277-1290). Not per provider, undated,
  once per probe; STAT costs a body token (bodies.rs:142).
- retention_horizon: stored_not_shown at group level (db.rs:338, 621 low_posted; TUI). Not a release-page field.
- new_news: not_collected.
- transfer_facts: parsed_not_stored (Prometheus only; nntp.rs:331-349; metrics/names.rs:65-68).

### overview_row
- number: parsed_not_stored (Overview.number; cursors only; not in segments insert store.rs:689).
- subject: shown (files.subject → "Posted as"/Title; web.rs:1086-1088, 1057-1059). Per-file subject stored; only the
  lowest part's.
- from: shown (releases.poster → Poster/Email; web.rs:1079-1085; store.rs:1395 overwritten on every save).
- date: shown as `posted_date` (web.rs:1067) with the offset stripped (dates.rs:132-137); the probed first article's
  raw Date header is also shown (web.rs:1294-1298).
- message_id: stored_not_shown (segments.local/domain + domains.suffix; store.rs:181-188, 464-498). Only in the NZB.
- references: parsed_not_stored (nntp.rs:108; dropped at store.rs:689).
- bytes: shown as the release Size total (web.rs:1061-1066) and per article in the NZB; per-article not on the page.
  Stored per article (segments.bytes).
- lines: parsed_not_stored (nntp.rs:109; dropped at store.rs:689).
- xref: shown as "Crossposted to" from the probe's ARTICLE of one article (step.rs:148-150; web.rs:1299-1302); the
  XOVER Xref column is ignored (nntp.rs:2712-2728); host and numbers in the stored raw Xref are not shown (hidden at
  web.rs:1295).
- extra_columns: not_collected.

### headers_mandatory / optional / mail+mime / server-injected / poster-tool
For the FIRST article of the main file, every header is IN HAND (split_article, yenc.rs:88-117; up to 256 headers) and
all but the 12 KEPT_HEADERS are dropped by `keep_headers` (mod.rs:81-104). For the LAST main-file article, the up-to-5
extra front articles, the moviehash tail articles and the NFO/SFV/TXT articles, headers are parsed and dropped entirely
(step.rs:115, 177, 245, 344). The middle article is STAT only (no headers). So:
- Date, Injection-Date, NNTP-Posting-Date, Path, Injection-Info, Organization, NNTP-Posting-Host, X-Complaints-To:
  shown (raw, 512-char cap; web.rs:1294-1298). One article per release; no parsing (Path hops, posting-account, dates
  as times), no comparison (Date vs Injection-Date).
- User-Agent, X-Newsreader, X-Newsposter: shown as "Posted with" (first found of the three; web.rs:1291-1293), stored
  as `agent` column, searchable (field_search.rs:195-197). If two of them are present the second is stored in
  `info.headers` and hidden (web.rs:1295).
- Xref: see overview_row.xref.
- Newsgroups, Approved, Archive, Control, Supersedes, Distribution, Expires, Followup-To, Summary, Lines, Bytes,
  Cancel-Lock, Cancel-Key, In-Reply-To, Reply-To, Sender, Comments, Keywords, MIME-Version, Content-Type,
  Content-Transfer-Encoding, Content-Disposition, Content-Description/ID/Length, X-Trace, X-Abuse-*, X-Received-Bytes,
  X-Received-Body-CRC, X-Original-Bytes/Lines, X-Postfilter/X-Spam-*/X-Antivirus*, X-Ref, X-Server-Date, X-Mailer,
  X-No-Archive, X-Face/Face, X-Priority/X-MSMail-Priority/Importance/Precedence, X-XML*, X-User-*, any other X-*:
  parsed_not_stored for the first main-file article (in hand, free to keep: add the name to KEPT_HEADERS or keep the
  whole Vec); not_collected for every other article of the release.
- X-Received-Body-CRC specifically: in hand AND checkable for free — the decoded/raw body lines of the first and last
  articles are in memory (step.rs:146-152, 177-179), so CRC32 over the body text with CRLF plus the final "." could be
  computed there; the current `crc32` helper (archive/mod.rs:258) works on bytes.
- header_order: parsed_not_stored (Vec order in `split_article`, lost in the BTreeMap at mod.rs:96-104).
- header_count_and_bytes: parsed_not_stored (headers.len() and the raw article length are in hand; `raw` at
  step.rs:145 includes the header block).
- folding_and_case: parsed_not_stored, partly destroyed: continuation lines are unfolded and trimmed (yenc.rs:100-104),
  names/values trimmed (108-109), CR/LF stripped at the NNTP layer (nntp.rs:559-562, 574-575). Header-name case is in
  the Vec but canonicalised by keep_headers (mod.rs:99-100).
- date_format_quirks: shown only as the raw Date string (web.rs:1294-1298); no classification. Deriving it is free.
- from_shape: shown partly (parse_poster at render; MADE UP tag; web.rs:1079-1085; release_info.rs:451, 501); the shape
  itself is not stored. Only the release's one `poster` (the last save's first article), not the probed article's From
  (From is not in KEPT_HEADERS, so the probed article's own From is parsed_not_stored).

### nntp_additions
- hdr_xhdr_bulk_headers: not_collected (no HDR/XHDR; command list §2).
- response_code_signals: parsed_not_stored (nntp.rs:166-183 fold 420/423/430; 1810-1818; text dropped; a 451 becomes a
  Transient error string in pp_state.last_error, engine.rs:1010 region).
- article_number_portability: parsed_not_stored (numbers → cursors).
- server_assigned_message_ids: stored_not_shown (segments local/domain; no decoding).
- encoded_words_and_charset: not_collected (from_utf8_lossy at nntp.rs:2713; no RFC 2047 anywhere; yenc name likewise
  yenc.rs:52-54).
- mime_part_fields (Content-MD5, message/partial, Content-Disposition params): parsed_not_stored (first article headers in
  hand; dropped by keep_headers).
- x_nxg: parsed_not_stored (same).
- x_usenet_provider_and_backbone_tokens: Path and X-Complaints-To shown raw; X-Ufk/X-Ufhash/X-Received-*/X-Trace/
  X-Postfilter parsed_not_stored (first article) — no provider classification anywhere.
- spotnet_spot: not_collected (no X-XML handling; a spot article would only be read if it were the main file of a
  release passing the body gates, which spots do not).
- availability_prediction: shown (health), but one STAT set per probe, never repeated; co-removal across releases is not
  derived.

### yenc: ybegin / ypart / yend
- ybegin.name: shown ("File name", web.rs:1252). First article of the main file only; `from_utf8_lossy` (yenc.rs:52);
  never compared with the Subject name (`info.file`, stored_not_shown) or the inner name.
- ybegin.size: shown ("File size", web.rs:1253-1259).
- ybegin.line: shown ("Line length", 1261).
- ybegin.part: stored_not_shown (`yenc.part`, yenc.rs:59). The last article's part is parsed and dropped (step.rs:178).
- ybegin.total: shown ("Parts", 1260) and compared with the Subject total (mod.rs:132).
- ypart.begin / ypart.end: stored_not_shown (`yenc.begin/end`); shown indirectly as Article size and the ALL BYTES
  COVERED check (mod.rs:120-137). Last article's begin/end: parsed_not_stored (used in derive and last_part, then
  dropped).
- yend.size: stored_not_shown (`yenc.yend_size`); the PART SIZE check is shown (web.rs:1273). Last article's:
  parsed_not_stored.
- yend.part: not_collected (yenc.rs:65-70 reads size, pcrc32, crc32 only). In hand (the =yend line is in `lines`).
- yend.pcrc32: stored_not_shown for the first article (`yenc.pcrc32`); the PART CRC verdict is shown (web.rs:1274). The
  last article's pcrc32 is parsed and dropped and its decoded body (`last_body`, step.rs:179) is in hand, so checking
  and storing it is free. Front articles 2..6 and the tail parts: pcrc32 in hand (read_part parses the lines,
  step.rs:245-249) and dropped. All OTHER parts need one ARTICLE/BODY each (the =yend line is at the end).
- yend.crc32: shown ("CRC32" link; column `crc32`, searchable). The first article's own crc32= (some encoders put it on
  every part) is stored in `yenc.crc32` and only used as a fallback (mod.rs:139).

### encoding_quirks
- escape_set: not_collected; in hand (raw lines of first/last articles; decoder records nothing, nntp.rs:2744-2772).
- line_length_variance: not_collected; in hand (line lengths of every kept line; `line_len` declared value stored).
- line_endings: not_collected and NOT in hand: `read_line_capped` strips every trailing CR/LF (nntp.rs:559-562), so
  CRLF vs bare LF is gone before the probe sees the article. Needs a change in nntp.rs.
- segment_size: shown ("Article size").
- overhead: shown ("Overhead"; first article wire bytes vs its decoded span, mod.rs:127-130).
- preamble_and_trailer: not_collected; in hand (lines before `=ybegin` and after `=yend` are in `article.lines`;
  yenc_decode skips them).
- control_line_syntax: not_collected; in hand (the raw =ybegin/=ypart/=yend lines).
- part_numbering: shown (continuity check; `parts_agree` mod.rs:132). Only Subject-total vs yEnc-total of ONE article.
- filename_mismatch: stored_not_shown (`info.file` Subject name, `yenc.name`, `archive.name`/layers name sit in the
  JSON; `yenc.name` and the archive name are shown, `info.file` is not; nothing compares them).

### non_yenc_bodies
- uuencode / base64_mime / xxencode_binhex: not_collected (yenc_decode → None → raw lines joined, nntp.rs:609, 675,
  step.rs:116; no other decoder). The raw lines are in hand for the probed articles.
- plain_text: shown (NFO text, SFV table; step.rs:110-130; web.rs:1411-1436). Only .nfo/.sfv/.txt names, 3 files max.

### decoded_payload (as far as it is in the yenc block)
- magic: shown (Packing: Format/Method/Encryption/First file/Unpacked/File CRC32; Layers; Tracks; web.rs:1305-1387).
- first_16k_md5: not_collected; in hand (`front` holds the decoded first article(s), step.rs:139-157).
- full_file_crc32_md5: shown piecemeal (yEnc file CRC32, inner-file CRC32 web.rs:1326, SFV CRCs 1421-1436); not
  reconciled with each other or with PAR2 MD5s (release_fingerprint).
- opensubtitles_moviehash: shown (web.rs:1388-1390) + OpenSubtitles match (1391-1403); column + index.

### yenc_additions
- encoder_fingerprint_tuple: not_collected; in hand for the first/last article.
- first_article_file_key: stored_not_shown in pieces (file_size, pcrc32, segment_size, total, name all in the JSON;
  no key is computed or indexed).
- crc32_interval_algebra: not_collected; needs every part's pcrc32 (one body per part) — not in hand.
- node_yencode_issue_3: not_collected; in hand (last byte of the first/last article).
- uuencode_frame: not_collected.
- decoder_caveats: unclear/not a field — Atlas uses its own decoder (nntp.rs:2744-2772); PR #55 swaps in the
  `nntmux-yenc` crate with a line-by-line shim (§10).
- late_article_join_key: not_collected (late articles join through the Subject-derived file key; store.rs held_back).
- v8_random_state_recovery: not_collected.
- encoder_tells (=yend formatting): not_collected; in hand.

### derived_per_article items that need headers or bodies
- message_id_grammar: stored_not_shown (segments.local/domain, domains.suffix; nothing classifies or searches domains).
- message_id_timestamp: not_collected (derivable offline from stored ids; no fetch needed).
- date_shape: shown only as the raw Date/Injection-Date/NNTP-Posting-Date of one article + `posted_date`; no deltas,
  no per-article dates (dropped at save, §2).
- bytes_lines_ratio: not_collected (`lines` dropped at store.rs:689).
- from_identity: shown (Poster/Email/MADE UP) for the release's single `poster`; the probed article's From is
  parsed_not_stored.
- part sizes: segments.bytes per article is stored (wire size, store.rs:689) and not shown per article; yEnc
  decoded part size is stored for the first article only (yend_size, begin/end); the last article's is parsed and
  dropped; others need a body each. `last_article_bytes_key`: stored_not_shown (segments.bytes).
- pcrc32: see yend.pcrc32.
- Path / User-Agent / X-Newsposter / Injection-Date / Date: shown (one article). X-Received-*: parsed_not_stored.
- header order: parsed_not_stored.
- lines_sequence_key: not_collected (`:lines` dropped).

## 7. Data model summary (who writes, who reads)

- `s{n}.release_probe(release_id PK, crc32, agent, info JSON, moviehash)` — writer: ProbeStep via StepWrite (applied by
  the stage writer in one transaction, engine.rs); readers: web.rs:1166 (`probe::read`), field_search.rs:213-217
  (crc32/agent columns), opensubtitles.rs:141-147 (moviehash).
- `s{n}.release_opensubtitles(...)` — writer: OpenSubtitlesStep (opensubtitles.rs:214-229); reader: web.rs:1167.
- `s{n}.releases(poster, posted_date, size, parts, ...)` — writer: indexer save (store.rs:1391-1397); readers: search.rs
  ReleaseRow (9-18), web.rs release_html.
- `s{n}.files(filename, subject, subject_part, expected, file_total, seen bitmap, touched_at, blob)` and
  `s{n}.segments(file_id, local, domain, part, bytes) without rowid` + `domains(id, suffix)` (store.rs:165-188;
  sealed into files.blob v1, blob.rs:1-19) — the only per-article store; readers: `file_segments` (store.rs:1902-1930)
  via `StepCtx::segments`, NZB building, release_info contents.
- `s{n}.pp_state(done, failed, rerun_mask, ...)`, `pp_queue`, `pp_walks(kind, top_id, before_id, redo_mask, group_name)`
  (schema.rs:21-76) and `meta.pp_stamp_{id}` (engine.rs:1103-1111) — how reruns/backfills are driven.
- `release_fingerprint(posted, content, par2_block, par2_blocks, posted_names)` (duplicates.rs:21-39) — twins/alts
  for the Copies panel (web.rs:1171-1181, 1555-1573).

## 8. Extension points

1. New field already in the probed bytes → add to `ProbeInfo` (mod.rs:52-78) as `Option<T>` or with `#[serde(default)]`
   (see §3), fill it in `run` (step.rs:141-160 for the first article; 176-180 for the last, where `article.headers`,
   `last_lines`, `last_body` are available) or in `derive` (mod.rs:118-140), render it in `probe_html`
   (web.rs:1243-1453; `dd` + `panel`). Old rows show it only after a re-probe (§9), unless the UI computes it from
   fields already in the JSON (e.g. yend_size, begin/end, pcrc32, yenc.part, info.file, raw Xref host/numbers:
   render-only, zero network).
2. More headers → extend `KEPT_HEADERS` (mod.rs:81-94) or store the full ordered `Vec<(String,String)>` alongside
   (header order, count, casing). Cap at 256 headers (yenc.rs:85) and 512 chars/value (mod.rs:100) — Spotnet X-XML
   blobs would be cut.
3. Second header sample for free → keep `keep_headers(&article.headers)` of the LAST article at step.rs:176-180 (e.g.
   `headers_last`), which gives Date/Injection-Date deltas across a post and a second Message-ID shape.
4. Per-part yEnc data for the articles already read → `read_part` (step.rs:241-250) returns only the body; returning
   `(YencLines, body)` would let the step collect `pcrc32`/`begin`/`end` for parts 1, 2..6, the middle (if changed from
   STAT to ARTICLE: costs a body) and the tail parts. For ALL parts a new fetch per part is needed (pcrc32 sits at the
   end of each article); that is outside `bodies_per_release`=8 and `body_fetches_per_minute`=600.
5. Searchable columns → `PROBE.added` (mod.rs:46) + `indexes` (31-44) + `SET_PROBE` params (49-50, step.rs:204-207) +
   `field_search::plan` `@probe:` conditions (field_search.rs:190-197, 213-217) + the "More fields" form (web.rs:884-912)
   + `Fields` (field_search.rs:29-31, 89-90). Existing rows need a one-off `update release_probe set col =
   json_extract(info, '$.path')` (SQLite JSON is built into the bundled rusqlite) or a re-probe.
6. Backfill of existing releases → bump `config_stamp` (step.rs:99-101; next free bit is 4 on main; PR #48 also claims
   bit 2, §10) so `seed_shard` inserts a `steps` walk with `redo_mask = probe bit` in every shard at start/reload
   (engine.rs:285-305, 1073-1112); or `atlas --reprocess --step probe [--group g] --apply` (reprocess.rs:25-40, 74-99,
   137-162). Walks run newest-first at `backlog_per_second/8` per shard (engine.rs:412-485), re-judge the rules, then
   the probe reruns in the net lane (engine.rs:585-700, 920-1010) with the body budget. `Reruns.on_growth` already
   re-probes a release that gained articles (step.rs:93-95, engine.rs:599-601).
7. NNTP-level fields (line endings, status texts, :lines/:references/Xref column, per-article Date/From) → nntp.rs
   `read_line_capped`/`article_raw` (548-635), `parse_overview`/`into_article` (2712-2728, 102-116) and the segments
   schema/blob format (store.rs:181-188, 689; blob.rs) — i.e. the indexer area, not the probe.
8. A "not probed because …" line on the page → `release_html` around web.rs:1163-1170 (today `unwrap_or_default` hides
   the absence); the reasons are knowable from `pp_state.done/failed` bits and the body gate (rules.rs:197-209).

## 9. Backfill mechanics and cost

- Trigger: `config_stamp` bit → one `pp_walks` row per shard (kind "steps", redo_mask = 1<<4), created when the stage
  starts or its config reloads (engine.rs:285-305, 1103-1111). Documented in docs/port/03-postprocess-stage.md:828.
- Walk: newest release first (`order by r.id desc`, engine.rs:434-437), `backlog_per_second` (2000 default,
  config.rs:628) split over 8 shards, batches of WALK_BATCH; each visited release is re-judged (rules) and the probe
  bit cleared so it runs again (engine.rs:585-600).
- Network cost: the probe itself: 2–8 requests (ARTICLE up to 4 MiB each, one STAT) per release, 600 requests/min
  budget shared with the archive steps (bodies.rs:102-122), 2 body connections (config.rs:630), 120 s step deadline
  (engine.rs:44). README.md:405 estimates 15 M of 118 M releases pass the body gates; at ~4 requests each and 600/min
  that is roughly 70 days of continuous probing for a full re-probe, and ~14 TB of article traffic for the archive
  steps alone. A render-only change (fields already in the JSON) costs nothing. A JSON→column SQL pass costs one
  UPDATE over every release_probe row per shard (size unknown here; see open questions).
- Re-probing also REPLACES health (first/middle/last) and every other value (insert or replace), so "when it vanished"
  is unknowable without a history table.

## 10. Risks and conflicts

Perf/schema:
- `release_probe.info` is read whole per page view (fine) but is not queryable; every new searchable field needs a
  column + partial index per shard, created on open (schema.rs:114-131) — cheap for empty columns, but the fill is a
  full-table UPDATE or a re-probe.
- Serde strictness (§3): adding a non-Option field without `#[serde(default)]` blanks the probe panels for every old
  release. `layers` shows the right pattern (mod.rs:70-71).
- `insert or replace` loses history; a per-provider or dated health needs a new table (e.g. `release_probe_health`).
- Header blobs: 512-char cap and BTreeMap dedupe (a repeated header name keeps only the last value, mod.rs:100).
- Per-article storage (pcrc32/yend sizes/dates for all parts) would land in `segments`, the largest table
  (`without rowid`, PK (file_id, local, domain)) and in the sealed `files.blob` v1 format (blob.rs:1-5): a schema and
  blob-version change touching compaction (`compact.rs`) and `file_segments` — high effort, indexer area.
- Body budget: adding fetches to the probe (e.g. middle ARTICLE instead of STAT, or more parts) eats into
  `bodies_per_release` (8) that the archive steps share, and `there()` already returns None once the budget is gone
  (step.rs:64-71) so health goes missing on busy releases.
- `applies` excludes `*.jpg`; `body_gate` excludes small/incomplete/image releases: "every release page" cannot show
  probe data for them — the page should say why (today it is silent, web.rs:1163-1170).
- First article > 4 MiB → `Got::TooBig` → health true, no yenc/headers (step.rs:67, 145).

Open PRs / branches (fetched as refs/pull/N/head; GitHub API is not reachable from this session, so titles are the
head commit subjects):
- **PR #48** (feat/release-page-media-cover, based on a29ee52): adds `release_cover` table (`probe::COVER`,
  mod.rs:+47-112), `SET_COVER/DROP_COVER` writes in `ProbeStep::run` (step.rs:+192-202), changes `probe_html`'s
  signature to take `cover: Option<i64>` and adds an Attachments table, cover panel, Muxed/aspect/primaries/MaxCLL
  lines, a `/ui/cover` route, `nzb_files_html` and a "Files in the NZB" table (web.rs diff), and sets
  `config_stamp() = 3` claiming bit 2 — which main already uses for nested archives (#52, step.rs:96-101). Merging it
  as-is would NOT re-probe releases probed after #52 for covers; the cover bit must become 4, and any new probe
  field's backfill bit must be chosen after both.
- **PR #51** (feat/mid-file-keyframes) and **PR #54** (feat/h264-intra-decoder, contains #51): add `src/probe/stills.rs`
  (new `StillsStep` StepId(6), table `release_stills`, schema.rs and steps.rs +1 each), make `main_file`, `fetch_raw`
  and `read_part` `pub(super)` in step.rs, and add `moviehash::volumes` (moviehash.rs). Any change to those three
  helpers' signatures (e.g. returning YencLines from `read_part`) conflicts; `src/probe/mod.rs` gains `pub mod stills;`.
- **PR #55** (yEnc via the `nntmux-yenc` crate): rewrites `yenc_decode` (nntp.rs:2744+) around
  `nntmux_yenc::decoder::decode` with a `push_yenc_line` shim; adds a git dependency. Any plan to record escape
  sets/line stats inside the decoder, or to keep CR/LF, must be written against this version. Semantics note from its
  comment: the shared decoder drops CR and LF even after an escape.
- **PR #57** (instant-sort): +495 lines in web.rs, all in the search/sort region (hunks between web.rs:5 and ~730), not
  in `release_html`/`probe_html` — same file, low textual-conflict risk; also touches api.rs.
- **PR #58** (Decode compressed RAR5 layers) and merged **#56** (origin/main): `src/probe/codec/*`, `nest.rs`, `archive.rs`,
  and the `pub mod codec;` line in `src/probe/mod.rs` — every probe-area PR adds a `pub mod` line near mod.rs:4-11, so
  mod.rs merges will conflict trivially; `ProbeInfo` itself is untouched by #56/#58.
- **PR #21** (feat/keep-per-article, "Keep uploads obfuscated per article") has no merge base with main (rebased or
  historical); its head touches filters.rs/nzb.rs only — unrelated to probe storage despite the name.
- **"stream-details" branch**: no such head on origin (`git ls-remote --heads origin` lists feat/*, fix/*, port/*,
  instant-sort, backfill-throughput …); could not be assessed.
- **Finish web UI thread's twin grouping**: the Copies panel code (`copies_html` web.rs:1555-1573, `copy_card`
  1463-1548, `duplicates_page` 1575-1655) sits right next to `probe_html` (1243-1453); edits there and a bigger
  probe_html will merge but reviewers should expect adjacent hunks in web.rs. `copy_card` does not read
  `release_probe` today, so probe fields on twin cards would be a further change.

## 11. Open questions

1. How big is `release_probe` today (rows, avg `info` size)? Decides whether a JSON→column UPDATE is minutes or hours.
2. Should "stored, never shown" yEnc values (part, begin, end, yend_size, pcrc32, info.file, raw Xref) be rendered
   as-is (render-only, no backfill) or only after a re-probe that also fixes their one-article scope?
3. Is a second header sample (last article) worth a serde shape change (`headers_last`), or should `headers` become an
   ordered list `Vec<(String,String)>` under a new key to keep order and all names?
4. Per-provider health needs the server key in the result: `BackgroundBody` has none (nntp.rs:1056-1062). Add it?
5. Which `config_stamp` bit does the cover PR end up with after #52? Needs a decision before another bit is claimed.
6. Should probe fields reach the API (newznab attrs)? Nothing in api.rs reads them today.
7. SQLite JSON functions: confirm `json_extract` is compiled in the bundled build (default since SQLite 3.38; rusqlite
   0.40 bundles a newer SQLite) before planning a SQL backfill.
