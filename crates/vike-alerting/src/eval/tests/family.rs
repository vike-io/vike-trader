//! The `FamilyCollapse` rule: counts never a cadence, the licence witness, scope, cross-talk.

use super::*;
use crate::rule::{AlertRule, RuleTrigger};

// ---- the FAMILY-collapse rule -------------------------------------------------------------

#[test]
fn a_family_collapse_names_the_family_both_counts_and_the_licence() {
    let rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
    let fired = fire_signal(&rule, &collapse("book/polymarket/btc-updown-5m"), 1)
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
    let fired = fire_signal(&rule, &collapse("book/polymarket/f"), 1).unwrap();
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
    let fired = fire_signal(&rule, &sig, 1).unwrap();
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
    assert!(fire_signal(&rule, &collapse("book/polymarket/btc-updown-5m"), 1).is_some());
    assert!(
        fire_signal(&rule, &collapse("trade/binance/BTCUSDT.P"), 2).is_none(),
        "a scoped rule must not fire for a family outside its scope"
    );
}

/// Cross-talk in both directions, as the two rules beside it already pin: a family signal must
/// not fire a series rule and a series signal must not fire the family rule. Three rule ids,
/// three faults — an operator correlating on one must not receive another.
#[test]
fn a_family_signal_and_rule_do_not_cross_with_the_series_kinds() {
    let fam_rule = AlertRule::new("f", RuleTrigger::FamilyCollapse { series_prefix: None });
    assert!(fire_signal(&fam_rule, &stale("book/polymarket/0xtok", Some(1), 1), 1).is_none());
    for other in [
        RuleTrigger::SeriesStale { series_prefix: None },
        RuleTrigger::SeriesSlow { series_prefix: None },
    ] {
        let rule = AlertRule::new("o", other);
        assert!(
            fire_signal(&rule, &collapse("book/polymarket/btc-updown-5m"), 2).is_none(),
            "a family signal must not fire {:?}",
            rule.trigger
        );
    }
}
