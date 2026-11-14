//! **The S2 parity gate for the three STUDIO verbs**: a run executed LOCALLY (via `run_slice_local`
//! / `run_paramscan_local` / `run_walkforward_local`) and the SAME run executed OVER THE WIRE must
//! produce a BIT-IDENTICAL answer. That is what keeps the remote path honest — it is not a second
//! implementation, it is the same `crate::run::run_slice` reached through the serialized DTOs.
//!
//! ⚠ **This file is `vike-datahub`'s `run_slice_roundtrip.rs` and `run_sweep_walkforward_roundtrip.rs`,
//! merged and MOVED here by ruling 7** of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`. Those verbs are served by
//! `vike-backend backtesthub --addr` now, so a gate driving `vike_datahub::serve` would compare a local
//! run against a wrong-plane refusal.
//!
//! **This crate is the natural place for the gate**: the wire end is `vike_backtesthub::backtesthub_server`
//! (layer 65, an upward DEV edge from here), the local end is this crate's own runners, and the server
//! calls those runners DIRECTLY now. (It used to mount them through a boxed `StudioRunTable`, built
//! by `studio_run_table`, because the server sat at layer 30 and could not name this crate; that seam
//! and its stub-table tests are gone.) `crates/vike-backtesthub/tests/backtest_plane.rs` proves the
//! negotiation and the dispatch of every verb over a real socket; this file proves the ANSWER is
//! bit-identical to the local one.
//!
//! It also pins the load-bearing serde subtlety the sweep half was written for: a sweep whose PBO is
//! `NaN` (a single-point grid — NORMAL operation) crosses the wire as `None`, NOT a decode error,
//! because `StudioParamscanWire::pbo` is `Option<f64>`.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over a `HistoryStoreDataFusion` seeded on a temp dir. The
//! concrete store rather than `HistoryStoreMemory` because that double cannot hold bars — its
//! `append_bars`/`load_bars` are inert stubs — so a meaningful run needs the real one.

use std::sync::Arc;

use cross_script::{CROSS_SCRIPT, LONG_CROSS_SCRIPT};
use sweep_script::SWEEP_SCRIPT;
use tempfile::TempDir;
use vike_backtesthub::studio_run::{
    run_paramscan_local, run_slice, run_slice_local, run_walkforward_local,
    run_walkforward_slice_with_params,
};
use vike_backtesthub::test_support::spawn_server_over;
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::wire_studio::{
    DataSliceWire, ParamscanWire, ReplayKindWire, StrategyEngineConfigWire, StrategySpecWire,
    WalkforwardWindowSearchWire, WalkforwardWire,
};
use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::test_support::flat_bar_zero_volume;
use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::params::ParamValue;
use vike_store::HistoryStoreDataFusion;
use vike_studio_core::{DataSlice, StrategySpec};
// `cross_script`: the SMA(5/20)-cross Rhai strategies from `vike_studio_core::run`'s own tests — the
// long-only one TRADES over the seeded store (binance `BTCUSDT`, a spot pair, judged by the spot law), so
// the trades vector is non-empty and its round-trip is exercised over the wire. `sweep_script`: the parameterized SMA-cross from `vike_studio_core::run`'s sweep tests —
// `param("fast", ..)` is the axis a `ParamscanWire` overrides, so each grid point trades differently.
use vike_backtesthub_client::test_support::btcusdt_1m_bars;
use vike_studio_core::test_support::walk::random_walk_bars;
use vike_studio_core::test_support::{cross_script, sweep_script};

