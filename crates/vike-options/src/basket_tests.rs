use super::*;

/// Long 1x 100-strike call at 5.00.
fn long_call() -> Basket {
    Basket::new(vec![Leg::new(1.0, 100.0, true, 5.0)], 100.0)
}

/// Bull call spread: long 100C @5, short 110C @2. Debit 3.
fn vertical() -> Basket {
    Basket::new(vec![Leg::new(1.0, 100.0, true, 5.0), Leg::new(-1.0, 110.0, true, 2.0)], 100.0)
}

/// Long straddle: 100C @5 + 100P @4. Debit 9.
fn straddle() -> Basket {
    Basket::new(vec![Leg::new(1.0, 100.0, true, 5.0), Leg::new(1.0, 100.0, false, 4.0)], 100.0)
}

/// Iron condor: -90P@2 +85P@1 -110C@2 +115C@1. Net credit 2, wings 5 wide.
fn condor() -> Basket {
    Basket::new(
        vec![
            Leg::new(-1.0, 90.0, false, 2.0),
            Leg::new(1.0, 85.0, false, 1.0),
            Leg::new(-1.0, 110.0, true, 2.0),
            Leg::new(1.0, 115.0, true, 1.0),
        ],
        100.0,
    )
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

// ---- payoff / premium goldens (hand-computed) ----

#[test]
fn long_call_payoff_and_bounds() {
    let b = long_call();
    assert_eq!(b.net_premium(), 5.0);
    assert_eq!(b.payoff_at(0.0), -5.0);
    assert_eq!(b.payoff_at(90.0), -5.0);
    assert_eq!(b.payoff_at(100.0), -5.0);
    assert_eq!(b.payoff_at(110.0), 5.0);
    assert_eq!(b.break_evens(), vec![105.0]);
    assert_eq!(b.max_loss(), PayoffBound::Bounded(-5.0));
    assert_eq!(b.max_profit(), PayoffBound::Unbounded);
}

#[test]
fn vertical_spread_payoff_and_bounds() {
    let b = vertical();
    assert_eq!(b.net_premium(), 3.0);
    assert_eq!(b.payoff_at(90.0), -3.0);
    assert_eq!(b.payoff_at(110.0), 7.0);
    assert_eq!(b.payoff_at(200.0), 7.0); // capped by the short wing
    assert_eq!(b.break_evens(), vec![103.0]);
    assert_eq!(b.max_profit(), PayoffBound::Bounded(7.0));
    assert_eq!(b.max_loss(), PayoffBound::Bounded(-3.0));
    assert_eq!(b.upper_slope(), 0.0);
}

#[test]
fn straddle_payoff_and_bounds() {
    let b = straddle();
    assert_eq!(b.net_premium(), 9.0);
    assert_eq!(b.payoff_at(100.0), -9.0);
    assert_eq!(b.payoff_at(120.0), 11.0);
    assert_eq!(b.payoff_at(80.0), 11.0);
    assert_eq!(b.break_evens(), vec![91.0, 109.0]);
    assert_eq!(b.max_loss(), PayoffBound::Bounded(-9.0));
    assert_eq!(b.max_profit(), PayoffBound::Unbounded);
    // Downside is bounded by S = 0: put pays 100, less the 9 debit.
    assert_eq!(b.payoff_at(0.0), 91.0);
}

#[test]
fn iron_condor_payoff_and_bounds() {
    let b = condor();
    assert_eq!(b.net_premium(), -2.0); // net credit
    assert_eq!(b.payoff_at(100.0), 2.0);
    assert_eq!(b.payoff_at(80.0), -3.0);
    assert_eq!(b.payoff_at(200.0), -3.0);
    assert_eq!(b.break_evens(), vec![88.0, 112.0]);
    assert_eq!(b.max_profit(), PayoffBound::Bounded(2.0));
    assert_eq!(b.max_loss(), PayoffBound::Bounded(-3.0));
    assert_eq!(b.payoff_at(88.0), 0.0);
    assert_eq!(b.payoff_at(112.0), 0.0);
}

// ---- unbounded detection ----

#[test]
fn naked_short_call_loss_is_unbounded_profit_is_not() {
    let b = Basket::new(vec![Leg::new(-1.0, 100.0, true, 5.0)], 100.0);
    assert_eq!(b.max_loss(), PayoffBound::Unbounded);
    assert_eq!(b.max_profit(), PayoffBound::Bounded(5.0));
    assert_eq!(b.break_evens(), vec![105.0]);
}

#[test]
fn naked_short_put_downside_is_bounded_at_zero() {
    let b = Basket::new(vec![Leg::new(-1.0, 100.0, false, 5.0)], 100.0);
    assert_eq!(b.upper_slope(), 0.0);
    assert_eq!(b.max_profit(), PayoffBound::Bounded(5.0));
    // Worst case is S_T = 0: -(100) + 5 credit.
    assert_eq!(b.max_loss(), PayoffBound::Bounded(-95.0));
    assert_eq!(b.break_evens(), vec![95.0]);
}

#[test]
fn ratio_backspread_is_unbounded_up_and_bounded_down() {
    // -1x 100C @6, +2x 110C @2  ⇒ upper slope = +1.
    let b =
        Basket::new(vec![Leg::new(-1.0, 100.0, true, 6.0), Leg::new(2.0, 110.0, true, 2.0)], 100.0);
    assert_eq!(b.upper_slope(), 1.0);
    assert_eq!(b.max_profit(), PayoffBound::Unbounded);
    assert_eq!(b.net_premium(), -2.0);
    // Worst point is the 110 kink: -(10) + 0 + 2 credit.
    assert_eq!(b.max_loss(), PayoffBound::Bounded(-8.0));
}

#[test]
fn empty_basket_is_flat_and_bounded() {
    let b = Basket::new(vec![], 100.0);
    assert_eq!(b.net_premium(), 0.0);
    assert_eq!(b.payoff_at(123.0), 0.0);
    assert_eq!(b.upper_slope(), 0.0);
    assert_eq!(b.max_profit(), PayoffBound::Bounded(0.0));
    assert_eq!(b.max_loss(), PayoffBound::Bounded(0.0));
    // Flat at exactly zero: the single node touches zero.
    assert_eq!(b.break_evens(), vec![0.0]);
}

#[test]
fn payoff_bound_accessors() {
    assert_eq!(PayoffBound::Bounded(2.5).value(), Some(2.5));
    assert_eq!(PayoffBound::Unbounded.value(), None);
    assert!(PayoffBound::Unbounded.is_unbounded());
    assert!(!PayoffBound::Bounded(0.0).is_unbounded());
}

#[test]
fn right_slope_matches_numeric_derivative() {
    let b = condor();
    for &p in &[70.0, 87.0, 95.0, 100.0, 112.0, 130.0] {
        let analytic = b.slope_at(p);
        let eps = 1e-4;
        let fd = (b.payoff_at(p + eps) - b.payoff_at(p)) / eps;
        assert!(close(analytic, fd, 1e-6), "p={p} analytic={analytic} fd={fd}");
    }
    // At/beyond the highest strike the right-hand slope IS the upper slope.
    assert_eq!(b.slope_at(115.0), b.upper_slope());
    assert_eq!(b.slope_at(1e9), b.upper_slope());
}

// ---- net greeks ----

#[test]
fn net_greeks_are_bit_identical_to_summed_single_leg_calls() {
    let b = condor();
    let mk = |iv: f64| LegMarket { iv, tau: 0.25, spot: 100.0, r: 0.03 };
    let markets = [mk(0.55), mk(0.60), mk(0.50), mk(0.58)];
    let net = b.net_greeks(&markets).expect("computable");

    let (mut d, mut g, mut th, mut v) = (0.0, 0.0, 0.0, 0.0);
    let (mut vn, mut vm, mut ch, mut vt, mut co) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (leg, m) in b.legs.iter().zip(markets.iter()) {
        let (ld, lg, lt, lv) =
            black_scholes_greeks(m.spot, leg.strike, m.tau, m.iv, leg.kind(), m.r).unwrap();
        let so = second_order_greeks(m.spot, leg.strike, m.tau, m.iv, leg.kind(), m.r).unwrap();
        d += leg.qty * ld;
        g += leg.qty * lg;
        th += leg.qty * lt;
        v += leg.qty * lv;
        vn += leg.qty * so.vanna;
        vm += leg.qty * so.vomma;
        ch += leg.qty * so.charm;
        vt += leg.qty * so.veta;
        co += leg.qty * so.color;
    }
    // Bit-identical, not merely close.
    assert_eq!(net.delta.to_bits(), d.to_bits());
    assert_eq!(net.gamma.to_bits(), g.to_bits());
    assert_eq!(net.theta.to_bits(), th.to_bits());
    assert_eq!(net.vega.to_bits(), v.to_bits());
    assert_eq!(net.vanna.to_bits(), vn.to_bits());
    assert_eq!(net.vomma.to_bits(), vm.to_bits());
    assert_eq!(net.charm.to_bits(), ch.to_bits());
    assert_eq!(net.veta.to_bits(), vt.to_bits());
    assert_eq!(net.color.to_bits(), co.to_bits());
}

#[test]
fn net_greeks_of_a_straddle_has_near_zero_delta_and_double_gamma() {
    let b = straddle();
    let m = LegMarket { iv: 0.5, tau: 0.25, spot: 100.0, r: 0.0 };
    let net = b.net_greeks(&[m, m]).unwrap();
    // At r = 0 a same-strike call+put straddle: delta = N(d1) + N(d1) - 1, small near ATM.
    assert!(net.delta.abs() < 0.2, "delta={}", net.delta);
    let single = black_scholes_greeks(100.0, 100.0, 0.25, 0.5, OptionKind::Call, 0.0).unwrap();
    assert!(close(net.gamma, 2.0 * single.1, 1e-15));
    assert!(net.vega > 0.0 && net.theta < 0.0);
}

#[test]
fn net_greeks_rejects_length_mismatch_and_invalid_legs() {
    let b = straddle();
    let m = LegMarket { iv: 0.5, tau: 0.25, spot: 100.0, r: 0.0 };
    assert_eq!(b.net_greeks(&[m]), None); // wrong length
    let expired = LegMarket { tau: 0.0, ..m };
    assert_eq!(b.net_greeks(&[m, expired]), None); // one leg not computable
}

// ---- POP ----

fn dist(sigma: f64, tau: f64) -> TerminalDist {
    TerminalDist { spot: 100.0, sigma, tau, r: 0.0 }
}

#[test]
fn pop_of_a_far_otm_short_put_is_near_one() {
    // Short 50P for 5: break-even 45, spot 100, modest vol ⇒ almost surely profitable.
    let b = Basket::new(vec![Leg::new(-1.0, 50.0, false, 5.0)], 100.0);
    let p = b.probability_of_profit(&dist(0.2, 0.08)).unwrap();
    assert!(p > 0.999_999, "pop={p}");
    assert!(p <= 1.0);
}

#[test]
fn pop_of_a_deep_itm_long_call_is_near_one() {
    // Long 10C bought BELOW intrinsic-at-spot: break-even 60 with spot 100 and low vol.
    let b = Basket::new(vec![Leg::new(1.0, 10.0, true, 50.0)], 100.0);
    let p = b.probability_of_profit(&dist(0.2, 0.08)).unwrap();
    assert!(p > 0.999_99, "pop={p}");
}

#[test]
fn pop_of_a_basket_and_its_negation_sum_to_one() {
    // Profit regions are exact complements (break-evens are measure zero).
    let b = straddle();
    let neg = Basket::new(
        b.legs.iter().map(|l| Leg::new(-l.qty, l.strike, l.is_call, l.premium)).collect(),
        b.underlying_spot,
    );
    let d = dist(0.15, 0.25);
    let a = b.probability_of_profit(&d).unwrap();
    let c = neg.probability_of_profit(&d).unwrap();
    assert!(close(a + c, 1.0, 1e-12), "long={a} short={c}");
    // Sanity: at 15% vol the 91/109 break-evens are a stretch, so the long side is the
    // less likely one (raise the vol and this flips -- the complement law above does not).
    assert!(a < 0.5 && c > 0.5, "long={a} short={c}");
}

#[test]
fn pop_of_a_condor_matches_the_direct_interval_probability() {
    let b = condor();
    let d = dist(0.35, 0.25);
    let (mu_ln, s) = d.log_params().unwrap();
    // Profitable exactly on (88, 112) — the two break-evens.
    let direct = d.cdf(112.0, mu_ln, s) - d.cdf(88.0, mu_ln, s);
    let p = b.probability_of_profit(&d).unwrap();
    assert!(close(p, direct, 1e-15), "p={p} direct={direct}");
    assert!(p > 0.0 && p < 1.0);
}

#[test]
fn pop_rejects_degenerate_distributions() {
    let b = straddle();
    assert_eq!(b.probability_of_profit(&dist(0.0, 0.25)), None);
    assert_eq!(b.probability_of_profit(&dist(0.4, 0.0)), None);
    let bad_spot = TerminalDist { spot: 0.0, ..dist(0.4, 0.25) };
    assert_eq!(b.probability_of_profit(&bad_spot), None);
}

// ---- EV ----

#[test]
fn ev_of_a_zero_premium_long_call_matches_black_scholes_at_zero_rate() {
    // With r = 0 the discount factor is 1, so E[(S_T - K)+] == the BS call price.
    let b = Basket::new(vec![Leg::new(1.0, 105.0, true, 0.0)], 100.0);
    let d = dist(0.4, 0.5);
    let ev = b.expected_value(&d, &EvGrid::default()).unwrap();
    let bs =
        crate::greeks::black_scholes_price(100.0, 105.0, 0.5, 0.4, OptionKind::Call, 0.0).unwrap();
    assert!(close(ev, bs, 1e-11), "ev={ev} bs={bs}");
}

#[test]
fn ev_of_a_zero_premium_long_put_matches_black_scholes_at_zero_rate() {
    let b = Basket::new(vec![Leg::new(1.0, 95.0, false, 0.0)], 100.0);
    let d = dist(0.4, 0.5);
    let ev = b.expected_value(&d, &EvGrid::default()).unwrap();
    let bs =
        crate::greeks::black_scholes_price(100.0, 95.0, 0.5, 0.4, OptionKind::Put, 0.0).unwrap();
    assert!(close(ev, bs, 1e-11), "ev={ev} bs={bs}");
}

#[test]
fn ev_converges_as_the_grid_refines() {
    let b = condor();
    let d = dist(0.35, 0.25);
    let coarse = b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 1024 }).unwrap();
    let fine = b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 16384 }).unwrap();
    assert!(close(coarse, fine, 1e-9), "coarse={coarse} fine={fine}");
    // The trapezoid error should shrink with the step count, not wander.
    let mid = b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 4096 }).unwrap();
    assert!((mid - fine).abs() <= (coarse - fine).abs() + 1e-15);
}

