use super::*;
use vike_marketdata::BookLevel;
use vike_marketdata::test_support::{book, trade};

fn vpin_study() -> ActiveStudy {
    // bucket_volume 10 so a handful of trades commit buckets; window 2.
    ActiveStudy::with_params(1, get_study("vpin").unwrap(), vec![10.0, 2.0, 1.0])
}

#[test]
fn registry_lookup_and_shape() {
    assert_eq!(study_registry().len(), 3);
    assert_eq!(get_study("vpin").unwrap().kind, StudyKind::Vpin);
    assert!(!get_study("vpin").unwrap().needs_book);
    assert!(get_study("book_imbalance").unwrap().needs_book);
    assert!(get_study("otr").unwrap().needs_book);
    assert!(get_study("nope").is_none());
    for m in study_registry() {
        assert_eq!(m.outputs.len(), 1, "{} is single-line today", m.name);
        assert!(!m.params.is_empty());
    }
}

#[test]
fn params_seed_from_defaults_and_coerce_clamps() {
    let s = ActiveStudy::new(7, get_study("vpin").unwrap());
    assert_eq!(s.params, vec![1000.0, 50.0, 1.0]);
    // out-of-range + missing entries are clamped/defaulted by `coerce`
    let s2 = ActiveStudy::with_params(8, get_study("book_imbalance").unwrap(), vec![9999.0]);
    assert_eq!(s2.params, vec![100.0, 0.0]);
}

/// The study twin of `indicators_tests`' `a_fresh_oscillators_levels_have_no_colour_chosen`: no
/// colour chosen, so the level paints in the theme's line.
#[test]
fn a_fresh_studys_levels_have_no_colour_chosen() {
    let s = vpin_study();
    assert_eq!(s.bands.len(), 3, "VPIN's levels are 0.2/0.5/0.8");
    assert!(s.bands.iter().all(|b| b.color.is_none() && b.show), "{:?}", s.bands);
}

#[test]
fn pre_warmup_samples_are_nan() {
    let mut s = vpin_study();
    s.sync(3, false);
    let v = s.series();
    assert_eq!(v.len(), 3);
    assert!(v.iter().all(|x| x.is_nan()), "{v:?}");
    assert_eq!(s.committed_len(), 3);
}

#[test]
fn ticks_then_bar_close_samples_committed_value() {
    let mut s = vpin_study();
    s.sync(1, false); // bar 0 closes before any trade → NaN
    // 10 units of buy volume fills bucket 1 exactly → imbalance 1.0, VPIN = 1.0
    s.on_trade(&trade(1, 100.0, 10.0, false), None);
    s.sync(2, false);
    let v = s.series();
    assert!(v[0].is_nan());
    assert_eq!(v[1], 1.0);
}

