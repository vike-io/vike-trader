use super::*;

fn bar(ts: i64, close: f64, volume: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: close,
        high: close,
        low: close,
        close,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn window_stats_measures_sigma_and_mean_volume() {
    // closes 100, 110, 121 -> returns 0.1, 0.1 -> sigma 0 (constant growth), avg vol 200
    let bars: Vec<Bar> = vec![bar(0, 100.0, 100.0), bar(1, 110.0, 200.0), bar(2, 121.0, 300.0)];
    let s = window_stats(&bars, 10).unwrap();
    assert!(s.sigma.abs() < 1e-15, "constant returns => zero dispersion, got {}", s.sigma);
    assert!((s.average_volume - 200.0).abs() < 1e-12);
}

#[test]
fn window_stats_honors_the_window_and_needs_three_bars() {
    let bars: Vec<Bar> = (0..10).map(|i| bar(i, 100.0 + i as f64, 10.0 * (i + 1) as f64)).collect();
    // last 3 bars: volumes 80, 90, 100 -> mean 90
    let s = window_stats(&bars, 3).unwrap();
    assert!((s.average_volume - 90.0).abs() < 1e-12);
    assert!(s.sigma > 0.0);
    // whole series mean volume 55
    let all = window_stats(&bars, 100).unwrap();
    assert!((all.average_volume - 55.0).abs() < 1e-12);
    assert!(window_stats(&bars[..2], 10).is_none(), "two bars = one return, not enough");
    assert!(window_stats(&bars, 0).is_none());
}

#[test]
fn window_stats_rejects_a_degenerate_window() {
    let bad: Vec<Bar> = vec![bar(0, 100.0, 10.0), bar(1, 0.0, 10.0), bar(2, 100.0, 10.0)];
    assert!(window_stats(&bad, 10).is_none(), "a zero close has no meaningful return");
    let novol: Vec<Bar> = vec![bar(0, 100.0, 0.0), bar(1, 101.0, 0.0), bar(2, 102.0, 0.0)];
    assert!(window_stats(&novol, 10).is_none(), "zero volume cannot price impact");
}

/// The bar a `ts` falls INSIDE is left out, whether `ts` is its open or strictly inside it.
#[test]
fn window_stats_before_reads_only_the_bars_that_closed_before() {
    let bars: Vec<Bar> =
        (0..6).map(|i| bar(i * 10, 100.0 + i as f64, 10.0 * (i + 1) as f64)).collect();
    let closed = window_stats(&bars[..4], 3);
    assert_eq!(window_stats_before(&bars, 40, 3), closed, "ts at bar 4's open");
    assert_eq!(window_stats_before(&bars, 45, 3), closed, "ts inside bar 4");
    assert_eq!(window_stats_before(&bars, 20, 3), None, "only bars 0 and 1 closed before");
}

const CONTEXT: ImpactModelMarketStatistics =
    ImpactModelMarketStatistics { sigma: 0.02, average_volume: 1_000.0 };

/// Hand-computed: qty 10 of a 1000 mean volume at sigma 2% and unit horizon, so
/// `nu = 0.01` and both terms read off it.
#[test]
fn unit_impact_terms_match_a_hand_computed_value() {
    let (p, t) = unit_impact_terms(10.0, &CONTEXT, 1.0).unwrap();
    assert_eq!(p, 0.02 * libm::pow(0.01, 0.891));
    assert_eq!(t, 0.02 * libm::pow(0.01, 0.600));
    // At a horizon of 2 the temporary term sees half the rate and the permanent one does not move.
    let (p2, t2) = unit_impact_terms(10.0, &CONTEXT, 2.0).unwrap();
    assert!((p2 - p).abs() < 1e-18, "{p2} vs {p}");
    assert_eq!(t2, 0.02 * libm::pow(0.005, 0.600));
    assert_eq!(unit_impact_terms(0.0, &CONTEXT, 1.0), None);
    assert_eq!(unit_impact_terms(10.0, &CONTEXT, 0.0), None);
    let flat = ImpactModelMarketStatistics { sigma: 0.0, ..CONTEXT };
    assert_eq!(unit_impact_terms(10.0, &flat, 1.0), None);
}

/// A fill whose prices are exactly the model's at `(base, gamma, eta)`.
fn modelled(
    side: i32,
    quantity: f64,
    base: f64,
    gamma: f64,
    eta: f64,
    execution_time: f64,
) -> ImpactFill {
    let (p, t) = unit_impact_terms(quantity, &CONTEXT, execution_time).unwrap();
    let s = f64::from(side);
    let arrival_mid = 100.0;
    ImpactFill {
        side,
        quantity,
        price: arrival_mid * (1.0 + s * (base + gamma * p + eta * t)),
        arrival_mid,
        after_mid: arrival_mid * (1.0 + s * gamma * p),
        context: CONTEXT,
    }
}

/// Fills priced by the model are fitted back to its coefficients, on both sides.
#[test]
fn calibrate_impact_recovers_the_coefficients_that_priced_the_fills() {
    let (base, gamma, eta, execution_time) = (0.0002, 0.5, 0.25, 2.0);
    let fills: Vec<ImpactFill> = [(1, 5.0), (-1, 20.0), (1, 80.0), (-1, 160.0), (1, 400.0)]
        .iter()
        .map(|&(side, quantity)| modelled(side, quantity, base, gamma, eta, execution_time))
        .collect();
    let fit = calibrate_impact(&fills, execution_time).unwrap();
    assert_eq!(fit.fills, 5);
    let close = |got: f64, want: f64| (got - want).abs() <= 1e-9 * want.abs();
    assert!(close(fit.gamma, gamma), "gamma {}", fit.gamma);
    assert!(close(fit.eta, eta), "eta {}", fit.eta);
    assert!((fit.base - base).abs() < 1e-12, "base {}", fit.base);
}

/// Hand-computed least squares on two fills with no permanent move: `gamma` is 0, and `eta` and
/// `base` are the line through the two (temporary term, cost) points.
#[test]
fn calibrate_impact_is_the_line_through_two_points() {
    let fill = |quantity: f64, cost: f64| ImpactFill {
        side: 1,
        quantity,
        price: 100.0 * (1.0 + cost),
        arrival_mid: 100.0,
        after_mid: 100.0,
        context: CONTEXT,
    };
    let (t1, t2) = (
        unit_impact_terms(10.0, &CONTEXT, 1.0).unwrap().1,
        unit_impact_terms(40.0, &CONTEXT, 1.0).unwrap().1,
    );
    let fit = calibrate_impact(&[fill(10.0, 0.001), fill(40.0, 0.003)], 1.0).unwrap();
    assert_eq!(fit.gamma, 0.0);
    let (c1, c2) = ((100.0 * 1.001 - 100.0) / 100.0, (100.0 * 1.003 - 100.0) / 100.0);
    let eta = (c2 - c1) / (t2 - t1);
    assert!((fit.eta - eta).abs() <= 1e-9 * eta, "eta {} vs {eta}", fit.eta);
    assert!((fit.base - (c1 - eta * t1)).abs() < 1e-12, "base {}", fit.base);
}

/// Nothing to identify: one fill, fills of one size in one context, a side that is not `±1`.
#[test]
fn calibrate_impact_refuses_what_it_cannot_identify() {
    let one = modelled(1, 10.0, 0.0, 0.3, 0.1, 1.0);
    assert_eq!(calibrate_impact(&[one], 1.0), None);
    assert_eq!(calibrate_impact(&[one, one], 1.0), None, "the temporary term does not vary");
    let sideless = ImpactFill { side: 0, ..modelled(1, 40.0, 0.0, 0.3, 0.1, 1.0) };
    assert_eq!(calibrate_impact(&[one, sideless], 1.0), None, "a side of 0 is skipped");
}

// --- the impact model, moved with it from `crates/vike-simulator/src/impact/tests.rs` (2026-11-17) ---

/// Hand-computed against the published constants. nu = 1000 / (1.0 * 100_000) = 0.01, and at
/// the unit horizon nu == X/V, so both terms read off the same number.
///   permanent = 0.314 * 0.02 * 0.01^0.891
///   temporary = 0.142 * 0.02 * 0.01^0.600
///
/// The expectations in this module spell `libm::pow`, matching `impact_fraction`. They assert at
/// `1e-15` ABSOLUTE on quantities of order `1e-3`, i.e. ~1e-12 relative, so a last-bit
/// disagreement between `libm` and a platform `powf` would never have reddened them — this is
/// hygiene, not a fix. It buys two things: the comparison becomes exact rather than merely
/// tolerant, and no reader can copy a `.powf` out of a test in this file into production.
#[test]
fn almgren_chriss_matches_a_hand_computed_value() {
    let m = ImpactModelAlmgrenChriss::default();
    let input = ImpactInputs { qty: 1000.0, average_volume: 100_000.0, sigma: 0.02 };
    let nu: f64 = 0.01;
    let expect = 0.314 * 0.02 * libm::pow(nu * 1.0, 0.891) + 0.142 * 0.02 * libm::pow(nu, 0.600);
    assert!(
        (m.impact_fraction(&input) - expect).abs() < 1e-15,
        "got {}",
        m.impact_fraction(&input)
    );
    // Sanity on the magnitude: 1% of daily volume at 2% daily vol costs a few bp.
    assert!(expect > 1e-4 && expect < 1e-2, "{expect}");
}

/// A second hand-computed point at a NON-UNIT horizon, written in the PAPER's own variables
/// (`X/V` for permanent, `nu = X/(V*T)` for temporary) rather than in the code's algebra — so
/// it catches a permanent term that has picked up a spurious `T` factor. This is the exact
/// deviation an earlier revision carried: `gamma*sigma*T*nu^alpha`, which equals the paper
/// only at `T = 1` and inflates by `T^(1-alpha)` elsewhere.
#[test]
fn execution_time_scales_only_the_temporary_term() {
    let t: f64 = 2.0;
    let m = ImpactModelAlmgrenChriss::with_execution_time(t);
    let (qty, vol, sigma) = (4000.0f64, 100_000.0f64, 0.03f64);
    let input = ImpactInputs { qty, average_volume: vol, sigma };
    let fraction_of_volume = qty / vol; // X/V = 0.04 — the permanent term's argument
    let nu = qty / t / vol; // 0.02 — the participation RATE
    let expect =
        0.314 * sigma * libm::pow(fraction_of_volume, 0.891) + 0.142 * sigma * libm::pow(nu, 0.600);
    assert!((m.impact_fraction(&input) - expect).abs() < 1e-15);
}

/// The horizon-independence of the permanent term, stated directly: hold `qty`/`average_volume`
/// fixed, vary `exec_time`, and the ONLY thing that may move is the temporary term. Pinned by
/// reconstructing the permanent term once and subtracting it at each horizon.
#[test]
fn permanent_impact_does_not_move_with_the_horizon() {
    let (qty, vol, sigma) = (10_000.0f64, 200_000.0f64, 0.025f64);
    let permanent = AC_GAMMA * sigma * libm::pow(qty / vol, AC_ALPHA);
    let mut previous_temporary = f64::INFINITY;
    for t in [0.25f64, 1.0, 4.0, 10.0] {
        let m = ImpactModelAlmgrenChriss::with_execution_time(t);
        let total = m.impact_fraction(&ImpactInputs { qty, average_volume: vol, sigma });
        let temporary = total - permanent;
        let expect_temporary = AC_ETA * sigma * libm::pow(qty / t / vol, AC_BETA);
        assert!(
            (temporary - expect_temporary).abs() < 1e-15,
            "permanent term drifted at exec_time={t}: residual {temporary} != {expect_temporary}"
        );
        assert!(temporary < previous_temporary, "a slower horizon must cost less temporarily");
        previous_temporary = temporary;
    }
}

#[test]
fn impact_is_strictly_monotone_in_size() {
    let m = ImpactModelAlmgrenChriss::default();
    let f = |qty| m.impact_fraction(&ImpactInputs { qty, average_volume: 50_000.0, sigma: 0.02 });
    let mut previous = f(1.0);
    for qty in [10.0, 100.0, 500.0, 1_000.0, 5_000.0, 25_000.0, 100_000.0] {
        let current = f(qty);
        assert!(current > previous, "not increasing at qty={qty}: {current} <= {previous}");
        previous = current;
    }
}

/// Concavity: doubling size less than doubles the cost (both exponents are < 1). This is the
/// economically load-bearing shape — a linear model would make large orders uninvestable.
#[test]
fn impact_is_concave_in_size() {
    let m = ImpactModelAlmgrenChriss::default();
    let f = |qty| m.impact_fraction(&ImpactInputs { qty, average_volume: 50_000.0, sigma: 0.02 });
    for qty in [100.0, 1_000.0, 10_000.0] {
        assert!(f(2.0 * qty) < 2.0 * f(qty), "not concave at {qty}");
    }
}

#[test]
fn impact_scales_linearly_in_sigma_and_falls_with_liquidity() {
    let m = ImpactModelAlmgrenChriss::default();
    let a = m.impact_fraction(&ImpactInputs { qty: 500.0, average_volume: 50_000.0, sigma: 0.01 });
    let b = m.impact_fraction(&ImpactInputs { qty: 500.0, average_volume: 50_000.0, sigma: 0.02 });
    assert!((b - 2.0 * a).abs() < 1e-15, "sigma must enter linearly");
    let deep =
        m.impact_fraction(&ImpactInputs { qty: 500.0, average_volume: 500_000.0, sigma: 0.01 });
    assert!(deep < a, "a deeper book must cost less");
}

#[test]
fn degenerate_inputs_charge_nothing_rather_than_nan() {
    let m = ImpactModelAlmgrenChriss::default();
    for input in [
        ImpactInputs { qty: 0.0, average_volume: 1000.0, sigma: 0.02 },
        ImpactInputs { qty: -5.0, average_volume: 1000.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, average_volume: 0.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, average_volume: 1000.0, sigma: 0.0 },
        ImpactInputs { qty: 100.0, average_volume: 1000.0, sigma: f64::NAN },
        ImpactInputs { qty: f64::INFINITY, average_volume: 1000.0, sigma: 0.02 },
    ] {
        assert_eq!(m.impact_fraction(&input), 0.0, "{input:?}");
    }
    assert_eq!(
        ImpactModelAlmgrenChriss::with_execution_time(0.0).impact_fraction(&ImpactInputs {
            qty: 100.0,
            average_volume: 1000.0,
            sigma: 0.02
        }),
        0.0
    );
}

/// **The fit inverts THIS model**: fills priced by `ImpactModelAlmgrenChriss` at a known
/// `(gamma, eta)` plus a flat `base` (the permanent mid move from `PermanentOnly`, the fill from
/// `Both`), handed to `vike_fills::market_impact::calibrate_impact`, come back as that triple. This
/// is what stops the calibration's regressors from drifting off the engine's formula.
#[test]
fn the_calibration_recovers_the_coefficients_this_model_charged() {
    let (base, gamma, eta, exec_time) = (0.0003, 0.9, 0.05, 3.0);
    let model = ImpactModelAlmgrenChriss::with_coefficients(gamma, eta, exec_time);
    let contexts = [(0.01, 500.0), (0.03, 2_000.0), (0.02, 900.0)];
    let mut fills = Vec::new();
    for (i, qty) in [3.0, 40.0, 120.0, 700.0, 15.0, 260.0].into_iter().enumerate() {
        let (sigma, average_volume) = contexts[i % contexts.len()];
        let side = if i % 2 == 0 { 1 } else { -1 };
        let s = f64::from(side);
        let input = ImpactInputs { qty, average_volume, sigma };
        let arrival_mid = 250.0;
        fills.push(ImpactFill {
            side,
            quantity: qty,
            price: arrival_mid * (1.0 + s * (base + model.impact_fraction(&input))),
            arrival_mid,
            after_mid: arrival_mid
                * (1.0 + s * model.impact_fraction_for(&input, ImpactTerms::PermanentOnly)),
            context: ImpactModelMarketStatistics { sigma, average_volume },
        });
    }
    let fit = calibrate_impact(&fills, exec_time).expect("identifiable");
    assert_eq!(fit.fills, fills.len());
    assert!((fit.gamma - gamma).abs() <= 1e-9 * gamma, "gamma {}", fit.gamma);
    assert!((fit.eta - eta).abs() <= 1e-9 * eta, "eta {}", fit.eta);
    assert!((fit.base - base).abs() < 1e-12, "base {}", fit.base);
}

// --- the lane split: which TERMS a fill law has not already paid ---------------------------

/// `PermanentOnly` is EXACTLY the permanent term — reconstructed from the published constants
/// rather than by subtracting, so a bug that moved a factor from one term to the other cannot
/// hide behind an identity that holds by construction.
#[test]
fn permanent_only_is_exactly_the_permanent_term() {
    for t in [0.5f64, 1.0, 7.0] {
        let m = ImpactModelAlmgrenChriss::with_execution_time(t);
        let input = ImpactInputs { qty: 3_000.0, average_volume: 80_000.0, sigma: 0.018 };
        let want = AC_GAMMA * input.sigma * libm::pow(input.qty / input.average_volume, AC_ALPHA);
        let got = m.impact_fraction_for(&input, ImpactTerms::PermanentOnly);
        assert!((got - want).abs() < 1e-15, "exec_time={t}: {got} vs {want}");
    }
}

/// The double-count claim, as arithmetic: a lane that already walked a real book pays strictly
/// LESS here than one that priced its fill off a single quote — and the gap it does not pay is
/// precisely the temporary term, the thing the walk supplied.
#[test]
fn a_walked_book_is_charged_strictly_less_than_a_top_of_book_quote() {
    let m = ImpactModelAlmgrenChriss::default();
    let input = ImpactInputs { qty: 2_500.0, average_volume: 60_000.0, sigma: 0.02 };
    let both = m.impact_fraction_for(&input, ImpactTerms::Both);
    let permanent = m.impact_fraction_for(&input, ImpactTerms::PermanentOnly);
    assert!(permanent > 0.0, "the footprint a replayed book cannot show is still a real cost");
    assert!(permanent < both, "{permanent} is not strictly less than {both}");
    let nu = input.qty / input.average_volume;
    let temporary = AC_ETA * input.sigma * libm::pow(nu, AC_BETA);
    assert!((both - permanent - temporary).abs() < 1e-15, "the gap is not the temporary term");
}

/// `gamma` is the L2 lane's ONLY lever, and this is that claim as arithmetic rather than as
/// prose. Two pins, and the second is the one that matters: a `PermanentOnly` charge scales
/// LINEARLY with `gamma`, and it does not move AT ALL with `exec_time` — so an operator told
/// to answer a unit mismatch with the horizon knob would be turning a dial wired to nothing.
///
/// This is the positive twin of `permanent_impact_does_not_move_with_the_horizon` above:
/// that one proves the inertness inside the whole model, this one proves it survives the
/// `PermanentOnly` selection the L2 lane actually asks for, AND that something else works.
#[test]
fn gamma_moves_a_permanent_only_charge_and_the_horizon_does_not() {
    let input = ImpactInputs { qty: 3_000.0, average_volume: 80_000.0, sigma: 0.018 };
    let published =
        ImpactModelAlmgrenChriss::default().impact_fraction_for(&input, ImpactTerms::PermanentOnly);
    assert!(published > 0.0, "the fixture must produce a real charge");

    // The horizon knob: inert on this selection, at every horizon. Compared at `1e-15`
    // ABSOLUTE on a quantity of order `1e-4` (~1e-11 relative) rather than bit-for-bit,
    // because the code reconstructs `X/V` as `(qty / exec_time / average_volume) * exec_time`, and
    // that round trip is exact in the paper's algebra but not in binary. The tolerance is
    // still orders tighter than the thing it watches for: an `exec_time` that had leaked into
    // the permanent term would move this by a FACTOR, not by a last bit.
    for t in [0.1f64, 1.0, 1_000.0] {
        let got = ImpactModelAlmgrenChriss::with_execution_time(t)
            .impact_fraction_for(&input, ImpactTerms::PermanentOnly);
        assert!(
            (got - published).abs() < 1e-15,
            "exec_time={t} moved a PermanentOnly charge ({got} vs {published}) — the L2 \
                 lane's knob is not the horizon"
        );
    }
    // the coefficient knob: strictly linear, and it reaches this selection
    for scale in [0.02f64, 0.5, 3.0] {
        let m = ImpactModelAlmgrenChriss::with_coefficients(AC_GAMMA * scale, AC_ETA, 1.0);
        let got = m.impact_fraction_for(&input, ImpactTerms::PermanentOnly);
        assert!(
            (got - published * scale).abs() < 1e-15,
            "gamma must scale a PermanentOnly charge linearly: {got} != {published} * {scale}"
        );
    }
}

/// ...and `eta` reaches the OTHER half, leaving the permanent term where it was — so the two
/// coefficients are independent levers rather than one knob spelt twice.
#[test]
fn eta_moves_only_the_temporary_half() {
    let input = ImpactInputs { qty: 3_000.0, average_volume: 80_000.0, sigma: 0.018 };
    let base = ImpactModelAlmgrenChriss::default();
    let permanent = base.impact_fraction_for(&input, ImpactTerms::PermanentOnly);
    let m = ImpactModelAlmgrenChriss::with_coefficients(AC_GAMMA, AC_ETA * 4.0, 1.0);
    assert_eq!(
        m.impact_fraction_for(&input, ImpactTerms::PermanentOnly).to_bits(),
        permanent.to_bits(),
        "eta leaked into the permanent term"
    );
    let want_temporary = 4.0 * (base.impact_fraction_for(&input, ImpactTerms::Both) - permanent);
    let got_temporary = m.impact_fraction_for(&input, ImpactTerms::Both) - permanent;
    assert!(
        (got_temporary - want_temporary).abs() < 1e-15,
        "eta must scale the temporary term linearly: {got_temporary} != {want_temporary}"
    );
}

/// The trait's monotonicity contract binds EACH `terms` value on its own — the L2 lane must
/// not be the one place where a bigger order can cost less.
#[test]
fn each_terms_selection_is_monotone_and_concave_in_size() {
    let m = ImpactModelAlmgrenChriss::default();
    for terms in [ImpactTerms::Both, ImpactTerms::PermanentOnly] {
        let f = |qty| {
            m.impact_fraction_for(
                &ImpactInputs { qty, average_volume: 50_000.0, sigma: 0.02 },
                terms,
            )
        };
        let mut previous = f(1.0);
        for qty in [10.0, 100.0, 1_000.0, 25_000.0] {
            let current = f(qty);
            assert!(
                current > previous,
                "{terms:?} not increasing at qty={qty}: {current} <= {previous}"
            );
            assert!(f(2.0 * qty) < 2.0 * current, "{terms:?} not concave at {qty}");
            previous = current;
        }
    }
}

/// The provided method is the `Both` spelling, bit for bit — every pre-split call site keeps
/// its number.
#[test]
fn impact_fraction_is_the_both_spelling_bit_for_bit() {
    let m = ImpactModelAlmgrenChriss::default();
    for qty in [1.0f64, 750.0, 90_000.0] {
        let input = ImpactInputs { qty, average_volume: 40_000.0, sigma: 0.03 };
        assert_eq!(
            m.impact_fraction(&input).to_bits(),
            m.impact_fraction_for(&input, ImpactTerms::Both).to_bits()
        );
    }
}

/// Degenerate inputs charge nothing under EITHER selection — the `PermanentOnly` arm shares
/// the guards rather than reaching them by a second route.
#[test]
fn degenerate_inputs_charge_nothing_under_either_terms_selection() {
    let m = ImpactModelAlmgrenChriss::default();
    for input in [
        ImpactInputs { qty: 0.0, average_volume: 1000.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, average_volume: 0.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, average_volume: 1000.0, sigma: f64::NAN },
    ] {
        assert_eq!(m.impact_fraction_for(&input, ImpactTerms::PermanentOnly), 0.0, "{input:?}");
    }
}

/// The trait object shape the engine stores carries the same `Send + Sync` bound as the
/// `properties` `Arc` beside it. NOT because anything moves it across a thread today —
/// `crates/vike-backtest/src/search/optimize/evaluator.rs`'s `SearchEvaluatorStore` maps `run_backtest`
/// sequentially, and `OrderBrokerSimulator` holds `Vec<Rc<Vec<Bar>>>` so it is not `Send` at all — but so
/// that this field is never the thing that blocks a future parallel sweep. Relaxing the bound later
/// is a breaking change; keeping it costs nothing.
#[test]
fn model_is_a_send_sync_trait_object() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn ImpactModel>();
    let _boxed: std::sync::Arc<dyn ImpactModel> =
        std::sync::Arc::new(ImpactModelAlmgrenChriss::default());
}
