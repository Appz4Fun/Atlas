//! Actual payload HEAD provenance controls; no fixture can mint a HeadReply.
use super::*;
use crate::archive::nzb::{NzbFile, Parsed, Segment};
use crate::spotnet::{
    native_peer_tests::{Mode, Peer, fixture},
    native_plan::payload_head_matches,
};

const SPOT_ID: &str = "vKCGetIwSd0z2PcagDlUt@spot.net";

fn plan_input(mid: &str) -> (full::FullSpot, Parsed) {
    let spot = full::FullSpot { title: "Own.Spot.Payload".into(), poster: "Spotter".into(), ..Default::default() };
    let parsed = Parsed {
        complete: true,
        files: vec![NzbFile {
            name: "actual.mkv".into(),
            subject: "\"actual.mkv\" yEnc (1/1)".into(),
            groups: vec!["example.spot".into()],
            articles: 1,
            segments: vec![Segment { number: 1, bytes: 123, message_id: mid.into() }],
            ..Default::default()
        }],
        ..Default::default()
    };
    (spot, parsed)
}

#[test]
fn payload_head_requires_the_exact_mid_and_selected_actual_group() {
    for head in [
        "Message-ID: <payload@fixture.test>\r\nNewsgroups: example.spot\r\n",
        "Message-ID: <payload@fixture.test>\nXref: server example.spot:42\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: other.group,example.spot\nXref: server other.group:7 example.spot:42\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: EXAMPLE.SPOT\n",
    ] {
        assert!(payload_head_matches("example.spot", "payload@fixture.test", head.as_bytes()));
    }
    assert!(payload_head_matches(
        "example.spot",
        "payload@fixture.test",
        b"Message-ID: <payload@fixture.test>\nNewsgroups: example.spot\nFrom: legacy-\xff\n"
    ));
    for head in [
        "Message-ID: <different@fixture.test>\nNewsgroups: example.spot\n",
        "Newsgroups: example.spot\n",
        "Message-ID: <payload@fixture.test>\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: other.group\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: example.spot\nXref: server other.group:42\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: other.group\nXref: server example.spot:42\n",
        "Message-ID: <payload@fixture.test>\nXref: server example.spot:not-a-number\n",
        "Message-ID: <payload@fixture.test>\nXref: server example.spot:0\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: example.spot\nMessage-ID: <different@fixture.test>\n",
        "Message-ID: <payload@fixture.test>\nNewsgroups: example.spot\nNewsgroups: other.group\n",
        "Message-ID: <payload@fixture.test>\nXref: server example.spot:42\nXref: server other.group:7\n",
        "Message-ID: <payload@fixture.test>\n\nNewsgroups: example.spot\n",
    ] {
        assert!(!payload_head_matches("example.spot", "payload@fixture.test", head.as_bytes()));
    }
    assert!(!payload_head_matches(
        "example.spot",
        "payload@fixture.test",
        b"Message-ID: <payload@fixture.test>\nNewsgroups: example.spot\xff\n"
    ));
    assert!(!payload_head_matches(
        "alt.binaries.preteen",
        "payload@fixture.test",
        b"Message-ID: <payload@fixture.test>\nNewsgroups: alt.binaries.preteen\n"
    ));
}

#[test]
fn payload_plan_rejects_invalid_mid_and_unshared_declared_group_before_commands() {
    for mid in [
        "",
        "missing-at",
        "@host",
        "local@",
        "a b@host",
        "<a@host>",
        "a@host\r\nBODY <other@host>",
        "a\0@host",
        "a@b@c",
    ] {
        let (spot, parsed) = plan_input(mid);
        assert!(Plan::new(&spot, &parsed).unwrap().is_none(), "invalid payload MID");
    }
    let (spot, mut parsed) = plan_input("actual-part@fixture.test");
    let mut second = parsed.files[0].clone();
    second.groups = vec!["another.allowed.group".into()];
    parsed.files.push(second);
    assert!(Plan::new(&spot, &parsed).unwrap().is_none(), "one stored release group must be declared by every file");
}

