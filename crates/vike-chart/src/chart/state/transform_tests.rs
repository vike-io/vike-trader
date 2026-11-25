//! The transform-style recompute cache (chart-perf T6): every cached path equals a full transform.

use super::*;

// === chart-perf T6: transform-style recompute cache ===================================

const T6_PARAMS: ChartStyleTransformParams =
    ChartStyleTransformParams { line_break_n: 3, point_figure_reversal: 3 };

/// A render `ChartBar` with a ±1 range around `close` — one-minute epoch like [`bar`] above.
fn tbar(i: usize, close: f64) -> ChartBar {
    ChartBar {
        t: i as f64,
        ot: 1_700_000_040_000 + i as i64 * 60_000,
        o: close,
        h: close + 1.0,
        l: close - 1.0,
        c: close,
        v: 100.0 + i as f64,
    }
}

/// Trend + oscillation so every transform (Renko/Range/LineBreak/Kagi/PnF) sees real
/// breakouts against its `auto_box`(~2.0, since h-l == 2) — otherwise the transforms return
/// empty vecs and the equivalence gate would pass vacuously.
fn close_at(i: usize) -> f64 {
    100.0 + (i as f64 * 0.05).sin() * 8.0 + i as f64 * 0.03
}

/// The oracle: exactly how `chart.rs` builds the `owned` proxy for each transform style over a
/// full (closed + forming) series — independent of [`ChartState::transformed_shared`]'s internals.
fn oracle_transform(
    style: ChartStyle,
    params: ChartStyleTransformParams,
    bars: &[ChartBar],
    first_ot: i64,
) -> Vec<ChartBar> {
    use ChartStyle::*;
    match style {
        HeikinAshi => heikin_ashi(bars),
        Renko => transforms::reindex(transforms::renko(bars)),
        Range => transforms::reindex(transforms::range_bars(bars)),
        LineBreak => transforms::reindex(transforms::line_break(bars, params.line_break_n)),
        Kagi => transforms::reindex(
            transforms::kagi(bars)
                .prices
                .iter()
                .map(|&p| ChartBar { t: 0.0, ot: first_ot, o: p, h: p, l: p, c: p, v: 0.0 })
                .collect(),
        ),
        PointFigure => {
            let (cols, _) = transforms::point_and_figure(bars, params.point_figure_reversal);
            transforms::reindex(
                cols.iter()
                    .map(|c| ChartBar {
                        t: 0.0,
                        ot: first_ot,
                        o: c.bottom,
                        h: c.top,
                        l: c.bottom,
                        c: c.top,
                        v: 0.0,
                    })
                    .collect(),
            )
        }
        _ => vec![],
    }
}

/// Byte/value-identical assertion — every f64 field compared by `to_bits`, not `==`.
fn assert_bars_bit_identical(got: &[ChartBar], want: &[ChartBar], label: &str) {
    assert_eq!(got.len(), want.len(), "{label}: length {} != oracle {}", got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g.t.to_bits(), w.t.to_bits(), "{label}[{i}].t");
        assert_eq!(g.ot, w.ot, "{label}[{i}].ot");
        assert_eq!(g.o.to_bits(), w.o.to_bits(), "{label}[{i}].o");
        assert_eq!(g.h.to_bits(), w.h.to_bits(), "{label}[{i}].h");
        assert_eq!(g.l.to_bits(), w.l.to_bits(), "{label}[{i}].l");
        assert_eq!(g.c.to_bits(), w.c.to_bits(), "{label}[{i}].c");
        assert_eq!(g.v.to_bits(), w.v.to_bits(), "{label}[{i}].v");
    }
}

fn set_series(cs: &mut ChartState, bars: &[ChartBar], closed_len: usize) {
    cs.bars = bars.to_vec();
    cs.closed_len = closed_len;
    cs.refresh_caches();
}

/// Install `(bars, closed_len)`, run `transformed_shared`, and assert it is byte-identical to the
/// oracle full-transform of (closed + forming).
fn drive_and_check(
    cs: &mut ChartState,
    bars: &[ChartBar],
    closed_len: usize,
    style: ChartStyle,
    label: &str,
) {
    set_series(cs, bars, closed_len);
    let got = cs.transformed_shared(style, T6_PARAMS);
    let first_ot = cs.bars.first().map_or(0, |b| b.ot);
    let want = oracle_transform(style, T6_PARAMS, &cs.bars, first_ot);
    assert_bars_bit_identical(&got, &want, label);
}

