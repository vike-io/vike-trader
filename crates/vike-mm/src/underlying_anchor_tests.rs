use super::*;

const EPS: f64 = 1e-12;

/// Blend `weight`, in the time-to-resolution regime with a 300 s window closing at T = 300_000 ms.
fn blend_params(weight: f64) -> AsParams {
    AsParams {
        underlying_weight: weight,
        underlying_beta: 1.0,
        window_secs: 300.0,
        horizon_mode: HorizonMode::TimeToResolution,
        resolution_ts: Some(300_000),
        resolution_blackout_ms: 0,
        gamma: 0.5,
        kappa_mode: KappaMode::Fixed,
        q_scale: 1.0,
        min_standoff_ticks: 1.0,
        ..AsParams::default()
    }
}

#[test]
fn blend_off_returns_book_mid() {
    // weight 0 ⇒ book mid, even with a live underlying set.
    let mut st = AsState::new(blend_params(0.0));
    st.set_underlying(100.5, 100.0, 1e-4);
    assert!((st.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
}

#[test]
fn blend_pulls_the_anchor_toward_the_model() {
    // ts = 150_000 ⇒ t = 150 s of the 300 s window; a spot above the open ⇒ p_up ≈ 1, so the
    // w = 0.5 blend pulls the 0.50 book mid up, landing strictly between the mid and the model.
    let mut st = AsState::new(blend_params(0.5));
    st.set_underlying(100.5, 100.0, 1e-4);
    let model = st.model_fair(150_000).expect("model available");
    assert!(model > 0.9, "up-drift ⇒ high p_up, got {model}");
    let anchor = st.blended_anchor(0.50, 150_000);
    assert!(anchor > 0.50 && anchor < model, "anchor {anchor} in (0.50, {model})");
    assert!((anchor - (0.5 * 0.50 + 0.5 * model)).abs() < EPS);
}

#[test]
fn falls_back_to_book_mid_when_uncomputable() {
    // weight > 0 but no underlying fed ⇒ book mid.
    let st = AsState::new(blend_params(0.5));
    assert!((st.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
    // underlying set but window_secs == 0 ⇒ book mid.
    let mut no_window = AsState::new(AsParams { window_secs: 0.0, ..blend_params(0.5) });
    no_window.set_underlying(100.5, 100.0, 1e-4);
    assert!((no_window.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
    // ConstantTau (τ undefined) ⇒ book mid.
    let mut const_tau =
        AsState::new(AsParams { horizon_mode: HorizonMode::ConstantTau, ..blend_params(0.5) });
    const_tau.set_underlying(100.5, 100.0, 1e-4);
    assert!((const_tau.blended_anchor(0.50, 150_000) - 0.50).abs() < EPS);
}

#[test]
fn model_fair_matches_the_shared_p_up() {
    let mut st = AsState::new(blend_params(0.5));
    st.set_underlying(100.5, 100.0, 1e-4);
    // t = window − (T − now)/1000 = 300 − (300_000 − 150_000)/1000 = 150 s.
    let expected = vike_model::p_up(1.0, 100.5, 100.0, 1e-4, 150.0, 300.0);
    assert!((st.model_fair(150_000).unwrap() - expected).abs() < EPS);
}

#[test]
fn price_without_an_underlying_is_byte_identical() {
    let view = BookView {
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.49, 100.0)],
        asks: vec![BookLevel::new(0.51, 100.0)],
    };
    // Blend configured (weight 0.5) but no underlying fed ⇒ same quote as blend-off.
    let on = AsState::new(blend_params(0.5)).price(&view, 1.0, 150_000);
    let off = AsState::new(blend_params(0.0)).price(&view, 1.0, 150_000);
    assert_eq!(on, off, "no underlying ⇒ anchor is the book mid ⇒ byte-identical");
}
