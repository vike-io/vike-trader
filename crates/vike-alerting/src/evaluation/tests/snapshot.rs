//! `evaluation_snapshot_rule`: the price crossing edge, the drawdown latch, the reconcile-alert edge.

use super::*;
use crate::rule::{AlertRule, Compare, RuleTrigger};
use crate::testkit::{Facts, price_rule, snap_mark, snap_with_mark};

// ---- Price crossing (edge) --------------------------------------------------------------

#[test]
fn price_above_fires_only_on_the_upward_crossing() {
    let rule = price_rule("p", Compare::Above, 100.0);
    let mut st = RuleState::default();
    // First mark below the level: seeds last_value, never fires (no prior to cross from).
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(99.0), 1).is_none());
    // Crosses up 99 -> 101: fires once.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(101.0), 2).is_some());
    // Stays above (101 -> 102): must NOT re-fire (edge, not level).
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(102.0), 3).is_none());
    // Drops back under then crosses up again: re-arms and fires.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(98.0), 4).is_none());
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(105.0), 5).is_some());
}

#[test]
fn price_below_fires_on_the_downward_crossing_and_ignores_the_wrong_symbol() {
    let rule = price_rule("p", Compare::Below, 100.0);
    let mut st = RuleState::default();
    // A mark for a DIFFERENT symbol never matches (no mark for BTCUSDT ⇒ None, no state change).
    assert!(
        evaluation_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "ETHUSDT", 1.0), 1)
            .is_none()
    );
    assert_eq!(st.last_value, None, "a non-matching snapshot must not seed last_value");
    // 101 -> 99 crosses down.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(101.0), 2).is_none());
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap_mark(99.0), 3).is_some());
}

// ---- Drawdown latch ---------------------------------------------------------------------

/// A snapshot whose DRAWDOWN CURVE is `curve`, expressed the way a real core does: a fixed
/// 1000 capital base plus one venue block carrying the daemon's own realized PnL. `equity_total`
/// is set to a deliberately DIFFERENT, much larger number — the shape a live mount produces,
/// where the block has adopted a venue wallet — so a rule that reads the wrong field is caught.
fn drawdown_snap(curve: f64) -> Facts {
    Facts {
        drawdown_curve: curve,
        capital_base: 1_000.0,
        pnl_total: curve - 1_000.0,
        ..Default::default()
    }
}

#[test]
fn drawdown_latches_on_breach_and_rearms_after_recovery() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 });
    let mut st = RuleState::default();
    let snap = drawdown_snap;
    // Peak climbs to 1000; no drawdown → no fire.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap(1000.0), 1).is_none());
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap(1010.0), 2).is_none());
    // Drops to 900 = 10.9% off the 1010 peak → fires once.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap(900.0), 3).is_some());
    // Still down → latched, no re-fire.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap(880.0), 4).is_none());
    // Recovers above the threshold band → re-arms (no fire on recovery).
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap(1005.0), 5).is_none());
    assert!(!st.latched, "recovery re-arms the latch");
    // A fresh breach fires again.
    assert!(evaluation_snapshot_rule(&rule, &mut st, &snap(850.0), 6).is_some());
}

/// **The caller's peak is the peak** (`SnapshotFacts::drawdown_peak`): a first reading of 800 under a latch's peak
/// of 1000 is the latch's 20%, not 0%; a curve above that peak is a new high and no drop; and with no peak from
/// the caller the rule runs on its session peak, which that first reading then seeded.
#[test]
fn the_rule_measures_from_the_callers_peak_when_it_has_one() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 });
    let latched =
        |curve: f64, peak: f64| Facts { drawdown_peak: Some(peak), ..drawdown_snap(curve) };

    let mut st = RuleState::default();
    let fired =
        evaluation_snapshot_rule(&rule, &mut st, &latched(800.0, 1_000.0), 1).expect("20% >= 10%");
    assert!(fired.body.contains("own-PnL drawdown 20.00%"), "{}", fired.body);
    assert!(fired.body.contains("peak 1000"), "{}", fired.body);

    let mut st = RuleState::default();
    assert!(evaluation_snapshot_rule(&rule, &mut st, &latched(1_500.0, 1_000.0), 1).is_none());
    assert_eq!(st.peak_equity, Some(1_500.0));

    let mut st = RuleState::default();
    assert!(evaluation_snapshot_rule(&rule, &mut st, &drawdown_snap(800.0), 1).is_none());
    assert_eq!(st.peak_equity, Some(800.0));
}

/// A snapshot whose PRIMARY group is flat and which carries one OTHER currency group's curve and peak.
fn drawdown_snap_with_group(currency: &str, curve: f64, peak: f64) -> Facts {
    Facts {
        group_curves: vec![crate::GroupCurveFact { currency: currency.into(), curve, peak }],
        ..drawdown_snap(1_000.0)
    }
}