/// THE gate: for EACH of the six transform styles, `transformed_shared` is byte-identical to a full
/// recompute of (closed + forming) at every step of a realistic tick sequence — seed 300
/// closed + forming, five forming-only ticks, a bar close, another forming tick, then five
/// bars closing at once.
#[test]
fn transformed_matches_full_recompute_per_style() {
    use ChartStyle::*;
    for style in [HeikinAshi, Renko, Range, LineBreak, Kagi, PointFigure] {
        let mut cs = ChartState::default();

        // seed: 300 closed bars (0..299) + forming (300)
        let mut series: Vec<ChartBar> = (0..=300).map(|i| tbar(i, close_at(i))).collect();
        drive_and_check(&mut cs, &series, 300, style, &format!("{style:?} seed"));
        assert!(
            !cs.transformed_shared(style, T6_PARAMS).is_empty(),
            "{style:?}: expected a non-empty transform (the gate must not pass vacuously)"
        );

        // forming-tick x5: mutate the forming bar (index 300) only
        for k in 0..5 {
            series[300] = tbar(300, close_at(300) + (k as f64 + 1.0) * 0.7);
            drive_and_check(&mut cs, &series, 300, style, &format!("{style:?} forming-tick {k}"));
        }

        // close 1: bar 300 closes, new forming 301
        series.push(tbar(301, close_at(301)));
        drive_and_check(&mut cs, &series, 301, style, &format!("{style:?} close 1"));

        // forming-tick after close 1
        series[301] = tbar(301, close_at(301) + 1.3);
        drive_and_check(
            &mut cs,
            &series,
            301,
            style,
            &format!("{style:?} forming-tick after close 1"),
        );

        // close 5 at once: bars 302..=306 append, closed prefix 301 -> 306, forming 306
        for i in 302..=306 {
            series.push(tbar(i, close_at(i)));
        }
        drive_and_check(&mut cs, &series, 306, style, &format!("{style:?} close 5"));
    }
}

/// The work counter: closed-prefix recomputes bump only on bar-close / style / param change
/// for the TWO-TIER styles (HeikinAshi/LineBreak) and never on a forming tick or a static
/// frame; the FALLBACK styles (Renko/Range/Kagi/PnF) serve a static frame from cache (no bump)
/// but reprocess on a forming tick (their `auto_box` reads the forming bar) — the documented
/// residual cost.
#[test]
fn transformed_recomputes_only_tail() {
    use ChartStyle::*;

    // two-tier: forming ticks reuse the cached closed prefix.
    for style in [HeikinAshi, LineBreak] {
        let mut cs = ChartState::default();
        let mut series: Vec<ChartBar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);
        cs.transformed_shared(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: seed builds the closed prefix once"
        );

        // static repeat (identical state) → O(1) cache hit, no bump
        cs.transformed_shared(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: static repeat is a cache hit"
        );

        // forming ticks → closed prefix unchanged → reuse, no bump
        for k in 0..4 {
            series[200] = tbar(200, close_at(200) + (k as f64 + 1.0) * 0.6);
            set_series(&mut cs, &series, 200);
            cs.transformed_shared(style, T6_PARAMS);
        }
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: forming ticks reuse the closed prefix"
        );

        // bar close → closed prefix changes → bump
        series.push(tbar(201, close_at(201)));
        set_series(&mut cs, &series, 201);
        cs.transformed_shared(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            2,
            "{style:?}: a bar close rebuilds the closed prefix"
        );

        // param change → different key → bump
        cs.transformed_shared(
            style,
            ChartStyleTransformParams { line_break_n: 2, point_figure_reversal: 3 },
        );
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            3,
            "{style:?}: a param change rebuilds the closed prefix"
        );

        // style change → different key → bump
        cs.transformed_shared(Candles, T6_PARAMS); // non-transform style: still a key change
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            4,
            "{style:?}: a style change rebuilds"
        );
    }

    // fallback: static frame cached, forming tick reprocesses.
    for style in [Renko, Range, Kagi, PointFigure] {
        let mut cs = ChartState::default();
        let mut series: Vec<ChartBar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);
        cs.transformed_shared(style, T6_PARAMS);
        assert_eq!(cs.transform_closed_recompute_count.get(), 1, "{style:?}: seed recompute");

        // static repeat → cache hit, no bump (the win the fallback still delivers)
        cs.transformed_shared(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: static repeat is a cache hit"
        );

        // forming tick → global auto_box shifts → must reprocess → bump
        series[200] = tbar(200, close_at(200) + 3.0);
        set_series(&mut cs, &series, 200);
        cs.transformed_shared(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            2,
            "{style:?}: a forming tick reprocesses (global auto_box)"
        );
    }
}

