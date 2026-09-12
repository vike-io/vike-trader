//! Parity gate for the v7 PROFILE-shaped `RunSweepProfile` / `RunWalkforwardProfile` verbs: a run
//! executed LOCALLY (`vike_backtest::harness::run_sweep` / `harness::run_walkforward` over the same
//! store) and the SAME run executed OVER THE WIRE must produce a BYTE-IDENTICAL report JSON — the
//! profile-shaped sibling of the Studio-verb gate that used to sit beside it in `vike-datahub`.
//!
//! It also pins the CAPABILITY these verbs exist for: because the whole profile TOML crosses the
//! wire and the SERVER parses it with the one `BacktestProfile::from_toml_str`, sections the old
//! client-side `WireSlice`/`WireEngineParams` mapping could not carry — here `[risk]` — reach the
//! engine and change the answer. Under the DTO-shaped verbs that section was silently dropped.
//!
//! ⚠ It MOVED here from `vike-datahub` under ruling 7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`, with the verbs it gates:
//! `RunSweepProfile`/`RunWalkforwardProfile` are served by `vike-backend backtest --addr` now, so a
//! parity gate over `vike_datahub::serve` would be comparing the local run against a refusal.
//!
//! Both verbs are served on EVERY build of this crate that compiles the server (the harness is
//! DataFusion-free — proven by
//! `tests/compute_plane.rs`'s lean-build tests). This file is behind `datafusion-store` only
//! because it needs
//! a store that can actually HOLD bars: `MemHistStore`'s `append_bars`/`load_bars` are inert stubs,
//! so a meaningful run needs the concrete `DataFusionHist` over a temp dir.
#![cfg(feature = "datafusion-store")]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_backtest::compute_server::serve;
use vike_backtest::harness::{self, BacktestProfile, RankMetric};
use vike_data::{DataFusionHist, HistStore};
use vike_datahub_client::DatahubClient;
use vike_model::Bar;

const CASH: f64 = 1000.0;

/// The profile every test ships: `engine_extra` adds `[engine]` keys, `extra` appends whole
/// sections (a `[sweep]` grid, a `[walkforward]` table, a `[risk]` section…). Bar mode over the
/// seeded fixture below.
///
/// `fee_rate` is left OFF by default deliberately: on this fixture (open == close per bar) a
/// zero-cost fill leaves equity at exactly `cash` on the entry bar, which is the contract
/// `walk_forward_strategy` debug-asserts for every window's first sample.
fn profile_toml(engine_extra: &str, extra: &str) -> String {
    format!(
        r#"
name = "profile_verb_parity"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1m"
from = "0"
to = "100000000"

[engine]
cash = {CASH}
{engine_extra}

[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
{extra}
"#
    )
}

/// A `DataFusionHist` over a temp dir seeded with 400 deterministic oscillating BTCUSDT bars (the
/// `run_sweep_walkforward_roundtrip` fixture). The `TempDir` is returned so the caller keeps it
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

/// LOCAL == REMOTE for the sweep verb: the same profile ranked by the same metric produces the
/// identical `SweepReport` JSON whether run in-process or shipped as TOML to the server. The server
/// does the ranking, so a client never re-implements sharpe/return/max_dd.
#[test]
fn profile_sweep_is_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let toml = profile_toml("", "\n[sweep]\nsize = [1.0, 2.0, 3.0]\n");
    let profile = BacktestProfile::from_toml_str(&toml).expect("profile parses");
    let local = harness::run_sweep(&profile, Arc::clone(&store), RankMetric::Sharpe)
        .expect("local sweep runs");
    let local_json = serde_json::to_string(&local).expect("local report serializes");

    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote_json = client.run_sweep_profile(&toml, Some("sharpe")).expect("remote sweep runs");

    assert_eq!(local_json, remote_json, "local and remote sweep reports must be byte-identical");

    let v: serde_json::Value = serde_json::from_str(&remote_json).unwrap();
    assert_eq!(v["rows"].as_array().map(Vec::len), Some(3), "one row per grid point");
    assert_eq!(v["rank_by"], "sharpe");
    // Rows arrive ALREADY ranked, each carrying its own BacktestReport — what the CLI renders.
    assert!(v["rows"][0]["report"]["final_equity"].is_number(), "rows carry server stats: {v}");
}

/// LOCAL == REMOTE for the walk-forward verb, and the split count (read from the profile's
/// `[walkforward]` table server-side) produces exactly that many OOS windows.
#[test]
fn profile_walkforward_is_byte_identical_local_and_remote() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(Arc::clone(&store));

    let toml = profile_toml("", "\n[walkforward]\nn_splits = 4\n");
    let profile = BacktestProfile::from_toml_str(&toml).expect("profile parses");
    let local = harness::run_walkforward(&profile, Arc::clone(&store)).expect("local wf runs");
    let local_json = serde_json::to_string(&local).expect("local report serializes");

    let mut client = DatahubClient::connect(addr).expect("connect");
    let remote_json = client.run_walkforward_profile(&toml).expect("remote wf runs");

    assert_eq!(local_json, remote_json, "local and remote walk-forward reports must match");

    let v: serde_json::Value = serde_json::from_str(&remote_json).unwrap();
    assert_eq!(v["windows"].as_array().map(Vec::len), Some(4), "one window per split");
    assert!(v["oos_equity_curve"].as_array().is_some_and(|c| !c.is_empty()));
}

/// THE CAPABILITY THESE VERBS EXIST FOR: a `[risk]` section — which the DTO-shaped `RunSweep` wire
/// verb cannot carry at all — crosses with the profile and reaches the engine.
///
/// The signal is the one `harness_risk_wiring.rs` established: with `fee_rate > 0` a FILLED order
/// strictly moves equity off `cash` (the fee is debited immediately), while a DENIED order leaves it
/// at EXACTLY `cash` — no fill, no fee, no position to mark. So with a `max_notional_per_order` far
/// below the first order's notional every grid point must land exactly on `cash`, and the same
/// profile without `[risk]` must not. A silently-dropped section would make the two runs identical.
#[test]
fn a_risk_section_crosses_the_wire_and_changes_the_sweep() {
    let (_dir, store) = seeded_bar_store();
    let addr = spawn_server(store);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let grid = "\n[sweep]\nsize = [1.0, 2.0]\n";
    let unrestricted = client
        .run_sweep_profile(&profile_toml("fee_rate = 0.001", grid), None)
        .expect("the unrestricted sweep runs");
    let restricted = client
        .run_sweep_profile(
            &profile_toml(
                "fee_rate = 0.001",
                &format!("{grid}\n[risk]\nmax_notional_per_order = 1.0\n"),
            ),
            None,
        )
        .expect("the risk-gated sweep runs");

    let equities = |json: &str| -> Vec<f64> {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        v["rows"]
            .as_array()
            .expect("rows array")
            .iter()
            .map(|r| {
                r["report"]["final_equity"]
                    .as_f64()
                    .unwrap_or_else(|| panic!("every point must run: {r}"))
            })
            .collect()
    };

    let gated = equities(&restricted);
    assert_eq!(gated.len(), 2, "one row per grid point");
    for e in &gated {
        assert_eq!(*e, CASH, "every risk-denied point must leave equity at cash: {gated:?}");
    }
    let ungated = equities(&unrestricted);
    assert!(
        ungated.iter().all(|e| *e != CASH),
        "without [risk] every point must actually fill (fee debited), or this proves nothing: \
         {ungated:?}"
    );
}
