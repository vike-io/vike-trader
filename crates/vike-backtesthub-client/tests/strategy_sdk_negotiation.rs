//! The Studio's SDK pick (`StrategySpecWire::File`'s `sdk`, `Request::BuildStrategyFile`'s; decision 0296) and its
//! negotiation, driven against the fake peer — the shape of `slippage_model_negotiation.rs`, for the same reason:
//! `Request` has no `deny_unknown_fields`, so a daemon predating the field would decode the frame, drop the SDK and
//! build the file's own. So the assertion that carries the weight is that NOTHING WAS SENT.
//!
//! Loopback only, no store, no server crate.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{FEATURE_BUILD_STRATEGY_FILE, FEATURE_STRATEGY_SDK};
use vike_backtesthub_client::test_support::{btcusdt_1m_bars, spawn_fake_peer};
use vike_backtesthub_client::wire_studio::StrategySpecWire;

/// A daemon that serves the Studio verbs and the Build, and predates the SDK pick.
fn an_old_backtesthub() -> Vec<String> {
    ["backtest", "run_slice", "run_sweep", "run_walkforward", FEATURE_BUILD_STRATEGY_FILE]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn file(sdk: Option<u32>) -> StrategySpecWire {
    StrategySpecWire::File { file: "trend/a.rs".to_string(), params_toml: String::new(), sdk }
}

/// How many frames the client wrote after the handshake: one `run_slice` of `spec`, then one Build naming `sdk`.
fn frames_sent(
    features: Vec<String>,
    sdk: Option<u32>,
) -> (Result<(), String>, Result<(), String>, usize) {
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let run = client.run_slice(file(sdk), btcusdt_1m_bars(), None).map(|_| ());
    let build = client.build_strategy_file("trend/a.rs", "pub fn build() {}\n", sdk).map(|_| ());
    drop(client);
    let sent = rx.recv().expect("count").len();
    handle.join().expect("fake server thread joins cleanly");
    (run, build, sent)
}

/// **The one that matters.** An SDK an old daemon would drop is refused before the write, on a run and a Build.
#[test]
fn an_sdk_an_old_daemon_would_drop_is_refused_without_sending() {
    let (run, build, sent) = frames_sent(an_old_backtesthub(), Some(4));
    for err in [run.expect_err("run refused client-side"), build.expect_err("build refused")] {
        assert!(err.contains(FEATURE_STRATEGY_SDK) && err.contains("nothing was sent"), "{err}");
    }
    assert_eq!(sent, 0, "the client sent NOTHING");
}

/// The other direction: no SDK (an older client's bytes) goes to any peer, and an advertising daemon is sent the
/// pick. The fake runs nothing, so each fails on its error answer, after the write.
#[test]
fn no_sdk_and_an_advertising_daemon_are_sent() {
    assert_eq!(frames_sent(an_old_backtesthub(), None).2, 2, "no SDK is sent to any peer");
    let mut features = an_old_backtesthub();
    features.push(FEATURE_STRATEGY_SDK.to_string());
    assert_eq!(frames_sent(features, Some(4)).2, 2, "an advertised capability is used");
}
