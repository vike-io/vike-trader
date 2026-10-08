use super::*;
use crate::harness::optimize::optimize;
use crate::harness::sweep::sweep_tests::base_with_paramscan;
use crate::harness::sweep::{RankBy, expand_paramscan_overrides};
use std::sync::Mutex;

// ---------------------------------------------------------------------------------------
// Fixtures. Deliberately STORE-FREE and backtest-free: the searcher's whole contract with the
// world is `PointEvaluator`, so every test below runs in the default trait-only build as
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
                    row: ParamscanRow {
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

/// Three float axes of six values each: 216 grid points, small enough for
/// [`expand_paramscan_overrides`] to enumerate as the TRUTH and large enough that the derived
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
    let grid = expand_paramscan_overrides(base).expect("the fixture space expands");
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

fn candidates(rows: &[ParamscanRow]) -> Vec<Candidate> {
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(BOWL_SWEEP);
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

/// The real correctness test: on a space small enough for [`expand_paramscan_overrides`] to
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

    let base = base_with_paramscan(SMALL_SWEEP);
    let truth = grid_optimum(&base);
    let grid = expand_paramscan_overrides(&base).expect("the fixture space expands");

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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(BOWL_SWEEP);
    let grid: BTreeSet<String> = expand_paramscan_overrides(&base)
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
    let base = base_with_paramscan("[sweep]\nn = [1, 5, 9, 13]");
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(
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
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 1.0, 2.0]");
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan("[sweep]\nmode = [\"fast\", \"slow\"]");
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
    let base = base_with_paramscan("");
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
    assert_eq!((degenerate.population, degenerate.max_evaluations, degenerate.elite), (1, 1, 1));

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
    let base = base_with_paramscan(BOWL_SWEEP);
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
    let base = base_with_paramscan(BOWL_SWEEP);
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
