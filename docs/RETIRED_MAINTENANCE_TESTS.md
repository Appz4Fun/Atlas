# Retired maintenance test ledger

Pinned source baseline: `5a69479413a5388b59403478a74c0c6ce0079c46`. This finite source-only change concerns only the fourteen explicitly assigned targets. No build, test, PostgreSQL, provider, service, database-reset, cache, asset or data operation ran.

Fresh-catalog scope retires one-off SQLite conversion/compaction/frozen/mirror/bulk-maintenance engines. Current native workers, naming/classifier/identity, providers and APIs remain supported. A removed CLI success is not relabelled as a refusal test, and a current behavior is not retired merely because its old fixture used SQLite.

Full-source hashes below bind the original files, including cfg/test attributes, imports and helpers. Each row additionally hashes the exact original `fn ...` definition through its column-zero closing brace, UTF-8 bytes with original newlines. Old selectors use `integration-target::function` notation; run the named function with `--test integration-target --exact function` when appropriate. New and retained tests are all UNRUN.

## Finite result and open work

Eight strict legacy targets are deleted. Four mixed/pure targets are reduced; two mixed targets remain byte-identical. Seven old assertions remain as selectors: six mixed behavior cases awaiting native ports and the pure no-HTTP decoder boundary. Two additional pure classifier vector selectors preserve the historical naming examples. The remaining mixed cases intentionally still expose their old fixture/API dependencies; this checkpoint does not claim the full integration graph compiles.

No unrelated `tests/common`, supported NNTP/provider/API/CRC/grouping test, production module, Cargo file, footer, README attribution, or preservation rule changes. Existing native/pure controls cited below were read as source, not executed or promoted into proof.

## Exact retained native ports

- `naming_reflag::a_named_row_stays_visible` — PORT-VISIBILITY: use an accepted native writer/typed rename result for an obfuscated raw name with display_name; query actual native search/release APIs and assert it remains visible. Retire only the obsolete reflag-command setup after that assertion exists.
- `naming_unname::naming_keeps_its_bit_and_failed_later_steps_clear` — PORT-NAME-BITS: real native Queue/StepRunner with a cleared-name marker, Naming done bit retained, later failed/done bits reset, attempts zero; no direct identity/receipt minting.
- `naming_unname_later::later_steps_are_queued` — PORT-NAME-LATER: real native owner/writer and typed clear/rerun path for one done and one failed later-step case; assert both queued, each later step runs once, and Naming leaves the restored raw name unnamed.
- `pp_auto_compact::the_stage_stops_for_a_compaction_and_carries_on` — PORT-WORKER-METRICS: remove the obsolete compaction trigger only after actual configured native workers continue after a genuine supported pause/drain boundary; assert pp_state progress and the exact stats.json queue_due/walk and status postprocess_backlog shapes.
- `pp_reprocess::the_walk_clears_the_mask_and_the_step_runs_again` — PORT-WALK-RUNNER: request a native walk for an actual accepted release; drive the real StepRunner, assert exactly one rerun, terminal done mask and finished walk; retain genuine settle/readiness observations.
- `pp_reprocess::a_walks_request_waits_for_a_release_still_settling` — PORT-WALK-SETTLE: real native writer with fresh touched input and settle2s; request redo through owned Queue, wait for actual StepRunner completion, assert rerun done bit rather than manufacturing timestamps or receipt state.

## Original selector ledger

### tests/auto_compact.rs

Original full-source SHA-256: `e995da39c1ff87341edd7237e819d82d00d7d999b84dc4188661ba6ca98a839d`.

**COMPACT:** Scheduled SQLite shard rewrite, inode replacement, last_compact marker, and shared file-lock handoff are retired.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `auto_compact::compacts_on_schedule_and_keeps_indexing` | `7db2991e5fcf8c0fac506b52bd5150e1412c642efb71d9d6043d585c7b56121e` | RETIRED COMPACT; engine/command rationale above |

### tests/auto_compact_reload.rs

Original full-source SHA-256: `edd9e1d66a1e63271474865891d450bb446e3a02a4ccd774f2a85ba84efbb895`.

**COMPACT:** Reloading the retired auto_run_compact engine is not a supported PostgreSQL worker behavior.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `auto_compact_reload::turning_auto_compaction_on_and_off_applies_while_indexing` | `5086050d0663cebc8c80b1222d60e1299b94bcc9d79b4b80e1b8d22bf7b8d168` | RETIRED COMPACT; engine/command rationale above |

### tests/compact_command.rs

Original full-source SHA-256: `8478c07357150373775be2059fb7e9a5a5954586acf4a18fcd9ece9ff7d49222`.

