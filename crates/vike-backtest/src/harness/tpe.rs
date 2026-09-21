//! TPE (Tree-structured Parzen Estimator) Bayesian optimization over a `[sweep]` profile — the
//! ask/tell alternative to the exhaustive cartesian grid (`harness::sweep`) and the
//! successive-halving refinement (`harness::euler`). It converges on good `strategy.params` in far
//! fewer backtests by MODELLING which regions of the space produce good scores and proposing there,
//! instead of enumerating the whole grid.
//!
//! CLEAN-ROOM from the mathematics of Bergstra, Bardenet, Bengio & Kégl, *"Algorithms for
//! Hyper-Parameter Optimization"* (NeurIPS 2011), §4 — NO AGPL/Optuna/Hyperopt/Jesse source was
//! consulted. The algorithm, restated for our MAXIMIZE convention (the objective seam is
//! higher-is-better; the paper minimizes a loss `y`, so "good" = high score here):
//!
//! ```text
//! 1. Keep the observation history {(x_i, score_i)}.
//! 2. Split it at the gamma quantile of score: the top `gamma` fraction is GOOD, the rest BAD.
//! 3. Per parameter (INDEPENDENTLY — the TPE factorization), build two Parzen density estimates:
//!        l(x) = p(x | good)      g(x) = p(x | bad)
//! 4. Expected Improvement is monotone in the ratio l(x)/g(x) (paper Eq. 2:
//!        EI ∝ (gamma + (1-gamma)·g/l)^-1  ), so propose x that MAXIMIZES l(x)/g(x).
//!    CONTINUOUS: draw `n_candidates` samples from l(x) and return the argmax of l(x)/g(x) (each
//!    sample is a fresh point). DISCRETE: draw the next category directly from l(x) — over a small
//!    fixed category set the argmax would lock onto the incumbent and never try an unevaluated
//!    category, so l-sampling (which keeps the prior's exploration mass) is used instead.
//! ```
//!
//! Densities (paper §4.1): a CONTINUOUS parameter uses an *adaptive Parzen estimator* — one
//! Gaussian per good (resp. bad) observation plus a broad uniform-like prior Gaussian at the domain
//! midpoint; each kernel's bandwidth is the **max distance to its sorted neighbours** (the standard
//! rule), clipped so it can neither collapse to a spike nor exceed the domain width. A DISCRETE
//! parameter uses a **prior-smoothed weighted frequency** histogram (add-`prior_weight` Laplace
//! smoothing over the categories). The first `n_startup` asks are seeded-random to seed the model.
//!
//! DETERMINISM CONTRACT (hard — this codebase forbids wall-clock/`Math.random` in its determinism
//! regime): every stochastic step draws from a seeded [`SplitMix64`] owned by the optimizer, so
//! **same seed + same space + same data ⇒ identical proposal sequence**, byte-for-byte (pinned by
//! `same_seed_same_proposals`). `SplitMix64` is used rather than `rand`'s `StdRng` deliberately:
//! `rand` IS a dep, but `StdRng`'s algorithm is not guaranteed stable across versions, whereas this
//! ~10-line generator is fixed forever.
//!
//! ⚠ **CORRECTED 2026-08-28: that contract claimed "byte-for-byte" and did NOT hold ACROSS
//! PLATFORMS.** The integer stream was never the leak — SplitMix64 is exact `u64` arithmetic and is
//! identical on every box. The leak was the two FLOAT transforms sitting on top of it:
//! [`SplitMix64::gaussian`]'s Box–Muller (`ln` + `cos`) and [`gaussian_pdf`]'s `exp`. IEEE 754
//! requires `+ - * /` and `sqrt` to be correctly rounded and requires NOTHING of `ln`/`cos`/`exp`,
//! so an `f64` METHOD call reaches the PLATFORM's libm — MSVC's CRT on the Windows dev box, glibc
//! on the the CI box Linux boxes — and the two disagree in the last bit. Here that difference does not
//! stay last-bit: one perturbed Gaussian draw moves a [`Parzen::sample`] coordinate, which moves
//! which of the `n_candidates` wins the `l(x)/g(x)` argmax, which moves the proposed point, which
//! moves the history every LATER proposal is built from. The sequences do not drift apart, they
//! FORK — so the same seed over the same store searched a different part of the space on the
//! desktop than on the CI box, and a TPE result was not reproducible across the two machines this
//! workspace's operator compares on routinely.
//!
//! ⚠ **And `same_seed_same_proposals` pins NOTHING about that** — read what it does rather than
//! what its name suggests: it runs the same closure TWICE IN ONE PROCESS and compares the two runs,
//! so it is satisfied by any generator that is a pure function of its seed, on any single platform,
//! against any libm whatsoever. `run_tpe_is_deterministic_for_a_fixed_seed` one layer up has the
//! same shape. Neither is a bad test — within-box determinism is exactly what they were written to
//! hold — but a CROSS-platform claim cannot be gated by a test that only ever runs on one platform
//! at a time, and this module's contract had been stating one that had never been measured.
//!
//! All three transcendentals now call the `libm` CRATE, one pure-Rust implementation compiled INTO
//! the binary and therefore the same code on both boxes;
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! accepted verdict and carries the measurement behind it. ⚠ Both `.sqrt()` calls are deliberately
//! left as `f64` methods — IEEE 754 DOES require `sqrt` correctly rounded, so routing it through
//! `libm` would buy nothing and would blur the rule. Nothing golden pins a proposal sequence, so
//! this conversion re-records no constant; it does MOVE the trials a given seed produces, on Linux
//! as well as on Windows, which is the one-time re-baseline that record names as the price.
//!
//! Gated with the rest of `harness/` behind `hist-replay`. The ask/tell core is store-free and
//! pure; only [`run_tpe`] (the driver) touches a `HistStore`, reusing the EXISTING per-config
//! backtest path (`super::run_backtest`) and the objective seam (`crate::objective`) verbatim —
//! nothing about scoring or backtesting is reimplemented here.

