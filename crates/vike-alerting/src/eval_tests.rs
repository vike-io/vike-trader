use super::*;
use crate::rule::AlertRule;

/// The test implementation of [`SnapshotFacts`] — plain OWNED data, which is the whole
/// reason that input is a trait rather than a struct of borrowed slices (see its doc): a
/// helper can return this by value, where a borrowing struct would be self-referential.
#[derive(Default)]
struct Facts {
    marks: Vec<(String, String, f64)>,
    drawdown_curve: f64,
    capital_base: f64,
    pnl_total: f64,
    /// `(kind, detail, proposed_event_count)`, owned here and borrowed on the way out.
    recon: Vec<(String, String, usize)>,
}

impl SnapshotFacts for Facts {
    fn mark(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.marks.iter().find(|(v, s, _)| v == venue && s == symbol).map(|(_, _, px)| *px)
    }
    fn drawdown_curve(&self) -> f64 {
        self.drawdown_curve
    }
    fn capital_base(&self) -> f64 {
        self.capital_base
    }
    fn pnl_total(&self) -> f64 {
        self.pnl_total
    }
    fn recon_alerts(&self) -> Vec<ReconAlertFact<'_>> {
        self.recon
            .iter()
            .map(|(k, d, n)| ReconAlertFact { kind: k, detail: d, proposed_event_count: *n })
            .collect()
    }
}

fn snap_with_mark(venue: &str, symbol: &str, px: f64) -> Facts {
    Facts { marks: vec![(venue.to_string(), symbol.to_string(), px)], ..Default::default() }
}

/// ⚠ The five fields [`eval_event_rule`] renders, and no more. `side` is `i32` because that
/// is what `FillEvent::side` is on the core side, so the rendered text is byte-identical.
fn fill_event<'a>(venue: &'a str, symbol: &'a str) -> AlertEvent<'a> {
    AlertEvent::Fill { venue, symbol, side: 1, last_qty: 0.5, last_px: 100.0 }
}

// ---- Price crossing (edge) --------------------------------------------------------------

#[test]
fn price_above_fires_only_on_the_upward_crossing() {
    let rule = AlertRule::new(
        "p",
        RuleTrigger::Price {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            op: Compare::Above,
            level: 100.0,
        },
    );
    let mut st = RuleState::default();
    // First mark below the level: seeds last_value, never fires (no prior to cross from).
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 99.0), 1)
            .is_none()
    );
    // Crosses up 99 -> 101: fires once.
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 101.0), 2)
            .is_some()
    );
    // Stays above (101 -> 102): must NOT re-fire (edge, not level).
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 102.0), 3)
            .is_none()
    );
    // Drops back under then crosses up again: re-arms and fires.
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 98.0), 4)
            .is_none()
    );
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 105.0), 5)
            .is_some()
    );
}

#[test]
fn price_below_fires_on_the_downward_crossing_and_ignores_the_wrong_symbol() {
    let rule = AlertRule::new(
        "p",
        RuleTrigger::Price {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            op: Compare::Below,
            level: 100.0,
        },
    );
    let mut st = RuleState::default();
    // A mark for a DIFFERENT symbol never matches (no mark for BTCUSDT ⇒ None, no state change).
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "ETHUSDT", 1.0), 1).is_none()
    );
    assert_eq!(st.last_value, None, "a non-matching snapshot must not seed last_value");
    // 101 -> 99 crosses down.
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 101.0), 2)
            .is_none()
    );
    assert!(
        eval_snapshot_rule(&rule, &mut st, &snap_with_mark("binance", "BTCUSDT", 99.0), 3)
            .is_some()
    );
}

// ---- Indicator threshold ----------------------------------------------------------------

#[test]
fn indicator_rule_matches_instrument_and_output_then_crosses() {
    let rule = AlertRule::new(
        "i",
        RuleTrigger::Indicator {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            indicator: "rsi".into(),
            output: 0,
            params: vec![14.0],
            op: Compare::Below,
            threshold: 30.0,
        },
    );
    let mut st = RuleState::default();
    let s = |v: f64| IndicatorSample {
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        indicator: "rsi".into(),
        output: 0,
        value: v,
    };
    // Wrong indicator / output / symbol never fire and never touch state.
    let wrong = IndicatorSample {
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        indicator: "macd".into(),
        output: 0,
        value: 5.0,
    };
    assert!(eval_indicator_rule(&rule, &mut st, &wrong, 1).is_none());
    assert_eq!(st.last_value, None);
    // 40 (seed, no prior) then 25: crosses below 30 → fires.
    assert!(eval_indicator_rule(&rule, &mut st, &s(40.0), 2).is_none());
    assert!(eval_indicator_rule(&rule, &mut st, &s(25.0), 3).is_some());
    // still below → no re-fire.
    assert!(eval_indicator_rule(&rule, &mut st, &s(20.0), 4).is_none());
}

// ---- Fill / OrderRejected events --------------------------------------------------------

#[test]
fn fill_rule_scoping_fires_only_for_the_matching_instrument() {
    let mut st = RuleState::default();
    let scoped = AlertRule::new(
        "f",
        RuleTrigger::Fill { venue: Some("binance".into()), symbol: Some("BTCUSDT".into()) },
    );
    assert!(eval_event_rule(&scoped, &mut st, &fill_event("binance", "BTCUSDT"), 1).is_some());
    assert!(
        eval_event_rule(&scoped, &mut RuleState::default(), &fill_event("okx", "BTCUSDT"), 2)
            .is_none(),
        "wrong venue must not fire"
    );
    assert!(
        eval_event_rule(&scoped, &mut RuleState::default(), &fill_event("binance", "ETHUSDT"), 3)
            .is_none(),
        "wrong symbol must not fire"
    );
    // Unscoped fires for any fill.
    let any = AlertRule::new("f2", RuleTrigger::Fill { venue: None, symbol: None });
    assert!(
        eval_event_rule(&any, &mut RuleState::default(), &fill_event("okx", "SOLUSDT"), 4)
            .is_some()
    );
    // A non-fill event never fires a fill rule.
    let rej = AlertEvent::OrderRejected { client_order_id: "c1", reason: "x" };
    assert!(eval_event_rule(&any, &mut RuleState::default(), &rej, 5).is_none());
}