fn service(runtime: &crate::pg::indexer_runtime::Runtime, peer: &Peer, handle: tokio::runtime::Handle) -> Service {
    Service::new(
        &runtime.owner,
        runtime.db.clone(),
        crate::config::SpotsConfig {
            blacklist_url: String::new(),
            concurrency: 2,
            per_pass: 4,
            comments_per_pass: 4,
            ..Default::default()
        },
        Arc::new(Pool::new(&[peer.server.clone()])),
        handle,
    )
    .unwrap()
}

#[test]
#[ignore = "explicit isolated PG and owned loopback payload HEAD provenance"]
fn signed_spot_declared_group_cannot_publish_unverified_payloads() {
    for mode in [
        Mode::PayloadBlocked,
        Mode::PayloadNoGroups,
        Mode::PayloadWrongMid,
        Mode::PayloadGroupMismatch,
        Mode::PayloadGroupConflict,
        Mode::PayloadTooBig,
        Mode::PayloadMissing,
        Mode::PayloadSecondBlocked,
    ] {
        let (schema, runtime, _dir) = fixture();
        let peer = Peer::new(mode);
        let net = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let service = service(&runtime, &peer, net.handle().clone());
        let pass = service.spots_tick(1793000000).unwrap();
        assert_eq!((pass.saved, pass.skipped, pass.retry), (0, 1, 0));
        assert_eq!(runtime.owner.failure(), None, "a refused provider payload is not a storage failure");
        assert!(peer.count("HEAD") >= 2, "the signed Spot and its actual payload were both requested");
        if matches!(mode, Mode::PayloadSecondBlocked) {
            assert_eq!(peer.count("HEAD"), 3, "every referenced MID needs its own HEAD");
        }
        let mut observer = schema.pool.get().unwrap();
        for table in ["releases", "files", "segments", "spots"] {
            assert_eq!(
                observer.query_one(&format!("select count(*) from {table}"), &[]).unwrap().get::<_, i64>(0),
                0,
                "no partial {table} publication"
            );
        }
        assert_eq!(observer.query_one("select state from spots_seen", &[]).unwrap().get::<_, i64>(0), store::SKIPPED);
        runtime.owner.close();
    }
}

#[test]
#[ignore = "explicit isolated PG and owned loopback invalid payload MID"]
fn invalid_payload_mid_never_becomes_a_head_command() {
    let (schema, runtime, _dir) = fixture();
    let peer = Peer::new(Mode::PayloadInvalidMid);
    let net = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service = service(&runtime, &peer, net.handle().clone());
    let pass = service.spots_tick(1793000000).unwrap();
    assert_eq!((pass.saved, pass.skipped), (0, 1));
    assert_eq!(peer.count("HEAD"), 1, "only the genuine signed Spot HEAD was requested");
    assert_eq!(schema.pool.get().unwrap().query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0), 0);
    runtime.owner.close();
}

#[test]
#[ignore = "explicit isolated PG and owned loopback transient payload HEAD"]
fn transient_payload_head_remains_a_retry_without_poisoning_the_owner() {
    let (schema, runtime, _dir) = fixture();
    let peer = Peer::new(Mode::PayloadTransient);
    let net = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service = service(&runtime, &peer, net.handle().clone());
    let pass = service.spots_tick(1793000000).unwrap();
    assert_eq!((pass.saved, pass.skipped, pass.retry), (0, 0, 1));
    let row = schema.pool.get().unwrap().query_one("select state,tries from spots_seen", &[]).unwrap();
    assert_eq!((row.get::<_, i64>(0), row.get::<_, i64>(1)), (store::TODO, 1));
    assert_eq!(runtime.owner.failure(), None);
    runtime.owner.close();
}

#[test]
#[ignore = "explicit isolated PG and original shared pass admission"]
fn insufficient_head_admission_starts_no_provider_request() {
    let (_schema, runtime, _dir) = fixture();
    let peer = Peer::new(Mode::Good);
    let net = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service = service(&runtime, &peer, net.handle().clone());
    let (spot, parsed) = plan_input("actual-part@fixture.test");
    let plan = Plan::new(&spot, &parsed).unwrap().unwrap();
    let shared = ingest::Shared::new(PAYLOAD_HEAD_BYTES + ingest::CAP_OVERSHOOT - 1);
    assert_eq!(service.verify_payloads(&plan, &shared).unwrap(), None);
    assert_eq!(peer.count("HEAD"), 0);
    assert_eq!(runtime.owner.failure(), None);
    runtime.owner.close();
}

