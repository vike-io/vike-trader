use super::*;
use crate::rule::AlertRule;

fn stale(series: &str, silent_for_ms: Option<i64>, rows: u64) -> AlertSignal {
    AlertSignal::SeriesStale { series: series.to_string(), silent_for_ms, rows }
}

/// The 2026-08-05 shape: a series that was writing 250k rows/min stops, and the alert NAMES it.
/// A venue-scoped `Feed` rule cannot say which series — that is the whole reason this variant
/// exists.
#[test]
fn an_unscoped_rule_fires_for_any_series_and_names_it() {
    let rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    let fired = eval_signal_rule(
        &rule,
        &mut RuleState::default(),
        &stale("book/polymarket/0xtok", Some(19 * 60_000), 3_800_000),
        1,
    )
    .expect("an unscoped rule fires for any silent series");
    assert!(fired.body.contains("book/polymarket/0xtok"), "{}", fired.body);
    assert!(fired.body.contains("1140s"), "the silence duration is the diagnosis: {}", fired.body);
    assert!(fired.body.contains("3800000"), "…and so is the row count it had: {}", fired.body);
}

/// Never-started reads differently from stopped: the second is a venue stream that died, the
/// first is a subscription that was accepted and never served.
#[test]
fn a_never_started_series_reads_as_never_started_not_as_stale_for_zero() {
    let rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    let fired = eval_signal_rule(
        &rule,
        &mut RuleState::default(),
        &stale("depth/binance/BTCUSDT", None, 0),
        1,
    )
    .unwrap();
    assert!(fired.body.contains("NEVER received a row"), "{}", fired.body);
    assert!(fired.body.contains("stream name"), "{}", fired.body);
}

/// A PREFIX scope, because a Polymarket token id does not exist yet when the rule is written.
#[test]
fn the_scope_is_a_prefix_so_a_rotating_family_is_expressible() {
    let rule = AlertRule::new(
        "s",
        RuleTrigger::SeriesStale { series_prefix: Some("book/polymarket/".into()) },
    );
    // a token minted after the rule was written still matches.
    assert!(
        eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &stale("book/polymarket/9911", Some(600_000), 5),
            1
        )
        .is_some()
    );
    // a different venue does not.
    assert!(
        eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &stale("trade/binance/BTCUSDT", Some(600_000), 5),
            2
        )
        .is_none()
    );
    // …nor a different KIND of the same venue (the prefix carries the kind too).
    assert!(
        eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &stale("trade/polymarket/9911", Some(600_000), 5),
            3
        )
        .is_none()
    );
}

/// Cross-talk in both directions: another signal must not fire a stale rule, and a stale signal
/// must not fire another rule.
#[test]
fn a_series_stale_signal_and_rule_do_not_cross_with_the_other_kinds() {
    let stale_rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    assert!(
        eval_signal_rule(
            &stale_rule,
            &mut RuleState::default(),
            &AlertSignal::Feed { venue: "binance".into(), degraded: true },
            1
        )
        .is_none()
    );

    let feed_rule =
        AlertRule::new("f", RuleTrigger::Feed { venue: None, state: FeedState::Degraded });
    assert!(
        eval_signal_rule(
            &feed_rule,
            &mut RuleState::default(),
            &stale("trade/binance/BTCUSDT", Some(1), 1),
            2
        )
        .is_none()
    );
}

/// The rule-level cooldown gates a series-stale rule like any other discrete signal. ⚠ It is
/// PER RULE, not per series — which is exactly why `vike_recorder::liveness::SilenceWatch` owns
/// the per-series repeat gate instead of leaning on this one: with six series silent at once, a
/// rule cooldown would page for the first and swallow the other five.
#[test]
fn the_rule_cooldown_is_per_rule_which_is_why_the_producer_gates_per_series() {
    let mut rule = AlertRule::new("s", RuleTrigger::SeriesStale { series_prefix: None });
    rule.cooldown_ms = 60_000;
    let mut st = RuleState::default();
    assert!(eval_signal_rule(&rule, &mut st, &stale("a/v/s", Some(1), 1), 1_000).is_some());
    assert!(
        eval_signal_rule(&rule, &mut st, &stale("b/v/s", Some(1), 1), 1_001).is_none(),
        "a DIFFERENT series is still suppressed by the same rule's cooldown"
    );
}

// ---- the FAMILY-collapse rule -------------------------------------------------------------

/// The measured 2026-08-05 window, as it crosses the boundary into a delivered body.
///
/// The counts are the ones the the CI box replay produced for the first fully-dark 30 s window
/// (`kind=book/venue=polymarket/group=btc-updown-5m/date=2026-08-05`, window opening 04:23:00Z):
/// ZERO items across the four still-subscribed members, against a rolling baseline of 53,485
/// rows per 30 s taken over the family's own previous twenty windows — while binance wrote 348
/// to 909 trades in every minute of the same span.
fn collapse(family: &str) -> AlertSignal {
    AlertSignal::FamilyCollapse {
        family: family.to_string(),
        observed_items: 0,
        baseline_items: 53_485,
        window_secs: 30,
        members: 4,
        ring_windows: 20,
        licence: "trade/binance/BTCUSDT.P".to_string(),
        licence_items: 435,
    }
}

