use super::*;

#[test]
fn fee_matches_the_probability_curve() {
    assert_eq!(fee(0.25), 0.072 * 0.25 * 0.75);
    // symmetric about 0.5 (to fp, not bitwise: `a*b` and `b*a` round the same but
    // `0.072*0.3*0.7` and `0.072*0.7*0.3` do not), and zero at the boundaries
    assert!((fee(0.3) - fee(0.7)).abs() <= f64::EPSILON * fee(0.3));
    assert_eq!(fee(0.0), 0.0);
    assert_eq!(fee(1.0), 0.0);
}

#[test]
fn p_up_is_a_half_when_spot_has_not_moved() {
    assert_eq!(p_up(0.83, 100.0, 100.0, 1e-4, 100.0, H), 0.5);
}

#[test]
fn p_up_rises_with_an_up_move_and_falls_with_a_down_move() {
    let up = p_up(0.83, 100.5, 100.0, 1e-4, 100.0, H);
    let dn = p_up(0.83, 99.5, 100.0, 1e-4, 100.0, H);
    assert!(up > 0.5, "{up}");
    assert!(dn < 0.5, "{dn}");
    // The model is symmetric in LOG space (the mirror of s·k is s/k, not 2s − s·k), so pin the
    // symmetry the way the formula actually has it: p_up(s·k) + p_up(s/k) == 1.
    let k = 1.005_f64;
    let a = p_up(0.83, 100.0 * k, 100.0, 1e-4, 100.0, H);
    let b = p_up(0.83, 100.0 / k, 100.0, 1e-4, 100.0, H);
    assert!((a + b - 1.0).abs() < 1e-12, "{a} {b}");
}

#[test]
fn wc_is_the_min_over_betas_not_the_mean() {
    let (s_now, s_open, sigma, t) = (100.3, 100.0, 1e-4, 100.0);
    let a = wc_betas(0, 0.2, s_now, s_open, sigma, t, H, &[BETAS[0]]);
    let b = wc_betas(0, 0.2, s_now, s_open, sigma, t, H, &[BETAS[1]]);
    let both = wc(0, 0.2, s_now, s_open, sigma, t, H);
    assert_eq!(both, a.min(b));
    assert_ne!(both, 0.5 * (a + b));
}

#[test]
fn wc_down_side_uses_one_minus_p_up() {
    let (ask, s_now, s_open, sigma, t) = (0.2, 100.3, 100.0, 1e-4, 100.0);
    let up = wc_betas(0, ask, s_now, s_open, sigma, t, H, &[0.83]);
    let dn = wc_betas(1, ask, s_now, s_open, sigma, t, H, &[0.83]);
    // (p) − ask − f  and  (1−p) − ask − f  differ by exactly (2p − 1)
    let p = p_up(0.83, s_now, s_open, sigma, t, H);
    assert!(((up - dn) - (2.0 * p - 1.0)).abs() < 1e-15);
}

#[test]
fn cheap_band_is_half_open() {
    assert!(in_cheap_band(0.10));
    assert!(in_cheap_band(0.3499999));
    assert!(!in_cheap_band(0.35));
    assert!(!in_cheap_band(0.0999999));
}

#[test]
fn cheap_time_window_is_inclusive_on_both_tte_bounds() {
    // tte = 300 − t, allowed [15, 270]  <=>  t in [30, 285]
    assert!(!cheap_time_ok(29.0, H));
    assert!(cheap_time_ok(30.0, H));
    assert!(cheap_time_ok(285.0, H));
    assert!(!cheap_time_ok(286.0, H));
}

#[test]
fn cheap_gate_rejects_out_of_band_out_of_time_and_thin_edge() {
    let (s_now, s_open, sigma, t) = (100.6, 100.0, 1e-4, 100.0);
    // in band + in time + fat edge -> fires
    assert!(cheap_gate(0, 0.20, s_now, s_open, sigma, t, THETA, H).is_some());
    // same print, out of band
    assert!(cheap_gate(0, 0.40, s_now, s_open, sigma, t, THETA, H).is_none());
    // same print, outside the time window (t = 10 -> tte 290 > 270)
    assert!(cheap_gate(0, 0.20, s_now, s_open, sigma, 10.0, THETA, H).is_none());
    // same print, wrong side (Down, after an up move) -> negative edge
    assert!(cheap_gate(1, 0.20, s_now, s_open, sigma, t, THETA, H).is_none());
}

#[test]
fn cheap_gate_is_strictly_greater_than_theta() {
    // Construct a print whose edge is EXACTLY reproducible, then gate at that exact value.
    let (ask, s_now, s_open, sigma, t) = (0.20, 100.6, 100.0, 1e-4, 100.0);
    let e = wc(0, ask, s_now, s_open, sigma, t, H);
    assert!(cheap_gate(0, ask, s_now, s_open, sigma, t, e, H).is_none(), "equal must not fire");
    let just_under = f64::from_bits(e.to_bits() - 1);
    assert_eq!(cheap_gate(0, ask, s_now, s_open, sigma, t, just_under, H), Some(e));
}

