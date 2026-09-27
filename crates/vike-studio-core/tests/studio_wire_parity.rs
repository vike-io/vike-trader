//! **The S2 parity gate for the three STUDIO verbs**: a run executed LOCALLY (via `run_slice_local`
//! / `run_sweep_local` / `run_walkforward_local`) and the SAME run executed OVER THE WIRE must
//! produce a BIT-IDENTICAL answer. That is what keeps the remote path honest — it is not a second
//! implementation, it is the same `crate::run::run_slice` reached through the serialized DTOs.
//!
//! ⚠ **This file is `vike-datahub`'s `run_slice_roundtrip.rs` and `run_sweep_walkforward_roundtrip.rs`,
//! merged and MOVED here by ruling 7** of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`. Those verbs are served by
//! `vike-backend backtest --addr` now, so a gate driving `vike_datahub::serve` would compare a local
//! run against a wrong-plane refusal.
//!
//! **This crate is the only place the gate can live, and that is the same layer fact the seam itself
//! comes from.** The wire end is `vike_backtest::compute_server` (layer 50); the local end is this
//! crate's own runners (layer 55); layer 50 may not name layer 55, and the edge already runs the
//! other way. So only a test in THIS crate can hold both ends — which is also why it mounts the REAL
//! `studio_run_table` rather than a stub: `crates/vike-backtest/tests/compute_plane.rs` proves the
//! MOUNT and the DISPATCH with stubs because that crate cannot reach the runners, and this file
//! proves the ANSWER because it can.
//!
//! It also pins the load-bearing serde subtlety the sweep half was written for: a sweep whose PBO is
//! `NaN` (a single-point grid — NORMAL operation) crosses the wire as `None`, NOT a decode error,
//! because `WireParamscanResult::pbo` is `Option<f64>`.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over a `DataFusionHist` seeded on a temp dir. The
//! concrete store rather than `MemHistStore` because that double cannot hold bars — its
//! `append_bars`/`load_bars` are inert stubs — so a meaningful run needs the real one.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_backtest::compute_server::serve_with_studio;
use vike_data::{DataFusionHist, HistStore};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::wire_studio::{
    WireEngineParams, WireParamscan, WireSlice, WireSliceKind, WireSpec, WireWalkforward,
    WireWindowSearch,
};
use vike_model::{Bar, QuoteTick, TradeTick};
use vike_studio_core::{run_slice_local, run_sweep_local, run_walkforward_local, studio_run_table};

