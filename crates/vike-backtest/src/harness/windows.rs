//! Every `[walkforward]` window FORM, resolved into ONE list of [`Split`]s — the single
//! place that answers "which bars is this window's train, and which its test".
//!
//! Three spellings reach it and there is one answer type, deliberately:
//!
//! * `n_splits = N` — delegated verbatim to [`walk_forward_splits`], so a profile written
//!   before the other forms existed resolves to the identical list it always did. `purge`
//!   and `embargo` are REFUSED on this form by
//!   [`super::profile::WalkforwardCfg::window_form`]:
//!   `docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md` is accepted and its
//!   verdict is scoped to exactly this splitter.
//! * `train`/`test`/`step` durations — converted to BAR COUNTS here and delegated to
//!   [`purged_walk_forward_hours`], which already carries the purge and the
//!   expanding-vs-sliding switch (its parameters are index counts despite the `_hours`
//!   naming, which its own doc states).
//! * the same three fields with the `bars` suffix — the same form; the conversion is the
//!   identity.
//!
//! `docs/decisions/0053-purge-and-embargo-on-the-duration-walk-forward.md` is the argument
//! for the two gap knobs on this path, and it does NOT rest on the train/test information
//! leak that 0046 refuted. What a purge guards is a forward-looking LABEL, which the
//! `research` plane's model fits have and a parameter search does not — so on this path both
//! knobs default to zero, and a profile that does not spell them resolves to the window list
//! it always did.
//!
//! # ⚠ A calendar span is anchored ONCE, at the first bar
//!
//! `"12mo"` becomes the exact number of milliseconds from `bars[0].ts` to the same civil
//! date twelve months later ([`add_calendar_months`], which clamps 31 January to 28/29
//! February), divided by the NOMINAL bar step from `data.interval`. The resulting count is
//! then held constant for every window.
//!
//! Two consequences, both accepted and both recorded in 0053. Over a GAPPY series — FX
//! weekends, a venue outage — a `12mo` window holds fewer than twelve months of wall-clock
//! time, because the divisor assumes every step is present. And later windows drift from
//! calendar boundaries, because the count does not recompute per window. The alternative — a
//! splitter that reads `Bar.ts` per window — is the genuinely calendar-aware form, and it
//! wants `WfWindow` to report DATES rather than index ranges, which is a wire-visible change
//! (`crates/vike-cli/src/cmd/walkforward.rs`'s renderer prints integer ranges). That is
//! 0046's own reopener and it is a separate question.
//!
//! # ⚠ A step shorter than the validation window is REFUSED
//!
//! `step < test` makes consecutive validation windows OVERLAP, and
//! [`crate::walkforward::walk_forward_over_windows`] folds every window onto ONE running equity —
//! so an overlapping list compounds the same price history more than once and the stitched OOS
//! return comes back roughly the overlap factor too high, `oos_sharpe` with it. `resolve_windows`
//! refuses it by name. `step == test` is the contiguous default and `step > test` is separation
//! (what `embargo` produces); only the overlapping direction is refused.
//!
//! Reachable only through this form: `walk_forward_splits` sets `test_end = (s + 1) * chunk` and
//! cannot overlap, so before the duration form existed no profile could put an overlapping list in
//! front of that stitch.
//!
//! # ⚠ A window LENGTH floors; a GAP ceils
//!
//! `train`/`test`/`step` round DOWN and `purge`/`embargo` round UP — this module's `Rounding`
//! carries the argument. A length that floors is never longer than asked; a gap that CEILED is
//! never narrower than asked, which is the direction that stays safe if anything with a
//! forward-looking label ever reaches this splitter.
//!
//! # ⚠ What `embargo` means here
//!
//! The minimum gap between one window's `test_end` and the next window's `test_start`,
//! applied by `apply_embargo` as a post-filter that DROPS a window starting inside the zone.
//! Not López de Prado's train-side embargo: on a forward walk every window's train precedes
//! its test, so the only bars a train-side embargo could remove sit in a later window's
//! INTERIOR, and a [`Split`] is a contiguous half-open quadruple with nowhere to put a hole.
//! Dropping rather than SHIFTING is also deliberate — shifting a window would make `step`
//! mean something other than what the operator wrote.

