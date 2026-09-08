//! Pluggable sweep-ranking objectives: a scalar score over a [`BacktestReport`], HIGHER is
//! better. The parameter-sweep runner (`harness::sweep::run_sweep_with`) ranks its rows by an
//! [`Objective`]; the existing `RankMetric` variants are exposed as built-in constructors
//! (`RankMetric::objective`, in `harness::sweep` — direction folded in, so e.g. max-drawdown's
//! objective is `-max_drawdown`), and this module adds the composite [`multi_metric`] objective.
//!
//! FEATURE-FREE by design (like `crate::report`, which it scores): nothing here names the gated
//! profile parser or the DataFusion tree, so leaf crates can reuse the same objectives and the
//! unit tests run in the default `cargo test -p vike-backtest` lane.
//!
//! ⚠ **Every `ln` written below is `libm::log`, never `f64::ln`** — the method call resolves to the
//! PLATFORM's libm, which IEEE 754 does not require to round `ln` correctly, so MSVC and glibc
//! disagree in the last bit and a ranking built from the result can order two near-tied sweep rows
//! differently on the two boxes. `shape_max` below carries the full argument and the decision
//! record; extend this module with `libm::` spellings.
//!
//! Two conventions/laws, both property-tested below — a sweep's #1 row feeds downstream selection,
//! so "degenerate points cannot be crowned" has to be provable, not merely intended:
//!
//! - **`NaN` = unrankable.** A `NaN` score is legal and means "cannot be ranked"; the sweep sorter
//!   puts those rows LAST (mirroring `RankMetric`'s NaN-last rule). Every [`multi_metric`] input
//!   that is `NaN` poisons the score rather than being silently substituted.
//! - **The sign law** (only [`multi_metric`] can violate this, so only it is bound by it): every
//!   LOSING run scores strictly below every PROFITABLE one, whatever its other stats say. The
//!   shaping terms are ≥ 0 factors that mostly sit under `1`, so applying them multiplicatively to
//!   a NEGATIVE base would shrink a loss TOWARD zero — i.e. reward it — and rank the most
//!   catastrophic point first on an all-losing grid (the common case when tuning a bad strategy).
//!   [`multi_metric_score`] therefore has two regimes: multiplicative shaping on the profitable
//!   domain, and a monotone loss-AMPLIFYING branch on the losing one.

use crate::report::BacktestReport;

/// A sweep-ranking objective: a scalar score over one run's report. HIGHER is better; `NaN`
/// means "unrankable" and sorts last.
pub type Objective = Box<dyn Fn(&BacktestReport) -> f64 + Send + Sync>;