#[test]
#[ignore = "explicit isolated PG and actual repeated reference payload HEAD"]
fn known_nzb_reference_rechecks_payload_headers_before_reusing_a_release() {
    for mode in [Mode::Good, Mode::PayloadRevoked] {
        let (schema, runtime, _dir) = fixture();
        let peer = Peer::new(mode);
        let net = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let service = service(&runtime, &peer, net.handle().clone());
        assert_eq!(service.spots_tick(1793000000).unwrap().saved, 1);
        let repeated =
            service.fetch_and_settle(SPOT_ID, &Arc::new(ingest::Shared::new(ingest::MAX_PASS_POSTED))).unwrap();
        assert_eq!(peer.count("HEAD"), 4, "both attempts read the Spot and actual payload HEADs");
        assert_eq!(peer.count("BODY"), 2, "reuse still parses the referenced NZB before checking all its payloads");
        assert_eq!(
            repeated,
            Some(Some(if matches!(mode, Mode::PayloadRevoked) { store::SKIPPED } else { store::SAVED }))
        );
        assert_eq!(
            schema.pool.get().unwrap().query_one("select count(*) from releases", &[]).unwrap().get::<_, i64>(0),
            1
        );
        assert_eq!(runtime.owner.failure(), None);
        runtime.owner.close();
    }
}

#[test]
#[ignore = "explicit isolated PG and actual changed NZB reference payload"]
fn changed_reference_publishes_the_verified_plan_instead_of_copying_prior_payload() {
    let (schema, runtime, _dir) = fixture();
    let peer = Peer::new(Mode::PayloadChangedReference);
    let net = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let service = service(&runtime, &peer, net.handle().clone());
    assert_eq!(service.spots_tick(1793000000).unwrap().saved, 1);
    let mut observer = schema.pool.get().unwrap();
    let prior =
        observer.query_one("select release_id,nzb_key from spots_seen where message_id=$1", &[&SPOT_ID]).unwrap();
    let prior_id = prior.get::<_, i64>(0);
    let prior_key = prior.get::<_, String>(1);

    assert_eq!(
        service.fetch_and_settle(SPOT_ID, &Arc::new(ingest::Shared::new(ingest::MAX_PASS_POSTED))).unwrap(),
        Some(Some(store::SAVED))
    );
    let current =
        observer.query_one("select release_id,nzb_key from spots_seen where message_id=$1", &[&SPOT_ID]).unwrap();
    let current_id = current.get::<_, i64>(0);
    assert_ne!(current_id, prior_id, "new HEAD-verified MID/group cannot copy the old reference's release");
    assert_ne!(current.get::<_, String>(1), prior_key, "bookkeeping retains the newly parsed payload key");
    let row = observer.query_one("select r.group_name,s.message_id,s.part,s.bytes from releases r join segments s on s.release_id=r.id where r.id=$1", &[&current_id]).unwrap();
    assert_eq!(
        (row.get::<_, String>(0), row.get::<_, Vec<u8>>(1), row.get::<_, i64>(2), row.get::<_, i64>(3)),
        ("another.allowed.group".into(), b"<changed-part@fixture.test>".to_vec(), 1, 123)
    );
    let old = observer
        .query_one(
            "select r.group_name,s.message_id from releases r join segments s on s.release_id=r.id where r.id=$1",
            &[&prior_id],
        )
        .unwrap();
    assert_eq!(
        (old.get::<_, String>(0), old.get::<_, Vec<u8>>(1)),
        ("example.spot".into(), b"<actual-part@fixture.test>".to_vec())
    );
    assert_eq!(peer.count("HEAD"), 4, "both actual payloads were HEAD checked");
    assert_eq!(peer.count("BODY"), 2);
    assert_eq!(runtime.owner.failure(), None);
    runtime.owner.close();
}