use vike_model::Bar;
use vike_model::time::{Span, add_calendar_months, interval_ms};

use super::HarnessError;
use super::profile::{WalkforwardCfg, WindowForm};
use vike_analytics::validation::{Split, WalkMode, purged_walk_forward_hours, walk_forward_splits};

/// Resolve `cfg`'s window form against this series into the one internal window list.
///
/// `interval` is the profile's `data.interval`, needed only by the duration form and only
/// for the `m`/`h`/`d`/`w`/`mo`/`y` suffixes — a `bars` span is already a count.
///
/// An EMPTY result is not an error here: "this range holds no window" is a fact about the
/// series, and the drivers turn it into a named refusal (`super::walkforward`'s pre-flight).
/// What IS an error here is a span that cannot become a usable count at all.
pub fn resolve_windows(
    cfg: &WalkforwardCfg,
    bars: &[Bar],
    interval: &str,
    mode: WalkMode,
) -> Result<Vec<Split>, HarnessError> {
    match cfg.window_form()? {
        WindowForm::Splits => {
            let k = cfg.n_splits.expect("window_form proved n_splits present");
            // Re-checked rather than trusted: `BacktestProfile::validate` refuses a zero at
            // load, and a hand-built profile never passed through it.
            if k == 0 {
                return Err(HarnessError::Validation(
                    "walkforward.n_splits must be >= 1, got 0".to_string(),
                ));
            }
            Ok(walk_forward_splits(bars.len(), k, mode))
        }
        WindowForm::Duration => {
            let anchor = bars.first().map_or(0, |b| b.ts);
            let train = to_bars(
                cfg.train_span()?.expect("window_form proved train present"),
                anchor,
                interval,
                "train",
                Rounding::Floor,
            )?;
            let test = to_bars(
                cfg.test_span()?.expect("window_form proved test present"),
                anchor,
                interval,
                "test",
                Rounding::Floor,
            )?;
            // Absent `step` tiles the validation windows edge to edge.
            let step = match cfg.step_span()? {
                Some(s) => to_bars(s, anchor, interval, "step", Rounding::Floor)?,
                None => test,
            };
            // ⚠ The one refusal that is about a RELATION between two resolved counts rather than
            // about either on its own — which is why it lives here and not in `window_form`: both
            // sides are durations until this function has an interval and an anchor to convert
            // them against, and `test = "1mo"` beside `step = "2w"` is only 744-against-336 once
            // it does.
            //
            // A step SHORTER than the validation window makes consecutive windows OVERLAP, and
            // `crate::walkforward::walk_forward_over_windows` folds every window onto ONE running
            // equity — so overlapping windows compound the same price history more than once and
            // the stitched OOS return comes back roughly the overlap factor too high, with
            // `oos_sharpe` distorted the same way. This is reachable ONLY through the duration
            // form: `walk_forward_splits` sets `test_end = (s + 1) * chunk` and cannot overlap, so
            // before the duration form existed no profile could put an overlapping list in front
            // of that stitch.
            if step < test {
                return Err(HarnessError::Validation(format!(
                    "walkforward.step resolves to {step} bar(s) and walkforward.test to {test} — \
                     a step shorter than the validation window makes consecutive windows OVERLAP, \
                     and the stitched out-of-sample curve would compound the same bars more than \
                     once, reporting a return and a Sharpe roughly the overlap factor too high. \
                     Set step >= test: equal tiles the windows edge to edge (that is also what an \
                     absent step means), and larger leaves a gap between them."
                )));
            }
            let purge = match cfg.purge_span()? {
                Some(s) => to_bars(s, anchor, interval, "purge", Rounding::Ceil)?,
                None => 0,
            };
            let embargo = match cfg.embargo_span()? {
                Some(s) => to_bars(s, anchor, interval, "embargo", Rounding::Ceil)?,
                None => 0,
            };
            let expanding = mode == WalkMode::Anchored;
            let raw = purged_walk_forward_hours(bars.len(), train, test, step, purge, expanding);
            Ok(apply_embargo(raw, embargo))
        }
    }
}

