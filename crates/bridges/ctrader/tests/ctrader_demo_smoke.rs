//! LIVE demo smoke (network + REAL cTrader demo endpoint + creds — run manually):
//!     cargo test -p vike-ctrader --test ctrader_demo_smoke -- --ignored --nocapture
//!
//! Connects to the REAL cTrader demo endpoint (`demo.ctraderapi.com:5035`, the full two-stage
//! OAuth handshake + symbol discovery), submits a tiny EURUSD BUY market order at the venue's
//! minimum volume (1000 units == 100_000 centi-units == 0.01 lot; see `symbols::VolumeGrid`'s
//! doc, live-verified 2026-07-14), awaits the `OrderFilled` on the ingest lane, then FLATTENS via
//! the close-by-position-id path (a `reduce_only` SELL that `CtraderExec` routes to
//! `ProtoOAClosePositionReq` against the tracked open position) and awaits ITS fill.
//!
//! The demo account is in HEDGING mode, where the OLD flatten (a plain opposite `ProtoOANewOrderReq`)
//! OPENS an opposing hedged position instead of netting flat — so the real proof is a
//! MODE-AGNOSTIC net-exposure check: after the flatten, a dedicated `CtraderReconClient` fetches the
//! venue's EURUSD positions and the test asserts their net signed quantity is ≈ 0 (truly flat at the
//! venue), which the old opposite-order flatten could NOT achieve on a hedging account.
//!
//! Credentials come from the workspace's gitignored `.env` (`CTRADER_CLIENT_ID`/`_SECRET` +
//! `CTRADER_DEMO_ACCESS_TOKEN`/`_REFRESH_TOKEN`/optional `_ACCOUNT_ID` — see
//! `config::CtraderConfig`), loaded via `load_workspace_dotenv_from` + `CtraderConfig::from_vars`. Absent creds -> the test SKIPS
//! (the live gate), never fails CI (it is also `#[ignore]`d, so it never even runs there).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::Environment;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::conn::connect_and_auth_exec;
use vike_ctrader::exec::CtraderExec;
use vike_ctrader::CtraderReconClient;
use vike_exec::lanes::Ingest;
use vike_exec::recon::ReconClient;
use vike_exec::{event_channel, ExecutionClient};
use vike_model::events::Event;
use vike_model::{now_ms, OrderRequest};

use common::NoopSink;

/// The venue's minimum EURUSD order volume in UNITS (100_000 centi-units == 0.01 lot;
/// `symbols::VolumeGrid`'s doc comment records the live-verified grid).
const MIN_EURUSD_UNITS: f64 = 1000.0;

fn market_order(coid: &str, side: i32) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty: MIN_EURUSD_UNITS,
        order_type: "market".into(),
        ts: now_ms(),
        ..Default::default()
    }
}

/// A `reduce_only` flatten order — `CtraderExec::submit` routes it to `ProtoOAClosePositionReq`
/// against the tracked open position rather than opening an opposing (hedged) one.
fn flatten_order(coid: &str, side: i32) -> OrderRequest {
    OrderRequest { reduce_only: true, ..market_order(coid, side) }
}

/// Block until `Event::OrderFilled` for `coid` arrives on the ingest lane, or `total` elapses
/// (panics on timeout — a demo market order that never fills is itself a failure worth seeing).
fn wait_for_fill(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, coid: &str, total: Duration) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ingest = tokio::time::timeout(remaining, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for OrderFilled({coid})"))
                .expect("ingest channel closed");
            if let Ingest::Event(Event::OrderFilled(f)) = ingest {
                if f.client_order_id == coid {
                    return;
                }
            }
        }
    });
}

/// The venue's net signed EURUSD position quantity (long +, short −), summed over every position
/// report a dedicated `CtraderReconClient` returns — the mode-agnostic "am I flat?" measure.
fn net_eurusd_qty(config: &CtraderConfig) -> f64 {
    let recon = CtraderReconClient::connect(&config.to_conn_config(), "EURUSD")
        .expect("open a reconcile connection to fetch positions");
    let reports = recon.fetch_position_status_reports().expect("fetch positions");
    reports.iter().map(|p| p.qty).sum()
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + CTRADER_DEMO creds — run manually (see module doc)"]
fn ctrader_demo_market_buy_then_flatten() {
    vike_log::test_init();
    // Tests own the `.env` I/O (`from_env` was deleted with the settings-registry conversion):
    // load the workspace map here, then the same pure `from_vars` gate as production.
    let vars = vike_bridge_core::credentials::load_workspace_dotenv_from(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
    );
    let Some(config) = CtraderConfig::from_vars(Environment::Demo, &vars) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: CTRADER_DEMO creds absent");
        return;
    };

    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_exec(config.to_conn_config(), Arc::new(NoopSink), events.clone())
        .expect("connect + auth against the real cTrader demo endpoint");
    let mut exec = CtraderExec::new(handle, events);

    let buy_coid = format!("vtrctradersmoke{}", now_ms() % 100_000_000);
    exec.submit(&market_order(&buy_coid, 1));
    wait_for_fill(&mut rx, &buy_coid, Duration::from_secs(30));
    tracing::info!(target: "vike_ctrader::smoke", "buy {buy_coid} filled");

    // Flatten via close-by-position-id (routes to ProtoOAClosePositionReq, not an opposite order).
    let flat_coid = format!("vtrctradersmoke{}f", now_ms() % 100_000_000);
    exec.submit(&flatten_order(&flat_coid, -1));
    wait_for_fill(&mut rx, &flat_coid, Duration::from_secs(30));
    tracing::info!(target: "vike_ctrader::smoke", "flatten {flat_coid} filled via close-position");

    // MODE-AGNOSTIC FLAT assertion: the venue's net EURUSD exposure must be ≈ 0. On a HEDGING
    // account the old opposite-order flatten would leave a stacked long+short pair (net-zero fills
    // on the wire, but two OPEN positions at the venue); close-by-position-id truly flattens it.
    // Poll briefly — the venue's position report can lag the fill by a moment.
    let mut net = f64::NAN;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        net = net_eurusd_qty(&config);
        if net.abs() < 1e-6 {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(net.abs() < 1e-6, "expected FLAT net EURUSD exposure after close, got net qty = {net}");
    tracing::info!(target: "vike_ctrader::smoke", "venue net EURUSD exposure flat (qty={net}) — demo ladder green");

    drop(exec); // deterministic teardown (ActorHandle::drop joins the actor thread)
}
