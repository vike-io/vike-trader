//! TPE (Tree-structured Parzen Estimator) — the ask/tell Bayesian sampler, as a LIBRARY.
//!
//! CLEAN-ROOM from the mathematics of Bergstra, Bardenet, Bengio & Kegl, *"Algorithms for
//! Hyper-Parameter Optimization"* (NeurIPS 2011), section 4 — no AGPL/Optuna/Hyperopt source was
//! consulted. It converges on good points in far fewer evaluations than an exhaustive grid by
//! MODELLING which regions produce good scores and proposing there.
//!
//! # Why it lives here and not where it was written
//!
//! It was `crates/vike-backtest/src/search/tpe.rs`, at layer 50, and its own module doc already
//! drew the line this move cuts along: *"The ask/tell core is store-free and pure; only `run_tpe`
//! (the driver) touches a `HistStore`."* The core never named a profile, a store or a backtest —
//! only `f64` scores over a named parameter space.
//!
//! Sitting at 50 it was reachable by the simulator and by nothing below it. In particular it was
//! out of reach of `vike-user-research` (35), the user-study host, whose studies can run ONE
//! backtest through `StudySim` and could therefore sweep a grid and nothing cleverer. This crate
//! is at 15 (it was 20 when the sampler moved): the study host already depends on it, so the
//! sampler arrives there with NO new edge, and the simulator reaches down to it as it reaches down
//! to anything else.
//!
//! ⚠ The alternative was worse than doing nothing: a study that wants adaptive search and cannot
//! have it writes its own, and the tree then carries two Parzen estimators drifting apart. That is
//! the shape `docs/decisions/` keeps recording — a capability one layer above the caller who needs
//! it, worked around rather than moved.
//!
//! # Determinism is a CONTRACT here, not a nicety
//!
//! One perturbed Gaussian draw moves a sampled coordinate, which moves which candidate wins the
//! `l(x)/g(x)` argmax, which moves the proposed point, which moves the history every LATER
//! proposal is built from. The sequences do not drift apart, they FORK.
//!
//! So every transcendental calls the `libm` CRATE, never the `f64` method: the method spellings
//! reach the PLATFORM's libm, which IEEE 754 leaves free to round `log`/`cos` however it likes, so
//! MSVC and glibc differ in the last bit and the same seed searches a different space on each box.
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
//! verdict. ⚠ Both `.sqrt()` calls stay `f64` methods deliberately — IEEE 754 DOES require `sqrt`
//! correctly rounded, so routing it through `libm` would buy nothing and blur the rule.
//!
//! ⚠ A same-seed-twice-in-one-process test pins NONE of that, and the original file says so about
//! its own: such a test is satisfied by any pure function of its seed, on any single platform,
//! against any libm. Within-box determinism is what those tests hold; the cross-platform claim
//! rests on the `libm` crate and on this crate's `libm_platform_probe`, which now covers this file.

use std::cmp::Ordering;

use indexmap::IndexMap;

/// The per-category pseudocount of the discrete prior AND the weight of the continuous prior kernel
/// (paper's `prior_weight`, `1.0`): every category keeps at least this much mass, and the broad
/// prior Gaussian counts as one extra observation, so a region the good group has never visited is
/// improbable but never impossible.
const PRIOR_WEIGHT: f64 = 1.0;

/// Density floor for the `l(x)/g(x)` ratio — keeps a candidate that lands where the bad density has
/// decayed to ~0 from producing a `+inf`/`NaN` EI, and vice-versa. Small enough not to distort any
/// real comparison.
const TINY: f64 = 1e-12;

// ---------------------------------------------------------------------------------------------
// Deterministic PRNG (SplitMix64) — see the determinism contract in the module doc.
// ---------------------------------------------------------------------------------------------

