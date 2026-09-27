//! Reconciliation-activation LIVE demo smoke (Deribit sibling of `okx_reconcile_smoke.rs` in
//! `vike-okx` / `binance_reconcile_smoke.rs` in `vike-binance`): proves the real
//! `DeribitReconClient` — the client `vike-app`'s `make_engine` deribit arm now wires up behind
//! `VIKE_RECONCILE=1` — actually AUTHS, FETCHES, and PARSES against the live Deribit TESTNET
//! (demo) JSON-RPC WS, not just the fixture bodies `tests/offline/recon_client_parse.rs` exercises offline.
//!
//! Constructed exactly like `vike-app`'s deribit arm: `DeribitReconClient::connect(&creds, SYMBOL)`
//! — a DEDICATED authed order-WS (its own socket, isolated from any exec order path), on TESTNET.
//!
//!     cargo test -p vike-deribit --test deribit_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY, double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! `DERIBIT_DEMO_*` creds in the workspace `.env`; self-skips with a `tracing::warn!` when absent —
//! see `deribit_smoke.rs`). No order-placing variant. It covers the full `ReconClient` read
//! surface — `fetch_order_status_reports` / `fetch_fill_reports` / `fetch_position_status_reports` /
//! `fetch_balance` — asserting each SUCCEEDS and parses into plausible, internally-consistent
//! values against the live venue.

use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_deribit::DeribitReconClient;
use vike_exec::recon::ReconClient;

const SYMBOL: &str = "BTC-PERPETUAL";

/// The double-gate every `*_smoke.rs` in this crate uses (see `deribit_smoke.rs`): load the
/// workspace `.env`, look up `DERIBIT_DEMO_API_KEY`/`_API_SECRET`, and return `None` (after a
/// `tracing::warn!`) when absent.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("deribit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_deribit", "SKIP: DERIBIT_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn deribit_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    // Dedicated-socket recon client: opens + auths its OWN testnet order-WS (blocking).
    let Some(client) = DeribitReconClient::connect(&creds, SYMBOL) else {
        panic!("DeribitReconClient::connect failed — demo order-WS auth did not succeed");
    };

    // 1) open orders — Ok even when flat (empty slice); every returned row parses.
    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    for o in &orders {
        assert_eq!(o.venue, "deribit");
        assert!(o.qty >= 0.0, "order qty is non-negative: {o:?}");
        assert!(o.side == 1 || o.side == -1, "order side is ±1: {o:?}");
    }
    tracing::info!(target: "vike_deribit", n = orders.len(), "open orders fetched + parsed");

    // 2) recent fills within a 24h lookback — Ok even when none; every row parses.
    let since = vike_model::clock::now_ms() - 24 * 60 * 60 * 1_000;
    let fills = client.fetch_fill_reports(since).expect("fetch_fill_reports");
    for f in &fills {
        assert_eq!(f.venue, "deribit");
        assert!(f.last_qty > 0.0, "fill qty is positive: {f:?}");
        assert!(f.last_px > 0.0, "fill price is positive: {f:?}");
        assert!(f.side == 1 || f.side == -1, "fill side is ±1: {f:?}");
    }
    tracing::info!(target: "vike_deribit", n = fills.len(), "fills fetched + parsed");

    // 3) option positions for the currency — Ok even when flat.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    for p in &positions {
        assert_eq!(p.venue, "deribit");
    }
    tracing::info!(target: "vike_deribit", n = positions.len(), "positions fetched + parsed");

    // 4) account balance — the demo account always reports a summary; some non-negative equity.
    let balance = client.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_deribit", ?balance, "balance fetched");
    if let Some(b) = balance {
        assert!(b.is_finite(), "balance is a finite number: {b}");
    }
}
