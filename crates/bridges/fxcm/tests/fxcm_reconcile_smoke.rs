//! Live demo RECONCILE smoke — proves the real `FxcmReconClient` (`vike_fxcm::recon_client`, the
//! ReconFactory seam) actually logs in, snapshots the ForexConnect Orders/Trades tables over the
//! native FFI, AND parses them — not just the literal fixture bodies `tests/fxcm_reconcile_parse.rs`
//! exercises offline. Built through the SAME `recon_client(&config, symbol)` factory a live mount
//! would call (its OWN dedicated session thread for reconcile reads, isolated from the exec side).
//!
//! ```text
//! # Needs the vendored ForexConnect SDK so the native shim (incl. fc_orders/fc_trades) is built in:
//! set FCSDK_DIR=C:\path\to\vendor\fcsdk               # Windows
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux           # Linux (or rely on the default)
//! cargo test -p vike-fxcm --features fxcm --test fxcm_reconcile_smoke -- --ignored --nocapture
//! ```
//!
//! READ-ONLY — safe to run any time, never places an order. Triple-gated like the other `*_smoke.rs`:
//! 1. `#[ignore]` — never runs in the normal suite.
//! 2. Self-skips (a printed note, then an early `return`) when `FXCM_DEMO_*` creds are absent from the
//!    workspace `.env`.
//! 3. Self-skips when the factory returns `None` — a STUB build (no SDK, so `fc_login` is
//!    `Unavailable`) OR a failed/rotated demo login — so a cred-less or SDK-less rig sees a clean
//!    skip, never a red. Calls all four `ReconClient` methods, asserting each succeeds and parses into
//!    plausible, internally consistent values: every returned row carries `venue == "fxcm"` and the
//!    mounted symbol, and the position report is never empty (FXCM omits a symbol once flat, so
//!    `parse_positions` synthesizes the flat `BOTH` row `recon::diff` needs — a load-bearing contract).

use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_fxcm::{load_fxcm_config_from, recon_client};

/// The mounted canonical symbol every returned report row must carry. EUR/USD is on the FXCM demo;
/// the read-only order/fill/position/balance snapshots don't depend on market hours.
const SYMBOL: &str = "EURUSD";

/// Same config gate the other FXCM paths use: the workspace `.env`; `None` (-> self-skip) when
/// `FXCM_DEMO_*` is absent. Never sweeps the process env — this repo's credentials live only in
/// the `.env` file.
fn cfg() -> Option<vike_fxcm::FxcmConfig> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    load_fxcm_config_from(Environment::Demo, &vars)
}

#[test]
#[ignore = "live demo; needs the ForexConnect SDK (--features fxcm) + FXCM_DEMO_* creds — read-only, run manually (see module doc)"]
fn fxcm_reconcile_fetch_smoke() {
    vike_log::test_init();
    let Some(c) = cfg() else {
        eprintln!("skip: no FXCM_DEMO creds");
        return;
    };
    // The production seam: a dedicated session thread for reconcile reads. `None` means one of two
    // things that used to be reported identically — and reporting them identically is the defect:
    // a STUB build (no vendored SDK, so `fc_login` cannot run) is a legitimate skip, while a LINKED
    // build failing here means the demo LOGIN FAILED and the test must redden.
    let Some(client) = recon_client(&c, SYMBOL) else {
        assert!(
            !vike_fxcm::sdk_linked(),
            "FXCM credentials are configured and the ForexConnect SDK IS linked, yet the reconcile \
             client could not be built — the demo login FAILED (stale password, or the venue's \
             host-discovery endpoint is down). This used to print a skip note and return, which \
             reported an unreachable venue as a GREEN test. See `vike_fxcm::sdk_linked()`."
        );
        eprintln!("skip: STUB build without the ForexConnect SDK");
        return;
    };

    // Demo account balance isn't a documented funding contract — accept `Some`/`None`, but a `Some`
    // must be a real, finite number.
    let balance = client.fetch_balance().expect("fetch_balance");
    println!("account balance: {balance:?}");
    if let Some(b) = balance {
        assert!(b.is_finite(), "implausible account balance: {b}");
    }

    let orders = client.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    println!("resting orders for {SYMBOL}: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "fxcm");
        assert_eq!(o.symbol, SYMBOL, "orders are filtered to the mounted symbol");
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    println!("open-trade fills for {SYMBOL}: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "fxcm");
        assert_eq!(f.symbol, SYMBOL, "fills are filtered to the mounted symbol");
    }

    // FXCM omits a symbol from the Trades table once flat — `parse_positions` synthesizes ONE flat
    // `BOTH` row so the reconcile diff can still detect a stale local position. Asserting it live
    // pins the trait dispatch, not just the offline parser.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "position report must never be empty (flat row is synthesized)");
    assert_eq!(positions[0].venue, "fxcm");
    assert_eq!(positions[0].symbol, SYMBOL);
    assert_eq!(
        positions[0].position_side,
        vike_model::events::PositionSide::Both,
        "FXCM reports net position as BOTH so it matches BOTH-keyed local state"
    );
    println!(
        "position for {SYMBOL}: qty={} avg_px={} side={:?}",
        positions[0].qty, positions[0].avg_px, positions[0].position_side
    );

    println!(
        "fxcm reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live"
    );
}
