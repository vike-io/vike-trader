//! Reconciliation-activation Task 8 LIVE demo smoke (OKX sibling of
//! `binance_reconcile_smoke.rs` in `vike-binance`): proves the real `ReconcileClientOkx`
//! (`vike_okx::reconcile`, the client `vike_okx::adapter::VenueAdapterOkx` builds for a live okx
//! start, which reconciles by default since 2026-09-06) actually fetches AND parses
//! against the live OKX demo (simulated-trading) REST API — not just the fixture bodies
//! `tests/offline/reconcile_client_parse.rs` exercises offline. Constructed exactly like that factory:
//! same signer (`OkxV5Signer`), same rate-gated `rest_transport(true)` (demo /
//! `x-simulated-trading: 1`), the same demo REST base (`vike_okx::test_support::demo_net`, the seed row the
//! factory's `OkxNet` resolves) + a live-fetched `ct_val`. (⚠ This named
//! `vike-app`'s `build_recon_client`, behind `VIKE_RECONCILE=1`, until 2026-09-28, and then
//! `vike_tradehub_core::build_recon_client`'s okx arm until okx started itself through its bridge.)
//!
//!     cargo test -p vike-okx --test okx_smokes -- --ignored --nocapture okx_reconcile_smoke::
//!
//! READ-ONLY, double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! the default DEMO account's creds + passphrase in the store; self-skips with a `tracing::warn!`
//! when absent — see `okx_perp_smoke.rs`). No order-placing variant here (kept to the binance
//! sibling, per the reconciliation-activation task brief) — this smoke covers the full
//! `ReconcileClient` read surface: `fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
//! `fetch_position_status_reports`, asserting each succeeds and parses into plausible,
//! internally-consistent values. `fetch_position_status_reports` is asserted NON-EMPTY: OKX
//! omits a symbol entirely once flat (no `pos: "0"` row), so `parse_positions` synthesizes ONE
//! flat row when the venue sends none — see `reconcile/mod.rs`'s module doc; this smoke pins that
//! synthesis live, not just against the fixture body.
//!
//! The client is mounted on the instId [`INST_ID`], as the factory mounts it; every report it returns names the
//! USER name [`SYMBOL`] (decision 0290), read off the held kept list (`crate::demo_credentials`).

use serde_json::json;

use crate::demo_credentials::load_demo_credentials;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_execution::recon::ReconcileClient;
use vike_model::now_ms;
use vike_okx::ReconcileClientOkx;
use vike_okx::execution::rest::{PATH_INSTRUMENTS, parse_okx_perp_instruments};
use vike_okx::signer::OkxV5Signer;
use vike_okx::test_support::demo_net;
use vike_okx::transport::client::rest_transport;
use vike_okx::transport::client::unwrap_okx;

/// The venue's instId: what the instruments read and the client's mount sign.
const INST_ID: &str = "BTC-USDT-SWAP";
/// The USER name of [`INST_ID`] (decision 0290): what every report names.
const SYMBOL: &str = "BTCUSDT.P";

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_credentials() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(
            &demo_net().rest,
            PATH_INSTRUMENTS,
            &[("instType", "SWAP".into()), ("instId", INST_ID.into())],
        )
        .expect("public instruments");
    let ct_val = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({"data": d})).expect("instruments data"),
    )[INST_ID]
        .ct_val;

    let client = ReconcileClientOkx::new(
        OkxV5Signer::new(&creds, now_ms),
        rest_transport(true).with_rate_gate(vike_okx::transport::ratelimit::rest_rate_gate()),
        demo_net().rest,
        INST_ID,
        ct_val,
    );

    // OKX's demo `cashBal` funding isn't asserted positive by the existing `okx_perp_smoke.rs`
    // ladder either — accept `None`/`Some`, but a `Some` value must be plausible, never garbage.
    let balance = client.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_okx", "USDT cash balance: {balance:?}");
    match balance {
        Some(b) => assert!(b.is_finite() && b >= 0.0, "implausible balance: {b}"),
        None => {
            tracing::warn!(target: "vike_okx", "no USDT row in account balance (unfunded demo account?)")
        }
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    tracing::info!(target: "vike_okx", "open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "okx");
        assert_eq!(o.symbol, SYMBOL);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    tracing::info!(target: "vike_okx", "recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "okx");
        assert_eq!(f.symbol, SYMBOL);
    }

    // OKX omits a flat symbol entirely — `parse_positions` synthesizes ONE flat row so a stale
    // local position stays detectable (see `reconcile/mod.rs`'s module doc). Live-pin that here.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "positions must be non-empty (synthesized flat row if none)");
    assert_eq!(positions[0].symbol, SYMBOL);
    tracing::info!(target: "vike_okx", "position: qty={} avg_px={}", positions[0].qty, positions[0].average_price);

    tracing::info!(target: "vike_okx", "okx reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}