**COMPACT:** Old --compact dispatch, missing-main/shard swap recovery, and help advertisement of retired --purge/--reprocess commands are removed.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `compact_command::a_successful_compact_command_records_when_it_ran` | `d308e660699dbedbe6641409e074cb35e18e47b18ca7ceb22d84467eb9b33835` | RETIRED COMPACT; engine/command rationale above |
| `compact_command::a_compact_command_refuses_shards_whose_main_database_is_missing` | `00487787b9b4e08b518e26fd7550957bd33f4a2c9b3113c73ec8a56511c542f1` | RETIRED COMPACT; engine/command rationale above |
| `compact_command::a_compact_command_finishes_a_conversion_swap_cut_short_first` | `793d69abf14f2119b85a90fb1745b09a2e71ff2318a9e578e09c3cc460b28885` | RETIRED COMPACT; engine/command rationale above |
| `compact_command::help_lists_purge_and_reprocess` | `627391c797e50800f1482bc67e3f4ea2fcba52dec47911d35ea6951f2c66344f` | RETIRED COMPACT; engine/command rationale above |

### tests/compact_lock.rs

Original full-source SHA-256: `34c8094fb6ce25f32ff9773065a98b39256d6aaa072aa2d5150f62634301f931`.

**COMPACT:** Exclusion against a SQLite compaction file lock and precompact backup files is specific to the retired shard-swap engine; it is not the native Owner/session lock contract.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `compact_lock::the_indexer_and_a_compaction_lock_each_other_out` | `a2a6d93812db1445f802057b1a881fc6bdecc78074ca533e777cb63da40784cd` | RETIRED COMPACT; engine/command rationale above |

### tests/convert.rs

Original full-source SHA-256: `28a23edc6b8e0f7a639a907c0f8d43371a19793688e23f9ff18d0ffb81cb9ce1`.

**CONVERT:** Pre-shard SQLite release/article/cursor migration, id remapping, WAL checkpoints, resumable copy and file-swap recovery are not part of fresh-catalog startup. No old catalog is imported.

Valuable media/cache preservation remains required by AGENTS and the fresh-start cutover scope. `a_convert_leaves_the_media_cache_alone` concerned the retired converter only; deleting that fixture authorizes no cache or asset operation.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `convert::old_databases_convert_without_losing_anything` | `aa729962cc93245a60d71c1613820b7c1a379353581f96359f4f7a51a53e1d40` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_conversion_stopped_during_the_check_picks_up_there` | `def37851dc0ce702edf35582a70f8e6b0da726abd928cc05b4ac523bfb061a63` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_conversion_runs_alone` | `ea41664fca0d0758c7dfcf7c4bf1114aaa3225e961a447645f32b893aba3d015` | RETIRED CONVERT; engine/command rationale above |
| `convert::an_indexer_converting_at_start_runs_alone` | `13a606e6c1ae3cc543baa4c09e6adbdf2a61e3e728ddc4ca0eac627b1b966c2a` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_swap_whose_checkpoint_is_held_back_is_refused` | `6ebe6f454bd9c0cdf5a368e53a901266a823a2f2688621219ba7df8f4ba60802` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_stopped_conversion_copies_again_when_the_old_database_changed` | `9b2ad503dd94846fa031c31c9938954a2c0db99f5b55f1b3f9da52e820c2ff69` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_stopped_conversion_copies_again_when_a_release_changed_under_the_same_counts` | `188d793310c6cbb6c4824a571a9059a2e7745011fa2a472f5135223c554680da` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_stopped_conversion_copies_again_when_only_the_cursors_moved` | `73c8a78b21cef1250e4c0f54d0f0c234dce48c12734c96aa1c4a1fad540630e7` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_swap_cut_after_the_old_database_moved_aside_is_finished_at_start` | `e15a29ed9a955766a66dcb30b07eee525218d752d38e7c6c742a0fd109eb04fa` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_read_during_the_swap_makes_no_main_database` | `df3aa0abf54d7621f9bf07ec0ee0d90e70072fcdff6c1c148faf73ce36330e67` | RETIRED CONVERT; engine/command rationale above |
| `convert::an_empty_main_database_made_during_the_swap_is_taken_out` | `59c044b939e0ab0963d5f868245471fb133c2aff0422b28a3039c7736b63bb6c` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_main_database_with_tables_is_left_alone_beside_a_cut_swap` | `d75065033473699a17f6791fa2fecd55264b8844bc04cbd2fb8db42cc42ec602` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_convert_after_a_cut_swap_finishes_the_swap` | `9c838b1bfc827a644273f63cf35b490d83ec6b174fa5c5cd4a811bdaf2764a2d` | RETIRED CONVERT; engine/command rationale above |
| `convert::an_old_database_moved_aside_without_a_finished_new_one_is_put_back` | `202bb18405bbaaa57d1a811c015e172e45920e744da223f545821f5a7b6d455c` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_lost_main_database_after_a_conversion_does_not_bring_the_old_one_back` | `d32550c4cad98dc137459a7a28a64e004cf0576e99d786172b8ce6d5d7d17847` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_huge_sparse_release_id_converts_without_a_slot_per_id` | `eeee7c180eae7af113bc470bf7a300cf01df5484ef17b6ed8bc32003e7b3eade` | RETIRED CONVERT; engine/command rationale above |
| `convert::converted_shards_have_empty_stage_tables` | `0ca691f402f3373b9e45d9dfa24f2164b89fcec35d3176f5c186fbf49da713d3` | RETIRED CONVERT; engine/command rationale above |
| `convert::a_convert_leaves_the_media_cache_alone` | `71cd0dc7b84662e4f6b7820b2abedcd2f3dbedf2e222aa0c33d98ea4758be500` | RETIRED CONVERT; engine/command rationale above |