#[test]
fn a_family_collapse_names_the_family_both_counts_and_the_licence() {
    let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
    let fired = eval_signal_rule(
        &rule,
        &mut RuleState::default(),
        &collapse("book/polymarket/btc-updown-5m"),
        1,
    )
    .expect("an unscoped rule fires for any family");
    let b = &fired.body;
    assert!(b.contains("book/polymarket/btc-updown-5m"), "the family is named: {b}");
    assert!(b.contains("53485"), "the baseline it fell from belongs in the body: {b}");
    assert!(b.contains('0'), "…and the observed count: {b}");
    assert!(b.contains("4 member"), "the member count separates a family from one token: {b}");
    assert!(
        b.contains("trade/binance/BTCUSDT.P") && b.contains("435"),
        "the licence is the diagnosis — without it a dead family and a stalled process read \
             the same: {b}"
    );
}

/// ⚠ **The body must never state a per-second rate.** The second number is LEARNED, and
/// `SeriesSlow`'s is DECLARED; rendering this one as "/s" would present an invented expectation
/// in the vocabulary reserved for a real one, which is the whole reason this variant exists
/// rather than a second reading of that one.
#[test]
fn the_family_body_states_counts_and_never_a_cadence() {
    let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
    let fired =
        eval_signal_rule(&rule, &mut RuleState::default(), &collapse("book/polymarket/f"), 1)
            .unwrap();
    assert!(!fired.body.contains("/s"), "no per-second figure may appear: {}", fired.body);
    assert!(
        !fired.body.contains("expected"),
        "and not the word that means a DECLARED cadence: {}",
        fired.body
    );
}

/// **A verdict with NO out-of-family witness says so, rather than asserting one.**
///
/// A recorder that records exactly one family has nothing outside it to ask, so its producer
/// waives the licence rather than being permanently unable to fire. Rendering that through the
/// ordinary sentence would claim "another series kept writing" when none exists — a false
/// diagnosis, in the one field an operator uses to tell a dead family from a stalled process.
#[test]
fn a_verdict_with_no_witness_never_claims_one() {
    let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
    let sig = AlertSignal::FamilyCollapse {
        family: "book/polymarket/btc-updown-5m".to_string(),
        observed_items: 0,
        baseline_items: 22_285,
        window_secs: 30,
        members: 4,
        ring_windows: 20,
        licence: String::new(),
        licence_items: 0,
    };
    let fired = eval_signal_rule(&rule, &mut RuleState::default(), &sig, 1).unwrap();
    assert!(
        fired.body.contains("no other series"),
        "an unwitnessed verdict must say it is unwitnessed: {}",
        fired.body
    );
    assert!(
        !fired.body.contains("still receiving data"),
        "…and must not claim corroboration it does not have: {}",
        fired.body
    );
}

/// The prefix scope reaches a FAMILY key, whose first two segments are a member series key's
/// first two — so one profile setting scopes all three recorder rules the same way.
#[test]
fn the_family_scope_is_a_prefix_over_the_family_key() {
    let rule = AlertRule::new(
        "f",
        RuleTrigger::FamilyCollapse { series_prefix: Some("book/polymarket/".into()) },
    );
    assert!(
        eval_signal_rule(
            &rule,
            &mut RuleState::default(),
            &collapse("book/polymarket/btc-updown-5m"),
            1
        )
        .is_some()
    );
    assert!(
        eval_signal_rule(&rule, &mut RuleState::default(), &collapse("trade/binance/BTCUSDT.P"), 2)
            .is_none(),
        "a scoped rule must not fire for a family outside its scope"
    );
}

/// Cross-talk in both directions, as the two rules beside it already pin: a family signal must
/// not fire a series rule and a series signal must not fire the family rule. Three rule ids,
/// three faults — an operator correlating on one must not receive another.
#[test]
fn a_family_signal_and_rule_do_not_cross_with_the_series_kinds() {
    let fam_rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
    assert!(
        eval_signal_rule(
            &fam_rule,
            &mut RuleState::default(),
            &stale("book/polymarket/0xtok", Some(1), 1),
            1
        )
        .is_none()
    );
    for other in [
        RuleTrigger::SeriesStale { series_prefix: None },
        RuleTrigger::SeriesSlow { series_prefix: None },
    ] {
        let rule = AlertRule::new("o", other);
        assert!(
            eval_signal_rule(
                &rule,
                &mut RuleState::default(),
                &collapse("book/polymarket/btc-updown-5m"),
                2
            )
            .is_none(),
            "a family signal must not fire {:?}",
            rule.trigger
        );
    }
}
