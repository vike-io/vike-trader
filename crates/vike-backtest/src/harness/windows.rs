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
use crate::validation::{Split, WalkMode, purged_walk_forward_hours, walk_forward_splits};

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

#[cfg(test)]
mod tests {
    use super::*;

    const STEP_MS: i64 = 3_600_000; // 1h
    const INTERVAL: &str = "1h";

    /// A flat OHLC bar at `ts` — the shape `crate::walkforward`'s own fixture builds, minus the
    /// trend: nothing here runs a strategy, so only the TIMESTAMP is load-bearing (it is what a
    /// calendar span is anchored at).
    fn flat_bar(ts: i64) -> Bar {
        Bar {
            ts,
            open: 100.0,
            high: 100.0,
            low: 100.0,
            close: 100.0,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// N hourly bars starting at 2026-01-01T00:00:00Z, so a calendar span has a real anchor.
    fn bars(n: usize) -> Vec<Bar> {
        let t0 = vike_model::time::days_from_civil(2026, 1, 1) * 86_400_000;
        (0..n).map(|i| flat_bar(t0 + i as i64 * STEP_MS)).collect()
    }

    fn cfg(toml: &str) -> WalkforwardCfg {
        toml::from_str(toml).expect("the fixture parses")
    }

    /// The split-count form is byte-identical to the splitter it always used — the window
    /// list is a NEW route to the SAME walk, not a new walk.
    #[test]
    fn the_split_count_form_is_byte_identical_to_the_old_splitter() {
        let b = bars(500);
        let got = resolve_windows(&cfg("n_splits = 4"), &b, INTERVAL, WalkMode::Anchored).unwrap();
        assert_eq!(got, walk_forward_splits(500, 4, WalkMode::Anchored));
        let rolling =
            resolve_windows(&cfg("n_splits = 4"), &b, INTERVAL, WalkMode::Rolling).unwrap();
        assert_eq!(rolling, walk_forward_splits(500, 4, WalkMode::Rolling));
    }

    /// A calendar span resolves against the BAR INTERVAL, anchored at the first bar. January
    /// 2026 is 31 days = 744 hourly bars — a different number from `30d`, which is the whole
    /// reason `mo` exists.
    #[test]
    fn a_calendar_train_resolves_against_the_bar_interval() {
        let b = bars(3000);
        let w = resolve_windows(
            &cfg("train = \"1mo\"\ntest = \"168h\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(w[0].train_start, 0);
        assert_eq!(w[0].train_end, 744); // 31 days of hourly bars
        assert_eq!(w[0].test_start, 744); // no purge ⇒ contiguous
        assert_eq!(w[0].test_end, 744 + 168);
        assert_eq!(w[1].test_start, w[0].test_end); // step defaults to test
    }

    /// `bars` needs neither an anchor nor an interval — it IS the count.
    #[test]
    fn the_bars_suffix_needs_no_interval_and_no_anchor() {
        let b = bars(1000);
        let w = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\nstep = \"200bars\""),
            &b,
            INTERVAL,
            WalkMode::Rolling,
        )
        .unwrap();
        assert_eq!(w[0], Split { train_start: 0, train_end: 400, test_start: 400, test_end: 500 });
        assert_eq!(w[1].train_start, 200); // sliding, by step
        assert_eq!(w[1].train_end, 600);
        assert_eq!(w[1].test_start, 600);
    }

    /// `purge` opens EXACTLY its gap: half-open boundaries make
    /// `test_start - train_end == purge` by construction.
    #[test]
    fn purge_opens_exactly_its_gap_between_train_and_test() {
        let b = bars(1000);
        let w = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\npurge = \"8h\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(w[0].train_end, 400);
        assert_eq!(w[0].test_start, 408);
        assert_eq!(w[0].test_end, 508);
    }

    /// `embargo` DROPS a window whose validation half starts inside the zone after the last
    /// KEPT window, and always keeps the first. Dropping rather than shifting is the design:
    /// shifting would make `step` mean something other than what was written.
    #[test]
    fn embargo_drops_a_window_that_starts_inside_the_zone() {
        let b = bars(1000);
        let plain = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\nstep = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        let embargoed = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\nstep = \"100bars\"\n\
                 embargo = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert!(embargoed.len() < plain.len(), "{} vs {}", embargoed.len(), plain.len());
        assert_eq!(embargoed[0], plain[0]);
        for pair in embargoed.windows(2) {
            assert!(pair[1].test_start >= pair[0].test_end + 100, "{pair:?}");
        }
    }

    /// An ABSENT embargo is the IDENTITY, so every profile written before the knob existed
    /// resolves to the same window list.
    #[test]
    fn an_absent_embargo_is_the_identity() {
        let b = bars(1000);
        let a = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(a, purged_walk_forward_hours(1000, 400, 100, 100, 0, true));
    }

    /// A WINDOW LENGTH shorter than one bar is refused rather than rounded to zero, and the
    /// message names the key and the interval.
    #[test]
    fn a_window_length_shorter_than_one_bar_is_refused() {
        let b = bars(1000);
        let e = resolve_windows(
            &cfg("train = \"30m\"\ntest = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap_err();
        let msg = format!("{e}");
        assert!(msg.contains("walkforward.train") && msg.contains("1h"), "{msg}");
    }

    /// A window LENGTH floors and a GAP CEILS — opposite directions, and the direction is the
    /// point. Flooring a length loses at most one bar of geometry; flooring a GAP hands back
    /// LESS separation than was asked for, which is the leak-permissive direction the moment
    /// anything with a forward-looking label reaches this splitter.
    ///
    /// At `"1h"`: `train = "90m"` is one bar (floor of 1.5), `purge = "90m"` is two (ceil).
    #[test]
    fn a_window_floors_and_a_gap_ceils() {
        let b = bars(1000);
        let w = resolve_windows(
            &cfg("train = \"90m\"\ntest = \"100bars\"\npurge = \"90m\"\nembargo = \"90m\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(w[0].train_end, 1, "90m of 1h bars FLOORS to one bar as a length");
        assert_eq!(w[0].test_start, 3, "…and CEILS to two as a gap: train_end 1 + purge 2");
    }

    /// …and the CEIL rule is pinned for `embargo` too, not only for `purge`.
    ///
    /// ⚠ This test exists because every other embargo test in this file is BLIND to the rounding
    /// direction, and flipping `embargo`'s call site to `Rounding::Floor` left all of them — and
    /// every `walkforward` and `walkforward_windows` test — green.
    /// `embargo_drops_a_window_that_starts_inside_the_zone` spells `"100bars"`, and a
    /// `Span::Bars` returns from `to_bars` before any rounding happens;
    /// `an_absent_embargo_is_the_identity` sets none; and
    /// `a_window_floors_and_a_gap_ceils` does pass `embargo = "90m"` but asserts only on window
    /// ZERO, which `apply_embargo` always keeps.
    ///
    /// So the fixture has to be built for it: `step = "101bars"` against `test = "100bars"` leaves
    /// consecutive raw windows separated by EXACTLY ONE bar, which is the only separation at which
    /// a 1-bar embargo and a 2-bar embargo disagree. `"90m"` at `"1h"` is 1.5 bars — 1 floored,
    /// 2 ceiled — so the KEPT-WINDOW COUNT is what tells the two apart.
    #[test]
    fn the_gap_ceil_rule_is_pinned_for_embargo_as_well() {
        let b = bars(1000);
        let base = "train = \"400bars\"\ntest = \"100bars\"\nstep = \"101bars\"";

        let raw = resolve_windows(&cfg(base), &b, INTERVAL, WalkMode::Anchored).unwrap();
        assert_eq!(raw.len(), 5, "the fixture's unembargoed walk: {raw:?}");
        for pair in raw.windows(2) {
            assert_eq!(
                pair[1].test_start,
                pair[0].test_end + 1,
                "the separation this test turns on is exactly one bar: {pair:?}"
            );
        }

        let embargoed = resolve_windows(
            &cfg(&format!("{base}\nembargo = \"90m\"")),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(
            embargoed.len(),
            3,
            "90m at 1h CEILS to 2 bars, which exceeds the 1-bar separation and drops every other \
             window; FLOORING it to 1 bar would keep all 5 — got {embargoed:?}"
        );
        assert_eq!(
            embargoed,
            vec![raw[0], raw[2], raw[4]],
            "…and it is the alternating subsequence, not some other three"
        );
    }

    /// A `data.interval` that parses but is ZERO-LENGTH is refused rather than dividing by zero.
    ///
    /// ⚠ `vike_model::time::interval_ms("0m")` answers `Some(0)`, so the interval IS parseable and
    /// only the `> 0` filter in `to_bars` stands between it and `ms / 0`. `BacktestProfile::validate`
    /// does not close this — it runs `data.interval` through that parser only when
    /// `engine.timeframes` is non-empty — so the guard is the whole of the protection on the
    /// `pub fn resolve_windows` seam, and this case is what stops a later tidy-up from deleting it
    /// green.
    #[test]
    fn a_zero_length_interval_is_refused_rather_than_dividing_by_zero() {
        let b = bars(1000);
        let e =
            resolve_windows(&cfg("train = \"30d\"\ntest = \"7d\""), &b, "0m", WalkMode::Anchored)
                .unwrap_err();
        let msg = format!("{e}");
        assert!(msg.contains("walkforward.train") && msg.contains("\"0m\""), "{msg}");
    }

    /// A GAP shorter than one bar is therefore ROUNDED UP rather than refused — the `< 1`
    /// refusal above is structurally unreachable for `purge`/`embargo`, because ceiling a
    /// positive span can never reach zero. Pinned so the asymmetry is a decision on the record
    /// rather than a hole somebody closes by "fixing" the refusal to cover both.
    #[test]
    fn a_gap_shorter_than_one_bar_rounds_up_instead_of_being_refused() {
        let b = bars(1000);
        let w = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\npurge = \"30m\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(w[0].test_start - w[0].train_end, 1, "30m at 1h is one whole bar of gap");
    }

    /// ⚠ A `step` SHORTER than `test` makes consecutive validation windows OVERLAP, and
    /// `crate::walkforward::walk_forward_over_windows` compounds every window onto ONE running
    /// equity — so the same bars would be counted more than once and the stitched OOS return
    /// would come back roughly the overlap factor too high. Refused, and the message names both
    /// resolved bar counts.
    ///
    /// The spelling is not exotic: "retrain fortnightly, evaluate a month" is `test = "1mo"`,
    /// `step = "2w"`, which at `"1h"` is 744 against 336 — overlap by construction, and mixing
    /// those two suffixes is legal.
    #[test]
    fn a_step_shorter_than_the_test_window_is_refused() {
        let b = bars(3000);
        let e = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\nstep = \"50bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap_err();
        let msg = format!("{e}");
        assert!(
            msg.contains("walkforward.step")
                && msg.contains("50")
                && msg.contains("100")
                && msg.contains("OVERLAP"),
            "{msg}"
        );

        let realistic = resolve_windows(
            &cfg("train = \"1mo\"\ntest = \"1mo\"\nstep = \"2w\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap_err();
        let msg = format!("{realistic}");
        assert!(msg.contains("744") && msg.contains("336"), "{msg}");
    }

    /// …and `step == test` is the CONTIGUOUS case, which must NOT be caught by that refusal —
    /// it is the default (an absent `step` IS `test`) and it tiles the validation windows edge
    /// to edge with no overlap and no gap. A `step` LARGER than `test` stays legal too: that is
    /// separation, which is what `embargo` produces deliberately.
    #[test]
    fn a_step_equal_to_or_larger_than_the_test_window_is_accepted() {
        let b = bars(1000);
        let equal = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\nstep = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        let defaulted = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert_eq!(equal, defaulted, "an absent step IS test, so the two must resolve identically");
        for pair in equal.windows(2) {
            assert_eq!(pair[1].test_start, pair[0].test_end, "contiguous: no overlap, no gap");
        }
        let larger = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\"\nstep = \"200bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        for pair in larger.windows(2) {
            assert!(pair[1].test_start >= pair[0].test_end, "{pair:?}");
        }
    }

    /// A series too short for the declared train window yields NO windows — the driver is
    /// what turns that into a refusal, and this pins that the list really is empty.
    #[test]
    fn a_series_too_short_for_the_train_window_yields_no_windows() {
        let b = bars(100);
        let w = resolve_windows(
            &cfg("train = \"400bars\"\ntest = \"100bars\""),
            &b,
            INTERVAL,
            WalkMode::Anchored,
        )
        .unwrap();
        assert!(w.is_empty());
    }
}
