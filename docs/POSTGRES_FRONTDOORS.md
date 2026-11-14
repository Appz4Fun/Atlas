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
| `--archive-par2-cache --source-baseline-sha256 SHA256 --apply` | Explicit original-Owner maintenance: retain inherited PAR2/preservation heaps cold and create an empty bounded live scope |
| `--dump-ext`, `--restore-ext DIR` | Existing native external-cache dump/restore, checksums and destination checks |
| `--save-replay` | Requires both `--pg-database` and `--pg-schema`; its own parser checks the isolated staging destination and scratch paths |
| `--header-store`, `--overview` | Independent NXV/overview tools; no product catalog configuration is required |

Every command line is checked for retired operations before tool
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
`--count-repair-candidates`, `--split-reposts`, and the standalone SQLite CRC report `--crc-links`. No command is a successful
no-op, and this retirement is not a claim that the old CLI operations were ported.

Current naming, PreDB, media, repair and other configured native workers retain
their actual typed mutations, ownership and outage behavior. Native external-cache
backup/restore protects valuable PreDB/API/costly caches; this source change does
not authorize deleting original data, caches, assets or recovery copies. Old
converter/maintenance source remains recoverable in the frozen pre-cutover Git
history. Current native CRC/key/file associations retain their algorithms and typed steps;
retiring the standalone report does not retire those pipeline behaviors.

## Build and qualification status

The production source now removes the SQLite indexer/storage engines, connection
adapters and legacy cache/read/command halves, retaining their native or pure
successors. The direct `rusqlite` manifest dependency is removed. Root separately
refreshes the lockfile and performs the first normal BIN/non-test LIB build;
source checks alone do not establish a SQLite-free compiled artifact.

Retained integration/support targets are still being ported to actual PostgreSQL
or pure fixtures. No `autotests` override, backend feature or SQLite development
dependency hides that open test graph. Native ownership, digest, lease/claim/ack,
cause propagation, NXV durability, schema migrations and costly-cache preservation
remain qualification requirements. Full supported-target builds, actual native
worker/Web tests, preservation and runtime no-fallback checks remain separate.
This source checkpoint is not service activation or deployment evidence.

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

## Explicit inherited PAR2 archival

Stop and drain the indexer before running `atlas --archive-par2-cache
--source-baseline-sha256 SHA256 --apply` against the intended checked configuration.
The digest references an already reviewed OLD-scope preservation checkpoint; it
is recorded as an operator reference, not manufactured COPY parity. The command
acquires the genuine native Runtime/Owner fence, starts no workers/providers,
and refuses mixed commands or missing `--apply`.
Its defensive preclaim checks may scan PP tables; every maintenance statement
has a five-second server deadline as well as a five-second lock deadline.
Timeouts refuse the operation and roll back the entire transaction, without an
automatic retry. Metadata-only archival describes the retained payload heaps,
not a claim that every ownership-admission check uses constant work.

One durable transaction renames exactly the inherited `ext_raw_par2`,
`ext_raw_par2_files`, `ext_preserved_cache_rows`, and their partial accounting
relations/sequence to closed `atlas_par2_cold_*` names. It copies or deletes no
payload rows. Cold tables reject writes. All other active cache relations,
including PreDB, HTTP/API, current probes and covers, remain untouched. Preserved
variants were already inert originals, not active fallback hits; they remain
physically retained cold, as do the separately preserved source and old backup.

The empty canonical hot relations keep the original schemas, indexes, guards,
and bounded retention. Their actual empty EOFs establish readiness for a NEW
scope. New posts use normal configured PAR2 processing and a genuine bounded live
cache. The immutable receipt records old/new OIDs and physical nodes, catalog and
owner identity, and the old checkpoint reference. Retry with the same reference
validates that receipt without rotating or discarding new hot facts. Any DDL
failure rolls back the whole transaction; ambiguous completion must be retried
only after the old original session has drained.

Backups record the source PAR2 scope. A different scope requires a reviewed,
explicit `--dump-ext --allow-shrink` baseline; the old parity checkpoint remains
about the old scope. The command never creates that baseline automatically and
never activates postprocessing or changes configured rates. This maintenance
entrypoint is not called by startup. Source changes and fixtures alone are not
operational admission or proof of production completion.
