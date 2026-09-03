//! Euler (successive-halving) parameter search — the pure, store-free core.
//!
//! The parameter sweep (`harness::sweep`) is a plain CARTESIAN GRID: every point of every axis is
//! evaluated, so resolution costs exponentially. Euler search (ported in spirit from LEAN's
//! `EulerSearchOptimizationStrategy`) buys the same FINAL resolution far cheaper: evaluate the
//! coarse grid once, then repeatedly halve the per-axis step and re-search only the immediate
//! neighbourhood of the current best point.
//!
//! ```text
//! depth 0        the caller's coarse grid                     N points
//! depth 1..=D    step_i /= 2; evaluate {best_i - step_i, best_i, best_i + step_i}^d
//!                (clamped to each axis's [min, max], deduped against everything
//!                already evaluated)                           <= 3^d - 1 new points per depth
//! ```
//!
//! **The evaluation-count tradeoff.** After `D` halvings the effective step is `step/2^D`, i.e.
//! the resolution of a grid with `2^D * (n_i - 1) + 1` values on each axis — `prod_i (2^D*(n_i-1)+1)`
//! points. Euler pays `N + D*(3^d - 1)` at most (usually far less: every candidate already seen is
//! skipped). For a 2-axis 5x5 grid at depth 3 that is at most 25 + 24 = 49 evaluations against
//! 33x33 = 1089 for the equivalent-resolution grid.
//!
//! A halving that fails to improve the incumbent is NOT a stop condition — it is the signal to
//! shrink again, since only a smaller step can reach the points between the incumbent and the ones
//! just rejected. The search runs the full depth budget and stops early ONLY when a depth produces
//! no unseen candidate at all (the neighbourhood is exhausted: every finer step canonicalizes onto
//! points already evaluated). [`SearchTrace::depth_reached`] therefore reports the resolution
//! ACTUALLY attained, which is what the `harness::euler` budget line compares against.
//!
//! The price is that it is a LOCAL refinement: it can only descend into the basin its coarse grid
//! already found, so a coarse grid too sparse to see a narrow optimum will refine the wrong peak
//! (pinned by `refinement_stays_in_the_basin_the_coarse_grid_chose`). Keep the coarse grid wide,
//! the depth bounded (`max_depth`, default
//! [`EulerConfig::DEFAULT_MAX_DEPTH`]), and treat this as a refinement of a grid, not a replacement
//! for one.
//!
//! Scoring convention matches [`crate::objective`]: HIGHER is better and `NaN` means "unrankable" —
//! a `NaN` point can never become the incumbent best (so refinement never chases a degenerate
//! point), and only becomes `best` at all if literally nothing finite was evaluated.

/// One numeric axis of an Euler search: the coarse grid values the caller would have swept.
/// `values` needs at least one entry; the axis's `[min, max]` refinement box is its own
/// min/max, and the initial step is the smallest gap between consecutive sorted values (so a
/// non-uniform coarse axis refines at its own finest existing spacing).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchAxis {
    pub key: String,
    pub values: Vec<f64>,
    /// Integer-valued axis: every candidate coordinate is rounded to a whole number, and the axis
    /// stops refining once its step falls below `1` (there is nothing between two integers).
    pub integral: bool,
}

impl SearchAxis {
    /// A continuous (float) axis.
    pub fn new(key: impl Into<String>, values: Vec<f64>) -> Self {
        SearchAxis { key: key.into(), values, integral: false }
    }

    /// An integer-valued axis (refinement floors its step at `1`).
    pub fn integral(key: impl Into<String>, values: Vec<f64>) -> Self {
        SearchAxis { key: key.into(), values, integral: true }
    }

    fn min(&self) -> f64 {
        self.values.iter().copied().fold(f64::INFINITY, f64::min)
    }

    fn max(&self) -> f64 {
        self.values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }

