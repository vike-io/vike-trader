//! `--fail-if EXPR` — the criteria `vike-cli backtest gate` judges a run against, parsed and
//! evaluated. PURE: nothing here opens a file or reads a clock.
//!
//! # The grammar
//!
//! ```text
//! EXPR      := CRITERION (',' CRITERION)*
//! CRITERION := METRIC ':' SIGN NUMBER ['%']
//! SIGN      := '+' | '-'
//! ```
//!
//! The SIGN says which direction is BAD — `-` fails on a fall, `+` fails on a rise — and the number
//! is the TOLERANCE. `%` makes it a share of the baseline; its absence makes it an absolute amount
//! in the metric's own units. `sharpe:-5%,max_dd:+10%` is the design's own example
//! (`docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §7.1).
//!
//! # ⚠ It is RELATIVE ONLY, and `--against` is what pays for that
//!
//! There is no absolute form (`sharpe:>1.0`). A CI step wanting "n_trades must exceed zero" cannot
//! spell it here and falls back to `--json` piped through `jq`; `gate`'s usage line says so. That is
//! an acceptable gap for one grammar, and a second one would double the parser, the evaluator and
//! the verdict renderer at once — while `--against` is REQUIRED anyway, so every gate already holds
//! a baseline.
//!
//! # Four rules that look like details and are not
//!
//! **The sign is REQUIRED.** `sharpe:5%` is refused. A criterion with no direction is a gate
//! checking the wrong side of a number, and it reads green for exactly as long as the metric moves
//! the way you did not care about.
//!
//! **The percentage is of `|baseline|`.** With a baseline of `-0.5`, `baseline * 0.95` is `-0.475` —
//! ABOVE the baseline — so a `-5%` gate built that way would permit a decline and refuse an
//! improvement. A baseline of exactly `0.0` therefore gives a tolerance of `0.0`, and the verdict
//! renders that reason on the row rather than leaving it to be discovered.
//!
//! **The comparison is STRICT, with no epsilon.** A run byte-identical to its baseline has a delta
//! of exactly zero and passes; a `0%` gate catching one ULP is the correct behaviour for the
//! strictest gate the grammar can spell, and an epsilon would make it unwritable.
//!
//! **A metric is resolved against the DOCUMENT, not a roster.**
//! `crates/vike-analytics/src/report.rs`'s `BacktestReport` derives `Serialize` only and this crate
//! does not link that crate at all, so a criterion names a JSON key. Which means it also works on a
//! research run's report, and on every field the run record gains later, with no edit here — and it
//! means a typo cannot be a parse error. It is caught at JUDGEMENT: an unknown metric is
//! [`Outcome::Unevaluated`], and unevaluated is not a pass.

use std::fmt;

/// The `--rank-by` spellings, mapped onto the report keys they name.
///
/// ⚠ Two spellings of one number already ship:
/// `crates/vike-datahub-client/src/flag_vocab.rs`'s `RANK_METRICS` teaches `max_dd`, `return`
/// and `equity` while the report's own keys are
/// `max_drawdown`, `total_return` and `final_equity`. The JSON KEY is canonical — the document is
/// the contract — and the rank spellings are accepted on top, so a user who learned one surface can
/// use the other. `every_alias_names_a_key_a_real_report_carries` holds the table.
const METRIC_ALIASES: &[(&str, &str)] =
    &[("max_dd", "max_drawdown"), ("return", "total_return"), ("equity", "final_equity")];

/// One `METRIC:SIGN NUMBER[%]` criterion.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Criterion {
    /// The metric as the operator SPELLED it — what every message names it by, so a refusal quotes
    /// the command line back rather than an alias resolution the operator never typed.
    pub(crate) metric: String,
    /// The JSON key it resolves to, after [`METRIC_ALIASES`]. Equal to [`Self::metric`] for every
    /// criterion that named a key outright.
    pub(crate) key: String,
    /// `true` when a RISE is what fails (`+`), `false` when a FALL is (`-`).
    pub(crate) rise_is_bad: bool,
    /// The magnitude, always non-negative — the direction is the sign's job and never the number's.
    pub(crate) tolerance: f64,
    /// Whether [`Self::tolerance`] is a share of `|baseline|` rather than an absolute amount.
    pub(crate) percent: bool,
}

