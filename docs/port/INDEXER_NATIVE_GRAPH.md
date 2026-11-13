# Native indexer graph transition

This source checkpoint removes `MainConnection::Sqlite`, the shard writer/first-job queues and their callbacks, SQLite housekeeping, and `store::Prepared` from the parse/save path. `Db::save` delegates to the existing native admission implementation. The native pool/Owner distinction, original-owner checks, cache admission, cursor API, cancellation cleanup, NXV durability wait and unsaved-header ownership remain. `Indexer::new` now takes a checked `Db`; old SQLite fixture callers must supply a native fixture before the complete test graph builds.

This component is not a standalone build claim: legacy BG/Spotnet/inferred/repair callbacks are being retired or connected to their existing native services in companion slices. The overview owner's `reread_window`/shared-range work remains separate.

All tests below are **UNRUN**. Rustfmt parsing and `git diff --check` are source checks only.

## Retained and ported controls

The following existing tests retain their assertions and now use fresh migrated PostgreSQL schemas rather than local SQLite databases:

- `indexer::tests::each_speculative_window_has_to_hold_enough_on_its_own`
- `indexer::tests::a_speculative_window_is_judged_once_every_day_of_it_is_done`
- `indexer::tests::a_chunk_taken_over_leaves_the_older_days_and_the_window_alone`
- `indexer::tests::cursors_saved_under_a_servers_old_key_are_adopted_once`
- `indexer::tests::plain_cursors_stay_with_the_server_that_kept_them`

Existing PostgreSQL save/cursor and both cancelled-claim controls remain; assertions invoking the deleted SQLite callback APIs are removed. Pure refusal, range coverage, host identity, split/date, name-source and speculative-window arithmetic controls remain.

## Retired engine-specific controls

The following tests exercise the removed shard engine rather than a supported native interface:

- `a_slice_nobody_waits_for_isnt_saved`: removed SQLite queue's receiver-presence rule. Native cancellation retains its existing different contract: admitted blocking saves keep their resources through completion; cancelled callers replay before cursor acknowledgment. Existing `pg::ingest::tests::cancelled_callers_do_not_release_inflight_admission` tests this actual native behavior.
- `an_idle_writer_still_seals_what_is_due`: removed SQLite blob-sealing housekeeping. PostgreSQL stores typed inventory directly; NXV log durability remains required in `parse_and_save`.
- `a_job_on_the_firsts_lane_runs_ahead_of_queued_stage_work`, `jobs_on_the_firsts_lane_run_in_order_and_before_the_writer_stops`, `a_stage_job_runs_on_the_writer_and_answers`, `stage_jobs_dont_hold_up_saves`, `stage_jobs_run_once_saves_go_quiet`, `queued_stage_jobs_run_before_the_writer_stops`: removed arbitrary SQLite StageWork/first-job queues. Native stage ownership, target observation and receipt settlement remain covered by the existing native queue/worker suites; this checkpoint makes no new qualification claim for them.

Required whole-graph validation includes all retained cursor/chunk/ingest tests, the original-owner/cancel/drain suites, actual multi-provider loopback fixtures after their native fixture port, and the full shipping target build after companion source closure.