fn ramp(n: usize) -> Vec<(f64, f64)> {
    // a deterministic non-degenerate series (a pure geometric ramp has zero return variance,
    // so alternate the step to give the estimator something to measure)
    (0..n)
        .map(|i| {
            let px = 100.0 * (1.0 + 0.001 * ((i % 3) as f64) + 0.0001 * i as f64);
            (1_000_000.0 + i as f64, px)
        })
        .collect()
}

#[test]
fn trailing_sigma_is_none_below_the_warmup_floor() {
    let s = ramp(SIGMA_MIN_SECONDS - 1);
    assert_eq!(trailing_sigma(s, SIGMA_LOOKBACK_S, 1_000_100.0), None);
    let s = ramp(SIGMA_MIN_SECONDS);
    assert!(trailing_sigma(s, SIGMA_LOOKBACK_S, 1_000_100.0).is_some());
}

#[test]
fn trailing_sigma_counts_observed_seconds_not_interpolated_ones() {
    // 29 observed seconds spread over 60 real seconds: the interpolated grid would be 60 long,
    // but the warmup floor counts OBSERVED seconds -> still None.
    let s: Vec<(f64, f64)> = ramp(60).into_iter().step_by(2).take(29).collect();
    assert_eq!(s.len(), 29);
    assert_eq!(trailing_sigma(s, SIGMA_LOOKBACK_S, 1_000_100.0), None);
}

#[test]
fn trailing_sigma_drops_samples_older_than_the_cutoff() {
    let all = ramp(120);
    let now = 1_000_119.0;
    // lookback 40 keeps ts >= now-40 = 1_000_079 -> 41 samples
    let windowed: Vec<(f64, f64)> =
        all.iter().copied().filter(|(ts, _)| *ts >= now - 40.0).collect();
    assert_eq!(windowed.len(), 41);
    assert_eq!(trailing_sigma(all, 40.0, now), trailing_sigma(windowed, 40.0, now));
}

#[test]
fn trailing_sigma_last_write_wins_inside_one_second() {
    let base = ramp(40);
    // two prints per second; the LAST one must be the sample kept
    let mut dup: Vec<(f64, f64)> = Vec::new();
    for &(ts, px) in &base {
        dup.push((ts + 0.1, px * 1.5));
        dup.push((ts + 0.9, px));
    }
    assert_eq!(
        trailing_sigma(dup, SIGMA_LOOKBACK_S, 1_000_100.0),
        trailing_sigma(base, SIGMA_LOOKBACK_S, 1_000_100.0)
    );
}

#[test]
fn trailing_sigma_interpolates_a_gap_into_a_ramp_not_a_jump() {
    // A dead-flat series with ONE step, seen either densely or with the step's interior
    // seconds missing. Interpolation must spread the step over the gap: the sparse series'
    // sigma equals that of the explicitly-interpolated dense series, and is far BELOW what a
    // single spurious jump return would produce.
    let mut dense: Vec<(f64, f64)> = Vec::new();
    for i in 0..40u32 {
        dense.push((1_000_000.0 + i as f64, 100.0));
    }
    for i in 40..50u32 {
        // linear ramp 100 -> 110 over 10 seconds
        dense.push((1_000_000.0 + i as f64, 100.0 + (i - 39) as f64));
    }
    let sparse: Vec<(f64, f64)> =
        dense.iter().copied().filter(|(ts, _)| !(1_000_040.0..1_000_049.0).contains(ts)).collect();
    let a = trailing_sigma(dense.clone(), SIGMA_LOOKBACK_S, 1_000_100.0).unwrap();
    let b = trailing_sigma(sparse, SIGMA_LOOKBACK_S, 1_000_100.0).unwrap();
    assert!((a - b).abs() < 1e-15, "{a} vs {b}");

    // ...and the un-interpolated (raw sparse-grid) estimator would be much larger: prove the
    // interpolation actually suppressed a jump by comparing against a same-length series where
    // the whole step lands on ONE return.
    let mut jump: Vec<(f64, f64)> = Vec::new();
    for i in 0..49u32 {
        jump.push((1_000_000.0 + i as f64, if i < 40 { 100.0 } else { 110.0 }));
    }
    // The whole 10 % step lands on ONE return instead of ten, so the jump's stddev is
    // ~sqrt(10)x the ramp's — the concrete damping the INTERPOLATE clause buys.
    let c = trailing_sigma(jump, SIGMA_LOOKBACK_S, 1_000_100.0).unwrap();
    assert!(c > 3.0 * a, "interpolation must damp the stall: ramp {a} vs jump {c}");
}

#[test]
fn price_at_returns_the_most_recent_not_the_nearest() {
    let s = vec![(10.0, 1.0), (20.0, 2.0), (30.0, 3.0)];
    assert_eq!(price_at(s.clone(), 25.0), Some(2.0));
    assert_eq!(price_at(s.clone(), 30.0), Some(3.0));
    assert_eq!(price_at(s.clone(), 9.9), None);
    // duplicate stamps: the FIRST in iteration order wins (strict `>` in the scan)
    let d = vec![(10.0, 1.0), (10.0, 9.0)];
    assert_eq!(price_at(d, 15.0), Some(1.0));
}