    /// Initial step: the smallest gap between consecutive sorted coarse values, or `0` for a
    /// single-valued (pinned) axis — a `0`-step axis never generates neighbours, so it simply
    /// stays pinned through every refinement depth.
    fn initial_step(&self) -> f64 {
        let mut vs = self.values.clone();
        vs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut step = f64::INFINITY;
        for w in vs.windows(2) {
            let gap = w[1] - w[0];
            if gap > 0.0 && gap < step {
                step = gap;
            }
        }
        if step.is_finite() {
            step
        } else {
            0.0
        }
    }

    /// Snap a raw coordinate into this axis's box (and onto whole numbers if `integral`).
    fn canonicalize(&self, x: f64) -> f64 {
        let clamped = x.clamp(self.min(), self.max());
        if self.integral {
            clamped.round()
        } else {
            clamped
        }
    }
}

/// Bounds of an Euler search. `max_depth` is the number of successive halvings after the coarse
/// grid — the whole point of the knob is that the evaluation budget stays BOUNDED (see the module
/// doc's cost formula), so it is capped at [`EulerConfig::MAX_DEPTH_CAP`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EulerConfig {
    pub max_depth: u32,
}

impl EulerConfig {
    /// Three halvings: 8x the coarse resolution on every axis, at most `3*(3^d - 1)` extra runs.
    pub const DEFAULT_MAX_DEPTH: u32 = 3;
    /// Hard ceiling on `max_depth` — 16 halvings is already 65536x the coarse resolution, well past
    /// any meaningful f64 parameter granularity, and the guard keeps a typo'd CLI value from
    /// spending an unbounded number of backtests.
    pub const MAX_DEPTH_CAP: u32 = 16;

    /// Clamp `max_depth` into `0..=MAX_DEPTH_CAP`. `0` is legal and means "coarse grid only",
    /// which makes an Euler search evaluate exactly the grid's points.
    pub fn with_depth(max_depth: u32) -> Self {
        EulerConfig { max_depth: max_depth.min(Self::MAX_DEPTH_CAP) }
    }
}

impl Default for EulerConfig {
    fn default() -> Self {
        EulerConfig { max_depth: Self::DEFAULT_MAX_DEPTH }
    }
}

/// What an Euler search did: every point it evaluated, in evaluation order, plus the winner and
/// how many halvings it actually performed (`<= max_depth` — it stops early only once a halving
/// produces no unseen candidate).
#[derive(Debug, Clone)]
pub struct SearchTrace {
    /// `(coordinates, score)` per evaluation, in the order they were evaluated. Length is the
    /// evaluation count the tradeoff is measured in.
    pub evaluated: Vec<(Vec<f64>, f64)>,
    /// Index into [`SearchTrace::evaluated`] of the best (highest-scoring, non-`NaN`) point.
    /// `None` only for an empty search (no axes, or no values).
    pub best: Option<usize>,
    /// Halvings actually performed — counting only depths that produced at least one unseen
    /// candidate, so this IS the refinement resolution the run attained (`step / 2^depth_reached`).
    pub depth_reached: u32,
}

impl SearchTrace {
    /// The winning `(coordinates, score)`, if anything was evaluated.
    pub fn best_point(&self) -> Option<&(Vec<f64>, f64)> {
        self.best.and_then(|i| self.evaluated.get(i))
    }

    /// How many evaluations the search spent.
    pub fn n_evaluated(&self) -> usize {
        self.evaluated.len()
    }
}

/// `true` when `cand` is a strictly better incumbent than `best`: higher score wins, and a `NaN`
/// candidate NEVER displaces a finite incumbent (the objective seam's "NaN = unrankable" rule —
/// refinement must not chase a degenerate point).
fn better(cand: f64, best: f64) -> bool {
    if cand.is_nan() {
        false
    } else if best.is_nan() {
        true
    } else {
        cand > best
    }
}

/// The cartesian product of every axis's coarse `values`, in odometer order (last axis varies
/// fastest) — the same deterministic order `harness::sweep::expand_sweep` produces.
fn coarse_grid(axes: &[SearchAxis]) -> Vec<Vec<f64>> {
    if axes.is_empty() || axes.iter().any(|a| a.values.is_empty()) {
        return Vec::new();
    }
    let total: usize = axes.iter().map(|a| a.values.len()).product();
    let mut out = Vec::with_capacity(total);
    let mut idx = vec![0usize; axes.len()];
    for _ in 0..total {
        out.push(axes.iter().zip(&idx).map(|(a, &i)| a.canonicalize(a.values[i])).collect());
        for i in (0..axes.len()).rev() {
            idx[i] += 1;
            if idx[i] < axes[i].values.len() {
                break;
            }
            idx[i] = 0;
        }
    }
    out
}