### tests/move_ext_keep_source.rs

Original full-source SHA-256: `d3a6dd37568a76866c716d9ba929491a1698bdd3f082c1a197bfd454507e3fc7`.

**KEEP-CLI:** Retired --move-ext/--keep-schema preservation CLI mixed a SQLite source fixture with selected PG schema import. This retirement does not remove cache preservation, authorize deleting sources, or replace native external-cache backup/restore qualification.

Native ext_* backup/restore and independent valuable-source preservation remain required. No runtime preservation or source-integrity proof is claimed by this source-only deletion.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `move_ext_keep_source::cli_selected_partial_keep_adds_missing_facts_and_leaves_source_unchanged` | `fc2cc56383e02811a761545fa1ecc1c2d352400a6122be0abc3ae71213655d5a` | RETIRED KEEP-CLI; engine/command rationale above |
| `move_ext_keep_source::cli_rejects_invalid_sources_without_committing_destination_changes` | `9fef479b536c5bb8218d669a80e517b03ad84f11b46c50202669b5d041e7e08f` | RETIRED KEEP-CLI; engine/command rationale above |

### tests/naming_reflag.rs

Original full-source SHA-256: `18a21a8db832c4adc98b26c5d2d42669fe51351ae710399ef960c6e760bd88ee`.

**REFLAG:** One-off --reflag-obfuscated command counters, legacy flag rewrites, samples, cursor/WAL resumption, locks, and command help are retired; current classifier policy remains.

Pure golden vectors (collision suffix normalized by the current public `naming::without_collision_suffix` boundary before raw classification): `naming_reflag::current_classifier_vectors_survive_reflag_retirement`; existing `naming::obfuscation::tests::{flags_random_shapes,leaves_real_names_alone,everything_the_old_rule_flagged_stays_flagged}`. Naming precedence remains `naming::decide::tests::a_subjects_scene_name_doesnt_beat_a_better_source`. These are classifier/precedence oracles, not command or native visibility equivalence.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `naming_reflag::a_dry_run_changes_nothing_and_counts` | `52b4139da7c44b17100cb30ff2d1b0d254669b542f796b6f1b7fc04469845214` | RETIRED REFLAG; engine/command rationale above; classifier vectors retained in new pure selector |
| `naming_reflag::apply_sets_the_flag_and_search_follows` | `66dd48744d866da73fc17152566f6fdb247e316d8d9a8fedc259dc0606431dbd` | RETIRED REFLAG; engine/command rationale above; classifier vectors retained in new pure selector |
| `naming_reflag::a_named_row_stays_visible` | `75f676f1d028498c1e23179bfb53e4772ae976eef2f50c6fcd27a7d45be36280` | RETAINED MIXED, same exact selector/body; native port listed above |
| `naming_reflag::a_step_named_row_is_left_alone` | `b307f2cfc23c4b4fafddad5b33f342c6ed0b70824b392754f928d50797d76065` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::a_second_run_finds_nothing` | `292832c0c9edc20da1d91316422a68f15533a6d64ee8b7d5c77e6b55d2014a12` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::a_stopped_run_resumes` | `51429bb8b26167f4c530bb70c13f28ea388e4024333cb87f467a3c8744c1a7dc` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::apply_is_refused_while_anything_holds_the_lock` | `1b2ffd6745677320c57826fbfafb443f89fa1cb88b8014e291196bb8cd7d5b74` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::it_is_refused_with_an_indexer_alive` | `a6d05d6672b400dcd2722f7c17d5f845a6a7286783a91068f5eed3b3262fe16c` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::a_reader_holding_the_wal_stops_it_cleanly` | `47c3729a42632fd9c873f4cb79e473b383b21cb8cd83e9100502ed7a814674aa` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::reflag_runs_after_a_convert` | `0972d31949b0bac0b6474e2d0314882da7a37837cdd03205da9cad3457cd5ab5` | RETIRED REFLAG; engine/command rationale above |
| `naming_reflag::help_lists_it` | `81ed9cb34f2666848260ca8dcfb418c8b47f7a773589cca7ce0e8fc99ce2bbd9` | RETIRED REFLAG; engine/command rationale above |

