# Retired storage engines

The PostgreSQL edition retires SQLite shard conversion, compaction and catalog
mirroring. Their commands already reject before any storage/provider work; native
startup owns its canonical PostgreSQL session and does not start those engines.
This source checkpoint removes the three root module registrations and their
implementations. The only remaining production-text references were in the old
release_info SQLite reader half, which is a separate mandatory caller closure.
No native configured service, parser, codec, filename/CRC/hash/identity algorithm,
backup/dump/restore or pure NXV/overview tool uses these engines.

`RETIRED_STORAGE_ENGINE_TESTS.json` records every inline test name, source line and
old file hash. These test their deleted conversion/compaction/mirroring machinery:
locks and swaps, WAL/backups, old shard schemas, outbox publishing, SQLite sorting
indexes and copying old tables. They are retired with those commands, rather than
rewritten as refusal-only claims of native behavioral parity. Shared codec and
current native identity, cache, query and save semantics retain their own pure/PG
controls. Old standalone integration targets for these retired commands require
explicit test-graph retirement separately; retained provider/grouping/worker tests
continue to be ported and must not be deleted as a group.

The original exact engine sources remain recoverable in frozen pre-cutover Git
history and this checkpoint's parent. No data files or valuable caches are opened,
changed or removed. Actual data retirement and final preservation parity remain
separate. Source retirement is not a complete build-graph, migration, deployment
or runtime qualification claim. All new graph builds/tests are **UNRUN**.
