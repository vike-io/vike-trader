//! The VECTORIZED signal backtest: a position series and a log-return series in, per-bar PnL,
//! equity and trade statistics out.
//!
//! # Why this is not in vike-backtest
//!
//! `vike-backtest` is an event-driven simulator over orders and a broker, with real fee, slippage
//! and adverse-fill semantics — the more honest number, and the wrong tool for sweeping hundreds
//! of signal variants. This is ~100 lines of arithmetic, and reaching it should not cost 32k lines
//! of simulator — the same argument that extracted this crate in the first place.
//!
//! # Why the frictioned model-selection scorer lives here and not in `vike-ml`
//!
//! [`neg_sharpe_mini_backtest`] ranks a candidate MODEL, so `vike-ml` — the crate that owns the
//! learner — looks like its home. It cannot go there, and that is a merge gate rather than a
//! judgement call: the body names ONLY symbols from this crate ([`hysteresis`],
//! [`HysteresisBands`], [`apply_min_hold`], [`bar_pnl`], [`sharpe_from_bar_pnl`],
//! [`mean_variance`]) and otherwise takes plain slices and scalars, so hosting it in `vike-ml`
//! would mean a `vike-ml -> vike-analytics` normal dependency — and both crates declare
//! `layer = 20`, while `crates/vike-ops/tests/layer_gate.rs` demands a STRICTLY LOWER layer for
//! every normal `vike-*` dependency. Which is the right answer anyway: the scorer is arithmetic
//! over a position series and a return series, which is exactly what this module is. The learner
//! side keeps only the choice of WHICH score ranks its candidates.
//!
//! # The one invariant everything else rests on
//!
//! `pnl[t] = position[t - 1] * log_ret[t]`. The position must be the one held BEFORE the return
//! was known. Every lookahead bug in a signal backtest is a violation of that single line.

use vike_model::py_sum;

use crate::metrics::mean_variance;
use crate::signal::{hysteresis, HysteresisBands};

/// Lock a position in for `n_bars` after every entry or flip.
///
/// Sequential by nature: when the position becomes non-zero at bar `i` (from flat, or by flipping
/// sign), bars `i ..= i + n_bars - 1` are forced to that value and ANY signal change inside the
/// window is ignored — including a flip to the opposite sign. A minimum hold that any opposite
/// signal could break would not be a minimum hold: the lock is unconditional for its whole
/// duration, and a flip only starts a fresh lock once it lands on a bar that is no longer inside
/// one (see `min_hold_locks_a_flip_too` for that case, and
/// `a_flip_inside_the_lock_window_is_suppressed` for this one).
///
/// ⚠ The comparison against the previous bar (`out[i - 1]`) reads the ALREADY-OVERWRITTEN output,
/// not the input. That is deliberate and matches the oracle: a bar forced by a lock is, for the
/// purpose of detecting the next entry, the position that was actually held — see
/// `the_entry_check_reads_the_held_position_not_the_original_signal` for a case where the two
/// disagree.
pub fn apply_min_hold(positions: &[i8], n_bars: usize) -> Vec<i8> {
    let mut out = positions.to_vec();
    if n_bars < 1 {
        return out;
    }
    let mut locked_until: isize = -1;
    let mut locked_value: i8 = 0;
    for i in 0..out.len() {
        if (i as isize) <= locked_until {
            out[i] = locked_value;
            continue;
        }
        if out[i] != 0 && (i == 0 || out[i - 1] == 0 || out[i - 1] != out[i]) {
            locked_until = i as isize + n_bars as isize - 1;
            locked_value = out[i];
        }
    }
    out
}