/// A dedup key for a coordinate vector: raw f64 bit patterns. Exact equality is the RIGHT
/// predicate here — every candidate is produced by the same `canonicalize` arithmetic from the same
/// incumbent, so a repeat is bit-identical; an epsilon match would instead silently drop genuinely
/// distinct neighbours at deep refinement levels.
fn key_of(point: &[f64]) -> Vec<u64> {
    point.iter().map(|x| x.to_bits()).collect()
}

/// Keep only the points not already in `seen`, in INPUT order, registering each survivor.
/// Deduping a whole depth's candidate list up front is equivalent to deduping one-at-a-time as the
/// old point-wise loop did: `seen` membership depends only on the candidate coordinates, never on a
/// score, so evaluation order cannot change which candidates survive.
fn dedup_new(
    points: Vec<Vec<f64>>,
    seen: &mut std::collections::HashSet<Vec<u64>>,
) -> Vec<Vec<f64>> {
    points.into_iter().filter(|p| seen.insert(key_of(p))).collect()
}

/// Run a bounded Euler (successive-halving) search over `axes`, scoring each point with `eval`
/// (HIGHER is better, `NaN` = unrankable). Deterministic: the coarse grid is evaluated in odometer
/// order and each refinement's neighbourhood in odometer order over `{-step, 0, +step}` per axis.
///
/// Returns the full [`SearchTrace`] — the caller keeps every evaluated point (a sweep report row
/// each), not just the winner.
///
/// A thin point-wise wrapper over [`euler_search_batched`] (one `eval` call per point, in the same
/// order) — the two produce identical traces, which is what lets the harness parallelize the batch
/// form without changing any result.
pub fn euler_search<F>(axes: &[SearchAxis], cfg: &EulerConfig, mut eval: F) -> SearchTrace
where
    F: FnMut(&[f64]) -> f64,
{
    euler_search_batched(axes, cfg, |batch| batch.iter().map(|p| eval(p.as_slice())).collect())
}