/// The SMA(5/20)-cross Rhai strategy from `vike_studio_core::run`'s own tests — it TRADES over the
/// seeded store, so the trades vector is non-empty and its round-trip is exercised over the wire.
const CROSS_SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar() {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

/// The parameterized SMA-cross from `vike_studio_core::run`'s sweep tests — `param("fast", ..)` is
/// the axis a `WireParamscan` overrides, so each grid point trades differently.
const SWEEP_SCRIPT: &str = r#"
const QTY = 1.0;
let fast = param("fast", 5.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

/// A `DataFusionHist` over a temp dir seeded with 400 deterministic oscillating BTCUSDT bars (the
/// `vike_studio_core::run::seeded_store` fixture). The `TempDir` is returned so the caller keeps it
/// alive for the store's lifetime.
fn seeded_bar_store() -> (TempDir, Arc<dyn HistStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mut px = 100.0f64;
    let mut seed = 0x1234_5678u64;
    let bars: Vec<Bar> = (0..400)
        .map(|i| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
            Bar {
                ts: 60_000 * (i as i64 + 1),
                open: px,
                high: px,
                low: px,
                close: px,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect();
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    (dir, store)
}

/// A `DataFusionHist` over a temp dir seeded with recorded polymarket/TKN quotes + trades (the
/// `vike_studio_core::run::tick_store` fixture), for the `SliceKind::Ticks` round-trip.
fn seeded_tick_store() -> (TempDir, Arc<dyn HistStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let quotes: Vec<QuoteTick> = (0..200)
        .map(|i| QuoteTick {
            ts: 1_000 * (i as i64 + 1),
            local_ts: 1_000 * (i as i64 + 1),
            bid: 100.0 + (i % 5) as f64,
            ask: 100.5 + (i % 5) as f64,
            bid_size: 5.0,
            ask_size: 5.0,
            symbol: "TKN".to_string(),
        })
        .collect();
    let trades: Vec<TradeTick> = (0..200)
        .map(|i| TradeTick {
            ts: 1_000 * (i as i64 + 1),
            local_ts: 1_000 * (i as i64 + 1),
            price: 100.25 + (i % 5) as f64,
            size: 1.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".to_string(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).unwrap();
    store.append_trades("polymarket", "TKN", &trades, None).unwrap();
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    (dir, store)
}

/// Bind an ephemeral loopback listener and spawn `serve` over `store` on a detached thread; return
/// the assigned address. The store is MOVED into the serve thread (the caller keeps its own clone
/// for the local run).
fn spawn_server(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_studio(listener, store, Some(studio_run_table()));
    });
    addr
}

fn bar_slice() -> WireSlice {
    WireSlice {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string()],
        interval: "1m".to_string(),
        start: None,
        end: None,
        kind: WireSliceKind::Bars,
    }
}

fn assert_curves_bit_identical(local: &[f64], remote: &[f64]) {
    assert_eq!(local.len(), remote.len(), "equity curve lengths differ (local vs remote)");
    for (i, (a, b)) in local.iter().zip(remote).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "equity[{i}] differs: local {a} vs remote {b}");
    }
}

/// The core parity gate: a native `buy_hold` bar run, with a NON-default `WireEngineParams` (so the
/// full DTO conversion path is exercised), produces a bit-identical equity curve local vs remote.
#[test]
fn bar_run_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Native {
        name: "buy_hold".to_string(),
        params_toml: "size = 1.0\nsymbol = \"BTCUSDT\"\n".to_string(),
    };
    let slice = bar_slice();
    let params =
        WireEngineParams { cash: Some(5000.0), fee_rate: Some(0.001), slippage: Some(0.0) };

    // local: drives the SAME `run_slice_local` the server arm does
    let local =
        run_slice_local(&spec, &slice, Some(&params), Arc::clone(&store)).expect("local run");
    // remote: DTOs serialize out, the answer deserializes back
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote =
        client.run_slice(spec.clone(), slice.clone(), Some(params.clone())).expect("remote run");

    assert!(!local.equity_curve.is_empty(), "buy_hold over 400 bars stamps an equity curve");
    assert_curves_bit_identical(&local.equity_curve, &remote.equity_curve);
    assert_eq!(local.final_equity.to_bits(), remote.final_equity.to_bits());
    assert_eq!(local.n_trades, remote.n_trades);
    assert_eq!(local.trades.len(), remote.trades.len());
}

/// A Rhai SMA-cross run TRADES, so this proves the `WireTrade` vector round-trips over the wire
/// bit-for-bit (entry/exit/size/pnl/side), not just the equity curve. `params: None` exercises the
/// all-default engine path.
#[test]
fn rhai_cross_trades_round_trip_bit_identical() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(CROSS_SCRIPT.to_string());
    let slice = bar_slice();

    let local = run_slice_local(&spec, &slice, None, Arc::clone(&store)).expect("local run");
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote = client.run_slice(spec.clone(), slice.clone(), None).expect("remote run");

    assert!(local.n_trades > 0, "the SMA cross should trade over 400 bars");
    assert_eq!(local.n_trades, remote.n_trades);
    assert_eq!(local.trades.len(), remote.trades.len(), "trade counts differ local vs remote");
    for (i, (a, b)) in local.trades.iter().zip(&remote.trades).enumerate() {
        assert_eq!(a.entry_price.to_bits(), b.entry_price.to_bits(), "trade[{i}] entry");
        assert_eq!(a.exit_price.to_bits(), b.exit_price.to_bits(), "trade[{i}] exit");
        assert_eq!(a.size.to_bits(), b.size.to_bits(), "trade[{i}] size");
        assert_eq!(a.pnl.to_bits(), b.pnl.to_bits(), "trade[{i}] pnl");
        assert_eq!(a.is_long, b.is_long, "trade[{i}] side");
    }
    assert_curves_bit_identical(&local.equity_curve, &remote.equity_curve);
}

