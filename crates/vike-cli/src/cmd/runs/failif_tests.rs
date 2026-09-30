use super::*;

fn one(expr: &str) -> Criterion {
    let mut cs = parse_fail_if(expr).unwrap();
    assert_eq!(cs.len(), 1);
    cs.pop().unwrap()
}

/// **The ranking, the rung and the word are ONE decision**, pinned here because both judging
/// verbs now read it from this module rather than each from its own copy.
///
/// ⚠ The load-bearing rows are the last two: a BREACH outranks an unevaluated criterion in
/// EITHER order, so a real failure can never be masked by a probe that could not answer
/// elsewhere in the same run. Asserted both ways round because `.max()` replaced by "the first
/// non-pass wins" would pass one of them.
#[test]
fn every_outcome_set_collapses_onto_one_rung_and_one_word() {
    let unevaluated = || Outcome::Unevaluated("nothing to compare".to_string());
    for (outcomes, exit, word) in [
        (vec![Outcome::Pass, Outcome::Pass], Exit::Ok, "pass"),
        (vec![unevaluated(), Outcome::Pass], Exit::Empty, "unevaluated"),
        (vec![Outcome::Breach, Outcome::Pass], Exit::Breach, "breach"),
        (vec![unevaluated(), Outcome::Breach], Exit::Breach, "breach"),
        (vec![Outcome::Breach, unevaluated()], Exit::Breach, "breach"),
    ] {
        assert_eq!(rung(outcomes.iter()), exit, "{outcomes:?}");
        assert_eq!(verdict_word(rung(outcomes.iter())), word, "{outcomes:?}");
    }
}

/// An EMPTY criterion set is emphatically not a pass. Unreachable through either verb — each
/// refuses a line that declares no criterion — and pinned anyway, because the one thing this
/// rung may never do is answer `0` for "nothing happened".
#[test]
fn no_criteria_at_all_is_not_a_pass() {
    let none: [Outcome; 0] = [];
    assert_eq!(rung(&none), Exit::Empty);
    assert_eq!(verdict_word(rung(&none)), "unevaluated");
}

/// The grammar §7.1 writes, parsed.
#[test]
fn the_examples_from_the_design_parse() {
    let cs = parse_fail_if("sharpe:-5%,max_dd:+10%").unwrap();
    assert_eq!(cs.len(), 2);
    assert_eq!(cs[0].metric, "sharpe");
    assert!(!cs[0].rise_is_bad, "a `-` means a FALL is what fails");
    assert_eq!(cs[0].tolerance, 5.0);
    assert!(cs[0].percent);
    assert_eq!(
        cs[1].key, "max_drawdown",
        "`max_dd` is the --rank-by spelling; the KEY is canonical"
    );
    assert_eq!(cs[1].metric, "max_dd", "…and the SPELLING is what a message quotes back");
    assert!(cs[1].rise_is_bad);
}

/// An ABSOLUTE tolerance is the `%`-less form, in the metric's own units.
#[test]
fn a_tolerance_with_no_percent_is_absolute() {
    let c = one("sharpe:-0.2");
    assert_eq!(c.tolerance, 0.2);
    assert!(!c.percent);
    let j = judge(&c, Some(1.0), Some(0.85));
    assert_eq!(j.allowed.unwrap(), 0.8);
    assert_eq!(j.outcome, Outcome::Pass, "0.85 is inside an absolute 0.2 of 1.0");
    assert_eq!(judge(&c, Some(1.0), Some(0.79)).outcome, Outcome::Breach);
}

/// ⚠ THE SIGN IS REQUIRED. A criterion with no direction is a gate checking the wrong side of
/// the number, which reads green for as long as the metric moves the way you did not care about.
#[test]
fn a_criterion_with_no_direction_is_refused() {
    let err = parse_fail_if("sharpe:5%").unwrap_err();
    assert!(err.contains('+') && err.contains('-'), "names both signs: {err}");
}

/// Every other way the expression can be wrong, each named.
#[test]
fn the_refusals_name_what_was_typed() {
    for bad in ["", "sharpe", "sharpe:", ":+5%", "sharpe:+abc%", "sharpe:+-5%", "sharpe:+nan"] {
        let err = parse_fail_if(bad).unwrap_err();
        assert!(!err.is_empty(), "`{bad}` must be refused with a reason");
    }
    // A duplicated metric is refused rather than silently letting the last one win.
    let err = parse_fail_if("sharpe:-5%,sharpe:-10%").unwrap_err();
    assert!(err.contains("sharpe"), "{err}");
    // ⚠ …including when the two spellings are an ALIAS PAIR. `max_dd` and `max_drawdown` are one
    // number, and a gate silently judging it twice against two tolerances is the same defect
    // wearing a second name.
    let err = parse_fail_if("max_dd:+5%,max_drawdown:+10%").unwrap_err();
    assert!(err.contains("max_drawdown"), "{err}");
    // A NEGATIVE tolerance is a sign typed twice; the direction is the sign's job.
    assert!(parse_fail_if("sharpe:--5%").is_err());
}

