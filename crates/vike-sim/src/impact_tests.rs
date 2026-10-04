use super::*;

fn bar(ts: i64, close: f64, volume: f64) -> Bar {
    Bar {
        ts,
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

/// Hand-computed against the published constants. nu = 1000 / (1.0 * 100_000) = 0.01, and at
/// the unit horizon nu == X/V, so both terms read off the same number.
///   permanent = 0.314 * 0.02 * 0.01^0.891
///   temporary = 0.142 * 0.02 * 0.01^0.600
///
/// The expectations in this module spell `libm::pow`, matching `impact_frac`. They assert at
/// `1e-15` ABSOLUTE on quantities of order `1e-3`, i.e. ~1e-12 relative, so a last-bit
/// disagreement between `libm` and a platform `powf` would never have reddened them — this is
/// hygiene, not a fix. It buys two things: the comparison becomes exact rather than merely
/// tolerant, and no reader can copy a `.powf` out of a test in this file into production.
#[test]
fn almgren_chriss_matches_a_hand_computed_value() {
    let m = AlmgrenChriss::default();
    let input = ImpactInputs { qty: 1000.0, avg_volume: 100_000.0, sigma: 0.02 };
    let nu: f64 = 0.01;
    let expect = 0.314 * 0.02 * libm::pow(nu * 1.0, 0.891) + 0.142 * 0.02 * libm::pow(nu, 0.600);
    assert!((m.impact_frac(&input) - expect).abs() < 1e-15, "got {}", m.impact_frac(&input));
    // Sanity on the magnitude: 1% of daily volume at 2% daily vol costs a few bp.
    assert!(expect > 1e-4 && expect < 1e-2, "{expect}");
}

/// A second hand-computed point at a NON-UNIT horizon, written in the PAPER's own variables
/// (`X/V` for permanent, `nu = X/(V*T)` for temporary) rather than in the code's algebra — so
/// it catches a permanent term that has picked up a spurious `T` factor. This is the exact
/// deviation an earlier revision carried: `gamma*sigma*T*nu^alpha`, which equals the paper
/// only at `T = 1` and inflates by `T^(1-alpha)` elsewhere.
#[test]
fn exec_time_scales_only_the_temporary_term() {
    let t: f64 = 2.0;
    let m = AlmgrenChriss::with_exec_time(t);
    let (qty, vol, sigma) = (4000.0f64, 100_000.0f64, 0.03f64);
    let input = ImpactInputs { qty, avg_volume: vol, sigma };
    let frac_of_volume = qty / vol; // X/V = 0.04 — the permanent term's argument
    let nu = qty / t / vol; // 0.02 — the participation RATE
    let expect =
        0.314 * sigma * libm::pow(frac_of_volume, 0.891) + 0.142 * sigma * libm::pow(nu, 0.600);
    assert!((m.impact_frac(&input) - expect).abs() < 1e-15);
}

/// The horizon-independence of the permanent term, stated directly: hold `qty`/`avg_volume`
/// fixed, vary `exec_time`, and the ONLY thing that may move is the temporary term. Pinned by
/// reconstructing the permanent term once and subtracting it at each horizon.
#[test]
fn permanent_impact_does_not_move_with_the_horizon() {
    let (qty, vol, sigma) = (10_000.0f64, 200_000.0f64, 0.025f64);
    let permanent = AC_GAMMA * sigma * libm::pow(qty / vol, AC_ALPHA);
    let mut prev_temporary = f64::INFINITY;
    for t in [0.25f64, 1.0, 4.0, 10.0] {
        let m = AlmgrenChriss::with_exec_time(t);
        let total = m.impact_frac(&ImpactInputs { qty, avg_volume: vol, sigma });
        let temporary = total - permanent;
        let expect_temporary = AC_ETA * sigma * libm::pow(qty / t / vol, AC_BETA);
        assert!(
            (temporary - expect_temporary).abs() < 1e-15,
            "permanent term drifted at exec_time={t}: residual {temporary} != {expect_temporary}"
        );
        assert!(temporary < prev_temporary, "a slower horizon must cost less temporarily");
        prev_temporary = temporary;
    }
}

#[test]
fn impact_is_strictly_monotone_in_size() {
    let m = AlmgrenChriss::default();
    let f = |qty| m.impact_frac(&ImpactInputs { qty, avg_volume: 50_000.0, sigma: 0.02 });
    let mut prev = f(1.0);
    for qty in [10.0, 100.0, 500.0, 1_000.0, 5_000.0, 25_000.0, 100_000.0] {
        let cur = f(qty);
        assert!(cur > prev, "not increasing at qty={qty}: {cur} <= {prev}");
        prev = cur;
    }
}

/// Concavity: doubling size less than doubles the cost (both exponents are < 1). This is the
/// economically load-bearing shape — a linear model would make large orders uninvestable.
#[test]
fn impact_is_concave_in_size() {
    let m = AlmgrenChriss::default();
    let f = |qty| m.impact_frac(&ImpactInputs { qty, avg_volume: 50_000.0, sigma: 0.02 });
    for qty in [100.0, 1_000.0, 10_000.0] {
        assert!(f(2.0 * qty) < 2.0 * f(qty), "not concave at {qty}");
    }
}

#[test]
fn impact_scales_linearly_in_sigma_and_falls_with_liquidity() {
    let m = AlmgrenChriss::default();
    let a = m.impact_frac(&ImpactInputs { qty: 500.0, avg_volume: 50_000.0, sigma: 0.01 });
    let b = m.impact_frac(&ImpactInputs { qty: 500.0, avg_volume: 50_000.0, sigma: 0.02 });
    assert!((b - 2.0 * a).abs() < 1e-15, "sigma must enter linearly");
    let deep = m.impact_frac(&ImpactInputs { qty: 500.0, avg_volume: 500_000.0, sigma: 0.01 });
    assert!(deep < a, "a deeper book must cost less");
}

#[test]
fn degenerate_inputs_charge_nothing_rather_than_nan() {
    let m = AlmgrenChriss::default();
    for input in [
        ImpactInputs { qty: 0.0, avg_volume: 1000.0, sigma: 0.02 },
        ImpactInputs { qty: -5.0, avg_volume: 1000.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, avg_volume: 0.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, avg_volume: 1000.0, sigma: 0.0 },
        ImpactInputs { qty: 100.0, avg_volume: 1000.0, sigma: f64::NAN },
        ImpactInputs { qty: f64::INFINITY, avg_volume: 1000.0, sigma: 0.02 },
    ] {
        assert_eq!(m.impact_frac(&input), 0.0, "{input:?}");
    }
    assert_eq!(
        AlmgrenChriss::with_exec_time(0.0).impact_frac(&ImpactInputs {
            qty: 100.0,
            avg_volume: 1000.0,
            sigma: 0.02
        }),
        0.0
    );
}

#[test]
fn window_stats_measures_sigma_and_mean_volume() {
    // closes 100, 110, 121 -> returns 0.1, 0.1 -> sigma 0 (constant growth), avg vol 200
    let bars: Vec<Bar> = vec![bar(0, 100.0, 100.0), bar(1, 110.0, 200.0), bar(2, 121.0, 300.0)];
    let s = window_stats(&bars, 10).unwrap();
    assert!(s.sigma.abs() < 1e-15, "constant returns => zero dispersion, got {}", s.sigma);
    assert!((s.avg_volume - 200.0).abs() < 1e-12);
}

#[test]
fn window_stats_honors_the_window_and_needs_three_bars() {
    let bars: Vec<Bar> = (0..10).map(|i| bar(i, 100.0 + i as f64, 10.0 * (i + 1) as f64)).collect();
    // last 3 bars: volumes 80, 90, 100 -> mean 90
    let s = window_stats(&bars, 3).unwrap();
    assert!((s.avg_volume - 90.0).abs() < 1e-12);
    assert!(s.sigma > 0.0);
    // whole series mean volume 55
    let all = window_stats(&bars, 100).unwrap();
    assert!((all.avg_volume - 55.0).abs() < 1e-12);
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

// --- the lane split: which TERMS a fill law has not already paid ---------------------------

/// `PermanentOnly` is EXACTLY the permanent term — reconstructed from the published constants
/// rather than by subtracting, so a bug that moved a factor from one term to the other cannot
/// hide behind an identity that holds by construction.
#[test]
fn permanent_only_is_exactly_the_permanent_term() {
    for t in [0.5f64, 1.0, 7.0] {
        let m = AlmgrenChriss::with_exec_time(t);
        let input = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.018 };
        let want = AC_GAMMA * input.sigma * libm::pow(input.qty / input.avg_volume, AC_ALPHA);
        let got = m.impact_frac_for(&input, ImpactTerms::PermanentOnly);
        assert!((got - want).abs() < 1e-15, "exec_time={t}: {got} vs {want}");
    }
}

/// The double-count claim, as arithmetic: a lane that already walked a real book pays strictly
/// LESS here than one that priced its fill off a single quote — and the gap it does not pay is
/// precisely the temporary term, the thing the walk supplied.
#[test]
fn a_walked_book_is_charged_strictly_less_than_a_top_of_book_quote() {
    let m = AlmgrenChriss::default();
    let input = ImpactInputs { qty: 2_500.0, avg_volume: 60_000.0, sigma: 0.02 };
    let both = m.impact_frac_for(&input, ImpactTerms::Both);
    let permanent = m.impact_frac_for(&input, ImpactTerms::PermanentOnly);
    assert!(permanent > 0.0, "the footprint a replayed book cannot show is still a real cost");
    assert!(permanent < both, "{permanent} is not strictly less than {both}");
    let nu = input.qty / input.avg_volume;
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
    let input = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.018 };
    let published = AlmgrenChriss::default().impact_frac_for(&input, ImpactTerms::PermanentOnly);
    assert!(published > 0.0, "the fixture must produce a real charge");

    // The horizon knob: inert on this selection, at every horizon. Compared at `1e-15`
    // ABSOLUTE on a quantity of order `1e-4` (~1e-11 relative) rather than bit-for-bit,
    // because the code reconstructs `X/V` as `(qty / exec_time / avg_volume) * exec_time`, and
    // that round trip is exact in the paper's algebra but not in binary. The tolerance is
    // still orders tighter than the thing it watches for: an `exec_time` that had leaked into
    // the permanent term would move this by a FACTOR, not by a last bit.
    for t in [0.1f64, 1.0, 1_000.0] {
        let got =
            AlmgrenChriss::with_exec_time(t).impact_frac_for(&input, ImpactTerms::PermanentOnly);
        assert!(
            (got - published).abs() < 1e-15,
            "exec_time={t} moved a PermanentOnly charge ({got} vs {published}) — the L2 \
                 lane's knob is not the horizon"
        );
    }
    // the coefficient knob: strictly linear, and it reaches this selection
    for scale in [0.02f64, 0.5, 3.0] {
        let m = AlmgrenChriss::with_coefficients(AC_GAMMA * scale, AC_ETA, 1.0);
        let got = m.impact_frac_for(&input, ImpactTerms::PermanentOnly);
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
    let input = ImpactInputs { qty: 3_000.0, avg_volume: 80_000.0, sigma: 0.018 };
    let base = AlmgrenChriss::default();
    let permanent = base.impact_frac_for(&input, ImpactTerms::PermanentOnly);
    let m = AlmgrenChriss::with_coefficients(AC_GAMMA, AC_ETA * 4.0, 1.0);
    assert_eq!(
        m.impact_frac_for(&input, ImpactTerms::PermanentOnly).to_bits(),
        permanent.to_bits(),
        "eta leaked into the permanent term"
    );
    let want_temporary = 4.0 * (base.impact_frac_for(&input, ImpactTerms::Both) - permanent);
    let got_temporary = m.impact_frac_for(&input, ImpactTerms::Both) - permanent;
    assert!(
        (got_temporary - want_temporary).abs() < 1e-15,
        "eta must scale the temporary term linearly: {got_temporary} != {want_temporary}"
    );
}

/// The trait's monotonicity contract binds EACH `terms` value on its own — the L2 lane must
/// not be the one place where a bigger order can cost less.
#[test]
fn each_terms_selection_is_monotone_and_concave_in_size() {
    let m = AlmgrenChriss::default();
    for terms in [ImpactTerms::Both, ImpactTerms::PermanentOnly] {
        let f = |qty| {
            m.impact_frac_for(&ImpactInputs { qty, avg_volume: 50_000.0, sigma: 0.02 }, terms)
        };
        let mut prev = f(1.0);
        for qty in [10.0, 100.0, 1_000.0, 25_000.0] {
            let cur = f(qty);
            assert!(cur > prev, "{terms:?} not increasing at qty={qty}: {cur} <= {prev}");
            assert!(f(2.0 * qty) < 2.0 * cur, "{terms:?} not concave at {qty}");
            prev = cur;
        }
    }
}

/// The provided method is the `Both` spelling, bit for bit — every pre-split call site keeps
/// its number.
#[test]
fn impact_frac_is_the_both_spelling_bit_for_bit() {
    let m = AlmgrenChriss::default();
    for qty in [1.0f64, 750.0, 90_000.0] {
        let input = ImpactInputs { qty, avg_volume: 40_000.0, sigma: 0.03 };
        assert_eq!(
            m.impact_frac(&input).to_bits(),
            m.impact_frac_for(&input, ImpactTerms::Both).to_bits()
        );
    }
}

/// Degenerate inputs charge nothing under EITHER selection — the `PermanentOnly` arm shares
/// the guards rather than reaching them by a second route.
#[test]
fn degenerate_inputs_charge_nothing_under_either_terms_selection() {
    let m = AlmgrenChriss::default();
    for input in [
        ImpactInputs { qty: 0.0, avg_volume: 1000.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, avg_volume: 0.0, sigma: 0.02 },
        ImpactInputs { qty: 100.0, avg_volume: 1000.0, sigma: f64::NAN },
    ] {
        assert_eq!(m.impact_frac_for(&input, ImpactTerms::PermanentOnly), 0.0, "{input:?}");
    }
}

// --- the tick-lane window -------------------------------------------------------------------

/// The two windows are ONE measurement: a `TickWindow` fed the same (price, size) pairs a bar
/// series carries measures bit-identically. This is what stops the tick lane from growing a
/// second, subtly different definition of `sigma`.
#[test]
fn the_tick_window_measures_exactly_what_the_bar_window_measures() {
    let bars: Vec<Bar> = vec![
        bar(0, 100.0, 12.0),
        bar(1, 101.5, 9.0),
        bar(2, 100.75, 31.0),
        bar(3, 103.0, 4.0),
        bar(4, 102.25, 17.0),
    ];
    let mut w = TickWindow::with_capacity(bars.len());
    for b in &bars {
        w.push(b.close, b.volume);
    }
    let from_bars = window_stats(&bars, bars.len()).expect("measurable");
    let from_ticks = w.stats().expect("measurable");
    assert_eq!(from_ticks.sigma.to_bits(), from_bars.sigma.to_bits());
    assert_eq!(from_ticks.avg_volume.to_bits(), from_bars.avg_volume.to_bits());
}

/// The window is BOUNDED: it holds `cap` prints and evicts the oldest, so a long replay costs
/// a constant. Proven by measurement, not by reading `len` — the retained window must be the
/// one the last `cap` prints describe.
#[test]
fn the_tick_window_is_bounded_and_keeps_the_newest_prints() {
    let mut w = TickWindow::with_capacity(3);
    for (px, sz) in [(10.0, 1.0), (11.0, 1.0), (12.0, 5.0), (13.0, 9.0), (14.0, 10.0)] {
        w.push(px, sz);
    }
    assert_eq!(w.len(), 3, "the window grew past its capacity");
    let tail: Vec<Bar> = vec![bar(0, 12.0, 5.0), bar(1, 13.0, 9.0), bar(2, 14.0, 10.0)];
    let want = window_stats(&tail, 3).expect("measurable");
    let got = w.stats().expect("measurable");
    assert_eq!(got.avg_volume.to_bits(), want.avg_volume.to_bits(), "not the newest three");
    assert_eq!(got.sigma.to_bits(), want.sigma.to_bits());
}

/// Warmup and degenerate tapes measure NOTHING rather than guessing — the tick twin of
/// `window_stats_honors_the_window_and_needs_three_bars`.
#[test]
fn the_tick_window_refuses_to_measure_a_window_it_cannot_support() {
    let mut w = TickWindow::with_capacity(8);
    assert!(w.is_empty() && w.stats().is_none(), "an empty window measures nothing");
    w.push(10.0, 1.0);
    w.push(11.0, 1.0);
    assert!(w.stats().is_none(), "two prints are one return, not enough");
    w.push(12.0, 1.0);
    assert!(w.stats().is_some(), "three prints are measurable");
    // a zero-size tape cannot price participation at all
    let mut novol = TickWindow::with_capacity(8);
    for px in [10.0, 11.0, 12.0] {
        novol.push(px, 0.0);
    }
    assert!(novol.stats().is_none(), "zero traded size cannot price impact");
    // ...nor can a non-positive print
    let mut bad = TickWindow::with_capacity(8);
    for px in [10.0, 0.0, 12.0] {
        bad.push(px, 1.0);
    }
    assert!(bad.stats().is_none(), "a zero price has no meaningful return");
    // a zero-capacity window records nothing at all rather than growing without bound
    let mut nocap = TickWindow::with_capacity(0);
    for px in [10.0, 11.0, 12.0] {
        nocap.push(px, 1.0);
    }
    assert!(nocap.is_empty() && nocap.stats().is_none());
}

/// The trait object shape the engine stores carries the same `Send + Sync` bound as the
/// `properties` `Arc` beside it. NOT because anything moves it across a thread today —
/// `crates/vike-backtest/src/harness/optimize.rs`'s `StoreEvaluator` maps `run_backtest` sequentially, and `SimBroker` holds
/// `Vec<Rc<Vec<Bar>>>` so it is not `Send` at all — but so that this field is never the thing
/// that blocks a future parallel sweep. Relaxing the bound later is a breaking change;
/// keeping it costs nothing.
#[test]
fn model_is_a_send_sync_trait_object() {
    fn assert_send_sync<T: Send + Sync + ?Sized>() {}
    assert_send_sync::<dyn ImpactModel>();
    let _boxed: std::sync::Arc<dyn ImpactModel> = std::sync::Arc::new(AlmgrenChriss::default());
}