use std::sync::Arc;

use indexmap::IndexMap;
use vike_data::HistStore;
use vike_ml::tpe::{ParamDomain, TpeConfig, TpeOptimizer, TpeSpace};

use crate::objective::Objective;

use super::optimize::{
    Candidate, Evaluated, Optimized, Optimizer, PointEvaluator, SearchOutcome, StoreEvaluator,
    optimize,
};
use super::sweep::{
    ParamscanExec, ParamscanReport, ParamscanRow, numeric_sweep_axes, sort_scored_rows,
};
use super::{BacktestProfile, HarnessError};

// ⚠ The ask/tell SAMPLER that stood here is now `vike_ml::tpe`, imported above. This file's own
// module doc had already drawn that line — "the ask/tell core is store-free and pure; only
// `run_tpe` (the driver) touches a `HistStore`" — and the core never named a profile, a store or a
// backtest. What moved is the mathematics; what stayed is its application to a `[sweep]` profile.
// It moved DOWN, to layer 20, so `vike-user-research` (35) can reach it: a study could run one
// backtest through `StudySim` and therefore sweep a grid and nothing cleverer, while the only
// adaptive sampler in the tree sat one layer above it. See `crates/vike-ml/src/tpe.rs`.

/// Read a profile's `[sweep]` table into a [`TpeSpace`]. Each numeric axis becomes a CONTINUOUS
/// range bounded by the axis's min and max (integral axes stay whole) — TPE explores BETWEEN the
/// grid points, the same philosophy as `--optimizer euler`, not merely re-picking them. The shared
/// reader (`sweep::numeric_sweep_axes`) owns the sorted-by-key axis order and the
/// array/non-empty/numeric/type-homogeneity validation (non-numeric or type-mixed axes are a
/// [`HarnessError::Validation`] naming the grid method — the SAME rule as the euler reader, now
/// literally the same code).
fn tpe_space_from_profile(base: &BacktestProfile) -> Result<TpeSpace, HarnessError> {
    // The lane name this reader interpolates into its narrowing errors is `TpeSearch`'s own
    // `name()` — one fact, not a second literal.
    Ok(numeric_sweep_axes(base, TpeSearch::NAME)?
        .into_iter()
        .map(|axis| {
            let lo = axis.values.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = axis.values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (axis.key, ParamDomain::Continuous { lo, hi, integral: axis.integral })
        })
        .collect())
}

/// Render one proposed config as an `optimize::Candidate`: each space parameter's coordinate back in
/// its own TOML type. PURE rendering — the profile BUILD half moved to `optimize::StoreEvaluator`,
/// which is what lets the loop below drop its `?` (and with it the discard-every-collected-row
/// semantics a mid-loop build failure used to have).
fn overrides_for(space: &[(String, ParamDomain)], params: &IndexMap<String, f64>) -> Candidate {
    space
        .iter()
        .map(|(name, domain)| {
            let x = params.get(name).copied().unwrap_or_else(|| domain.default_value());
            // The render lives HERE, not on `ParamDomain`: vike-ml proposes numbers and
            // carries no `toml` dependency, so the crate owning the params table picks the
            // wire type. Same three lines `euler`'s `AxisKind::to_toml` carries, deliberately
            // duplicated for the same reason that one is.
            let v = if domain.integral() {
                toml::Value::Integer(x.round() as i64)
            } else {
                toml::Value::Float(x)
            };
            (name.clone(), v)
        })
        .collect()
}

