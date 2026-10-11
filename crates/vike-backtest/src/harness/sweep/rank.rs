//! How a sweep's rows rank and render: the metrics, one row, the report, and the trial-matrix opt-in.

use std::fmt;

use vike_analytics::report::BacktestReport;

use crate::search::objective::Objective;

#[cfg(doc)]
use super::{ParamscanPoint, run_paramscan, run_paramscan_with};
#[cfg(doc)]
use crate::harness::{HarnessError, run_backtest};

/// Which [`BacktestReport`] metric ranks a sweep's rows, and in which direction "better" sorts.
/// Sharpe/`TotalReturn`/`FinalEquity` rank descending (bigger is better); `MaxDrawdown` ranks
/// ascending (a smaller drawdown is better).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankMetric {
    #[default]
    Sharpe,
    TotalReturn,
    MaxDrawdown,
    FinalEquity,
}

impl RankMetric {
    /// Case-insensitive CLI parse: `"sharpe"|"return"|"max_dd"|"equity"`. Returns `None` for
    /// anything else — the caller (the sweep bin) turns that into a usage error.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sharpe" => Some(RankMetric::Sharpe),
            "return" => Some(RankMetric::TotalReturn),
            "max_dd" => Some(RankMetric::MaxDrawdown),
            "equity" => Some(RankMetric::FinalEquity),
            _ => None,
        }
    }

    /// The metric value a [`BacktestReport`] contributes for this ranking — the raw number,
    /// direction handled separately by `sort_key` (and, on the runtime path, folded into
    /// [`RankMetric::objective`] instead).
    fn value(self, report: &BacktestReport) -> f64 {
        match self {
            RankMetric::Sharpe => report.sharpe,
            RankMetric::TotalReturn => report.total_return,
            RankMetric::MaxDrawdown => report.max_drawdown,
            RankMetric::FinalEquity => report.final_equity,
        }
    }

    /// A sort key where SMALLER is better, for every metric — descending metrics get negated,
    /// `MaxDrawdown` (smaller is better) passes through as-is. A NaN key (e.g. a Sharpe over a
    /// zero-variance curve) is ordered LAST among successes by `cmp_reports`, so a degenerate point
    /// can never rank first.
    ///
    /// ⚠ TEST-ONLY since the optimizer seam landed. The runtime path ranks through
    /// [`RankMetric::objective`] + `optimize::report_from_outcome`; this pair survives ONLY as the
    /// oracle `metric_objectives_rank_identically_to_cmp_reports` compares that ordering against,
    /// which is what makes the retirement a proof rather than a claim.
    #[cfg(test)]
    fn sort_key(self, report: &BacktestReport) -> f64 {
        match self {
            RankMetric::MaxDrawdown => self.value(report),
            _ => -self.value(report),
        }
    }

    /// Compare two successful reports best-first for this metric. Unlike a raw
    /// `partial_cmp(...).unwrap_or(Equal)`, a NaN sort key always sorts LAST (worst) — never first —
    /// so a NaN-Sharpe grid point can't win a sweep and feed downstream selection (e.g. DSR).
    ///
    /// ⚠ TEST-ONLY — see `sort_key` above for why it is kept.
    #[cfg(test)]
    pub(super) fn cmp_reports(self, a: &BacktestReport, b: &BacktestReport) -> std::cmp::Ordering {
        let (ka, kb) = (self.sort_key(a), self.sort_key(b));
        match (ka.is_nan(), kb.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater, // a's key is NaN → a is worse → sorts last
            (false, true) => std::cmp::Ordering::Less,    // b's key is NaN → b is worse
            (false, false) => ka.partial_cmp(&kb).expect("non-NaN f64s compare totally"),
        }
    }

    /// This metric's CLI/report name — the public twin of the internal [`RankMetric::label`], for
    /// callers (the `backtest` bin's euler path) that must label an objective-ranked report with
    /// the metric it was built from.
    pub fn name(self) -> &'static str {
        self.label()
    }

    fn label(self) -> &'static str {
        match self {
            RankMetric::Sharpe => "sharpe",
            RankMetric::TotalReturn => "return",
            RankMetric::MaxDrawdown => "max_dd",
            RankMetric::FinalEquity => "equity",
        }
    }

    /// This metric as a higher-is-better [`Objective`] — the built-in constructors of the
    /// objective seam. Direction is folded in (`MaxDrawdown` becomes `-max_drawdown`), so ranking
    /// by `self.objective()` through [`run_paramscan_with`] orders rows EXACTLY like the classic
    /// [`run_paramscan`] comparator, NaN-last included (see the
    /// `metric_objectives_rank_identically_to_cmp_reports` regression test).
    pub fn objective(self) -> Objective {
        match self {
            RankMetric::MaxDrawdown => Box::new(move |r: &BacktestReport| -self.value(r)),
            _ => Box::new(move |r: &BacktestReport| self.value(r)),
        }
    }
}