/// Per-bar PnL: `position[t-1] * log_ret[t]`, minus `slippage_bps` on every position change.
///
/// The series starts flat, so entering at bar 0 IS a change and is charged. A `NaN` return is
/// treated as `0.0` PnL for that bar — a position change occurring on the same bar is still
/// charged.
///
/// A flip (long straight to short, or the reverse) costs exactly ONE `slippage_bps` unit, not two,
/// even though it trades two units of notional. `positions[t] != prev` is a 0/1 indicator, and that
/// is faithful to `friction.apply_slippage`'s `changes.astype(int)` — this is NOT a bug to "fix".
///
/// Truncates to `min(positions.len(), log_ret.len())` if the two slices disagree in length; see
/// `trade_stats`'s doc for how that truncation differs from ITS OWN mismatched-length handling.
pub fn bar_pnl(positions: &[i8], log_ret: &[f64], slippage_bps: f64) -> Vec<f64> {
    let n = positions.len().min(log_ret.len());
    let mut out = Vec::with_capacity(n);
    let slip = slippage_bps / 10_000.0;
    for t in 0..n {
        let prev = if t == 0 { 0 } else { positions[t - 1] };
        let r = if log_ret[t].is_nan() { 0.0 } else { log_ret[t] };
        let mut p = prev as f64 * r;
        if positions[t] != prev {
            p -= slip;
        }
        out.push(p);
    }
    out
}

/// Equity curve from per-bar LOG PnL: `exp(cumsum(pnl))`, starting from 1.0 at the first bar.
///
/// ⚠ A `NaN` pnl contributes `0.0` here. That DIVERGES from the pandas oracle for an
/// externally-supplied series: `cumsum()` defaults to `skipna=True` (what `engine.py` uses), so
/// pandas emits `NaN` at exactly the NaN bar's own position and resumes the correct running total
/// immediately after. This function emits a finite, stale value at that bar instead. The two
/// therefore disagree at that one bar and nowhere else — a caller feeding a hand-built series sees
/// a continuous curve where the oracle shows a single hole, not a curve that dies from there on. In
/// the normal pipeline `pnl` comes from [`bar_pnl`], which never emits NaN, so this is only
/// reachable from a hand-built series passed directly here.
pub fn equity_from_pnl(pnl: &[f64]) -> Vec<f64> {
    let mut acc = 0.0;
    pnl.iter()
        .map(|p| {
            acc += if p.is_nan() { 0.0 } else { *p };
            libm::exp(acc)
        })
        .collect()
}

/// Aggregate per-trade statistics over a position and PnL series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignalTradeStats {
    /// Entries and flips. Exiting to flat is not a new trade.
    pub n_trades: usize,
    /// Mean run length of consecutive identical non-zero positions.
    pub avg_holding_bars: f64,
    /// Share of NON-FLAT bars with positive PnL.
    pub win_rate: f64,
    /// Mean PnL over non-flat bars.
    pub avg_pnl: f64,
}

/// `n_trades` and `avg_holding_bars` walk the FULL `positions` slice; `win_rate` and `avg_pnl` walk
/// `positions.iter().zip(pnl.iter())`, which silently truncates to the SHORTER of the two slices.
/// A mismatched-length call therefore computes trade-count/holding-bars stats over one horizon and
/// win-rate/avg-pnl stats over a different (shorter) one, with no error — pass same-length slices.
pub fn trade_stats(positions: &[i8], pnl: &[f64]) -> SignalTradeStats {
    let mut n_trades = 0usize;
    let mut holds: Vec<usize> = Vec::new();
    let mut run = 0usize;
    let mut run_sign: i8 = 0;
    for (i, &p) in positions.iter().enumerate() {
        let prev = if i == 0 { 0 } else { positions[i - 1] };
        if p != prev && p != 0 {
            n_trades += 1;
        }
        if p != 0 && p == run_sign {
            run += 1;
        } else {
            if run > 0 {
                holds.push(run);
            }
            run_sign = p;
            run = if p != 0 { 1 } else { 0 };
        }
    }
    if run > 0 {
        holds.push(run);
    }
    let avg_holding_bars = if holds.is_empty() {
        0.0
    } else {
        py_sum(holds.iter().map(|h| *h as f64)) / holds.len() as f64
    };

    let live: Vec<f64> = positions
        .iter()
        .zip(pnl.iter())
        .filter(|(p, v)| **p != 0 && !v.is_nan())
        .map(|(_, v)| *v)
        .collect();
    let win_rate = if live.is_empty() {
        0.0
    } else {
        live.iter().filter(|v| **v > 0.0).count() as f64 / live.len() as f64
    };
    let avg_pnl =
        if live.is_empty() { 0.0 } else { py_sum(live.iter().copied()) / live.len() as f64 };

    SignalTradeStats { n_trades, avg_holding_bars, win_rate, avg_pnl }
}

