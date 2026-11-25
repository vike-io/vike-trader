//! **The balance CROSSING under a FOLDING policy says so in the ring** — the one branch of
//! `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_reports` that no other test of this
//! crate asserted: the `warn` plus the ring note `RECON balance … absorbed …` that fires when a
//! `BalanceDrift` beyond tolerance is FOLDED rather than held.
//!
//! Why it exists: a folding policy lands the venue's figure in the book through the synthesized
//! `Event::AccountState` and raises no alert, so this note is the only operator-readable trace of
//! the absorption. The `tracing::warn!` beside it is not asserted here (the white-box
//! `runtime/tests/recon_held.rs` suite captures log lines with `tracing-test`; this test reads the
//! ring note, which is what an operator sees and reaches `CoreSnapshot::recent_events`).
//!
//! The setup is the one a real pass carries, not inputs picked to reach the line:
//!
//! - the policy is the one an operator NAMES (`ReconcilePolicy::of(ReconcilePolicyName::Synthesize)`, the
//!   only preset that folds a `BalanceDrift` — asserted below through `mode_applies`, the authority,
//!   rather than assumed);
//! - the payload is built from a `PassConfig` (the four configuration-derived fields of
//!   `vike_tradehub_core::ReconcileConfig`) field by field, exactly as the reconcile manager builds it
//!   (`reconcile_balance: true` — Feature 2 — and the DEFAULT `ReconcileBalanceTolerance`);
//! - `route_key: None`, which is what `vike_tradehub_core::ReconcileLeg::sole_account_of` produces for a
//!   single-account venue.
//!
//! Three passes: the first ADOPTS (cold start, `Delta` mode — the neighbouring branch, which must
//! NOT produce the crossing note), the second crosses (+251 against a band of ~1.03) and folds, and
//! the third reports the now-agreeing figure — the crossing is ONE-SHOT, because the folded
//! `AccountState` re-anchors the baseline and the next pass agrees.

use vike_core::spawn_core;
use vike_execution::recon::{ReconcilePolicy, mode_applies};
use vike_execution::testing::{RecordingClient, dyn_engine};
use vike_execution::{BalanceMode, Command, ReconcileReports};
use vike_model::{ReconcileDivergenceKind, ReconcilePolicyName};

use crate::kit::handle::wait_for_snapshot;
use crate::kit::requests::PassConfig;
use vike_core::testkit::test_config;

/// The figure the first pass ADOPTS (the account is still `Delta`, so nothing is diffed).
const ADOPTED: f64 = 9_999.0;
/// The figure every later pass reports: +251 against the adopted anchor, far past the default band.
const CROSSED: f64 = 10_250.0;

/// One pass's payload, built from `config` exactly as the reconcile manager builds it.
fn pass(config: &PassConfig, balance: f64) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: config.policy.clone(),
        balance: Some(balance),
        generate_missing_orders: config.generate_missing_orders,
        reconcile_balance: config.reconcile_balance,
        balance_tol: config.balance_tol,
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }))
}

fn ring_lines_containing(snap: &vike_execution::CoreSnapshot, needle: &str) -> Vec<String> {
    snap.recent_events.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

/// The primary account's published balance, or NaN before the first block exists.
fn balance(snap: &vike_execution::CoreSnapshot) -> f64 {
    snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN)
}

fn is(x: f64, want: f64) -> bool {
    (x - want).abs() < 1e-9
}

#[test]
fn a_balance_crossing_under_a_folding_policy_leaves_one_absorbed_note() {
    let config = PassConfig {
        policy: ReconcilePolicy::of(ReconcilePolicyName::Synthesize),
        reconcile_balance: true,
        ..PassConfig::default()
    };
    // PRECONDITIONS — that these inputs reach the crossing branch and not a neighbour of it.
    assert!(
        mode_applies(&config.policy, ReconcileDivergenceKind::BalanceDrift),
        "the crossing note fires only under a policy that FOLDS a BalanceDrift"
    );
    let band = config.balance_tol.abs_floor.max(config.balance_tol.rel_frac * CROSSED.abs());
    assert!((CROSSED - ADOPTED).abs() > band, "the second pass must be BEYOND the default band");

    let primary = dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let handle = spawn_core(primary, test_config(10_000.0));
    let cell = handle.snapshot_cell();

    // Pass 1 — the cold-start ADOPT. Authoritative from here on, anchored at ADOPTED.
    handle.send_command(pass(&config, ADOPTED));
    wait_for_snapshot(&cell, 5, "pass 1 adopted the venue figure", |s| {
        s.balance_mode == BalanceMode::Authoritative && is(balance(s), ADOPTED)
    });
    let snap = cell.load_full();
    assert_eq!(
        ring_lines_containing(&snap, "RECON balance"),
        vec!["RECON balance binance: adopted authoritative 9999 [account binance]".to_string()],
        "pass 1 is the ADOPT branch, and only that branch"
    );

    // Pass 2 — the CROSSING, folded on its second pass in a row (decision 0043 as amended 2026-11-21;
    // the first only starts its streak). The folded `AccountState` is the only thing that can move the
    // balance to CROSSED, so waiting on it waits on pass 2 without waiting on the note itself.
    handle.send_command(pass(&config, CROSSED));
    handle.send_command(pass(&config, CROSSED));
    wait_for_snapshot(&cell, 5, "pass 2 folded the venue figure", |s| is(balance(s), CROSSED));
    let snap = cell.load_full();
    let pass2_timestamp = snap.reconcile.last_pass_timestamp;
    assert_eq!(
        ring_lines_containing(&snap, "absorbed"),
        vec![
            "RECON balance binance USDT: absorbed +251 unexplained (venue 10250 vs expected 9999) \
             [account binance]"
                .to_string()
        ],
        "a folded crossing must leave exactly this note in the ring; ring = {:?}",
        snap.recent_events
    );
    // The state the crossing leaves: the venue's figure folded, nothing held, no position made.
    assert_eq!(snap.balance_mode, BalanceMode::Authoritative);
    assert!(
        snap.reconcile.alerts.is_empty(),
        "a folding policy holds nothing: {:?}",
        snap.reconcile.alerts
    );
    assert!(
        snap.portfolio.venues[0].positions.is_empty(),
        "a balance-only pass must not create positions: {:?}",
        snap.portfolio.venues[0].positions
    );

    // Pass 3 — the venue repeats CROSSED, which now AGREES with the re-anchored local figure.
    handle.send_command(pass(&config, CROSSED));
    wait_for_snapshot(&cell, 5, "pass 3 completed", |s| {
        s.reconcile.last_pass_timestamp > pass2_timestamp
    });
    let snap = cell.load_full();
    handle.shutdown_and_join();

    assert_eq!(
        ring_lines_containing(&snap, "absorbed").len(),
        1,
        "the crossing is ONE-SHOT — an agreeing pass adds no second note; ring = {:?}",
        snap.recent_events
    );
    assert!(is(balance(&snap), CROSSED), "the folded figure stands; got {}", balance(&snap));
    assert!(snap.reconcile.alerts.is_empty());
}
