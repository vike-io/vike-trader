//! The Studio run fields of 2026-11-20 against an OLDER backtesthub, driven against the fake peer.
//!
//! Two REQUEST fields: the funding switch (`StrategyEngineConfigWire::attach_funding`, `FEATURE_ATTACH_FUNDING`), which a
//! daemon predating it would decode, drop and charge a perpetual's funding, and a Rhai buffer's parameters
//! (`StrategySpecWire::RhaiWithParameters`, `FEATURE_RHAI_PARAMETERS`), which it could not decode. Each is refused
//! before the write, and a request carrying neither is an older client's bytes, sent to any peer. Three REPLY fields
//! (the curve times, the window times, a scan point's drawdown) need no word (decision 0112, verdict 3): an older
//! server's frame reads them as absent, and the client draws what it drew before.
//!
//! Loopback only, no store, no server crate.

use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::proto::{FEATURE_ATTACH_FUNDING, FEATURE_RHAI_PARAMETERS};
use vike_backtesthub_client::summary::{
    ParamscanPointWire, RunSummaryWire, WalkforwardSummaryWire,
};
use vike_backtesthub_client::test_support::{btcusdt_1m_bars, spawn_fake_peer};
use vike_backtesthub_client::wire_studio::{
    ParamscanWire, StrategyEngineConfigWire, StrategySpecWire, WalkforwardResultsWire,
};

/// A daemon that serves the Studio verbs and the summaries and predates both fields.
fn an_old_backtesthub() -> Vec<String> {
    ["backtest", "run_slice", "run_sweep", "run_walkforward", "run_summary"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn funding_off() -> StrategyEngineConfigWire {
    StrategyEngineConfigWire { attach_funding: Some(false), ..StrategyEngineConfigWire::default() }
}

fn rhai_with_parameters() -> StrategySpecWire {
    StrategySpecWire::RhaiWithParameters {
        source: "fn on_bar() {}".to_string(),
        parameters_toml: "fast = 7.0\ncommission = 0.1\n".to_string(),
    }
}

/// The client's answer and how many frames it wrote after the handshake, for one `run_slice` (or, with `scan`, one
/// summarized scan) of `spec` with `params`. The fake runs nothing, so a sent request fails on its answer.
fn frames_sent(
    features: Vec<String>,
    spec: StrategySpecWire,
    params: Option<StrategyEngineConfigWire>,
    scan: bool,
) -> (Result<(), String>, usize) {
    let (addr, rx, handle) = spawn_fake_peer(features, |_| None);
    let mut client = BacktesthubClient::connect(addr).expect("handshake succeeds");
    let out = if scan {
        let grid = ParamscanWire { axes: Vec::new() };
        client.run_paramscan_summary(spec, btcusdt_1m_bars(), grid, params).map(|_| ())
    } else {
        client.run_slice(spec, btcusdt_1m_bars(), params).map(|_| ())
    };
    drop(client);
    let sent = rx.recv().expect("count").len();
    handle.join().expect("fake server thread joins cleanly");
    (out, sent)
}

/// **The one that matters.** Each request field an older daemon would drop or could not decode is refused before the
/// write, naming its word, on a plain run and a summarized one alike.
#[test]
fn a_field_an_old_daemon_would_drop_is_refused_without_sending() {
    let rhai = || StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    for scan in [false, true] {
        let (out, sent) = frames_sent(an_old_backtesthub(), rhai(), Some(funding_off()), scan);
        let err = out.expect_err("the funding switch is refused client-side");
        assert!(err.contains(FEATURE_ATTACH_FUNDING) && err.contains("nothing was sent"), "{err}");
        assert_eq!(sent, 0, "the client sent NOTHING");

        let (out, sent) = frames_sent(an_old_backtesthub(), rhai_with_parameters(), None, scan);
        let err = out.expect_err("the parameters are refused client-side");
        assert!(err.contains(FEATURE_RHAI_PARAMETERS) && err.contains("nothing was sent"), "{err}");
        assert_eq!(sent, 0, "the client sent NOTHING");
    }
}

/// The other direction: a request carrying neither field goes to an older daemon, and a daemon that advertises both
/// words is sent both.
#[test]
fn a_request_without_the_fields_and_an_advertising_daemon_are_sent() {
    let rhai = StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    let cash =
        StrategyEngineConfigWire { cash: Some(5_000.0), ..StrategyEngineConfigWire::default() };
    assert!(!cash.needs_attach_funding());
    assert_eq!(frames_sent(an_old_backtesthub(), rhai, Some(cash), false).1, 1);
    let mut features = an_old_backtesthub();
    features.extend([FEATURE_ATTACH_FUNDING.to_string(), FEATURE_RHAI_PARAMETERS.to_string()]);
    let (_, sent) = frames_sent(features, rhai_with_parameters(), Some(funding_off()), false);
    assert_eq!(sent, 1, "advertised capabilities are used");
}

/// No switch: the engine config's bytes are an older client's; the switch set crosses as one key and reads back.
#[test]
fn the_funding_switch_is_one_key_and_absent_when_unset() {
    let unset = StrategyEngineConfigWire { cash: Some(0.5), ..StrategyEngineConfigWire::default() };
    assert_eq!(
        serde_json::to_string(&unset).unwrap(),
        r#"{"cash":0.5,"fee_rate":null,"slippage":null}"#
    );
    let off = StrategyEngineConfigWire { attach_funding: Some(false), ..unset };
    let json = serde_json::to_string(&off).unwrap();
    assert_eq!(json, r#"{"cash":0.5,"fee_rate":null,"slippage":null,"attach_funding":false}"#);
    assert_eq!(serde_json::from_str::<StrategyEngineConfigWire>(&json).unwrap(), off);
}

/// An OLDER server's reply frames, as it wrote them, read by this client: no curve times, no window times, no
/// drawdown, each read as absent, so the client draws step numbers and an empty drawdown cell as before.
#[test]
fn an_older_servers_replies_read_the_new_fields_as_absent() {
    let summary: RunSummaryWire = serde_json::from_str(
        r#"{"rows":[],"sharpe":null,"total_return":null,"final_equity":1.0,"n_trades":0,
            "probabilistic_sharpe":null,"curve":[[0,1.0],[1,2.0]],"curve_len":2,
            "returns_histogram":{"low":0.0,"width":0.0,"counts":[]}}"#,
    )
    .expect("an older summary decodes");
    assert!(summary.curve_timestamp.is_empty());

    let point: ParamscanPointWire = serde_json::from_str(
        r#"{"overrides":[],"sharpe":null,"total_return":null,"final_equity":1.0,"n_trades":0}"#,
    )
    .expect("an older point decodes");
    assert_eq!(point.max_drawdown, None);

    let walk = WalkforwardResultsWire { equity_curve: vec![1.0, 2.0], ..empty_walk() };
    let json = serde_json::to_string(&walk).unwrap();
    assert!(!json.contains("equity_timestamp"), "an empty list is not written: {json}");
    let back: WalkforwardResultsWire = serde_json::from_str(&json).unwrap();
    assert!(WalkforwardSummaryWire::of(back).equity_curve_timestamp.is_empty());
}

/// A walk with no step and no curve, its overall results the default record's.
fn empty_walk() -> WalkforwardResultsWire {
    let record = vike_analytics::TradingRecord::default();
    WalkforwardResultsWire {
        steps: Vec::new(),
        overall: vike_analytics::results::TradingResults::from_record(None, &record, 252.0),
        equity_curve: Vec::new(),
        equity_timestamp: Vec::new(),
        profitable_step_share: 0.0,
        account_currency: None,
        cost_model: None,
    }
}