/// Tunables of the composite [`multi_metric`] objective — every constant in the formula is a pub
/// field here, so a caller can reshape the score without a new objective.
///
/// The score (see [`multi_metric_score`]) — note the TWO regimes the sign law demands:
///
/// ```text
/// base    = total_return - max_drawdown          // profit net of drawdown
/// shape   = ln(1 + profit_factor)                // profit_factor clamped to profit_factor_cap
///         * winrate_coeff                        // winrate_floor + (1 - winrate_floor)*win_rate
///         * trade_count_penalty(n_trades)        // see [`trade_count_penalty`]
///         -> clamped into [0, shape_max],  shape_max = ln(1 + profit_factor_cap)
/// quality = shape / shape_max                    // the same shaping, normalized to [0, 1]
///
/// score   = if base > 0 { base * shape }                                    // profitable regime
///           else        { base * (1 + loss_quality_weight * (1 - quality)) } // losing regime
/// ```
///
/// So the score's zero point means "did nothing": `> 0` is a quality-shaped profit, `< 0` a
/// quality-AMPLIFIED loss, and exactly `0` a breakeven/no-trade/no-winner run that there is
/// nothing to rank. Under the default `winrate_floor = 0.0` every profitable-but-winner-less run
/// collapses onto that `0` plateau (`shape = 0`); raise `winrate_floor` above `0` to keep the
/// profitable regime strictly ordered.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct MultiMetricParams {
    /// Trade count at (and above) which [`trade_count_penalty`] is exactly `1.0` — the
    /// statistical-significance knee. Below it the penalty falls off linearly. Default `50`.
    pub target_trades: usize,
    /// Lower bound of [`trade_count_penalty`] — even a near-zero trade count keeps this fraction
    /// of its shaping (so a promising-but-thin PROFITABLE point stays visible in the ranking;
    /// symmetrically, a thin LOSING point keeps most of its loss amplification). Default `0.1`.
    pub penalty_floor: f64,
    /// Floor of the win-rate coefficient: `winrate_coeff = winrate_floor + (1 - winrate_floor) *
    /// win_rate`. The default `0.0` makes the coefficient the plain win rate (so a 0%-win-rate
    /// profitable point lands on the `0` plateau, and a 0%-win-rate LOSER takes the full loss
    /// amplification); raise it to soften the win-rate term for low-win-rate/high-payoff styles.
    pub winrate_floor: f64,
    /// Clamp applied to `profit_factor` before `ln(1 + pf)`. `metrics::profit_factor` returns the
    /// house `f64::INFINITY` sentinel for a run with no losing trades — unclamped, that would give
    /// every all-win point an infinite score and rank it first regardless of its other stats. The
    /// default `100.0` keeps the term finite (`ln(101) ≈ 4.6`) while leaving any realistic
    /// profit factor untouched. Also the normalizer of the losing regime's `quality` term.
    pub profit_factor_cap: f64,
    /// How hard the shaping terms bite in the LOSING regime, where they may only DEEPEN a loss —
    /// never lift it (that is the sign law): the loss is multiplied by `1 + loss_quality_weight *
    /// (1 - quality)`, i.e. a perfect-quality loser keeps its raw `base` and a zero-quality one
    /// (no winners at all, a 1-trade sample) is amplified by `1 + loss_quality_weight`. Default
    /// `1.0` — the worst-quality loss counts double, which keeps loss SIZE the dominant term
    /// (only losses within 2x of each other can be reordered by quality). `0.0` disables the
    /// shaping entirely, ranking losers purely by profit-net-of-drawdown. Negative values are
    /// treated as `0.0` (a negative weight could flip a loss positive and break the sign law).
    pub loss_quality_weight: f64,
}

impl Default for MultiMetricParams {
    fn default() -> Self {
        MultiMetricParams {
            target_trades: 50,
            penalty_floor: 0.1,
            winrate_floor: 0.0,
            profit_factor_cap: 100.0,
            loss_quality_weight: 1.0,
        }
    }
}

/// The trade-count penalty term of [`multi_metric_score`]:
///
/// ```text
/// penalty = if n < target { max(penalty_floor, 1 - |n - target| / target) } else { 1.0 }
/// ```
///
/// Piecewise: `1.0` at and above `target_trades`, linearly falling below it, floored at
/// `penalty_floor` (so `n = 0` scores `penalty_floor`, not `0`). Monotonically non-decreasing in
/// `n`. A degenerate `target_trades == 0` config disables the penalty entirely (`1.0`).
pub fn trade_count_penalty(n: usize, params: &MultiMetricParams) -> f64 {
    if params.target_trades == 0 {
        return 1.0;
    }
    let target = params.target_trades as f64;
    let n = n as f64;
    if n < target { params.penalty_floor.max(1.0 - (target - n) / target) } else { 1.0 }
}

