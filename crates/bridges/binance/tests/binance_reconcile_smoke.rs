//! Reconciliation-activation Task 8 LIVE demo smoke: proves the real `BinanceReconClient`
//! (`vike_binance::recon_client`, the client `vike-app`'s `build_recon_client` wires up behind
//! `VIKE_RECONCILE=1` for the binance arm) actually fetches AND parses against the live Binance
//! demo REST API — not just the literal fixture bodies `tests/offline/recon_client_parse.rs` exercises
//! offline (no network). Constructed exactly like `vike-app::build_recon_client`'s binance arm:
//! same signer (`BinanceHmacSigner`), same rate-gated `UreqTransport`, same demo base URLs
//! (`spot::DEMO_REST` / `perp::DEMO_FAPI_REST`).
//!
//!     cargo test -p vike-binance --test binance_reconcile_smoke -- --ignored --nocapture
//!
//! Three scenarios, each its own `#[ignore]`d `#[test]`, double-gated exactly like every other
//! `*_smoke.rs` in this crate (see e.g. `binance_demo_smoke.rs`): network + `BINANCE_DEMO_*`
//! creds in the workspace `.env` (`load_workspace_dotenv_from` + `load_credentials_from`), self-skip
//! (a `tracing::warn!`, then an early `return`) when creds are absent — factored out here as
//! [`load_demo_creds`] since three tests in this one file share the identical gate.
//!
//!   - `binance_spot_reconcile_fetch_smoke` / `binance_perp_reconcile_fetch_smoke` — READ-ONLY,
//!     safe to run any time, never places an order: construct the real `BinanceReconClient` in
//!     both its `Spot` and `Perp` modes and call all four `ReconClient` methods
//!     (`fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
//!     `fetch_position_status_reports`), asserting each succeeds and parses into plausible,
//!     internally-consistent values (right venue/symbol on every row; spot's position report is
//!     always `Ok(vec![])`; perp's `positionRisk` always echoes `BTCUSDT` even flat — see
//!     `recon_client.rs`'s module doc for why both of those are load-bearing contracts, not
//!     incidental).
//!   - `binance_reconcile_order_lifecycle_smoke` — ORDER-PLACING (kept separate, clearly marked
//!     `#[ignore]`d): mirrors `binance_demo_smoke.rs`'s far-from-market LIMIT ladder (a BUY at
//!     half the live mark, sized to clear `min_notional`, that can never fill), but the
//!     verification step is the `ReconClient` report fetch itself, proving the exact seam the
//!     live reconcile engine depends on: place -> the order becomes visible through
//!     `BinanceReconClient::fetch_order_status_reports` with `NEW` normalized to `ACCEPTED` ->
//!     cancel -> the next fetch no longer shows it.

use vike_binance::BinanceReconClient;
use vike_binance::perp::DEMO_FAPI_REST;
use vike_binance::spot::{BinanceSpotRest, DEMO_REST, PATH_EXCHANGE_INFO, parse_symbol_properties};
use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BinanceHmacSigner;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::recon::ReconClient;
use vike_model::clock::now_ms;
use vike_model::events::Event;

const SYMBOL: &str = "BTCUSDT";

