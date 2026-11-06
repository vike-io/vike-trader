use super::*;

/// `scale` guard, `blackout_ms` base window, resolution at T = 1_000 ms.
fn guard_params(scale: f64, blackout_ms: i64) -> AsParams {
    AsParams {
        atm_blackout_scale: scale,
        resolution_blackout_ms: blackout_ms,
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(1_000),
        ..AsParams::default()
    }
}

#[test]
fn blackout_off_is_the_base_window() {
    // scale 0 ⇒ base window, even with a live ATM underlying.
    let mut st = AsState::new(guard_params(0.0, 200));
    st.set_underlying(100.0, 100.0, 1e-4);
    assert_eq!(st.effective_blackout_ms(1_000, 500), 200);
    // fixed base: 500 < 1000−200 = 800 ⇒ not yet; 850 ≥ 800 ⇒ in blackout.
    assert!(!st.in_blackout(500));
    assert!(st.in_blackout(850));
}

#[test]
fn near_atm_widens_the_blackout_earlier() {
    // scale 2 at ATM (s_now == s_open ⇒ z = 0) ⇒ window = 200·(1 + 2) = 600.
    let mut st = AsState::new(guard_params(2.0, 200));
    st.set_underlying(100.0, 100.0, 1e-4);
    assert_eq!(st.effective_blackout_ms(1_000, 500), 600);
    // blacks out at ts ≥ 1000 − 600 = 400 — EARLIER than the base (which needs ts ≥ 800).
    assert!(st.in_blackout(500), "near ATM blacks out earlier");
    let mut base = AsState::new(guard_params(0.0, 200));
    base.set_underlying(100.0, 100.0, 1e-4);
    assert!(!base.in_blackout(500), "the base window is not yet in blackout at the same ts");
}

#[test]
fn far_from_atm_relaxes_to_the_base_window() {
    // a deep-ITM underlying ⇒ z huge ⇒ exp(−z²/2) ≈ 0 ⇒ window ≈ base.
    let mut st = AsState::new(guard_params(2.0, 200));
    st.set_underlying(120.0, 100.0, 1e-4);
    assert_eq!(st.effective_blackout_ms(1_000, 500), 200);
}

#[test]
fn no_underlying_uses_the_base_window() {
    // scale > 0 but no underlying fed ⇒ base, byte-identical.
    let st = AsState::new(guard_params(2.0, 200));
    assert_eq!(st.effective_blackout_ms(1_000, 500), 200);
}
