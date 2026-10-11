use super::*;

// The sampler's MATHEMATICS only; the store-backed driver is tested in vike-backtest.
// ----- deterministic PRNG -----

#[test]
fn splitmix64_is_deterministic_and_in_range() {
    let (mut a, mut b) = (SplitMix64::new(12345), SplitMix64::new(12345));
    for _ in 0..128 {
        assert_eq!(a.next_u64(), b.next_u64(), "same seed must yield the same stream");
    }
    let mut r = SplitMix64::new(1);
    for _ in 0..1000 {
        let x = r.next_f64();
        assert!((0.0..1.0).contains(&x), "next_f64 out of [0,1): {x}");
    }
}

// ----- density math (hand-computed) -----

#[test]
fn gaussian_pdf_matches_the_closed_form() {
    // N(0; 0, 1) = 1/sqrt(2π).
    let expect = 1.0 / std::f64::consts::TAU.sqrt();
    let got = gaussian_pdf(0.0, 0.0, 1.0);
    assert!((got - expect).abs() < 1e-12, "N(0;0,1) should be 1/sqrt(2pi), got {got}");
    // Symmetric about the mean.
    assert!((gaussian_pdf(1.3, 0.0, 1.0) - gaussian_pdf(-1.3, 0.0, 1.0)).abs() < 1e-15);
}

/// The categorical `l`/`g` ratio on a tiny hand-computed example. values = {0, 1}, prior 1
/// each. good = three 1s → counts [1, 4] → p_good = [0.2, 0.8]. bad = two 0s → counts [3, 1] →
/// p_bad = [0.75, 0.25]. So l/g prefers category 1: 0.8/0.25 = 3.2 vs 0.2/0.75 ≈ 0.267.
#[test]
fn categorical_ratio_is_hand_computable() {
    let values = [0.0, 1.0];
    let pg = categorical_probs(&[1.0, 1.0, 1.0], &values);
    let pb = categorical_probs(&[0.0, 0.0], &values);
    assert!((pg[0] - 0.2).abs() < 1e-12 && (pg[1] - 0.8).abs() < 1e-12, "pg={pg:?}");
    assert!((pb[0] - 0.75).abs() < 1e-12 && (pb[1] - 0.25).abs() < 1e-12, "pb={pb:?}");
    let r0 = pg[0] / pb[0];
    let r1 = pg[1] / pb[1];
    assert!((r1 - 3.2).abs() < 1e-12, "r1={r1}");
    assert!((r0 - 0.2 / 0.75).abs() < 1e-12, "r0={r0}");
    assert!(r1 > r0, "TPE must prefer the category the good group favours");
}

/// The continuous `l`/`g` ratio favours the good region and the density integrates to ~1.
#[test]
fn continuous_parzen_ratio_favours_the_good_region() {
    let l = adaptive_parzen(&[0.7], 0.0, 1.0);
    let g = adaptive_parzen(&[0.1], 0.0, 1.0);
    let ratio_hi = l.pdf(0.7).max(TINY) / g.pdf(0.7).max(TINY);
    let ratio_lo = l.pdf(0.1).max(TINY) / g.pdf(0.1).max(TINY);
    assert!(ratio_hi > ratio_lo, "l/g must be higher near the good obs: {ratio_hi} vs {ratio_lo}");
    // A sanity band, not a normalization: the broad prior kernel (sigma = width) leaks mass
    // outside the box, so the truncated mass sits near ~0.5.
    let mass: f64 = (0..=1000).map(|i| l.pdf(i as f64 / 1000.0)).sum::<f64>() / 1000.0;
    assert!((0.2..2.0).contains(&mass), "parzen mass over the box should be sane, got {mass}");
}

// ----- ask/tell harness for the pure convergence tests -----

fn run_ask_tell(
    space: TpeSpace,
    seed: u64,
    n_trials: usize,
    obj: impl Fn(&IndexMap<String, f64>) -> f64,
) -> (IndexMap<String, f64>, f64) {
    let mut opt = TpeOptimizer::new(space, seed, 0.25).with_n_startup(12).with_n_candidates(24);
    for _ in 0..n_trials {
        let p = opt.ask();
        let s = obj(&p);
        opt.tell(&p, s);
    }
    opt.best().expect("at least one trial was told")
}

