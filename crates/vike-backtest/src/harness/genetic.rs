//! A GENETIC search over a `[sweep]` profile — the fourth [`Optimizer`], and the FIRST written
//! against the seam rather than read out of it.
//!
//! Grid, euler and TPE all predate `super::optimize` and were converted onto it; this one was
//! designed by `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` §4 BEFORE the trait
//! landed, precisely so the trait would be tried against a method nobody had written yet. Ruling 15
//! of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` is why it lands
//! second: "trait first, then genetic as its FIRST USER". Everything below is that spec's
//! pseudocode made exact; where it left a decision to the implementation, the decision is argued
//! where it is made.
//!
//! # The genome is INDICES, and every other property falls out of that
//!
//! Euler and TPE both carry `Vec<f64>` COORDINATES and explore BETWEEN the authored grid points.
//! This searcher carries `Vec<usize>` — one index per axis into that axis's own `values` array —
//! so it is a COMBINATORIAL searcher over exactly the point set `super::sweep::expand_paramscan` would
//! enumerate. Three consequences, each load-bearing:
//!
//! 1. **Every operator is integer arithmetic.** Crossover picks a gene from one parent or the
//!    other; mutation steps an index; selection COMPARES scores through
//!    [`super::sweep::cmp_scores_desc`]. No coordinate is ever computed, so no transcendental is
//!    reachable — `crates/vike-backtest/tests/libm_platform_probe.rs`'s
//!    `production_code_calls_libm_not_the_platform` scans this file like every other, and there is
//!    nothing here for it to find. That is what makes the cross-box determinism claim below
//!    STRUCTURAL rather than a promise, and it is the one place this method is stronger than
//!    `super::tpe` — whose `SplitMix64::gaussian` had to be converted to `libm` after the same
//!    seed was measured searching a different part of the space on two boxes.
//! 2. **The reachable set is the grid's point set**, so "compare the searcher against the truth"
//!    is an EQUALITY against [`super::sweep::expand_paramscan_overrides`]'s argmax on a small space,
//!    not a proximity threshold — see `genetic_finds_the_grid_optimum`.
//! 3. It gives up the between-the-points exploration euler and TPE have. Declared rather than
//!    hidden: on a continuous axis this method can only re-pick what the author wrote down, and
//!    euler is the better tool there.
//!
//! # Determinism — the mechanism, and the boundary it stops at
//!
//! `crates/vike-ops/tests/hygiene/clock_pin.rs` lists `crates/vike-backtest` in
//! `DETERMINISM_CRITICAL_CRATES`, and its `no_scoped_crate_declares_an_rng_dependency` refuses a
//! `rand` / `getrandom` / `fastrand` manifest line in ANY section, dev-dependencies included. It
//! does not refuse randomness: its own doc says "a seeded generator whose seed is a parameter is
//! fine — it is an INPUT". So the seed is a [`GeneticConfig`] field with no default, no environment
//! read (which `crates/vike-ops/tests/settings/settings_registry.rs`'s `LIBRARY_PIN` ratchet would refuse by
//! name, this being a `Layer::Library` file) and no clock read (which `clock_pin.rs`'s own
//! `clock_readers_do_not_grow` would refuse by name), and the generator is a dozen lines in this
//! file. No exemption row was added to anything, and none was needed.
//!
//! ⚠ It is **counter-based**, not a stream, and that is a deliberate difference from
//! `super::tpe`'s `SplitMix64`. Every decision is a pure hash of its own COORDINATES —
//! `(seed, purpose, generation, slot)` plus a per-decision counter — so how many draws one operator
//! consumes cannot move any other operator's decisions. A stateful stream couples them: a GA's draw
//! COUNT per generation depends on how many mutations fired, so changing the mutation rule would
//! re-route every later decision, which is exactly the FORK failure `super::tpe`'s module doc
//! records. The mixer is MurmurHash3's `fmix64` (Appleby, public domain, frozen) rather than a
//! second copy of SplitMix64's three constants: `SplitMix64` is a STREAM (`next` advances a state)
//! and is private to `super::tpe`, so sharing it would mean either widening it — which would put
//! `.gaussian()`, the exact call that forked TPE across two boxes, one keystroke from a searcher
//! whose determinism claim is stronger than TPE's — or duplicating its constants in the same crate,
//! which is the shape this repository's gates exist to stop. A counter-based mixer is a different
//! primitive from a stream, so this is not the same code written twice.
//!
//! ⚠ `std::collections::hash_map::DefaultHasher` would have passed every gate here and is still
//! wrong: `std` documents its algorithm as unspecified and free to change between releases, so a
//! toolchain bump would silently move every search with nothing going red. Named as REJECTED rather
//! than merely avoided, because it is the obvious thing a reader reaches for. For the same reason
//! nothing in this module iterates a `HashMap`/`HashSet` — `BTreeMap`/`BTreeSet` throughout, so no
//! ordering depends on `RandomState`.
//!
//! ⚠ **The claim is SCOPED, and the scope is not decoration.** The SEARCHER is bit-identical given
//! the score stream, on every box. The SCORES come from `super::run_backtest` through
//! [`PointEvaluator`], and
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` records that
//! this workspace's equity fold still differs between MSVC and glibc — so a real store-backed run
//! can still crown a different winner on two boxes, for the ENGINE's reason and not this module's.
//! `super::tpe` was corrected for claiming "byte-for-byte" when it held only within a box; this
//! module does not repeat it one file over.
//!
//! # Batch width, and what it inherits for free
//!
//! One `evaluate` call per generation, carrying the WHOLE deduplicated population — the widest
//! batch of any method here, and an ALGORITHMIC statement rather than a policy: every individual is
//! independent. This module names no `ParamscanExec`, no `install_bounded` and no rayon, and therefore
//! inherits the `VIKE_SWEEP_THREADS` memory cap and the order-preserving collect without knowing
//! they exist ([`PointEvaluator`]'s pool rule). A population of 200 gets 4-way concurrency, not 200
//! threads.
//!
//! # Dedup: an elite is CACHED, not re-evaluated
//!
//! The one thing §4 leaves to the implementation. Cached, for three reasons: an elite carried
//! unchanged is a bit-identical candidate over an identical store, so re-running it spends real
//! budget to re-derive a known answer and duplicates a row in the report; `crate::search`'s own
//! `seen` is the in-crate precedent; and it is what makes the budget a count of BACKTESTS rather
//! than of proposals, which is the only budget an operator can act on. ⚠ It argues nothing about
//! lifting dedup into the seam — `super::tpe` deliberately RE-RUNS a re-proposal and
//! `super::euler`'s `euler_and_tpe_rank_rows_exactly_like_run_sweep_with` pins that; nothing here
//! touches it. The consequence to expect: this method's row count is smaller than its proposal
//! count, and smaller still on an axis carrying a duplicate value.
//!
//! The dedup KEY is the rendered VALUES' bit patterns, never the index vector — the same predicate
//! and the same argument as `crate::search`'s `key_of`. An axis written `size = [1.0, 1.0, 2.0]`
//! has two distinct points and three indices; keying on indices would pay for the same backtest
//! twice.
//!
//! # What it does NOT accept
//!
//! [`super::sweep::numeric_sweep_axes`] is the space reader — the same one euler and TPE call, so
//! the array/non-empty/numeric/type-homogeneity rule and its refusal strings stay one fact rather
//! than a fourth copy. That inherits the refusal of string and bool axes, which an index genome
//! would not need (it never asks what is halfway between two values) — so this method could in
//! principle be the second PERMISSIVE one, accepting everything `super::sweep::GridSearch` accepts.
//! Reaching that means widening `super::sweep`'s private `sweep_axis_array` and carrying a second
//! space type here, which is a change with its own argument to make and its own tests to write. A
//! declared v1 cost and a clean follow-up, not something designed around.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use super::optimize::{Candidate, Evaluated, Optimizer, PointEvaluator, SearchOutcome};
use super::sweep::{
    NumericAxis, ParamscanRow, cmp_scores_desc, numeric_sweep_axes, sort_scored_rows,
};
use super::{BacktestProfile, HarnessError};
use vike_ml::tpe::TpeConfig;

// ---------------------------------------------------------------------------------------------
// The deterministic decision stream — see the determinism contract in the module doc.
// ---------------------------------------------------------------------------------------------

/// MurmurHash3's 64-bit finalizer (Austin Appleby, public domain). Published, frozen, and pure
/// integer arithmetic — the whole reason this module can claim a cross-box identical decision
/// stream. It is an avalanche mixer, not a statistical generator: all that is claimed is that
/// adjacent coordinates decorrelate, which is what a search heuristic's draws need.
const fn fmix64(mut z: u64) -> u64 {
    z ^= z >> 33;
    z = z.wrapping_mul(0xff51_afd7_ed55_8ccd);
    z ^= z >> 33;
    z = z.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    z ^= z >> 33;
    z
}

/// The decisions this module makes, as distinct coordinates so two of them can never share a
/// stream. The values are arbitrary and stable; what matters is only that they differ.
const PURPOSE_SEED_SHUFFLE: u64 = 1;
const PURPOSE_PARENT_A: u64 = 2;
const PURPOSE_PARENT_B: u64 = 3;
const PURPOSE_CROSSOVER: u64 = 4;
const PURPOSE_MUTATE: u64 = 5;
const PURPOSE_REPAIR: u64 = 6;

/// A domain separator, so a seed reused by some other counter-based site would not reproduce this
/// module's stream.
const GENETIC_DOMAIN: u64 = 0x6765_6e65_7469_6300; // "genetic\0"

/// One decision's draw sequence, ADDRESSED by its coordinates rather than positioned in a stream.
///
/// Construct one per decision (`Draw::at(seed, purpose, generation, slot)`); the draws it yields
/// are a pure function of those four numbers and of how many have been taken. Nothing carries over
/// between decisions, which is the property the module doc's fork argument rests on.
#[derive(Debug, Clone)]
struct Draw {
    key: u64,
    counter: u64,
}

impl Draw {
    /// Fold the coordinates through the mixer one at a time rather than PACKING them into bit
    /// fields: a packed layout aliases two decisions the moment a field overflows its width, and
    /// nothing would go red when it did.
    fn at(seed: u64, purpose: u64, generation: u64, slot: u64) -> Self {
        let mut key = fmix64(seed ^ GENETIC_DOMAIN);
        key = fmix64(key ^ purpose);
        key = fmix64(key ^ generation);
        key = fmix64(key ^ slot);
        Draw { key, counter: 0 }
    }

    fn next_u64(&mut self) -> u64 {
        self.counter = self.counter.wrapping_add(1);
        fmix64(self.key ^ self.counter.wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }

    /// A uniform index below `n` by Lemire's multiply-shift — ONE draw, no rejection loop, no
    /// modulo and no float. The residual bias is under `n / 2^64` and is stated rather than hidden:
    /// a rejection loop would trade it for a variable draw count, and a fixed draw count is worth
    /// more here than the last 1e-19 of uniformity.
    ///
    /// `n <= 1` has exactly one valid index, so it answers 0 rather than multiplying.
    fn below(&mut self, n: usize) -> usize {
        if n <= 1 {
            return 0;
        }
        ((u128::from(self.next_u64()) * n as u128) >> 64) as usize
    }
}

// ---------------------------------------------------------------------------------------------
// Configuration, and every default's derivation.
// ---------------------------------------------------------------------------------------------

/// Bounds of a genetic run. `seed` is REQUIRED — the same shape [`TpeConfig::new`] takes, and for
/// the same reason: a defaulted seed is a hidden constant that changes results silently, and the
/// operator-facing `--seed` flag belongs in the binary that already spells TPE's.
///
/// The three sizing knobs are `Option`, meaning "derive from the search space". They cannot be
/// derived at construction because the space is not read until [`Optimizer::search`] holds the
/// profile; `GeneticConfig::resolve` is the ONE place every default is computed, and
/// `defaults_scale_with_the_space` pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneticConfig {
    pub seed: u64,
    /// Individuals per generation, i.e. BACKTESTS per generation. `None` derives.
    pub population: Option<usize>,
    /// Generation ceiling. `None` derives.
    pub generations: Option<usize>,
    /// The HARD evaluation budget. `None` derives. Capped at the grid size either way.
    pub max_evaluations: Option<usize>,
}

impl GeneticConfig {
    /// The smallest population at which every operator still MEANS something: elitism takes one
    /// seat, a tournament of [`GeneticConfig::TOURNAMENT`] needs that many draws to be a sample
    /// rather than near-truncation, and crossover needs two parents that can differ. A count of the
    /// seats the operators occupy, not a round number.
    pub const MIN_POPULATION: usize = 4;

    /// Population is the number of BACKTESTS spent before any selection happens, so its ceiling is
    /// a spending decision. Anchored on [`TpeConfig::DEFAULT_TRIALS`] — the sibling method's whole
    /// accepted default budget — because this tree has already accepted that many backtests as a
    /// reasonable amount to spend before a searcher reports. Read as a SYMBOL, so the two cannot
    /// drift into two literals.
    pub const MAX_POPULATION: usize = TpeConfig::DEFAULT_TRIALS;

    /// A one-generation run performs no selection at all — it is a random sample wearing a GA's
    /// name. Two is the smallest run in which selection, crossover and mutation each act once.
    pub const MIN_GENERATIONS: usize = 2;

    /// The absolute ceiling on a DERIVED evaluation budget, composed from the two constants above:
    /// the largest population this file will fund, run for the fewest generations at which the run
    /// is a genetic search at all. It is therefore the largest spend the file's own two arguments
    /// can justify, and nothing below it needs a number of its own.
    ///
    /// ⚠ It exists because `population x generations` had NO absolute ceiling and `generations` is
    /// `GENOME_PASSES * genes` — it scales with the WIDTH of the space, which the `.min(grid_size)`
    /// honesty cap cannot bound because the grid is growing faster still. Two measured cases: eight
    /// axes of two values derived 16 x 16 = 256 evaluations against a 256-point grid (the method
    /// paying enumeration's full price for an approximate answer, i.e. strictly dominated by
    /// `super::sweep::GridSearch`), and twelve axes of three derived 64 x 24 = 1536 backtests, 24x
    /// [`TpeConfig::DEFAULT_TRIALS`]. Both are pinned in `defaults_scale_with_the_space`.
    ///
    /// ⚠ It caps the DERIVATION only, unlike the grid cap in `GeneticConfig::resolve`, which
    /// applies to an explicit budget as well. The two rules differ because the arguments differ:
    /// spending more than enumeration is never defensible, whoever asked for it, while an operator
    /// who names `population` or `generations` has made the spending decision themselves and this
    /// file has no standing to second-guess it. `super::euler`'s `MAX_AXES` is the sibling guard
    /// against the same failure mode; this method reports the trade-off (`GeneticBudget` carries
    /// `grid_size` beside `max_evaluations`, and `Display` prints both) rather than refusing a wide
    /// space outright, which is `docs/decisions/0013-degrade-vs-refuse.md`'s rule.
    pub const MAX_DERIVED_EVALUATIONS: usize = Self::MAX_POPULATION * Self::MIN_GENERATIONS;

    /// Mutation fires at rate `1/d` (`d` = axis count), so ONE gene changes per child in
    /// expectation and `d` generations is one full pass over the genome. Two passes is the smallest
    /// number that lets a beneficial mutation found in pass 1 be RECOMBINED in pass 2 — which is
    /// the only thing crossover is for. One pass would make the crossover operator decorative.
    pub const GENOME_PASSES: usize = 2;

    /// Generations with no improvement to the incumbent before the run stops. Mutation is the only
    /// operator that can introduce a value absent from the current population and it fires once per
    /// child in expectation: one barren generation is noise, two is a coincidence, three is the
    /// population having stopped producing genomes worth the budget. A floor on EVIDENCE, not a
    /// timeout — and an integer count of GENERATIONS rather than an epsilon on a score, because an
    /// epsilon's right value depends on the objective's scale and an uncalibrated constant is a
    /// flake in this repository.
    pub const STALL_GENERATIONS: usize = 3;

    /// Tournament size. `k = 2` is a coin flip between two random individuals and gives almost no
    /// selection pressure at small populations; `k = population` is truncation selection and
    /// destroys diversity in one generation. At [`GeneticConfig::MIN_POPULATION`] this is the
    /// largest `k` that still leaves an individual OUTSIDE the tournament — i.e. the largest `k`
    /// that is still a sample.
    pub const TOURNAMENT: usize = 3;

    /// Elites per generation = `population / ELITE_DIVISOR`, floored at 1. The FLOOR is a
    /// correctness property rather than tuning: without at least one elite carried, the incumbent
    /// can be bred away and the reported best can go BACKWARDS between generations, which for a
    /// search RESULT is a surprise nobody can act on. The 1/8 fraction is the conventional elitism
    /// rate, stated as a convention rather than dressed up as derived — it leaves at least 87.5% of
    /// every generation for exploration.
    pub const ELITE_DIVISOR: usize = 8;

    /// A config that derives every bound from the space it is handed.
    pub fn new(seed: u64) -> Self {
        GeneticConfig { seed, population: None, generations: None, max_evaluations: None }
    }

    /// Fix the population explicitly (a `--population` flag's shape). The `with_*`-returns-`Self`
    /// idiom is `super::tpe::TpeOptimizer`'s.
    pub fn with_population(mut self, n: usize) -> Self {
        self.population = Some(n);
        self
    }

    /// Fix the generation ceiling explicitly.
    pub fn with_generations(mut self, n: usize) -> Self {
        self.generations = Some(n);
        self
    }

    /// Fix the evaluation budget explicitly. Still capped at the grid size — see
    /// `GeneticConfig::resolve`.
    pub fn with_max_evaluations(mut self, n: usize) -> Self {
        self.max_evaluations = Some(n);
        self
    }

    /// THE one place every default is computed, from the space's own shape.
    fn resolve(&self, axes: &[NumericAxis]) -> GeneticPlan {
        let genes = axes.len().max(1);
        let widest = axes.iter().map(|a| a.values.len()).max().unwrap_or(1);
        // ⚠ `saturating_mul`: a handful of wide axes overflow `usize`. A saturated grid size
        // degrades correctly — it clamps the population at MAX_POPULATION and stops binding the
        // budget — but it must saturate rather than wrap, or the honesty cap below would invert.
        let grid_size = axes.iter().fold(1usize, |n, a| n.saturating_mul(a.values.len()));

        // Two FLOORS, each with a job, then the seat count and the spending ceiling.
        //   `widest` — generation 0 is stratified, so at this size it carries EVERY value of EVERY
        //              axis at least once. That is the maximum marginal coverage a population can
        //              buy, and it is what makes the optimum REACHABLE rather than hoped for
        //              (`generation_zero_covers_every_value_of_every_axis`).
        //              ⚠ The guarantee is CONDITIONAL on `widest <= MAX_POPULATION`: the clamp
        //              below is a spending ceiling and wins, so an axis wider than 64 values does
        //              not get full coverage in generation 0 and cannot. What it gets instead is
        //              `seed_population`'s even SPREAD across the whole axis rather than its head —
        //              which is a weaker property, stated here rather than implied away, and the
        //              defect that made stating it necessary was a modular deal that could not
        //              reach any index past `population - 1` at all.
        //   `isqrt`  — the classic sizing rule for an enumerable space: one individual per
        //              sqrt-slice, so the population samples the space at the density at which a
        //              separable objective's per-axis signal is visible in one generation. INTEGER
        //              isqrt, never `(n as f64).sqrt()`, so this line stays clear of the libm probe
        //              for no gain.
        //              ⚠ It sizes the POPULATION and bounds nothing about the total spend. This
        //              comment used to argue it was "the largest population for which `population`
        //              generations still costs no more than the exhaustive grid" — a true statement
        //              about a searcher that ran `population` generations, which this one does not:
        //              it runs `GENOME_PASSES * genes` of them. The spend that actually follows is
        //              `min(grid, population * GENOME_PASSES * genes)`, whose saving against the
        //              grid is `2 * genes / sqrt(grid)` and therefore evaporates on a narrow, deep
        //              space. `MAX_DERIVED_EVALUATIONS` is the ceiling that argument was missing.
        let population = self
            .population
            .unwrap_or_else(|| {
                widest
                    .max(isqrt_ceil(grid_size))
                    .clamp(Self::MIN_POPULATION, Self::MAX_POPULATION)
                    .min(grid_size)
            })
            .max(1);

        let generations =
            self.generations.unwrap_or(Self::GENOME_PASSES * genes).max(Self::MIN_GENERATIONS);

        // TWO ceilings, and they deliberately have different reach.
        //
        // ⚠ `grid_size` is ABSOLUTE — an explicit budget included. The GA may never spend more than
        // the exhaustive grid it is an alternative to. That is `super::euler`'s own honesty rule
        // (its `EulerBudget` reports `equivalent_grid` at the depth ACTUALLY reached, so a run
        // cannot advertise a saving it did not buy), restated as a hard ceiling rather than as a
        // report, because a method that can cost more than enumeration has no reason to exist.
        //
        // ⚠ `MAX_DERIVED_EVALUATIONS` binds a FULLY DERIVED plan and nothing else. Naming any one of
        // the three knobs is a spending decision, and this file has no standing to second-guess one:
        // an explicit `max_evaluations` is the budget outright, and an explicit `population` or
        // `generations` states the spend as a product. See that constant for why an absolute ceiling
        // was needed at all — the short version is that `generations` scales with the axis COUNT, so
        // on a narrow, deep space `population x generations` reached the grid's own price and
        // `.min(grid_size)` could not see it.
        let sized = population.saturating_mul(generations);
        let fully_derived = self.max_evaluations.is_none()
            && self.population.is_none()
            && self.generations.is_none();
        let default_budget =
            if fully_derived { sized.min(Self::MAX_DERIVED_EVALUATIONS) } else { sized };
        let max_evaluations =
            self.max_evaluations.unwrap_or(usize::MAX).min(default_budget).min(grid_size).max(1);

        let elite = (population / Self::ELITE_DIVISOR).max(1).min(population);

        GeneticPlan {
            population,
            generations,
            max_evaluations,
            elite,
            tournament: Self::TOURNAMENT,
            stall_generations: Self::STALL_GENERATIONS,
            grid_size,
            genes,
        }
    }
}

/// Integer `ceil(sqrt(n))`. `usize::isqrt` is exact, so the correction is one comparison, and
/// `r * r` cannot overflow because `r <= isqrt(usize::MAX)`.
fn isqrt_ceil(n: usize) -> usize {
    let r = n.isqrt();
    if r * r < n { r + 1 } else { r }
}

/// A [`GeneticConfig`] resolved against ONE space — every number the loop reads, with nothing left
/// to derive. Separate from the config so the derivation is a pure function that can be pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GeneticPlan {
    population: usize,
    generations: usize,
    /// The HARD ceiling on EVALUATIONS (backtests), never on proposals.
    max_evaluations: usize,
    elite: usize,
    tournament: usize,
    stall_generations: usize,
    grid_size: usize,
    genes: usize,
}

/// What a genetic run cost — the `super::euler::EulerBudget` twin: REPORT the trade-off rather than
/// refuse a space where it is a bad one.
///
/// `proposed - evaluated` is what the dedup cache saved; `evaluated` against `grid_size` is the
/// saving against enumeration, which is the number an operator actually chooses a method on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneticBudget {
    /// Distinct points actually backtested — one per row.
    pub evaluated: usize,
    /// Individuals proposed, before dedup and before the budget truncated a generation.
    pub proposed: usize,
    pub population: usize,
    pub generations_run: usize,
    pub max_generations: usize,
    pub max_evaluations: usize,
    /// What `super::sweep::GridSearch` would have cost over the same space.
    pub grid_size: usize,
    /// `Some(g)` means the run stopped after `g` generations for a QUALITY or DIVERSITY reason —
    /// the incumbent stalled, or the population stopped producing genomes the cache had not already
    /// seen — rather than exhausting its generation or evaluation budget. The first genuine
    /// quality-based early exit in this tree, and it is an ordinary `break`.
    pub converged_at: Option<usize>,
    pub seed: u64,
}

impl std::fmt::Display for GeneticBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "genetic: {} evaluations from {} proposals over {}/{} generations of {} (seed {}; the \
             full grid is {})",
            self.evaluated,
            self.proposed,
            self.generations_run,
            self.max_generations,
            self.population,
            self.seed,
            self.grid_size
        )?;
        if let Some(at) = self.converged_at {
            write!(f, ", converged at generation {at}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The genome and its operators — integer arithmetic only, all the way down.
// ---------------------------------------------------------------------------------------------

/// One individual: an index into each axis's `values`, axes in `numeric_sweep_axes`'s sorted-key
/// order.
type Genome = Vec<usize>;

/// The dedup key: the rendered VALUES' bit patterns. See the module doc — an axis with a repeated
/// value has fewer points than indices, and paying for the same backtest twice is the defect
/// `crate::search`'s `key_of` already avoids by the same predicate.
fn key_of(axes: &[NumericAxis], genome: &Genome) -> Vec<u64> {
    axes.iter().zip(genome).map(|(a, &i)| a.values[i].to_bits()).collect()
}

/// Render one genome as the [`Candidate`] the evaluator takes. `integral` renders back as a TOML
/// integer, so a searched point deserializes exactly as the grid path's clone of the same authored
/// value would — the third copy of the three-line render `super::euler`'s `AxisKind::to_toml` and
/// `super::tpe::ParamDomain`'s `to_toml` already carry, duplicated deliberately because the spec
/// (§5, item 8) refuses a shared space type.
fn render(axes: &[NumericAxis], genome: &Genome) -> Candidate {
    axes.iter()
        .zip(genome)
        .map(|(a, &i)| {
            let x = a.values[i];
            let v = if a.integral {
                toml::Value::Integer(x.round() as i64)
            } else {
                toml::Value::Float(x)
            };
            (a.key.clone(), v)
        })
        .collect()
}

/// Generation 0: STRATIFIED, not uniform noise. Each axis deals its values evenly across the
/// population slots and then permutes that dealing with its own Fisher-Yates shuffle, so with
/// `population >= |values|` every value of that axis appears at least once while the axes stay
/// mutually decorrelated. This is Latin-hypercube sampling adapted to a discrete grid; it cuts the
/// variance of generation 0, which is what makes `genetic_finds_the_grid_optimum` a real test
/// rather than a lucky one.
///
/// ⚠ **The deal is `slot * k / population`, a SPREAD — never `slot % k`, a modular wrap.** They are
/// the same multiset whenever `population >= k` (both deal each value `population / k` times, give
/// or take one) and they diverge completely when the axis is WIDER than the population: the wrap
/// deals exactly `{0 .. population - 1}` and the shuffle only permutes it, so every index past
/// `population - 1` is absent from generation 0 under EVERY seed, while the spread walks the whole
/// axis in even steps. That is not hypothetical at the derived defaults — `resolve` clamps the
/// population at [`GeneticConfig::MAX_POPULATION`] (64), so an authored axis of 99 values wrapped
/// to indices 0..=63 and never seeded the top 35 of them. Mutation recovers the tail over later
/// generations, so it was a seeding BIAS rather than an unreachable region: the search was anchored
/// on the low-index corner of every wide axis, which is the opposite of what stratified seeding is
/// for. `generation_zero_spans_an_axis_wider_than_the_population` is the pin.
fn seed_population(axes: &[NumericAxis], population: usize, seed: u64) -> Vec<Genome> {
    let mut pop = vec![vec![0usize; axes.len()]; population];
    for (ai, axis) in axes.iter().enumerate() {
        let k = axis.values.len();
        // Integer-only, one multiplication per slot, and `population >= 1` at every call site
        // (`resolve` floors it at 1), so the division is safe. `s * k` is the one product here that
        // could overflow on an absurd space; `s < population` makes the quotient `< k` by
        // construction otherwise, so the clamp exists solely so a saturated product degrades to
        // "deal the top index" instead of off the axis.
        let top = k.saturating_sub(1);
        let mut deal: Vec<usize> =
            (0..population).map(|s| (s.saturating_mul(k) / population).min(top)).collect();
        let mut draw = Draw::at(seed, PURPOSE_SEED_SHUFFLE, ai as u64, 0);
        for i in (1..deal.len()).rev() {
            let j = draw.below(i + 1);
            deal.swap(i, j);
        }
        for (slot, &v) in deal.iter().enumerate() {
            pop[slot][ai] = v;
        }
    }
    pop
}

/// Tournament selection, comparing through [`super::sweep::cmp_scores_desc`] — the crate's ONE
/// ordering rule, so a tournament's winner, `sort_scored_rows` and the report's first row cannot
/// disagree, and a `NaN` (a failed backtest) loses every tournament for free rather than by a rule
/// restated here. Displacing the incumbent requires a STRICT improvement, so a tie resolves to the
/// earlier-drawn individual and never to a comparator's visit order.
///
/// This also rules OUT fitness-proportionate ("roulette") selection on correctness grounds rather
/// than determinism ones: Sharpe and total return go negative routinely, and roulette is undefined
/// there.
fn tournament(scores: &[f64], k: usize, draw: &mut Draw) -> usize {
    let n = scores.len();
    let mut best = draw.below(n);
    for _ in 1..k.max(1) {
        let challenger = draw.below(n);
        if cmp_scores_desc(scores[challenger], scores[best]) == Ordering::Less {
            best = challenger;
        }
    }
    best
}

/// UNIFORM crossover, per gene. Single-point crossover encodes a linkage assumption between
/// ADJACENT genes, and this genome's order is `numeric_sweep_axes`'s — `[sweep]`'s keys sorted
/// ALPHABETICALLY. Gene adjacency here is an artefact of parameter naming, so single-point's whole
/// premise is false by construction. Uniform is closed over the space (each axis's index range is
/// independent), so a child is always valid and no repair-for-validity step exists.
fn crossover(a: &Genome, b: &Genome, draw: &mut Draw) -> Genome {
    a.iter().zip(b).map(|(&x, &y)| if draw.next_u64() & 1 == 0 { x } else { y }).collect()
}

/// Step an index to a DIFFERENT one, uniformly among the other `k - 1`. A mutation that may redraw
/// the value it already had is a silent no-op that inflates the nominal rate and cannot be measured
/// from outside; `a_mutation_always_moves` pins that it never does.
fn step_to_another(current: usize, k: usize, draw: &mut Draw) -> usize {
    debug_assert!(k >= 2, "callers skip a single-valued axis");
    (current + 1 + draw.below(k - 1)) % k
}

/// Per-gene mutation at rate `1/d` — Muhlenbein's rule, so ONE gene changes per child in
/// expectation and the rate is DERIVED from the space's own dimension rather than chosen. Spelled
/// as an integer draw (`below(d) == 0`), never a float comparison, which is part of what keeps the
/// whole operator path off the platform's libm. A single-valued axis is skipped: it has nowhere to
/// go.
fn mutate(genome: &mut Genome, axes: &[NumericAxis], genes: usize, draw: &mut Draw) {
    for (gene, axis) in genome.iter_mut().zip(axes) {
        let k = axis.values.len();
        // The rate draw is taken whether or not the axis CAN move, so a single-valued axis does not
        // shift every later gene's decision.
        let fires = draw.below(genes) == 0;
        if k >= 2 && fires {
            *gene = step_to_another(*gene, k, draw);
        }
    }
}

/// Redraw ONE mutable gene — the repair step's unit. A child duplicating an already-evaluated point
/// costs a population slot for nothing, and a converged population produces them constantly.
fn redraw_one_gene(genome: &mut Genome, axes: &[NumericAxis], draw: &mut Draw) {
    let mutable: Vec<usize> = (0..axes.len()).filter(|&i| axes[i].values.len() >= 2).collect();
    if mutable.is_empty() {
        return;
    }
    let i = mutable[draw.below(mutable.len())];
    genome[i] = step_to_another(genome[i], axes[i].values.len(), draw);
}

/// Breed the next generation: elites carried unchanged, then tournament-selected parents through
/// uniform crossover and mutation, then a BOUNDED repair pass.
///
/// The repair retry limit is the axis count: after `d` further mutations a child shares nothing with
/// its parents and additional retries are plain rejection sampling, so `d` is where repairing stops
/// being repair. Bounded, so it cannot spin on an exhausted space.
fn next_generation(
    axes: &[NumericAxis],
    pop: &[Genome],
    seen: &BTreeMap<Vec<u64>, f64>,
    plan: &GeneticPlan,
    seed: u64,
    generation: usize,
) -> Vec<Genome> {
    let scores: Vec<f64> =
        pop.iter().map(|g| seen.get(&key_of(axes, g)).copied().unwrap_or(f64::NAN)).collect();

    // `sort_by` is STABLE, so tied elites keep population order — the same tie rule the report's own
    // `sort_scored_rows` uses.
    let mut order: Vec<usize> = (0..pop.len()).collect();
    order.sort_by(|&i, &j| cmp_scores_desc(scores[i], scores[j]));

    let mut next: Vec<Genome> =
        order.iter().take(plan.elite.min(pop.len())).map(|&i| pop[i].clone()).collect();

    let gen_coord = generation as u64;
    for slot in next.len()..plan.population {
        let s = slot as u64;
        let mut pick_a = Draw::at(seed, PURPOSE_PARENT_A, gen_coord, s);
        let mut pick_b = Draw::at(seed, PURPOSE_PARENT_B, gen_coord, s);
        let a = tournament(&scores, plan.tournament, &mut pick_a);
        let b = tournament(&scores, plan.tournament, &mut pick_b);

        let mut cross = Draw::at(seed, PURPOSE_CROSSOVER, gen_coord, s);
        let mut child = crossover(&pop[a], &pop[b], &mut cross);

        let mut mutation = Draw::at(seed, PURPOSE_MUTATE, gen_coord, s);
        mutate(&mut child, axes, plan.genes, &mut mutation);

        let mut repair = Draw::at(seed, PURPOSE_REPAIR, gen_coord, s);
        let mut tries = 0;
        while tries < plan.genes && seen.contains_key(&key_of(axes, &child)) {
            redraw_one_gene(&mut child, axes, &mut repair);
            tries += 1;
        }
        next.push(child);
    }
    next
}

// ---------------------------------------------------------------------------------------------
// The optimizer.
// ---------------------------------------------------------------------------------------------

/// The genetic search, as an [`Optimizer`].
///
/// ⚠ **It is REGISTERED now** — `backtest --optimizer genetic --seed N`, through
/// `crates/vike-backtest/src/backtest_cli.rs`'s `optimizer_for`. This paragraph said the opposite
/// until that follow-up landed, and what it said was that the registration was deliberately held
/// back: `optimizer_for` was being rewritten from a hand-written ladder into a `Method` enum at the
/// time, and two branches inserting a row into one table at different offsets is the silent
/// duplicate-insertion class this repository has already paid for. The follow-up cost what that
/// note predicted — one enum variant, one `optimizer_for` arm, the flag rows, and `"genetic"` in
/// that file's method-name literals.
///
/// ⚠ The one thing it flagged for the follow-up was real and is worth carrying: `METHOD_FLAGS` was
/// ONE OWNER per flag and `--seed` belonged to `"tpe"`, so that table is now keyed on a SET of
/// owners. Its own doc argues the shape and the two alternatives that were rejected.
///
/// ⚠ **`--optimizer genetic` REFUSES to run without `--seed`**, where tpe's `--seed` defaults to
/// `0`. That is [`GeneticConfig`]'s "seed is REQUIRED" decision carried up to the operator rather
/// than quietly undone at the one call site that could hand this type a constant;
/// `backtest_cli.rs`'s `require_seed` carries the full argument, including why the asymmetry with
/// tpe is not the one-flag-two-fates defect class.
///
/// ⚠ **The three sizing knobs reach no flag**, so a CLI run is always the fully-derived plan
/// [`GeneticConfig::resolve`] computes — which means `MAX_DERIVED_EVALUATIONS`, the ceiling that
/// binds a fully derived plan and nothing else, binds EVERY run an operator can currently ask for.
/// Deliberate for now: each knob is a spending decision that owes its own argument and its own
/// `METHOD_FLAGS` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneticSearch {
    pub cfg: GeneticConfig,
}

impl GeneticSearch {
    /// This method's operator-facing name, as a const so the space reader interpolates the SAME
    /// string [`Optimizer::name`] answers with — the idiom `super::euler::EulerSearch` and
    /// `super::tpe::TpeSearch` both use.
    const NAME: &'static str = "genetic";

    /// A method value over `cfg`. [`GeneticConfig`] is `Copy`.
    pub fn new(cfg: GeneticConfig) -> Self {
        GeneticSearch { cfg }
    }

    /// The loop, plus the typed budget [`Optimizer::search`]'s summary line is rendered from.
    fn search_with_budget(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<(Vec<ParamscanRow>, GeneticBudget), HarnessError> {
        let axes = numeric_sweep_axes(base, Self::NAME)?;
        let plan = self.cfg.resolve(&axes);
        let seed = self.cfg.seed;

        let mut pop = seed_population(&axes, plan.population, seed);
        // BTreeMap, not HashMap: nothing here may depend on `RandomState`'s per-process order.
        let mut seen: BTreeMap<Vec<u64>, f64> = BTreeMap::new();
        let mut rows: Vec<ParamscanRow> = Vec::new();
        let mut proposed = 0usize;
        let mut generations_run = 0usize;
        let mut converged_at: Option<usize> = None;
        let mut best = f64::NAN;
        let mut last_gain = 0usize;

        for generation in 0..plan.generations {
            proposed += pop.len();

            // Dedup in POPULATION order — against the cache, and within the batch itself.
            let mut batch_keys: BTreeSet<Vec<u64>> = BTreeSet::new();
            let mut fresh: Vec<Genome> = Vec::with_capacity(pop.len());
            for g in &pop {
                let key = key_of(&axes, g);
                if seen.contains_key(&key) || !batch_keys.insert(key) {
                    continue;
                }
                fresh.push(g.clone());
            }
            fresh.truncate(plan.max_evaluations.saturating_sub(rows.len()));

            if fresh.is_empty() {
                // The population has collapsed onto points already evaluated. Stop rather than hand
                // the evaluator an EMPTY batch, which every implementation would answer correctly
                // and which would still burn the remaining generations doing nothing.
                converged_at = Some(generation);
                break;
            }

            let batch: Vec<Candidate> = fresh.iter().map(|g| render(&axes, g)).collect();
            let evaluated = eval.evaluate(batch);
            assert_eq!(
                evaluated.len(),
                fresh.len(),
                "a PointEvaluator answers one Evaluated per candidate, in input order (trait \
                 contract)"
            );
            for (g, Evaluated { row, score }) in fresh.iter().zip(evaluated) {
                seen.insert(key_of(&axes, g), score);
                rows.push(row);
                // A STRICT improvement, through the shared comparator: a NaN can never displace a
                // finite incumbent, and a tie is not a gain.
                if cmp_scores_desc(score, best) == Ordering::Less {
                    best = score;
                    last_gain = generation;
                }
            }
            generations_run = generation + 1;

            if rows.len() >= plan.max_evaluations {
                break; // budget exhausted — reported as a budget, never as convergence
            }
            if generation + 1 >= plan.generations {
                break;
            }
            if generation - last_gain >= plan.stall_generations {
                converged_at = Some(generations_run);
                break;
            }

            let bred = next_generation(&axes, &pop, &seen, &plan, seed, generation);
            pop = bred;
        }

        let budget = GeneticBudget {
            evaluated: rows.len(),
            proposed,
            population: plan.population,
            generations_run,
            max_generations: plan.generations,
            max_evaluations: plan.max_evaluations,
            grid_size: plan.grid_size,
            converged_at,
            seed,
        };
        Ok((rows, budget))
    }
}

impl Optimizer for GeneticSearch {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    /// The numeric/type-homogeneous narrowing, through the same shared reader euler and TPE use —
    /// so a space this method cannot search is refused BEFORE a store opens, not at generation 0.
    fn accepts(&self, base: &BacktestProfile) -> Result<(), HarnessError> {
        numeric_sweep_axes(base, Self::NAME).map(|_| ())
    }

    /// **`None`, for `super::euler::EulerSearch::budget_hint`'s reason plus two of its own.**
    ///
    /// `GeneticPlan`'s `max_evaluations` is a HARD ceiling and is therefore tempting, but it is a
    /// ceiling the run routinely beats: every proposal the dedup cache has already seen costs no
    /// backtest (`GeneticBudget` reports `proposed - evaluated` precisely because that saving is
    /// large), and `converged_at` exits the loop early when the incumbent stalls or the population
    /// stops producing unseen genomes. A denominator a run beats by a factor turns a progress line
    /// into a countdown to a time that never arrives.
    ///
    /// ⚠ An `AtMost` total would be the honest shape for both this method and euler, and
    /// `super::optimize::ProgressEvent` deliberately does not have one — it renders `n/N`, which reads as a
    /// promise. That is the extension point if an ETA under these two methods is ever wanted; it is
    /// not a hole to be filled by publishing the bound as if it were exact.
    ///
    /// What genetic reports instead is the truth afterwards: [`GeneticBudget`], through
    /// `SearchOutcome`'s already-rendered `summary`.
    fn budget_hint(&self, _base: &BacktestProfile) -> Option<u64> {
        None
    }

    fn search(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<SearchOutcome, HarnessError> {
        let (rows, budget) = self.search_with_budget(base, eval)?;

        // ⚠ `best` comes from THE shared ordering rule applied to these rows, never from the loop's
        // own incumbent — the same trap `super::tpe::TpeSearch::search` calls out about
        // `TpeOptimizer::best`. A summary line that could disagree with the report's first row is
        // worse than no summary.
        let mut ranked = rows.clone();
        sort_scored_rows(&mut ranked);
        let best = ranked.first().and_then(|r| r.score);
        let summary = format!(
            "{budget}, best score {}",
            best.map(|s| format!("{s:.4}")).unwrap_or_else(|| "n/a".to_string())
        );

        // Rows in EVALUATION order — the trajectory, preserved up to the assembler.
        Ok(SearchOutcome { rows, summary: Some(summary) })
    }
}

#[path = "genetic_tests.rs"]
#[cfg(test)]
mod genetic_tests;
