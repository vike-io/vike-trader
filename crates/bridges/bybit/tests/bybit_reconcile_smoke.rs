//! Reconciliation-activation Task 8 LIVE demo smoke (Bybit sibling of
//! `binance_reconcile_smoke.rs` in `vike-binance`): proves the real `BybitReconClient`
//! (`vike_bybit::recon_client`, the client `vike-app`'s `build_recon_client` wires up behind
//! `VIKE_RECONCILE=1` for the bybit arm) actually fetches AND parses against the live Bybit
//! demo REST API — not just the fixture bodies `tests/offline/recon_client_parse.rs` exercises offline.
//! Constructed exactly like `vike-app::build_recon_client`'s bybit arm: same signer
//! (`BybitV5Signer`), same rate-gated `UreqBybitTransport`, same `perp::DEMO_REST` base URL.
//! Unlike OKX's `ReconClient`, Bybit's needs no live-fetched `ct_val`/instrument prefetch — its
//! constructor is just signer + transport + base_url + symbol, so this smoke skips straight to
//! the fetch calls.
//!
//!     cargo test -p vike-bybit --test bybit_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY, double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! `BYBIT_DEMO_*` creds in the workspace `.env`; self-skips with a `tracing::warn!` when absent —
//! see `bybit_perp_smoke.rs`). No order-placing variant here (kept to the binance sibling, per
//! the reconciliation-activation task brief) — this smoke covers the full `ReconClient` read
//! surface: `fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
//! `fetch_position_status_reports`, asserting each succeeds and parses into plausible,
//! internally-consistent values. The UNIFIED-account USDT `walletBalance` is asserted `> 0.0`
//! (not just non-negative) — the SAME live demo account `bybit_perp_smoke.rs`'s
//! `bybit_ladder_long_short_flat` already hard-asserts this against.

use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::BybitV5Signer;
use vike_bybit::BybitReconClient;
use vike_bybit::perp::DEMO_REST;
use vike_bybit::transport::UreqBybitTransport;
use vike_exec::recon::ReconClient;
use vike_model::clock::now_ms;

const SYMBOL: &str = "BTCUSDT";

/// The double-gate every `*_smoke.rs` in this crate uses (see `bybit_perp_smoke.rs`): load the
/// workspace `.env`, look up `BYBIT_DEMO_API_KEY`/`_API_SECRET`, and return `None` (after a
/// `tracing::warn!`) when absent.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("bybit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    let client = BybitReconClient::new(
        BybitV5Signer::new(&creds, now_ms),
        UreqBybitTransport::new().with_rate_gate(vike_bybit::ratelimit::rest_rate_gate()),
        DEMO_REST,
        SYMBOL,
    );

    // the UNIFIED account's USDT wallet balance — mirrors `bybit_perp_smoke.rs`'s established
    // `> 0.0` assertion against this same live demo account/endpoint.
    let balance = client.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_bybit", "UNIFIED USDT wallet balance: {balance:?}");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b > 0.0),
        "UNIFIED USDT wallet balance must be live and funded on the demo account: {balance:?}"
    );

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    tracing::info!(target: "vike_bybit", "open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "bybit");
        assert_eq!(o.symbol, SYMBOL);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    tracing::info!(target: "vike_bybit", "recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "bybit");
        assert_eq!(f.symbol, SYMBOL);
    }

    // Unlike binance perp / OKX, Bybit's `/v5/position/list` non-emptiness for a flat symbol
    // isn't a pinned contract in `recon_client.rs` — assert only that the fetch succeeds and
    // parses, without over-claiming a specific row count.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    tracing::info!(target: "vike_bybit", "positions: {}", positions.len());
    for p in &positions {
        assert_eq!(p.venue, "bybit");
        assert_eq!(p.symbol, SYMBOL);
    }

    tracing::info!(target: "vike_bybit", "bybit reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}
