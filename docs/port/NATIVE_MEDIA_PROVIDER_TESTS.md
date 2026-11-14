# Native provider fixture contracts

The provider cases require a genuine original native owner even for loopback HTTP: `media::http::Client` checks its captured `service_cache::Cache` before requests. Disabling HTTP response caching does not remove that ownership requirement. The retired external `media_providers` target is replaced by the following 36 registered LIB parents; no provider behavior is retired.

Each parent is explicitly ignored until isolated PostgreSQL is supplied through `ATLAS_PG_TEST_URL`. Missing admission fails rather than skips. The parent owns a fresh migrated schema and private home, creates the selected TOML with mode 0600 and matching storage/header settings, and runs only its same named child. The child uses `Runtime::open`, retains its original Owner, constructs the real `service_cache::Cache`, and executes the complete unchanged case inside `media::cache::native`. Production cache/HTTP checks are unchanged.

The closed child has a 120-second deadline and separate 64-KiB retained stdout/stderr limits; both pipes are drained to EOF and joined before the parent drops its schema. Owner close runs on unwind; completion is reported only after providers, loopback fixtures and the owner finish. No private destination or raw child output is copied into parent failure text. These fixtures assume a Unix platform, one serial parent at a time, explicit disposable PostgreSQL, and loopback HTTP only. Original rate, retry, request-count, payload and timing assertions are unchanged.

Run the native parent family explicitly with `cargo test --lib media::native_provider_tests:: -- --ignored --test-threads=1` under the project's normal resource admission. This command is documentation, not a claim of execution or qualification.

| Original external selector | Native LIB parent |
| --- | --- |
| `media_providers::a_hit_takes_one_call` | `media::native_provider_tests::a_hit_takes_one_call` |
| `media_providers::an_aka_matches` | `media::native_provider_tests::an_aka_matches` |
| `media_providers::a_country_mismatch_falls_to_search` | `media::native_provider_tests::a_country_mismatch_falls_to_search` |
| `media_providers::a_year_mismatch_falls_to_search` | `media::native_provider_tests::a_year_mismatch_falls_to_search` |
| `media_providers::a_404_falls_to_search_then_misses_in_two_calls` | `media::native_provider_tests::a_404_falls_to_search_then_misses_in_two_calls` |
| `media_providers::a_wrong_show_is_rejected` | `media::native_provider_tests::a_wrong_show_is_rejected` |
| `media_providers::a_429_pauses_for_retry_after` | `media::native_provider_tests::a_429_pauses_for_retry_after` |
| `media_providers::a_500_pauses_and_doubles` | `media::native_provider_tests::a_500_pauses_and_doubles` |
| `media_providers::a_connection_refused_pauses` | `media::native_provider_tests::a_connection_refused_pauses` |
| `media_providers::bad_json_is_bad_not_a_miss` | `media::native_provider_tests::bad_json_is_bad_not_a_miss` |
| `media_providers::the_bucket_paces_requests` | `media::native_provider_tests::the_bucket_paces_requests` |
| `media_providers::a_two_request_lookup_finishes_at_the_lowest_rate` | `media::native_provider_tests::a_two_request_lookup_finishes_at_the_lowest_rate` |
| `media_providers::an_oversized_name_is_no_candidate` | `media::native_provider_tests::an_oversized_name_is_no_candidate` |
| `media_providers::a_redirect_isnt_followed` | `media::native_provider_tests::a_redirect_isnt_followed` |
| `media_providers::no_token_within_two_seconds_gives_paused` | `media::native_provider_tests::no_token_within_two_seconds_gives_paused` |
| `media_providers::a_huge_body_is_refused` | `media::native_provider_tests::a_huge_body_is_refused` |
| `media_providers::tmdb_tv_takes_two_calls_and_finds_external_ids` | `media::native_provider_tests::tmdb_tv_takes_two_calls_and_finds_external_ids` |
| `media_providers::tmdb_movie_passes_the_year_and_finds_imdb` | `media::native_provider_tests::tmdb_movie_passes_the_year_and_finds_imdb` |
| `media_providers::tmdb_miss_takes_one_call` | `media::native_provider_tests::tmdb_miss_takes_one_call` |
| `media_providers::tmdb_picks_the_best_of_many` | `media::native_provider_tests::tmdb_picks_the_best_of_many` |
| `media_providers::tmdb_null_ids_are_none` | `media::native_provider_tests::tmdb_null_ids_are_none` |
| `media_providers::tmdb_401_is_bad_key_and_records_nothing` | `media::native_provider_tests::tmdb_401_is_bad_key_and_records_nothing` |
| `media_providers::tmdb_key_never_reaches_the_log` | `media::native_provider_tests::tmdb_key_never_reaches_the_log` |
| `media_providers::trakt_sends_its_headers` | `media::native_provider_tests::trakt_sends_its_headers` |
| `media_providers::trakt_movie_finds_all_its_ids` | `media::native_provider_tests::trakt_movie_finds_all_its_ids` |
| `media_providers::trakt_miss_is_an_empty_list` | `media::native_provider_tests::trakt_miss_is_an_empty_list` |
| `media_providers::a_client_is_a_public_type` | `media::native_provider_tests::a_client_is_a_public_type` |
| `media_providers::tvdb_logs_in_once_and_finds_its_ids` | `media::native_provider_tests::tvdb_logs_in_once_and_finds_its_ids` |
| `media_providers::tvdb_matches_an_alias_a_country_and_a_year` | `media::native_provider_tests::tvdb_matches_an_alias_a_country_and_a_year` |
| `media_providers::tvdb_asks_only_for_tv` | `media::native_provider_tests::tvdb_asks_only_for_tv` |
| `media_providers::a_refused_tvdb_key_pauses_and_says_nothing_of_it` | `media::native_provider_tests::a_refused_tvdb_key_pauses_and_says_nothing_of_it` |
| `media_providers::a_token_it_stops_taking_logs_in_again` | `media::native_provider_tests::a_token_it_stops_taking_logs_in_again` |
| `media_providers::a_login_with_no_token_is_bad_not_a_miss` | `media::native_provider_tests::a_login_with_no_token_is_bad_not_a_miss` |
| `media_providers::an_empty_tvdb_search_is_a_miss` | `media::native_provider_tests::an_empty_tvdb_search_is_a_miss` |
| `media_providers::a_translated_name_matches` | `media::native_provider_tests::a_translated_name_matches` |
| `media_providers::a_pin_goes_with_the_key_when_there_is_one` | `media::native_provider_tests::a_pin_goes_with_the_key_when_there_is_one` |
