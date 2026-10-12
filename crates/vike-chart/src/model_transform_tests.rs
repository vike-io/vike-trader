//! The transform-style recompute cache (chart-perf T6): every cached path equals a full transform.

use super::*;

// === chart-perf T6: transform-style recompute cache ===================================

const T6_PARAMS: TransformParams = TransformParams { line_break_n: 3, pnf_reversal: 3 };

/// A render `Bar` with a ±1 range around `close` — one-minute epoch like [`bar`] above.
fn tbar(i: usize, close: f64) -> Bar {
    Bar {
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
/// full (closed + forming) series — independent of [`ChartState::transformed`]'s internals.
fn oracle_transform(
    style: ChartStyle,
    params: TransformParams,
    bars: &[Bar],
    first_ot: i64,
) -> Vec<Bar> {
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
                .map(|&p| Bar { t: 0.0, ot: first_ot, o: p, h: p, l: p, c: p, v: 0.0 })
                .collect(),
        ),
        PointFigure => {
            let (cols, _) = transforms::point_and_figure(bars, params.pnf_reversal);
            transforms::reindex(
                cols.iter()
                    .map(|c| Bar {
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
fn assert_bars_bit_identical(got: &[Bar], want: &[Bar], label: &str) {
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

fn set_series(cs: &mut ChartState, bars: &[Bar], closed_len: usize) {
    cs.bars = bars.to_vec();
    cs.closed_len = closed_len;
    cs.refresh_caches();
}

/// Install `(bars, closed_len)`, run `transformed`, and assert it is byte-identical to the
/// oracle full-transform of (closed + forming).
fn drive_and_check(
    cs: &mut ChartState,
    bars: &[Bar],
    closed_len: usize,
    style: ChartStyle,
    label: &str,
) {
    set_series(cs, bars, closed_len);
    let got = cs.transformed(style, T6_PARAMS);
    let first_ot = cs.bars.first().map_or(0, |b| b.ot);
    let want = oracle_transform(style, T6_PARAMS, &cs.bars, first_ot);
    assert_bars_bit_identical(&got, &want, label);
}

/// THE gate: for EACH of the six transform styles, `transformed` is byte-identical to a full
/// recompute of (closed + forming) at every step of a realistic tick sequence — seed 300
/// closed + forming, five forming-only ticks, a bar close, another forming tick, then five
/// bars closing at once.
#[test]
fn transformed_matches_full_recompute_per_style() {
    use ChartStyle::*;
    for style in [HeikinAshi, Renko, Range, LineBreak, Kagi, PointFigure] {
        let mut cs = ChartState::default();

        // seed: 300 closed bars (0..299) + forming (300)
        let mut series: Vec<Bar> = (0..=300).map(|i| tbar(i, close_at(i))).collect();
        drive_and_check(&mut cs, &series, 300, style, &format!("{style:?} seed"));
        assert!(
            !cs.transformed(style, T6_PARAMS).is_empty(),
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
        let mut series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: seed builds the closed prefix once"
        );

        // static repeat (identical state) → O(1) cache hit, no bump
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: static repeat is a cache hit"
        );

        // forming ticks → closed prefix unchanged → reuse, no bump
        for k in 0..4 {
            series[200] = tbar(200, close_at(200) + (k as f64 + 1.0) * 0.6);
            set_series(&mut cs, &series, 200);
            cs.transformed(style, T6_PARAMS);
        }
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: forming ticks reuse the closed prefix"
        );

        // bar close → closed prefix changes → bump
        series.push(tbar(201, close_at(201)));
        set_series(&mut cs, &series, 201);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            2,
            "{style:?}: a bar close rebuilds the closed prefix"
        );

        // param change → different key → bump
        cs.transformed(style, TransformParams { line_break_n: 2, pnf_reversal: 3 });
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            3,
            "{style:?}: a param change rebuilds the closed prefix"
        );

        // style change → different key → bump
        cs.transformed(Candles, T6_PARAMS); // non-transform style: still a key change
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            4,
            "{style:?}: a style change rebuilds"
        );
    }

    // fallback: static frame cached, forming tick reprocesses.
    for style in [Renko, Range, Kagi, PointFigure] {
        let mut cs = ChartState::default();
        let mut series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(cs.transform_closed_recompute_count.get(), 1, "{style:?}: seed recompute");

        // static repeat → cache hit, no bump (the win the fallback still delivers)
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: static repeat is a cache hit"
        );

        // forming tick → global auto_box shifts → must reprocess → bump
        series[200] = tbar(200, close_at(200) + 3.0);
        set_series(&mut cs, &series, 200);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            2,
            "{style:?}: a forming tick reprocesses (global auto_box)"
        );
    }
}

/// The Kagi/PnF structured results the custom drawing reads (`cached_{kagi,pnf}_shared`) are
/// populated by `transformed` and byte-identical to a fresh `transforms::{kagi,point_and_figure}`
/// over (closed + forming) — the data-flow the `chart.rs` `draw_kagi`/`draw_pnf` rewire relies
/// on. Each style leaves the OTHER structured slot empty.
#[test]
fn cached_structured_matches_fresh_compute() {
    let mut cs = ChartState::default();
    let series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
    set_series(&mut cs, &series, 200);

    cs.transformed(ChartStyle::Kagi, T6_PARAMS);
    let k = cs.cached_kagi_shared().expect("Kagi style populates the structured Kagi cache");
    let fresh = transforms::kagi(&cs.bars);
    assert_eq!(k.prices.len(), fresh.prices.len(), "kagi price count");
    for (a, b) in k.prices.iter().zip(&fresh.prices) {
        assert_eq!(a.to_bits(), b.to_bits(), "kagi price bits");
    }
    assert_eq!(k.thick, fresh.thick, "kagi thick flags");
    assert!(cs.cached_pnf_shared().is_none(), "Kagi style leaves the PnF slot empty");

    cs.transformed(ChartStyle::PointFigure, T6_PARAMS);
    let pnf = cs.cached_pnf_shared().expect("PnF style populates the structured PnF cache");
    let (cols, box_) = transforms::point_and_figure(&cs.bars, T6_PARAMS.pnf_reversal);
    assert_eq!(pnf.1.to_bits(), box_.to_bits(), "pnf box bits");
    assert_eq!(pnf.0.len(), cols.len(), "pnf column count");
    for (a, b) in pnf.0.iter().zip(&cols) {
        assert_eq!(a.up, b.up, "pnf column up flag");
        assert_eq!(a.top.to_bits(), b.top.to_bits(), "pnf column top bits");
        assert_eq!(a.bottom.to_bits(), b.bottom.to_bits(), "pnf column bottom bits");
    }
    assert!(cs.cached_kagi_shared().is_none(), "PnF style leaves the Kagi slot empty");
}
