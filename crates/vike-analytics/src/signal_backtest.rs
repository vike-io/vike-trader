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
//! `layer = 15`, while `crates/vike-ops/tests/architecture/layer_gate.rs` demands a STRICTLY LOWER layer for
//! every normal `vike-*` dependency. (This said `layer = 20` until 2026-09-28; both dropped to the
//! `leaf` floor on 2026-09-23, and equal rank refuses the edge exactly as it did.) Which is the
//! right answer anyway: the scorer is arithmetic over a position series and a return series,
//! which is exactly what this module is. The learner side keeps only the choice of WHICH score
//! ranks its candidates.
//!
//! # The one invariant everything else rests on
//!
//! `pnl[t] = position[t - 1] * log_ret[t]`. The position must be the one held BEFORE the return
//! was known. Every lookahead bug in a signal backtest is a violation of that single line.

use vike_model::py_sum;

use crate::metrics::mean_variance;
use crate::signal::{HysteresisBands, hysteresis};

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
    ///
    /// ⚠ **Prefer [`SharpeObjective::for_interval`], which derives it.** This field stays public
    /// for a caller that already HOLDS a derived value, but a literal here is the whole hazard the
    /// sentence above only describes.
    pub periods_per_year: f64,
}

impl<'a> SharpeObjective<'a> {
    /// Build the objective with the annualisation constant DERIVED from the slice's bar interval,
    /// through the same [`crate::report::periods_per_year_for_interval`] the REPORTED backtest
    /// resolves — so "pass the same one" stops being a thing to remember.
    ///
    /// # Why a constructor rather than a gate
    ///
    /// The field's doc has always carried the rule, and a rule in a doc comment is obeyed by
    /// whoever reads it. The obvious next move is a test that fails when a caller passes a literal
    /// — and it could not fail for its stated reason today, because **this scorer has no production
    /// caller at all**: every call is inside this file's own test module, and the two mentions in
    /// `crates/vike-user-research/src/contract.rs` are prose about the `INFINITY` convention. A
    /// gate over an empty caller set is an assertion that cannot fail, which this workspace treats
    /// as worse than no gate.
    ///
    /// So the hazard is the FIRST caller, and what helps a first caller is a shorter correct path
    /// than the incorrect one. `interval` is the same string a profile's `[data]` table already
    /// names; an unparseable one falls back exactly as the authority does, rather than erroring
    /// here and inventing a second answer to a question that crate already owns.
    #[must_use]
    pub fn for_interval(
        bar_returns: &'a [f64],
        interval: &str,
        bands: HysteresisBands,
        min_hold_bars: usize,
        slippage_bps: f64,
    ) -> Self {
        Self {
            bar_returns,
            bands,
            min_hold_bars,
            slippage_bps,
            periods_per_year: crate::report::periods_per_year_for_interval(interval),
        }
    }
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

#[path = "signal_backtest_tests.rs"]
#[cfg(test)]
mod signal_backtest_tests;
