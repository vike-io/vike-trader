//! LIVE demo smoke (network + fxPractice creds — run manually): proves the real
//! `OandaReconClient` (`vike_oanda::recon_client`) actually fetches AND parses against the live
//! OANDA v20 fxPractice REST API — not just the synthetic bodies `tests/offline/oanda_reconcile_parse.rs`
//! exercises offline (no network).
//!
//!     cargo test -p vike-oanda --test oanda_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY — safe to run any time, never places an order: constructs the real `OandaReconClient`
//! and calls all four `ReconClient` methods (`fetch_balance` / `fetch_order_status_reports` /
//! `fetch_fill_reports` / `fetch_position_status_reports`), asserting each succeeds and parses into
//! plausible, internally-consistent values (right venue/symbol on every row; a `Some` balance is a
//! real finite number; the position report always yields the mounted symbol — the synthesized flat
//! row when the account holds no EUR/USD, see `recon_client.rs`'s module doc).
//!
//! Double-gated exactly like every other `*_smoke.rs` (see `oanda_demo_smoke.rs`): network +
//! the default DEMO oanda account's `API_KEY` / `ACCOUNT_ID` in the credential store (resolved via
//! `vike_secrets::smoke_directory`), self-skip (a `tracing::warn!`, then an early `return`) when
//! creds are absent — never fails CI.

use vike_exec::recon::ReconClient;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_oanda::{OandaReconClient, load_oanda_config_from};

const SYMBOL: &str = "EURUSD";

#[test]
#[ignore = "network + a demo (fxPractice) oanda account's credentials — run manually (see module doc)"]
fn oanda_reconcile_fetch_smoke() {
    vike_log::test_init();
    let dir = vike_secrets::smoke_directory();
    let creds = dir.credentials_for("oanda", &AccountLabel::Default, AccountTier::Demo);
    let Some(config) = load_oanda_config_from(AccountTier::Demo, &creds) else {
        tracing::warn!(target: "vike_oanda::smoke", "SKIP: demo oanda account credentials absent");
        return;
    };

    let client = OandaReconClient::connect(&config, SYMBOL).expect("connect (fxPractice /summary)");

    // Balance: the fxPractice account carries a virtual balance — a `Some` must be a real finite
    // number (an fxPractice account is always funded, so we expect `Some`, but accept `None`
    // rather than assert a hard floor).
    let balance = client.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_oanda::smoke", "account balance: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite(), "implausible balance: {b}");
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    tracing::info!(target: "vike_oanda::smoke", "open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "oanda");
        assert_eq!(o.symbol, SYMBOL);
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    tracing::info!(target: "vike_oanda::smoke", "recent fills: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "oanda");
        assert_eq!(f.symbol, SYMBOL);
    }

    // `/positions` filtered to EUR_USD; when the account has never traded it, the client synthesizes
    // a flat row — so the report is always non-empty and always echoes the mounted symbol (this is
    // load-bearing for `recon::diff` — see the module doc).
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "positions must always echo {SYMBOL} (flat row when absent)");
    assert_eq!(positions[0].symbol, SYMBOL);
    tracing::info!(
        target: "vike_oanda::smoke",
        "position: qty={} avg_px={}", positions[0].qty, positions[0].avg_px
    );

    tracing::info!(
        target: "vike_oanda::smoke",
        "reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live"
    );
}
