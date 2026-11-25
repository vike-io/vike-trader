//! Reconciliation-activation Task 8 LIVE demo smoke (Bybit sibling of
//! `binance_reconcile_smoke.rs` in `vike-binance`): proves the real `ReconcileClientBybit`
//! (`vike_bybit::reconcile`, the client `vike_bybit::adapter::VenueAdapterBybit`'s `start` builds
//! for a live bybit start — `vike_tradehub_core::build_recon_client`'s bybit arm did until
//! docs/decisions/0096 — which reconciles by default since 2026-09-06) actually fetches AND
//! parses against the live Bybit demo REST API — not just the fixture bodies
//! `tests/offline/reconcile_client_parse.rs` exercises offline. Constructed exactly like that factory:
//! same signer (`BybitV5Signer`), same rate-gated `RestTransportJsonUreq`, same demo base URL (the release
//! seed's demo `trade` row, `perp::seed_endpoints`). (⚠ This named `vike-app`'s `build_recon_client`, behind `VIKE_RECONCILE=1`, until
//! 2026-09-28.)
//! Unlike OKX's `ReconcileClient`, Bybit's needs no live-fetched `ct_val`/instrument prefetch — its
//! constructor is just signer + transport + base_url + symbol, so this smoke skips straight to
//! the fetch calls.
//!
//!     cargo test -p vike-bybit --test bybit_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY, double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! the default DEMO account's creds in the store; self-skips with a `tracing::warn!` when absent —
//! see `bybit_perp_smoke.rs`). No order-placing variant here (kept to the binance sibling, per
//! the reconciliation-activation task brief) — this smoke covers the full `ReconcileClient` read
//! surface: `fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
//! `fetch_position_status_reports`, asserting each succeeds and parses into plausible,
//! internally-consistent values. The UNIFIED-account USDT `walletBalance` is asserted `> 0.0`
//! (not just non-negative) — the SAME live demo account `bybit_perp_smoke.rs`'s
//! `bybit_ladder_long_short_flat` already hard-asserts this against.

use vike_bybit::ReconcileClientBybit;
use vike_bybit::execution::rest::seed_endpoints;
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::client::rest_transport;
use vike_execution::recon::ReconcileClient;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::now_ms;
use vike_secrets::credentials::{Credentials, api_credentials};

/// The linear perpetual's USER name (decision 0290): the client reads its kept row's `BTCUSDT` in `linear` and names
/// every row it reports by it (`vike_bybit::history::kept_list_for_tests` holds the row).
const SYMBOL: &str = "BTCUSDT.P";

/// The double-gate every `*_smoke.rs` in this crate uses (see `bybit_perp_smoke.rs`): load the
/// store, resolve the default DEMO account's `API_KEY`/`API_SECRET`, and return `None` (after a
/// `tracing::warn!`) when absent.
fn load_demo_credentials() -> Option<Credentials> {
    let directory = vike_secrets::smoke_directory();
    let account = directory.credentials_for("bybit", &AccountLabel::Default, AccountTier::Demo);
    let creds = api_credentials(&account, "bybit");
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn bybit_reconcile_fetch_smoke() {
    vike_log::test_init();
    vike_bybit::history::kept_list_for_tests();
    let Some(creds) = load_demo_credentials() else { return };

    let client = ReconcileClientBybit::new(
        BybitV5Signer::new(&creds, now_ms),
        rest_transport().with_rate_gate(vike_bybit::transport::ratelimit::rest_rate_gate()),
        seed_endpoints(AccountTier::Demo).rest,
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
    // isn't a pinned contract in `reconcile/mod.rs` — assert only that the fetch succeeds and
    // parses, without over-claiming a specific row count.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    tracing::info!(target: "vike_bybit", "positions: {}", positions.len());
    for p in &positions {
        assert_eq!(p.venue, "bybit");
        assert_eq!(p.symbol, SYMBOL);
    }

    tracing::info!(target: "vike_bybit", "bybit reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}