/// A `HistoryStoreDataFusion` over a temp dir seeded with 400 deterministic oscillating BTCUSDT bars (the
/// `crates/vike-backtesthub/src/studio_run/tests/support.rs`'s `seeded_store` fixture). The `TempDir` is returned so the caller keeps it
/// alive for the store's lifetime.
fn seeded_bar_store() -> (TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars = random_walk_bars(400, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    (dir, store)
}

/// A `HistoryStoreDataFusion` over a temp dir seeded with recorded polymarket/TKN quotes + trades (the
/// `crates/vike-backtesthub/src/studio_run/tests/tick_slices.rs`'s `tick_store` fixture), for the `ReplayKind::Tick` round-trip.
fn seeded_tick_store() -> (TempDir, Arc<dyn HistoryStore + Send + Sync>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let quotes: Vec<TickQuote> = (0..200)
        .map(|i| TickQuote {
            timestamp: 1_000 * (i as i64 + 1),
            local_timestamp: 1_000 * (i as i64 + 1),
            bid: 100.0 + (i % 5) as f64,
            ask: 100.5 + (i % 5) as f64,
            bid_size: 5.0,
            ask_size: 5.0,
            symbol: "TKN".to_string(),
        })
        .collect();
    let trades: Vec<TickTrade> = (0..200)
        .map(|i| TickTrade {
            timestamp: 1_000 * (i as i64 + 1),
            local_timestamp: 1_000 * (i as i64 + 1),
            price: 100.25 + (i % 5) as f64,
            size: 1.0,
            is_buyer_maker: i % 2 == 0,
            symbol: "TKN".to_string(),
        })
        .collect();
    store.append_quotes("polymarket", "TKN", &quotes, None).unwrap();
    store.append_trades("polymarket", "TKN", &trades, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    (dir, store)
}

fn assert_curves_bit_identical(local: &[f64], remote: &[f64]) {
    assert_eq!(local.len(), remote.len(), "equity curve lengths differ (local vs remote)");
    for (i, (a, b)) in local.iter().zip(remote).enumerate() {
        assert_eq!(a.to_bits(), b.to_bits(), "equity[{i}] differs: local {a} vs remote {b}");
    }
}

/// The core parity gate: a native `buy_hold` bar run, with a NON-default `StrategyEngineConfigWire` (so the
/// full DTO conversion path is exercised), produces a bit-identical equity curve local vs remote.
#[test]
fn bar_run_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: "size = 1.0\nsymbol = \"BTCUSDT\"\n".to_string(),
    };
    let slice = btcusdt_1m_bars();
    let params = StrategyEngineConfigWire {
        cash: Some(5000.0),
        fee_rate: Some(0.001),
        slippage: Some(0.0),
        ..StrategyEngineConfigWire::default()
    };

    // local: drives the SAME `run_slice_local` the server arm does
    let local = run_slice_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        Some(&params),
        Arc::clone(&store),
    )
    .expect("local run");
    // remote: DTOs serialize out, the answer deserializes back
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote =
        client.run_slice(spec.clone(), slice.clone(), Some(params.clone())).expect("remote run");

    assert!(!local.equity_curve.is_empty(), "buy_hold over 400 bars stamps an equity curve");
    assert_curves_bit_identical(&local.equity_curve, &remote.equity_curve);
    assert_eq!(local.final_equity.to_bits(), remote.final_equity.to_bits());
    assert_eq!(local.n_trades, remote.n_trades);
    assert_eq!(local.trades.len(), remote.trades.len());
}

/// A Rhai SMA-cross run TRADES, so this proves the `vike_model::Trade` vector round-trips over the wire
/// bit-for-bit (entry/exit/size/pnl/side), not just the equity curve. `params: None` exercises the
/// all-default engine path.
#[test]
fn rhai_cross_trades_round_trip_bit_identical() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::Rhai(LONG_CROSS_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();

    let local = run_slice_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        None,
        Arc::clone(&store),
    )
    .expect("local run");
    let mut client = BacktesthubClient::connect(addr).expect("connect");
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

/// A `ReplayKind::Tick` slice — the case Design A (ship the tape to the laptop) would have ruined —
/// round-trips through `replay_ticks` server-side, bit-identical local vs remote.
#[test]
fn tick_slice_run_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_tick_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: "symbol = \"TKN\"\n".to_string(),
    };
    let slice = DataSliceWire {
        venue: "polymarket".to_string(),
        symbols: vec!["TKN".to_string()],
        interval: String::new(),
        start: None,
        end: None,
        kind: ReplayKindWire::Ticks,
    };

    let local = run_slice_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        None,
        Arc::clone(&store),
    )
    .expect("local tick run");
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client.run_slice(spec.clone(), slice.clone(), None).expect("remote tick run");

    assert!(!local.equity_curve.is_empty(), "the tick replay stamps an equity curve");
    assert_curves_bit_identical(&local.equity_curve, &remote.equity_curve);
}