impl fmt::Display for Criterion {
    /// The criterion as it was typed, re-rendered from the parse — so a verdict row and the command
    /// line cannot come to disagree about what was asked.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.rise_is_bad { '+' } else { '-' };
        let pct = if self.percent { "%" } else { "" };
        // ⚠ `{}` and not a fixed precision: Rust's `Display` for `f64` is the SHORTEST round-tripping
        // form, so `5.0` prints `5` and `0.2` prints `0.2`. A hand-rolled trim of trailing zeros
        // turns `50` into `5`, which is a criterion that says something else entirely.
        write!(f, "{}:{sign}{}{pct}", self.metric, self.tolerance)
    }
}

/// What one criterion decided.
///
/// ⚠ **The DECLARATION ORDER is the ranking**, and the ranking is the whole safety property:
/// `Breach` beats `Unevaluated` beats `Pass`, so a real failure is never masked by a typo elsewhere
/// in the same expression and an unchecked criterion never reads as a passing one. The same
/// ordered-level-and-`.max()` shape `crates/vike-cli/src/cmd/config_check.rs`'s `Report::worst`
/// uses. `PartialOrd`/`Ord` are DERIVED from that order rather than hand-written, so there is no
/// second table to disagree with the declaration.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Outcome {
    /// Inside the tolerance.
    Pass,
    /// One of the two documents carries no finite number under this key, so nothing was compared.
    /// The string is the sentence the verdict row renders.
    Unevaluated(String),
    /// The metric moved past the tolerance, in the direction the sign declared bad.
    Breach,
}

impl Outcome {
    /// The word a verdict row prints. One spelling, so the human table and the `--json` document
    /// cannot drift.
    pub(crate) fn word(&self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Unevaluated(_) => "unevaluated",
            Outcome::Breach => "breach",
        }
    }
}

/// One criterion, judged, carrying every number the renderer needs so nothing is recomputed.
#[derive(Clone, Debug)]
pub(crate) struct Judgement {
    pub(crate) criterion: Criterion,
    /// The baseline run's value, or `None` when it carries no finite number under the key.
    pub(crate) baseline: Option<f64>,
    /// The judged run's value, same rule.
    pub(crate) value: Option<f64>,
    /// The worst value that would still have passed — `None` when nothing was evaluated.
    pub(crate) allowed: Option<f64>,
    pub(crate) outcome: Outcome,
}

impl Judgement {
    /// `value - baseline`, or `None` when either side is missing.
    pub(crate) fn delta(&self) -> Option<f64> {
        match (self.baseline, self.value) {
            (Some(b), Some(v)) => Some(v - b),
            _ => None,
        }
    }
}

/// Parse a whole `--fail-if` expression. Every failure names what was typed.
pub(crate) fn parse_fail_if(expr: &str) -> Result<Vec<Criterion>, String> {
    if expr.trim().is_empty() {
        return Err(
            "--fail-if takes at least one criterion, e.g. `sharpe:-5%,max_dd:+10%` — METRIC, a \
             colon, a SIGN saying which direction is bad, the tolerance, and an optional `%`"
                .to_string(),
        );
    }
    let mut out: Vec<Criterion> = Vec::new();
    for raw in expr.split(',') {
        let c = parse_one(raw.trim())?;
        // ⚠ A duplicated metric is REFUSED rather than letting the last one win. Two criteria over
        // one number is a command line somebody edited badly, and silently keeping one of them is a
        // gate that checks something other than what it was told to.
        if let Some(prev) = out.iter().find(|p| p.key == c.key) {
            return Err(format!(
                "`{prev}` and `{c}` both judge `{}` — name each metric once; a silently-dropped \
                 criterion is a gate checking something other than what it was given",
                c.key
            ));
        }
        out.push(c);
    }
    Ok(out)
}