/// [`euler_search`], but handing the caller one whole DEPTH's already-deduped candidate batch at a
/// time instead of one point at a time — the seam a caller uses to evaluate the batch in parallel
/// (the harness's rayon sweep) without changing the search itself.
///
/// CONTRACT: `eval_batch` must return exactly one score per input point, in INPUT order (panics
/// otherwise). Because the batch is folded back in that order — dedup already done, incumbent
/// tracking unchanged — the resulting [`SearchTrace`] is identical whether the batch was scored
/// sequentially or concurrently. That is the whole point: determinism does not depend on completion
/// order.
///
/// Batches are: the coarse grid (one batch), then one batch per refinement depth (the incumbent's
/// `3^d` neighbourhood, minus everything already evaluated). Depths remain strictly sequential —
/// each refinement centres on the previous depth's winner, so only WITHIN a batch is there any
/// independence to exploit.
pub fn euler_search_batched<F>(
    axes: &[SearchAxis],
    cfg: &EulerConfig,
    mut eval_batch: F,
) -> SearchTrace
where
    F: FnMut(&[Vec<f64>]) -> Vec<f64>,
{
    let mut evaluated: Vec<(Vec<f64>, f64)> = Vec::new();
    let mut seen: std::collections::HashSet<Vec<u64>> = std::collections::HashSet::new();
    let mut best: Option<usize> = None;

    // Score one already-deduped batch and append its points in INPUT order, tracking the incumbent
    // exactly as the old point-at-a-time loop did.
    let mut run_batch =
        |batch: Vec<Vec<f64>>, evaluated: &mut Vec<(Vec<f64>, f64)>, best: &mut Option<usize>| {
            if batch.is_empty() {
                return;
            }
            let scores = eval_batch(batch.as_slice());
            assert_eq!(
                scores.len(),
                batch.len(),
                "euler_search_batched: eval_batch must return one score per point, in input order"
            );
            for (point, score) in batch.into_iter().zip(scores) {
                evaluated.push((point, score));
                let i = evaluated.len() - 1;
                let is_better = match *best {
                    // The first point evaluated is the incumbent whatever it scored — an all-NaN search
                    // still reports a best (see `all_nan_search_still_returns_a_best_and_terminates`).
                    None => true,
                    Some(b) => better(score, evaluated[b].1),
                };
                if is_better {
                    *best = Some(i);
                }
            }
        };

    let coarse = dedup_new(coarse_grid(axes), &mut seen);
    run_batch(coarse, &mut evaluated, &mut best);

    let Some(mut best_idx) = best else {
        return SearchTrace { evaluated, best: None, depth_reached: 0 };
    };

    let mut steps: Vec<f64> = axes.iter().map(|a| a.initial_step()).collect();
    let mut depth_reached = 0u32;

    for _ in 0..cfg.max_depth {
        // Halve every axis's step. An integral axis floors at 1 (nothing lives between two
        // integers), and once it is AT 1 a further halving would round back to the same
        // candidates — which the dedup then drops, so the axis simply stops contributing.
        for (step, axis) in steps.iter_mut().zip(axes) {
            *step /= 2.0;
            if axis.integral && *step < 1.0 {
                *step = if axis.initial_step() >= 1.0 { 1.0 } else { 0.0 };
            }
        }
        if steps.iter().all(|s| *s <= 0.0) {
            break;
        }

        let center = evaluated[best_idx].0.clone();

        // Odometer over {-1, 0, +1} per axis — the 3^d neighbourhood of the incumbent.
        let total: usize = 3usize.saturating_pow(axes.len() as u32);
        let mut idx = vec![0usize; axes.len()];
        let mut candidates: Vec<Vec<f64>> = Vec::with_capacity(total);
        for _ in 0..total {
            let point: Vec<f64> = axes
                .iter()
                .enumerate()
                .map(|(i, axis)| {
                    let delta = (idx[i] as f64 - 1.0) * steps[i];
                    axis.canonicalize(center[i] + delta)
                })
                .collect();
            candidates.push(point);
            for i in (0..axes.len()).rev() {
                idx[i] += 1;
                if idx[i] < 3 {
                    break;
                }
                idx[i] = 0;
            }
        }
        let fresh = dedup_new(candidates, &mut seen);
        let any_new = !fresh.is_empty();
        run_batch(fresh, &mut evaluated, &mut best);

        // SHRINK AND RE-SEARCH ARE DECOUPLED. A halving that fails to beat the incumbent is the
        // SIGNAL TO SHRINK AGAIN, not a reason to stop: shrinking is the only way to reach points
        // NEARER the incumbent than the current step, and those are exactly the points a
        // non-improving neighbourhood has not yet ruled out. (Concretely: on a peak at 0.30 with
        // coarse best 0.25, step 0.125 probes 0.125/0.375 — both worse — while step 0.0625 finds
        // 0.3125, which is better. Stopping at the first non-improving halving would return the
        // coarse grid's own answer, refining nothing.)
        //
        // The ONE genuine stop condition is a depth that produced ZERO unseen candidates: the
        // whole neighbourhood is already evaluated, and since candidates are `canonicalize`d
        // (clamped to the box, rounded on an integral axis) every finer step collapses onto the
        // same points, so no later depth can produce anything new either.
        if !any_new {
            break;
        }
        depth_reached += 1;
        best_idx = best.expect("best stays Some once set");
    }

    SearchTrace { evaluated, best, depth_reached }
}

