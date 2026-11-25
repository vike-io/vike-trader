//! Every `[walkforward]` window FORM, resolved into ONE list of [`WalkforwardSplit`]s — the single
//! place that answers "which bars is this window's train, and which its test".
//!
//! Three spellings reach it and there is one answer type, deliberately:
//!
//! * `n_splits = N` — delegated verbatim to [`walkforward_splits`], so a profile written
//!   before the other forms existed resolves to the identical list it always did. `purge`
//!   and `embargo` are REFUSED on this form by
//!   [`super::profile::ProfileWalkforward::window_form`]:
//!   `docs/decisions/0046-the-bar-mode-walk-forward-has-no-purge.md` is accepted and its
//!   verdict is scoped to exactly this splitter.
//! * `train`/`test`/`step` durations — converted to BAR COUNTS here and delegated to
//!   [`purged_walk_forward_hours`], which already carries the purge and the
//!   expanding-vs-sliding switch (its parameters are index counts despite the `_hours`
//!   naming, which its own doc states).
//! * the same three fields with the `bars` suffix — the same form; the conversion is the
//!   identity.
//!
//! The fourth form, `groups`/`test_groups` (the combinatorial walk), is not a window list at all:
//! [`resolve_windows`] refuses it and [`combinatorial_splits`] resolves it for
//! `super::combinatorial`, its `embargo` rounding up like every gap here.
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
//! wants `WalkforwardResultsStep` to report DATES rather than index ranges, which is a wire-visible change
//! (`crates/vike-cli/src/cmd/backtest/render/walkforward.rs`'s renderer prints integer ranges). That is
//! 0046's own reopener and it is a separate question.
//!
//! # ⚠ A step shorter than the validation window is REFUSED
//!
//! `step < test` makes consecutive validation windows OVERLAP, and
//! [`crate::walkforward::walk_forward_over_windows`] folds every window onto ONE running equity —
//! so an overlapping list compounds the same price history more than once and the stitched OOS
//! return comes back roughly the overlap factor too high, its Sharpe with it. `resolve_windows`
//! refuses it by name. `step == test` is the contiguous default and `step > test` is separation
//! (what `embargo` produces); only the overlapping direction is refused.
//!
//! Reachable only through this form: `walkforward_splits` sets `test_end = (s + 1) * chunk` and
//! cannot overlap, so before the duration form existed no profile could put an overlapping list in
//! front of that stitch.
//!
//! # ⚠ A window LENGTH floors; a GAP ceils
//!
//! `train`/`test`/`step` round DOWN and `purge`/`embargo` round UP — this module's `WalkforwardRounding`
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
//! INTERIOR, and a [`WalkforwardSplit`] is a contiguous half-open quadruple with nowhere to put a hole.
//! Dropping rather than SHIFTING is also deliberate — shifting a window would make `step`
//! mean something other than what the operator wrote.

use vike_marketdata::Bar;
use vike_marketdata::interval::bar_step_ms;
use vike_model::time::{Span, add_calendar_months};

use super::combinatorial::CombinatorialSplits;
use crate::HarnessError;
use crate::run::profile::{ProfileWalkforward, WalkforwardWindowForm};
use crate::run::refusals::ProfileRefusal;
use vike_analytics::validation::{
    WalkforwardMode, WalkforwardSplit, purged_walk_forward_hours, walkforward_splits,
};

/// Resolve `config`'s window form against this series into the one internal window list.
///
/// `interval` is the profile's `data.interval`, needed only by the duration form and only
/// for the `m`/`h`/`d`/`w`/`mo`/`y` suffixes — a `bars` span is already a count.
///
/// An EMPTY result is not an error here: "this range holds no window" is a fact about the
/// series, and the drivers turn it into a named refusal (`super::runner`'s pre-flight).
/// What IS an error here is a span that cannot become a usable count at all.
pub fn resolve_windows(
    config: &ProfileWalkforward,
    bars: &[Bar],
    interval: &str,
    mode: WalkforwardMode,
) -> Result<Vec<WalkforwardSplit>, HarnessError> {
    let anchor = bars.first().map_or(0, |b| b.timestamp);
    resolve_windows_on(config, bars.len(), anchor, WalkforwardStep::Interval(interval), mode)
}

/// What one step of the axis a window list is cut on is: a bar of `data.interval` (a bar walk), or one seed bar
/// of a tick walk (`engine.seed_bar_interval_ms` milliseconds, `super::ticks`), whose `bars` suffix counts them.
#[derive(Debug, Clone, Copy)]
pub(crate) enum WalkforwardStep<'a> {
    Interval(&'a str),
    SeedMs(i64),
}

impl WalkforwardStep<'_> {
    /// The step as the refusals name it: the interval, or the seed bar's width.
    fn unit(self) -> String {
        match self {
            WalkforwardStep::Interval(interval) => interval.to_string(),
            WalkforwardStep::SeedMs(ms) => format!("{ms} ms seed"),
        }
    }
}

