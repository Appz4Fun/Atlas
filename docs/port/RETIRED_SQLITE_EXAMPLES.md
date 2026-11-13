# Retired SQLite diagnostic examples

The fresh PostgreSQL catalog removes three examples whose operations depended on
retired SQLite storage. Their Cargo example targets are retired; no successful
no-op, compatibility alias, feature gate, or new write authority replaces them.
No database, sidecar, cache, configuration, credential, asset or article file is
removed by this source change.

| Retired target | Original operation | Reason for retirement |
| --- | --- | --- |
| `zz_catalog_seed` | Read selected SQLite shards under a compaction hold and fill the old PostgreSQL shared-catalog mirror using `ATLAS_CATALOG_URL`. | The shard/mirror converter is retired. A fresh native catalog does not seed old releases through this route. |
| `probe_search` | Time SQLite search-table warming, category-filtered FTS, per-connection reads and shard metadata/password lookups; optional `--warm`, `--plain`, `--fresh`. | Those readers and warming tables are retired. The current bounded native `ReadView` and its original-owner callers remain internal APIs; this diagnostic does not expose them or open a substitute backend. |
| `bench_writes` | Generate header slices and benchmark SQLite `ShardWriter`, ID allocation, per-shard transactions, WAL checkpoints and database-file growth on a copied old catalog. | Those writer/ID/checkpoint/file-size operations no longer exist. A native benchmark would need the genuine owner, shared ingest admission and header-store acknowledgement; changing this example into one would be a separate tool design. |

Native production ingestion, PostgreSQL writer/read tests and existing bounded
reader/owner APIs remain. Their tests are not claimed as equivalent performance
benchmarks or replacements for these retired commands. Other examples, including
pure grouping/filter/pipeline/parity benchmarks and NNTP diagnostics, are unchanged.

## Original source and coverage

The complete original files remain in Git commit
`c9fa17f87f90d668d00cffa11e79dfdd9c53141b`:

| Path | Git blob |
| --- | --- |
| `examples/zz_catalog_seed.rs` | `bafa6eae1fe05caa791a21b4967c7d4e66b8d9cc` |
| `examples/probe_search.rs` | `5eb4109cd7b5b14ff0918af3fa3f125bc15fb138` |
| `examples/bench_writes.rs` | `414fbb685f0df904b47c032eb64f74990a57c2cf` |

For source inspection, use `git show <commit>:<path>` with that commit and path.
Historical grouping design notes may still refer to the old benchmark; they do
not describe an available current example target. Offline preservation/import
artifacts and original data remain subject to their separate retention rules.

None of these three files contained unit tests, and Cargo.toml has no explicit
entries for these targets. Removing their files removes their auto-discovered
example targets without a Cargo dependency or lockfile change. No parser, codec,
provider, cache or owner regression assertion is retired in this slice.

This checkpoint is source-only: all builds, all-target compilation, tests,
PostgreSQL and example executions are **UNRUN**. The coordinator owns normal
BIN/LIB and all-target qualification of the combined graph.