/// The largest value [`shape`] can take: `ln(1 + profit_factor_cap)` with both other factors at
/// their maximum of `1`. Floored at `0` so a degenerate `profit_factor_cap <= 0` (or `NaN`) config
/// still yields a usable, non-negative bound — `f64::max` returns the non-`NaN` operand, so a
/// `NaN` cap collapses the shaping to `0` instead of poisoning every score.
///
/// ⚠ The `ln` is `libm::log`, not `f64::ln`. IEEE 754 requires `+ - * /` and `sqrt` correctly
/// rounded and requires NOTHING of `ln`, so the method spelling resolves to whatever libm the
/// PLATFORM ships — MSVC's CRT on the Windows dev box, glibc on the CI box — and the two differ in the
/// last bit. Here that decides a RANKING: this value is the normalizer of the losing regime's
/// `quality` term and the upper clamp of [`shape`], so two boxes could order two near-tied sweep
/// rows differently and crown a different #1 config off the same data. `libm` is one pure-Rust
/// implementation compiled into the binary, so both boxes run the same code;
/// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
/// verdict and carries the measurement. Both spellings return `NaN` for a non-positive argument
/// and `NaN` for `NaN`, so the `f64::max` NaN behaviour documented above is unchanged.
fn shape_max(params: &MultiMetricParams) -> f64 {
    libm::log(1.0 + params.profit_factor_cap).max(0.0)
}

/// The shaping coefficient `ln(1 + pf) * winrate_coeff * trade_count_penalty(n)`, clamped into
/// `[0, shape_max]`.
///
/// The clamp is what makes the sign law hold BY CONSTRUCTION rather than by trusting the report's
/// provenance: `metrics::profit_factor`/`win_rate` are always `>= 0` / in `0..=1`, but a
/// hand-built report (or an exotic `winrate_floor`) could otherwise make `shape` negative — e.g.
/// `ln(1 + pf)` with `pf < 0` — and flip a PROFITABLE run's score below a losing one.
/// `f64::clamp` returns `NaN` unchanged (and cannot panic here: `0.0 <= shape_max`), so a `NaN`
/// metric still poisons the score instead of being laundered into a number.
///
/// ⚠ `libm::log(1.0 + pf)`, not `(1.0 + pf).ln()` — see [`shape_max`] for the platform-libm
/// argument and the decision record. What is deliberately NOT claimed here: this is not a live
/// order path and it is not even the sweep's default ranking. [`multi_metric`] is OPT-IN
/// (`--rank-by multi` in the `backtest` bin); with the flag absent the bin takes
/// `RankMetric::default()`, which is `Sharpe`, and nothing in this module is called at all. The
/// exposure is a research one — a sweep, euler refinement or TPE run ranked `multi` could pick a
/// different winning config on the desktop than on the CI box — and it is worth converting for exactly
/// that reason, not by inflating it into a trading hazard.
fn shape(report: &BacktestReport, params: &MultiMetricParams, shape_max: f64) -> f64 {
    // NaN-preserving clamp: `NaN > cap` is false, so NaN passes through (and poisons the score),
    // unlike `f64::min`, which would quietly substitute the cap.
    let pf = if report.profit_factor > params.profit_factor_cap {
        params.profit_factor_cap
    } else {
        report.profit_factor
    };
    let winrate_coeff = params.winrate_floor + (1.0 - params.winrate_floor) * report.win_rate;
    let penalty = trade_count_penalty(report.n_trades, params);
    (libm::log(1.0 + pf) * winrate_coeff * penalty).clamp(0.0, shape_max)
}