/// A `SliceKind::Ticks` slice — the case Design A (ship the tape to the laptop) would have ruined —
/// round-trips through `replay_ticks` server-side, bit-identical local vs remote.
#[test]
fn tick_slice_run_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_tick_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Native {
        name: "buy_hold".to_string(),
        params_toml: "symbol = \"TKN\"\n".to_string(),
    };
    let slice = WireSlice {
        venue: "polymarket".to_string(),
        symbols: vec!["TKN".to_string()],
        interval: String::new(),
        start: None,
        end: None,
        kind: WireSliceKind::Ticks,
    };

    let local = run_slice_local(&spec, &slice, None, Arc::clone(&store)).expect("local tick run");
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote = client.run_slice(spec.clone(), slice.clone(), None).expect("remote tick run");

    assert!(!local.equity_curve.is_empty(), "the tick replay stamps an equity curve");
    assert_curves_bit_identical(&local.equity_curve, &remote.equity_curve);
}

/// A bad Rhai script comes back as a `Response::Error` carrying the `compile` class, and the
/// connection SURVIVES — the same client still answers a `Ping` afterward.
#[test]
fn bad_rhai_script_returns_error_and_connection_survives() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(store);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let err = client
        .run_slice(WireSpec::Rhai("fn on_bar( {".to_string()), bar_slice(), None)
        .expect_err("a bad script must not produce a RunResult");
    assert!(err.contains("compile"), "the error carries the compile class: {err}");

    // The connection survived the run error — the SAME client still pings.
    client.ping().expect("connection survives a RunSlice error");
}
#[test]
fn walkforward_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(CROSS_SCRIPT.to_string());
    let slice = bar_slice();
    let wf = WireWalkforward::fixed(4);

    // local: drives the SAME `run_walkforward_local` the server arm does
    let local = run_walkforward_local(&spec, &slice, &wf, None, Arc::clone(&store))
        .expect("local walkforward");
    // remote: DTOs serialize out, the answer deserializes back
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote =
        client.run_walkforward(spec.clone(), slice.clone(), wf, None).expect("remote walkforward");

    assert_eq!(local.windows.len(), 4, "n_splits=4 stitches 4 OOS windows");
    assert_eq!(local.windows.len(), remote.windows.len());
    for (i, (a, b)) in local.windows.iter().zip(&remote.windows).enumerate() {
        assert_eq!(a.test_range, b.test_range, "window[{i}] test_range");
        assert_eq!(a.oos_return.to_bits(), b.oos_return.to_bits(), "window[{i}] oos_return");
    }
    assert!(!local.oos_equity_curve.is_empty(), "the stitched OOS curve is non-empty");
    assert_curves_bit_identical(&local.oos_equity_curve, &remote.oos_equity_curve);
    assert_eq!(local.oos_return.to_bits(), remote.oos_return.to_bits(), "oos_return");
    assert_eq!(local.oos_sharpe.to_bits(), remote.oos_sharpe.to_bits(), "oos_sharpe");
    assert_eq!(local.wf_consistency.to_bits(), remote.wf_consistency.to_bits(), "wf_consistency");
}

