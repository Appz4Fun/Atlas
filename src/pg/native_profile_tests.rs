//! Pure admission checks; the normal-binary fixture separately exercises startup.
use super::Profile;
use crate::config::{Config, HeaderStoreConfig};

fn headers() -> HeaderStoreConfig {
    HeaderStoreConfig { enabled: true, ..Default::default() }
}

fn enabled() -> Config {
    crate::config::from_value_for_test(&serde_json::json!({
        "inferred_parts":true,
        "postprocess":{"enabled":true,"workers":2,"body_connections":2,"body_max_bytes":134217728},
        "predb":{"enabled":true},"spots":{"enabled":true,"comments":true},
        "archive":{"enabled":true},
        "media":{"parse":true,"nfo":true,"details_per_minute":120,
            "tvmaze":{"enabled":true},"allocine":{"enabled":true},
            "tmdb":{"api_key":"fixture"},"tvdb":{"api_key":"fixture"},
            "trakt":{"client_id":"fixture"},"omdb":{"api_key":"fixture"},
            "opensubtitles":{"api_key":"fixture"}}
    }))
}

#[cfg(unix)]
#[test]
fn enabled_native_services_are_admitted_without_mutating_the_selection() {
    let config = enabled();
    let original = config.clone();
    assert!(config.postprocess.enabled && config.predb.enabled && config.spots.enabled);
    assert!(config.spots.comments && config.inferred_parts && config.archive.enabled);
    assert!(config.media.any_provider() && config.media.details_per_minute > 0);
    assert!(Profile::check(&config, &headers()).is_ok());
    assert_eq!(config.postprocess, original.postprocess);
    assert_eq!(config.predb, original.predb);
    assert_eq!(config.media, original.media);
    assert_eq!(config.spots, original.spots);
    assert_eq!(config.inferred_parts, original.inferred_parts);
}

#[test]
fn inferred_repair_platform_admission_is_not_a_silent_disable() {
    let config = enabled();
    #[cfg(unix)]
    assert!(Profile::check(&config, &headers()).is_ok());
    #[cfg(not(unix))]
    assert!(Profile::check(&config, &headers()).unwrap_err().to_string().contains("Unix"));
}

#[test]
fn retired_backends_and_missing_canonical_headers_still_refuse() {
    let mut config = enabled();
    config.inferred_parts = false;
    assert!(Profile::check(&config, &HeaderStoreConfig::default()).is_err());
    let edits: [(fn(&mut Config), &str); 4] = [
        (|c| c.auto_run_compact = true, "compaction"),
        (|c| c.shared_catalog.enabled = true, "mirror"),
        (|c| c.overview_archive.enabled = true, "raw overview archive"),
        (|c| c.sealed_log.addr = "private-fixture.invalid".into(), "offload"),
    ];
    for (edit, reason) in edits {
        let mut invalid = config.clone();
        edit(&mut invalid);
        let message = Profile::check(&invalid, &headers()).unwrap_err().to_string();
        assert!(message.contains(reason), "expected a specific retired-feature refusal");
        assert!(!message.contains("private-fixture"));
    }
}

#[test]
fn constructor_resource_overflow_refuses_before_allocation() {
    let mut config = enabled();
    config.inferred_parts = false;
    config.postprocess.body_connections = tokio::sync::Semaphore::MAX_PERMITS + 1;
    assert!(Profile::check(&config, &headers()).unwrap_err().to_string().contains("semaphore"));
    config.postprocess.body_connections = 1;
    config.postprocess.workers = usize::MAX;
    assert!(Profile::check(&config, &headers()).unwrap_err().to_string().contains("worker count"));
    config.postprocess.workers = 1;
    config.postprocess.body_max_bytes = -1;
    assert!(Profile::check(&config, &headers()).unwrap_err().to_string().contains("BODY"));
    config.postprocess.body_max_bytes = i64::MAX;
    config.postprocess.body_connections = 3;
    assert!(Profile::check(&config, &headers()).is_err());
}

#[test]
fn configured_zero_and_unlimited_semantics_remain_available() {
    let mut config = enabled();
    config.inferred_parts = false;
    config.postprocess.workers = 0; // The existing constructor takes max(1).
    config.postprocess.body_connections = 0;
    config.postprocess.backlog_per_second = 0;
    config.postprocess.body_fetches_per_minute = 0;
    config.postprocess.bodies_per_release = 0;
    config.postprocess.body_max_size_bytes = 0;
    config.media.details_per_minute = 0;
    config.media.tvmaze.requests_per_minute = -1;
    config.media.tmdb.requests_per_minute = -1;
    config.freeze.enabled = true; // Native retention does not route to SQLite.
    assert!(Profile::check(&config, &headers()).is_ok());
    config.postprocess.enabled = false;
    config.postprocess.body_connections = usize::MAX;
    config.postprocess.body_max_bytes = -1;
    assert!(Profile::check(&config, &headers()).is_ok(), "an unused BODY constructor imposes no new limit");
}

#[test]
fn direct_unclamped_spot_settings_refuse_but_file_parser_clamps_them() {
    let mut config = enabled();
    config.inferred_parts = false;
    config.spots.concurrency = 65;
    assert!(Profile::check(&config, &headers()).unwrap_err().to_string().contains("Spotnet"));
    let parsed = crate::config::from_value_for_test(&serde_json::json!({"inferred_parts":false,
        "spots":{"enabled":true,"concurrency":65,"per_pass":10001,"poll_minutes":0}}));
    assert_eq!(parsed.spots.concurrency, 64);
    assert_eq!(parsed.spots.per_pass, 10000);
    assert_eq!(parsed.spots.poll_minutes, 1);
    assert!(Profile::check(&parsed, &headers()).is_ok());
}