/// [`resolve_windows`] over an axis of `len` steps from `anchor_ms`, each `step` long: a bar series, or a tick
/// walk's seed-bar grid.
pub(crate) fn resolve_windows_on(
    config: &ProfileWalkforward,
    len: usize,
    anchor: i64,
    interval: WalkforwardStep<'_>,
    mode: WalkforwardMode,
) -> Result<Vec<WalkforwardSplit>, HarnessError> {
    match config.window_form()? {
        WalkforwardWindowForm::Splits => {
            let k = config.n_splits.expect("window_form proved n_splits present");
            // Re-checked rather than trusted: `ProfileBacktest::validate` refuses a zero at
            // load, and a hand-built profile never passed through it.
            if k == 0 {
                return Err(HarnessError::from(ProfileRefusal::WalkforwardSplitsBelowOne));
            }
            // `walkforward_splits` cuts the series into `k + 1` equal chunks (the first trains the
            // first window, one per window after it), so fewer bars than chunks makes every chunk
            // EMPTY. Refused before the splitter runs: it would allocate and loop `k` windows of no
            // bars, and a client-supplied `k` of a billion is a billion of them.
            if len < k.saturating_add(1) {
                return Err(splits_too_many(len, k, 0, false));
            }
            Ok(walkforward_splits(len, k, mode))
        }
        WalkforwardWindowForm::Duration => {
            let train = to_bars(
                config.train_span()?.expect("window_form proved train present"),
                anchor,
                interval,
                "train",
                WalkforwardRounding::Floor,
            )?;
            let test = to_bars(
                config.test_span()?.expect("window_form proved test present"),
                anchor,
                interval,
                "test",
                WalkforwardRounding::Floor,
            )?;
            // Absent `step` tiles the validation windows edge to edge.
            let step = match config.step_span()? {
                Some(s) => to_bars(s, anchor, interval, "step", WalkforwardRounding::Floor)?,
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
            // its Sharpe distorted the same way. This is reachable ONLY through the duration
            // form: `walkforward_splits` sets `test_end = (s + 1) * chunk` and cannot overlap, so
            // before the duration form existed no profile could put an overlapping list in front
            // of that stitch.
            if step < test {
                return Err(HarnessError::from(ProfileRefusal::WalkforwardStepShorterThanTest {
                    step,
                    test,
                }));
            }
            let purge = match config.purge_span()? {
                Some(s) => to_bars(s, anchor, interval, "purge", WalkforwardRounding::Ceil)?,
                None => 0,
            };
            let embargo = match config.embargo_span()? {
                Some(s) => to_bars(s, anchor, interval, "embargo", WalkforwardRounding::Ceil)?,
                None => 0,
            };
            let expanding = mode == WalkforwardMode::Anchored;
            let raw = purged_walk_forward_hours(len, train, test, step, purge, expanding);
            Ok(apply_embargo(raw, embargo))
        }
        WalkforwardWindowForm::Combinatorial => {
            Err(HarnessError::from(ProfileRefusal::WalkforwardCombinatorialNotStitched))
        }
    }
}

/// The refusal of `n_splits = k` over `n` bars whose chunks (`n / (k + 1)` bars each) hold no more
/// than `warmup` bars, so a window would decide nothing: the numbers, and the largest `n_splits`
/// this range can carry. Spelled once for both places that refuse it, `resolve_windows` (an EMPTY
/// chunk) and `super::runner`'s pre-flight (a chunk inside the warm-up). `searched`: the warm-up is
/// the grid point's that warms up least (`super::runner::walk_warmup`).
pub(crate) fn splits_too_many(n: usize, k: usize, warmup: usize, searched: bool) -> HarnessError {
    let chunks = k.saturating_add(1);
    let need = warmup.saturating_add(1);
    let why = if warmup == 0 {
        String::from("a window needs at least 1 bar")
    } else {
        format!(
            "a window needs at least {need} bars: its engine starts afresh and decides nothing in its \
             first {warmup} warm-up bar(s) (data.warmup, else the strategy's own{})",
            super::runner::warmup_point_words(searched)
        )
    };
    let most = (n / need).saturating_sub(1);
    let fix = if most >= 1 {
        format!("Set n_splits to at most {most}, or widen data.from/data.to")
    } else {
        format!(
            "No n_splits fits this range: one split needs at least {} bars; widen data.from/data.to",
            need.saturating_mul(2)
        )
    };
    HarnessError::from(ProfileRefusal::WalkforwardSplitsTooMany {
        k,
        n,
        chunks,
        chunk_bars: n / chunks,
        why,
        fix,
    })
}

/// Form (d) resolved against this series: `groups` and `test_groups` as declared, and `embargo`
/// as a bar count that ROUNDS UP (a gap, `WalkforwardRounding::Ceil`), zero when absent.
/// Refused for any other form, so a caller cannot walk a split-count table combinatorially.
pub fn combinatorial_splits(
    config: &ProfileWalkforward,
    bars: &[Bar],
    interval: &str,
) -> Result<CombinatorialSplits, HarnessError> {
    let anchor = bars.first().map_or(0, |b| b.timestamp);
    combinatorial_splits_on(config, anchor, WalkforwardStep::Interval(interval))
}

/// [`combinatorial_splits`] over an axis from `anchor` whose step is `interval` (a tick walk's seed bar).
pub(crate) fn combinatorial_splits_on(
    config: &ProfileWalkforward,
    anchor: i64,
    interval: WalkforwardStep<'_>,
) -> Result<CombinatorialSplits, HarnessError> {
    if config.window_form()? != WalkforwardWindowForm::Combinatorial {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardCombinatorialNeedsGroups));
    }
    let embargo_bars = match config.embargo_span()? {
        Some(s) => to_bars(s, anchor, interval, "embargo", WalkforwardRounding::Ceil)?,
        None => 0,
    };
    Ok(CombinatorialSplits {
        groups: config.groups.expect("window_form proved groups present"),
        test_groups: config.test_groups.expect("window_form proved test_groups present"),
        embargo_bars,
    })
}