/// **The SEARCHING walk-forward's parity gate, and the proof that the arm actually searched.**
///
/// Two claims in one run, because they are the same run: local and remote answer bit-identically
/// (S2, unchanged by the search arm), and the answer is DIFFERENT from the fixed walk's — every
/// window records what it chose, and the stamp names the metric that chose it. Without the second
/// half a green here would prove only that two identical fixed walks agree, which is what the
/// server-side re-check exists to make impossible and what this test would otherwise not notice.
#[test]
fn a_searching_walkforward_is_bit_identical_local_and_remote_and_records_its_choices() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let slice = bar_slice();
    let wf = WireWalkforward::searching(
        4,
        WireWindowSearch {
            method: "sweep".to_string(),
            grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0, 8.0])] },
            rank_by: Some("equity".to_string()),
        },
    );

    let local = run_walkforward_local(&spec, &slice, &wf, None, Arc::clone(&store))
        .expect("local searching walkforward");
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote = client
        .run_walkforward(spec.clone(), slice.clone(), wf.clone(), None)
        .expect("remote searching walkforward");

    assert_eq!(local.windows.len(), 4, "n_splits=4 stitches 4 OOS windows");
    for (i, (a, b)) in local.windows.iter().zip(&remote.windows).enumerate() {
        assert_eq!(a.test_range, b.test_range, "window[{i}] test_range");
        assert_eq!(a.oos_return.to_bits(), b.oos_return.to_bits(), "window[{i}] oos_return");
        // THE SEARCH HAPPENED: each window carries the axis value it picked on its train half,
        // rendered as TOML text, and the two routes picked the same one.
        let chosen = a.chosen_params.as_ref().unwrap_or_else(|| {
            panic!("window[{i}] recorded no choice — a searching walk that chose nothing is the fixed walk")
        });
        assert_eq!(chosen.len(), 1, "one swept axis, one recorded choice");
        assert_eq!(chosen[0].0, "fast");
        assert_eq!(Some(chosen), b.chosen_params.as_ref(), "window[{i}] choice differs remote");
    }
    assert_curves_bit_identical(&local.oos_equity_curve, &remote.oos_equity_curve);
    assert_eq!(local.oos_sharpe.to_bits(), remote.oos_sharpe.to_bits(), "oos_sharpe");

    // …and the stamp says what chose the winner, identically on both routes.
    let stamp = local.cost_model.as_ref().expect("a searching walk is stamped");
    assert_eq!(stamp.rank_metric.as_deref(), Some("equity"), "the metric the caller asked for");
    assert_eq!(stamp.source, "derived");
    assert_eq!(stamp.lane.as_deref(), Some("binance"));
    assert_eq!(local.cost_model, remote.cost_model, "the stamp is one answer, not two");
}

/// **The winner is RUN, not merely recorded.** A window that reported a choice it did not apply
/// would satisfy every assertion above — `chosen_params` would still be populated and the two
/// routes would still agree — so this closes that hole with a one-point grid whose value is NOT the
/// script's default: the search can only pick `fast = 3`, and the walk must therefore produce a
/// DIFFERENT out-of-sample curve from the control, which walks the script's own `fast = 5`.
///
/// A one-point grid rather than a wide one on purpose: with three candidates the winner might BE
/// the default, and the assertion would be a coin toss dressed as a gate.
#[test]
fn the_chosen_point_actually_runs_the_validation_half() {
    let (_dir, store) = seeded_bar_store();
    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());

    let control = run_walkforward_local(
        &spec,
        &bar_slice(),
        &WireWalkforward::fixed(4),
        None,
        Arc::clone(&store),
    )
    .expect("the control walks the script's own fast = 5");
    let searched = run_walkforward_local(
        &spec,
        &bar_slice(),
        &WireWalkforward::searching(
            4,
            WireWindowSearch {
                method: "sweep".to_string(),
                grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0])] },
                rank_by: None,
            },
        ),
        None,
        store,
    )
    .expect("the searching walk has exactly one candidate to pick");

    for (i, w) in searched.windows.iter().enumerate() {
        assert_eq!(
            w.chosen_params,
            Some(vec![("fast".to_string(), "3.0".to_string())]),
            "window[{i}] must record the only candidate there was"
        );
    }
    assert_ne!(
        searched.oos_equity_curve, control.oos_equity_curve,
        "a walk that CHOSE fast = 3 and then ran fast = 5 would be a report about a run that never \
         happened — the recorded choice has to reach the validation engine"
    );
}