fn parse_one(term: &str) -> Result<Criterion, String> {
    if term.is_empty() {
        return Err("--fail-if: an empty criterion — terms are joined by a single comma, e.g. \
             `sharpe:-5%,max_dd:+10%`"
            .to_string());
    }
    let Some((metric, rhs)) = term.split_once(':') else {
        return Err(format!(
            "--fail-if `{term}`: a criterion is METRIC:SIGN NUMBER[%], e.g. `sharpe:-5%` — the \
             colon is missing"
        ));
    };
    let metric = metric.trim();
    if metric.is_empty() {
        return Err(format!("--fail-if `{term}`: the metric before the colon is empty"));
    }
    let rhs = rhs.trim();
    let (rise_is_bad, magnitude) = match rhs.strip_prefix('+') {
        Some(rest) => (true, rest),
        None => match rhs.strip_prefix('-') {
            Some(rest) => (false, rest),
            // ⚠ THE SIGN IS REQUIRED. A criterion with no direction is a gate checking the wrong
            // side of the number, which reads green for as long as the metric moves the way you did
            // not care about.
            None => {
                return Err(format!(
                    "--fail-if `{term}`: the tolerance needs a SIGN saying which direction is bad — \
                     `-` fails on a FALL (`{metric}:-5%`), `+` fails on a RISE (`{metric}:+5%`). \
                     Without one this gate would check a side of the number you did not choose."
                ));
            }
        },
    };
    let (number, percent) = match magnitude.strip_suffix('%') {
        Some(n) => (n, true),
        None => (magnitude, false),
    };
    let tolerance: f64 = number.trim().parse().map_err(|_| {
        format!("--fail-if `{term}`: `{number}` is not a number — the tolerance is the MAGNITUDE")
    })?;
    if !tolerance.is_finite() {
        return Err(format!("--fail-if `{term}`: `{number}` is not a finite tolerance"));
    }
    if tolerance < 0.0 {
        return Err(format!(
            "--fail-if `{term}`: the tolerance is negative — the DIRECTION is the sign's job and \
             the number is the magnitude, so `{metric}:-5%` is what `{term}` was probably meant to \
             be"
        ));
    }
    let key = METRIC_ALIASES
        .iter()
        .find(|(alias, _)| *alias == metric)
        .map_or_else(|| metric.to_string(), |(_, key)| (*key).to_string());
    Ok(Criterion { metric: metric.to_string(), key, rise_is_bad, tolerance, percent })
}

/// Judge one criterion against a baseline and a value.
///
/// The `allowed` bound is computed once and carried on the [`Judgement`], so the renderer prints the
/// number the comparison actually used rather than recomputing one that could differ in the last
/// bit.
pub(crate) fn judge(c: &Criterion, baseline: Option<f64>, value: Option<f64>) -> Judgement {
    let (Some(b), Some(v)) = (baseline, value) else {
        let which = match (baseline.is_some(), value.is_some()) {
            (false, true) => "the baseline run",
            (true, false) => "the judged run",
            _ => "neither run",
        };
        return Judgement {
            criterion: c.clone(),
            baseline,
            value,
            allowed: None,
            outcome: Outcome::Unevaluated(format!(
                "{which} carries no finite number under `{}`",
                c.key
            )),
        };
    };
    // ⚠ OF `|baseline|`, never of `baseline`. With a baseline of -0.5, `b * 0.95` is -0.475 — ABOVE
    // the baseline — so a `-5%` gate built that way permits a decline and refuses an improvement.
    // A baseline of exactly 0.0 therefore gives a tolerance of 0.0 and ANY move in the bad direction
    // breaches; the verdict renders that reason rather than special-casing it away.
    let span = if c.percent { c.tolerance / 100.0 * b.abs() } else { c.tolerance };
    let (allowed, breached) =
        if c.rise_is_bad { (b + span, v > b + span) } else { (b - span, v < b - span) };
    Judgement {
        criterion: c.clone(),
        baseline,
        value,
        allowed: Some(allowed),
        outcome: if breached { Outcome::Breach } else { Outcome::Pass },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(expr: &str) -> Criterion {
        let mut cs = parse_fail_if(expr).unwrap();
        assert_eq!(cs.len(), 1);
        cs.pop().unwrap()
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
}
