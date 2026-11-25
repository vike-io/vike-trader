//! The Studio SLIPPAGE MODEL capability's negotiation, driven against the fake peer — the shape of
//! `walkforward_search_negotiation.rs`, for the same reason: `Request` has no
//! `deny_unknown_fields`, so a daemon predating `StrategyEngineConfigWire::slippage_model` would decode the
//! frame, drop the model and the tick, and charge the VALUE as a flat fraction of the price (two
//! ticks as 200%). So the assertion that carries the weight is that NOTHING WAS SENT.
//!
//! Loopback only, no store, no server crate.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::FEATURE_SLIPPAGE_MODEL;
use vike_backtesthub_client::test_support::{btcusdt_1m_bars, spawn_fake_peer};
use vike_backtesthub_client::wire_studio::{StrategyEngineConfigWire, StrategySpecWire};

/// A daemon that serves the Studio verbs and predates the slippage model.
fn an_old_backtesthub() -> Vec<String> {
    ["backtest", "run_slice", "run_sweep", "run_walkforward", "walkforward_search"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn ticks() -> StrategyEngineConfigWire {
    StrategyEngineConfigWire {
        slippage: Some(2.0),
        slippage_model: Some("ticks".to_string()),
        slippage_tick: Some(0.01),
        ..StrategyEngineConfigWire::default()
    }
}

/// How many frames the client wrote after the handshake, for one `run_slice` with `params`.
fn frames_sent(
    features: Vec<String>,
    params: StrategyEngineConfigWire,
) -> (Result<(), String>, usize) {
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let spec = StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    let out = client.run_slice(spec, btcusdt_1m_bars(), Some(params)).map(|_| ());
    drop(client);
    let sent = rx.recv().expect("count").len();
    handle.join().expect("fake server thread joins cleanly");
    (out, sent)
}

/// **The one that matters.** A model an old daemon would drop is refused before the write.
#[test]
fn a_slippage_model_an_old_daemon_would_drop_is_refused_without_sending() {
    let (out, sent) = frames_sent(an_old_backtesthub(), ticks());
    let err = out.expect_err("refused client-side");
    assert!(err.contains(FEATURE_SLIPPAGE_MODEL) && err.contains("nothing was sent"), "{err}");
    assert_eq!(sent, 0, "the client sent NOTHING");
}

/// The other direction: a bare value (the flat fraction, the field's meaning before the model) and a
/// daemon that advertises the capability are both SENT. The fake runs nothing, so each fails on its
/// error answer, after the write.
#[test]
fn a_bare_value_and_an_advertising_daemon_are_sent() {
    let bare =
        StrategyEngineConfigWire { slippage: Some(0.0005), ..StrategyEngineConfigWire::default() };
    assert!(!bare.needs_capability());
    assert_eq!(frames_sent(an_old_backtesthub(), bare).1, 1, "a bare value is sent to any peer");
    let mut features = an_old_backtesthub();
    features.push(FEATURE_SLIPPAGE_MODEL.to_string());
    assert_eq!(frames_sent(features, ticks()).1, 1, "an advertised capability is used");
}

/// No model and no tick: the frame is byte-identical to the one before the fields existed, and an
/// old frame decodes for a new peer as no model.
#[test]
fn params_without_a_model_serialize_without_the_new_keys() {
    let bare =
        StrategyEngineConfigWire { slippage: Some(0.25), ..StrategyEngineConfigWire::default() };
    let json = serde_json::to_string(&bare).expect("serialize");
    assert_eq!(json, r#"{"cash":null,"fee_rate":null,"slippage":0.25}"#);
    let back: StrategyEngineConfigWire = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, bare);
}