/// The FIXED walk over the same fixture records NO choice and names NO metric — the control, and
/// the contrast that makes the test above mean something.
#[test]
fn the_fixed_walk_records_no_choice_and_names_no_rank_metric() {
    let (_dir, store) = seeded_bar_store();
    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let local = run_walkforward_local(&spec, &bar_slice(), &WireWalkforward::fixed(4), None, store)
        .expect("local fixed walkforward");
    for (i, w) in local.windows.iter().enumerate() {
        assert_eq!(w.chosen_params, None, "window[{i}] of a FIXED walk chose nothing");
    }
    let stamp = local.cost_model.as_ref().expect("a fixed walk is stamped too");
    assert_eq!(
        stamp.rank_metric, None,
        "nothing was ranked, so naming a metric would claim a provenance this report does not have"
    );
    // It IS priced, though — that is the half of 0063 that is not about searching at all.
    assert_eq!(stamp.source, "derived");
    assert_eq!(stamp.variant, "PercentMakerTaker");
}

/// **The refusal a capability cannot express**, proven over the REAL server: a method the daemon
/// does not know is a named error, never a fixed walk wearing a successful report. This is the
/// server-side half of the negotiation — the client-side half is
/// `crates/vike-datahub-client/tests/walkforward_search_negotiation.rs`, and neither is reachable
/// by the other's route.
#[test]
fn the_server_refuses_an_unknown_search_method_rather_than_walking_fixed() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));
    let mut client = DatahubClient::connect(addr).expect("connect");

    let wf = WireWalkforward::searching(
        4,
        WireWindowSearch {
            method: "bayesian".to_string(),
            grid: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
            rank_by: None,
        },
    );
    let err = client
        .run_walkforward(WireSpec::Rhai(SWEEP_SCRIPT.to_string()), bar_slice(), wf, None)
        .expect_err("an unknown method must not produce a WalkforwardResult");
    assert!(err.contains("bayesian"), "the refusal quotes what was sent: {err}");
    assert!(err.contains("none | sweep"), "and names the accepted set: {err}");
    // The connection survived the refusal, like every other run error on this verb.
    client.ping().expect("connection survives a refused search");
}

/// A `sweep` method with an EMPTY grid is refused rather than quietly becoming the fixed walk:
/// `cartesian(&[])` answers ONE point carrying no overrides, so the report would record a "choice"
/// that chose nothing while claiming to have optimized.
#[test]
fn the_server_refuses_a_sweep_with_no_axes() {
    let (_dir, store) = seeded_bar_store();
    let wf = WireWalkforward::searching(
        4,
        WireWindowSearch {
            method: "sweep".to_string(),
            grid: WireParamscan::default(),
            rank_by: None,
        },
    );
    let err = run_walkforward_local(
        &WireSpec::Rhai(SWEEP_SCRIPT.to_string()),
        &bar_slice(),
        &wf,
        None,
        store,
    )
    .expect_err("an empty grid must be refused");
    assert!(err.message.contains("GRID"), "the refusal says what is missing: {err:?}");
}