/// A seeded SplitMix64 generator (Steele, Lea & Flood, 2014). Ten lines, no external dep, and
/// bit-stable forever — the reproducibility this optimizer's determinism contract rests on.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    /// The canonical SplitMix64 mixing step.
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)` from the top 53 bits (the f64 mantissa width).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A standard normal via one Box–Muller transform (deterministic given the stream).
    ///
    /// ⚠ `libm::log`/`libm::cos`, never `u1.ln()`/`(…).cos()`. The method spellings call the
    /// PLATFORM's libm, which IEEE 754 leaves free to round these two however it likes, so MSVC and
    /// glibc differ in the last bit — and a last-bit difference in ONE draw forks the whole
    /// proposal sequence (module doc). `.sqrt()` stays a method call on purpose: IEEE 754 DOES
    /// require `sqrt` correctly rounded, so it is already the same number everywhere.
    fn gaussian(&mut self) -> f64 {
        let u1 = self.next_f64().max(f64::MIN_POSITIVE); // avoid log(0)
        let u2 = self.next_f64();
        (-2.0 * libm::log(u1)).sqrt() * libm::cos(std::f64::consts::TAU * u2)
    }
}

// ---------------------------------------------------------------------------------------------
// Parameter space.
// ---------------------------------------------------------------------------------------------

/// One tunable parameter's domain: a continuous numeric range, or a discrete candidate set (the
/// shape today's `[sweep]` grids take). `integral` renders proposals back as whole numbers.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamDomain {
    /// A continuous range `[lo, hi]` TPE may propose ANY value within (the between-the-grid-points
    /// exploration `--optimizer euler` also does).
    Continuous { lo: f64, hi: f64, integral: bool },
    /// A discrete set of candidate values — treated categorically (each distinct value is a
    /// category in the Parzen histogram).
    Discrete { values: Vec<f64>, integral: bool },
}

impl ParamDomain {
    /// A continuous float range `[lo, hi]`.
    pub fn continuous(lo: f64, hi: f64) -> Self {
        ParamDomain::Continuous { lo, hi, integral: false }
    }

    /// A continuous integer range `[lo, hi]` (proposals round to whole numbers).
    pub fn continuous_integral(lo: f64, hi: f64) -> Self {
        ParamDomain::Continuous { lo, hi, integral: true }
    }

    /// A discrete float candidate set.
    pub fn discrete(values: Vec<f64>) -> Self {
        ParamDomain::Discrete { values, integral: false }
    }

    /// A discrete integer candidate set (proposals render as TOML integers).
    pub fn discrete_integral(values: Vec<f64>) -> Self {
        ParamDomain::Discrete { values, integral: true }
    }

    /// Whether proposals on this dim are whole numbers. Public because the CALLER renders a
    /// coordinate into its own wire type and needs to know which one - see the note on the
    /// deleted `to_toml` below.
    pub fn integral(&self) -> bool {
        match self {
            ParamDomain::Continuous { integral, .. } | ParamDomain::Discrete { integral, .. } => {
                *integral
            }
        }
    }

    /// A fallback coordinate for this dim when a `tell`'s map omits the parameter — the continuous
    /// midpoint or the first discrete candidate. Public for the same reason as `integral`.
    pub fn default_value(&self) -> f64 {
        match self {
            ParamDomain::Continuous { lo, hi, .. } => 0.5 * (lo + hi),
            ParamDomain::Discrete { values, .. } => values.first().copied().unwrap_or(0.0),
        }
    }

    // `to_toml` IS NOT HERE, and its absence is what keeps this crate's dependency list honest.
    // A sampler proposes NUMBERS; which wire type a number lands in belongs to whoever owns the
    // table it lands in. Keeping the three-line render here would have made a maths crate
    // depend on `toml` for one match arm - a dependency every consumer of vike-ml would then
    // compile, to serialize a format most of them never touch. The caller renders instead,
    // reading `integral()` above: `vike_backtest::search::tpe`'s `overrides_for`.

    /// A uniform-random draw from this domain — the startup (model-seeding) sampler, and the
    /// baseline the convergence tests race TPE against.
    fn sample_uniform(&self, rng: &mut SplitMix64) -> f64 {
        match self {
            ParamDomain::Continuous { lo, hi, integral } => {
                let x = lo + rng.next_f64() * (hi - lo);
                if *integral { x.round().clamp(*lo, *hi) } else { x }
            }
            ParamDomain::Discrete { values, .. } => {
                if values.is_empty() {
                    return 0.0;
                }
                let i = (rng.next_f64() * values.len() as f64) as usize;
                values[i.min(values.len() - 1)]
            }
        }
    }
}

/// The full search space: `(name, domain)` per tunable parameter, in a fixed order (the order every
/// coordinate vector and proposal [`IndexMap`] follows).
pub type TpeSpace = Vec<(String, ParamDomain)>;

// ---------------------------------------------------------------------------------------------
// Adaptive Parzen density (continuous) + categorical density (discrete).
// ---------------------------------------------------------------------------------------------

/// A one-dimensional Gaussian-mixture density: `sum_i weights[i]·N(x; mus[i], sigmas[i])`.
#[derive(Debug, Clone)]
struct Parzen {
    weights: Vec<f64>,
    mus: Vec<f64>,
    sigmas: Vec<f64>,
}

impl Parzen {
    /// The mixture density at `x`.
    fn pdf(&self, x: f64) -> f64 {
        self.weights
            .iter()
            .zip(&self.mus)
            .zip(&self.sigmas)
            .map(|((w, mu), s)| w * gaussian_pdf(x, *mu, *s))
            .sum()
    }

    /// One sample: pick a component by weight, draw its Gaussian, clamp into `[lo, hi]`.
    fn sample(&self, rng: &mut SplitMix64, lo: f64, hi: f64) -> f64 {
        let r = rng.next_f64();
        let mut acc = 0.0;
        let mut idx = self.weights.len() - 1;
        for (i, w) in self.weights.iter().enumerate() {
            acc += *w;
            if r <= acc {
                idx = i;
                break;
            }
        }
        (self.mus[idx] + self.sigmas[idx] * rng.gaussian()).clamp(lo, hi)
    }
}

/// The Gaussian pdf `N(x; mu, sigma)` (`sigma > 0` guaranteed by [`adaptive_parzen`]'s clamp).
///
/// ⚠ `libm::exp`, never `(…).exp()` — the same platform-libm reason as [`SplitMix64::gaussian`],
/// and it bites through a second, quieter path here: this pdf IS the `l(x)/g(x)` Expected-
/// Improvement ratio that [`propose_dim`] takes an argmax over, so a last-bit difference does not
/// nudge a score, it can crown a DIFFERENT one of the `n_candidates` draws. `TAU.sqrt()` stays a
/// method call: `sqrt` is correctly rounded by IEEE 754 and is portable already.
fn gaussian_pdf(x: f64, mu: f64, sigma: f64) -> f64 {
    let z = (x - mu) / sigma;
    libm::exp(-0.5 * z * z) / (sigma * std::f64::consts::TAU.sqrt())
}

/// Build the adaptive Parzen estimator over `obs` on `[lo, hi]` (Bergstra §4.1): one unit-weight
/// Gaussian per observation plus a broad prior Gaussian (`PRIOR_WEIGHT`) at the midpoint, each
/// bandwidth = **max distance to its sorted neighbours** (the domain edges bound the endpoints),
/// clipped to `[width/(1+n), width]` so no kernel becomes a spike or floods the whole domain. An
/// empty `obs` degrades to the prior alone.
fn adaptive_parzen(obs: &[f64], lo: f64, hi: f64) -> Parzen {
    let width = (hi - lo).max(f64::MIN_POSITIVE);
    let prior_mu = 0.5 * (lo + hi);
    let prior_sigma = width;

    // Sorted means: clamped observations (weight 1) + the prior (flagged, PRIOR_WEIGHT).
    let mut means: Vec<(f64, bool)> = obs.iter().map(|&m| (m.clamp(lo, hi), false)).collect();
    means.push((prior_mu, true));
    means.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));

    let n = means.len();
    let sigma_max = prior_sigma;
    // The min bandwidth shrinks as evidence accumulates (paper's `1 + len`), never to zero.
    let sigma_min = prior_sigma / (100.0_f64).min(1.0 + obs.len() as f64);

    let mut weights = Vec::with_capacity(n);
    let mut mus = Vec::with_capacity(n);
    let mut sigmas = Vec::with_capacity(n);
    for i in 0..n {
        let (mu, is_prior) = means[i];
        let left = if i == 0 { lo } else { means[i - 1].0 };
        let right = if i == n - 1 { hi } else { means[i + 1].0 };
        let sigma = if is_prior {
            prior_sigma
        } else {
            (mu - left).max(right - mu).clamp(sigma_min, sigma_max)
        };
        weights.push(if is_prior { PRIOR_WEIGHT } else { 1.0 });
        mus.push(mu);
        sigmas.push(sigma);
    }

    let total: f64 = weights.iter().sum();
    for w in weights.iter_mut() {
        *w /= total;
    }
    Parzen { weights, mus, sigmas }
}

/// Prior-smoothed categorical probabilities over `values`: `p(k) ∝ PRIOR_WEIGHT + count_k`
/// (add-`PRIOR_WEIGHT` Laplace smoothing). Every category keeps non-zero mass, so `l/g` never
/// divides by an unseen category.
fn categorical_probs(obs: &[f64], values: &[f64]) -> Vec<f64> {
    let mut counts = vec![PRIOR_WEIGHT; values.len()];
    for &o in obs {
        counts[nearest_index(values, o)] += 1.0;
    }
    let total: f64 = counts.iter().sum();
    counts.iter().map(|c| c / total).collect()
}

/// Index of the candidate in `values` closest to `x` (observations always coincide with a category,
/// but the nearest-match keeps this robust to float rendering).
fn nearest_index(values: &[f64], x: f64) -> usize {
    let mut best = 0;
    let mut best_d = f64::INFINITY;
    for (i, &v) in values.iter().enumerate() {
        let d = (v - x).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Draw a category index from `probs` (assumed to sum to ~1).
fn sample_categorical(rng: &mut SplitMix64, probs: &[f64]) -> usize {
    let r = rng.next_f64();
    let mut acc = 0.0;
    for (i, p) in probs.iter().enumerate() {
        acc += *p;
        if r <= acc {
            return i;
        }
    }
    probs.len() - 1
}

/// Propose one coordinate for a single dimension. The two domain kinds use the two standard TPE
/// acquisitions (module doc): a CONTINUOUS parameter samples `n_candidates` from `l` and returns
/// the argmax of `l(x)/g(x)` (the Expected-Improvement proxy) — every sample is a fresh point, so
/// the argmax refines toward the good region; a DISCRETE parameter draws the next category directly
/// from `l` (the good-category distribution). Over a small FIXED category set the argmax would lock
/// onto the incumbent and never try an unevaluated category (a deterministic-objective trap),
/// whereas sampling from `l` keeps the prior's exploration mass while still favouring the good
/// group's categories. `good`/`bad` are this dimension's coordinates in each group (both non-empty
/// by construction).
fn propose_dim(
    rng: &mut SplitMix64,
    domain: &ParamDomain,
    good: &[f64],
    bad: &[f64],
    n_candidates: usize,
) -> f64 {
    match domain {
        ParamDomain::Continuous { lo, hi, integral } => {
            let (lo, hi, integral) = (*lo, *hi, *integral);
            if hi <= lo {
                return lo; // a pinned axis has nothing to search
            }
            let l = adaptive_parzen(good, lo, hi);
            let g = adaptive_parzen(bad, lo, hi);
            let mut best_x = 0.5 * (lo + hi);
            let mut best_ei = f64::NEG_INFINITY;
            for _ in 0..n_candidates {
                let mut x = l.sample(rng, lo, hi);
                if integral {
                    x = x.round().clamp(lo, hi);
                }
                let ei = l.pdf(x).max(TINY) / g.pdf(x).max(TINY);
                if ei > best_ei {
                    best_ei = ei;
                    best_x = x;
                }
            }
            best_x
        }
        ParamDomain::Discrete { values, .. } => {
            if values.len() <= 1 {
                return values.first().copied().unwrap_or(0.0);
            }
            let pg = categorical_probs(good, values);
            values[sample_categorical(rng, &pg)]
        }
    }
}

/// Map a `NaN` (unrankable) score to the worst possible value so it always lands in the BAD group
/// and never in GOOD — the objective seam's "NaN = unrankable" rule.
fn finite_or_worst(s: f64) -> f64 {
    if s.is_nan() { f64::NEG_INFINITY } else { s }
}

// ---------------------------------------------------------------------------------------------
// The ask/tell optimizer.
// ---------------------------------------------------------------------------------------------

/// One recorded observation: the coordinate vector (space order) and its objective score.
#[derive(Debug, Clone)]
struct Trial {
    coords: Vec<f64>,
    score: f64,
}

/// The seeded, deterministic TPE optimizer (module doc). Drive it with [`TpeOptimizer::ask`] to get
/// the next config and [`TpeOptimizer::tell`] to record its observed objective; the first
/// `n_startup` asks are random to seed the model, thereafter every ask is a TPE proposal.
#[derive(Debug)]
pub struct TpeOptimizer {
    space: TpeSpace,
    gamma: f64,
    n_startup: usize,
    n_candidates: usize,
    rng: SplitMix64,
    trials: Vec<Trial>,
}

/// Default number of random warm-up asks before TPE proposals begin.
const DEFAULT_STARTUP: usize = 10;
/// Default number of EI candidates sampled from `l(x)` per proposed dimension.
const DEFAULT_CANDIDATES: usize = 24;

impl TpeOptimizer {
    /// A fresh optimizer over `space`, seeded by `seed`, splitting good/bad at the `gamma` quantile
    /// of score (e.g. `0.25`). `n_startup`/`n_candidates` take their defaults — override with
    /// [`Self::with_n_startup`] / [`Self::with_n_candidates`].
    pub fn new(space: TpeSpace, seed: u64, gamma: f64) -> Self {
        TpeOptimizer {
            space,
            gamma,
            n_startup: DEFAULT_STARTUP,
            n_candidates: DEFAULT_CANDIDATES,
            rng: SplitMix64::new(seed),
            trials: Vec::new(),
        }
    }

    /// Set the number of random warm-up asks (at least 2 are always taken, so a split has both a
    /// good and a bad group).
    pub fn with_n_startup(mut self, n: usize) -> Self {
        self.n_startup = n;
        self
    }

    /// Set the number of EI candidates drawn per dimension per proposal (at least 1).
    pub fn with_n_candidates(mut self, n: usize) -> Self {
        self.n_candidates = n.max(1);
        self
    }

    /// Propose the next configuration as `name -> value` in space order.
    pub fn ask(&mut self) -> IndexMap<String, f64> {
        let coords = if self.trials.len() < self.n_startup.max(2) {
            self.sample_random()
        } else {
            self.sample_tpe()
        };
        self.coords_to_map(&coords)
    }

    /// Record the objective `score` observed for a previously-asked `params` (higher is better;
    /// `NaN` = unrankable). Any space parameter absent from `params` falls back to the domain
    /// default.
    pub fn tell(&mut self, params: &IndexMap<String, f64>, score: f64) {
        let mut coords = Vec::with_capacity(self.space.len());
        for (name, domain) in &self.space {
            coords.push(params.get(name).copied().unwrap_or_else(|| domain.default_value()));
        }
        self.trials.push(Trial { coords, score });
    }

    /// The best-scoring observation so far (`None` before any finite-scored `tell`).
    pub fn best(&self) -> Option<(IndexMap<String, f64>, f64)> {
        let mut best_i: Option<usize> = None;
        for (i, t) in self.trials.iter().enumerate() {
            if t.score.is_nan() {
                continue;
            }
            let take = match best_i {
                None => true,
                Some(b) => t.score > self.trials[b].score,
            };
            if take {
                best_i = Some(i);
            }
        }
        best_i.map(|i| (self.coords_to_map(&self.trials[i].coords), self.trials[i].score))
    }

    /// A uniform-random coordinate vector (the startup sampler).
    fn sample_random(&mut self) -> Vec<f64> {
        let space = &self.space;
        let rng = &mut self.rng;
        space.iter().map(|(_, d)| d.sample_uniform(rng)).collect()
    }

    /// A TPE-proposed coordinate vector: split the history, then propose each dimension
    /// independently off its own `l`/`g` densities.
    fn sample_tpe(&mut self) -> Vec<f64> {
        let (good_idx, bad_idx) = self.split_indices();
        // Disjoint field borrows: rng is &mut, space/trials are &.
        let n_candidates = self.n_candidates;
        let space = &self.space;
        let trials = &self.trials;
        let rng = &mut self.rng;
        let mut coords = Vec::with_capacity(space.len());
        for (dim, (_, domain)) in space.iter().enumerate() {
            let good: Vec<f64> = good_idx.iter().map(|&i| trials[i].coords[dim]).collect();
            let bad: Vec<f64> = bad_idx.iter().map(|&i| trials[i].coords[dim]).collect();
            coords.push(propose_dim(rng, domain, &good, &bad, n_candidates));
        }
        coords
    }

    /// Partition the trials into GOOD (top `gamma` by score) and BAD indices. `NaN` scores are the
    /// worst, so they always fall in BAD. A stable sort keeps ties in trial order (deterministic).
    /// Both groups are non-empty (`ask` only reaches TPE with at least two trials).
    fn split_indices(&self) -> (Vec<usize>, Vec<usize>) {
        let n = self.trials.len();
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&a, &b| {
            let sa = finite_or_worst(self.trials[a].score);
            let sb = finite_or_worst(self.trials[b].score);
            sb.partial_cmp(&sa).unwrap_or(Ordering::Equal) // descending: best first
        });
        // `n >= 2` here (see the doc), so `n - 1 >= 1`; the saturating form is defensive only.
        let raw_good = (self.gamma * n as f64).round() as usize;
        let n_good = raw_good.clamp(1, n.saturating_sub(1).max(1));
        (idx[..n_good].to_vec(), idx[n_good..].to_vec())
    }

    fn coords_to_map(&self, coords: &[f64]) -> IndexMap<String, f64> {
        self.space.iter().zip(coords).map(|((name, _), &c)| (name.clone(), c)).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The store-backed driver.
// ---------------------------------------------------------------------------------------------

/// Bounds of a `run_tpe` run (vike-backtest's harness driver): how many backtests to spend, the RNG seed, and the model knobs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TpeConfig {
    pub n_trials: usize,
    pub seed: u64,
    pub gamma: f64,
    pub n_startup: usize,
    pub n_candidates: usize,
}

impl TpeConfig {
    /// Default trial budget when the CLI omits `--trials`.
    pub const DEFAULT_TRIALS: usize = 64;
    /// Default good/bad quantile split.
    pub const DEFAULT_GAMMA: f64 = 0.25;
    /// Default random warm-up count.
    pub const DEFAULT_STARTUP: usize = 10;
    /// Default EI candidate count per dimension.
    pub const DEFAULT_CANDIDATES: usize = 24;

    /// A config with the published defaults for everything but the trial budget and seed.
    pub fn new(n_trials: usize, seed: u64) -> Self {
        TpeConfig {
            n_trials,
            seed,
            gamma: Self::DEFAULT_GAMMA,
            n_startup: Self::DEFAULT_STARTUP,
            n_candidates: Self::DEFAULT_CANDIDATES,
        }
    }
}

#[path = "tpe_tests.rs"]
#[cfg(test)]
mod tpe_tests;
