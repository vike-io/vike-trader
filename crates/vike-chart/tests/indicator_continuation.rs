//! A chart indicator seeded over a long history and then handed ONE more bar must answer with the
//! number a full `vectorize` over the whole series gives for that bar.
//!
//! `Active::recompute_full` seeds a `trims_history()` indicator from the retained TAIL of the
//! history, which is only sound for a bounded-window kernel; a cumulative indicator seeded that way
//! continues from the wrong state. The series it already drew is right (it comes from `vectorize`),
//! so the fault would show only on the NEXT live bar. This test is the measurement: the history is
//! well past the old 512-bar trim threshold, and the controls (`sma`, `rsi`, `adx`, `obv`) are the
//! indicators that were never in question.

use vike_chart::indicators::{Active, get_any};
use vike_chart::model::Bar;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let x = i as f64;
            let c = 100.0 + 8.0 * (x * 0.07).sin() + x * 0.01 + 0.5 * (x * 1.9).cos();
            let o = c - 0.3 * (x * 0.5).sin();
            Bar {
                t: x,
                ot: i as i64 * 60_000,
                o,
                h: c.max(o) + 0.6 + 0.2 * (x * 0.31).sin().abs(),
                l: c.min(o) - 0.6 - 0.2 * (x * 0.17).cos().abs(),
                c,
                v: 1000.0 + 300.0 * (x * 0.23).sin() + (i % 11) as f64 * 17.0,
            }
        })
        .collect()
}

fn model(b: &Bar) -> vike_marketdata::Bar {
    vike_marketdata::Bar {
        ts: b.ot,
        open: b.o,
        high: b.h,
        low: b.l,
        close: b.c,
        volume: b.v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn one_more_bar_after_a_long_history_equals_the_full_vectorize() {
    const N: usize = 3000;
    let all = bars(N + 1);
    let truth_bars: Vec<vike_marketdata::Bar> = all.iter().map(model).collect();
    let mut wrong: Vec<String> = Vec::new();
    for name in [
        // the cumulative / path-dependent set
        "ad",
        "adosc",
        "asi",
        "mcginley",
        "net_volume",
        "nvi",
        "pvi",
        "pvt",
        // controls: hand-written incremental and bounded-window kernels
        "obv",
        "vwap",
        "psar",
        "sma",
        "rsi",
        "adx",
        "connors_rsi",
        "dpo",
    ] {
        let spec = get_any(name).unwrap_or_else(|| panic!("no registry row {name}"));
        let mut active = Active::new(1, spec, &all[..N]);
        active.push_bar(&all[N]);
        let truth = spec.build().vectorize(&truth_bars);
        for (k, line) in active.outputs.iter().enumerate() {
            let got = *line.series.last().expect("a value for the pushed bar");
            let want = truth[k][N];
            let same = got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan());
            if !same {
                wrong.push(format!(
                    "{name}.{}: pushed bar gives {got}, full vectorize {want}",
                    line.name
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "continuation differs from the full series:\n{}", wrong.join("\n"));
}