/// Annualized Sharpe of a per-bar LOG PnL series: `mean / sd * sqrt(periods_per_year)`, with the
/// SAMPLE standard deviation (`ddof = 1`).
///
/// This is NOT [`crate::metrics::sharpe`] with different arguments. That one takes an EQUITY
/// CURVE and derives simple per-bar returns from it; this one takes the PnL directly and never
/// exponentiates. Both are correct in their own vocabulary and they disagree numerically, so the
/// input is in the name.
///
/// `ddof = 1` is load-bearing and is where a port silently drifts: it reproduces pandas'
/// `Series.std()` default, which is what the oracle's `backtest/metrics.py`'s `sharpe` uses, and
/// the survival gate compares the result against a fixed threshold.
/// [`crate::stats::block_bootstrap_sharpe`] deliberately uses the POPULATION form internally —
/// that is not an inconsistency to fix: one is a point compared against stored oracle numbers, the
/// other is an interval that was never required to match anything.
///
/// Returns `0.0` for fewer than two non-NaN observations or a zero/non-finite sd, matching the
/// oracle's `len(s) == 0 or s.std() == 0` guard: callers compare against a threshold, and zero
/// fails every threshold.
pub fn sharpe_from_bar_pnl(pnl: &[f64], periods_per_year: f64) -> f64 {
    let clean: Vec<f64> = pnl.iter().copied().filter(|v| !v.is_nan()).collect();
    if clean.len() < 2 {
        return 0.0;
    }
    let (mean, var) = crate::metrics::mean_variance(clean.iter().copied(), 1.0);
    let sd = var.sqrt();
    if sd == 0.0 || !sd.is_finite() {
        return 0.0;
    }
    mean / sd * periods_per_year.sqrt()
}

/// Largest peak-to-trough drop as a SIGNED fraction: negative when a drawdown exists, `0.0` when
/// none does. `min((eq - cummax) / cummax)`.
///
/// This is NOT [`crate::metrics::max_drawdown`] with a different name. That one returns the same
/// magnitude POSITIVE ("0.2 == 20%") and is what the tearsheet renders; this one reproduces the
/// oracle's `backtest/metrics.py`'s `max_drawdown`, which returns `dd.min()`, and is what the
/// parity gate compares against 271 stored Python runs. Do not "unify" them by changing the other:
/// its sign is user-visible in `vike-report`'s output, and a silent flip there is a UI bug in a
/// number operators read.
///
/// `0.0` for an empty curve, matching the oracle's `len(eq) == 0` guard.
pub fn max_drawdown_signed(equity: &[f64]) -> f64 {
    if equity.is_empty() {
        return 0.0;
    }
    let mut peak = equity[0];
    let mut worst = 0.0f64;
    for &v in equity {
        peak = peak.max(v);
        if peak > 0.0 {
            worst = worst.min((v - peak) / peak);
        }
    }
    worst
}

// ---- the frictioned model-selection scorer ------------------------------------------------------