/// The Kagi/PnF structured results the custom drawing reads (`cached_{kagi,pnf}_shared`) are
/// populated by `transformed_shared` and byte-identical to a fresh `transforms::{kagi,point_and_figure}`
/// over (closed + forming) — the data-flow the `chart.rs` `draw_kagi`/`draw_pnf` rewire relies
/// on. Each style leaves the OTHER structured slot empty.
#[test]
fn cached_structured_matches_fresh_compute() {
    let mut cs = ChartState::default();
    let series: Vec<ChartBar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
    set_series(&mut cs, &series, 200);

    cs.transformed_shared(ChartStyle::Kagi, T6_PARAMS);
    let k = cs.cached_kagi_shared().expect("Kagi style populates the structured Kagi cache");
    let fresh = transforms::kagi(&cs.bars);
    assert_eq!(k.prices.len(), fresh.prices.len(), "kagi price count");
    for (a, b) in k.prices.iter().zip(&fresh.prices) {
        assert_eq!(a.to_bits(), b.to_bits(), "kagi price bits");
    }
    assert_eq!(k.thick, fresh.thick, "kagi thick flags");
    assert!(cs.cached_pnf_shared().is_none(), "Kagi style leaves the PnF slot empty");

    cs.transformed_shared(ChartStyle::PointFigure, T6_PARAMS);
    let pnf = cs.cached_pnf_shared().expect("PnF style populates the structured PnF cache");
    let (cols, box_) = transforms::point_and_figure(&cs.bars, T6_PARAMS.point_figure_reversal);
    assert_eq!(pnf.1.to_bits(), box_.to_bits(), "pnf box bits");
    assert_eq!(pnf.0.len(), cols.len(), "pnf column count");
    for (a, b) in pnf.0.iter().zip(&cols) {
        assert_eq!(a.up, b.up, "pnf column up flag");
        assert_eq!(a.top.to_bits(), b.top.to_bits(), "pnf column top bits");
        assert_eq!(a.bottom.to_bits(), b.bottom.to_bits(), "pnf column bottom bits");
    }
    assert!(cs.cached_kagi_shared().is_none(), "PnF style leaves the Kagi slot empty");
}

// === the chart's price maths are the indicators' (one copy, decision 0294) ===================

/// Every awkward value a price can take: both zeros, `NaN`, both infinities, a subnormal, a huge
/// one, and two plain prices.
const AWKWARD: [f64; 9] =
    [0.0, -0.0, 1.5, -2.25, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 7.0e-310, 1.0e308];

/// One bar for each way of putting [`AWKWARD`] values in the four prices, at bar `i`'s place.
fn awkward_bars(i: usize) -> impl Iterator<Item = ChartBar> {
    let n = AWKWARD.len();
    (0..n.pow(4)).map(move |k| {
        let pick = |field: u32| AWKWARD[k / n.pow(field) % n];
        ChartBar { o: pick(0), h: pick(1), l: pick(2), c: pick(3), ..tbar(i, 0.0) }
    })
}

/// The chart's Heikin-Ashi, batch and two-tier (closed prefix + forming bar), is the indicators'
/// `heikin_ashi` bit for bit on the chart's own bars: an awkward bar first, in the middle and
/// forming, so a `NaN` or an infinity reaches the first open, a later open and the forming tail.
#[test]
fn the_chart_heikin_ashi_is_the_indicators_bits() {
    let at = |i: usize, odd: ChartBar| ChartBar { t: i as f64, ot: tbar(i, 0.0).ot, ..odd };
    for odd in awkward_bars(1) {
        for bars in [
            vec![at(0, odd), tbar(1, close_at(1)), tbar(2, close_at(2))],
            vec![tbar(0, close_at(0)), odd, tbar(2, close_at(2))],
            vec![tbar(0, close_at(0)), tbar(1, close_at(1)), at(2, odd)],
        ] {
            let column = |f: fn(&ChartBar) -> f64| bars.iter().map(f).collect::<Vec<f64>>();
            let [o, h, l, c] = vike_indicators::series::heikin_ashi(
                &column(|b| b.o),
                &column(|b| b.h),
                &column(|b| b.l),
                &column(|b| b.c),
            );
            let want: Vec<ChartBar> = bars
                .iter()
                .enumerate()
                .map(|(i, b)| ChartBar { o: o[i], h: h[i], l: l[i], c: c[i], ..*b })
                .collect();
            assert_bars_bit_identical(&heikin_ashi(&bars), &want, "batch");
            let mut cs = ChartState::default();
            set_series(&mut cs, &bars, bars.len() - 1);
            assert_bars_bit_identical(
                &cs.transformed_shared(ChartStyle::HeikinAshi, T6_PARAMS),
                &want,
                "two-tier",
            );
        }
    }
}

/// The study sources `hl2`, `hlc3` and `ohlc4` are the indicators' `medprice`, `typprice` and
/// `avgprice` bit for bit on every awkward bar.
#[test]
fn the_chart_study_sources_are_the_indicators_bits() {
    use crate::chart::study::ChartStudySource;
    use vike_indicators::series::{avgprice, medprice, typprice};
    for b in awkward_bars(0) {
        let pairs = [
            (ChartStudySource::Hl2, medprice(&[b.h], &[b.l])[0]),
            (ChartStudySource::Hlc3, typprice(&[b.h], &[b.l], &[b.c])[0]),
            (ChartStudySource::Ohlc4, avgprice(&[b.o], &[b.h], &[b.l], &[b.c])[0]),
        ];
        for (source, want) in pairs {
            let got = source.value(&b);
            assert_eq!(got.to_bits(), want.to_bits(), "{source:?} of {b:?}: {got} != {want}");
        }
    }
}