/// The composite score of the [`multi_metric`] objective — the pure, directly-testable form.
/// See [`MultiMetricParams`] for the formula. HIGHER is better.
///
/// Obeys the module's two laws by construction:
///
/// - **`NaN` in, `NaN` out.** Every input feeds a product that a `NaN` poisons, and neither clamp
///   substitutes it (`profit_factor`'s `inf` sentinel IS clamped to `profit_factor_cap`; `NaN` is
///   not) — so an unrankable point sorts last instead of scoring some incidental number.
/// - **Sign law.** `base > 0` ⟹ `score = base * shape >= 0` (`shape` is clamped non-negative);
///   `base < 0` ⟹ `score = base * m` with `m = 1 + weight*(1 - quality) >= 1`, hence
///   `score <= base < 0`. Every losing run is therefore STRICTLY below every profitable one, and
///   within the losing regime worse quality means a deeper (lower-ranked) score — the exact
///   inversion the pure-multiplicative form had, where the flattering all-zero stats of a
///   catastrophic point shrank its score toward `-0.0` and crowned it.
pub fn multi_metric_score(report: &BacktestReport, params: &MultiMetricParams) -> f64 {
    let base = report.total_return - report.max_drawdown;
    let shape_max = shape_max(params);
    let shape = shape(report, params, shape_max);

    if base > 0.0 {
        // Profitable regime: the shaping terms scale the profit. All are `>= 0`, so a profitable
        // run can never score below zero (it can score exactly zero — the "no winners" plateau).
        base * shape
    } else {
        // Losing/breakeven regime. `quality` is `shape` normalized to `0..=1`; the multiplier
        // `1 + weight*(1 - quality)` lives in `[1, 1 + weight]`, so shaping can only make a loss
        // WORSE. `shape_max == 0` only for a degenerate `profit_factor_cap <= 0`, where `shape` is
        // itself clamped into `[0, 0]` — passing it through is both the right value and the
        // NaN-preserving one.
        let quality = if shape_max > 0.0 { shape / shape_max } else { shape };
        // A negative weight would invert the multiplier (and could flip a loss positive), so it is
        // floored at 0 — which also maps a `NaN` weight to "no shaping" rather than poisoning
        // every losing row.
        let weight =
            if params.loss_quality_weight > 0.0 { params.loss_quality_weight } else { 0.0 };
        base * (1.0 + weight * (1.0 - quality))
    }
}