/// What ranked a [`ParamscanReport`]: a built-in [`RankMetric`] (the classic `--rank-by` names —
/// serializes as the same bare string as before, so default JSON is unchanged) or a named
/// [`Objective`] (serializes as its label, e.g. `"multi"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum RankBy {
    Metric(RankMetric),
    Objective(String),
}

impl RankBy {
    /// The human label the report header prints: the metric's own label, or the objective's name.
    pub fn label(&self) -> &str {
        match self {
            RankBy::Metric(m) => m.label(),
            RankBy::Objective(name) => name,
        }
    }
}

/// One row of a [`ParamscanReport`]: the overrides that produced this point, plus either its
/// [`BacktestReport`] (success) or the stringified [`HarnessError`] (failure) — never both.
/// `score` is stamped only by the objective path ([`run_paramscan_with`]); the classic
/// [`run_paramscan`] leaves it `None`, which is skipped on serialization — so default `--json`
/// output is unchanged.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamscanRow {
    pub overrides: Vec<(String, toml::Value)>,
    pub report: Option<BacktestReport>,
    pub error: Option<String>,
    /// The [`Objective`] score of this row's report (higher is better), objective path only.
    /// A non-finite ("unrankable") score serializes as `null` — see [`ser_opt_score`].
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_score")]
    pub score: Option<f64>,
}

/// Serialize a stamped score, mapping a non-finite value (a NaN "unrankable" score) to JSON
/// `null` — `None` never reaches here (it is skipped at the field level).
///
/// NB serde_json already nulls non-finite floats on its own (only float MAP KEYS are an error
/// there), so this is not a rescue from a serialization failure: it PINS that shape as the
/// documented contract — `"score": null` means unrankable — independent of the serializer backend
/// (a format that hard-errors on non-finite floats would otherwise break `--json`).
fn ser_opt_score<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) if x.is_finite() => s.serialize_f64(*x),
        _ => s.serialize_none(),
    }
}

/// The ranked result of a whole sweep: every [`ParamscanPoint`] run through [`run_backtest`], sorted
/// best-first by `rank_by` (failed rows always sort last).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamscanReport {
    pub rows: Vec<ParamscanRow>,
    pub rank_by: RankBy,
    /// The METHOD's own cost line — euler's budget, tpe's trial line — or `None` for the grid,
    /// which says nothing about itself and never has.
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing.** The grid path leaves this `None`, so a grid
    /// document is byte-identical to the one this crate emitted before the field existed — which is
    /// what lets `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
    /// `profile_sweep_is_byte_identical_local_and_remote` keep comparing bytes.
    ///
    /// ⚠ It exists because a REMOTE search had nowhere to report its cost. The engine binary prints
    /// `super::Optimized::summary` to stderr and always has; a client reading
    /// `vike_datahub_client::proto::Response::ParamscanReport` sees only this document, so before stage
    /// 7 a remote euler or tpe run reported its budget nowhere at all. `Display` deliberately does
    /// NOT render it — the engine prints it separately, and rendering it here too would double the
    /// line on the one surface that already had it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// **How many BUCKETS a trial's retained return vector holds — the opt-in that makes a trial