#[test]
fn ev_is_deterministic() {
    let b = straddle();
    let d = dist(0.5, 0.25);
    let a = b.expected_value(&d, &EvGrid::default()).unwrap();
    let c = b.expected_value(&d, &EvGrid::default()).unwrap();
    assert_eq!(a.to_bits(), c.to_bits());
}

#[test]
fn ev_of_a_fairly_priced_straddle_is_near_zero() {
    // Price both legs at their BS value under the same law ⇒ zero-EV bet at r = 0.
    let (s, k, tau, sigma) = (100.0, 100.0, 0.25, 0.5);
    let c = crate::greeks::black_scholes_price(s, k, tau, sigma, OptionKind::Call, 0.0).unwrap();
    let p = crate::greeks::black_scholes_price(s, k, tau, sigma, OptionKind::Put, 0.0).unwrap();
    let b = Basket::new(vec![Leg::new(1.0, k, true, c), Leg::new(1.0, k, false, p)], s);
    let ev = b
        .expected_value(&TerminalDist { spot: s, sigma, tau, r: 0.0 }, &EvGrid::default())
        .unwrap();
    // Two legs, so ~2x the single-leg quadrature budget the tests above pin at 1e-11.
    assert!(ev.abs() < 1e-10, "ev={ev}");
}

