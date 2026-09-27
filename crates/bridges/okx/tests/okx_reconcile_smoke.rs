//! Reconciliation-activation Task 8 LIVE demo smoke (OKX sibling of
//! `binance_reconcile_smoke.rs` in `vike-binance`): proves the real `OkxReconClient`
//! (`vike_okx::recon_client`, the client `vike-app`'s `build_recon_client` wires up behind
//! `VIKE_RECONCILE=1` for the okx arm) actually fetches AND parses against the live OKX demo
//! (simulated-trading) REST API — not just the fixture bodies `tests/offline/recon_client_parse.rs`
//! exercises offline. Constructed exactly like `vike-app::build_recon_client`'s okx arm: same
//! signer (`OkxV5Signer`), same rate-gated `UreqOkxTransport::new(true)` (demo /
//! `x-simulated-trading: 1`), same `perp::REST` base URL + a live-fetched `ct_val`.
//!
//!     cargo test -p vike-okx --test okx_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY, double-gated exactly like every other `*_smoke.rs` in this crate (network +
//! `OKX_DEMO_*` creds + passphrase in the workspace `.env`; self-skips with a `tracing::warn!`
//! when absent — see `okx_perp_smoke.rs`). No order-placing variant here (kept to the binance
//! sibling, per the reconciliation-activation task brief) — this smoke covers the full
//! `ReconClient` read surface: `fetch_balance`/`fetch_order_status_reports`/`fetch_fill_reports`/
//! `fetch_position_status_reports`, asserting each succeeds and parses into plausible,
//! internally-consistent values. `fetch_position_status_reports` is asserted NON-EMPTY: OKX
//! omits a symbol entirely once flat (no `pos: "0"` row), so `parse_positions` synthesizes ONE
//! flat row when the venue sends none — see `recon_client.rs`'s module doc; this smoke pins that
//! synthesis live, not just against the fixture body.

use serde_json::json;

use vike_bridge_core::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_dotenv_from,
};
use vike_bridge_core::signer::OkxV5Signer;
use vike_exec::recon::ReconClient;
use vike_model::clock::now_ms;
use vike_okx::OkxReconClient;
use vike_okx::perp::{PATH_INSTRUMENTS, REST, parse_okx_perp_instruments};
use vike_okx::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};

const SYMBOL: &str = "BTC-USDT-SWAP";

/// The double-gate every `*_smoke.rs` in this crate uses (see `okx_perp_smoke.rs`): load the
/// workspace `.env`, look up `OKX_DEMO_API_KEY`/`_API_SECRET`/`_API_PASSPHRASE`, and return
/// `None` (after a `tracing::warn!`) when absent.
fn load_demo_creds() -> Option<Credentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("okx", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_okx", "SKIP: OKX_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + demo creds — run manually (see module doc)"]
fn okx_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };
    assert!(creds.passphrase.is_some(), "OKX needs the API passphrase");

    let transport =
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()); // demo: x-simulated-trading: 1
    let info = transport
        .public(REST, PATH_INSTRUMENTS, &[("instType", "SWAP".into()), ("instId", SYMBOL.into())])
        .expect("public instruments");
    let ct_val = parse_okx_perp_instruments(
        &unwrap_okx(info).map(|d| json!({"data": d})).expect("instruments data"),
    )[SYMBOL]
        .ct_val;

    let client = OkxReconClient::new(
        OkxV5Signer::new(&creds, now_ms),
        UreqOkxTransport::new(true).with_rate_gate(vike_okx::ratelimit::rest_rate_gate()),
        REST,
        SYMBOL,
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
    // local position stays detectable (see `recon_client.rs`'s module doc). Live-pin that here.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "positions must be non-empty (synthesized flat row if none)");
    assert_eq!(positions[0].symbol, SYMBOL);
    tracing::info!(target: "vike_okx", "position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px);

    tracing::info!(target: "vike_okx", "okx reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}
