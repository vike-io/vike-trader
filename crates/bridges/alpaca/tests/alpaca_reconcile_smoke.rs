//! Live sandbox RECONCILE smoke — proves the real `AlpacaReconClient`
//! (`vike_alpaca::recon_client`, the factory the ReconFactory seam wires up) actually fetches AND
//! parses against the live Alpaca Broker sandbox — not just the literal fixture bodies
//! `tests/recon_client_parse.rs` exercises offline (no network). Built through the SAME
//! `recon_client(&config, symbol)` factory a live mount would call (fresh OAuth `TokenSource`/
//! `AlpacaRest` dedicated to reconcile reads), so this exercises the production construction path,
//! not a hand-rolled client.
//!
//!     cargo test -p vike-alpaca --test alpaca_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY — safe to run any time, never places an order. Double-gated exactly like every other
//! `*_smoke.rs` in this crate (see `alpaca_live_smoke.rs`): network + `ALPACA_SANDBOX_*` creds in
//! the workspace `.env`, self-skip (a printed note, then an early `return`) when creds are absent,
//! so CI and a cred-less rig see a clean skip, never a failure. Calls all four `ReconClient`
//! methods (`fetch_order_status_reports`/`fetch_fill_reports`/`fetch_position_status_reports`/
//! `fetch_balance`), asserting each succeeds and parses into plausible, internally-consistent
//! values: every returned row carries `venue == "alpaca"` and the mounted vike symbol, and the
//! position report is never empty — Alpaca omits a symbol once flat, so `parse_positions`
//! synthesizes the flat row `recon::diff::diff` needs (a load-bearing contract, see
//! `recon_client.rs`'s module doc — asserting it live pins the trait dispatch, not just the pure
//! parser).

use vike_alpaca::load_alpaca_config_from;
use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};

/// The mounted vike-side symbol every returned report row must carry (equity; the read-only
/// order/fill/position/balance fetches are independent of US market hours).
const SYMBOL: &str = "AAPL";

/// Same config gate `alpaca_live_smoke.rs` uses: the credential store; `None` (→ self-skip) when
/// `ALPACA_SANDBOX_*` is absent. No CREDENTIAL is ever swept out of the process env — this repo
/// keeps them only in the store. The one variable read here names the store's DIRECTORY, which is
/// what lets this smoke run from a lane at all: the Alpaca hosts are unreachable from the Windows
/// dev box, and a lane's checkout has no `secrets.env` of its own (`settings/` is gitignored).
fn cfg() -> Option<vike_alpaca::AlpacaConfig> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    load_alpaca_config_from(Environment::Demo, &vars)
}

#[test]
#[ignore = "live sandbox; needs ALPACA_SANDBOX_* creds — read-only, run manually (see module doc)"]
fn alpaca_reconcile_fetch_smoke() {
    let Some(c) = cfg() else {
        eprintln!("skip: no ALPACA_SANDBOX creds");
        return;
    };
    // The production seam: a fresh OAuth client-credentials lifecycle dedicated to reconcile reads.
    // Construction is pure/infallible (no network) → always `Some`; the OAuth handshake happens
    // lazily on the first fetch below.
    let client = vike_alpaca::recon_client(&c, SYMBOL).expect("recon_client factory always Some");

    // Sandbox account cash isn't a documented funding contract — accept `Some`/`None`, but a
    // `Some` must be a real, finite number (Alpaca's `cash` can be NEGATIVE on margin, so no
    // non-negativity assert — see `parse_balance`'s `-23140.2` fixture).
    let balance = client.fetch_balance().expect("fetch_balance");
    println!("account cash: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite(), "implausible account cash: {b}");
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    println!("open/historical orders for {SYMBOL}: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "alpaca");
        assert_eq!(o.symbol, SYMBOL, "orders endpoint is symbol-filtered server-side");
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    println!("recent fills for {SYMBOL}: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "alpaca");
        assert_eq!(f.symbol, SYMBOL, "fills are filtered client-side to the mounted symbol");
    }

    // Alpaca omits a symbol entirely once flat — `parse_positions` synthesizes ONE flat row so the
    // reconcile diff can still detect a stale local position. Asserting it live pins the trait
    // dispatch, not just the offline parser.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "position report must never be empty (flat row is synthesized)");
    assert_eq!(positions[0].venue, "alpaca");
    assert_eq!(positions[0].symbol, SYMBOL);
    println!(
        "position for {SYMBOL}: qty={} avg_px={} side={:?}",
        positions[0].qty, positions[0].avg_px, positions[0].position_side
    );

    println!("alpaca reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live");
}