### tests/naming_unname.rs

Original full-source SHA-256: `5e417b9b1a62a863a2e88d4ea43f8b4d02fb64d66b8f950056c5b49e0c38d015`.

**UNNAME:** One-off --unname/--reprocess command selection, clear-source filters, opt-in cleanup, FTS repair, resume/lock state and help are retired; current naming worker semantics remain.

Pure golden vectors (collision suffix normalized by the current public `naming::without_collision_suffix` boundary before raw classification): `naming_unname::current_classifier_vectors_survive_unname_retirement`; naming `current_name/effective_source` and the existing `naming::decide` source-precedence tests remain unchanged. Mixed rerun-bit behavior is preserved explicitly below.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `naming_unname::a_dry_run_changes_nothing_and_counts` | `8eab813eabb04ac9a7dd9f3e67e6924a430219fa6865e72f8976fd3b199295d8` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::apply_clears_only_regex_and_posted_names` | `24882815689951f9e22c6c69e65e796444256d91742f48696d3caaa94654df2d` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::search_follows` | `483af53699c74e214baa0a2366affb1e8445edf2f2adc97e8ab59e1d1ecb5b72` | RETIRED UNNAME; engine/command rationale above; classifier vectors retained in new pure selector |
| `naming_unname::it_removes_the_opt_in` | `cbc01a730fe1bf285ed9e0e4093bd9cd81e02358f31d6c0f36b56f75680372a9` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::naming_keeps_its_bit_and_failed_later_steps_clear` | `2590c1f030c90ce311427723b9e046b0f267d9d4045aad6cdf2b7c96ef244130` | RETAINED MIXED, same exact selector/body; native port listed above |
| `naming_unname::a_stopped_run_resumes` | `ada17561a99302830e16820c58d6a3dc3ed56d7df1739b6f7a11ddb08f391d77` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::apply_is_refused_while_anything_holds_the_lock` | `b7da862c9c7ee7b9dcea9940fb43841a18fe01ef7a0eee61aabd7af03114300f` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::it_is_refused_with_an_indexer_alive` | `44a30c663f57035c7754425752b4b8975ee98eb4a5b47454691e5b3b9e1a9e6f` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::a_second_run_finds_nothing` | `0ff9e74336acf07a321d23cae0ab3a13ec854e22ba97da431bba9922a23da515` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::help_lists_it` | `64c11cc04f6630fef8e85ccd3a92a16b09032e5ff66f2446700d66303d86d5cf` | RETIRED UNNAME; engine/command rationale above |
| `naming_unname::archive_nfo_is_refused_with_the_list` | `cee69b904ff99297247fa9e80a1cb58e8f2a7ea973d6bfabba2c5b2830d88e04` | RETIRED UNNAME; engine/command rationale above |

### tests/naming_unname_later.rs

Original full-source SHA-256: `de52d8c4a2281d5332cd9af4926ff97792c4316aaddb000f35442ca538294b96`.

**MIXED:** The same case checks current Naming and later-step execution after clearing a name; no portion is retired in this checkpoint.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `naming_unname_later::later_steps_are_queued` | `0f9eb2b7bac1ff13fab208ac21dbb1c0e6a0ea21c1bf8f9b7a72d49e10a8f67a` | RETAINED MIXED, same exact selector/body; native port listed above |

### tests/pp_auto_compact.rs

Original full-source SHA-256: `b3c7b87fc34ec8b2608df4de680cd93c7ec98112610887ac111bcb15afcb46a8`.

**MIXED:** The same case checks actual postprocessing continuation and stats/status schema after the obsolete compaction trigger; preserve the full case until a native worker/metrics split exists.

Supporting native worker join/commit tests in `src/postprocess/native_workers_tests.rs` remain unchanged; exact legacy metrics-and-continuation assertions are retained here pending a dedicated port.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `pp_auto_compact::the_stage_stops_for_a_compaction_and_carries_on` | `7b08e001ebead8a3cf9dfe0ad1b68bbb95ef7f15fe8e54b98e87ef9024d5593f` | RETAINED MIXED, same exact selector/body; native port listed above |

### tests/pp_purge.rs

Original full-source SHA-256: `a8170018dd3a071eab8313326e8d34a18bfb33f4887e79be72fdd3aec16e57b8`.

**PURGE:** One-off --purge class selection, dry-run samples, destructive bulk deletion, shard totals/FTS/GC, huge-release chunks, cursor/WAL resumption, locks, and subsequent --compact are retired. Existing worker Rules remain separately tested; their current size/type/completeness/body gates are not asserted equivalent to obsolete CLI purge selection.

Current pure worker policy: `postprocess::rules::tests::{judging_a_row,judging_files,the_body_gate,over_a_thousand_native_files_one_article_short_is_not_dead}`. The new body-gate/settling policy is kept as defined by current source; no old purge deletion behavior is promised.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `pp_purge::a_dry_run_changes_nothing_and_says_what_it_would_do` | `99dcbd4b70cd437fefde85bf3b1247bc7d9f496514ba490ff74092b26cd6edd4` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::no_flag_means_fragments_only` | `5d8d86975d706b62076acf0d8e0546dc153a19f956603af061e0cc06263ff2c6` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::a_fresh_post_is_not_purged` | `9f35c3ce08c68025cc0979e3733bb5a8a39b67fcd24487bd2d8f5fd863e74607` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::the_dry_run_names_blocked_groups_and_sizes_small_ones` | `0a538100b257e9b4dfc6bcd19542588377447fc24aa4fce0c2efe7cfafffdb4e` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::search_and_totals_are_right_after_it` | `f8941c062bf804ceed423b7db0c37e42061b081ea5cf2754818eba16708bff67` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::a_stopped_purge_resumes_without_counting_twice` | `0fd0f9726ebd89811a85b518668477c10d8d3ea643cedd1adebbdb9dbc1f1a57` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::other_classes_start_over` | `faaf8f4cee319c0e3c78832d98248085648a8388e8db4fae610c7b8dd16cfdfa` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::a_huge_release_goes_in_chunks_and_is_finished_first` | `133611fe5d369f72de35e3edb4557ce2a35e3838415223dba75cb9b6c512ed8f` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::big_batches_commit_in_pieces` | `1b5ad3a0d892127cb3c33252933b3b9a0b68ef6b3a8a8e7f60b00ab10760e5c9` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::a_reader_holding_the_wal_stops_it_cleanly` | `9fef246af245d29ac131048372eaf37be2fb7843a485ff759d03d77eb92dc1d1` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::apply_is_refused_while_anything_holds_the_lock` | `f64951cbacb784ae70e6c3bfd9af2dbcbf688de4dce1beadd7e0e502718dd3ba` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::apply_is_refused_with_an_indexer_alive` | `ae702ea35dcffd24de0158b0cebc5b50df124421d425d9c73c0dc31d9692954e` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::then_compact_shrinks_it` | `ecd3b754f3870b75c64dd0312cb0738c333084fe5d11d2bedb2513ecbd05a5e3` | RETIRED PURGE; engine/command rationale above |
| `pp_purge::another_size_limit_starts_over` | `163ec6e3bc9c8e865b23815cb1816d1e233fc4664c79ed3c01fc456b25ab9ffe` | RETIRED PURGE; engine/command rationale above |

### tests/pp_reprocess.rs

Original full-source SHA-256: `bc411852bc7cfca6fc4db43a4df19acffee34142da18bd0f11c7d7a8522bd2ce`.

**REPROCESS:** One-off --reprocess CLI dry-run/listing, per-shard request counts, --failed/group filters, old locks and upgrade path are retired. Current walk result/settling cases are preserved below.

Native walk controls remain unchanged in `src/pg/stage_walk_tests.rs`, including idempotent seed, redo-mask/atomic receipt/cursor handling and future-due ownership. They are supporting native controls, not full replacements for the two preserved actual StepRunner oracles.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `pp_reprocess::a_dry_run_prints_counts_and_changes_nothing` | `42ee654074572b24a22b0adfc6fbf44855bba02713786f316d3d665534587139` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::an_unknown_step_is_refused_with_the_list` | `e4ca00610357d0825557572a61266570d4a88660418550a66928e09ce31787e7` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::apply_adds_one_walk_per_shard_and_no_more_on_a_second_run` | `43aaae5474bc0d08c75bd8b4226f6ae4d329b547c38c031df3bb30a9a64ce5d9` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::the_walk_clears_the_mask_and_the_step_runs_again` | `11b12338d8c889b08f0cbb381ca45a4de0c54b67d9e41358557439a90e77fea5` | RETAINED MIXED, same exact selector/body; native port listed above |
| `pp_reprocess::group_limits_it_to_that_groups_shard_and_rows` | `1d14de5f0e8e1d97f284e5c57c4ac69011307011ebaf8967cd8debe5635b5322` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::failed_clears_failed_bits_and_retries_now` | `07d07b5078f1435bc7a0c2e695b93ccff5d32012b8a1ff2ce7f1a2ec65736cbf` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::failed_with_a_group_leaves_other_groups_failures_alone` | `738a57b0e7fdad360686cee23943dadf66191f8e1b0297a4693af5e73b33f377` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::it_runs_beside_a_writer` | `c77c3052d485ac6ef03543935c44bc1b26c01d884d1ce7e34644540c39671e0d` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::it_is_refused_during_a_compaction` | `d81795b6fbd264585e0a93b70269db3a4a359d57baf713790f9c95942d0bc9c1` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::a_walks_request_waits_for_a_release_still_settling` | `ac82664d7a1e9f544826e750d47c8b97fffe86b51d39076785f3687e8447c137` | RETAINED MIXED, same exact selector/body; native port listed above |
| `pp_reprocess::the_dry_run_counts_failed_releases` | `c4453d6f99efb22b07b9ddb0229ebeb59768fae95aaa11d9f2679c048fa48489` | RETIRED REPROCESS; engine/command rationale above |
| `pp_reprocess::it_can_be_the_first_command_after_an_upgrade` | `4619431402c24b3adf030c5e7f472c4c42b3c6522e6c9e82298a6d836fa805fd` | RETIRED REPROCESS; engine/command rationale above |

### tests/predb_import.rs

Original full-source SHA-256: `3619f595265c145de32a4056a86bd3475c2a6ccf5f41d1ff3481873e35271b41`.

**IMPORT:** Retired --predb-import command/SQLite bulk writer, late cursor behavior, batch/WAL controls, decompression/line-count/byte caps, file errors and help are importer-engine contracts. Current pure entry decoding remains independently tested; feed polling is not claimed equivalent to the removed file-import engine.

Current pure decoder: `predb::file_import::tests::line_decoder_keeps_title_category_timestamp_and_rejects_invalid_bytes`; the exact integration selector `predb_import::the_import_path_has_no_http` remains byte-identical. No stream-import-cap equivalence is claimed.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `predb_import::a_dry_run_counts_and_writes_nothing` | `c0fa52c8908e8a5de10f09db95613e62f14c0e276f05be82a837d4a246b056e2` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::apply_imports_plain_and_gzip` | `4a53bc6c2dbb0a3f690cf2820d0883123359cfaca94c5815f0d6e34c8fa20a1c` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_bulk_import_skips_the_late_queue_and_says_so` | `e8f9efe0a479c892b68fa2883222705b76fea327b32d6cec054e61b3871a2558` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_big_file_commits_in_batches` | `5d3df6fb2d6261287bdd707dead9aae70ca17af1b66ba6fee5bfdd6e8d976e1d` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::it_is_refused_with_an_indexer_alive` | `5eaed352c64dfb2a11bde189314fec5cb5d9d6d22882675e409965f3e6e6e8b2` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_missing_file_is_exit_1` | `ac43eb52b82fbe14db67dba71cebee24ddac4258a278c584ec319048030e000e` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_line_with_no_newline_is_skipped_and_counted` | `5f45ec536ee36195d202c233d21d8aea411fa146fb31901a2ec311e6c78865a8` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::exactly_the_line_cap_is_kept` | `4363e1c07b2e12e21de18f48b2defe96d8109e0a781883fb59ed9f4f72ef718f` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_gzip_bomb_stops_at_the_byte_cap` | `01297233d95139ddb28d98875e40968d1b2dd66121f3c140379da7d24ad0f86d` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::the_byte_cap_inside_a_line_stores_no_part_of_it` | `6ba731e69a167fa7005b72c2427dd3884e71424cf41223cd900d0894cf5566ea` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::too_many_lines_stops` | `7e597b3ccbc37b684cf79605b50a3072cff3b19b5690f20014c75987b2ce9559` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_big_import_keeps_its_wal_small` | `2f2ff3073d2a9c79939f16635d668a7db7a03c3913a6952350566c48181f7bb1` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::a_bad_utf8_line_is_invalid` | `23ff8ab7249047c4792366cfbdb03378329aecc01f55432202f5a5c96fe35eda` | RETIRED IMPORT; engine/command rationale above |
| `predb_import::the_import_path_has_no_http` | `e8966b4baef82a69579bf74ebbe694010d9e059d9b4db59e50a29f922b6dc5e2` | RETAINED PURE, same exact selector/body |
| `predb_import::help_lists_it` | `affda00503a4926886833de25b5aed30a5c2319d5cc7f03b1cf85fd43dc1f5b2` | RETIRED IMPORT; engine/command rationale above |

### tests/web_frozen.rs

Original full-source SHA-256: `a0dd78b4b0595013e66afa794c2279cde4f424a7f3967df143a7ec84f45494ac`.

**FROZEN:** Deep frozen-table search, FROZEN badges, frozen NZB/download selection, thaw authentication/redirects and thaw into live SQLite shards are the retired frozen feature. Native live Web/API authentication/search/NZB remain supported and unchanged; no frozen oracle is relabelled a native live-web pass.

Native authenticated asset routes remain in `src/api/native_asset_http_tests.rs` and `src/api/native_image_asset_http_tests.rs`; normal live search/NZB/frontdoors remain assigned to their owners. These do not replace frozen deep-search/thaw behavior.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `web_frozen::a_frozen_release_is_found_downloaded_and_thawed_from_the_web_ui` | `987f891a78c9ea488c5db065c409df5d91795025ab3eb3ab0a8d9893dfd8b1f7` | RETIRED FROZEN; engine/command rationale above |

## Root validation selectors and limits

After the remaining mixed fixtures are ported, ROOT owns meaningful exact-head compilation and actual native runtime validation. This source checkpoint adds only the following pure selectors; they remain UNRUN:

```text
<new-naming_reflag-artifact> --exact current_classifier_vectors_survive_reflag_retirement
<new-naming_unname-artifact> --exact current_classifier_vectors_survive_unname_retirement
<new-predb_import-artifact> --exact the_import_path_has_no_http
```

The six preserved mixed cases are OPEN port requirements, not admitted old SQLite tests, ignored successes, or claimed native passes. Root must keep actual SQL test admission mandatory and explicit when implementing those ports. Current native test source references above do not waive original oracles or qualify unrun tests.

Ledger totals at the pinned baseline: **93 original selectors = 86 retired strict command/engine selectors + 6 retained mixed selectors + 1 retained pure selector**. Two new pure vector selectors are additional, not counted as original CLI coverage.

## Follow-up: strict shard schema cases and retained native cascade

This separate finite family is pinned to `28da8d4219b2282240a6cb22e771888b6c230c54`. The original fourteen-target ledger above is preserved. Only `tests/pp_schema.rs`, the existing private `src/pg/stage_schema_tests.rs`, and this ledger change. No SQLite dependency/adapter, production API, wipe/menu, shared stage helper, Cargo discovery policy, or unrelated integration target is added or altered.

### tests/pp_schema.rs

Original full-source SHA-256: `1caea63007997e90012219e86afa5190650620b89ff9186a76ac24153b662a96`. Four strict engine cases retire; the all-populated-table deletion oracle moves before this integration target is removed.

| Original selector | Original function SHA-256 | Disposition |
| --- | --- | --- |
| `pp_schema::an_old_shard_gets_the_new_tables_without_a_rewrite` | `b6bf595fac5e905b9ccfea6f11d16413c7a95c7ef513fc2c8431e818b50a7cb4` | RETIRED SQLite shard-open upgrade, sqlite_master/GC trigger and PRAGMA page growth policy. Native added-column types/null/defaults and migration idempotence remain in pg::stage_schema_tests and pg::migrate tests. |
| `pp_schema::deleting_a_release_clears_it_from_every_registered_table` | `16740b72a3f7ad859014b1488cf187d6d383df33c2af68040ff5add7223b413f` | PORTED to pg::stage_schema_tests::deleting_a_release_clears_it_from_every_registered_table, preserving the populated all-table oracle and adding unrelated survivor/outliving-cache checks. |
| `pp_schema::the_menu_purge_clears_stage_rows` | `37d1b0fbdb9bedca6142479f57bc2cc2cb05cac1caace7ab5c894830cc644056` | RETIRED purge-incomplete menu/SQLite entry point. Generic release deletion cleanup is retained below; no native purge command is created. |
| `pp_schema::no_index_is_built_on_an_existing_table` | `c394a50ac2ed27895d9189ec7664fb016a8a3e4896ed29177af0346e29897f71` | RETIRED old SQLite registry/index-placement policy; current PostgreSQL index migrations are supported. |
| `pp_schema::a_wipe_leaves_new_empty_tables` | `b8951f496674ba88443d5a6a8eaef1e95a23ec7b2dadbc943f58f5aa86fd677f` | RETIRED db::wipe/eight-shard recreation. Existing native fresh-empty/orphan checks remain; no reset operation is introduced. |

The native case is an ordinary unignored test with the existing `Schema::new()` optional admission: no URL returns before setup, while malformed nonempty URLs still fail. Root must supply its restricted disposable QA URL and verify actual execution to qualify PostgreSQL coverage; absent-URL compatibility is not a native pass. There is no internal selected child to silently skip. All native controls are UNRUN at this source checkpoint.

A real Runtime/Owner and accepted release writer create victim, survivor and separate real link leads. Actual Queue claims, StepReads input, typed category writes and settlement create state/category/receipts while retaining a genuinely scheduled queue row. The writer itself creates file_timing. Closed per-table SQL creates only legal ordinary metadata rows for the other29 tables; it never derives generic fillers or writes catalog nonce/epochs, claims, input digests or operation receipts. The retained release and victim each have a nonzero row in EVERY33 former registered release-owned relationship before deletion, then every victim row must be gone and each survivor row must be byte-equivalent JSON. The original special keys remain `beside_files.row_id` and `upload_routes.origin_id`.

The complete33-table inventory is literal in the case, with uniqueness and prepopulation assertions. Real files/segments are checked too. Deletion runs only on the original Owner through the unchanged native transaction gate. Read-only snapshots of every ext_* cache relation and actual identity/epoch/operation records must match across deletion, with populated natural PreDB/media/OpenSubtitles/probe sentinels. Global/outliving pp_walks, sidecars, junk_releases and junk_collections are populated and preserved; the real pending word-index deletion/tombstone entry remains valid bookkeeping. Sidecars explicitly outlive releases and are not misclassified by their release_id column. No trigger-only/FK-only/two-table substitute replaces the full populated oracle.


## Follow-up: retained API completeness contract

### tests/incomplete_api.rs

Original full-source SHA-256: `5e2bf4b43e69c4d8a013a0e3f346e1b2c9beb3026925923271ae2ab8cca863a6`.

`incomplete_api::incomplete_releases_are_left_out_of_the_api_unless_asked_for` is PORTED to `api::native_incomplete_tests::incomplete_releases_are_left_out_of_the_api_unless_asked_for`. No Issue #232 behavior is retired. Its exact four post shapes, nine original articles, parser/grouping path, four single-release assertions and all eight public API requests remain. The native writer commits the posts in the original order; the real word keeper and readonly queries establish search readiness before the public `api::handle` assertions.

The test retains default omission of incomplete releases, scanning past newer incomplete rows for limit=1, Film/Gappy name filtering, the exact total=1 response, both deep requests, inclusion of all four posts when requested, exactly three incomplete attributes, and each item's completeness flag. The three incomplete shapes remain missing files, a single partial file, and a missing part despite all files being present. The accepted deep parameter is covered as a native request; no retired frozen-table behavior is claimed.

The ordinary default parent returns before setup without a configured PostgreSQL URL or on unsupported platforms. A malformed nonempty URL remains a strict failure. Its ignored internal child requires the exact case marker and parent-owned migrated schema; it has no optional success path. Genuine Runtime/Owner admission and typed writer commits are used without direct release inserts, synthetic authority or a public writer seam. The existing isolated-child helper checks successful exit and its completion marker, then cleans up its own namespace. Keeper/NXV/Owner drain precedes successful completion; no providers or external APIs are contacted. This direct-writer fixture has no input-header NXV blocks and does not claim header-durability coverage.

Explicit live validation selects only the parent with a nonempty disposable PostgreSQL URL:

```text
cargo test --frozen --lib -j1 api::native_incomplete_tests::incomplete_releases_are_left_out_of_the_api_unless_asked_for -- --exact --test-threads=1
```

The same parent without a URL verifies optional default admission only; that result is not a PostgreSQL pass. No other integration target, shared native helper, production dispatch or test-discovery policy changes.


## Follow-up: obsolete SQLite enqueue timing comparator

### tests/pp_enqueue_cost.rs

Original full-source SHA-256: `6a0ed0c9a9efe36903e12ea91a5e078244c80673dfab951aab5050fb38634405`.

`pp_enqueue_cost::enqueue_cost` is a RETIRED informational benchmark for the removed SQLite save/database engine and `store::SKIP_ENQUEUE_FOR_BENCH` queue-bypass toggle. The ignored case made five alternating pairs of 100,000 one-article saves in batches of 10,000, then printed ratios and a median. It contained no assertions. Its old database constructor, save entry point and bypass toggle no longer exist; compiling an ignored target still requires those removed APIs.

No correctness assertion, native benchmark, native queue bypass, throughput result or performance parity is claimed by this retirement. Queue-on-save, due-time, replay and late-part behavior remain independently covered by the native release-writer tests. The old postprocess plan's measurement and instructions are explicitly historical, with this retirement linked. Only this obsolete benchmark target and its documentation change; production sources, shared fixtures and Cargo discovery stay unchanged. Other legacy integration blockers remain open.
