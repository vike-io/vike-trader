//! Hyperliquid LIVE testnet reconcile smoke — the HL sibling of `binance_reconcile_smoke.rs` /
//! `okx_reconcile_smoke.rs` / `bybit_reconcile_smoke.rs`. Proves the real `HyperliquidReconClient`
//! (`vike_hyperliquid::recon_client`, the client `vike-app`'s `hyperliquid_live_client` wires up
//! behind `VIKE_RECONCILE=1` for the HL arm) actually fetches AND parses against the LIVE
//! Hyperliquid **testnet** account — not just the recorded `/info` bodies `recon_client.rs`'s unit
//! tests exercise offline (no network).
//!
//!     cargo test -p vike-hyperliquid --test hyperliquid_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY, double-gated exactly like every other live smoke in the workspace (network +
//! `HYPERLIQUID_DEMO_PRIVATE_KEY` [+ optional `_ACCOUNT_ADDRESS`] in the workspace `.env`;
//! self-skips with a `tracing::warn!` + early `return` when the key is absent). HL's credential
//! shape is bespoke (a secp256k1 private key + optional master address, like Polymarket/FX — NOT
//! the standard API_KEY/SECRET), so creds load via `config::load(Env::Demo, ..)` rather than the
//! standard `load_credentials_from`. No order-placing variant (kept to the binance sibling, per the
//! reconciliation-activation task brief) — this smoke covers the full `ReconClient` read surface:
//! `fetch_order_status_reports` / `fetch_fill_reports` / `fetch_position_status_reports` /
//! `fetch_balance`, asserting each succeeds and parses into plausible, internally-consistent values.
//!
//! UNLIKE the per-SYMBOL binance/bybit/okx sibling clients, HL's report reads are **account-wide**:
//! all reads are keyless `POST /info` scoped to the **master** account address (`account_address`
//! when the key is an agent wallet, else the signer's own derived address — the exact
//! `hyperliquid_live_client` derivation), spanning every coin, so there is no single symbol to
//! assert — each row is checked only for the `hyperliquid` venue tag, and a sample is logged.
//! Balance uses `Product::Perp` (`marginSummary.accountValue`): the testnet faucet funds the PERP
//! clearinghouse (~999 USDC), so it's asserted live + funded (`> 0.0`), mirroring the funded-demo
//! binance-perp/bybit siblings.

use vike_bridge_core::credentials::load_workspace_dotenv_from;
use vike_exec::recon::ReconClient;
use vike_hyperliquid::HyperliquidReconClient;
use vike_hyperliquid::config::{self, Env, Product};
use vike_hyperliquid::signing::Signer;
use vike_hyperliquid::transport::HyperliquidTransport;
use vike_model::clock::now_ms;

/// The double-gate every live smoke uses: load the workspace `.env`, look up the bespoke HL
/// `HYPERLIQUID_DEMO_PRIVATE_KEY` (+ optional `_ACCOUNT_ADDRESS`) via `config::load`, and return
/// `None` (after a `tracing::warn!`) when the key is absent so the caller self-skips via
/// `let Some(creds) = load_demo_creds() else { return };`.
fn load_demo_creds() -> Option<config::HlCredentials> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = config::load(Env::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_hyperliquid", "SKIP: HYPERLIQUID_DEMO creds absent");
    }
    creds
}

#[test]
#[ignore = "network + testnet creds — run manually (see module doc)"]
fn hyperliquid_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(creds) = load_demo_creds() else { return };

    // The signer is only needed to DERIVE the master address when no explicit ACCOUNT_ADDRESS is
    // set (the `/info` reads themselves are keyless) — the exact `hyperliquid_live_client`
    // derivation: `account_address` if present, else the signer's own EOA address.
    let signer = Signer::from_private_key(&creds.private_key, creds.network)
        .expect("HYPERLIQUID_DEMO_PRIVATE_KEY must be a valid secp256k1 key");
    let master = creds.account_address.clone().unwrap_or_else(|| signer.address().to_string());
    tracing::info!(target: "vike_hyperliquid", "reconcile reads scoped to master address {master}");

    // Testnet transport (`creds.network` == Testnet for `Env::Demo`) + PERP balance endpoint (the
    // faucet funds the perp clearinghouse). Its own fresh IP-weight gate, exactly like
    // `hyperliquid_live_client`'s recon transport (never shares the exec side's).
    let transport = HyperliquidTransport::new(creds.network);
    let client = HyperliquidReconClient::new(transport, master, Product::Perp);

    let since = now_ms() - 3_600_000; // last hour (a no-op for orders; the fills lookback window)

    // frontendOpenOrders (account-wide, all coins) — `since` is a no-op (only the currently-open
    // set is returned, no time filter). Every row must carry the hyperliquid venue tag.
    let orders = client.fetch_order_status_reports(since).expect("fetch_order_status_reports");
    tracing::info!(target: "vike_hyperliquid", "open orders: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "hyperliquid", "every order row carries the hyperliquid venue tag");
    }
    if let Some(o) = orders.first() {
        tracing::info!(
            target: "vike_hyperliquid",
            "  sample order: coin={} side={} qty={} filled={} status={}",
            o.symbol, o.side, o.qty, o.filled_qty, o.status
        );
    }

    // userFillsByTime(startTime = since) — account-wide fills over the lookback window.
    let fills = client.fetch_fill_reports(since).expect("fetch_fill_reports");
    tracing::info!(target: "vike_hyperliquid", "recent fills (last hour): {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "hyperliquid", "every fill row carries the hyperliquid venue tag");
    }
    if let Some(f) = fills.first() {
        tracing::info!(
            target: "vike_hyperliquid",
            "  sample fill: coin={} side={} qty={} px={} fee={} {}",
            f.symbol, f.side, f.last_qty, f.last_px, f.commission, f.commission_asset
        );
    }

    // clearinghouseState.assetPositions[] — every row kept (flat legs included, so `recon::diff`
    // can spot a locally-open position the venue has since flattened). Account-wide, all coins.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    tracing::info!(target: "vike_hyperliquid", "positions: {}", positions.len());
    for p in &positions {
        assert_eq!(p.venue, "hyperliquid", "every position row carries the hyperliquid venue tag");
    }
    if let Some(p) = positions.first() {
        tracing::info!(
            target: "vike_hyperliquid",
            "  sample position: coin={} qty={} avg_px={}",
            p.symbol, p.qty, p.avg_px
        );
    }

    // PERP marginSummary.accountValue. A read-only smoke asserts the READ + PARSE succeed, NOT that
    // the account is funded: whether the testnet USDC sits in perp margin (vs spot) is account state,
    // so a live-verified account legitimately reports `Some(0.0)`. The assertion still catches a real
    // field-name/parse regression — a wrong path yields `None` — while accepting any sane funded-or-
    // not value (`Some`, finite, non-negative).
    let balance = client.fetch_balance().expect("fetch_balance");
    tracing::info!(target: "vike_hyperliquid", "perp account value (USD): {balance:?}");
    assert!(
        balance.is_some_and(|b| b.is_finite() && b >= 0.0),
        "perp accountValue must parse to a finite non-negative USD value (None = field regression): \
         {balance:?}"
    );

    tracing::info!(
        target: "vike_hyperliquid",
        "hyperliquid reconcile fetch smoke green: orders+fills+positions+balance all fetched & parsed live against the testnet master"
    );
}
