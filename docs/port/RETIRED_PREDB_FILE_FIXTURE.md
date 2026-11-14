# Retired SQLite PreDB sidecar-file fixture

`tests/predb_file.rs` tested the removed `atlas.predb.db` sidecar and shard maintenance. Its baseline is commit `60b653aed79e70213b6484edfd7517a668fa3b3f`, Git blob `688d902cd22e2be11c9f8f251600ed5da74a2904`, SHA-256 `155025eac46091c5d96b4b7f574ced09205791c611dc5fd1d92724006b10eb7c`. The six selectors below are retired storage contracts; this is not a claim that their bodies were ported to PostgreSQL.

| Original selector | Retirement reason |
| --- | --- |
| `predb_file::the_path_sits_next_to_the_shards` | The removed `predb::db::path_for` selected a SQLite file beside release shards. Native PreDB uses the configured PostgreSQL destination. |
| `predb_file::create_is_idempotent` | The removed `predb::db::create` reopened that SQLite file and asserted WAL mode. |
| `predb_file::no_file_by_default` | The SQLite sidecar existence assertion is obsolete. Its independent `!Config::default().predb.enabled` assertion is preserved in `config::tests::predb_defaults_when_absent`. |
| `predb_file::a_wipe_keeps_the_titles` | The removed `db::wipe` deleted SQLite release shards while preserving the SQLite PreDB file. |
| `predb_file::compaction_leaves_the_predb_file_alone` | `--compact` is retired and refused before storage dispatch; SQLite sidecar bytes, mtime and temporary filenames are no longer product contracts. |
| `predb_file::conversion_leaves_the_predb_file_alone` | The removed `convert::run` rewrote a SQLite catalog while preserving a separate SQLite PreDB file. |

Native ownership remains in `src/pg/service_cache/predb.rs` (the original-owner PreDB service), `src/pg/predb_cache.rs` (native cache/imports), and `src/pg/service_cache/predb_late.rs` (native late matching). PostgreSQL migration idempotence remains covered by `pg::migrate::tests::migrating_twice_applies_each_version_once`. The production retirement boundary remains `app::retired_storage_command` and the check before dispatch in `src/main.rs`. Native PreDB behavior and fixtures are unchanged; no runtime data, files, configuration or preserved caches are deleted by this source change.
