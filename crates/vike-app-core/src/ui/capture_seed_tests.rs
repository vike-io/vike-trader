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
fn a_seed_prices_the_resting_order_below_the_market_and_leaves_the_market_order_unpriced() {
    let s = plan_trade_seed("binance", "BTCUSDT", 50_000.0, 7).expect("a usable close");
    assert_eq!(s.resting.order_type, "limit");
    assert_eq!(s.resting.price, Some(50_000.0 * RESTING_DISCOUNT));
    assert!(
        s.resting.price.expect("a limit price") < 50_000.0,
        "the resting order must sit BELOW the market or it is not resting"
    );
    assert_eq!(s.market.order_type, "market");
    assert_eq!(s.market.price, None, "a market order carries no price");
    assert_eq!((s.resting.qty, s.market.qty), (SEED_QTY, SEED_QTY));
    assert_eq!((s.resting.side, s.market.side), (SIDE_BUY, SIDE_BUY));
}

#[test]
fn both_seeded_orders_carry_the_asked_for_venue_and_symbol() {
    let s = plan_trade_seed("okx", "ETHUSDT", 2_000.0, 0).expect("a usable close");
    for r in [&s.resting, &s.market] {
        assert_eq!(r.venue, "okx");
        assert_eq!(r.symbol, "ETHUSDT");
    }
}

/// The ids must differ from each other AND across nonces — one core-wide id space, and a
/// collision is an order the engine refuses rather than a second order.
#[test]
fn seeded_client_order_ids_are_unique_within_and_across_calls() {
    let a = plan_trade_seed("binance", "BTCUSDT", 100.0, 1).expect("a usable close");
    let b = plan_trade_seed("binance", "BTCUSDT", 100.0, 2).expect("a usable close");
    let ids = [
        a.resting.client_order_id,
        a.market.client_order_id,
        b.resting.client_order_id,
        b.market.client_order_id,
    ];
    let uniq: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(uniq.len(), ids.len(), "seeded ids collided: {ids:?}");
}

/// The "not yet" arm — see [`plan_trade_seed`]'s doc for why this is not merely defensive.
#[test]
fn an_unusable_last_close_mints_no_order_at_all() {
    for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(
            plan_trade_seed("binance", "BTCUSDT", px, 0).is_none(),
            "a last close of {px} must mint nothing"
        );
    }
}

/// The "everything is fine" inputs, which every guard test below then spoils in exactly one
/// way — so a test that goes green proves the guard it names, not some other field.
fn ok_inputs() -> SeedInputs<'static> {
    SeedInputs {
        armed: true,
        frame: SEED_FRAME,
        remote_control: false,
        venue_is_live: false,
        last_close: Some(50_000.0),
        venue: "binance",
    }
}

fn plan_with(i: &SeedInputs<'_>) -> SeedPlan {
    // `CoreSnapshot::empty` is `crate::orders::order_dispatch`'s own test idiom. The snapshot is
    // consulted for exactly one thing here — `multiplier_of`, which an empty grid answers with
    // the 1.0 fallback — so an empty one over the seed's own (venue, symbol) is the honest
    // fixture rather than a stub that happens to compile.
    plan_trade_seed_commands(
        i,
        &crate::orders::order_entry::OrderLimits::default(),
        &vike_exec::CoreSnapshot::empty(i.venue, SEED_SYMBOL),
    )
}

#[test]
fn the_happy_path_submits_both_orders_and_spends_the_flag() {
    let p = plan_with(&ok_inputs());
    assert_eq!(p.commands.len(), 2, "the resting order AND the market order");
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    assert!(p.spend);
}

/// ⚠ THE SAFETY TEST. Both refusals must emit NO command — and both must SPEND the flag, or a
/// refused seed re-refuses on every frame for the rest of the session.
#[test]
fn neither_guard_can_emit_an_order_and_both_are_final() {
    for (name, i) in [
        ("remote control", SeedInputs { remote_control: true, ..ok_inputs() }),
        ("live venue", SeedInputs { venue_is_live: true, ..ok_inputs() }),
        // Both at once: still refused, still silent — the ladder must not fall through.
        ("both", SeedInputs { remote_control: true, venue_is_live: true, ..ok_inputs() }),
    ] {
        let p = plan_with(&i);
        assert!(p.commands.is_empty(), "{name}: a guarded seed emitted {:?}", p.commands.len());
        assert!(p.spend, "{name}: a refusal that re-arms warns every frame forever");
        assert_eq!(p.warnings.len(), 1, "{name}: a refusal must say so exactly once");
    }
}

/// A control-enabled observer is the case the venue check alone CANNOT catch — it arms no
/// venue live, so `venue_is_live` is false and only the remote guard stands
/// between a capture knob and a real account. Spelled as its own test because the combination
/// is the whole hazard.
#[test]
fn a_control_enabled_observer_is_refused_even_though_no_venue_reads_live() {
    let i = SeedInputs { remote_control: true, venue_is_live: false, ..ok_inputs() };
    assert!(plan_with(&i).commands.is_empty());
}

/// The three "not yet" arms, which must NOT spend the flag — each is a state a later frame
/// can leave.
#[test]
fn a_seed_that_cannot_act_yet_stays_armed_and_says_nothing() {
    for (name, i) in [
        ("disarmed", SeedInputs { armed: false, ..ok_inputs() }),
        ("too early", SeedInputs { frame: SEED_FRAME - 1, ..ok_inputs() }),
        ("no price yet", SeedInputs { last_close: None, ..ok_inputs() }),
        ("unusable price", SeedInputs { last_close: Some(0.0), ..ok_inputs() }),
    ] {
        let p = plan_with(&i);
        assert!(p.commands.is_empty(), "{name}");
        assert!(!p.spend, "{name}: must stay armed so a later frame can re-try");
        assert!(p.warnings.is_empty(), "{name}: a not-yet is not a complaint");
    }
}

/// A disarmed knob is the DEFAULT, and it must reach the same do-nothing answer whatever else
/// is true — including on a frame where every guard would have refused.
#[test]
fn an_unarmed_seed_is_inert_whatever_the_rest_of_the_world_looks_like() {
    let i = SeedInputs { armed: false, remote_control: true, venue_is_live: true, ..ok_inputs() };
    let p = plan_with(&i);
    assert!(p.commands.is_empty() && p.warnings.is_empty() && !p.spend);
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
