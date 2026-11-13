# SQLite graph test ports

This records implemented source ports. All ported tests remain UNRUN until the
coordinated graph build and actual PostgreSQL validation. Original reviewed native
checkpoints and qualification artifacts remain unchanged in their frozen sources.

| Existing control | New source control | Preserved assertion |
| --- | --- | --- |
| `raw_store::native_pg_opensubtitles_cache_tests::paired_sqlite_oracle_matches_native_cache_fields_nulls_keys_and_refresh_time` | `native_cache_matches_fixed_fields_nulls_keys_and_refresh_time` in the same module | Actual native writes preserve positive fields, negative/null rows and case-distinct keys, replace raw JSON according to contract, and refresh the positive row timestamp within the observed interval. Expected rows are fixed seed/upsert vectors, independently specified without another database engine. |
| Native cache children formerly initialized a legacy SQLite oracle or installed a legacy raw-store path override | Same native parent/child actions with an ordinary immutable sentinel file at the existing raw-store pathname | Native positive/negative/missing/costly/owner/worker assertions still run against real PostgreSQL; raw sentinel bytes remain unchanged, SQLite WAL/SHM and other unexpected files are refused, and the loopback HTTP listener observes no request. No legacy backend is selected. |
| Public cache API compatibility adapters accepted Option/void or checked Result | Adapters accept only Result | An API regression that loses storage errors cannot compile through the positive or error fixture. |
| `pg::outage::tests::nested_driver_io_is_transient_but_storage_io_is_not` wrapped driver/storage causes in `rusqlite::Error::UserFunctionError` | Same test using the real PostgreSQL driver cause through the native SQL error and anyhow context, and the independent filesystem cause through anyhow context | The owned loopback peer creates a real transport failure; it remains Unavailable, while an independent filesystem error remains Fatal. No SQLite error wrapper is needed. |

All other native cache/worker/asset/queue/stop/outage test selectors and assertions
in the cache module remain in place. This port does not replace native execution
with refusal tests, prove valuable production-cache preservation, or qualify the
unchanged original runtime artifact.
