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
    let axes = [SearchAxis::new("a", vec![1.0, 2.0]), SearchAxis::new("b", vec![10.0, 20.0, 30.0])];
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
    let pts =
        |t: &SearchTrace| -> Vec<Vec<f64>> { t.evaluated.iter().map(|(p, _)| p.clone()).collect() };
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
        if (p[0] - 0.5).abs() < 1e-12 { 1.0 } else { f64::NAN }
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
    assert_eq!(trace.depth_reached, 0, "a barren halving buys no resolution, so it is not counted");
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
        if narrow > wide { narrow } else { wide }
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