/// Which way a span rounds when it is not a whole number of bars. The two directions are
/// chosen per FIELD, and the choice is a safety property rather than a taste one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rounding {
    /// A window LENGTH — `train`, `test`, `step`. Rounds DOWN: the error is bounded by one bar
    /// and it only moves geometry, so a window is never longer than what was asked for.
    Floor,
    /// A GAP — `purge`, `embargo`. Rounds UP, and the direction is the point. Flooring a gap
    /// hands back LESS separation than was requested (`purge = "90m"` at `"1h"` would be a
    /// 60-minute gap for a 90-minute ask; `"36h"` at `"1d"` a 24-hour one), which is the
    /// leak-permissive direction.
    ///
    /// ⚠ Not a live leak on this path TODAY, and the distinction is worth keeping: the only
    /// consumer is a parameter search, and
    /// `docs/decisions/0053-purge-and-embargo-on-the-duration-walk-forward.md` argues that a
    /// search has no forward-looking label to protect. It becomes one the moment that record's
    /// first reopener fires — a walked-forward `research` study reaching this splitter — because
    /// then `purge` is sized against a label horizon `H`, and a floored gap
    /// (`floor(H / step) * step < H`) leaves the tail of every training window inside the
    /// horizon. Rounding up costs at most one bar of warm-up and cannot under-separate.
    Ceil,
}

/// One span as a BAR COUNT. `key` names the `[walkforward]` field, so a refusal tells the
/// operator which line to edit; `rounding` says which way a fractional result goes, and the
/// caller picks it per field rather than this function guessing from the name — a sixth field
/// then cannot inherit the wrong direction by being spelled wrong.
///
/// ⚠ The zero refusal below is reachable under `Rounding::Floor` ONLY, and structurally so:
/// ceiling a strictly positive span can never land on zero. So a GAP shorter than one bar is
/// rounded up to one rather than refused, which is the decision
/// `a_gap_shorter_than_one_bar_rounds_up_instead_of_being_refused` pins.
fn to_bars(
    span: Span,
    anchor_ms: i64,
    interval: &str,
    key: &str,
    rounding: Rounding,
) -> Result<usize, HarnessError> {
    let ms = match span {
        // Already a count: no anchor, no interval, no division.
        Span::Bars(n) => return Ok(n),
        Span::Ms(ms) => ms,
        Span::Months(k) => add_calendar_months(anchor_ms, k) - anchor_ms,
    };
    let step_ms = interval_ms(interval).filter(|&s| s > 0).ok_or_else(|| {
        // ⚠ The `> 0` is load-bearing, not belt-and-braces: `interval_ms("0m")` answers
        // `Some(0)`, and `data.interval` is only run through this parser at load when
        // `engine.timeframes` is non-empty — so a `"0m"` interval reaches here and would
        // divide by zero.
        HarnessError::Validation(format!(
            "walkforward.{key} is a duration, so it needs a bar interval to convert against — \
             data.interval {interval:?} is not one (want e.g. \"1m\", \"1h\", \"1d\"), or spell \
             the window with the `bars` suffix"
        ))
    })?;
    let n = match rounding {
        Rounding::Floor => ms / step_ms,
        // `ms` and `step_ms` are both strictly positive here, so the add cannot change sign and
        // the result cannot be negative.
        Rounding::Ceil => (ms + step_ms - 1) / step_ms,
    };
    if n < 1 {
        return Err(HarnessError::Validation(format!(
            "walkforward.{key} is shorter than one {interval} bar — it would resolve to zero \
             bars and change nothing; widen it or use the `bars` suffix"
        )));
    }
    Ok(n as usize)
}

/// Keep a window only when its validation half starts at or after the previous KEPT window's
/// `test_end + embargo`. The first window is always kept; a zero embargo returns the list
/// untouched, so an absent key is byte-identical to the walk before it existed.
fn apply_embargo(windows: Vec<Split>, embargo: usize) -> Vec<Split> {
    if embargo == 0 {
        return windows;
    }
    let mut kept: Vec<Split> = Vec::with_capacity(windows.len());
    let mut floor = 0usize;
    for w in windows {
        if w.test_start >= floor {
            floor = w.test_end.saturating_add(embargo);
            kept.push(w);
        }
    }
    kept
}

#[path = "windows_tests.rs"]
#[cfg(test)]
mod windows_tests;