/// **The cost model is DERIVED, and the derivation reaches the engine** — a run with no `fee_rate`
/// on the wire pays the lane's real fees, where every Studio run before `0063` paid nothing.
///
/// Asserted as a RELATION against a zero-fee override rather than against a copied rate:
/// `vike_model::fees` is the one authority for what binance charges, and pinning a number here
/// would be a second one.
#[test]
fn a_derived_schedule_actually_charges_where_an_explicit_zero_does_not() {
    let (_dir, store) = seeded_bar_store();
    let spec = WireSpec::Rhai(CROSS_SCRIPT.to_string());

    let derived =
        run_slice_local(&spec, &bar_slice(), None, Arc::clone(&store)).expect("derived-cost run");
    let free = run_slice_local(
        &spec,
        &bar_slice(),
        Some(&WireEngineParams { cash: None, fee_rate: Some(0.0), slippage: None }),
        Arc::clone(&store),
    )
    .expect("explicitly-free run");

    let d = derived.cost_model.as_ref().expect("stamped");
    let f = free.cost_model.as_ref().expect("stamped");
    assert_eq!(d.source, "derived");
    assert_eq!(f.source, "override", "an explicit zero is an OVERRIDE, not an absence");

    assert!(d.taker_fills > 0, "the fixture strategy trades, so the mix is non-trivial");
    assert!(
        d.fees_paid > 0.0,
        "a derived binance schedule CHARGES — this is the zero-cost backtest 0063 exists to end"
    );
    assert_eq!(f.fees_paid, 0.0, "an explicit zero rate charges nothing");
    assert!(
        derived.final_equity < free.final_equity,
        "and the charge reaches the equity curve: {} vs {}",
        derived.final_equity,
        free.final_equity
    );
    // The shipped Studio population is 100% market orders, so the maker side never engages —
    // 0063's scoping finding, made visible by the stamp rather than only stated in the record.
    assert_eq!(d.maker_fills, 0, "every template order is a market order");
}

/// The sweep verb is stamped too, and its stamp names the metric its code actually ranks by.
#[test]
fn a_sweep_answer_carries_the_cost_model_that_priced_every_point() {
    let (_dir, store) = seeded_bar_store();
    let sweep = WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] };
    let result = run_sweep_local(
        &WireSpec::Rhai(SWEEP_SCRIPT.to_string()),
        &bar_slice(),
        &sweep,
        None,
        store,
    )
    .expect("local sweep");
    let stamp = result.cost_model.as_ref().expect("a ranked answer is stamped");
    assert_eq!(stamp.source, "derived");
    assert_eq!(stamp.rank_metric.as_deref(), Some("sharpe"));
    assert!(stamp.taker_fills > 0, "the mix sums every point, and every point traded");
    // Per-entry results carry no stamp: one cost model priced them all, and repeating it per row
    // would invite a reader to look for a difference that cannot exist.
    assert!(result.entries.iter().all(|e| e.result.cost_model.is_none()));
}

/// The sweep parity gate: a MULTI-point grid produces bit-identical ranked entries (overrides + the
/// rendered result) and identical dsr/pbo/best_index local vs remote.
#[test]
fn sweep_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let slice = bar_slice();
    let sweep = WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0, 8.0])] };

    let local =
        run_sweep_local(&spec, &slice, &sweep, None, Arc::clone(&store)).expect("local sweep");
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote = client
        .run_paramscan(spec.clone(), slice.clone(), sweep.clone(), None)
        .expect("remote sweep");

    assert_eq!(local.entries.len(), 3, "one entry per grid point");
    assert_eq!(local.entries.len(), remote.entries.len());
    for (i, (a, b)) in local.entries.iter().zip(&remote.entries).enumerate() {
        assert_eq!(a.overrides, b.overrides, "entry[{i}] overrides");
        assert_curves_bit_identical(&a.result.equity_curve, &b.result.equity_curve);
        assert_eq!(
            a.result.final_equity.to_bits(),
            b.result.final_equity.to_bits(),
            "entry[{i}] final_equity"
        );
        assert_eq!(a.result.trades.len(), b.result.trades.len(), "entry[{i}] trades");
    }
    assert_eq!(local.best_index, remote.best_index, "best_index");
    assert_eq!(local.dsr.map(f64::to_bits), remote.dsr.map(f64::to_bits), "dsr");
    assert_eq!(local.pbo.map(f64::to_bits), remote.pbo.map(f64::to_bits), "pbo");
}