/// The TPE Bayesian search, as an [`Optimizer`] — the closest fit of the three, because TPE was
/// already factored the way the seam wants: [`TpeOptimizer`] is a store-free, loop-free ask/tell
/// machine and the driver was always thin.
///
/// Batch width is ONE, and that is an ALGORITHMIC property, not a policy: each proposal reads the
/// whole observation history, so there is nothing to hand over in parallel. Combined with
/// `optimize::PointEvaluator`'s pool rule (rayon only at width > 1), TPE's "no fan-out" is now EXACT
/// rather than a documented no-op parameter — it would otherwise build one four-thread pool per
/// single backtest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TpeSearch {
    pub cfg: TpeConfig,
}

impl TpeSearch {
    /// This method's operator-facing name, as a const so [`tpe_space_from_profile`] can interpolate
    /// the SAME string [`Optimizer::name`] answers with.
    const NAME: &'static str = "tpe";

    /// A method value over `cfg`. [`TpeConfig`] is `Copy`.
    pub fn new(cfg: TpeConfig) -> Self {
        TpeSearch { cfg }
    }
}

impl Optimizer for TpeSearch {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    /// The numeric/type-homogeneous narrowing, via the same shared reader euler uses — so a space
    /// failure is now caught BEFORE the store opens rather than at trial 1.
    fn accepts(&self, base: &BacktestProfile) -> Result<(), HarnessError> {
        tpe_space_from_profile(base).map(|_| ())
    }

    /// EXACT: TPE's loop is `for _ in 0..n_trials`, one candidate per turn, with no early stop and
    /// no dedup — so the trial budget IS the evaluation count, and this is the one method whose
    /// total an operator typed themselves (`--trials`).
    fn budget_hint(&self, _base: &BacktestProfile) -> Option<u64> {
        Some(self.cfg.n_trials as u64)
    }

    fn search(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<SearchOutcome, HarnessError> {
        let space = tpe_space_from_profile(base)?;
        let mut opt = TpeOptimizer::new(space.clone(), self.cfg.seed, self.cfg.gamma)
            .with_n_startup(self.cfg.n_startup)
            .with_n_candidates(self.cfg.n_candidates);

        let mut rows: Vec<ParamscanRow> = Vec::with_capacity(self.cfg.n_trials);
        for _ in 0..self.cfg.n_trials {
            let params = opt.ask();
            // ⚠ ONE candidate per call, and `tell` takes the RAW `IndexMap<String, f64>` `ask`
            // returned — never a value parsed back out of the TOML candidate. Owning the loop is
            // what keeps that map in scope: the render below ROUNDS an integral coordinate, so
            // telling the rendered value back would change the model and fork every later proposal.
            let evaluated = eval.evaluate(vec![overrides_for(&space, &params)]);
            let Evaluated { row, score } = evaluated.into_iter().next().expect(
                "a PointEvaluator answers one Evaluated per candidate, in input order (trait \
                 contract)",
            );
            // A failed backtest scores `NaN` (unrankable) — it must never steer the model.
            opt.tell(&params, score);
            rows.push(row);
        }

        // The run summary line, ALREADY RENDERED. ⚠ `best` comes from THE shared ordering rule
        // applied to these rows — never from `TpeOptimizer::best`, which is a THIRD ranking rule
        // (linear scan, strict `>`, earliest wins) that disagrees with the stable sort. It was
        // already dead in the driver and stays dead; the clone is 64 rows against 64 backtests.
        let mut ranked = rows.clone();
        sort_scored_rows(&mut ranked);
        let best = ranked.first().and_then(|r| r.score);
        let summary = format!(
            "tpe: {} trials (seed {}), best score {}",
            self.cfg.n_trials,
            self.cfg.seed,
            best.map(|s| format!("{s:.4}")).unwrap_or_else(|| "n/a".to_string())
        );

        // Rows in EVALUATION order — the trajectory, preserved up to the assembler.
        Ok(SearchOutcome { rows, summary: Some(summary) })
    }
}

/// Run a TPE search over `base`'s `[sweep]` axes: for `cfg.n_trials`, ask the seeded optimizer for
/// a config, run it through the EXISTING [`super::run_backtest`] path, score it with `objective`, and tell
/// the result back. Returns the SAME [`ParamscanReport`] shape the grid/euler paths return — one row
/// per trial, ranked best-first by score (failures score `NaN`/unrankable and sort last).
///
/// An ADAPTER over the optimizer seam: [`TpeSearch`] driven through `optimize::optimize`.
/// ⚠ The evaluator is built `ParamscanExec::Sequential` DELIBERATELY, and it changes nothing: TPE's
/// batch width is 1, and the pool rule already keeps a width-1 batch off rayon. Spelling it out is
/// how "the loop is inherently sequential (each proposal reads the whole history)" stops being a
/// comment and becomes the constructor argument — and it avoids an env read that could not have
/// altered a single result.
pub fn run_tpe(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
    cfg: &TpeConfig,
) -> Result<ParamscanReport, HarnessError> {
    let eval = StoreEvaluator::new(base, store, objective, label, ParamscanExec::Sequential)?;
    let Optimized { report, .. } = optimize(&TpeSearch::new(*cfg), base, &eval)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- profile → space reader -----

    fn base_with_paramscan(sweep: &str) -> BacktestProfile {
        BacktestProfile::from_toml_str(&format!(
            r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "0"
to = "100000"
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
[strategy.params]
size = 1.0
{sweep}
"#
        ))
        .unwrap()
    }

    #[test]
    fn tpe_space_reads_numeric_axes_as_ranges() {
        let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0, 3.0]\nn = [1, 10]");
        let space = tpe_space_from_profile(&base).unwrap();
        assert_eq!(space.len(), 2);
        // Axes are sorted by key: n, size.
        assert_eq!(space[0].0, "n");
        let ParamDomain::Continuous { lo, hi, integral } = &space[0].1 else {
            panic!("n should be a continuous range");
        };
        assert!((*lo - 1.0).abs() < 1e-12 && (*hi - 10.0).abs() < 1e-12 && *integral);
        assert_eq!(space[1].0, "size");
        let ParamDomain::Continuous { lo, hi, integral } = &space[1].1 else {
            panic!("size should be a continuous range");
        };
        assert!((*lo - 1.0).abs() < 1e-12 && (*hi - 3.0).abs() < 1e-12 && !*integral);
    }