/// The double-gate every `*_smoke.rs` in this crate uses (see e.g. `binance_demo_smoke.rs`):
/// load the workspace `.env`, look up `BINANCE_DEMO_API_KEY`/`_API_SECRET`, and return `None`
/// (after a `tracing::warn!`) when absent so each caller can self-skip via
/// `let Some(creds) = load_demo_creds() else { return };`.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("binance", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_binance", "SKIP: BINANCE_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_spot_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let client = BinanceReconClient::spot(signer, transport, DEMO_REST, SYMBOL);

    // Spot testnet funding isn't a documented API contract (unlike perp, below) — accept
    // `None`/`Some`, but a `Some` value must be a real, non-negative number, never garbage.
    let balance = client.fetch_balance().expect("fetch_balance (spot)");
    tracing::info!(target: "vike_binance", "spot USDT free balance: {balance:?}");
    match balance {
        Some(b) => assert!(b.is_finite() && b >= 0.0, "implausible spot balance: {b}"),
        None => {
            tracing::warn!(target: "vike_binance", "spot account has no USDT row (unfunded demo account?)")
        }
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports (spot)");
    tracing::info!(target: "vike_binance", "spot open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "binance");
        assert_eq!(o.symbol, SYMBOL);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports (spot)");
    tracing::info!(target: "vike_binance", "spot recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "binance");
        assert_eq!(f.symbol, SYMBOL);
    }

    // Spot has no venue-native position report (see `recon_client`'s module doc) — always
    // `Ok(vec![])`. Asserting it live pins the trait dispatch, not just the pure parser.
    let positions =
        client.fetch_position_status_reports().expect("fetch_position_status_reports (spot)");
    assert!(positions.is_empty(), "spot ReconClient must report no positions: {positions:?}");

    tracing::info!(target: "vike_binance", "spot reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn binance_perp_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let signer = BinanceHmacSigner::new(&creds, now_ms);
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::perp_rest_gate());
    let client = BinanceReconClient::perp(signer, transport, DEMO_FAPI_REST, SYMBOL);

    // The futures demo wallet is documented/established to auto-fund USDT margin (the SAME
    // account/endpoint `binance_perp_smoke.rs`'s `perp_ladder_long_short_flat` already hard-
    // asserts `balance > 0.0` against) — safe to assert with confidence, unlike spot above.
    let balance = client.fetch_balance().expect("fetch_balance (perp)");
    tracing::info!(target: "vike_binance", "perp USDT wallet balance: {balance:?}");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b > 0.0),
        "perp USDT wallet balance must be live and funded on the futures demo: {balance:?}"
    );

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports (perp)");
    tracing::info!(target: "vike_binance", "perp open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "binance");
        assert_eq!(o.symbol, SYMBOL);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports (perp)");
    tracing::info!(target: "vike_binance", "perp recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "binance");
        assert_eq!(f.symbol, SYMBOL);
    }

    // `positionRisk` always echoes every requested symbol, including a flat leg — see
    // `recon_client.rs`'s module doc for why that flat row is load-bearing for `diff::diff`.
    let positions =
        client.fetch_position_status_reports().expect("fetch_position_status_reports (perp)");
    assert!(!positions.is_empty(), "perp positionRisk must echo {SYMBOL} even when flat");
    assert_eq!(positions[0].symbol, SYMBOL);
    tracing::info!(target: "vike_binance", "perp position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px);

    tracing::info!(target: "vike_binance", "perp reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}

/// ORDER-PLACING (separate from the read-only fetch smokes above): rests a far-from-market LIMIT
/// BUY that can never fill, confirms it becomes visible through the real
/// `BinanceReconClient::fetch_order_status_reports` (not the raw `openOrders` REST call
/// `binance_demo_smoke.rs` checks against — this smoke proves the RECONCILE seam specifically),
/// cancels it, and confirms the next fetch no longer shows it.
#[test]
#[ignore = "network + demo creds — places + cancels a real (non-filling) demo order — run manually (see module doc)"]
fn binance_reconcile_order_lifecycle_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    // submit-side client: exact ladder `binance_demo_smoke.rs` uses to get a live mark + place.
    let transport =
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate());
    let info = transport
        .public(DEMO_REST, PATH_EXCHANGE_INFO, &[("symbol", SYMBOL.into())])
        .expect("exchangeInfo");
    let properties = parse_symbol_properties(&info)[SYMBOL];

    let submit_client = BinanceSpotRest {
        link_id: None,
        signer: BinanceHmacSigner::new(&creds, now_ms),
        transport,
        base_url: DEMO_REST.to_string(),
        symbol: SYMBOL.to_string(),
        properties,
        base_asset: "BTC".to_string(),
    };
    let offset = submit_client.server_time_offset(now_ms()).expect("server time");
    submit_client.signer.set_offset_ms(offset);

    let snap = submit_client.connect().expect("connect/reconcile");
    let mark = snap.position_avg_px[0].1;
    assert!(mark > 0.0, "ticker mark must be live");

    // far-from-market LIMIT BUY that must REST: half the mark, notional >= min (never fills).
    let limit_px = mark * 0.5;
    let qty = (properties.min_notional.max(5.0) * 1.6) / limit_px;
    let coid = format!("vtreconsmk{}", now_ms() % 100_000_000);
    let request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": SYMBOL,
        "side": 1, "qty": qty, "order_type": "limit", "price": limit_px,
        "ts": now_ms()
    }))
    .unwrap();
    let events = submit_client.submit_order(&request);
    tracing::debug!(target: "vike_binance", "submit events: {events:?}");
    assert!(
        matches!(events.last(), Some(Event::OrderAccepted(_))),
        "demo order must be ACCEPTED: {events:?}"
    );

    // a SEPARATE `BinanceReconClient` for verification — its own fresh signer/transport
    // instance, the "fresh REST client per purpose" idiom `vike-app::build_recon_client`'s doc
    // comment pins (submit and reconcile are different purposes, never share one instance).
    let recon_client = BinanceReconClient::spot(
        BinanceHmacSigner::new(&creds, now_ms),
        UreqTransport::new("binance").with_rate_gate(vike_binance::ratelimit::spot_rest_gate()),
        DEMO_REST,
        SYMBOL,
    );

    let orders = recon_client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let Some(resting) = orders.iter().find(|o| o.client_order_id.as_deref() == Some(coid.as_str()))
    else {
        panic!("order {coid} must appear in reconcile reports: {orders:?}");
    };
    assert_eq!(resting.status, "ACCEPTED", "resting NEW order normalizes to ACCEPTED");
    assert_eq!(resting.side, 1, "BUY -> +1");
    assert_eq!(resting.avg_px, 0.0, "unfilled -> zero avg_px, no divide-by-zero");
    tracing::info!(target: "vike_binance", "order {coid} visible via ReconClient: status={}", resting.status);

    submit_client.cancel_order(&coid).expect("cancel");

    let orders = recon_client
        .fetch_order_status_reports(0)
        .expect("fetch_order_status_reports after cancel");
    assert!(
        orders.iter().all(|o| o.client_order_id.as_deref() != Some(coid.as_str())),
        "order {coid} must be gone from reconcile reports after cancel: {orders:?}"
    );
    tracing::info!(target: "vike_binance", "reconcile order-lifecycle smoke green: place -> visible via ReconClient (ACCEPTED) -> cancel -> gone");
}