/// ⚠ **The core's latch trips the whole node when ANY currency group breaches, so the alert must see a
/// group the primary-group curve cannot** (decision 0183, item 13): the primary stays flat at 1000 while
/// the EUR group (1000 -> 700) draws down 30% of its own curve. FAILS on an alert that reads the primary
/// group alone: it stays silent while every engine goes liquidate-only.
#[test]
fn a_breach_of_another_currency_group_fires_with_the_group_named() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 });
    let mut st = RuleState::default();
    assert!(
        evaluation_snapshot_rule(
            &rule,
            &mut st,
            &drawdown_snap_with_group("EUR", 1_000.0, 1_000.0),
            1
        )
        .is_none()
    );
    let fired = evaluation_snapshot_rule(
        &rule,
        &mut st,
        &drawdown_snap_with_group("EUR", 700.0, 1_000.0),
        2,
    )
    .expect("the EUR group is 30% under its own peak");
    assert!(fired.body.contains("EUR group 30.00%"), "{}", fired.body);
    // Latched: still breached, no re-fire; the primary alone never re-arms or fires it.
    assert!(
        evaluation_snapshot_rule(
            &rule,
            &mut st,
            &drawdown_snap_with_group("EUR", 650.0, 1_000.0),
            3
        )
        .is_none()
    );
    assert_eq!(st.groups_latched, ["EUR"]);
    // Recovers under the threshold: re-arms (no fire on recovery), then a fresh breach fires again.
    assert!(
        evaluation_snapshot_rule(
            &rule,
            &mut st,
            &drawdown_snap_with_group("EUR", 1_000.0, 1_000.0),
            4
        )
        .is_none()
    );
    assert!(st.groups_latched.is_empty(), "recovery re-arms the group's latch");
    assert!(
        evaluation_snapshot_rule(
            &rule,
            &mut st,
            &drawdown_snap_with_group("EUR", 800.0, 1_000.0),
            5
        )
        .is_some()
    );
}

/// A node of one currency (no group curves) behaves exactly as before, and a group the caller stops
/// listing (it became the primary) drops its latch so a later breach fires.
#[test]
fn no_group_curves_means_the_old_rule_and_a_vanished_group_drops_its_latch() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 });
    let mut st = RuleState::default();
    assert!(evaluation_snapshot_rule(&rule, &mut st, &drawdown_snap(1_000.0), 1).is_none());
    assert!(st.groups_latched.is_empty());
    assert!(
        evaluation_snapshot_rule(
            &rule,
            &mut st,
            &drawdown_snap_with_group("EUR", 700.0, 1_000.0),
            2
        )
        .is_some()
    );
    assert!(evaluation_snapshot_rule(&rule, &mut st, &drawdown_snap(1_000.0), 3).is_none());
    assert!(st.groups_latched.is_empty(), "an unlisted group keeps no latch");
    assert!(
        evaluation_snapshot_rule(
            &rule,
            &mut st,
            &drawdown_snap_with_group("EUR", 700.0, 1_000.0),
            4
        )
        .is_some()
    );
}

/// Two groups breaching together share ONE alert (the evaluator returns one per call), both named, and
/// both are latched so neither re-fires.
#[test]
fn two_groups_breaching_together_share_one_alert_and_both_latch() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 });
    let mut st = RuleState::default();
    let both = Facts {
        group_curves: vec![
            crate::GroupCurveFact { currency: "EUR".into(), curve: 700.0, peak: 1_000.0 },
            crate::GroupCurveFact { currency: "GBP".into(), curve: 800.0, peak: 1_000.0 },
        ],
        ..drawdown_snap(1_000.0)
    };
    let fired =
        evaluation_snapshot_rule(&rule, &mut st, &both, 1).expect("both groups are breached");
    assert!(fired.body.contains("EUR group") && fired.body.contains("GBP group"), "{}", fired.body);
    assert!(evaluation_snapshot_rule(&rule, &mut st, &both, 2).is_none());
    assert_eq!(st.groups_latched.len(), 2);
}

/// ⚠ **The session peak is one number in the PRIMARY group's currency, so a different primary currency starts
/// it over** (decision 0183, item 13): the first live engine restarting into another currency used to leave
/// the euro peak standing against a curve in dollars until the daemon restarted, and the next ordinary
/// reading read as a 99% drawdown (or hid a real one).
#[test]
fn a_different_primary_currency_starts_the_session_peak_over() {
    let rule = AlertRule::new("d", RuleTrigger::Drawdown { fraction: 0.10 });
    let mut st = RuleState::default();
    let eur = |curve: f64| Facts { drawdown_currency: "EUR".into(), ..drawdown_snap(curve) };
    let usd = |curve: f64| Facts { drawdown_currency: "USD".into(), ..drawdown_snap(curve) };

    // a euro curve climbs to 50,000 (an adopted wallet), then the primary group becomes the dollar one
    assert!(evaluation_snapshot_rule(&rule, &mut st, &eur(50_000.0), 1).is_none());
    assert_eq!(st.peak_equity, Some(50_000.0));
    assert!(
        evaluation_snapshot_rule(&rule, &mut st, &usd(1_000.0), 2).is_none(),
        "not a 98% drawdown"
    );
    assert_eq!(st.peak_equity, Some(1_000.0), "the peak started over in dollars");
    // ...and a real dollar drawdown is measured against the dollar peak
    assert!(evaluation_snapshot_rule(&rule, &mut st, &usd(850.0), 3).is_some());

    // a latch taken in the old currency does not carry over either
    let mut st = RuleState::default();
    assert!(evaluation_snapshot_rule(&rule, &mut st, &eur(1_000.0), 1).is_none());
    assert!(evaluation_snapshot_rule(&rule, &mut st, &eur(500.0), 2).is_some());
    assert!(st.latched);
    assert!(evaluation_snapshot_rule(&rule, &mut st, &usd(1_000.0), 3).is_none());
    assert!(!st.latched, "a breach measured in the old currency says nothing about the new one");

    // a caller that never names a currency (every one-currency node) is the rule it always was
    let mut st = RuleState::default();
    assert!(evaluation_snapshot_rule(&rule, &mut st, &drawdown_snap(1_000.0), 1).is_none());
    assert!(evaluation_snapshot_rule(&rule, &mut st, &drawdown_snap(850.0), 2).is_some());
    assert_eq!(st.peak_currency.as_deref(), Some(""));
}

