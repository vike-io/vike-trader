use super::*;

const EPS: f64 = 1e-12;

#[test]
fn terminal_var_ramps_in_and_peaks_at_half() {
    let g = 1.0_f64;
    // OFF gates: non-positive γ_term OR non-positive ramp ⇒ exactly 0.
    assert!(terminal_var(0.0, 200, 50.0, 0.5).abs() < EPS);
    assert!(terminal_var(g, 0, 50.0, 0.5).abs() < EPS);
    assert!(terminal_var(g, -5, 50.0, 0.5).abs() < EPS);
    // Ramp weight: `τ ≥ ramp` ⇒ 0 (clamped even beyond); `τ = 0` ⇒ full `g·p(1−p)`; half at ramp/2.
    let full = g * 0.25; // p(1−p) at p = 0.5
    assert!(terminal_var(g, 200, 200.0, 0.5).abs() < EPS);
    assert!(terminal_var(g, 200, 400.0, 0.5).abs() < EPS);
    assert!((terminal_var(g, 200, 0.0, 0.5) - full).abs() < EPS);
    assert!((terminal_var(g, 200, 100.0, 0.5) - full * 0.5).abs() < EPS);
    // Monotone: strengthens as `τ` falls toward resolution.
    assert!(terminal_var(g, 200, 50.0, 0.5) > terminal_var(g, 200, 150.0, 0.5));
    // Peaks at p = 0.5, → 0 at the walls.
    assert!(terminal_var(g, 200, 0.0, 0.5) > terminal_var(g, 200, 0.0, 0.1));
    assert!(terminal_var(g, 200, 0.0, 0.1) > terminal_var(g, 200, 0.0, 0.01));
}

#[test]
fn penalty_strengthens_skew_where_diffusion_vanishes() {
    // The headline property: near resolution the diffusion variance → 0, so WITHOUT the penalty
    // the A-S skew term vanishes (r == s) — the wrong shape for a binary market.
    let (s, q_norm, gamma) = (0.5_f64, 1.0_f64, 0.5_f64);
    let v_diffusion = 0.0_f64; // H → 0
    assert!((as_reservation_price(s, q_norm, gamma, v_diffusion) - s).abs() < EPS);
    // WITH the penalty, the settlement variance keeps a live (and strengthening) skew.
    let tvar = terminal_var(0.8, 200, 100.0, s); // τ = 100 of a 200 ms ramp, p = 0.5
    assert!(tvar > 0.0);
    assert!(as_reservation_price(s, q_norm, gamma, v_diffusion + tvar) < s - 1e-9);
    // …and the half-spread widens with the added variance.
    let kappa = 50.0_f64;
    assert!(
        as_optimal_half_spread(gamma, v_diffusion + tvar, kappa)
            > as_optimal_half_spread(gamma, v_diffusion, kappa) + 1e-9
    );
}

#[test]
fn terminal_var_now_only_fires_in_time_to_resolution_regime() {
    let on = AsParams {
        terminal_penalty_gamma: 1.0,
        terminal_ramp_ms: 200,
        resolution_ts: Some(1_000),
        horizon_mode: HorizonMode::TimeToResolution,
        ..AsParams::default()
    };
    assert!(AsState::new(on).terminal_var_now(0.5, 900) > 0.0); // τ = 100 in a 200 ms ramp
    // ConstantTau ⇒ τ = (T − t) undefined ⇒ 0, even with the knobs set.
    let ct = AsParams { horizon_mode: HorizonMode::ConstantTau, ..on };
    assert!(AsState::new(ct).terminal_var_now(0.5, 900).abs() < EPS);
    // TimeToResolution but no known resolution_ts ⇒ 0.
    let no_res = AsParams { resolution_ts: None, ..on };
    assert!(AsState::new(no_res).terminal_var_now(0.5, 900).abs() < EPS);
}

#[test]
fn price_penalty_widens_and_skews_near_resolution() {
    let view = BookView {
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.49, 100.0)],
        asks: vec![BookLevel::new(0.51, 100.0)],
    };
    // T = 1000; price at ts = 900 (τ = 100) inside a 200 ms ramp; p = mid = 0.50 (peak). No
    // blackout so the terminal term (not the wall) governs; long inventory skews it toward selling.
    let base = AsParams {
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(1_000),
        resolution_blackout_ms: 0,
        variance_mode: VarianceMode::LocalCapped,
        kappa_mode: KappaMode::Fixed,
        gamma: 0.5,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        ..AsParams::default()
    };
    let (ob, oa) = AsState::new(base).price(&view, 1.0, 900).expect("two-sided (off)");
    let on = AsParams { terminal_penalty_gamma: 0.8, terminal_ramp_ms: 200, ..base };
    let (nb, na) = AsState::new(on).price(&view, 1.0, 900).expect("two-sided (on)");
    // Wider posted spread (settlement variance widened the half-spread AND skewed the reservation).
    assert!((na - nb) > (oa - ob) + 1e-9, "on {nb}..{na} vs off {ob}..{oa}");
    // Long-inventory skew pulls the ask no higher than the OFF case (urgency to sell down).
    assert!(na <= oa + 1e-9);
}

#[test]
fn price_is_byte_identical_outside_the_ramp_window() {
    let view = BookView {
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.49, 100.0)],
        asks: vec![BookLevel::new(0.51, 100.0)],
    };
    let base = AsParams {
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(10_000),
        resolution_blackout_ms: 0,
        kappa_mode: KappaMode::Fixed,
        gamma: 0.5,
        q_scale: 1.0,
        ..AsParams::default()
    };
    // Control (penalty OFF) vs penalty CONFIGURED but inactive — τ = 10_000 ≫ ramp 200 ⇒ weight 0.
    let off_q = AsState::new(base).price(&view, 3.0, 0).expect("off");
    let cfg = AsParams { terminal_penalty_gamma: 0.9, terminal_ramp_ms: 200, ..base };
    let in_q = AsState::new(cfg).price(&view, 3.0, 0).expect("configured-but-inactive");
    assert_eq!(off_q, in_q, "outside the ramp window the penalty is byte-identical");
}
