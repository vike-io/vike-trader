//! A bracket is three orders of one size: every priced leg is capped, and it pays the rate token.
use super::*;

/// ⚠ **A bracket is three orders of one size, and every priced leg is capped as a `Submit` at that
/// price would be**, so the node is never weaker for a bracket than for its legs sent one by one.
/// A MARKET entry is unpriced, like a market `Submit`, but its exits are not.
#[test]
fn every_priced_leg_of_a_bracket_is_capped_like_a_submit() {
    let mut l = limits(Some(1_000.0), 1e9);
    assert!(
        l.vet(&bracket(1, 10.0, Some(50.0), 45.0, 60.0), &[], &[]).is_none(),
        "every leg <= 600"
    );
    assert!(
        l.vet(&bracket(1, 10.0, Some(150.0), 145.0, 160.0), &[], &[]).is_some(),
        "every leg breaches"
    );
    assert!(
        l.vet(&bracket(1, 10.0, Some(150.0), 45.0, 60.0), &[], &[]).is_some(),
        "the limit entry alone"
    );
    assert!(
        l.vet(&bracket(1, 10.0, None, 45.0, 120.0), &[], &[]).is_some(),
        "market entry, TP leg: 1200"
    );
    assert!(
        l.vet(&bracket(-1, 10.0, Some(50.0), 120.0, 40.0), &[], &[]).is_some(),
        "a short's SL: 1200"
    );
    assert!(
        l.vet(&bracket(1, -10.0, Some(150.0), 145.0, 160.0), &[], &[]).is_some(),
        "|qty| is used"
    );
    assert!(
        l.vet(&bracket(1, 10.0, Some(-150.0), 45.0, 60.0), &[], &[]).is_some(),
        "|price| is used"
    );
}

/// The refusal NAMES the leg that breached (the largest, when several do), so an operator knows
/// which price to move — and a NaN leg, which `n > max` never selects, cannot hide a finite leg
/// that breaches.
#[test]
fn a_bracket_cap_refusal_names_the_breaching_leg() {
    let mut l = limits(Some(1_000.0), 1e9);
    for (cmd, leg) in [
        (bracket(1, 10.0, Some(150.0), 45.0, 60.0), "entry"),
        (bracket(1, 10.0, None, 45.0, 120.0), "take-profit"),
        (bracket(-1, 10.0, Some(50.0), 120.0, 40.0), "stop-loss"),
        (bracket(1, 10.0, Some(150.0), 145.0, 160.0), "take-profit"),
        (bracket(1, 10.0, Some(f64::NAN), 45.0, 120.0), "take-profit"),
    ] {
        let label = format!("{cmd:?}");
        let msg = l.vet(&cmd, &[], &[]).unwrap_or_else(|| panic!("{label} must be refused"));
        assert!(msg.contains(&format!("the bracket's {leg} leg")), "{label}: {msg}");
        assert!(msg.contains("max_notional_per_order"), "{label}: names the ceiling: {msg}");
    }
}

/// The dry-run answers the same cap and spends no token: two previews on a ONE-token bucket, and
/// the real `vet` after them still finds its token.
#[test]
fn a_bracket_preview_applies_the_same_cap_without_a_token() {
    let mut l = limits(Some(1_000.0), 1.0);
    assert!(l.preview_vet(&bracket(1, 10.0, Some(150.0), 145.0, 160.0), &[], &[]).is_some());
    assert!(l.preview_vet(&bracket(1, 10.0, Some(50.0), 45.0, 60.0), &[], &[]).is_none());
    assert!(
        l.vet(&bracket(1, 10.0, Some(50.0), 45.0, 60.0), &[], &[]).is_none(),
        "the bucket's one token is still there, so neither preview spent it"
    );
}

/// A bracket pays the rate token, like every command: a flood of brackets is still a flood.
#[test]
fn a_bracket_pays_the_rate_token() {
    let mut l = limits(None, 1.0);
    assert!(
        l.vet(&bracket(1, 1.0, None, 1.0, 2.0), &[], &[]).is_none(),
        "the first takes the one token"
    );
    assert!(
        l.vet(&bracket(1, 1.0, None, 1.0, 2.0), &[], &[]).is_some(),
        "the second is rate limited"
    );
}