#[test]
fn order_rejected_rule_fires_on_both_reject_and_deny() {
    let rule = AlertRule::new("r", RuleTrigger::OrderRejected);
    let rej = AlertEvent::OrderRejected { client_order_id: "c1", reason: "insufficient balance" };
    let den = AlertEvent::OrderDenied { client_order_id: "c2", reason: "risk gate" };
    let fired = eval_event_rule(&rule, &mut RuleState::default(), &rej, 1).unwrap();
    assert!(fired.body.contains("insufficient balance"));
    assert!(eval_event_rule(&rule, &mut RuleState::default(), &den, 2).is_some());
    // an accepted fill never fires an order-rejected rule.
    assert!(
        eval_event_rule(&rule, &mut RuleState::default(), &fill_event("binance", "BTCUSDT"), 3)
            .is_none()
    );
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

// ---- Feed / breaker / resolution signals ------------------------------------------------

#[test]
fn feed_signal_fires_on_the_requested_state_and_scope() {
    let degraded_rule = AlertRule::new(
        "fd",
        RuleTrigger::Feed { venue: Some("binance".into()), state: FeedState::Degraded },
    );
    // matching venue + degraded → fires.
    assert!(
        eval_signal_rule(
            &degraded_rule,
            &mut RuleState::default(),
            &AlertSignal::Feed { venue: "binance".into(), degraded: true },
            1
        )
        .is_some()
    );
    // a recovery signal must NOT fire a "degraded" rule.
    assert!(
        eval_signal_rule(
            &degraded_rule,
            &mut RuleState::default(),
            &AlertSignal::Feed { venue: "binance".into(), degraded: false },
            2
        )
        .is_none()
    );
    // wrong venue must not fire.
    assert!(
        eval_signal_rule(
            &degraded_rule,
            &mut RuleState::default(),
            &AlertSignal::Feed { venue: "okx".into(), degraded: true },
            3
        )
        .is_none()
    );
    // a "recovered" rule fires on the recovery signal.
    let recovered_rule =
        AlertRule::new("fr", RuleTrigger::Feed { venue: None, state: FeedState::Recovered });
    assert!(
        eval_signal_rule(
            &recovered_rule,
            &mut RuleState::default(),
            &AlertSignal::Feed { venue: "okx".into(), degraded: false },
            4
        )
        .is_some()
    );
}

#[test]
fn breaker_and_resolution_signals_fire_on_their_own_kind_only() {
    let breaker = AlertRule::new(
        "b",
        RuleTrigger::FillRateBreaker { venue: None, symbol: Some("BTCUSDT".into()) },
    );
    assert!(
        eval_signal_rule(
            &breaker,
            &mut RuleState::default(),
            &AlertSignal::FillRateBreaker { venue: "binance".into(), symbol: "BTCUSDT".into() },
            1
        )
        .is_some()
    );
    // symbol mismatch → no fire.
    assert!(
        eval_signal_rule(
            &breaker,
            &mut RuleState::default(),
            &AlertSignal::FillRateBreaker { venue: "binance".into(), symbol: "ETHUSDT".into() },
            2
        )
        .is_none()
    );
    // a feed signal never fires a breaker rule.
    assert!(
        eval_signal_rule(
            &breaker,
            &mut RuleState::default(),
            &AlertSignal::Feed { venue: "binance".into(), degraded: true },
            3
        )
        .is_none()
    );

    let resolution =
        AlertRule::new("pm", RuleTrigger::PolymarketResolution { token_id: Some("0xtok".into()) });
    assert!(
        eval_signal_rule(
            &resolution,
            &mut RuleState::default(),
            &AlertSignal::PolymarketResolution { token_id: "0xtok".into() },
            4
        )
        .is_some()
    );
    assert!(
        eval_signal_rule(
            &resolution,
            &mut RuleState::default(),
            &AlertSignal::PolymarketResolution { token_id: "0xOTHER".into() },
            5
        )
        .is_none()
    );
}

// ---- cooldown / once gating -------------------------------------------------------------

#[test]
fn cooldown_suppresses_refire_within_the_window() {
    let mut rule = AlertRule::new("f", RuleTrigger::Fill { venue: None, symbol: None });
    rule.cooldown_ms = 1000;
    let mut st = RuleState::default();
    let f = fill_event("binance", "BTCUSDT");
    assert!(eval_event_rule(&rule, &mut st, &f, 1000).is_some(), "first fill fires");
    assert!(eval_event_rule(&rule, &mut st, &f, 1500).is_none(), "within cooldown → suppressed");
    assert!(eval_event_rule(&rule, &mut st, &f, 2000).is_some(), "cooldown elapsed → fires");
}

#[test]
fn once_fires_at_most_a_single_time() {
    let mut rule = AlertRule::new("f", RuleTrigger::Fill { venue: None, symbol: None });
    rule.once = true;
    let mut st = RuleState::default();
    let f = fill_event("binance", "BTCUSDT");
    assert!(eval_event_rule(&rule, &mut st, &f, 1).is_some());
    assert!(eval_event_rule(&rule, &mut st, &f, 2).is_none(), "once ⇒ never again");
    assert!(eval_event_rule(&rule, &mut st, &f, 9_999).is_none());
}
