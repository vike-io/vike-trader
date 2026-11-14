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

/// Every string these tests name, plus the menu: the guard callers wrote by hand,
/// `interval_ms(iv).filter(|ms| *ms > 0)`, equals the home on each. No string here overflows, so the
/// only answers that differ from a bare [`interval_ms`] are the zero counts.
#[test]
fn the_bar_step_is_the_hand_written_guard_on_every_string_that_does_not_overflow() {
    let named = [
        "1m", "5m", "30s", "1s", "15m", "1h", "4h", "1d", "7d", "1w", "1M", "1mo", "funding", "",
        "m", "0m", "00d", "0s", "01m", "-1m", "+1m", "1x", "1.5h", "1.5m", "xm", "1 m", "60s",
        "1440m", "86400s",
    ];
    for iv in named.into_iter().chain(KLINE_MENU) {
        assert_eq!(bar_step_ms(iv), interval_ms(iv).filter(|ms| *ms > 0), "{iv:?}");
    }
    assert_eq!(interval_ms("0m"), Some(0), "the parser still answers zero");
    assert_eq!(bar_step_ms("0m"), None);
}

/// The overflow class: the widest day count that fits, the first that does not, and a count that
/// parses as an `i64` with no width in range under any unit. `interval_ms` is not called on the
/// wide ones: its product panics under this profile's `overflow-checks`, and wraps in release.
#[test]
fn a_width_that_does_not_fit_an_i64_has_no_bar_step() {
    assert_eq!(bar_step_ms("106751991167d"), Some(106_751_991_167 * 86_400_000));
    for wide in ["106751991168d", "213503982335d", "18014398509481984d", "9223372036854775807s"] {
        assert_eq!(bar_step_ms(wide), None, "{wide}");
        assert_eq!(canonical_bar_step_ms(wide), None, "{wide}");
    }
    assert_eq!(bar_step_ms("9223372036854775808s"), None, "past an i64 it never parsed");
}

/// A writer's check: one spelling per count, the same refusals as the width, and nothing else.
#[test]
fn a_leading_zero_is_a_second_spelling_and_nothing_else_is() {
    for iv in ["01m", "005m", "01d", "00d", "0m"] {
        assert_eq!(canonical_bar_step_ms(iv), None, "{iv}");
    }
    assert_eq!(bar_step_ms("01m"), Some(60_000), "a reader of a stored `01m` keeps its width");
    for iv in KLINE_MENU.into_iter().chain(["3m", "2h", "12h", "7d", "10s", "60s", "1440m"]) {
        assert_eq!(canonical_bar_step_ms(iv), bar_step_ms(iv), "{iv}");
        assert!(canonical_bar_step_ms(iv).is_some(), "{iv}");
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