/// A bad Rhai script comes back as a `Response::Error` carrying the `compile` class, and the
/// connection SURVIVES — the same client still answers a `Ping` afterward.
#[test]
fn bad_rhai_script_returns_error_and_connection_survives() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(store, vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let err = client
        .run_slice(StrategySpecWire::Rhai("fn on_bar( {".to_string()), btcusdt_1m_bars(), None)
        .expect_err("a bad script must not produce a RunResult");
    assert!(err.contains("compile"), "the error carries the compile class: {err}");

    // The connection survived the run error — the SAME client still pings.
    client.ping().expect("connection survives a RunSlice error");
}
#[test]
fn walkforward_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::Rhai(CROSS_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let walkforward = WalkforwardWire::fixed(4);

    // local: drives the SAME `run_walkforward_local` the server arm does
    let local = run_walkforward_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &walkforward,
        None,
        Arc::clone(&store),
    )
    .expect("local walkforward");
    // remote: DTOs serialize out, the answer deserializes back
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client
        .run_walkforward(spec.clone(), slice.clone(), walkforward, None)
        .expect("remote walkforward");

    assert_eq!(local.windows.len(), 4, "n_splits=4 stitches 4 OOS windows");
    assert_eq!(local.windows.len(), remote.windows.len());
    for (i, (a, b)) in local.windows.iter().zip(&remote.windows).enumerate() {
        assert_eq!(a.test_range, b.test_range, "window[{i}] test_range");
        assert_eq!(
            a.out_of_sample_return.to_bits(),
            b.out_of_sample_return.to_bits(),
            "window[{i}] out_of_sample_return"
        );
    }
    assert!(!local.out_of_sample_equity_curve.is_empty(), "the stitched OOS curve is non-empty");
    assert_curves_bit_identical(
        &local.out_of_sample_equity_curve,
        &remote.out_of_sample_equity_curve,
    );
    assert_eq!(
        local.out_of_sample_return.to_bits(),
        remote.out_of_sample_return.to_bits(),
        "out_of_sample_return"
    );
    assert_eq!(
        local.out_of_sample_sharpe.to_bits(),
        remote.out_of_sample_sharpe.to_bits(),
        "out_of_sample_sharpe"
    );
    assert_eq!(
        local.walkforward_consistency.to_bits(),
        remote.walkforward_consistency.to_bits(),
        "walkforward_consistency"
    );
}

