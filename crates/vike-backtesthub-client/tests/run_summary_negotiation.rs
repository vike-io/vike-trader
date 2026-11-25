//! The SUMMARIZED Studio runs' negotiation (`FEATURE_RUN_SUMMARY`, decision 0078's amendment of
//! 2026-11-16), driven against the fake peer: a backtesthub older than the summary is refused in words
//! by the desktop, before a frame is written, because the desktop and the backtesthub ship together.
//!
//! Loopback only, no store, no server crate.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::FEATURE_RUN_SUMMARY;
use vike_backtesthub_client::test_support::{btcusdt_1m_bars, spawn_fake_peer};
use vike_backtesthub_client::wire_studio::{ParamscanWire, StrategySpecWire, WalkforwardWire};

/// A daemon that serves the Studio verbs and predates the summary.
fn an_old_backtesthub() -> Vec<String> {
    ["backtest", "run_slice", "run_sweep", "run_walkforward", "walkforward_search"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// One summarized verb, called on a connected client.
type Call = Box<dyn Fn(&mut BacktesthubClient) -> Result<(), String>>;

/// Every summarized verb against `features`: each one's outcome and how many frames went out.
fn every_verb(features: Vec<String>) -> Vec<(Result<(), String>, usize)> {
    let spec = || StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    let calls: Vec<Call> = vec![
        Box::new(move |c| c.run_slice_summary(spec(), btcusdt_1m_bars(), None).map(|_| ())),
        Box::new(move |c| {
            c.run_paramscan_summary(spec(), btcusdt_1m_bars(), ParamscanWire::default(), None)
                .map(|_| ())
        }),
        Box::new(move |c| {
            c.run_walkforward_summary(spec(), btcusdt_1m_bars(), WalkforwardWire::fixed(2), None)
                .map(|_| ())
        }),
        Box::new(|c| c.read_trades("a-1", None, 0, 200).map(|_| ())),
        Box::new(|c| c.read_point("a-1", 1).map(|_| ())),
    ];
    calls
        .into_iter()
        .map(|call| {
            let (addr, rx, handle) = spawn_fake_peer(features.clone(), |_| None);
            let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
            let out = call(&mut client);
            drop(client);
            let sent = rx.recv().expect("count").len();
            handle.join().expect("fake server thread joins cleanly");
            (out, sent)
        })
        .collect()
}

/// An old backtesthub: every summarized verb refused in words naming the capability and the cure,
/// and nothing sent.
#[test]
fn an_old_backtesthub_is_refused_in_words_without_sending() {
    for (out, sent) in every_verb(an_old_backtesthub()) {
        let err = out.expect_err("refused client-side");
        assert!(
            err.contains(FEATURE_RUN_SUMMARY)
                && err.contains("nothing was sent")
                && err.contains("ship together"),
            "{err}"
        );
        assert_eq!(sent, 0, "the client sent NOTHING");
    }
}

/// A backtesthub that advertises the summary is sent every verb (the fake runs nothing, so each fails
/// on its answer, after the write).
#[test]
fn an_advertising_backtesthub_is_sent_every_verb() {
    let mut features = an_old_backtesthub();
    features.push(FEATURE_RUN_SUMMARY.to_string());
    for (_, sent) in every_verb(features) {
        assert_eq!(sent, 1);
    }
}