/// MATRIX possible at all, and `0` ([`ReturnBuckets::DISARMED`]) is the default.**
///
/// ⚠ **What this exists to dissolve.** A trial's equity curve is destroyed inside
/// `row_from_outcome`: it consumes the [`vike_analytics::BacktestResult`] to derive [`BacktestReport`]'s
/// scalars and drops `equity_curve`, `equity_ts`, `trades` and `per_symbol_curves`. So
/// `vike_analytics::overfit::pbo_cscv` and `deflated_sharpe_with_effective_n`, which need an
/// N-trial matrix of per-observation performance, had nothing to read — and
/// `crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_keep_trials` refused
/// `--keep-trials series` by name, arguing that retaining curves raises peak RSS on a box whose
/// concurrency cap is already `DEFAULT_SWEEP_THREADS` because each point materialises its own
/// data slice.
///
/// ⚠ **The measurement that dissolves it: the statistic does not want the CURVE.** `pbo_cscv`
/// splits `T` observations into `n_splits` contiguous blocks and compares IN-sample against
/// OUT-of-sample block means, so it needs `T >= n_splits` — sixteen in practice
/// ([`super::optimize::DEFAULT_CSCV_SPLITS`]) — and nothing more. A FIXED-SIZE bucketed return
/// vector is therefore the same instrument at a fortieth of the cost, and the arithmetic is pinned
/// by `bucketed_returns_cost_a_fortieth_of_a_curve` rather than asserted here.
///
/// ⚠ **Why 512 and not 16, 64 or 20 000.** Three constraints meet at a power of two:
/// * `T` must clear `n_splits` with room for the blocks to be a SAMPLE rather than a point each —
///   at `T = 16` every CSCV block is ONE observation and a block mean is that observation, so the
///   in-sample/out-of-sample comparison measures single-bucket noise;
/// * `512 = 2^9` is divisible by every power-of-two split count up to itself, so
///   `pbo_cscv`'s `g * t / n_splits` bounds land on exact boundaries and no group is short —
///   a ragged final block weights one CSCV group differently from the rest;
/// * it is far below a real run's bar count, so bucketing is a genuine DOWNSAMPLE (each bucket
///   compounds many bars) rather than an upsample that would have to fabricate observations.
///
/// ⚠ **Bucketing does not deflate the significance test, and that is why `n_obs` may be `T`.**
/// A bucket return compounds `b` bar returns, so its Sharpe scales as `sr_bar * sqrt(b)` while the
/// PSR's own `sqrt(n - 1)` factor shrinks as `sqrt(n_bars / b)` — the product
/// `sr_per_obs * sqrt(n - 1)` is invariant under bucketing for iid returns. What DOES move is the
/// third and fourth moments (compounding pulls them toward Gaussian by the CLT), which is a
/// property of the aggregation FREQUENCY the deflated-Sharpe literature already treats as a
/// modelling choice — daily versus monthly — rather than an error.
///
/// A `Copy` newtype with a DISARMED constant rather than a bare `usize`, for exactly the reason
/// [`super::optimize::TradeFloor`] is one: it is a consuming-builder argument on
/// [`super::optimize::StoreEvaluator`], so "unchanged" is the value nobody has to write and an
/// unarmed search stays byte-identical BY CONSTRUCTION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReturnBuckets(usize);

impl ReturnBuckets {
    /// Retain nothing. Byte-identical to this type never having existed — see the type doc.
    pub const DISARMED: ReturnBuckets = ReturnBuckets(0);

    /// The bucket count `--keep-trials returns` arms. The type doc argues the number.
    pub const DEFAULT_BUCKETS: usize = 512;

    /// [`ReturnBuckets::DEFAULT_BUCKETS`] buckets — what the CLI's opt-in installs.
    pub const DEFAULT: ReturnBuckets = ReturnBuckets(Self::DEFAULT_BUCKETS);

    /// `buckets` buckets. `0` builds [`ReturnBuckets::DISARMED`], so there is one meaning for "off".
    pub fn new(buckets: usize) -> Self {
        ReturnBuckets(buckets)
    }

    /// The requested count — what a document reports as its `T`.
    pub fn buckets(self) -> usize {
        self.0
    }

    /// Whether anything is retained at all.
    pub fn is_armed(self) -> bool {
        self.0 > 0
    }