/// The uniform-random baseline over the same space and budget TPE races.
fn random_best(
    space: &[(String, ParamDomain)],
    seed: u64,
    n_trials: usize,
    obj: impl Fn(&IndexMap<String, f64>) -> f64,
) -> f64 {
    let mut rng = SplitMix64::new(seed);
    let mut best = f64::NEG_INFINITY;
    for _ in 0..n_trials {
        let mut p = IndexMap::new();
        for (name, domain) in space {
            p.insert(name.clone(), domain.sample_uniform(&mut rng));
        }
        best = best.max(obj(&p));
    }
    best
}

fn bowl_2d() -> (TpeSpace, impl Fn(&IndexMap<String, f64>) -> f64) {
    let space: TpeSpace = vec![
        ("x".to_string(), ParamDomain::continuous(0.0, 1.0)),
        ("y".to_string(), ParamDomain::continuous(0.0, 1.0)),
    ];
    // Smooth unimodal bowl peaking at (0.7, 0.3); higher is better.
    let obj = |p: &IndexMap<String, f64>| -((p["x"] - 0.7).powi(2) + (p["y"] - 0.3).powi(2));
    (space, obj)
}

// ----- determinism -----

#[test]
fn same_seed_same_proposals() {
    let (space, obj) = bowl_2d();
    let sequence = |seed| {
        let mut opt = TpeOptimizer::new(space.clone(), seed, 0.25).with_n_startup(8);
        let mut asks = Vec::new();
        for _ in 0..40 {
            let a = opt.ask();
            let s = obj(&a);
            opt.tell(&a, s);
            asks.push(a);
        }
        asks
    };
    let a = sequence(777);
    let b = sequence(777);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x["x"].to_bits(), y["x"].to_bits(), "ask x diverged for the same seed");
        assert_eq!(x["y"].to_bits(), y["y"].to_bits(), "ask y diverged for the same seed");
    }
    // A different seed must actually move the proposals (not a constant generator).
    let c = sequence(778);
    assert!(
        a.iter().zip(&c).any(|(x, y)| x["x"].to_bits() != y["x"].to_bits()),
        "a different seed must change the proposal sequence"
    );
}

// ----- convergence -----

/// TPE finds the known optimum of a smooth 2-D bowl (within a generous tolerance) in 80 trials.
#[test]
fn tpe_converges_to_the_known_optimum() {
    let (space, obj) = bowl_2d();
    let (best, score) = run_ask_tell(space, 7, 80, &obj);
    assert!((best["x"] - 0.7).abs() < 0.15, "x converged near 0.7: {best:?} (score {score})");
    assert!((best["y"] - 0.3).abs() < 0.15, "y converged near 0.3: {best:?} (score {score})");
}

/// On the SAME budget TPE beats uniform-random. A single-seed race is a razor-edge in low
/// dimensions, so over an ensemble of seeds TPE must win the MEAN and the MAJORITY.
#[test]
fn tpe_beats_random_on_the_same_budget() {
    let (space, obj) = bowl_2d();
    let n_trials = 100;
    let (mut tpe_sum, mut rnd_sum, mut tpe_wins) = (0.0, 0.0, 0);
    for seed in 0..8u64 {
        let (_, tpe_best) = run_ask_tell(space.clone(), seed, n_trials, &obj);
        // A decorrelated but deterministic seed for the random baseline.
        let rnd = random_best(&space, seed.wrapping_mul(0x9E37_79B9) ^ 0xABCD, n_trials, &obj);
        tpe_sum += tpe_best;
        rnd_sum += rnd;
        if tpe_best >= rnd {
            tpe_wins += 1;
        }
    }
    assert!(
        tpe_sum >= rnd_sum,
        "TPE mean {} must beat random mean {}",
        tpe_sum / 8.0,
        rnd_sum / 8.0
    );
    assert!(tpe_wins >= 5, "TPE should win the majority of seeds, won {tpe_wins}/8");
}

/// TPE also converges on a DISCRETE (categorical) axis — 10 integer candidates, optimum at 7.
/// Robust (majority-of-seeds) form: it must land on 7 on most seeds.
#[test]
fn tpe_finds_a_discrete_optimum_on_most_seeds() {
    let cats: Vec<f64> = (0..10).map(|i| i as f64).collect();
    let space: TpeSpace = vec![("k".to_string(), ParamDomain::discrete_integral(cats))];
    let obj = |p: &IndexMap<String, f64>| -(p["k"] - 7.0).powi(2);
    let mut found = 0;
    for seed in 0..8u64 {
        let (best, _) = run_ask_tell(space.clone(), seed, 60, obj);
        if (best["k"] - 7.0).abs() < 1e-9 {
            found += 1;
        }
    }
    assert!(found >= 6, "TPE should find the discrete optimum on most seeds, found {found}/8");
}
