# Agent Rules (`AGENTS.md`)

## Persistent Web UI Footer & Attribution Rule

Every full HTML page served by the Web UI (`src/web.rs` via `page()`) **must** include a small persistent footer (`<footer class="site-footer">`) populated at compile time (`build.rs`) with:

- **Edition & Authors:** `Atlas - Rust Edition. Authored by: [Eraxty](https://github.com/Eraxty) & Forked and ported to Rust by: [xbmc4lyfe](https://github.com/xbmc4lyfe)`
- **Version & Commit:** `Version: <latest Rust git tag or v0.0.0.1 if untagged> · last git commit: [<7-char short SHA>](https://github.com/Appz4Fun/Atlas/commit/<full SHA>)`
- **Copyright & License:** `© <compile-time year> · License: [GPL v3 (GNU General Public License v3.0)](https://www.gnu.org/licenses/gpl-3.0.txt)`
- **Toolchain & Build Timestamp:** `Rust Version: <compiled rustc version> · Compilation Date: <UTC compilation timestamp>`
- **Patreon Support Link:** `Donate to my Patreon to help with AI token cost and get prioritized features and personal support:` with a badge/link to `https://www.patreon.com/cw/xbmc4lyfe`

Keep equivalent attribution, build/version info, and a prominent Patreon support callout (`https://www.patreon.com/cw/xbmc4lyfe`) in `README.md`.

## Safe Database & Shard Reset Rule (Preserving Historical PreDB, Media, Probe & Raw Cache Data)

When resetting/wiping the database to test fresh shard schemas **without** having to re-fetch PreDB feeds, external metadata APIs (TMDB/TVDB/TVMaze/Trakt/OMDb/OpenSubtitles), or Usenet `BODY` probes:

### 1. Preferred Automated Reset Command
1. Stop running Atlas services first:
   ```bash
   launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.atlas.indexer.plist ~/Library/LaunchAgents/com.atlas.web.plist
   ```
2. Run the built-in shard reset command (which automatically harvests all natural-keyed `release_opensubtitles`, `release_probe`, `archive_release`, `archive_inner_files`, `release_nfo_password`, `release_stills`, and `backfill_deepest` rows from `atlas.s0..s7.db` / `atlas.db` into `atlas.raw.db` before removing the shards):
   ```bash
   atlas --reset-shards --apply
   ```
3. To re-parse all archived raw PreDB HTTP payloads in `atlas.raw.db` into `atlas.predb.db` after changing a PreDB parser or schema:
   ```bash
   atlas --replay-raw-predb --apply
   ```

### 2. Files That MUST NEVER Be Deleted During a Reset
Always preserve these schema-independent sidecar databases and content-addressed asset directories in `~/.atlas/`:
- **`atlas.raw.db`** (`-wal`, `-shm`): Permanent raw HTTP cache (`raw_http_cache`), raw PreDB feed log (`raw_predb_log`), and natural-keyed Usenet/hash enrichments (`raw_opensubtitles`, `raw_probes`, `raw_archives`, `raw_archive_inner_files`, `raw_nfo_passwords`, `raw_stills`, `raw_backfill_deepest`, `raw_group_bounds`).
- **`atlas.predb.db`** (`-wal`, `-shm`): Catalogue of PreDB release titles and feed cursors. On a manual reset, run:
  ```sql
  UPDATE meta SET value = 0 WHERE key = 'late_after_id';
  ```
- **`atlas.media.db`** (`-wal`, `-shm`): Resolved TVDB/TVMaze/Trakt/TMDB/IMDb IDs and negative cache keyed by `title_key`. On a manual reset, run:
  ```sql
  UPDATE media_titles SET fanout_done = 0 WHERE state = 1;
  ```
- **`atlas.details.db`** (`-wal`, `-shm`): Rich movie/TV metadata, cast, genres, Blu-ray dates, and ratings keyed by `(kind, tmdb_id)`.
- **`posters/`**, **`covers/`**, **`stills/`**: Content-addressed JPEG caches (`{kind}_{tmdb_id}.jpg` and `<sha256>`).

### 3. Files That ARE Safe to Delete on a Shard Reset (After Harvesting to `atlas.raw.db`)
Only delete the main header database, the 8 release shards, and sealed-log segment files:
```bash
rm -f ~/.atlas/atlas.db ~/.atlas/atlas.db-wal ~/.atlas/atlas.db-shm \
      ~/.atlas/atlas.s[0-7].db ~/.atlas/atlas.s[0-7].db-wal ~/.atlas/atlas.s[0-7].db-shm \
      ~/.atlas/atlas.s[0-7].db.sealed-log-cursor \
      ~/.atlas/segment-*.log
```
