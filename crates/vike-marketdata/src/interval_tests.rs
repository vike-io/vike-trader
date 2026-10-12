use super::*;

#[test]
fn interval_ms_units_and_rejects() {
    assert_eq!(interval_ms("30s"), Some(30_000));
    assert_eq!(interval_ms("1s"), Some(1_000));
    assert_eq!(interval_ms("1m"), Some(60_000));
    assert_eq!(interval_ms("5m"), Some(300_000));
    assert_eq!(interval_ms("15m"), Some(900_000));
    assert_eq!(interval_ms("1h"), Some(3_600_000));
    assert_eq!(interval_ms("4h"), Some(14_400_000));
    assert_eq!(interval_ms("1d"), Some(86_400_000));
    // malformed → None
    assert_eq!(interval_ms("m"), None); // len < 2
    assert_eq!(interval_ms("1x"), None); // unknown unit
    assert_eq!(interval_ms("-1m"), None); // non-digit count
    assert_eq!(interval_ms("1.5m"), None); // non-digit count
    assert_eq!(interval_ms("xm"), None); // empty/non-digit count
}

/// [`measures_bar_step`] is [`interval_ms`] with the value dropped — asserted as an
/// EQUIVALENCE rather than by re-listing the vocabulary, so the predicate can never drift from
/// the parser it names, and then pinned on the three spellings
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s bug B is
/// about.
#[test]
fn the_bar_step_predicate_is_the_parser_with_the_value_dropped() {
    for iv in [
        "1m", "5m", "30s", "1h", "4h", "1d", "7d", "1w", "1M", "1mo", "funding", "", "m", "0m",
        "-1m", "1x", "1.5h",
    ] {
        assert_eq!(
            measures_bar_step(iv),
            interval_ms(iv).is_some(),
            "the predicate and the parser disagree about {iv:?}"
        );
    }
}

/// The three spellings bug B is about, named one at a time — a fold over a list would pass over
/// an empty one, and these three are the claim.
///
/// `1w`, `1M` and `1mo` are REAL intervals that three dispatched venues serve, which is why the
/// answer "the guard declines" was right for a garbage string and wrong for these. `7d` is here
/// as the contrast: a week the vocabulary CAN measure, and the step an operator who wants
/// weekly bars from a measurable window should ask for.
#[test]
fn a_week_and_a_month_have_no_bar_width_but_seven_days_does() {
    assert!(!measures_bar_step("1w"), "a week is outside the s/m/h/d vocabulary");
    assert!(!measures_bar_step("1M"), "a calendar month is not a multiple of anything");
    assert!(!measures_bar_step("1mo"), "the two-character month spelling is outside it too");
    assert!(measures_bar_step("7d"), "seven days is a plain multiple and measures fine");
    assert!(
        !measures_bar_step("funding"),
        "the reserved funding-rate label is not a step and must never be read as one"
    );
}