/// ⚠ THE PERCENTAGE IS OF `|baseline|`. With a negative baseline, `baseline * 0.95` is ABOVE the
/// baseline — a `-5%` gate would then permit a decline and refuse an improvement.
#[test]
fn a_negative_baseline_does_not_invert_the_tolerance() {
    let c = one("sharpe:-5%");
    let j = judge(&c, Some(-0.5), Some(-0.6));
    assert_eq!(j.outcome, Outcome::Breach, "a fall from -0.5 to -0.6 is a fall");
    assert_eq!(j.allowed.unwrap(), -0.525);

    let j = judge(&c, Some(-0.5), Some(-0.51));
    assert_eq!(j.outcome, Outcome::Pass, "…and a fall inside the tolerance is not");

    // The mutation this test exists against: `b * (1.0 - 5/100)` is -0.475, so the SAME two
    // cases would come back inverted.
    assert!(j.allowed.unwrap() < -0.5, "the floor is BELOW the baseline, not above it");
}

/// A ZERO baseline makes a percentage tolerance zero, so any move in the bad direction breaches.
/// Declared rather than special-cased — the verdict renders the reason.
#[test]
fn a_zero_baseline_gives_a_zero_tolerance() {
    let c = one("sharpe:-5%");
    assert_eq!(judge(&c, Some(0.0), Some(-0.000_1)).outcome, Outcome::Breach);
    assert_eq!(judge(&c, Some(0.0), Some(0.0)).outcome, Outcome::Pass);
    assert_eq!(judge(&c, Some(0.0), Some(1.0)).outcome, Outcome::Pass, "a RISE is not a fall");
}

/// The boundary PASSES: strict comparison, no epsilon. A run byte-identical to its baseline must
/// be green under a `0%` gate, and a `0%` gate must still be able to catch one ULP.
#[test]
fn exactly_at_the_boundary_passes() {
    let c = one("sharpe:-10%");
    assert_eq!(judge(&c, Some(1.0), Some(0.9)).outcome, Outcome::Pass);
    let zero = one("sharpe:-0%");
    assert_eq!(judge(&zero, Some(1.0), Some(1.0)).outcome, Outcome::Pass);
    assert_eq!(judge(&zero, Some(1.0), Some(0.999_999_999)).outcome, Outcome::Breach);
}

/// A metric MISSING from either document is UNEVALUATED, and unevaluated is not a pass.
/// `profit_factor` serializes as JSON `null` when non-finite, so this is a real document shape
/// rather than a hypothetical one.
#[test]
fn a_metric_absent_from_either_side_is_unevaluated() {
    let c = one("profit_factor:-5%");
    assert!(matches!(judge(&c, None, Some(2.0)).outcome, Outcome::Unevaluated(_)));
    assert!(matches!(judge(&c, Some(2.0), None).outcome, Outcome::Unevaluated(_)));
    match judge(&c, None, Some(2.0)).outcome {
        Outcome::Unevaluated(why) => {
            assert!(why.contains("profit_factor"), "{why}");
            assert!(why.contains("baseline"), "…and WHICH side was missing: {why}");
        }
        other => panic!("{other:?}"),
    }
    match judge(&c, Some(2.0), None).outcome {
        Outcome::Unevaluated(why) => assert!(why.contains("judged run"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// ⚠ **`Breach` beats `Unevaluated` beats `Pass`**, and the ordering is DERIVED from the
/// declaration order rather than written a second time. A real failure masked by a typo
/// elsewhere in the same expression, or an unchecked criterion reading as a passing one, are the
/// two failures this ordering exists to make impossible.
#[test]
fn the_outcome_ranking_puts_a_breach_above_everything() {
    assert!(Outcome::Breach > Outcome::Unevaluated(String::new()));
    assert!(Outcome::Unevaluated(String::new()) > Outcome::Pass);
    let worst = [Outcome::Pass, Outcome::Breach, Outcome::Unevaluated("x".into())]
        .into_iter()
        .max()
        .unwrap();
    assert_eq!(worst, Outcome::Breach);
}

/// A criterion re-renders as what was typed, so a verdict row and the command line cannot come
/// to disagree about what was asked.
#[test]
fn a_criterion_renders_back_as_the_operator_spelled_it() {
    assert_eq!(one("sharpe:-5%").to_string(), "sharpe:-5%");
    assert_eq!(one("max_dd:+10%").to_string(), "max_dd:+10%");
    assert_eq!(one("sharpe:-0.2").to_string(), "sharpe:-0.2");
    // ⚠ The case a trailing-zero trim gets wrong: `50` is not `5`.
    assert_eq!(one("n_trades:-50").to_string(), "n_trades:-50");
}

/// Every alias must name a key a real report carries. This crate cannot build a `BacktestReport`
/// — it does not link `vike-analytics`, deliberately — so the fixture is the DOCUMENT, which is
/// the contract anyway.
#[test]
fn every_alias_names_a_key_a_real_report_carries() {
    let report: serde_json::Value = serde_json::from_str(
        r#"{"name":"m","final_equity":10500.0,"total_return":0.05,"n_trades":412,
                "win_rate":0.51,"sharpe":1.8,"max_drawdown":0.12,"profit_factor":1.4,
                "funding_paid":0.0,"per_symbol_pnl":[]}"#,
    )
    .unwrap();
    for (alias, key) in METRIC_ALIASES {
        assert!(
            report.get(key).is_some(),
            "the alias `{alias}` points at `{key}`, which no report carries"
        );
        assert!(
            report.get(alias).is_none(),
            "`{alias}` is also a real report key, so the alias SHADOWS it — pick one"
        );
    }
}