/// A perpetual with stored funding: a REMOTE single run and a REMOTE walk-forward report the
/// `funding_paid` the same runs report in process, so the remote Studio shows what a local run paid.
/// bybit BTCUSDT.P, 48 hourly bars, `buy_hold` long 1.0, settlements inside both 2-split windows.
#[test]
fn a_remote_run_and_walk_forward_pay_the_funding_a_local_one_pays() {
    const HOUR: i64 = 3_600_000;
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars: Vec<Bar> =
        (0..48).map(|i| flat_bar_zero_volume(i * HOUR, 100.0 + i as f64)).collect();
    store.append_bars("bybit", "BTCUSDT.P", "1h", &bars, None).unwrap();
    let rows: Vec<Bar> = [20, 28, 36, 44]
        .map(|h| Bar { funding: Some(1e-4), ..flat_bar_zero_volume(h * HOUR, 0.0) })
        .to_vec();
    store.append_bars("bybit", "BTCUSDT.P", "funding", &rows, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let params_toml = "size = 1.0\nsymbol = \"BTCUSDT.P\"\n";
    let local_spec = StrategySpec::file_from_toml_str("trend/buy_hold.rs", params_toml).unwrap();
    let local_slice = DataSlice::bars("bybit", "BTCUSDT.P", "1h", TimestampRange::all());
    let local_run = run_slice(
        &local_spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &local_slice,
        &store,
        None,
    )
    .unwrap()
    .funding_paid;
    let local_walk = run_walkforward_slice_with_params(
        &local_spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &local_slice,
        &store,
        2,
        None,
    )
    .unwrap()
    .funding_paid;
    assert!(local_run < 0.0 && local_walk < 0.0, "a long pays: {local_run}, {local_walk}");

    let spec = StrategySpecWire::File {
        file: "trend/buy_hold.rs".to_string(),
        params_toml: params_toml.to_string(),
    };
    let slice = DataSliceWire {
        venue: "bybit".to_string(),
        symbols: vec!["BTCUSDT.P".to_string()],
        interval: "1h".to_string(),
        start: None,
        end: None,
        kind: ReplayKindWire::Bars,
    };
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote_run = client.run_slice(spec.clone(), slice.clone(), None).expect("remote run");
    let remote_walk = client
        .run_walkforward(spec, slice, WalkforwardWire::fixed(2), None)
        .expect("remote walkforward");
    assert_eq!(remote_run.funding_paid, local_run, "the remote single run");
    assert_eq!(remote_walk.funding_paid, local_walk, "the remote walk-forward");
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
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let walkforward = WalkforwardWire::searching(
        4,
        WalkforwardWindowSearchWire {
            method: "paramscan".to_string(),
            grid: ParamscanWire {
                axes: vec![(
                    "fast".to_string(),
                    vec![ParamValue::Float(3.0), ParamValue::Float(5.0), ParamValue::Float(8.0)],
                )],
            },
            rank_by: Some("equity".to_string()),
        },
    );

    let local = run_walkforward_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &walkforward,
        None,
        Arc::clone(&store),
    )
    .expect("local searching walkforward");
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client
        .run_walkforward(spec.clone(), slice.clone(), walkforward.clone(), None)
        .expect("remote searching walkforward");

    assert_eq!(local.windows.len(), 4, "n_splits=4 stitches 4 OOS windows");
    for (i, (a, b)) in local.windows.iter().zip(&remote.windows).enumerate() {
        assert_eq!(a.test_range, b.test_range, "window[{i}] test_range");
        assert_eq!(
            a.out_of_sample_return.to_bits(),
            b.out_of_sample_return.to_bits(),
            "window[{i}] out_of_sample_return"
        );
        // THE SEARCH HAPPENED: each window carries the axis value it picked on its train half,
        // as the kind its axis carried, and the two routes picked the same one.
        let chosen = a.chosen_params.as_ref().unwrap_or_else(|| {
            panic!("window[{i}] recorded no choice — a searching walk that chose nothing is the fixed walk")
        });
        assert_eq!(chosen.len(), 1, "one swept axis, one recorded choice");
        assert_eq!(chosen[0].0, "fast");
        assert_eq!(Some(chosen), b.chosen_params.as_ref(), "window[{i}] choice differs remote");
    }
    assert_curves_bit_identical(
        &local.out_of_sample_equity_curve,
        &remote.out_of_sample_equity_curve,
    );
    assert_eq!(
        local.out_of_sample_sharpe.to_bits(),
        remote.out_of_sample_sharpe.to_bits(),
        "out_of_sample_sharpe"
    );

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
    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());

    let control = run_walkforward_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
        &WalkforwardWire::fixed(4),
        None,
        Arc::clone(&store),
    )
    .expect("the control walks the script's own fast = 5");
    let searched = run_walkforward_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
        &WalkforwardWire::searching(
            4,
            WalkforwardWindowSearchWire {
                method: "paramscan".to_string(),
                grid: ParamscanWire {
                    axes: vec![("fast".to_string(), vec![ParamValue::Float(3.0)])],
                },
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
            Some(vec![("fast".to_string(), ParamValue::Float(3.0))]),
            "window[{i}] must record the only candidate there was"
        );
    }
    assert_ne!(
        searched.out_of_sample_equity_curve, control.out_of_sample_equity_curve,
        "a walk that CHOSE fast = 3 and then ran fast = 5 would be a report about a run that never \
         happened — the recorded choice has to reach the validation engine"
    );
}

