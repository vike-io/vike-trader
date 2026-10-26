use super::*;

fn bar(t: f64, l: f64, h: f64) -> Bar {
    Bar { t, ot: 0, o: l, h, l, c: h, v: 1.0 }
}

/// A ramp of `n` bars whose low/high both climb, so the lowest low is bar 0 and the highest
/// high is the last — the ordinary shape, and the one whose extremes are unambiguous.
fn ramp(n: usize) -> Vec<Bar> {
    (0..n).map(|i| bar(i as f64, 100.0 + i as f64, 110.0 + i as f64)).collect()
}

#[test]
fn apply_drawings_writes_both_overlays_and_reports_whether_it_drew() {
    // `Default` + `extend`, NOT a struct literal and NOT a field assignment: `ChartState`
    // carries private cache fields (`cache_key`, `vol_cache`, …) so a literal is illegal from
    // outside `vike-chart`, and a plain `st.bars = ..` right after a `default()` trips
    // `clippy::field_reassign_with_default`, which is a merge gate here. Same shape as
    // `crate::ui::core_sync`'s own `chart_with_ots` test builder.
    let mut st = vike_chart::model::ChartState::default();
    st.bars.extend(ramp(20));
    assert!(apply_drawings(&mut st));
    assert_eq!(st.overlays.len(), 2);
    assert!(st.overlays.contains_key(TREND_OVERLAY) && st.overlays.contains_key(SUPPORT_OVERLAY));

    // Idempotent: the map is keyed by NAME, so a second pass replaces rather than accumulates.
    assert!(apply_drawings(&mut st));
    assert_eq!(st.overlays.len(), 2, "re-applying must not stack drawings");

    // Nothing to draw ⇒ nothing written, and the caller is told so.
    let mut empty = vike_chart::model::ChartState::default();
    assert!(!apply_drawings(&mut empty));
    assert!(empty.overlays.is_empty(), "a series it cannot read must stay untouched");
}

#[test]
fn a_ramp_draws_a_trendline_from_the_lowest_low_to_the_highest_high() {
    let v = trendline_overlay(&ramp(20)).expect("20 bars is enough to draw");
    let trend = &v.iter().find(|(n, _)| *n == TREND_OVERLAY).expect("a trend line").1;
    assert_eq!(trend, &vec![[0.0, 100.0], [19.0, 129.0]]);
}

/// The x-ordering rule: on a DOWN series the highest high comes first, and the polyline must
/// still run left-to-right.
#[test]
fn a_falling_series_still_draws_its_trendline_left_to_right() {
    let bars: Vec<Bar> =
        (0..12).map(|i| bar(i as f64, 100.0 - i as f64, 110.0 - i as f64)).collect();
    let v = trendline_overlay(&bars).expect("12 bars is enough to draw");
    let trend = &v.iter().find(|(n, _)| *n == TREND_OVERLAY).expect("a trend line").1;
    assert!(
        trend[0][0] < trend[1][0],
        "the polyline must be x-ascending whichever extreme came first: {trend:?}"
    );
    assert_eq!(trend, &vec![[0.0, 110.0], [11.0, 89.0]]);
}

#[test]
fn the_support_level_is_horizontal_at_the_lowest_low_across_the_whole_range() {
    let v = trendline_overlay(&ramp(20)).expect("20 bars is enough to draw");
    let sup = &v.iter().find(|(n, _)| *n == SUPPORT_OVERLAY).expect("a support line").1;
    assert_eq!(sup, &vec![[0.0, 100.0], [19.0, 100.0]]);
}

/// Every drawing must satisfy the render's own admission rule — `paint_price_overlays` skips
/// anything shorter than two points, and an entry it skips is a drawing nobody sees.
#[test]
fn every_drawing_carries_at_least_the_two_points_the_render_requires() {
    let v = trendline_overlay(&ramp(20)).expect("20 bars is enough to draw");
    assert_eq!(v.len(), 2, "both drawings must be produced together");
    for (name, pts) in &v {
        assert!(pts.len() >= 2, "{name} has {} point(s), the render skips it", pts.len());
        assert!(
            pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
            "{name} carries a non-finite point"
        );
    }
}

#[test]
fn too_few_bars_draws_nothing() {
    for n in 0..MIN_DRAW_BARS {
        assert!(trendline_overlay(&ramp(n)).is_none(), "{n} bars must draw nothing");
    }
    assert!(trendline_overlay(&ramp(MIN_DRAW_BARS)).is_some(), "the floor itself draws");
}

/// A flat series has no extreme worth drawing — see the degenerate arm's comment.
#[test]
fn a_flat_series_draws_nothing() {
    let flat: Vec<Bar> = (0..20).map(|i| bar(i as f64, 100.0, 100.0)).collect();
    assert!(trendline_overlay(&flat).is_none());
}

/// A non-finite bar must not become the extreme it would otherwise win by `total_cmp`
/// (`NaN` sorts above every finite value, so an unfiltered `max_by` would pick it).
#[test]
fn a_non_finite_bar_is_not_allowed_to_become_an_extreme() {
    let mut bars = ramp(20);
    bars[5] = bar(5.0, f64::NAN, f64::NAN);
    let v = trendline_overlay(&bars).expect("the finite bars still draw");
    for (name, pts) in &v {
        assert!(
            pts.iter().all(|p| p[0].is_finite() && p[1].is_finite()),
            "{name} took a non-finite bar as an extreme: {pts:?}"
        );
    }
}
