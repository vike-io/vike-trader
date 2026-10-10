//! `eval_snapshot_rule`: the price crossing edge, the drawdown latch, the reconcile-alert edge.

use super::*;
use crate::rule::{AlertRule, Compare, RuleTrigger};
use crate::testkit::{Facts, price_rule, snap_mark, snap_with_mark};

// ---- Price crossing (edge) --------------------------------------------------------------

#[test]
fn price_above_fires_only_on_the_upward_crossing() {
    let rule = price_rule("p", Compare::Above, 100.0);
    let mut st = RuleState::default();
    // First mark below the level: seeds last_value, never fires (no prior to cross from).
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(99.0), 1).is_none());
    // Crosses up 99 -> 101: fires once.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(101.0), 2).is_some());
    // Stays above (101 -> 102): must NOT re-fire (edge, not level).
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(102.0), 3).is_none());
    // Drops back under then crosses up again: re-arms and fires.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(98.0), 4).is_none());
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(105.0), 5).is_some());
}

#[test]
fn price_below_fires_on_the_downward_crossing_and_ignores_the_wrong_symbol() {
    let rule = price_rule("p", Compare::Below, 100.0);
    let mut st = RuleState::default();
    // A mark for a DIFFERENT symbol never matches (no mark for BTCUSDT ⇒ None, no state change).
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "ETHUSDT", 1.0), 1).is_none()
    );
    assert_eq!(st.last_value, None, "a non-matching snapshot must not seed last_value");
    // 101 -> 99 crosses down.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(101.0), 2).is_none());
    assert!(eval_snapshot_rule(&rule, &mut st, &snap_mark(99.0), 3).is_some());
}

// ---- Drawdown latch ---------------------------------------------------------------------

/// A snapshot whose DRAWDOWN CURVE is `curve`, expressed the way a real core does: a fixed
/// 1000 capital base plus one venue block carrying the daemon's own realized PnL. `equity_total`
/// is set to a deliberately DIFFERENT, much larger number — the shape a live mount produces,
/// where the block has adopted a venue wallet — so a rule that reads the wrong field is caught.
fn dd_snap(curve: f64) -> Facts {
    Facts {
        drawdown_curve: curve,
        capital_base: 1_000.0,
        pnl_total: curve - 1_000.0,
        ..Default::default()
    }
}

#[test]
fn drawdown_latches_on_breach_and_rearms_after_recovery() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { pct: 0.10 });
    let mut st = RuleState::default();
    let snap = dd_snap;
    // Peak climbs to 1000; no drawdown → no fire.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap(1000.0), 1).is_none());
    assert!(eval_snapshot_rule(&rule, &mut st, &snap(1010.0), 2).is_none());
    // Drops to 900 = 10.9% off the 1010 peak → fires once.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap(900.0), 3).is_some());
    // Still down → latched, no re-fire.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap(880.0), 4).is_none());
    // Recovers above the threshold band → re-arms (no fire on recovery).
    assert!(eval_snapshot_rule(&rule, &mut st, &snap(1005.0), 5).is_none());
    assert!(!st.latched, "recovery re-arms the latch");
    // A fresh breach fires again.
    assert!(eval_snapshot_rule(&rule, &mut st, &snap(850.0), 6).is_some());
}

// ⚠ **TWO TESTS MOVED OUT OF THIS FILE on 2026-09-23, and they are the reason this crate
// could not simply drop its vike-core edge.** They built a `CoreSnapshot` whose `equity_total`
// was deliberately far larger than its own equity curve, so that a rule reading the wrong
// field was CAUGHT — the guard for the the CI box incident of 2026-08-17, where a third party's
// withdrawal from a SHARED account fired a false drawdown alert while a real 25% loss fired
// nothing at all. That distinction is computed by `Portfolio`'s own aggregation law, which
// this crate can no longer see: handing the evaluator a plain `drawdown_curve: f64` would only
// prove that the number passed in is the number read back.
//
// They now live in `crates/vike-tradehub/src/alerts.rs`, beside the CONVERSION — the only
// place the two quantities still exist side by side.

// ---- Recon alert ------------------------------------------------------------------------

#[test]
fn recon_alert_rule_fires_on_presence_edge_with_optional_kind_filter() {
    // ⚠ The ACCOUNT label is deliberately absent from [`ReconAlertFact`]: this rule keys on
    // PRESENCE and (optionally) on `kind`, never on the account, so the fact carries only
    // what is matched or rendered.
    let with_alert = Facts {
        recon: vec![("UnknownOrder".into(), "venue order X".into(), 2)],
        ..Default::default()
    };
    let empty = Facts::default();

    // Unfiltered: fires on the rising edge, then latches until the alert clears.
    let any = AlertRule::new("ra", RuleTrigger::ReconAlert { divergence_kind: None });
    let mut st = RuleState::default();
    assert!(eval_snapshot_rule(&any, &mut st, &empty, 1).is_none(), "no alert → no fire");
    assert!(eval_snapshot_rule(&any, &mut st, &with_alert, 2).is_some(), "rising edge fires");
    assert!(eval_snapshot_rule(&any, &mut st, &with_alert, 3).is_none(), "still present → latched");
    assert!(eval_snapshot_rule(&any, &mut st, &empty, 4).is_none(), "clears → re-arm, no fire");
    assert!(eval_snapshot_rule(&any, &mut st, &with_alert, 5).is_some(), "re-appears → fires");

    // Kind filter: only the named DivergenceKind fires.
    let scoped = AlertRule::new(
        "rb",
        RuleTrigger::ReconAlert { divergence_kind: Some("PositionDrift".into()) },
    );
    assert!(
        eval_snapshot_rule(&scoped, &mut RuleState::default(), &with_alert, 6).is_none(),
        "kind mismatch must not fire"
    );
}
