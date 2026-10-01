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

use crate::exit::Exit;

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

/// The RUNG a set of judged criteria collapses onto — the ONE mapping from [`Outcome`]'s ranking to
/// `crate::exit::Exit`, for every judging verb in this crate.
///
/// ⚠ It lives HERE, beside the enum whose DECLARATION ORDER is the ranking, rather than in either
/// gate module — and that is a correction. `crates/vike-cli/src/cmd/runs/gate.rs` (the compute
/// plane) and `crates/vike-cli/src/cmd/data/gate.rs` (the data plane) each carried a byte-identical
/// copy, so retuning one plane's mapping left the other on the old one with both suites green,
/// each asserting its own copy. The same argument `Outcome`'s own doc makes against a second
/// three-variant enum applies to a second collapse of it.
///
/// ⚠ Computed with `.max()` over the DERIVED ordering rather than a hand-written if-ladder, so "a
/// breach outranks everything" lives in exactly one place — the order the variants are declared in.
///
/// It takes an ITERATOR of outcomes rather than a slice of judgements because the two planes judge
/// different shapes: `failif::Judgement` carries two `f64`s and a [`Criterion`], the data plane's
/// carries four rendered cells. Each projects its own rows onto this.
pub(crate) fn rung<'a>(outcomes: impl IntoIterator<Item = &'a Outcome>) -> Exit {
    match outcomes.into_iter().max() {
        Some(Outcome::Breach) => Exit::Breach,
        Some(Outcome::Unevaluated(_)) => Exit::Empty,
        Some(Outcome::Pass) => Exit::Ok,
        // Unreachable through either verb — each refuses a line that would declare no criterion —
        // and pinned anyway, because the one thing this rung may never do is answer `0` for
        // "nothing happened".
        None => Exit::Empty,
    }
}

/// The one-word verdict, off the same rung the process exits on — the ONE vocabulary both judging
/// verbs render and both `--json` documents carry, for [`rung`]'s reason.
pub(crate) fn verdict_word(exit: Exit) -> &'static str {
    match exit {
        Exit::Breach => "breach",
        Exit::Empty => "unevaluated",
        _ => "pass",
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

#[path = "failif_tests.rs"]
#[cfg(test)]
mod failif_tests;