/// The LOAD-BEARING serde test: a single-point grid leaves `StudioParamscan::pbo` as `NaN` in NORMAL
/// operation. Because `WireParamscanResult::pbo` is `Option<f64>`, that `NaN` crosses the wire as `None`
/// and `run_paramscan` returns `Ok` — a plain `f64` field would serialize the `NaN` to `null` and then
/// FAIL to decode, turning this ordinary sweep into a client-side decode error. This test is the one
/// that fails if `pbo` were a plain `f64`.
#[test]
fn sweep_single_point_grid_pbo_is_none_not_a_decode_error() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(store);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let sweep = WireParamscan { axes: vec![("fast".to_string(), vec![5.0])] };

    let remote = client
        .run_paramscan(spec, bar_slice(), sweep, None)
        .expect("a single-point sweep must decode (not a `null`-into-f64 decode error)");
    assert_eq!(remote.entries.len(), 1, "one entry for a one-value grid");
    assert_eq!(remote.pbo, None, "single trial -> PBO not assessable, arrives as None");
}

/// A bad Rhai script makes every sweep point fail, which studio-core degrades to a generic
/// `compile`-class error (`"every sweep point failed to run"`). It comes back as a `Response::Error`
/// carrying that class, and the connection SURVIVES — the same client still answers a `Ping`.
#[test]
fn sweep_bad_script_returns_error_and_connection_survives() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(store);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let sweep = WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] };
    let err = client
        .run_paramscan(WireSpec::Rhai("fn on_bar( {".to_string()), bar_slice(), sweep, None)
        .expect_err("a bad script must not produce a SweepResult");
    // Assert the ACTUAL kind prefix — sweep does NOT surface a real Rhai line/col compile message
    // here, it surfaces studio-core's generic "every sweep point failed to run" under `compile`.
    assert!(err.contains("compile"), "the error carries the compile class: {err}");

    // The connection survived the run error — the SAME client still pings.
    client.ping().expect("connection survives a RunSweep error");
}

/// The v6 gap-closer: a `RunSweep` carrying a custom `cash` HONORS it — every grid point's equity
/// curve seeds at that cash, NOT the `EngineParams::default()` 10_000. This is the whole point of
/// the change: before v6 the sweep verb dropped `[engine]` costs and every point ran default cash.
/// Also proves local and remote agree bit-for-bit WITH a non-default params override.
#[test]
fn sweep_honors_custom_cash_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let slice = bar_slice();
    let sweep = WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] };
    let params = WireEngineParams { cash: Some(50_000.0), fee_rate: None, slippage: None };

    // default-cash run (no params) for the contrast: its curves seed at 10_000.
    let default_run =
        run_sweep_local(&spec, &slice, &sweep, None, Arc::clone(&store)).expect("default sweep");
    assert_eq!(
        default_run.entries[0].result.equity_curve.first().copied(),
        Some(10_000.0),
        "the default sweep still seeds at EngineParams::default().cash"
    );

    // custom-cash run: every point's equity curve now starts at 50_000.
    let local = run_sweep_local(&spec, &slice, &sweep, Some(&params), Arc::clone(&store))
        .expect("local custom-cash sweep");
    for (i, e) in local.entries.iter().enumerate() {
        assert_eq!(
            e.result.equity_curve.first().copied(),
            Some(50_000.0),
            "entry[{i}] equity curve seeds at the supplied cash, not the default"
        );
    }

    // and the SAME custom-cash run over the wire is bit-identical to local.
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote = client
        .run_paramscan(spec.clone(), slice.clone(), sweep.clone(), Some(params))
        .expect("remote custom-cash sweep");
    assert_eq!(local.entries.len(), remote.entries.len());
    for (a, b) in local.entries.iter().zip(&remote.entries) {
        assert_curves_bit_identical(&a.result.equity_curve, &b.result.equity_curve);
    }
}