/// The frictioned mini-backtest's inputs: rank a candidate by the annualised Sharpe of a backtest
/// of its OWN out-of-sample probabilities, instead of by how well those probabilities calibrate
/// against labels.
///
/// The mini-backtest is the frictioned pipeline above verbatim — [`hysteresis`] →
/// [`apply_min_hold`] → [`bar_pnl`] → [`sharpe_from_bar_pnl`], the same kernels a REPORTED
/// backtest folds, never a re-spelling — deliberately in the FRICTIONED shape (`min_hold_bars`,
/// `slippage_bps`) so a churny candidate pays for its churn at SELECTION time exactly as it would
/// out of sample.
///
/// Two artifacts a caller accepts by ranking with this, both IDENTICAL for every candidate and
/// therefore fair to rank across:
///
/// * **the fold starts FLAT at the slice's first row** — in the caller's real walk a position may
///   already be open when these bars arrive, but a slice carries no "position so far", so every
///   candidate is folded from flat over the same rows;
/// * **the slice is scored as ONE series even where it crosses a seam** — an asset boundary, a
///   session gap, whatever the caller's row order puts there. The caller's own per-span folding is
///   the REPORTED backtest; this is a ranking device, and the crossing is the same rows for every
///   candidate.
#[derive(Clone, Copy)]
pub struct SharpeObjective<'a> {
    /// One bar return per row, aligned with the caller's rows and carrying the same unchecked
    /// length invariant [`bar_pnl`] does — a disagreement TRUNCATES rather than erroring. A NaN
    /// return contributes `0.0` PnL for its bar, [`bar_pnl`]'s own rule.
    pub bar_returns: &'a [f64],
    pub bands: HysteresisBands,
    pub min_hold_bars: usize,
    pub slippage_bps: f64,
    /// The annualisation constant. Pass the SAME one the reported backtest uses, or the selection
    /// Sharpe and the reported Sharpe are annualised differently and cannot be compared.
    pub periods_per_year: f64,
}