/// Build the composite multi-metric [`Objective`] from its params — the boxed form
/// `run_sweep_with` ranks by (`--rank-by multi` in the `backtest` bin uses the default params).
pub fn multi_metric(params: MultiMetricParams) -> Objective {
    Box::new(move |report| multi_metric_score(report, &params))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finite, healthy baseline report the tests perturb one field at a time.
    fn report(total_return: f64, max_dd: f64, pf: f64, win_rate: f64, n: usize) -> BacktestReport {
        BacktestReport {
            name: None,
            final_equity: 1000.0 * (1.0 + total_return),
            total_return,
            n_trades: n,
            win_rate,
            sharpe: 1.0,
            max_drawdown: max_dd,
            profit_factor: pf,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        }
    }

    // --- trade_count_penalty ---

    #[test]
    fn penalty_floor_holds_at_zero_trades() {
        let p = MultiMetricParams::default();
        // n = 0: 1 - 50/50 = 0.0 -> floored at 0.1.
        assert_eq!(trade_count_penalty(0, &p), 0.1);
        // n = 1: 1 - 49/50 = 0.02 -> still below the floor.
        assert_eq!(trade_count_penalty(1, &p), 0.1);
    }

    #[test]
    fn penalty_is_monotone_nondecreasing_in_n() {
        let p = MultiMetricParams::default();
        let mut prev = trade_count_penalty(0, &p);
        for n in 1..=2 * p.target_trades {
            let cur = trade_count_penalty(n, &p);
            assert!(cur >= prev, "penalty must not decrease: n={n} gave {cur} < {prev}");
            prev = cur;
        }
    }

    #[test]
    fn penalty_target_boundary() {
        let p = MultiMetricParams::default();
        // Exactly at target: both branches agree on 1.0.
        assert_eq!(trade_count_penalty(p.target_trades, &p), 1.0);
        // One below: 1 - 1/50 = 0.98 (above the floor, on the linear ramp).
        let one_below = trade_count_penalty(p.target_trades - 1, &p);
        assert!((one_below - 0.98).abs() < 1e-12, "got {one_below}");
        // Above target: flat 1.0, no bonus.
        assert_eq!(trade_count_penalty(p.target_trades + 1, &p), 1.0);
        assert_eq!(trade_count_penalty(10 * p.target_trades, &p), 1.0);
    }

    #[test]
    fn penalty_zero_target_disables() {
        let p = MultiMetricParams { target_trades: 0, ..Default::default() };
        assert_eq!(trade_count_penalty(0, &p), 1.0);
        assert_eq!(trade_count_penalty(7, &p), 1.0);
    }

    // --- multi_metric_score ---

    #[test]
    fn score_matches_hand_computed_formula() {
        let p = MultiMetricParams::default();
        let r = report(0.30, 0.10, 2.0, 0.6, 50);
        // (0.30 - 0.10) * ln(3) * 0.6 * 1.0
        // `libm::log`, matching `shape` — these three expectations assert at `1e-12`, so a
        // last-bit platform/`libm` disagreement would never have reddened them; spelling them the
        // same way is hygiene, and it makes the comparison exact rather than merely tolerant.
        let want = 0.2 * libm::log(3.0) * 0.6;
        let got = multi_metric_score(&r, &p);
        assert!((got - want).abs() < 1e-12, "got {got}, want {want}");
    }

    #[test]
    fn score_monotone_in_profit_factor_below_cap() {
        let p = MultiMetricParams::default();
        let lo = multi_metric_score(&report(0.30, 0.10, 1.5, 0.6, 50), &p);
        let hi = multi_metric_score(&report(0.30, 0.10, 3.0, 0.6, 50), &p);
        assert!(hi > lo, "bigger profit factor must score higher: {hi} <= {lo}");
    }

    #[test]
    fn score_monotone_in_trade_count_up_to_target() {
        let p = MultiMetricParams::default();
        let base = |n| multi_metric_score(&report(0.30, 0.10, 2.0, 0.6, n), &p);
        assert!(base(40) > base(20), "more trades (below target) must score higher");
        assert_eq!(base(50), base(80), "at/above target the penalty is flat 1.0");
    }

    #[test]
    fn inf_profit_factor_is_capped_finite_never_wins_by_infinity() {
        let p = MultiMetricParams::default();
        let degenerate = report(0.05, 0.0, f64::INFINITY, 1.0, 1);
        let got = multi_metric_score(&degenerate, &p);
        assert!(got.is_finite(), "inf sentinel must be clamped, got {got}");
        // Exactly the capped formula: 0.05 * ln(101) * 1.0 * 0.1 (`libm::log`, matching `shape`).
        let want = 0.05 * libm::log(101.0) * 0.1;
        assert!((got - want).abs() < 1e-12, "got {got}, want {want}");
    }

    #[test]
    fn nan_inputs_propagate_to_nan_score() {
        let p = MultiMetricParams::default();
        // Profitable regime.
        assert!(multi_metric_score(&report(f64::NAN, 0.1, 2.0, 0.5, 50), &p).is_nan());
        assert!(multi_metric_score(&report(0.3, 0.1, f64::NAN, 0.5, 50), &p).is_nan());
        assert!(multi_metric_score(&report(0.3, 0.1, 2.0, f64::NAN, 50), &p).is_nan());
        // Losing regime: the amplifying branch must not launder a NaN into a rankable number.
        assert!(multi_metric_score(&report(-0.3, 0.1, f64::NAN, 0.5, 50), &p).is_nan());
        assert!(multi_metric_score(&report(-0.3, 0.1, 2.0, f64::NAN, 50), &p).is_nan());
        assert!(multi_metric_score(&report(-0.3, f64::NAN, 2.0, 0.5, 50), &p).is_nan());
        // ... including when the weight knob is off (0.0 * NaN is still NaN).
        let no_shaping = MultiMetricParams { loss_quality_weight: 0.0, ..Default::default() };
        assert!(multi_metric_score(&report(-0.3, 0.1, f64::NAN, 0.5, 50), &no_shaping).is_nan());
    }

    #[test]
    fn losing_run_scores_negative() {
        let p = MultiMetricParams::default();
        // total_return < max_drawdown -> profit_net_of_drawdown < 0 -> negative score.
        let got = multi_metric_score(&report(-0.10, 0.20, 0.5, 0.3, 60), &p);
        assert!(got < 0.0, "a net-losing run must score below zero, got {got}");
    }

    // --- the sign law (property-style; see the module doc) ---

    /// Every field of [`MultiMetricParams`] is `pub`, so the sign law has to survive a RESHAPED
    /// objective, not just the default one. These are the sane reshapings a caller might pick
    /// (the degenerate/hostile ones — negative cap, negative weight — have their own tests).
    fn param_variants() -> Vec<(&'static str, MultiMetricParams)> {
        let d = MultiMetricParams::default();
        vec![
            ("default", d),
            ("winrate_floor", MultiMetricParams { winrate_floor: 0.25, ..d }),
            ("floored_both", MultiMetricParams { winrate_floor: 0.5, penalty_floor: 0.5, ..d }),
            ("no_loss_shaping", MultiMetricParams { loss_quality_weight: 0.0, ..d }),
            ("heavy_loss_shaping", MultiMetricParams { loss_quality_weight: 3.0, ..d }),
            ("no_trade_penalty", MultiMetricParams { target_trades: 0, ..d }),
            ("tight_pf_cap", MultiMetricParams { profit_factor_cap: 5.0, ..d }),
        ]
    }

    /// THE law: over a broad deterministic grid — AND across every sane parameterization — EVERY
    /// losing run scores strictly below EVERY profitable one. Pre-fix, the pure-multiplicative
    /// form let sub-1 shaping factors shrink a negative base toward zero, so a catastrophic loser
    /// could outrank a mild one — and, on an all-losing grid, be crowned #1.
    #[test]
    fn sign_law_every_loser_scores_below_every_winner() {
        for (label, p) in param_variants() {
            let (mut worst_winner, mut best_loser) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut winners, mut losers) = (0usize, 0usize);
            for &tr in &[-0.95, -0.30, -0.05, -0.001, 0.0, 0.001, 0.05, 0.40, 2.00] {
                for &dd in &[0.0, 0.01, 0.20, 0.60] {
                    for &pf in &[0.0, 0.30, 1.00, 1.70, 5.00, f64::INFINITY] {
                        for &wr in &[0.0, 0.10, 0.50, 0.90, 1.00] {
                            for &n in &[0usize, 1, 7, 50, 500] {
                                let s = multi_metric_score(&report(tr, dd, pf, wr, n), &p);
                                assert!(s.is_finite(), "{label}: finite in, finite out: {s}");
                                let base = tr - dd;
                                if base > 0.0 {
                                    winners += 1;
                                    worst_winner = worst_winner.min(s);
                                } else if base < 0.0 {
                                    losers += 1;
                                    best_loser = best_loser.max(s);
                                }
                            }
                        }
                    }
                }
            }
            assert!(winners > 0 && losers > 0, "{label}: grid must cover both regimes");
            assert!(best_loser < 0.0, "{label}: every loser is strictly negative: {best_loser}");
            assert!(worst_winner >= 0.0, "{label}: no winner may go negative: {worst_winner}");
            assert!(
                best_loser < worst_winner,
                "{label}: sign law — best loser {best_loser} must rank below worst winner \
                 {worst_winner}"
            );
        }
    }

    /// Within the LOSING regime, ranking must stay monotone in loss size at fixed quality — i.e.
    /// the amplification may reorder losses only within a bounded band, never inverting a
    /// materially bigger loss above a smaller one. Checked across every parameterization.
    #[test]
    fn losing_regime_is_monotone_in_loss_size_at_fixed_quality() {
        for (label, p) in param_variants() {
            let mut prev = f64::INFINITY;
            // Ever-deeper losses, all other fields identical -> quality is constant.
            for i in 1..=100 {
                let tr = -(i as f64) * 0.01;
                let s = multi_metric_score(&report(tr, 0.05, 1.1, 0.4, 30), &p);
                assert!(s < 0.0, "{label}: a loss must score negative: {tr} -> {s}");
                assert!(s < prev, "{label}: deeper loss must score lower: {tr} -> {s} >= {prev}");
                prev = s;
            }
        }
    }

    /// The review's concrete failure, pinned: an all-losing point with the flattering all-zero
    /// stats (win_rate 0 -> winrate_coeff 0, profit_factor 0 -> ln(1) = 0) scored exactly `-0.0`
    /// under the old formula — ABOVE every mildly-losing point. It must now rank last.
    #[test]
    fn catastrophic_loss_ranks_below_a_mild_loss() {
        let p = MultiMetricParams::default();
        let catastrophic = multi_metric_score(&report(-0.95, 0.95, 0.0, 0.0, 200), &p);
        let mild = multi_metric_score(&report(-0.01, 0.02, 0.9, 0.45, 200), &p);
        assert!(catastrophic < 0.0, "the -95% point must not collapse to zero: {catastrophic}");
        assert!(
            catastrophic < mild,
            "a -95% wipeout must rank below a -1% loss: {catastrophic} >= {mild}"
        );
    }

    /// The same inversion via the trade-count term: two equally-losing points, the thin `n = 1`
    /// sample (penalty 0.1) used to outrank the `n = 100` one because the smaller penalty shrank
    /// its negative score. The thin sample must rank BELOW.
    #[test]
    fn thin_sample_ranks_below_thick_sample_when_both_lose() {
        let p = MultiMetricParams::default();
        let thin = multi_metric_score(&report(-0.10, 0.15, 1.2, 0.5, 1), &p);
        let thick = multi_metric_score(&report(-0.10, 0.15, 1.2, 0.5, 100), &p);
        assert!(thin < thick, "1-trade sample must not outrank a 100-trade one: {thin} >= {thick}");
    }

    /// Monotone in profit, in BOTH regimes and across the boundary: with every other field held
    /// fixed, a bigger `total_return` always scores strictly higher.
    #[test]
    fn score_is_strictly_monotone_in_profit() {
        let p = MultiMetricParams::default();
        let mut prev = f64::NEG_INFINITY;
        for i in 0..=200 {
            let total_return = -1.0 + i as f64 * 0.01; // -1.00 ..= +1.00, straddling breakeven
            let s = multi_metric_score(&report(total_return, 0.10, 2.0, 0.6, 50), &p);
            assert!(s > prev, "score must rise with total_return: {total_return} -> {s} <= {prev}");
            prev = s;
        }
    }

    /// The losing regime's amplification is BOUNDED by `loss_quality_weight`: worst quality costs
    /// exactly `1 + weight` times the raw loss, perfect quality costs exactly the raw loss, and
    /// `weight = 0` ranks losers purely by profit-net-of-drawdown.
    #[test]
    fn loss_amplification_is_bounded_by_its_weight() {
        let p = MultiMetricParams::default(); // weight 1.0 -> at worst 2x
        let base = -0.10 - 0.15;
        // Worst quality: no winners (pf 0, win_rate 0) and a zero-trade sample (penalty floor).
        let worst = multi_metric_score(&report(-0.10, 0.15, 0.0, 0.0, 0), &p);
        assert!((worst - 2.0 * base).abs() < 1e-12, "worst quality doubles the loss: {worst}");
        // Perfect quality: profit factor at the cap, 100% win rate, at/above the trade target.
        let best = multi_metric_score(&report(-0.10, 0.15, p.profit_factor_cap, 1.0, 50), &p);
        assert!((best - base).abs() < 1e-12, "perfect quality keeps the raw loss: {best}");
        // ... and never crosses zero, however good the quality.
        assert!(best < 0.0);

        let flat = MultiMetricParams { loss_quality_weight: 0.0, ..Default::default() };
        assert_eq!(multi_metric_score(&report(-0.10, 0.15, 0.0, 0.0, 0), &flat), base);
        assert_eq!(multi_metric_score(&report(-0.10, 0.15, 100.0, 1.0, 50), &flat), base);
    }

    /// A negative `loss_quality_weight` is treated as `0.0` — it would otherwise invert the
    /// multiplier and could flip a loss to a positive score, breaking the sign law.
    #[test]
    fn negative_loss_weight_cannot_flip_a_loss_positive() {
        let hostile = MultiMetricParams { loss_quality_weight: -5.0, ..Default::default() };
        let got = multi_metric_score(&report(-0.10, 0.15, 0.0, 0.0, 0), &hostile);
        assert_eq!(got, -0.25, "a negative weight degrades to no shaping, not to a sign flip");
    }

    /// An out-of-range `profit_factor` (only reachable from a hand-built report — `metrics::
    /// profit_factor` is always `>= 0`) cannot make `shape` negative and flip a PROFITABLE run
    /// below a losing one: `shape` is clamped into `[0, shape_max]`.
    #[test]
    fn out_of_range_profit_factor_cannot_break_the_sign_law() {
        let p = MultiMetricParams::default();
        // pf = -0.5 -> ln(0.5) < 0 unclamped.
        let winner = multi_metric_score(&report(0.30, 0.10, -0.5, 0.6, 50), &p);
        assert_eq!(winner, 0.0, "clamped to the zero plateau, never negative: {winner}");
        // An absurd winrate_floor (pushing winrate_coeff past 1) cannot lift `shape` past its cap.
        let hot = MultiMetricParams { winrate_floor: 10.0, ..Default::default() };
        let capped = multi_metric_score(&report(0.30, 0.10, 100.0, 0.5, 50), &hot);
        let want = 0.20 * libm::log(101.0); // base * shape_max, not base * 5.5 * ln(101)
        assert!((capped - want).abs() < 1e-12, "shape is capped at shape_max: {capped}");
    }

    /// A degenerate `profit_factor_cap` can neither panic (`clamp`'s bounds must stay ordered)
    /// nor break the sign law. `cap = 0` collapses the shaping to zero; a nonsense NEGATIVE cap
    /// makes `ln` of a non-positive number, i.e. every row scores `NaN` = unrankable — loud and
    /// sorted last, rather than silently plausible.
    #[test]
    fn degenerate_profit_factor_cap_is_inert_not_a_panic() {
        let zero_cap = MultiMetricParams { profit_factor_cap: 0.0, ..Default::default() };
        assert_eq!(multi_metric_score(&report(0.30, 0.10, 2.0, 0.6, 50), &zero_cap), 0.0);
        // Losing side: quality collapses to 0, so the loss takes the full 1 + weight multiplier.
        assert_eq!(multi_metric_score(&report(-0.10, 0.15, 2.0, 0.6, 50), &zero_cap), -0.50);

        let neg_cap = MultiMetricParams { profit_factor_cap: -3.0, ..Default::default() };
        assert!(multi_metric_score(&report(0.30, 0.10, 2.0, 0.6, 50), &neg_cap).is_nan());
        assert!(multi_metric_score(&report(-0.10, 0.15, 2.0, 0.6, 50), &neg_cap).is_nan());

        // A NaN cap must NOT poison every score: the pf clamp's `>` is false, and shape_max
        // floors at 0, so the shaping simply goes inert.
        let nan_cap = MultiMetricParams { profit_factor_cap: f64::NAN, ..Default::default() };
        assert_eq!(multi_metric_score(&report(0.30, 0.10, 2.0, 0.6, 50), &nan_cap), 0.0);
    }

    #[test]
    fn winrate_floor_softens_zero_winrate() {
        let hard = MultiMetricParams::default(); // winrate_floor 0.0
        let soft = MultiMetricParams { winrate_floor: 0.25, ..Default::default() };
        let r = report(0.30, 0.10, 2.0, 0.0, 50);
        assert_eq!(multi_metric_score(&r, &hard), 0.0, "default: 0% win rate zeroes the score");
        assert!(multi_metric_score(&r, &soft) > 0.0, "floored: score survives");
    }

    #[test]
    fn boxed_objective_equals_pure_score() {
        let p = MultiMetricParams::default();
        let obj = multi_metric(p);
        let r = report(0.30, 0.10, 2.0, 0.6, 42);
        assert_eq!(obj(&r), multi_metric_score(&r, &p));
    }
}
