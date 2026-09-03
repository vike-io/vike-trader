//! Live demo RECONCILE smoke — proves the real `IgReconClient` (`vike_ig::recon_client`, the
//! factory the ReconFactory seam wires up) actually fetches AND parses against the live IG demo
//! gateway, not just the literal fixture bodies `tests/recon_client_parse.rs` exercises offline (no
//! network). Built through the SAME `recon_client(&config, epic)` factory a live mount would call (a
//! fresh logged-in `IgSession` dedicated to reconcile reads), so this exercises the production
//! construction path, not a hand-rolled client.
//!
//!     cargo test -p vike-ig --test ig_reconcile_smoke -- --ignored --nocapture
//!
//! READ-ONLY — safe to run any time, never places an order. Double-gated like every other
//! `*_smoke.rs`: network + `IG_DEMO_*` creds in the workspace `.env`, self-skip (a printed note,
//! then an early `return`) when creds are absent OR the demo login fails (a rotated demo password
//! must not turn into a CI failure), so a cred-less rig sees a clean skip, never a red. Calls all
//! four `ReconClient` methods, asserting each succeeds and parses into plausible, internally
//! consistent values: every returned row carries `venue == "ig"` and the mounted epic, and the
//! position report is never empty — IG omits an epic once flat, so `parse_positions` synthesizes
//! the flat `BOTH` row `recon::diff` needs (a load-bearing contract, see `recon_client.rs`'s module
//! doc — asserting it live pins the trait dispatch, not just the pure parser).

use vike_bridge_core::credentials::{load_workspace_dotenv_from, Environment};
use vike_ig::load_ig_config_from;

/// The mounted IG epic every returned report row must carry. EUR/USD mini is present on the demo
/// gateway; the read-only order/fill/position/balance fetches don't depend on market hours.
const EPIC: &str = "CS.D.EURUSD.MINI.IP";

/// Same config gate the other IG paths use: the workspace `.env`; `None` (-> self-skip) when
/// `IG_DEMO_*` is absent. Never sweeps the process env — this repo's credentials live only in the
/// `.env` file.
fn cfg() -> Option<vike_ig::IgConfig> {
    let vars = load_workspace_dotenv_from(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    load_ig_config_from(Environment::Demo, &vars)
}

#[test]
#[ignore = "live demo; needs IG_DEMO_* creds — read-only, run manually (see module doc)"]
fn ig_reconcile_fetch_smoke() {
    let Some(c) = cfg() else {
        eprintln!("skip: no IG_DEMO creds");
        return;
    };
    // The production seam: a fresh session dedicated to reconcile reads. `connect` logs in, so a
    // `None` here means the demo login failed (stale password / gateway down) — a skip, not a red.
    let Some(client) = vike_ig::recon_client(&c, EPIC) else {
        eprintln!("skip: IG demo login failed (creds present but session not established)");
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
    println!("resting working orders for {EPIC}: {}", orders.len());
    for o in &orders {
        assert_eq!(o.venue, "ig");
        assert_eq!(o.symbol, EPIC, "working orders are filtered to the mounted epic");
        assert_eq!(o.status, "ACCEPTED", "a resting working order is ACCEPTED");
    }

    let fills = client.fetch_fill_reports(0).expect("fetch_fill_reports");
    println!("recent executed deals for {EPIC}: {}", fills.len());
    for f in &fills {
        assert_eq!(f.venue, "ig");
        assert_eq!(f.symbol, EPIC, "fills are filtered client-side to the mounted epic");
    }

    // IG omits an epic entirely once flat — `parse_positions` synthesizes ONE flat `BOTH` row so
    // the reconcile diff can still detect a stale local position. Asserting it live pins the trait
    // dispatch, not just the offline parser.
    let positions = client.fetch_position_status_reports().expect("fetch_position_status_reports");
    assert!(!positions.is_empty(), "position report must never be empty (flat row is synthesized)");
    assert_eq!(positions[0].venue, "ig");
    assert_eq!(positions[0].symbol, EPIC);
    assert_eq!(
        positions[0].position_side,
        vike_model::events::PositionSide::Both,
        "IG reports net position as BOTH so it matches BOTH-keyed local state"
    );
    println!(
        "position for {EPIC}: qty={} avg_px={} side={:?}",
        positions[0].qty, positions[0].avg_px, positions[0].position_side
    );

    println!(
        "ig reconcile fetch smoke green: balance+orders+fills+positions all fetched & parsed live"
    );
}
