//! Pluggable sweep-ranking objectives: a scalar score over a [`BacktestReport`], HIGHER is
//! better. The parameter-sweep runner (`harness::sweep::run_paramscan_with`) ranks its rows by an
//! [`Objective`]; the existing `RankMetric` variants are exposed as built-in constructors
//! (`RankMetric::objective`, in `harness::sweep` — direction folded in, so e.g. max-drawdown's
//! objective is `-max_drawdown`), and this module adds the composite [`multi_metric`] objective.
//!
//! FEATURE-FREE by design (like `vike_analytics::report`, which it scores): nothing here names the gated
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

use vike_analytics::report::BacktestReport;

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
/// `run_paramscan_with` ranks by (`--rank-by multi` in the `backtest` bin uses the default params).
pub fn multi_metric(params: MultiMetricParams) -> Objective {
    Box::new(move |report| multi_metric_score(report, &params))
}

#[path = "objective_tests.rs"]
#[cfg(test)]
mod objective_tests;
