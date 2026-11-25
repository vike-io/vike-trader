//! `run_study`'s negotiation, against the fake peer. The shape of `coverage_negotiation.rs`.
//!
//! ⚠ What this proves that `crates/vike-cli/tests/shipped_cli/study_report_refusal_cli.rs` cannot: that half
//! drives the shipped binary against a REAL data server, which advertises no `study` and never
//! will, so it can only ever exercise the REFUSAL. The direction that matters is the other one — a
//! peer that DOES advertise the capability must be SENT the request.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{
    FEATURE_RUN_FILES, FEATURE_STUDY, FEATURE_STUDY_FILES, StudyRunRequestWire,
};
use vike_backtesthub_client::study::StudyTierWire;
use vike_backtesthub_client::test_support::spawn_fake_peer;

fn a_recipe() -> StudyRunRequestWire {
    StudyRunRequestWire {
        study: "cohort".to_string(),
        recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
        from: "2026-04-07T05".to_string(),
        to: "2026-08-05T05".to_string(),
        ..StudyRunRequestWire::default()
    }
}

/// A daemon that does not mount a study runner is refused CLIENT-side, with nothing sent.
#[test]
fn a_daemon_without_the_capability_is_refused_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(
        ["backtest", "list_strategies"].iter().map(|s| s.to_string()).collect(),
        |_| None,
    );
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let err = client.run_study(&a_recipe()).expect_err("no capability, no run");
    assert!(err.contains(FEATURE_STUDY), "the refusal names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    assert!(rx.recv().expect("count").is_empty(), "nothing after the handshake");
    handle.join().expect("join");
}

/// ⚠ An older server, which advertises `study` but not `study_files` / `run_files`, is refused CLIENT-side
/// for every verb that needs them, by name, with nothing sent: a study of a TIER or a recipe FILE (it would
/// drop both and run the Rust study of that name), the study list, the runs list and a run's file.
#[test]
fn a_server_before_server_side_studies_is_refused_without_sending() {
    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_STUDY.to_string()], |_| None);
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let with_tier = StudyRunRequestWire { tier: Some(StudyTierWire::Rhai), ..a_recipe() };
    let refusals = [
        client.run_study(&with_tier).expect_err("a tier needs study_files"),
        client.list_studies().map(|_| ()).expect_err("the list needs study_files"),
        client.list_runs(0, 0).map(|_| ()).expect_err("the runs need run_files"),
        client.read_run_artifact("1-1", "a.csv", 0, 0).map(|_| ()).expect_err("so do their files"),
    ];
    for (refusal, feature) in refusals.iter().zip([
        FEATURE_STUDY_FILES,
        FEATURE_STUDY_FILES,
        FEATURE_RUN_FILES,
        FEATURE_RUN_FILES,
    ]) {
        assert!(refusal.contains(feature), "the refusal names {feature}: {refusal}");
        assert!(refusal.contains("Nothing was sent"), "{refusal}");
    }
    drop(client);
    assert!(rx.recv().expect("count").is_empty(), "nothing after the handshake");
    handle.join().expect("join");
}

/// …and a daemon that DOES advertise it is SENT the request. The fake answers a `Response::Error`,
/// so the call fails on the ANSWER — which is the point: reaching an answer proves the write.
#[test]
fn a_daemon_that_advertises_the_capability_receives_the_request() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["backtest".to_string(), FEATURE_STUDY.to_string()], |_| None);
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let _ = client.run_study(&a_recipe());
    drop(client);
    assert_eq!(rx.recv().expect("count").len(), 1, "an advertised capability must be USED");
    handle.join().expect("join");
}