#[test]
fn quiet_bars_carry_the_last_value() {
    let mut s = vpin_study();
    s.on_trade(&trade(1, 100.0, 10.0, false), None);
    s.sync(1, false);
    // three further bars close with NO ticks at all
    s.sync(4, false);
    assert_eq!(s.series(), &[1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn forming_tail_is_speculative_and_replaced() {
    let mut s = vpin_study();
    s.on_trade(&trade(1, 100.0, 10.0, false), None); // committed VPIN 1.0
    s.sync(1, true);
    assert_eq!(s.series().len(), 2, "one committed + one forming");
    assert_eq!(s.committed_len(), 1);
    // a half-filled SELL bucket makes the interim read differ from the committed one
    s.on_trade(&trade(2, 100.0, 5.0, true), None);
    s.sync(1, true);
    assert_eq!(s.series().len(), 2, "the old forming tail was replaced, not appended");
    assert_eq!(s.committed_len(), 1);
    assert_eq!(s.series()[0], 1.0, "the committed sample never moves");
    // dropping the forming bar leaves the committed series alone
    s.sync(1, false);
    assert_eq!(s.series(), &[1.0]);
}

#[test]
fn shrinking_closed_series_truncates() {
    let mut s = vpin_study();
    s.on_trade(&trade(1, 100.0, 10.0, false), None);
    s.sync(5, false);
    assert_eq!(s.series().len(), 5);
    s.sync(2, false); // symbol/interval swap: fewer closed bars
    assert_eq!(s.series().len(), 2);
    assert_eq!(s.committed_len(), 2);
}

#[test]
fn book_gated_studies_render_empty_without_a_book() {
    for name in ["book_imbalance", "otr"] {
        let mut s = ActiveStudy::new(3, get_study(name).unwrap());
        assert!(s.is_empty(), "{name}");
        s.sync(10, true);
        assert!(s.series().is_empty(), "{name} must fabricate nothing");
        assert_eq!(s.committed_len(), 0);
    }
    // VPIN is trades-only and never gated
    let mut v = vpin_study();
    assert!(!v.is_empty());
    v.sync(2, false);
    assert_eq!(v.series().len(), 2);
}

#[test]
fn imbalance_starts_sampling_once_a_book_arrives() {
    let mut s = ActiveStudy::with_params(4, get_study("book_imbalance").unwrap(), vec![1.0, 0.0]);
    s.sync(2, false);
    assert!(s.series().is_empty());
    // 3 bid vs 1 ask at the top level → (3−1)/(3+1) = 0.5
    s.on_book(1, &book(&[BookLevel::new(99.0, 3.0)], &[BookLevel::new(101.0, 1.0)]));
    assert!(!s.is_empty());
    s.sync(3, false);
    let v = s.series();
    assert_eq!(v.len(), 3);
    // bars closed before the first book still sample carry-last (= the first
    // reading, latched at feed time) — the series only STARTS at the gate flip.
    assert_eq!(v[2], 0.5);
}

#[test]
fn set_params_rebuilds_state_and_clears_the_series() {
    let mut s = vpin_study();
    s.on_trade(&trade(1, 100.0, 10.0, false), None);
    s.sync(2, false);
    assert_eq!(s.series().len(), 2);
    s.outputs[0].color = Color32::RED;
    s.set_params(vec![50.0, 4.0, 1.0]);
    assert_eq!(s.params, vec![50.0, 4.0, 1.0]);
    assert!(s.series().is_empty());
    assert_eq!(s.committed_len(), 0);
    assert_eq!(s.outputs[0].color, Color32::RED, "paint state survives a reconfigure");
    // re-warms from the next tick, at the NEW bucket size (50 units)
    s.on_trade(&trade(2, 100.0, 10.0, false), None);
    s.sync(1, false);
    assert!(s.series()[0].is_nan(), "10 < 50 units: no bucket committed yet");
}

#[test]
fn pane_registration_skips_hidden_and_gated_studies() {
    let mut studies = vec![
        vpin_study(),
        ActiveStudy::new(2, get_study("book_imbalance").unwrap()),
        ActiveStudy::new(3, get_study("otr").unwrap()),
    ];
    let mut present = vec![PaneKey::Price, PaneKey::Volume];
    push_study_panes(&mut present, &studies);
    assert_eq!(present, vec![PaneKey::Price, PaneKey::Volume, PaneKey::Study(1)]);

    // feed the imbalance study a book → its pane appears
    studies[1].on_book(1, &book(&[BookLevel::new(99.0, 3.0)], &[BookLevel::new(101.0, 1.0)]));
    studies[0].visible = false; // and hide VPIN
    let mut present = Vec::new();
    push_study_panes(&mut present, &studies);
    assert_eq!(present, vec![PaneKey::Study(2)]);
}

#[test]
fn otr_windows_sample_at_bar_close() {
    // depth 5, 100 ms windows
    let mut s = ActiveStudy::with_params(5, get_study("otr").unwrap(), vec![5.0, 100.0]);
    s.on_book(0, &book(&[BookLevel::new(99.0, 1.0)], &[BookLevel::new(101.0, 1.0)]));
    s.on_trade(&trade(10, 100.0, 1.0, false), None);
    // a book at ts >= 100 closes the first window
    s.on_book(100, &book(&[BookLevel::new(99.0, 2.0)], &[BookLevel::new(101.0, 1.0)]));
    s.sync(1, false);
    let v = s.series();
    assert_eq!(v.len(), 1);
    assert!(v[0].is_finite(), "a committed OTR window was sampled: {v:?}");
}