    #[test]
    fn tpe_space_rejects_a_non_numeric_axis_pointing_at_grid() {
        let base = base_with_paramscan("[sweep]\nmode = [\"a\", \"b\"]");
        match tpe_space_from_profile(&base) {
            // The refusal names the METHOD an operator would reach for, read off `GridSearch`'s
            // own `name()` — never a flag spelling this assertion would then be pinning.
            Err(HarnessError::Validation(m)) => {
                assert!(m.contains(super::super::sweep::GridSearch.name()), "{m}")
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn tpe_space_rejects_a_non_sweep_profile() {
        let base = base_with_paramscan("");
        assert!(matches!(tpe_space_from_profile(&base), Err(HarnessError::Validation(_))));
    }

    // ----- store-backed driver smoke (needs a concrete DataFusionHist) -----

    #[cfg(feature = "datafusion-store")]
    fn bar(ts: i64, price: f64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// End-to-end over a real store: `run_tpe` spends exactly `n_trials` backtests, every row is
    /// scored, rows come back ranked best-first, and the header names the objective.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn run_tpe_runs_and_ranks_over_a_store() {
        use crate::harness::sweep::{RankBy, RankMetric};
        use vike_data::DataFusionHist;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0), bar(3000, 99.0)];
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        let base = base_with_paramscan("[sweep]\nsize = [1.0, 5.0]");
        let objective = RankMetric::Sharpe.objective();
        let cfg = TpeConfig::new(12, 42);
        let report = run_tpe(&base, store, &objective, "sharpe", &cfg).unwrap();

        assert_eq!(report.rows.len(), 12, "one row per trial");
        assert_eq!(report.rank_by, RankBy::Objective("sharpe".to_string()));
        assert!(report.rows.iter().all(|r| r.report.is_some() && r.score.is_some()), "all scored");

        let scores: Vec<f64> = report.rows.iter().filter_map(|r| r.score).collect();
        for w in scores.windows(2) {
            if w[0].is_nan() || w[1].is_nan() {
                continue;
            }
            assert!(w[0] >= w[1], "rows must be ranked best-first: {scores:?}");
        }
    }

    /// Determinism through the whole driver: the same seed over the same store yields a
    /// byte-identical report (the same trials in the same order with bit-identical scores).
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn run_tpe_is_deterministic_for_a_fixed_seed() {
        use crate::harness::sweep::RankMetric;
        use vike_data::DataFusionHist;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0), bar(3000, 99.0)];
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0, 3.0, 4.0]");
        let objective = RankMetric::Sharpe.objective();
        let cfg = TpeConfig::new(16, 2024);

        let a = run_tpe(&base, store.clone(), &objective, "sharpe", &cfg).unwrap();
        let b = run_tpe(&base, store, &objective, "sharpe", &cfg).unwrap();
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap(),
            "same seed must produce a byte-identical report"
        );
    }
}