/// Which way a span rounds when it is not a whole number of bars. The two directions are
/// chosen per FIELD, and the choice is a safety property rather than a taste one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkforwardRounding {
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

/// The longest calendar span a `[walkforward]` field may name: ten thousand years.
const MAX_SPAN_MONTHS: i64 = 12 * 10_000;

/// One span as a BAR COUNT. `key` names the `[walkforward]` field, so a refusal tells the
/// operator which line to edit; `rounding` says which way a fractional result goes, and the
/// caller picks it per field rather than this function guessing from the name — a sixth field
/// then cannot inherit the wrong direction by being spelled wrong.
///
/// ⚠ The zero refusal below is reachable under `WalkforwardRounding::Floor` ONLY, and structurally so:
/// ceiling a strictly positive span can never land on zero. So a GAP shorter than one bar is
/// rounded up to one rather than refused, which is the decision
/// `a_gap_shorter_than_one_bar_rounds_up_instead_of_being_refused` pins.
fn to_bars(
    span: Span,
    anchor_ms: i64,
    step: WalkforwardStep<'_>,
    key: &str,
    rounding: WalkforwardRounding,
) -> Result<usize, HarnessError> {
    let interval = step.unit();
    let ms = match span {
        // Already a count: no anchor, no interval, no division.
        Span::Bars(n) => return Ok(n),
        Span::Ms(ms) => ms,
        // The calendar arithmetic overflows an `i64` of milliseconds around 300 million years, and
        // no series is a fraction of `MAX_SPAN_MONTHS` long, so a span past it is refused, not walked.
        Span::Months(k) if k > MAX_SPAN_MONTHS => {
            return Err(HarnessError::from(ProfileRefusal::WalkforwardSpanMonthsTooLong {
                key: key.to_string(),
                k,
                years: MAX_SPAN_MONTHS / 12,
            }));
        }
        Span::Months(k) => add_calendar_months(anchor_ms, k) - anchor_ms,
    };
    // A tick walk's seed width is positive: its pre-flight refused it otherwise.
    let step_ms = match step {
        WalkforwardStep::SeedMs(ms) => ms,
        WalkforwardStep::Interval(interval) => bar_step_ms(interval).ok_or_else(|| {
            // ⚠ `bar_step_ms` is load-bearing here: `data.interval` is only run through the parser at
            // load when `engine.timeframes` is non-empty, so a `"0m"` interval reaches here and would
            // divide by zero.
            HarnessError::from(ProfileRefusal::WalkforwardSpanNeedsBarInterval {
                key: key.to_string(),
                interval: interval.to_string(),
            })
        })?,
    };
    let n = match rounding {
        WalkforwardRounding::Floor => ms / step_ms,
        // `ms` and `step_ms` are both strictly positive here, so the add cannot change sign and
        // the result cannot be negative; within `step_ms` of `i64::MAX` it does not fit.
        WalkforwardRounding::Ceil => match ms.checked_add(step_ms - 1) {
            Some(ceiled) => ceiled / step_ms,
            None => {
                return Err(HarnessError::from(
                    ProfileRefusal::WalkforwardSpanMillisecondsTooLong {
                        key: key.to_string(),
                        ms,
                        interval: interval.clone(),
                    },
                ));
            }
        },
    };
    if n < 1 {
        return Err(HarnessError::from(ProfileRefusal::WalkforwardSpanShorterThanOneBar {
            key: key.to_string(),
            interval,
        }));
    }
    Ok(n as usize)
}

/// Keep a window only when its validation half starts at or after the previous KEPT window's
/// `test_end + embargo`. The first window is always kept; a zero embargo returns the list
/// untouched, so an absent key is byte-identical to the walk before it existed.
fn apply_embargo(windows: Vec<WalkforwardSplit>, embargo: usize) -> Vec<WalkforwardSplit> {
    if embargo == 0 {
        return windows;
    }
    let mut kept: Vec<WalkforwardSplit> = Vec::with_capacity(windows.len());
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