    /// One trial's equity curve as AT MOST [`ReturnBuckets::buckets`] contiguous block returns, or
    /// `None` when this curve cannot produce an admissible column.
    ///
    /// ⚠ **Block boundaries, not a resample, and the difference is that this reads B+1 points
    /// instead of walking the curve.** The return of block `[a, b]` is `E[b] / E[a] - 1`, which is
    /// exactly the product of the per-bar returns inside it — so a one-million-bar curve is
    /// bucketed by 513 index reads and one divide each, and nothing proportional to the curve's
    /// length is allocated or scanned. `vike_model::runs::decimate` — what
    /// `crates/vike-backtest/src/backtest_cli/run_record.rs`'s `run_series_from` uses for a SINGLE
    /// run's record — SAMPLES instead, which is right for a picture of a curve and wrong here: a
    /// sampled point pair would drop the compounding between them.
    ///
    /// ⚠ **The effective count is `min(buckets, n_returns)`, so no bucket is ever EMPTY.** A
    /// shorter range yields a smaller `T` rather than a vector padded with fabricated zeros — a
    /// zero return is a real observation to every statistic downstream, and inventing 300 of them
    /// would move the PBO of a 200-bar run toward "no overfit" on evidence that does not exist.
    /// Every trial of one search runs the SAME `[data]` range (a `[paramscan]` axis overrides
    /// `strategy.params`, never the data window), so one search's columns share one `T` and the
    /// matrix is rectangular by construction; `super::optimize::overfit_stats` still refuses a
    /// column of a different length rather than trusting that.
    ///
    /// ⚠ **A non-finite block return is `None` for the WHOLE trial, not a `NaN` in the column.**
    /// `pbo_cscv` answers `NaN` if any cell anywhere is non-finite, so one trial whose equity
    /// touched exactly `0.0` at a boundary would otherwise take the statistic down for all 500.
    /// Refusing the column is the honest version of the same fact, and the excluded count is
    /// reported.
    pub(crate) fn capture(self, curve: &[f64]) -> Option<Vec<f64>> {
        if !self.is_armed() {
            return None;
        }
        // `n - 1` per-bar returns are available; a bucket needs at least one.
        let n = curve.len();
        if n < 2 {
            return None;
        }
        let span = n - 1;
        let t = self.0.min(span);
        let mut out = Vec::with_capacity(t);
        // `floor(k * span / t)` is STRICTLY increasing because `span / t >= 1`, so the boundaries
        // never repeat and no bucket spans zero bars.
        let mut prev = curve[0];
        for k in 1..=t {
            let idx = k * span / t;
            let cur = curve[idx];
            if prev == 0.0 {
                return None;
            }
            let r = cur / prev - 1.0;
            if !r.is_finite() {
                return None;
            }
            out.push(r);
            prev = cur;
        }
        Some(out)
    }
}

/// Render one row's `k=v` overrides joined by spaces, e.g. `size=2.0 threshold=0.1`. Empty for a
/// no-sweep single point.
fn format_overrides(overrides: &[(String, toml::Value)]) -> String {
    overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

impl fmt::Display for ParamscanReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "sweep ranked by {} ({} points)", self.rank_by.label(), self.rows.len())?;
        for (i, row) in self.rows.iter().enumerate() {
            let rank = if i == 0 { "*#1".to_string() } else { format!("#{}", i + 1) };
            let overrides = format_overrides(&row.overrides);
            match &row.report {
                Some(r) => {
                    write!(
                        f,
                        "{rank:<4} {overrides:<30} ret={:.4} sharpe={:.4} max_dd={:.4} trades={}",
                        r.total_return, r.sharpe, r.max_drawdown, r.n_trades
                    )?;
                    // Objective path only — the classic run_paramscan never stamps a score, so the
                    // default table stays byte-identical.
                    if let Some(score) = row.score {
                        write!(f, " score={score:.4}")?;
                    }
                    // A zero-trade / flat row (see `zero_trade::ZeroTradeReport::analyze`) surfaces
                    // its top probable cause inline. `zero_trade` is `None` for any row that traded
                    // or moved equity, so a normal sweep table is byte-identical.
                    if let Some(top) = r.zero_trade.as_ref().and_then(|zt| zt.causes.first()) {
                        write!(f, " -- {}", top.headline)?;
                    }
                    writeln!(f)?;
                }
                None => writeln!(
                    f,
                    "{rank:<4} {overrides:<30} FAILED: {}",
                    row.error.as_deref().unwrap_or("(unknown error)")
                )?,
            }
        }
        Ok(())
    }
}