/// The FIXED walk over the same fixture records NO choice and names NO metric — the control, and
/// the contrast that makes the test above mean something.
#[test]
fn the_fixed_walk_records_no_choice_and_names_no_rank_metric() {
    let (_dir, store) = seeded_bar_store();
    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let local = run_walkforward_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
        &WalkforwardWire::fixed(4),
        None,
        store,
    )
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
/// `crates/vike-backtesthub-client/tests/walkforward_search_negotiation.rs`, and neither is reachable
/// by the other's route.
#[test]
fn the_server_refuses_an_unknown_search_method_rather_than_walking_fixed() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let walkforward = WalkforwardWire::searching(
        4,
        WalkforwardWindowSearchWire {
            method: "bayesian".to_string(),
            grid: ParamscanWire {
                axes: vec![(
                    "fast".to_string(),
                    vec![ParamValue::Float(3.0), ParamValue::Float(5.0)],
                )],
            },
            rank_by: None,
        },
    );
    let err = client
        .run_walkforward(
            StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string()),
            btcusdt_1m_bars(),
            walkforward,
            None,
        )
        .expect_err("an unknown method must not produce a WalkforwardResult");
    assert!(err.contains("bayesian"), "the refusal quotes what was sent: {err}");
    assert!(err.contains("none | paramscan"), "and names the accepted set: {err}");
    // The connection survived the refusal, like every other run error on this verb.
    client.ping().expect("connection survives a refused search");
}

/// A `sweep` method with an EMPTY grid is refused rather than quietly becoming the fixed walk: an
/// empty grid expands to ONE point carrying no overrides, so the report would record a "choice"
/// that chose nothing while claiming to have optimized. The refusal is the profile's own.
#[test]
fn the_server_refuses_a_sweep_with_no_axes() {
    let (_dir, store) = seeded_bar_store();
    let walkforward = WalkforwardWire::searching(
        4,
        WalkforwardWindowSearchWire {
            method: "paramscan".to_string(),
            grid: ParamscanWire::default(),
            rank_by: None,
        },
    );
    let err = run_walkforward_local(
        &StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string()),
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
        &walkforward,
        None,
        store,
    )
    .expect_err("an empty grid must be refused");
    assert!(
        err.wire_line().contains("[paramscan] table"),
        "the refusal says what is missing: {err:?}"
    );
}

/// **The cost model is DERIVED, and the derivation reaches the engine** — a run with no `fee_rate`
/// on the wire pays the lane's real fees, where every Studio run before `0063` paid nothing.
///
/// Asserted as a RELATION against a zero-fee override rather than against a copied rate:
/// `vike_venues::fees` is the one authority for what binance charges, and pinning a number here
/// would be a second one.
#[test]
fn a_derived_schedule_actually_charges_where_an_explicit_zero_does_not() {
    let (_dir, store) = seeded_bar_store();
    let spec = StrategySpecWire::Rhai(CROSS_SCRIPT.to_string());

    let derived = run_slice_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
        None,
        Arc::clone(&store),
    )
    .expect("derived-cost run");
    let free = run_slice_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
        Some(&StrategyEngineConfigWire {
            fee_rate: Some(0.0),
            ..StrategyEngineConfigWire::default()
        }),
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
    let sweep = ParamscanWire {
        axes: vec![("fast".to_string(), vec![ParamValue::Float(3.0), ParamValue::Float(5.0)])],
    };
    let result = run_paramscan_local(
        &StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string()),
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &btcusdt_1m_bars(),
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
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let sweep = ParamscanWire {
        axes: vec![(
            "fast".to_string(),
            vec![ParamValue::Float(3.0), ParamValue::Float(5.0), ParamValue::Float(8.0)],
        )],
    };

    let local = run_paramscan_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &sweep,
        None,
        Arc::clone(&store),
    )
    .expect("local sweep");
    let mut client = BacktesthubClient::connect(addr).expect("connect");
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
/// operation. Because `StudioParamscanWire::pbo` is `Option<f64>`, that `NaN` crosses the wire as `None`
/// and `run_paramscan` returns `Ok` — a plain `f64` field would serialize the `NaN` to `null` and then
/// FAIL to decode, turning this ordinary sweep into a client-side decode error. This test is the one
/// that fails if `pbo` were a plain `f64`.
#[test]
fn sweep_single_point_grid_pbo_is_none_not_a_decode_error() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(store, vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let sweep = ParamscanWire { axes: vec![("fast".to_string(), vec![ParamValue::Float(5.0)])] };

    let remote = client
        .run_paramscan(spec, btcusdt_1m_bars(), sweep, None)
        .expect("a single-point sweep must decode (not a `null`-into-f64 decode error)");
    assert_eq!(remote.entries.len(), 1, "one entry for a one-value grid");
    assert_eq!(remote.pbo, None, "single trial -> PBO not assessable, arrives as None");
}

