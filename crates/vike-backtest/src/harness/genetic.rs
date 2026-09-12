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
//! so it is a COMBINATORIAL searcher over exactly the point set `super::sweep::expand_sweep` would
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
//!    is an EQUALITY against [`super::sweep::expand_sweep_overrides`]'s argmax on a small space,
//!    not a proximity threshold — see `genetic_finds_the_grid_optimum`.
//! 3. It gives up the between-the-points exploration euler and TPE have. Declared rather than
//!    hidden: on a continuous axis this method can only re-pick what the author wrote down, and
//!    euler is the better tool there.
//!
//! # Determinism — the mechanism, and the boundary it stops at
//!
//! `crates/vike-ops/tests/clock_pin.rs` lists `crates/vike-backtest` in
//! `DETERMINISM_CRITICAL_CRATES`, and its `no_scoped_crate_declares_an_rng_dependency` refuses a
//! `rand` / `getrandom` / `fastrand` manifest line in ANY section, dev-dependencies included. It
//! does not refuse randomness: its own doc says "a seeded generator whose seed is a parameter is
//! fine — it is an INPUT". So the seed is a [`GeneticConfig`] field with no default, no environment
//! read (which `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchet would refuse by
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
//! independent. This module names no `SweepExec`, no `install_bounded` and no rayon, and therefore
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
use super::sweep::{NumericAxis, SweepRow, cmp_scores_desc, numeric_sweep_axes, sort_scored_rows};
use super::tpe::TpeConfig;
use super::{BacktestProfile, HarnessError};

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
    ) -> Result<(Vec<SweepRow>, GeneticBudget), HarnessError> {
        let axes = numeric_sweep_axes(base, Self::NAME)?;
        let plan = self.cfg.resolve(&axes);
        let seed = self.cfg.seed;

        let mut pop = seed_population(&axes, plan.population, seed);
        // BTreeMap, not HashMap: nothing here may depend on `RandomState`'s per-process order.
        let mut seen: BTreeMap<Vec<u64>, f64> = BTreeMap::new();
        let mut rows: Vec<SweepRow> = Vec::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::optimize::optimize;
    use crate::harness::sweep::{RankBy, expand_sweep_overrides};
    use std::sync::Mutex;

    // ---------------------------------------------------------------------------------------
    // Fixtures. Deliberately STORE-FREE and backtest-free: the searcher's whole contract with the
    // world is `PointEvaluator`, so every test below runs in the trait-only `hist-replay` build as
    // well as the `datafusion-store` one, and a failure names the searcher rather than the engine.
    // ---------------------------------------------------------------------------------------------

    /// A [`PointEvaluator`] with no backtest behind it: it scores a candidate with an analytic
    /// objective and records the width of every batch it was handed. The shape
    /// `harness::optimize`'s own `StubEvaluator` established.
    struct StubEvaluator {
        score_of: fn(&Candidate) -> f64,
        batches: Mutex<Vec<usize>>,
    }

    impl StubEvaluator {
        fn new(score_of: fn(&Candidate) -> f64) -> Self {
            StubEvaluator { score_of, batches: Mutex::new(Vec::new()) }
        }

        fn batches(&self) -> Vec<usize> {
            self.batches.lock().expect("no test panics while holding this").clone()
        }
    }

    impl PointEvaluator for StubEvaluator {
        fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
            self.batches.lock().expect("no test panics while holding this").push(batch.len());
            batch
                .into_iter()
                .map(|c| {
                    let score = (self.score_of)(&c);
                    Evaluated {
                        row: SweepRow {
                            overrides: c,
                            report: None,
                            error: Some("stub evaluator: no backtest".to_string()),
                            score: Some(score),
                        },
                        score,
                    }
                })
                .collect()
        }

        fn rank_by(&self) -> RankBy {
            RankBy::Objective("stub".to_string())
        }
    }

    fn base_with_sweep(sweep: &str) -> BacktestProfile {
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

    /// Three float axes of six values each: 216 grid points, small enough for
    /// [`expand_sweep_overrides`] to enumerate as the TRUTH and large enough that the derived
    /// budget (90 evaluations) is a real saving rather than the whole grid in disguise.
    const BOWL_SWEEP: &str = "[sweep]\n\
                              a = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]\n\
                              b = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]\n\
                              c = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]";

    /// The same objective over three axes of FIVE values: 125 points, which the derived budget
    /// searches 72 of. Used by `genetic_finds_the_grid_optimum` alone, and the difference from
    /// [`BOWL_SWEEP`] is deliberate — that test asserts an EXACT equality against the oracle for
    /// every seed, so it is sized where the claim is one the method can actually keep rather than
    /// where the headline saving looks best. On this space `bowl`'s maximum lands at `(4, 1, 4)`:
    /// its `c` target sits OUTSIDE the axis, so the optimum is on the boundary in two of three
    /// coordinates, which a searcher that drifts toward the middle of a space cannot fake.
    const SMALL_SWEEP: &str = "[sweep]\n\
                               a = [0.0, 1.0, 2.0, 3.0, 4.0]\n\
                               b = [0.0, 1.0, 2.0, 3.0, 4.0]\n\
                               c = [0.0, 1.0, 2.0, 3.0, 4.0]";

    fn axis_value(c: &Candidate, key: &str) -> f64 {
        c.iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .expect("the candidate carries every swept axis")
    }

    /// A separable, unimodal bowl whose single maximum sits at `(4, 1, 5)` — deliberately NOT the
    /// centre of the space, and on the boundary in one axis, so a searcher that merely samples near
    /// the middle cannot find it by accident. Pure `+ - *`: no transcendental, so the objective is
    /// identical on every box and this test measures the SEARCHER.
    fn bowl(c: &Candidate) -> f64 {
        let (x, y, z) = (axis_value(c, "a"), axis_value(c, "b"), axis_value(c, "c"));
        -((x - 4.0) * (x - 4.0) + (y - 1.0) * (y - 1.0) + (z - 5.0) * (z - 5.0))
    }

    fn flat(_c: &Candidate) -> f64 {
        1.0
    }

    fn by_size(c: &Candidate) -> f64 {
        axis_value(c, "size")
    }

    fn always_unrankable(_c: &Candidate) -> f64 {
        f64::NAN
    }

    /// The grid's TRUE argmax under `bowl`, plus the assertion that it is unique — without which
    /// `genetic_finds_the_grid_optimum` would be comparing against one of several right answers.
    fn grid_optimum(base: &BacktestProfile) -> Candidate {
        let grid = expand_sweep_overrides(base).expect("the fixture space expands");
        let mut best = 0usize;
        for i in 1..grid.len() {
            if cmp_scores_desc(bowl(&grid[i]), bowl(&grid[best])) == Ordering::Less {
                best = i;
            }
        }
        let top = bowl(&grid[best]);
        assert_eq!(
            grid.iter().filter(|c| bowl(c) == top).count(),
            1,
            "the fixture objective must have ONE maximum, or the equality below is not a test"
        );
        grid[best].clone()
    }

    fn candidates(rows: &[SweepRow]) -> Vec<Candidate> {
        rows.iter().map(|r| r.overrides.clone()).collect()
    }

    // ---------------------------------------------------------------------------------------
    // 1. Determinism — RUN IT TWICE AND COMPARE, never assert it.
    // ---------------------------------------------------------------------------------------

    /// The whole determinism claim, measured rather than declared: the same seed over the same
    /// profile produces the same candidates, in the same order, with the same batch cadence and the
    /// same summary. The comparison is the FULL evaluation-order trajectory, not the winner —
    /// a searcher that reached the same answer down a different path would fail here, which is the
    /// point.
    #[test]
    fn the_same_seed_searches_identically() {
        let base = base_with_sweep(BOWL_SWEEP);
        let opt = GeneticSearch::new(GeneticConfig::new(20_260_909));

        let first = StubEvaluator::new(bowl);
        let a = opt.search(&base, &first).expect("the search runs");
        let second = StubEvaluator::new(bowl);
        let b = opt.search(&base, &second).expect("the search runs");

        assert_eq!(candidates(&a.rows), candidates(&b.rows), "same seed, same candidate sequence");
        assert_eq!(first.batches(), second.batches(), "same seed, same batch cadence");
        assert_eq!(a.summary, b.summary, "same seed, same reported budget");
        assert!(!a.rows.is_empty(), "a search that evaluated nothing proves nothing");
    }

    /// The non-vacuity half. Without it, a searcher that ignored its seed entirely — or a generator
    /// that returned a constant — would satisfy the test above.
    #[test]
    fn a_different_seed_moves_the_search() {
        let base = base_with_sweep(BOWL_SWEEP);
        let one = GeneticSearch::new(GeneticConfig::new(1));
        let two = GeneticSearch::new(GeneticConfig::new(2));

        let ea = StubEvaluator::new(bowl);
        let eb = StubEvaluator::new(bowl);
        let a = one.search(&base, &ea).expect("the search runs");
        let b = two.search(&base, &eb).expect("the search runs");

        assert_ne!(
            candidates(&a.rows),
            candidates(&b.rows),
            "two seeds must search differently, or the seed is decorative"
        );
    }

    // ---------------------------------------------------------------------------------------
    // 2. Correctness against truth — the exhaustive grid is the oracle.
    // ---------------------------------------------------------------------------------------

    /// The real correctness test: on a space small enough for [`expand_sweep_overrides`] to
    /// enumerate exhaustively, the genetic searcher's best candidate is checked against the grid's
    /// TRUE argmax — an equality rather than a proximity threshold, which is only available because
    /// the genome is a vector of INDICES and the reachable set is therefore exactly the grid's point
    /// set. The searcher is given the DERIVED defaults, i.e. 72 evaluations of 125 points, with no
    /// knob tuned for the fixture.
    ///
    /// # ⚠ The three numbers below are MEASURED, and here is the measurement
    ///
    /// Run over seeds 0..16 on a the CI box lane, 2026-09-10, against [`SMALL_SWEEP`] at the DERIVED
    /// defaults (population 12, 6 generations, 72 evaluations), the per-seed SHORTFALL —
    /// how many of the 125 grid points score strictly better than the point that run crowned — was
    /// `[0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0]`, and 6 of the 16 runs stopped early on the
    /// stall exit. So **fourteen seeds found the unique optimum outright and the other two landed on
    /// the second-best point in the whole space**.
    ///
    /// ⚠ That array is the RE-MEASUREMENT taken after `seed_population`'s deal was corrected from a
    /// modular wrap to a spread; the earlier one read `[0 x 14, 1, 1]`. The headline is unchanged
    /// (fourteen exact, worst shortfall 1, six stalls) because on this fixture the axis is narrower
    /// than the population and both deals cover it — WHICH seeds miss moved, how many did not.
    ///
    /// The thresholds are that measurement with slack, and each gates a different claim. ⚠ Note what
    /// this test does and does not assert across seeds, because the distinction is the whole
    /// calibration: exact equality against the oracle is asserted for ONE seed, and every other seed
    /// is held only to the two aggregate floors below. It is not "all sixteen seeds land on the
    /// argmax", and a GA cannot promise that.
    ///
    /// * `EXACT_SEED` — ONE named seed's best candidate must EQUAL the oracle's, byte for byte.
    ///   This is a deterministic PIN rather than a probabilistic claim (the whole searcher is a pure
    ///   function of the seed), and it is what makes "checked against the true optimum" literal
    ///   rather than statistical.
    /// * `EXACT_HITS_FLOOR = 12` of 16 — measured 14. Two seeds of slack, because a GA carries no
    ///   global-optimum guarantee and an operator change that costs one seed is tuning, not a
    ///   regression.
    /// * `WORST_SHORTFALL = 2` — measured 1. This is the claim that actually matters to an operator:
    ///   whatever the seed, this method's answer is inside the grid's top three of 125 points at 58%
    ///   of the grid's cost.
    ///
    /// ⚠ If a future operator change reddens this, the correct response is to find out whether the
    /// search genuinely got worse — NOT to raise the budget or lower a floor until it goes green.
    #[test]
    fn genetic_finds_the_grid_optimum() {
        const EXACT_SEED: u64 = 0;
        const EXACT_HITS_FLOOR: usize = 12;
        const WORST_SHORTFALL: usize = 2;

        let base = base_with_sweep(SMALL_SWEEP);
        let truth = grid_optimum(&base);
        let grid = expand_sweep_overrides(&base).expect("the fixture space expands");

        let seeds = 16u64;
        // Per seed: how many grid points are STRICTLY better than what this run crowned. Zero means
        // it found the unique optimum. A RANK rather than a score, so the floors above survive a
        // change of fixture objective.
        let mut shortfall: Vec<usize> = Vec::new();
        let mut stalled = 0usize;
        for seed in 0..seeds {
            let eval = StubEvaluator::new(bowl);
            let opt = GeneticSearch::new(GeneticConfig::new(seed));
            let out = optimize(&opt, &base, &eval).expect("the search runs");
            let best = out.report.rows[0].overrides.clone();
            if seed == EXACT_SEED {
                assert_eq!(best, truth, "seed {EXACT_SEED} must land exactly on the oracle");
            }
            shortfall.push(grid.iter().filter(|c| bowl(c) > bowl(&best)).count());

            let (_, budget) = GeneticSearch::new(GeneticConfig::new(seed))
                .search_with_budget(&base, &StubEvaluator::new(bowl))
                .expect("the search runs");
            if budget.converged_at.is_some() {
                stalled += 1;
            }
        }

        let found = shortfall.iter().filter(|&&r| r == 0).count();
        let worst = shortfall.iter().copied().max().expect("sixteen seeds");
        assert!(
            found >= EXACT_HITS_FLOOR,
            "only {found}/{seeds} seeds found the oracle {truth:?}; shortfall {shortfall:?}"
        );
        assert!(
            worst <= WORST_SHORTFALL,
            "a seed crowned a point with {worst} strictly better grid points; shortfall \
             {shortfall:?}"
        );
        // Non-vacuity for the quality exit at the DERIVED sizes: measured 6 of 16 here, so a stall
        // default that had stopped firing altogether would be visible rather than silent.
        assert!(stalled > 0, "the stall exit never fired — is it still reachable at the defaults?");
    }

    /// The seed-independent half of the same claim, and the one that can never be luck: the
    /// searcher is confined to the grid's point set, so its best can never EXCEED the exhaustive
    /// grid's best. A searcher that had drifted off-grid (an interpolated coordinate, a rounding
    /// bug in `render`) could beat the oracle, and would be caught here.
    #[test]
    fn genetic_never_beats_the_exhaustive_grid() {
        let base = base_with_sweep(BOWL_SWEEP);
        let top = bowl(&grid_optimum(&base));
        for seed in 0..8u64 {
            let eval = StubEvaluator::new(bowl);
            let opt = GeneticSearch::new(GeneticConfig::new(seed));
            let out = opt.search(&base, &eval).expect("the search runs");
            for row in &out.rows {
                let s = row.score.expect("the stub scores every row");
                assert!(s <= top, "seed {seed} scored {s} above the grid's true maximum {top}");
            }
        }
    }

    /// Every candidate the searcher emits is a MEMBER of the grid's point set — TOML value for TOML
    /// value. Seed-independent, threshold-free, and the property the equality above rests on.
    #[test]
    fn every_evaluated_candidate_is_a_grid_point() {
        let base = base_with_sweep(BOWL_SWEEP);
        let grid: BTreeSet<String> = expand_sweep_overrides(&base)
            .expect("the fixture space expands")
            .iter()
            .map(|c| format!("{c:?}"))
            .collect();
        for seed in 0..8u64 {
            let eval = StubEvaluator::new(bowl);
            let opt = GeneticSearch::new(GeneticConfig::new(seed));
            let out = opt.search(&base, &eval).expect("the search runs");
            for row in &out.rows {
                assert!(
                    grid.contains(&format!("{:?}", row.overrides)),
                    "seed {seed} proposed {:?}, which is not a grid point",
                    row.overrides
                );
            }
        }
    }

    /// An INTEGER axis renders back as a TOML integer, byte-identical to what the grid path's clone
    /// of the authored value produces — so a strategy reading the param through `as_integer()`
    /// behaves the same under this method as under the grid.
    #[test]
    fn an_integer_axis_renders_as_toml_integers() {
        let base = base_with_sweep("[sweep]\nn = [1, 5, 9, 13]");
        let eval = StubEvaluator::new(flat);
        let opt = GeneticSearch::new(GeneticConfig::new(3));
        let out = opt.search(&base, &eval).expect("the search runs");
        assert!(!out.rows.is_empty());
        for row in &out.rows {
            match &row.overrides[0].1 {
                toml::Value::Integer(i) => assert!([1, 5, 9, 13].contains(i)),
                other => panic!("an all-integer axis must render as an integer, got {other:?}"),
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // 3. Termination — a budget that can be exhausted, and is.
    // ---------------------------------------------------------------------------------------

    /// The budget is a HARD ceiling on EVALUATIONS and it truncates a generation mid-flight: with a
    /// population of sixteen and a budget of five, exactly five backtests happen and the first (and
    /// only) batch is five wide.
    #[test]
    fn the_budget_is_exhausted_and_truncates_a_generation() {
        let base = base_with_sweep(BOWL_SWEEP);
        for seed in 0..8u64 {
            let eval = StubEvaluator::new(bowl);
            let cfg = GeneticConfig::new(seed)
                .with_population(16)
                .with_generations(3)
                .with_max_evaluations(5);
            let out = GeneticSearch::new(cfg).search(&base, &eval).expect("the search runs");
            assert_eq!(out.rows.len(), 5, "seed {seed}: the budget is a hard ceiling");
            assert_eq!(eval.batches(), vec![5], "seed {seed}: one truncated generation, no more");
        }
    }

    /// The generation ceiling terminates a run whose budget never binds, and the whole run stays
    /// under BOTH ceilings.
    #[test]
    fn the_generation_ceiling_terminates_the_run() {
        let base = base_with_sweep(BOWL_SWEEP);
        let eval = StubEvaluator::new(bowl);
        let cfg = GeneticConfig::new(11).with_population(4).with_generations(5);
        let (rows, budget) =
            GeneticSearch::new(cfg).search_with_budget(&base, &eval).expect("the search runs");
        assert!(budget.generations_run <= 5);
        assert!(rows.len() <= budget.max_evaluations);
        assert_eq!(budget.max_evaluations, 20, "4 x 5, under the 216-point grid");
        assert_eq!(rows.len(), budget.evaluated);
    }

    /// A run in which every backtest FAILS still terminates inside its budget rather than looping
    /// on an unrankable population — the `NaN` steering convention reaching the searcher intact.
    #[test]
    fn an_all_unrankable_run_terminates() {
        let base = base_with_sweep(BOWL_SWEEP);
        let eval = StubEvaluator::new(always_unrankable);
        let cfg = GeneticConfig::new(5).with_population(6).with_generations(6);
        let (rows, budget) =
            GeneticSearch::new(cfg).search_with_budget(&base, &eval).expect("the search runs");
        assert!(!rows.is_empty());
        assert!(rows.len() <= budget.max_evaluations);
        assert!(!eval.batches().contains(&0), "the evaluator is never handed an empty batch");
    }

    /// The QUALITY-based early exit, which is the first of its kind in this tree: against a flat
    /// objective the incumbent never improves, so after [`GeneticConfig::STALL_GENERATIONS`] barren
    /// generations the run stops with `converged_at` set — well inside a budget that would
    /// otherwise have allowed 160 backtests.
    #[test]
    fn a_flat_objective_stalls_out() {
        let base = base_with_sweep(
            "[sweep]\n\
             a = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]\n\
             b = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]\n\
             c = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]",
        );
        let eval = StubEvaluator::new(flat);
        let cfg = GeneticConfig::new(9).with_population(8).with_generations(20);
        let (rows, budget) =
            GeneticSearch::new(cfg).search_with_budget(&base, &eval).expect("the search runs");

        assert_eq!(budget.max_evaluations, 160, "8 x 20, under the 512-point grid");
        assert_eq!(
            budget.converged_at,
            Some(GeneticConfig::STALL_GENERATIONS + 1),
            "one generation to set the incumbent, then STALL_GENERATIONS barren ones"
        );
        assert_eq!(budget.generations_run, GeneticConfig::STALL_GENERATIONS + 1);
        assert!(rows.len() < 160, "the early exit must actually save backtests");
        assert!(
            GeneticSearch::new(cfg)
                .search(&base, &StubEvaluator::new(flat))
                .expect("the search runs")
                .summary
                .expect("genetic always reports its budget")
                .contains("converged at generation"),
            "the operator has to be able to see WHY it stopped"
        );
    }

    /// The dedup key is the rendered VALUES' bits, not the index vector: an axis carrying a
    /// duplicated value has fewer POINTS than indices, and the searcher must not pay for the same
    /// backtest twice. The same fixture exercises the diversity exit — with only two distinct
    /// points reachable, generation 1's population is entirely cached and the run stops rather than
    /// handing the evaluator an empty batch.
    #[test]
    fn a_duplicate_valued_axis_is_paid_for_once_and_then_converges() {
        let base = base_with_sweep("[sweep]\nsize = [1.0, 1.0, 2.0]");
        let eval = StubEvaluator::new(by_size);
        let (rows, budget) = GeneticSearch::new(GeneticConfig::new(4))
            .search_with_budget(&base, &eval)
            .expect("the search runs");

        assert_eq!(budget.grid_size, 3, "three array entries is what the GRID would cost");
        assert_eq!(rows.len(), 2, "but only two distinct points exist, so only two are run");
        assert_eq!(budget.converged_at, Some(1), "generation 1 is entirely cached");
        assert!(!eval.batches().contains(&0), "the evaluator is never handed an empty batch");
    }

    // ---------------------------------------------------------------------------------------
    // 4. The trait contract — driven through the same seam the other three are.
    // ---------------------------------------------------------------------------------------

    /// Dispatch behind `Box<dyn Optimizer>` through the ONE door (`optimize::optimize`), and the
    /// division of labour the seam insists on: the searcher returns EVALUATION order, the assembler
    /// ranks. Both are checked here rather than assumed.
    #[test]
    fn it_dispatches_as_a_boxed_optimizer_and_the_assembler_ranks() {
        let base = base_with_sweep(BOWL_SWEEP);
        let eval = StubEvaluator::new(bowl);
        let opt: Box<dyn Optimizer> = Box::new(GeneticSearch::new(GeneticConfig::new(6)));
        assert_eq!(opt.name(), "genetic");

        let out = optimize(opt.as_ref(), &base, &eval).expect("the search runs");
        assert_eq!(out.report.rank_by, RankBy::Objective("stub".to_string()));
        let scores: Vec<f64> = out.report.rows.iter().map(|r| r.score.expect("stub")).collect();
        for pair in scores.windows(2) {
            assert!(pair[0] >= pair[1], "the assembler ranks best-first: {scores:?}");
        }
        assert!(
            out.summary.as_deref().expect("a budget line").starts_with("genetic: "),
            "the summary is this method's already-rendered cost line"
        );
        // The searcher's own rows stay in EVALUATION order, which is a different sequence.
        let unranked = GeneticSearch::new(GeneticConfig::new(6))
            .search(&base, &StubEvaluator::new(bowl))
            .expect("the search runs");
        let raw: Vec<f64> = unranked.rows.iter().map(|r| r.score.expect("stub")).collect();
        assert_ne!(raw, scores, "search returns the trajectory; report_from_outcome sorts it");
    }

    /// The batch is the WHOLE deduplicated population, in ONE call per generation — the widest
    /// batch of any method here, and an algorithmic statement rather than a policy. Also the
    /// property that keeps `PointEvaluator`'s pool rule meaningful: a width-1 batch would never
    /// reach rayon at all.
    #[test]
    fn one_batch_per_generation_carries_the_whole_population() {
        let base = base_with_sweep(BOWL_SWEEP);
        let eval = StubEvaluator::new(bowl);
        let cfg = GeneticConfig::new(8).with_population(12).with_generations(4);
        GeneticSearch::new(cfg).search(&base, &eval).expect("the search runs");
        let batches = eval.batches();
        assert_eq!(batches.len(), 4, "one evaluate call per generation: {batches:?}");
        assert!(batches[0] > 1, "generation 0 is the whole stratified population: {batches:?}");
        assert!(
            batches.iter().all(|&w| (1..=12).contains(&w)),
            "never empty and never wider than the population: {batches:?}"
        );
    }

    /// A space this method cannot search is refused by `accepts`, BEFORE any evaluation — the same
    /// narrowing euler and TPE inherit from the shared reader, and the refusal names the grid.
    #[test]
    fn a_non_numeric_axis_is_refused_before_any_evaluation() {
        let base = base_with_sweep("[sweep]\nmode = [\"fast\", \"slow\"]");
        let eval = StubEvaluator::new(flat);
        let opt = GeneticSearch::new(GeneticConfig::new(0));
        match optimize(&opt, &base, &eval) {
            Err(HarnessError::Validation(m)) => {
                assert!(m.contains("genetic"), "the refusal names the method: {m}");
                assert!(m.contains("grid"), "and the method that CAN search it: {m}");
            }
            other => panic!("expected the numeric narrowing to refuse, got {other:?}"),
        }
        assert!(eval.batches().is_empty(), "nothing may be evaluated after a refusal");
    }

    /// A profile with no `[sweep]` table has nothing to search — refused rather than run as a
    /// single point repeated `population x generations` times.
    #[test]
    fn a_profile_with_no_sweep_table_is_refused() {
        let base = base_with_sweep("");
        let opt = GeneticSearch::new(GeneticConfig::new(0));
        assert!(matches!(opt.accepts(&base), Err(HarnessError::Validation(_))));
    }

    // ---------------------------------------------------------------------------------------
    // 5. The operators, pinned individually.
    // ---------------------------------------------------------------------------------------

    /// Every default, derived from the space's own shape. Pinned so a change to one of them is a
    /// deliberate act with a diff, rather than a number nobody notices moving.
    #[test]
    fn defaults_scale_with_the_space() {
        // `seeding_axes` is the shared builder — this test grew its own copy of it before the
        // seeding tests below needed one, and two identical builders in one module is one too many.
        let plan = |shape: &[usize]| GeneticConfig::new(0).resolve(&seeding_axes(shape));

        // One axis of four: the population floor is the seat count, and the budget is the grid.
        let one = plan(&[4]);
        assert_eq!((one.population, one.generations, one.max_evaluations), (4, 2, 4));
        assert_eq!(one.elite, 1);

        // The 216-point fixture: ceil(sqrt(216)) = 15 individuals, two passes over three genes,
        // 90 of 216 points.
        let three = plan(&[6, 6, 6]);
        assert_eq!((three.population, three.generations, three.max_evaluations), (15, 6, 90));

        // A NARROW but deep space: the widest axis is 2, so the isqrt floor is what sizes it — and
        // this is the shape that needed `MAX_DERIVED_EVALUATIONS`. `population x generations` is
        // 16 x 16 = 256 here, i.e. the WHOLE 256-point grid: without the absolute ceiling the
        // derived defaults spent enumeration's full price for an approximate answer, which is
        // `super::sweep::GridSearch` strictly dominating this method on its own defaults.
        let deep = plan(&[2, 2, 2, 2, 2, 2, 2, 2]);
        assert_eq!((deep.population, deep.generations, deep.grid_size), (16, 16, 256));
        assert_eq!(
            deep.max_evaluations,
            GeneticConfig::MAX_DERIVED_EVALUATIONS,
            "the derived ceiling binds before the grid does on a narrow, deep space"
        );
        assert!(deep.max_evaluations < deep.grid_size, "a method that costs the grid is not one");

        // The other end of the same failure: twelve axes of three is a 531,441-point grid, so
        // `.min(grid_size)` bounds nothing at all and the derived spend was 64 x 24 = 1536
        // backtests — 24x `TpeConfig::DEFAULT_TRIALS`, the very number MAX_POPULATION is anchored
        // on. The ceiling is what stands between an operator and that bill.
        let wide = plan(&[3; 12]);
        assert_eq!((wide.population, wide.generations), (GeneticConfig::MAX_POPULATION, 24));
        assert_eq!(wide.max_evaluations, GeneticConfig::MAX_DERIVED_EVALUATIONS);

        // ⚠ ...and the ceiling caps the DERIVATION only. An operator who names the sizing has made
        // the spending decision, so the same 512-point space sized by hand keeps its own budget.
        let by_hand = GeneticConfig::new(0)
            .with_population(8)
            .with_generations(20)
            .resolve(&seeding_axes(&[2, 2, 2, 2, 2, 2, 2, 2, 2]));
        assert_eq!(
            by_hand.max_evaluations, 160,
            "an explicit sizing is not second-guessed by the derived ceiling"
        );

        // ...nor is an explicit BUDGET, which is the same rule reached through the third knob: 200
        // is above MAX_DERIVED_EVALUATIONS and below the 256-point grid, and the grid is the only
        // ceiling entitled to cut it.
        let asked = 200usize;
        assert!(asked > GeneticConfig::MAX_DERIVED_EVALUATIONS, "or this case proves nothing");
        let asked_for = GeneticConfig::new(0)
            .with_max_evaluations(asked)
            .resolve(&seeding_axes(&[2, 2, 2, 2, 2, 2, 2, 2]));
        assert_eq!(asked_for.max_evaluations, asked, "an explicit budget IS the budget");

        // Every axis single-valued: one point, so one evaluation. No division by anything.
        let degenerate = plan(&[1, 1]);
        assert_eq!(
            (degenerate.population, degenerate.max_evaluations, degenerate.elite),
            (1, 1, 1)
        );

        // ⚠ A grid size that SATURATES `usize` must degrade rather than wrap: the population
        // clamps at MAX_POPULATION and the budget stops being bounded by the grid.
        let huge = plan(&[1000; 7]);
        assert_eq!(huge.grid_size, usize::MAX, "1000^7 overflows; it must saturate");
        assert_eq!(huge.population, GeneticConfig::MAX_POPULATION);
        assert_eq!(
            huge.max_evaluations,
            GeneticConfig::MAX_DERIVED_EVALUATIONS,
            "with the grid cap vacuous, the derived ceiling is the only thing bounding the bill"
        );
    }

    /// Axes of the given widths, values `0.0 ..= k-1`, for the seeding tests below.
    fn seeding_axes(shape: &[usize]) -> Vec<NumericAxis> {
        shape
            .iter()
            .enumerate()
            .map(|(i, &k)| NumericAxis {
                key: format!("k{i}"),
                values: (0..k).map(|v| v as f64).collect(),
                integral: false,
            })
            .collect()
    }

    /// The stratified seeding property the population floor is chosen FOR: at
    /// `population >= |values|`, generation 0 carries every value of every axis at least once.
    #[test]
    fn generation_zero_covers_every_value_of_every_axis() {
        let axes = seeding_axes(&[6, 4, 3]);
        for seed in 0..8u64 {
            for population in 6..14usize {
                let pop = seed_population(&axes, population, seed);
                assert_eq!(pop.len(), population);
                for (ai, axis) in axes.iter().enumerate() {
                    let seen: BTreeSet<usize> = pop.iter().map(|g| g[ai]).collect();
                    assert_eq!(
                        seen.len(),
                        axis.values.len(),
                        "seed {seed}, population {population}: axis {ai} lost a value"
                    );
                }
            }
        }
    }

    /// The other half, and the one the test above structurally COULD NOT see: what generation 0
    /// does when the axis is WIDER than the population.
    ///
    /// ⚠ Full coverage is unavailable there — `population` slots cannot hold `k > population`
    /// distinct values — so the claim is the weaker one the deal can actually keep: the seeded
    /// indices SPAN the axis. Every slot draws a distinct index, and the largest of them lands
    /// inside the axis's final `ceil(k / population)` slice, so no part of the axis is more than one
    /// stride from a seeded point.
    ///
    /// This is the pin for a real defect: the deal was `slot % k`, which for `k > population` is
    /// exactly `{0 .. population - 1}` — the shuffle permutes which slot gets which index and never
    /// which indices exist — so the top `k - population` values of every wide axis were absent from
    /// generation 0 under every seed. The reachable population sizes make it ordinary rather than
    /// exotic: `resolve` clamps at `MAX_POPULATION` = 64, so an authored `period = [2 .. 100]`
    /// seeded 64 of its 99 values and never the top 35. Under the old deal the `max_index`
    /// assertion below reads `population - 1` (63 against an axis of 99) and fails.
    #[test]
    fn generation_zero_spans_an_axis_wider_than_the_population() {
        // The reachable population sizes: both declared bounds plus two in between.
        let sizes = [GeneticConfig::MIN_POPULATION, 7, 13, GeneticConfig::MAX_POPULATION];
        for &k in &[20usize, 99, 200] {
            let axes = seeding_axes(&[k]);
            for seed in 0..8u64 {
                for population in sizes {
                    if population >= k {
                        continue; // the coverage test above owns that half
                    }
                    let pop = seed_population(&axes, population, seed);
                    let seen: BTreeSet<usize> = pop.iter().map(|g| g[0]).collect();
                    let ctx = format!("k={k}, population={population}, seed={seed}");

                    assert_eq!(
                        seen.len(),
                        population,
                        "{ctx}: a wider axis than the population must deal distinct indices"
                    );
                    assert!(*seen.last().expect("non-empty") < k, "{ctx}: dealt off the axis");
                    let stride = k.div_ceil(population);
                    assert!(
                        k - 1 - *seen.last().expect("non-empty") < stride,
                        "{ctx}: the deal stopped {} short of the axis top — it is clustering at \
                         the head, not spanning: {seen:?}",
                        k - 1 - *seen.last().expect("non-empty")
                    );
                    // ...and no interior gap wider than one stride either, so "spans" means an even
                    // net over the axis rather than two clumps at its ends.
                    let idx: Vec<usize> = seen.iter().copied().collect();
                    for pair in idx.windows(2) {
                        assert!(
                            pair[1] - pair[0] <= stride,
                            "{ctx}: gap {} between {} and {} exceeds the stride {stride}",
                            pair[1] - pair[0],
                            pair[0],
                            pair[1]
                        );
                    }
                }
            }
        }
    }

    /// A mutation ALWAYS moves, and never off the axis. A redraw that may land on the value it
    /// already had is a silent no-op that inflates the nominal rate and cannot be measured from
    /// outside the module.
    #[test]
    fn a_mutation_always_moves() {
        for k in 2..12usize {
            for current in 0..k {
                for coord in 0..64u64 {
                    let mut draw = Draw::at(77, PURPOSE_MUTATE, coord, current as u64);
                    for _ in 0..4 {
                        let next = step_to_another(current, k, &mut draw);
                        assert_ne!(next, current, "k={k}: a mutation must change the gene");
                        assert!(next < k, "k={k}: a mutation must stay on the axis");
                    }
                }
            }
        }
    }

    /// Every index below `n` is reachable, and none above it. `below` is the one place the raw
    /// stream becomes a decision, so a bias that excluded the last index would quietly make a whole
    /// axis value unreachable.
    #[test]
    fn a_bounded_draw_covers_its_whole_range() {
        for n in 1..10usize {
            let mut hit: BTreeSet<usize> = BTreeSet::new();
            for coord in 0..4096u64 {
                let mut draw = Draw::at(1, PURPOSE_CROSSOVER, coord, n as u64);
                let i = draw.below(n);
                assert!(i < n, "below({n}) returned {i}");
                hit.insert(i);
            }
            assert_eq!(hit.len(), n, "below({n}) never reached every index: {hit:?}");
        }
    }

    /// A `NaN` — a failed backtest — never DISPLACES a finite score in a tournament. Measured by
    /// running the same draw coordinates twice: once over an all-unrankable population (where the
    /// winner is whatever was drawn first, since nothing can displace it) and once with a single
    /// finite score planted. The winner may only be the planted index or the unchanged one; a third
    /// answer would mean an unrankable individual had won a comparison.
    #[test]
    fn a_nan_never_displaces_a_finite_score_in_a_tournament() {
        let all_nan = [f64::NAN; 4];
        let mut planted = all_nan;
        planted[2] = 1.0;
        let mut planted_won = 0usize;
        for coord in 0..512u64 {
            let mut a = Draw::at(2, PURPOSE_PARENT_A, coord, 0);
            let mut b = Draw::at(2, PURPOSE_PARENT_A, coord, 0);
            let unrankable_winner = tournament(&all_nan, GeneticConfig::TOURNAMENT, &mut a);
            let planted_winner = tournament(&planted, GeneticConfig::TOURNAMENT, &mut b);
            assert!(
                planted_winner == 2 || planted_winner == unrankable_winner,
                "coord {coord}: an unrankable individual won over a finite one"
            );
            if planted_winner == 2 {
                planted_won += 1;
            }
        }
        assert!(planted_won > 0, "the planted finite score never won — the test is vacuous");
    }

    /// The row the report puts first IS the best point the run evaluated — the searcher's own
    /// incumbent, `sort_scored_rows` and the summary line cannot disagree, because all three read
    /// [`super::sweep::cmp_scores_desc`] and none of them re-implements "better".
    #[test]
    fn the_reported_best_is_the_best_point_evaluated() {
        let base = base_with_sweep(BOWL_SWEEP);
        for seed in 0..8u64 {
            let eval = StubEvaluator::new(bowl);
            let out = GeneticSearch::new(GeneticConfig::new(seed))
                .search(&base, &eval)
                .expect("the search runs");
            let mut best = f64::NEG_INFINITY;
            for row in &out.rows {
                best = best.max(row.score.expect("stub"));
            }
            let mut ranked = out.rows.clone();
            sort_scored_rows(&mut ranked);
            assert_eq!(ranked[0].score.expect("stub"), best, "seed {seed}");
        }
    }

    /// The reported budget describes the run it came from — `evaluated` is the row count, the
    /// dedup saving is visible, and nothing claims a saving against a grid it never approached.
    #[test]
    fn the_budget_line_describes_the_run() {
        let base = base_with_sweep(BOWL_SWEEP);
        let eval = StubEvaluator::new(bowl);
        let (rows, budget) = GeneticSearch::new(GeneticConfig::new(12))
            .search_with_budget(&base, &eval)
            .expect("the search runs");
        assert_eq!(budget.evaluated, rows.len());
        assert!(budget.proposed >= budget.evaluated, "dedup can only ever remove work");
        assert_eq!(budget.grid_size, 216);
        assert!(budget.evaluated <= budget.grid_size, "never more than enumeration would cost");
        let line = budget.to_string();
        assert!(line.starts_with("genetic: "), "{line}");
        assert!(line.contains("the full grid is 216"), "{line}");
        assert!(line.contains("seed 12"), "{line}");
    }
}