/// The evaluation count a plain cartesian grid would need to reach the resolution an Euler search
/// of `max_depth` halvings reaches on `axes`: each axis's coarse spacing is halved `max_depth`
/// times, so `n_i` values become `2^D * (n_i - 1) + 1`. Saturating (an absurd depth reports
/// `usize::MAX` rather than wrapping) — this is a documentation/reporting helper for the tradeoff,
/// not part of the search.
///
/// PRECONDITION: this counts a UNIFORMLY spaced axis. [`SearchAxis::initial_step`] refines at the
/// axis's SMALLEST existing gap, so on a non-uniform axis (e.g. `[1, 2, 100]`, whose initial step is
/// `1`) the refinement never traverses the wide gaps and the comparison overstates what the search
/// covered. Callers pass `cfg` with the depth ACTUALLY reached (`SearchTrace::depth_reached`), not
/// `max_depth`, so the count at least describes a resolution the run really attained.
pub fn equivalent_grid_evaluations(axes: &[SearchAxis], cfg: &EulerConfig) -> usize {
    axes.iter().fold(1usize, |acc, a| {
        let n = a.values.len();
        let refined = if n <= 1 {
            n
        } else {
            2usize.saturating_pow(cfg.max_depth).saturating_mul(n - 1).saturating_add(1)
        };
        acc.saturating_mul(refined)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A smooth unimodal objective peaking at `x = 0.30` — the "known optimum" the convergence
    /// tests aim at. Higher is better.
    fn peak_1d(p: &[f64]) -> f64 {
        -(p[0] - 0.30).powi(2)
    }

    /// A 2-axis bowl peaking at `(0.30, 7.0)`.
    fn peak_2d(p: &[f64]) -> f64 {
        -((p[0] - 0.30).powi(2) + (p[1] - 7.0).powi(2) / 100.0)
    }

    #[test]
    fn coarse_grid_is_the_cartesian_product_in_odometer_order() {
        let axes =
            [SearchAxis::new("a", vec![1.0, 2.0]), SearchAxis::new("b", vec![10.0, 20.0, 30.0])];
        let g = coarse_grid(&axes);
        assert_eq!(g.len(), 6);
        assert_eq!(g[0], vec![1.0, 10.0]);
        assert_eq!(g[1], vec![1.0, 20.0]); // last axis varies fastest
        assert_eq!(g[3], vec![2.0, 10.0]);
    }

    #[test]
    fn depth_zero_evaluates_exactly_the_coarse_grid() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.5, 1.0])];
        let trace = euler_search(&axes, &EulerConfig::with_depth(0), peak_1d);
        assert_eq!(trace.n_evaluated(), 3, "depth 0 is the plain grid");
        assert_eq!(trace.depth_reached, 0);
        assert_eq!(trace.best_point().unwrap().0, vec![0.5], "closest coarse point wins");
    }

    /// THE lane's claim: successive halving reaches the known optimum's resolution in FAR fewer
    /// evaluations than the equivalent-resolution grid.
    #[test]
    fn halving_converges_in_fewer_evaluations_than_the_equivalent_grid() {
        // Coarse: 5 values, spacing 0.25 over [0, 1]. Depth 4 -> effective spacing 0.015625.
        let axes = [SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0])];
        let cfg = EulerConfig::with_depth(4);
        let trace = euler_search(&axes, &cfg, peak_1d);

        let best = trace.best_point().unwrap();
        assert!(
            (best.0[0] - 0.30).abs() <= 0.25 / 2f64.powi(4) + 1e-12,
            "converged to within the refined step of the optimum: {:?}",
            best.0
        );

        let grid = equivalent_grid_evaluations(&axes, &cfg);
        assert_eq!(grid, 2usize.pow(4) * 4 + 1, "65-point equivalent-resolution grid");
        assert!(
            trace.n_evaluated() < grid,
            "euler spent {} evaluations, equivalent grid {grid}",
            trace.n_evaluated()
        );
    }

    #[test]
    fn two_axis_search_converges_and_stays_far_under_the_grid() {
        let axes = [
            SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0]),
            SearchAxis::new("b", vec![0.0, 5.0, 10.0]),
        ];
        let cfg = EulerConfig::with_depth(3);
        let trace = euler_search(&axes, &cfg, peak_2d);

        let best = trace.best_point().unwrap();
        assert!((best.0[0] - 0.30).abs() < 0.1, "axis a converged: {:?}", best.0);
        assert!((best.0[1] - 7.0).abs() < 2.0, "axis b converged: {:?}", best.0);

        let grid = equivalent_grid_evaluations(&axes, &cfg);
        assert_eq!(grid, 33 * 17);
        assert!(
            trace.n_evaluated() < grid / 4,
            "euler spent {} of a {grid}-point equivalent grid",
            trace.n_evaluated()
        );
    }

    #[test]
    fn best_is_never_worse_than_the_best_coarse_point() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0])];
        let coarse_best =
            coarse_grid(&axes).iter().map(|p| peak_1d(p)).fold(f64::NEG_INFINITY, f64::max);
        let trace = euler_search(&axes, &EulerConfig::default(), peak_1d);
        assert!(
            trace.best_point().unwrap().1 >= coarse_best,
            "refinement may only improve on the grid"
        );
    }

    #[test]
    fn search_is_deterministic() {
        let axes = [
            SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0]),
            SearchAxis::new("b", vec![0.0, 5.0, 10.0]),
        ];
        let cfg = EulerConfig::default();
        let a = euler_search(&axes, &cfg, peak_2d);
        let b = euler_search(&axes, &cfg, peak_2d);
        let pts = |t: &SearchTrace| -> Vec<Vec<f64>> {
            t.evaluated.iter().map(|(p, _)| p.clone()).collect()
        };
        assert_eq!(pts(&a), pts(&b));
        assert_eq!(a.best, b.best);
    }

    #[test]
    fn every_evaluated_point_is_unique() {
        let axes = [
            SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0]),
            SearchAxis::new("b", vec![0.0, 5.0, 10.0]),
        ];
        let trace = euler_search(&axes, &EulerConfig::default(), peak_2d);
        let mut keys: Vec<Vec<u64>> = trace.evaluated.iter().map(|(p, _)| key_of(p)).collect();
        let n = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), n, "a point must never be backtested twice");
    }

    #[test]
    fn candidates_stay_inside_the_coarse_box() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.25, 0.5])];
        // Optimum sits outside [0, 0.5]; the search must clamp, never extrapolate.
        let trace = euler_search(&axes, &EulerConfig::default(), |p| -(p[0] - 5.0).powi(2));
        for (p, _) in &trace.evaluated {
            assert!((0.0..=0.5).contains(&p[0]), "candidate escaped the box: {p:?}");
        }
    }

    #[test]
    fn integral_axis_only_ever_evaluates_whole_numbers() {
        let axes = [SearchAxis::integral("n", vec![1.0, 5.0, 9.0])];
        let trace = euler_search(&axes, &EulerConfig::with_depth(5), |p| -(p[0] - 6.0).powi(2));
        for (p, _) in &trace.evaluated {
            assert_eq!(p[0], p[0].round(), "integral axis produced a fractional value: {p:?}");
        }
        assert_eq!(trace.best_point().unwrap().0, vec![6.0], "found the integer optimum");
    }

    #[test]
    fn nan_scores_never_become_the_incumbent() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.5, 1.0])];
        // Only 0.5 is finite; everything else is unrankable.
        let trace = euler_search(&axes, &EulerConfig::default(), |p| {
            if (p[0] - 0.5).abs() < 1e-12 {
                1.0
            } else {
                f64::NAN
            }
        });
        let best = trace.best_point().unwrap();
        assert_eq!(best.1, 1.0, "the one finite point wins");
        assert_eq!(best.0, vec![0.5]);
    }

    #[test]
    fn all_nan_search_still_returns_a_best_and_terminates() {
        let axes = [SearchAxis::new("a", vec![0.0, 1.0])];
        let trace = euler_search(&axes, &EulerConfig::default(), |_| f64::NAN);
        assert!(trace.best.is_some(), "a fully unrankable search still reports a row");
        assert!(trace.best_point().unwrap().1.is_nan());
    }

    #[test]
    fn single_valued_axis_is_pinned() {
        let axes = [SearchAxis::new("a", vec![0.7])];
        let trace = euler_search(&axes, &EulerConfig::default(), peak_1d);
        assert_eq!(trace.n_evaluated(), 1, "a zero-step axis generates no neighbours");
        assert_eq!(trace.best_point().unwrap().0, vec![0.7]);
    }

    #[test]
    fn empty_axes_is_an_empty_search() {
        let trace = euler_search(&[], &EulerConfig::default(), peak_1d);
        assert_eq!(trace.n_evaluated(), 0);
        assert!(trace.best.is_none());
        assert_eq!(trace.depth_reached, 0);
    }

    #[test]
    fn depth_is_capped_and_bounded_by_the_budget_formula() {
        assert_eq!(EulerConfig::with_depth(999).max_depth, EulerConfig::MAX_DEPTH_CAP);
        let axes =
            [SearchAxis::new("a", vec![0.0, 0.5, 1.0]), SearchAxis::new("b", vec![0.0, 0.5, 1.0])];
        let cfg = EulerConfig::with_depth(6);
        let trace = euler_search(&axes, &cfg, peak_2d);
        assert!(trace.depth_reached <= cfg.max_depth);
        // Documented ceiling: coarse grid + max_depth * (3^d - 1) new candidates.
        let ceiling = 9 + cfg.max_depth as usize * (3usize.pow(2) - 1);
        assert!(
            trace.n_evaluated() <= ceiling,
            "evaluation budget must stay bounded: {} > {ceiling}",
            trace.n_evaluated()
        );
    }

    /// The ONE early stop: a depth whose whole neighbourhood canonicalizes onto already-evaluated
    /// points. An integral axis at step 1 pinned at the box edge is exactly that case.
    #[test]
    fn search_stops_when_a_depth_produces_no_new_candidate() {
        let axes = [SearchAxis::integral("n", vec![1.0, 2.0, 3.0])];
        // Flat objective -> the incumbent is the first coarse point, n = 1 (the box's low edge).
        // Halving floors the step at 1, so the neighbourhood {0->clamped 1, 1, 2} is all seen.
        let trace = euler_search(&axes, &EulerConfig::with_depth(10), |_| 1.0);
        assert_eq!(trace.n_evaluated(), 3, "nothing beyond the coarse grid was reachable");
        assert_eq!(
            trace.depth_reached, 0,
            "a barren halving buys no resolution, so it is not counted"
        );
    }

    /// REGRESSION (the early-stop bug): a halving that fails to beat the incumbent must NOT end the
    /// search — shrinking further is the only way to reach the points between the incumbent and the
    /// ones just rejected. Here depth 1 (step 0.125) probes 0.125/0.375, both worse than the coarse
    /// best 0.25; only depth 2 (step 0.0625) reaches 0.3125, which wins.
    #[test]
    fn a_non_improving_halving_shrinks_and_retries_instead_of_stopping() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0])];
        let trace = euler_search(&axes, &EulerConfig::with_depth(2), peak_1d);
        assert_eq!(trace.depth_reached, 2, "the depth budget must be exhausted, not abandoned");
        assert!(
            trace.evaluated.iter().any(|(p, _)| (p[0] - 0.3125).abs() < 1e-12),
            "the depth-2 neighbourhood must have been searched: {:?}",
            trace.evaluated
        );
        assert_eq!(trace.best_point().unwrap().0, vec![0.3125]);
    }

    /// The lane's claim measured against a REAL grid rather than the counting formula: euler must
    /// match the fine grid's winning SCORE (to within the refined step) for a small fraction of its
    /// evaluations.
    #[test]
    fn euler_matches_the_actual_fine_grids_winner_far_cheaper() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0])];
        let cfg = EulerConfig::with_depth(4);

        // The real 65-point equal-resolution grid, evaluated exhaustively.
        let fine_n = 2usize.pow(4) * 4 + 1;
        let fine_step = 0.25 / 2f64.powi(4);
        let mut fine_evals = 0usize;
        let fine_best = (0..fine_n)
            .map(|i| {
                fine_evals += 1;
                peak_1d(&[i as f64 * fine_step])
            })
            .fold(f64::NEG_INFINITY, f64::max);
        assert_eq!(fine_evals, equivalent_grid_evaluations(&axes, &cfg));

        let trace = euler_search(&axes, &cfg, peak_1d);
        let euler_best = trace.best_point().unwrap().1;
        assert!(
            euler_best >= fine_best - 1e-9,
            "euler ({euler_best}) must not lose to the fine grid ({fine_best})"
        );
        assert!(
            trace.n_evaluated() * 4 < fine_evals,
            "euler spent {} of {fine_evals} evaluations",
            trace.n_evaluated()
        );
    }

    /// The DOCUMENTED residual, pinned so a future change cannot silently alter the semantics: on a
    /// bimodal objective the search stays in the basin its coarse best occupied, even though the
    /// other basin is globally higher.
    #[test]
    fn refinement_stays_in_the_basin_the_coarse_grid_chose() {
        // Wide shallow peak at 0.25 (coarse-visible), narrow tall peak at 0.86 (coarse-invisible).
        let bimodal = |p: &[f64]| {
            let wide = 1.0 - (p[0] - 0.25).powi(2);
            let narrow = 3.0 - 400.0 * (p[0] - 0.86).powi(2);
            if narrow > wide {
                narrow
            } else {
                wide
            }
        };
        let axes = [SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0])];
        let trace = euler_search(&axes, &EulerConfig::with_depth(5), bimodal);
        let best = trace.best_point().unwrap();
        assert!(
            (best.0[0] - 0.25).abs() < 0.1,
            "local refinement must stay in the coarse basin: {:?}",
            best.0
        );
        assert!(bimodal(&[0.86]) > best.1, "the missed global optimum is genuinely higher");
    }

    #[test]
    fn equivalent_grid_evaluations_handles_degenerate_axes() {
        assert_eq!(equivalent_grid_evaluations(&[], &EulerConfig::default()), 1);
        let pinned = [SearchAxis::new("a", vec![1.0])];
        assert_eq!(equivalent_grid_evaluations(&pinned, &EulerConfig::default()), 1);
    }

    /// REGRESSION (the parallel-sweep seam): the batch entry point must produce a trace
    /// BIT-IDENTICAL to the point-wise one — same points, same order, same scores, same winner,
    /// same depth. That equivalence is what lets `harness::euler` score a batch on a rayon pool
    /// without changing a single reported row.
    ///
    /// The batch closure here deliberately scores its points in REVERSE order before restoring
    /// input order, standing in for "some other completion order": nothing about the trace may
    /// depend on when a point finished, only on where it sat in the batch.
    #[test]
    fn batched_search_matches_the_point_wise_search_bit_for_bit() {
        let axes = [
            SearchAxis::new("a", vec![0.0, 0.25, 0.5, 0.75, 1.0]),
            SearchAxis::integral("b", vec![0.0, 5.0, 10.0]),
        ];
        let cfg = EulerConfig::with_depth(4);

        let point_wise = euler_search(&axes, &cfg, peak_2d);
        let batched = euler_search_batched(&axes, &cfg, |batch| {
            let mut scored: Vec<(usize, f64)> =
                batch.iter().enumerate().rev().map(|(i, p)| (i, peak_2d(p))).collect();
            scored.sort_by_key(|(i, _)| *i);
            scored.into_iter().map(|(_, s)| s).collect()
        });

        assert_eq!(batched.depth_reached, point_wise.depth_reached);
        assert_eq!(batched.best, point_wise.best);
        assert_eq!(batched.n_evaluated(), point_wise.n_evaluated());
        for (b, p) in batched.evaluated.iter().zip(&point_wise.evaluated) {
            assert_eq!(b.0, p.0, "same coordinates in the same evaluation order");
            assert_eq!(b.1.to_bits(), p.1.to_bits(), "bit-identical score at {:?}", p.0);
        }
    }

    /// A batch evaluator that returns the wrong number of scores is a contract violation, not a
    /// silently-truncated search.
    #[test]
    #[should_panic(expected = "one score per point")]
    fn batched_search_rejects_a_mismatched_score_count() {
        let axes = [SearchAxis::new("a", vec![0.0, 0.5, 1.0])];
        euler_search_batched(&axes, &EulerConfig::with_depth(0), |_batch| vec![0.0]);
    }
}