// ---- Stale conversion rate under a position -----------------------------------------------

fn stale_fact(account: &str, currency: &str, account_ccy: &str) -> crate::StaleRateFact {
    crate::StaleRateFact {
        account: account.into(),
        currency: currency.into(),
        account_ccy: account_ccy.into(),
    }
}

/// ⚠ **ONE alert per episode** (decision 0183): the rising edge of "a stale rate is valuing an open position"
/// fires once with the rate named, a second currency going stale beside it does not page again, and the rule
/// re-arms only when every held rate is fresh. Nothing held means nothing stale to say, so a quiet node (and
/// every node that holds no foreign-quoted position) fires nothing.
#[test]
fn a_stale_rate_under_a_position_fires_once_per_episode() {
    let rule = AlertRule::new("s", RuleTrigger::StaleRate);
    let mut st = RuleState::default();
    let facts = |rows: Vec<crate::StaleRateFact>| Facts { stale_rates: rows, ..Default::default() };

    assert!(evaluation_snapshot_rule(&rule, &mut st, &facts(vec![]), 1).is_none(), "nothing stale");
    let fired =
        evaluation_snapshot_rule(&rule, &mut st, &facts(vec![stale_fact("fxcm", "USD", "EUR")]), 2)
            .expect("the rising edge fires");
    assert!(fired.body.contains("fxcm: 1 USD in EUR"), "{}", fired.body);
    assert!(fired.body.contains("quote-rate-stale"), "{}", fired.body);

    let both = facts(vec![stale_fact("fxcm", "USD", "EUR"), stale_fact("oanda", "JPY", "USD")]);
    assert!(evaluation_snapshot_rule(&rule, &mut st, &both, 3).is_none(), "still the same episode");
    assert!(
        evaluation_snapshot_rule(&rule, &mut st, &facts(vec![]), 4).is_none(),
        "recovered: re-arm"
    );
    assert!(!st.latched);
    let again = evaluation_snapshot_rule(
        &rule,
        &mut st,
        &facts(vec![stale_fact("oanda", "JPY", "USD")]),
        5,
    )
    .expect("a new episode fires again");
    assert!(again.body.contains("oanda: 1 JPY in USD"), "{}", again.body);
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
fn reconcile_alert_rule_fires_on_presence_edge_with_optional_kind_filter() {
    // ⚠ The ACCOUNT label is deliberately absent from [`ReconcileAlertFact`]: this rule keys on
    // PRESENCE and (optionally) on `kind`, never on the account, so the fact carries only
    // what is matched or rendered.
    let with_alert = Facts {
        recon: vec![("UnknownOrder".into(), "venue order X".into(), 2)],
        ..Default::default()
    };
    let empty = Facts::default();

    // Unfiltered: fires on the rising edge, then latches until the alert clears.
    let any = AlertRule::new("ra", RuleTrigger::ReconciliationAlert { divergence_kind: None });
    let mut st = RuleState::default();
    assert!(evaluation_snapshot_rule(&any, &mut st, &empty, 1).is_none(), "no alert → no fire");
    assert!(evaluation_snapshot_rule(&any, &mut st, &with_alert, 2).is_some(), "rising edge fires");
    assert!(
        evaluation_snapshot_rule(&any, &mut st, &with_alert, 3).is_none(),
        "still present → latched"
    );
    assert!(
        evaluation_snapshot_rule(&any, &mut st, &empty, 4).is_none(),
        "clears → re-arm, no fire"
    );
    assert!(
        evaluation_snapshot_rule(&any, &mut st, &with_alert, 5).is_some(),
        "re-appears → fires"
    );

    // Kind filter: only the named ReconcileDivergenceKind fires.
    let scoped = AlertRule::new(
        "rb",
        RuleTrigger::ReconciliationAlert { divergence_kind: Some("PositionDrift".into()) },
    );
    assert!(
        evaluation_snapshot_rule(&scoped, &mut RuleState::default(), &with_alert, 6).is_none(),
        "kind mismatch must not fire"
    );
}
