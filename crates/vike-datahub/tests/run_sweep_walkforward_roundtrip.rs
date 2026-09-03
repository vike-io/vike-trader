//! PR-4 parity gate for the Studio `RunSweep` / `RunWalkforward` verbs: a run executed LOCALLY (via
//! the re-exported `run_sweep_local` / `run_walkforward_local`) and the SAME run executed OVER THE
//! WIRE (`serve` + `DatahubClient::run_sweep` / `run_walkforward`) must produce a BIT-IDENTICAL
//! answer — the sibling of `run_slice_roundtrip.rs`. It also pins the load-bearing serde subtlety: a
//! sweep whose PBO is `NaN` (a single-point grid, NORMAL operation) crosses the wire as `None`, NOT a
//! decode error, because `WireSweepResult::pbo` is `Option<f64>`.
//!
//! The whole file is behind `serve-datafusion` (the feature that turns on `vike-studio-core` +
//! `DataFusionHist` — a `MemHistStore` cannot store bars, so a meaningful run needs the concrete
//! store, seeded over a temp dir exactly as `vike_studio_core::run`'s own tests do). A default
//! `cargo test -p vike-datahub` compiles this file to nothing.
#![cfg(feature = "serve-datafusion")]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_data::{DataFusionHist, HistStore};
use vike_datahub::{run_sweep_local, run_walkforward_local, serve};
use vike_datahub_client::wire_studio::{
    WireEngineParams, WireSlice, WireSliceKind, WireSpec, WireSweep, WireWalkforward,
};
use vike_datahub_client::DatahubClient;
use vike_model::Bar;

/// The SMA(5/20)-cross Rhai strategy from `vike_studio_core::run`'s own tests — it TRADES over the
/// seeded store, so the walk-forward windows stitch a non-trivial OOS curve.
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
/// the axis a `WireSweep` overrides, so each grid point trades differently.
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

/// Bind an ephemeral loopback listener and spawn `serve` over `store` on a detached thread; return
/// the assigned address. The store is MOVED into the serve thread (the caller keeps its own clone
/// for the local run).
fn spawn_server(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve(listener, store);
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

/// The walk-forward parity gate: a multi-split anchored walk-forward produces a bit-identical stitched
/// OOS curve, per-window outcomes, and summary scalars local vs remote.
#[test]
fn walkforward_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(CROSS_SCRIPT.to_string());
    let slice = bar_slice();
    let wf = WireWalkforward { n_splits: 4 };

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

/// The sweep parity gate: a MULTI-point grid produces bit-identical ranked entries (overrides + the
/// rendered result) and identical dsr/pbo/best_index local vs remote.
#[test]
fn sweep_is_bit_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let slice = bar_slice();
    let sweep = WireSweep { axes: vec![("fast".to_string(), vec![3.0, 5.0, 8.0])] };

    let local =
        run_sweep_local(&spec, &slice, &sweep, None, Arc::clone(&store)).expect("local sweep");
    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote =
        client.run_sweep(spec.clone(), slice.clone(), sweep.clone(), None).expect("remote sweep");

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

/// The LOAD-BEARING serde test: a single-point grid leaves `StudioSweep::pbo` as `NaN` in NORMAL
/// operation. Because `WireSweepResult::pbo` is `Option<f64>`, that `NaN` crosses the wire as `None`
/// and `run_sweep` returns `Ok` — a plain `f64` field would serialize the `NaN` to `null` and then
/// FAIL to decode, turning this ordinary sweep into a client-side decode error. This test is the one
/// that fails if `pbo` were a plain `f64`.
#[test]
fn sweep_single_point_grid_pbo_is_none_not_a_decode_error() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(store);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let spec = WireSpec::Rhai(SWEEP_SCRIPT.to_string());
    let sweep = WireSweep { axes: vec![("fast".to_string(), vec![5.0])] };

    let remote = client
        .run_sweep(spec, bar_slice(), sweep, None)
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

    let sweep = WireSweep { axes: vec![("fast".to_string(), vec![3.0, 5.0])] };
    let err = client
        .run_sweep(WireSpec::Rhai("fn on_bar( {".to_string()), bar_slice(), sweep, None)
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
    let sweep = WireSweep { axes: vec![("fast".to_string(), vec![3.0, 5.0])] };
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
        .run_sweep(spec.clone(), slice.clone(), sweep.clone(), Some(params))
        .expect("remote custom-cash sweep");
    assert_eq!(local.entries.len(), remote.entries.len());
    for (a, b) in local.entries.iter().zip(&remote.entries) {
        assert_curves_bit_identical(&a.result.equity_curve, &b.result.equity_curve);
    }
}