/// A bad Rhai script is refused before any sweep point runs, as a `compile`-class error. It comes
/// back as a `Response::Error` carrying that class, and the connection SURVIVES — the same client
/// still answers a `Ping`.
#[test]
fn sweep_bad_script_returns_error_and_connection_survives() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(store, vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let sweep = ParamscanWire {
        axes: vec![("fast".to_string(), vec![ParamValue::Float(3.0), ParamValue::Float(5.0)])],
    };
    let err = client
        .run_paramscan(
            StrategySpecWire::Rhai("fn on_bar( {".to_string()),
            btcusdt_1m_bars(),
            sweep,
            None,
        )
        .expect_err("a bad script must not produce a SweepResult");
    assert!(err.contains("compile"), "the error carries the compile class: {err}");

    // The connection survived the run error — the SAME client still pings.
    client.ping().expect("connection survives a RunSweep error");
}

/// The v6 gap-closer: a `RunSweep` carrying a custom `cash` HONORS it — every grid point's equity
/// curve seeds at that cash, NOT the `StrategyEngineConfig::default()` 10_000. This is the whole point of
/// the change: before v6 the sweep verb dropped `[engine]` costs and every point ran default cash.
/// Also proves local and remote agree bit-for-bit WITH a non-default params override.
#[test]
fn sweep_honors_custom_cash_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server_over(Arc::clone(&store), vike_strategies_check::stand_in);

    let spec = StrategySpecWire::Rhai(SWEEP_SCRIPT.to_string());
    let slice = btcusdt_1m_bars();
    let sweep = ParamscanWire {
        axes: vec![("fast".to_string(), vec![ParamValue::Float(3.0), ParamValue::Float(5.0)])],
    };
    let params =
        StrategyEngineConfigWire { cash: Some(50_000.0), ..StrategyEngineConfigWire::default() };

    // default-cash run (no params) for the contrast: its curves seed at 10_000.
    let default_run = run_paramscan_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &sweep,
        None,
        Arc::clone(&store),
    )
    .expect("default sweep");
    assert_eq!(
        default_run.entries[0].result.equity_curve.first().copied(),
        Some(10_000.0),
        "the default sweep still seeds at StrategyEngineConfig::default().cash"
    );

    // custom-cash run: every point's equity curve now starts at 50_000.
    let local = run_paramscan_local(
        &spec,
        &vike_backtesthub::studio_run::StrategyFiles::stand_in(vike_strategies_check::stand_in),
        &slice,
        &sweep,
        Some(&params),
        Arc::clone(&store),
    )
    .expect("local custom-cash sweep");
    for (i, e) in local.entries.iter().enumerate() {
        assert_eq!(
            e.result.equity_curve.first().copied(),
            Some(50_000.0),
            "entry[{i}] equity curve seeds at the supplied cash, not the default"
        );
    }

    // and the SAME custom-cash run over the wire is bit-identical to local.
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let remote = client
        .run_paramscan(spec.clone(), slice.clone(), sweep.clone(), Some(params))
        .expect("remote custom-cash sweep");
    assert_eq!(local.entries.len(), remote.entries.len());
    for (a, b) in local.entries.iter().zip(&remote.entries) {
        assert_curves_bit_identical(&a.result.equity_curve, &b.result.equity_curve);
    }
}
