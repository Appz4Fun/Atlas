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

Every command line is checked for retired operations or the unavailable CRC report before tool
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

## Retired one-off catalog maintenance

The fresh PostgreSQL catalog does not upgrade the discarded SQLite header/release
state. Its one-off tools are also retired and rejected before any database or
provider work: `--purge`, `--reprocess`, `--predb-import`, `--replay-raw-predb`,
`--reflag-obfuscated`, `--unname`, `--undo-mojibake`, `--recount-articles`,
`--count-repair-candidates`, and `--split-reposts`. No command is a successful
no-op, and this retirement is not a claim that the old CLI operations were ported.

Current naming, PreDB, media, repair and other configured native workers retain
their actual typed mutations, ownership and outage behavior. Native external-cache
backup/restore protects valuable PreDB/API/costly caches; this source change does
not authorize deleting original data, caches, assets or recovery copies. Old
converter/maintenance source remains recoverable in the frozen pre-cutover Git
history. The old CRC association CLI `--crc-links` remains explicitly unavailable;
the retired SQLite report cannot be opened through the product.

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

The old command cases in `tests/pp_purge.rs`, `tests/pp_reprocess.rs`,
`tests/predb_import.rs`, `tests/naming_reflag.rs`, `tests/naming_unname.rs`,
`tests/compact_command.rs`, `tests/predb_file.rs`, `tests/history_catalog_cli.rs`,
and `tests/move_ext_keep_source.rs` concern retired SQLite maintenance/conversion.
Their relevant current-worker policy/parser/identity assertions must remain in
native or pure tests; old command success is not a PostgreSQL product contract.
The neutral default-home and TOML
migration tests now use retained `--selftest` admission without provider credentials.
SQLite API/NZB/provider-cache fixtures that depended on an absent configuration
must likewise move to explicit native destinations and owned cache scopes; an
unselected legacy fixture cannot authorize a product fallback.