/// The frictioned mini-backtest, NEGATED — the number a model search ARGMINs.
///
/// # The degenerate-slice rule: no evidence scores WORST, explicitly
///
/// **This is the whole reason the scorer is a function rather than three composed calls at each
/// call site.** A candidate is ranked by its Sharpe only when the mini-backtest produced evidence:
/// it must TRADE (at least one non-flat bar after the min-hold), and its PnL must have a DEFINED
/// sample Sharpe (at least two bars, positive finite variance). Anything else scores `INFINITY`,
/// the worst value an argmin can receive.
///
/// This cannot be left to [`sharpe_from_bar_pnl`], which answers `0.0` for exactly these cases.
/// That answer is CORRECT for its threshold-comparing callers — "zero fails every threshold" — and
/// exactly WRONG for an argmin, where `0.0` ranks a no-evidence candidate ABOVE every
/// negative-Sharpe one that actually traded. In a ranked UI leaderboard that sorts a strategy which
/// never opened a position to the TOP of a list of strategies that traded and lost. And it is not
/// an edge case: with [`HysteresisBands::default`]'s 0.75/0.25 entry bands over a hundred-odd-row
/// slice an all-flat candidate is COMMON, so a search without this rule keeps answering "the model
/// that never trades" — the first such index winning the tie among all of them.
/// `a_degenerate_candidate_never_outranks_one_that_traded_and_lost` pins it.
///
/// The checks, in order, and which inputs each one uniquely catches:
///
/// * **zero non-flat bars** — the all-flat slice. Stated first because it is the common case and
///   the rule's whole point; note it is IMPLIED by the variance check (no trades ⇒ `pnl ≡ 0.0`
///   exactly, including the slippage term, which charges only on a position CHANGE ⇒ zero
///   variance), so deleting this line alone changes no answer — the pin tests document that
///   equivalence rather than pretending each line is independently load-bearing;
/// * **fewer than two PnL bars** — a one-row slice, where a sample (`ddof = 1`) deviation does not
///   exist;
/// * **zero or non-finite variance** — a slice whose every PnL bar is equal (e.g. a constant
///   return exactly cancelling the slippage of a constant churn). The mirror of
///   [`sharpe_from_bar_pnl`]'s own `sd == 0.0 || !sd.is_finite()` guard, through the same
///   [`mean_variance`] over the same NaN-filtered series, so the two cannot disagree about which
///   slices are degenerate.
///
/// A NaN PROBABILITY needs no arm of its own: [`hysteresis`] defines NaN as "no estimate — hold",
/// the same rule a caller's live walk applies to the same model's out-of-sample probabilities, so
/// the mini-backtest inherits it; an all-NaN candidate never leaves flat and scores worst through
/// the trade check.
///
/// A single-TRADE slice is NOT degenerate: one entry that then holds gives `n - 1` market bars of
/// PnL, a defined variance, and a Sharpe as rankable as any other — pinned by
/// `a_single_trade_slice_ranks_by_its_real_sharpe`.
pub fn neg_sharpe_mini_backtest(proba: &[f64], s: &SharpeObjective<'_>) -> f64 {
    let held = apply_min_hold(&hysteresis(proba, s.bands), s.min_hold_bars);
    let pnl = bar_pnl(&held, s.bar_returns, s.slippage_bps);
    if !held.iter().any(|p| *p != 0) {
        return f64::INFINITY;
    }
    // The same NaN-filtered series `sharpe_from_bar_pnl` builds internally, so the two cannot
    // disagree about which slices are degenerate.
    let clean: Vec<f64> = pnl.iter().copied().filter(|v| !v.is_nan()).collect();
    if clean.len() < 2 {
        return f64::INFINITY;
    }
    let (_, var) = mean_variance(clean.iter().copied(), 1.0);
    if var == 0.0 || !var.is_finite() {
        return f64::INFINITY;
    }
    -sharpe_from_bar_pnl(&pnl, s.periods_per_year)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_hold_locks_a_new_entry_for_n_bars() {
        // Enter long at 0, signal drops to 0 at 1, but a 2-bar hold keeps it.
        let out = apply_min_hold(&[1, 0, 0, 0], 2);
        assert_eq!(out, vec![1, 1, 0, 0]);
    }

    #[test]
    fn min_hold_of_one_bar_changes_nothing() {
        let src = [1, 0, -1, -1];
        assert_eq!(apply_min_hold(&src, 1), src.to_vec());
    }

    #[test]
    fn a_flip_inside_the_lock_window_is_suppressed() {
        // Bar 0 enters long and locks bars 0..1, so bar 1's -1 never takes effect.
        // A min-hold any opposite signal could break would not be a min-hold.
        assert_eq!(apply_min_hold(&[1, -1, 0, 0], 2), vec![1, 1, 0, 0]);
    }

    #[test]
    fn min_hold_locks_a_flip_too() {
        // Bar 0 locks 0..1; bar 2 flips once that lock has expired and locks 2..3.
        assert_eq!(apply_min_hold(&[1, 0, -1, 0], 2), vec![1, 1, -1, -1]);
    }

    #[test]
    fn the_entry_check_reads_the_held_position_not_the_original_signal() {
        // Bar 0's lock forces bar 1 to +1. Bar 2 is therefore a flip against the position
        // ACTUALLY HELD — which reading the original input (-1 at bar 1) would hide, since
        // -1 -> -1 looks like no change. A mutant comparing positions[i-1] returns
        // [1, 1, -1, 0] here instead.
        assert_eq!(apply_min_hold(&[1, -1, -1, 0], 2), vec![1, 1, -1, -1]);
    }

    #[test]
    fn pnl_uses_the_previous_bars_position_never_the_current_one() {
        // No lookahead: bar 1's return is earned by bar 0's position.
        let pnl = bar_pnl(&[1, 1], &[0.10, 0.20], 0.0);
        assert_eq!(pnl[0], 0.0, "bar 0 has no prior position");
        assert_eq!(pnl[1], 0.20);
    }

    #[test]
    fn slippage_is_charged_on_every_position_change() {
        // 15 bps = 0.0015. Entering at bar 0 is a change from the implicit flat start.
        let pnl = bar_pnl(&[1, 1], &[0.0, 0.0], 15.0);
        assert!((pnl[0] - -0.0015).abs() < 1e-15);
        assert_eq!(pnl[1], 0.0, "no change, no charge");
    }

    #[test]
    fn slippage_is_charged_on_an_exit_to_flat_too() {
        // Exiting to flat is a position change and is charged, same as an entry.
        let pnl = bar_pnl(&[1, 0], &[0.0, 0.0], 15.0);
        assert!((pnl[1] - -0.0015).abs() < 1e-15);
    }

    #[test]
    fn a_short_earns_the_negated_return() {
        let pnl = bar_pnl(&[-1, -1], &[0.0, 0.20], 0.0);
        assert_eq!(pnl[1], -0.20);
    }

    #[test]
    fn equity_is_the_exponentiated_cumulative_log_pnl() {
        let eq = equity_from_pnl(&[0.0, 0.0]);
        assert_eq!(eq, vec![1.0, 1.0]);

        // A flat-PnL series can't distinguish cumulation from a dozen other implementations
        // (never accumulating, subtracting, a constant vec![1.0; n], exponentiating the mean —
        // all pass the case above too). This one only passes if the sum is genuinely running.
        let eq = equity_from_pnl(&[0.1, 0.2]);
        assert!((eq[0] - 0.1f64.exp()).abs() < 1e-15);
        assert!((eq[1] - 0.3f64.exp()).abs() < 1e-15); // cumulative, not per-bar
    }

    #[test]
    fn trade_stats_counts_entries_and_flips_but_not_exits() {
        // flat -> long (1 trade) -> flat -> short (2nd trade)
        let stats = trade_stats(&[1, 1, 0, -1], &[0.1, 0.1, 0.0, -0.1]);
        assert_eq!(stats.n_trades, 2);
    }

    #[test]
    fn n_trades_counts_a_direct_flip_not_just_entries_from_flat() {
        // The fixture above is two entries FROM FLAT; a mutant counting only those still returns
        // 2 there. This fixture has one entry and one DIRECT flip (no flat bar in between).
        assert_eq!(trade_stats(&[1, -1, -1, 0], &[0.1; 4]).n_trades, 2); // entry + direct flip
    }

    #[test]
    fn avg_holding_bars_and_avg_pnl_are_computed_correctly() {
        let s = trade_stats(&[1, 1, 0, -1], &[0.1, 0.1, 0.0, -0.1]);
        assert_eq!(s.avg_holding_bars, 1.5); // runs of 2 and 1
        assert!((s.avg_pnl - 0.1 / 3.0).abs() < 1e-15); // three non-flat bars
    }

    #[test]
    fn win_rate_ignores_flat_bars() {
        // Only the three non-flat bars count; two are positive.
        let stats = trade_stats(&[1, 1, 0, 1], &[0.1, -0.1, 99.0, 0.1]);
        assert!((stats.win_rate - 2.0 / 3.0).abs() < 1e-15);
    }

    #[test]
    fn sharpe_uses_the_sample_sd_not_the_population_one() {
        // [1,2,3,4]: mean 2.5, sample var 5/3, population var 5/4. The survival gate is
        // `sharpe >= 0.5`, and the population form is larger by sqrt(n/(n-1)) — enough to flip a
        // borderline candidate on a short series.
        let s = sharpe_from_bar_pnl(&[1.0, 2.0, 3.0, 4.0], 1.0);
        let expected = 2.5 / (5.0f64 / 3.0).sqrt();
        assert!((s - expected).abs() < 1e-12, "got {s}, want {expected}");
    }

    #[test]
    fn a_flat_series_scores_zero_rather_than_infinity() {
        assert_eq!(sharpe_from_bar_pnl(&[0.01; 50], 8760.0), 0.0);
    }

    #[test]
    fn fewer_than_two_observations_score_zero() {
        // ⚠ A DECLARED DIVERGENCE at n == 1, not a parity claim. The oracle guards on
        // `len(s) == 0 or s.std() == 0`; with exactly one observation pandas' `std()` is NaN
        // (ddof = 1, zero degrees of freedom), `NaN == 0` is False, so the oracle falls through and
        // returns `mean / NaN` = NaN. This returns 0.0. Both fail the `sharpe >= 0.5` survival gate
        // and neither reaches a stored per-rule row (a one-bar fold produces no run), so the
        // difference is unobservable downstream — but it is a difference, and calling it parity
        // would be false.
        assert_eq!(sharpe_from_bar_pnl(&[], 8760.0), 0.0);
        assert_eq!(sharpe_from_bar_pnl(&[0.5], 8760.0), 0.0, "oracle: NaN here; see the comment");
    }

    #[test]
    fn the_signed_drawdown_is_negative_where_the_positive_one_is_positive() {
        // The oracle returns `dd.min()`; `metrics::max_drawdown` returns the same magnitude with the
        // opposite sign. Both are correct in their own vocabulary — the parity gate needs the
        // oracle's.
        let eq = [1.0, 1.25, 1.0, 1.5];
        let signed = max_drawdown_signed(&eq);
        assert!((signed - (-0.2)).abs() < 1e-15, "got {signed}");
        assert!(
            (signed + crate::metrics::max_drawdown(&eq)).abs() < 1e-15,
            "the two must be exact negatives of each other"
        );
    }

    #[test]
    fn a_monotonically_rising_curve_has_no_drawdown_and_an_empty_one_is_zero() {
        assert_eq!(max_drawdown_signed(&[1.0, 1.1, 1.2]), 0.0);
        assert_eq!(max_drawdown_signed(&[]), 0.0, "the oracle's `len(eq) == 0` guard");
    }

    #[test]
    fn nans_are_dropped_not_propagated() {
        let a = sharpe_from_bar_pnl(&[0.1, -0.2, 0.3], 8760.0);
        let b = sharpe_from_bar_pnl(&[0.1, f64::NAN, -0.2, 0.3], 8760.0);
        assert_eq!(a, b);
    }

    #[test]
    fn it_disagrees_with_the_equity_curve_sharpe_which_is_the_whole_point() {
        // This test exists so that substituting `metrics::sharpe` — which compiles, because the
        // shapes match — fails loudly instead of silently changing the headline number.
        let pnl = [0.01, -0.02, 0.03, 0.005];
        let eq = equity_from_pnl(&pnl);
        assert_ne!(sharpe_from_bar_pnl(&pnl, 8760.0), crate::metrics::sharpe(&eq, 8760.0));
    }

    // ---- the frictioned mini-backtest scorer --------------------------------------------------

    /// A [`SharpeObjective`] at the cohort study's own defaults except the two knobs a test varies.
    fn sharpe_obj(
        bar_returns: &[f64],
        slippage_bps: f64,
        min_hold_bars: usize,
    ) -> SharpeObjective<'_> {
        SharpeObjective {
            bar_returns,
            bands: HysteresisBands::default(),
            min_hold_bars,
            slippage_bps,
            periods_per_year: 8_760.0,
        }
    }

    /// ⚠ THE reason this scorer is a function rather than three composed calls, stated as the
    /// ranking it protects: a candidate that never opened a position must rank BELOW one that
    /// traded and lost money. The second half of the test is the bug — the shared kernel's own
    /// answer for the degenerate slice, negated, sorting ABOVE the real loss.
    #[test]
    fn a_degenerate_candidate_never_outranks_one_that_traded_and_lost() {
        let lr = vec![-0.001; 40]; // a downward drift: a steady long really does lose
        let s = sharpe_obj(&lr, 1.5, 2);

        // Traded and lost: every probability above the long entry band, so it enters at bar 0.
        let loser = neg_sharpe_mini_backtest(&[0.9; 40], &s);
        assert!(loser.is_finite(), "the losing fixture scored degenerate — it tests nothing");
        assert!(loser > 0.0, "the losing fixture made money — it tests nothing");

        // No evidence: every probability inside the dead zone, so it never opens a position.
        let degenerate = neg_sharpe_mini_backtest(&[0.5; 40], &s);
        assert!(
            degenerate > loser,
            "a candidate that never traded ({degenerate}) outranked one that traded and lost \
             ({loser}) — in a ranked leaderboard that puts a strategy which never opened a \
             position above strategies that traded"
        );

        // ...and this is what it would score without the rule. `sharpe_from_bar_pnl` answers
        // `0.0` for a series with fewer than two cleaned points or zero dispersion — right for
        // its threshold-comparing callers, exactly wrong for an argmin.
        let flat = apply_min_hold(&hysteresis(&[0.5; 40], s.bands), s.min_hold_bars);
        assert!(flat.iter().all(|p| *p == 0), "the degenerate fixture traded — it tests nothing");
        let naive = -sharpe_from_bar_pnl(&bar_pnl(&flat, &lr, s.slippage_bps), s.periods_per_year);
        assert_eq!(naive, 0.0, "the kernel's own answer, which an argmin must never receive");
        assert!(naive < loser, "the naive composition already ranks correctly — nothing to guard");
    }

    /// The zero-trade rule, stated as the difference it exists to create: for an all-flat slice
    /// the shared kernel answers `0.0` (its threshold-caller contract) while the scorer answers
    /// `INFINITY` — explicitly, not through NaN luck.
    #[test]
    fn an_all_flat_slice_scores_worst_explicitly_not_zero() {
        // Every probability in the dead zone: no candidate bar ever enters.
        let proba = vec![0.5; 101];
        let lr = vec![0.001; 101];
        let s = sharpe_obj(&lr, 1.5, 2);
        let held = apply_min_hold(&hysteresis(&proba, s.bands), s.min_hold_bars);
        assert!(held.iter().all(|p| *p == 0), "the fixture entered — it tests nothing");
        assert_eq!(
            sharpe_from_bar_pnl(&bar_pnl(&held, &lr, s.slippage_bps), s.periods_per_year),
            0.0,
            "the kernel's own answer, which an argmin must never receive"
        );
        assert_eq!(neg_sharpe_mini_backtest(&proba, &s), f64::INFINITY);

        // An all-NaN candidate is the same case through the same rule: hysteresis holds flat on
        // NaN (a live walk's own semantics), so it never trades and scores worst.
        assert_eq!(neg_sharpe_mini_backtest(&[f64::NAN; 101], &s), f64::INFINITY);
    }

    /// "Too few bars for a defined Sharpe": a one-row slice trades but has no sample (`ddof = 1`)
    /// deviation, and an empty slice (a caller's cut clamped to the end of its rows) is not a
    /// panic.
    #[test]
    fn a_slice_too_short_for_a_sample_deviation_scores_worst() {
        assert_eq!(neg_sharpe_mini_backtest(&[0.9], &sharpe_obj(&[0.001], 1.5, 2)), f64::INFINITY);
        assert_eq!(neg_sharpe_mini_backtest(&[], &sharpe_obj(&[], 1.5, 2)), f64::INFINITY);
    }

    /// A candidate can TRADE and still have no Sharpe: a constant return exactly equal to minus
    /// the entry slippage makes every PnL bar identical, so the sample deviation is exactly zero.
    /// This is the one degenerate case the trade check cannot see, which is also why the variance
    /// check is the guard that catches the all-flat case if the (implied) trade check is ever
    /// simplified away — see [`neg_sharpe_mini_backtest`]'s doc for the implication.
    #[test]
    fn a_zero_variance_pnl_scores_worst_even_when_the_candidate_trades() {
        let slip = 10.0 / 10_000.0;
        let lr = vec![-slip; 40];
        let proba = vec![0.9; 40];
        let s = sharpe_obj(&lr, 10.0, 2);
        let held = apply_min_hold(&hysteresis(&proba, s.bands), s.min_hold_bars);
        assert!(held.iter().any(|p| *p != 0), "the fixture never traded — it tests nothing");
        assert_eq!(neg_sharpe_mini_backtest(&proba, &s), f64::INFINITY);
    }

    /// A single-TRADE slice is NOT degenerate — one entry then holding gives `n - 1` market bars
    /// and a defined variance — and its score IS the negated shared-kernel composition, which is
    /// the "reuse the kernels above, never re-spell them" claim stated as an exact identity
    /// (annualisation constant included).
    #[test]
    fn a_single_trade_slice_ranks_by_its_real_sharpe() {
        let lr: Vec<f64> = (0..40).map(|i| 0.001 * ((i as f64) * 0.37).sin()).collect();
        let proba = vec![0.9; 40];
        let s = sharpe_obj(&lr, 1.5, 2);
        let held = apply_min_hold(&hysteresis(&proba, s.bands), s.min_hold_bars);
        let want = -sharpe_from_bar_pnl(&bar_pnl(&held, &lr, 1.5), 8_760.0);
        let got = neg_sharpe_mini_backtest(&proba, &s);
        assert!(got.is_finite(), "a single-trade slice was scored degenerate");
        assert_eq!(got, want);
        assert_ne!(got, 0.0, "the fixture's Sharpe is zero — the identity proves nothing");
    }
}
