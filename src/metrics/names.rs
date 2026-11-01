//! Every metric atlas exports, in one place.

use super::Family;
use super::Kind::{Counter, Gauge, Histogram};

/// call lengths: 100µs to 30 minutes
const SECONDS: &[f64] = &[
    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
    120.0, 300.0, 600.0, 1800.0,
];

/// release sizes: 1MB to 1TB, doubling
pub const SIZES: &[f64] = &[
    1048576.0,
    2097152.0,
    4194304.0,
    8388608.0,
    16777216.0,
    33554432.0,
    67108864.0,
    134217728.0,
    268435456.0,
    536870912.0,
    1073741824.0,
    2147483648.0,
    4294967296.0,
    8589934592.0,
    17179869184.0,
    34359738368.0,
    68719476736.0,
    137438953472.0,
    274877906944.0,
    549755813888.0,
    1099511627776.0,
];

macro_rules! families {
    ($($id:ident $name:literal $kind:ident $bounds:expr, $help:literal;)*) => {
        $(pub static $id: Family = Family::new($name, $help, $kind, $bounds);)*
        pub static ALL: &[&Family] = &[$(&$id),*];
    };
}

families! {
    // the process
    BUILD_INFO "atlas_build_info" Gauge &[], "Always 1; the labels say what is running.";
    PROCESS_START "atlas_process_start_time_seconds" Gauge &[], "When this process started.";
    PROCESS_CPU "atlas_process_cpu_milliseconds_total" Counter &[], "CPU time this process has used.";
    PROCESS_RSS "atlas_process_resident_memory_bytes" Gauge &[], "Memory this process holds.";
    PROCESS_DISK "atlas_process_disk_bytes_total" Counter &[], "Disk bytes this process has read or written.";
    SCRAPE_SECONDS "atlas_scrape_render_seconds" Gauge &[], "How long the previous scrape took to put together.";

    // every timed operation
    FUNCTION_DURATION "atlas_function_duration_seconds" Histogram SECONDS, "How long each operation takes.";
    FUNCTION_IN_PROGRESS "atlas_function_in_progress" Gauge &[], "Calls of each operation running now.";
    FUNCTION_ERRORS "atlas_function_errors_total" Counter &[], "Calls of each operation that failed.";
    FUNCTION_BYTES "atlas_function_bytes_total" Counter &[], "Network bytes each operation moved.";

    // nntp
    NNTP_CONNECTIONS "atlas_nntp_connections" Gauge &[], "Connections per server: open, in_use, idle, limit, configured.";
    NNTP_WAITING "atlas_nntp_waiting_requests" Gauge &[], "Requests waiting for a free connection, per server.";
    NNTP_SERVER_STATE "atlas_nntp_server_state" Gauge &[], "1 for the state each server is in now.";
    NNTP_SERVER_INDEXING "atlas_nntp_server_indexing" Gauge &[], "1 when a server is used for indexing.";
    NNTP_CONNECTS "atlas_nntp_connects_total" Counter &[], "Connection attempts per server, by result.";
    NNTP_COMMANDS "atlas_nntp_commands_total" Counter &[], "NNTP commands per server, by command and result.";
    NNTP_COMMAND_SECONDS "atlas_nntp_command_duration_seconds" Histogram SECONDS, "How long NNTP commands take, per server.";
    NNTP_BYTES "atlas_nntp_bytes_total" Counter &[], "Bytes on the socket per server, by command and direction.";
    NNTP_FAILURES "atlas_nntp_failures_total" Counter &[], "Failures per server, by kind.";
    NNTP_LAST_FAILURE "atlas_nntp_last_failure_timestamp_seconds" Gauge &[], "When each kind of failure last hit each server.";
    NNTP_HEADERS "atlas_nntp_headers_total" Counter &[], "Headers read per server.";
    NNTP_TRAFFIC "atlas_nntp_traffic_bytes_total" Counter &[], "Bytes per server: wire (off the socket), text (after decompression).";

    // indexing
    ARTICLES "atlas_articles_total" Counter &[], "Articles indexed, per group.";
    ARTICLE_BYTES "atlas_article_bytes_total" Counter &[], "Post bytes indexed, per group.";
    RELEASES_SEEN "atlas_releases_seen_total" Counter &[], "Releases in the slices saved, per group: one release in many slices counts each time. New ones are in atlas_db_rows.";
    ARTICLES_SKIPPED "atlas_articles_skipped_total" Counter &[], "Articles the indexer skipped, per group.";
    GROUP_LAST_INDEXED "atlas_group_last_indexed_timestamp_seconds" Gauge &[], "When each group last indexed something.";
    INDEXER_ERRORS "atlas_indexer_errors_total" Counter &[], "Errors the indexing loop has counted.";
    INDEXER_RUNNING "atlas_indexer_up" Gauge &[], "1 while the indexing loop is running.";
    INDEXER_IDLE "atlas_indexer_idle" Gauge &[], "1 when every group is caught up.";
    INDEXER_WORKERS "atlas_indexer_workers" Gauge &[], "Workers indexing at once.";
    GROUPS_CONFIGURED "atlas_groups" Gauge &[], "Configured groups: indexed, blocked.";
    UNSAVED_HEADERS "atlas_unsaved_headers" Gauge &[], "Headers fetched and not saved yet, and the cap.";
    LOAD "atlas_load_nanoseconds_total" Counter &[], "Busy time of the indexing machinery, by what it was doing.";
    LOAD_COUNT "atlas_load_events_total" Counter &[], "Events of the indexing machinery, by kind.";
    WRITER_QUEUED "atlas_writer_queued_slices" Gauge &[], "Slices waiting for a database writer.";

    // post-processing
    STEP_RUNS "atlas_step_runs_total" Counter &[], "Runs of each processing step, by result.";
    STAGE_QUEUE "atlas_stage_queue" Gauge &[], "Releases waiting for the processing stage: due, net_wanted, failed.";
    STAGE_WALK "atlas_stage_walk" Gauge &[], "How far the backlog walks have come, in release ids: done, total.";
    STAGE_REJECTED "atlas_stage_rejected_total" Counter &[], "Releases the stage's rules rejected, by reason.";
    STAGE_EVENTS "atlas_stage_events_total" Counter &[], "Stage events: steps_done, matched, unmatched, retries, rows_walked, errors.";
    STAGE_BODIES "atlas_stage_bodies_total" Counter &[], "Article bodies the stage has fetched.";
    STAGE_BODY_BYTES "atlas_stage_body_bytes_total" Counter &[], "Bytes of the bodies the stage has fetched.";

    // names
    NAMES_APPLIED "atlas_names_applied_total" Counter &[], "Names written onto releases, by source.";
    DEOBFUSCATED "atlas_deobfuscated_total" Counter &[], "Obfuscated releases given a real name, by source.";

    // categories and sizes as releases settle
    CATEGORIZED "atlas_releases_categorized_total" Counter &[], "Releases filed under each category, reruns included.";
    RELEASE_SIZE "atlas_release_size_bytes" Histogram SIZES, "Size of releases as they are categorized, per root category.";

    // predb
    PREDB_LOOKUPS "atlas_predb_lookups_total" Counter &[], "PreDB lookups, by result.";
    PREDB_MATCHES "atlas_predb_matches_total" Counter &[], "Releases matched to a PreDB title, by how and by PreDB category.";
    PREDB_TITLES "atlas_predb_titles" Gauge &[], "Titles in the PreDB.";
    PREDB_FILE_BYTES "atlas_predb_file_bytes" Gauge &[], "Size of the PreDB file.";
    PREDB_LATE "atlas_predb_late_total" Counter &[], "Late title matching: titles, reruns, overflow, out_of_budget, errors.";
    PREDB_LATE_BEHIND "atlas_predb_late_behind" Gauge &[], "Titles the late matcher has yet to look at.";
    PREDB_FEED_POLLS "atlas_predb_feed_polls_total" Counter &[], "Polls of each PreDB feed, by result.";
    PREDB_FEED_FAILURES "atlas_predb_feed_failures" Gauge &[], "Failures in a row of each PreDB feed.";
    PREDB_FEED_ROWS "atlas_predb_feed_rows" Gauge &[], "Rows each PreDB feed has brought: inserted, updated, last.";
    PREDB_FEED_LAST_OK "atlas_predb_feed_last_ok_timestamp_seconds" Gauge &[], "When each PreDB feed last answered.";

    // media ids
    MEDIA_REQUESTS "atlas_media_requests_total" Counter &[], "Requests to each ID provider, by result.";
    MEDIA_TITLES "atlas_media_titles" Gauge &[], "Titles the ID cache holds.";

    // the api
    API_REQUESTS "atlas_api_requests_total" Counter &[], "Requests to the api, by endpoint and status.";
    API_BYTES "atlas_api_response_bytes_total" Counter &[], "Bytes the api answered with, by endpoint.";

    // the database
    DB_BYTES "atlas_db_bytes" Gauge &[], "Size on disk of each database file: main, shards, predb, media, by db or wal.";
    DB_TOTALS "atlas_db_rows" Gauge &[], "Running totals the shards keep: releases, articles.";
    BACKFILL_CHUNKS "atlas_backfill_chunks" Gauge &[], "Backfill day chunks: done, total, done_last_hour.";
    BACKFILL_GROUPS "atlas_backfill_groups" Gauge &[], "Groups with day chunks: all, splitting.";

    // the catalog scan, see `catalog`
    CATALOG_RELEASES "atlas_catalog_releases" Gauge &[], "Releases in the catalog, by state.";
    CATALOG_BYTES "atlas_catalog_bytes" Gauge &[], "Total size of the catalog's releases.";
    CATALOG_NAME_SOURCE "atlas_catalog_name_source_releases" Gauge &[], "Releases by where their name came from.";
    CATALOG_CATEGORY "atlas_catalog_category_releases" Gauge &[], "Releases per category.";
    CATALOG_CATEGORY_BYTES "atlas_catalog_category_bytes" Gauge &[], "Size of the releases per category.";
    CATALOG_GROUP "atlas_catalog_group_releases" Gauge &[], "Releases of the biggest groups.";
    CATALOG_GROUP_BYTES "atlas_catalog_group_bytes" Gauge &[], "Size of the biggest groups.";
    CATALOG_RELEASE_GROUP "atlas_catalog_release_group_releases" Gauge &[], "Video releases of the biggest release groups.";
    CATALOG_VIDEO "atlas_catalog_video_releases" Gauge &[], "Video releases, by whether a release group was read from the name.";
    CATALOG_SIZE "atlas_catalog_release_size_bytes" Gauge &[], "Catalog releases at or below each size, per root category (cumulative, le).";
    CATALOG_SIZE_SUM "atlas_catalog_release_size_sum_bytes" Gauge &[], "Size of the catalog's releases per root category.";
    CATALOG_SIZE_COUNT "atlas_catalog_release_size_count" Gauge &[], "Catalog releases per root category.";
    CATALOG_PREDB_KIND "atlas_catalog_predb_kind_releases" Gauge &[], "Releases matched to the PreDB, by how.";
    CATALOG_PREDB_CATEGORY "atlas_catalog_predb_category_releases" Gauge &[], "Releases matched to the PreDB, by PreDB category.";
    CATALOG_PREDB_FEED "atlas_catalog_predb_feed_releases" Gauge &[], "Releases matched to the PreDB, by the feed of the title.";
    CATALOG_PREDB_ROOT "atlas_catalog_predb_root_releases" Gauge &[], "Releases per root category, by whether a PreDB title matched.";
    CATALOG_SCAN_AT "atlas_catalog_scan_timestamp_seconds" Gauge &[], "When the catalog scan last finished.";
    CATALOG_SCAN_SECONDS "atlas_catalog_scan_duration_seconds" Gauge &[], "How long the last catalog scan took.";
    CATALOG_SCAN_ROWS "atlas_catalog_scan_rows" Gauge &[], "Rows the catalog scan looked at, and how far the running one has come.";
}
