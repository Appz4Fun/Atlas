# PostgreSQL product commands

Product startup requires a readable selected configuration with a usable `[storage]`
PostgreSQL URL and a valid schema name. The default backend is PostgreSQL, but its
empty default URL does not authorize an implicit libpq destination. Explicit SQLite
and unsupported backend names are rejected. Selected-file precedence, destination,
schema and plaintext/TLS checks remain in effect. Missing or invalid configuration
is refused before config migration, catalog setup, or provider startup.

The normal binary starts the existing native Web service path. Its independent
native API activation gate, authentication, asset selection, footer, and configured
worker admission remain unchanged. This source cleanup is not service activation,
production migration, a working-preview claim, or deployment evidence.

| Command | Retained behavior |
| --- | --- |
| No arguments | Checked PostgreSQL selection, then native Web startup |
| `--bg-indexer` | Existing native indexer ownership, queue and outage admission |
| `--selftest` | Provider login checks after checked PostgreSQL product selection |
| `--dump-ext`, `--restore-ext DIR` | Existing native external-cache dump/restore, checksums and destination checks |
| `--save-replay` | Requires both `--pg-database` and `--pg-schema`; its own parser checks the isolated staging destination and scratch paths |
| `--header-store`, `--overview` | Independent NXV/overview tools; no product catalog configuration is required |

Every command line is checked for retired or unavailable operations before tool
dispatch. Mixing one with a backup, replay, or pure reader cannot bypass refusal.

## Retired storage commands

The binary no longer dispatches shard conversion/compaction/reset, frozen storage,
sealed-log moves, shared-catalog mirroring, legacy search-index construction, or
SQLite-to-PostgreSQL import/export commands:

`--convert`, `--compact`, `--reset-shards`, `--sealed-log-move`,
`--shared-catalog-fill`, `--shared-catalog-check`, `--search-indexes`, `--thaw`,
`--frozen-stats`, `--export-keep`, `--move-ext`, `--restore-keep`, `--import-main`,
and `--import-history`.

No retirement command deletes data. Valuable PreDB payloads/feed watermarks,
external metadata and negative caches, natural-key probe/archive/PAR2/audio/still/
cover data, assets, configuration, credentials, and recoverable copies remain
protected. Historical conversion libraries still exist for the separate preservation
and shipping-graph work; they are not a runtime backend or accepted product command.

## Required maintenance ports

These contracts already lacked native product implementations and remain explicitly
refused. They are not retired storage features, successful no-ops, or permission to
remove the behavior:

| Contract | Required native successor |
| --- | --- |
| `--purge` | Existing age/class/settle rules, dry-run samples, bounded owned deletions, and stop/resume behavior |
| `--reprocess` | Step/group/failure selection, dry run, owned native queue writes and stale-result protection |
| `--predb-import`, `--replay-raw-predb` | Native feed/title/cache ingestion and payload replay while retaining external feed watermarks |
| `--reflag-obfuscated`, `--unname`, `--undo-mojibake` | Existing name-source selection, dry run, native row/identity/queue mutations |
| `--recount-articles`, `--count-repair-candidates` | Native persisted article/byte facts and PAR2 repair semantics, preserving bounded dry-run/apply behavior |
| `--split-reposts` | Native upload identity and release/file ownership across the supported list/id/server operations |
| `--crc-links` | Read-only native CRC association report with the existing sampling contract |

Existing SQLite command integration tests (purge/reprocess, naming, PreDB import,
and repair/repost reports) cannot qualify these future native successors. Preserve
their behavioral assertions while replacing the binary/storage fixtures with native
ones; do not treat new refusal tests as maintenance parity.

## Remaining build-graph closure

This finite stage removes product selection and command dispatch only. The indexer,
post-processing interfaces, generic SQL adapters, cache modules, conversion helpers,
SQLite-based tests, `rusqlite`, and its transitive dependencies still require their
separate cleanup. Native ownership, digest, lease/claim/ack, cause propagation,
NXV durability, schema migrations and costly-cache preservation must survive it.
A complete SQLite-free graph requires normal BIN/LIB, every supported feature and
target, actual native tests, and runtime no-fallback qualification. This change does
not claim that closure.

Focused new verification selectors are `--test pg_only_frontdoors`,
`app::native_cli_boundary_tests`, `app::presentation::tests`,
`config::tests::checked_storage_selection`, and
`pg::tests::storage_config_requires_a_postgres_destination_without_a_sqlite_default`.
Actual native backup/restore, replay/NXV, restart/outage, configured worker and Web
qualification remain required alongside those tests.

Specific retained binary-fixture transitions are `tests/pp_purge.rs`,
`tests/pp_reprocess.rs`, `tests/predb_import.rs`, `tests/naming_reflag.rs`, and
`tests/naming_unname.rs`: their success assertions need real native maintenance
implementations and PostgreSQL fixtures. `tests/compact_command.rs`, the CLI case
in `tests/predb_file.rs`, `tests/history_catalog_cli.rs`, and
`tests/move_ext_keep_source.rs` exercise retired conversion/storage commands and
belong with archived preservation tooling. The neutral default-home and TOML
migration tests now use retained `--selftest` admission without provider credentials.
SQLite API/NZB/provider-cache fixtures that depended on an absent configuration
must likewise move to explicit native destinations and owned cache scopes; an
unselected legacy fixture cannot authorize a product fallback.