#[test]
fn ev_rejects_degenerate_grids() {
    let b = straddle();
    let d = dist(0.4, 0.25);
    assert_eq!(b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 0 }), None);
    assert_eq!(b.expected_value(&d, &EvGrid { n_sigma: 0.0, steps: 64 }), None);
}

// ---- curve sampler ----

#[test]
fn payoff_curve_samples_endpoints_and_matches_payoff_at() {
    let b = vertical();
    let pts = b.payoff_curve(80.0, 120.0, 4);
    assert_eq!(pts.len(), 5);
    assert_eq!(pts[0].0, 80.0);
    assert_eq!(pts[4].0, 120.0);
    for (price, pnl) in pts {
        assert_eq!(pnl, b.payoff_at(price));
    }
}

#[test]
fn payoff_curve_around_spot_is_centred_and_guarded() {
    let b = vertical();
    let pts = b.payoff_curve_around_spot(0.2, 10);
    assert_eq!(pts.len(), 11);
    assert!(close(pts[0].0, 80.0, 1e-12));
    assert!(close(pts[10].0, 120.0, 1e-12));
    assert!(Basket::new(vec![], 0.0).payoff_curve_around_spot(0.2, 10).is_empty());
    assert!(b.payoff_curve_around_spot(0.0, 10).is_empty());
}

#[test]
fn payoff_curve_rejects_degenerate_ranges() {
    let b = vertical();
    assert!(b.payoff_curve(80.0, 120.0, 0).is_empty());
    assert!(b.payoff_curve(120.0, 80.0, 4).is_empty());
    assert!(b.payoff_curve(-1.0, 80.0, 4).is_empty());
}
