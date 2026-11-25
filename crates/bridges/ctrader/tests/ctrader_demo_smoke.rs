//! LIVE demo smoke (network + REAL cTrader demo endpoint + creds — run manually):
//!     cargo test -p vike-ctrader --test integration -- --ignored --nocapture ctrader_demo_smoke::
//!
//! Connects to the REAL cTrader demo endpoint (`demo.ctraderapi.com:5035`, the full two-stage
//! OAuth handshake + symbol discovery), submits a tiny EURUSD BUY market order at the venue's
//! minimum volume (1000 units == 100_000 centi-units == 0.01 lot; see `symbols::VolumeGrid`'s
//! doc, live-verified 2026-07-14), awaits the `OrderFilled` on the ingest lane, then FLATTENS via
//! the close-by-position-id path (a `reduce_only` SELL that `ExecutionClientCtrader` routes to
//! `ProtoOAClosePositionReq` against the tracked open position) and awaits ITS fill.
//!
//! The demo account is in HEDGING mode, where the OLD flatten (a plain opposite `ProtoOANewOrderReq`)
//! OPENS an opposing hedged position instead of netting flat — so the real proof is a
//! MODE-AGNOSTIC net-exposure check: after the flatten, a dedicated `ReconcileClientCtrader` fetches the
//! venue's EURUSD positions and the test asserts their net signed quantity is ≈ 0 (truly flat at the
//! venue), which the old opposite-order flatten could NOT achieve on a hedging account.
//!
//! Credentials are the DEFAULT demo account's — its `ACCESS_TOKEN`/`REFRESH_TOKEN`/optional
//! `ACCOUNT_ID` and the venue's `CLIENT_ID`/`CLIENT_SECRET` (see `config::CtraderConfig`) — resolved
//! through `vike_secrets::smoke_directory` + `CtraderConfig::from_credentials`. Absent creds -> the
//! test SKIPS (the live gate), never fails CI (it is also `#[ignore]`d, so it never even runs there).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::ReconcileClientCtrader;
use vike_ctrader::config::CtraderConfig;
use vike_ctrader::execution::ExecutionClientCtrader;
use vike_ctrader::transport::connection::connect_and_auth_execution;
use vike_execution::recon::ReconcileClient;
use vike_execution::{ExecutionClient, Ingest, event_channel};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{OrderRequest, now_ms};

use vike_ctrader::test_support::NoopSink;

/// The venue's minimum EURUSD order volume in UNITS (100_000 centi-units == 0.01 lot;
/// `symbols::VolumeGrid`'s doc comment records the live-verified grid).
const MIN_EURUSD_UNITS: f64 = 1000.0;

fn market_order(order_client_id: &str, side: i32) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side,
        qty: MIN_EURUSD_UNITS,
        order_type: vike_model::OrderType::Market,
        timestamp: now_ms(),
        ..Default::default()
    }
}

/// A `reduce_only` flatten order — `ExecutionClientCtrader::submit` routes it to `ProtoOAClosePositionReq`
/// against the tracked open position rather than opening an opposing (hedged) one.
fn flatten_order(order_client_id: &str, side: i32) -> OrderRequest {
    OrderRequest { reduce_only: true, ..market_order(order_client_id, side) }
}

/// Block until `Event::OrderFilled` for `order_client_id` arrives on the ingest lane, or `total` elapses
/// (panics on timeout — a demo market order that never fills is itself a failure worth seeing).
fn wait_for_fill(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    order_client_id: &str,
    total: Duration,
) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ingest = tokio::time::timeout(remaining, rx.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for OrderFilled({order_client_id})"))
                .expect("ingest channel closed");
            if let Ingest::Event(Event::OrderFilled(f)) = ingest
                && f.order_client_id == order_client_id
            {
                return;
            }
        }
    });
}

/// The venue's net signed EURUSD position quantity (long +, short −), summed over every position
/// report a dedicated `ReconcileClientCtrader` returns — the mode-agnostic "am I flat?" measure.
fn net_eurusd_quantity(config: &CtraderConfig) -> f64 {
    let recon = ReconcileClientCtrader::connect(&config.to_connection_config(), "EURUSD")
        .expect("open a reconcile connection to fetch positions");
    let reports = recon.fetch_position_status_reports().expect("fetch positions");
    reports.iter().map(|p| p.qty).sum()
}

#[test]
#[ignore = "network + REAL cTrader demo endpoint + a demo account's creds — run manually (see module doc)"]
fn ctrader_demo_market_buy_then_flatten() {
    vike_log::test_init();
    // Tests own the store I/O (`from_env` was deleted with the settings-registry conversion):
    // resolve the account here, then the same pure `from_credentials` gate as production.
    let directory = vike_secrets::smoke_directory();
    let credentials =
        directory.credentials_for("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let endpoints = vike_ctrader::test_support::demo_endpoints();
    let Some(config) = CtraderConfig::from_credentials(&endpoints, &credentials) else {
        tracing::warn!(target: "vike_ctrader::smoke", "SKIP: demo account creds absent");
        return;
    };

    let (events, mut rx) = event_channel(64);
    let handle = connect_and_auth_execution(
        config.to_connection_config(),
        Arc::new(NoopSink),
        events.clone(),
    )
    .expect("connect + auth against the real cTrader demo endpoint");
    // Decision 0290: an order names a USER name, read through this account's own listing as the datahub keeps it.
    vike_ctrader::test_support::keep_the_accounts_listing(&handle.symbols);
    let mut exec = ExecutionClientCtrader::new(handle, events);

    let buy_order_client_id = format!("vtrctradersmoke{}", now_ms() % 100_000_000);
    exec.submit(&market_order(&buy_order_client_id, 1));
    wait_for_fill(&mut rx, &buy_order_client_id, Duration::from_secs(30));
    tracing::info!(target: "vike_ctrader::smoke", "buy {buy_order_client_id} filled");

    // Flatten via close-by-position-id (routes to ProtoOAClosePositionReq, not an opposite order).
    // It goes out just after the buy's last frame, the worst moment for a quiet socket, so `after_ms` is about one round
    // trip (`crates/bridges/ctrader/CLAUDE.md`'s "A quiet exec socket"; it was a second more before that).
    let flat_order_client_id = format!("vtrctradersmoke{}f", now_ms() % 100_000_000);
    let submitted = Instant::now();
    exec.submit(&flatten_order(&flat_order_client_id, -1));
    wait_for_fill(&mut rx, &flat_order_client_id, Duration::from_secs(30));
    tracing::info!(
        target: "vike_ctrader::smoke",
        after_ms = submitted.elapsed().as_millis() as u64,
        "flatten {flat_order_client_id} filled via close-position"
    );

    // MODE-AGNOSTIC FLAT assertion: the venue's net EURUSD exposure must be ≈ 0. On a HEDGING
    // account the old opposite-order flatten would leave a stacked long+short pair (net-zero fills
    // on the wire, but two OPEN positions at the venue); close-by-position-id truly flattens it.
    // Poll briefly — the venue's position report can lag the fill by a moment.
    let mut net = f64::NAN;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        net = net_eurusd_quantity(&config);
        if net.abs() < 1e-6 {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(net.abs() < 1e-6, "expected FLAT net EURUSD exposure after close, got net qty = {net}");
    tracing::info!(target: "vike_ctrader::smoke", "venue net EURUSD exposure flat (qty={net}) — demo ladder green");

    drop(exec); // deterministic teardown (ActorHandle::drop joins the actor thread)
}
