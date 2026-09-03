//! PR-3 parity gate for the Studio `RunSlice` verb: a run executed LOCALLY (via the re-exported
//! `run_slice_local`) and the SAME run executed OVER THE WIRE (`serve` + `DatahubClient::run_slice`)
//! must produce a BIT-IDENTICAL answer. This is what keeps S2 honest — the remote path is not a
//! second implementation, it is the same `vike_studio_core::run::run_slice` reached through the
//! serialized DTOs.
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
use vike_datahub::{run_slice_local, serve};
use vike_datahub_client::wire_studio::{WireEngineParams, WireSlice, WireSliceKind, WireSpec};
use vike_datahub_client::DatahubClient;
use vike_model::{Bar, QuoteTick, TradeTick};

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
