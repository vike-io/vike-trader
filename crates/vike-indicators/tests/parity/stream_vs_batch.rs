//! The headline gate at default length: `on_bar` folded equals `vectorize`, plus value/reset/shape.

use vike_indicators::{make, registry};

use super::{assert_lines_bit_eq, bits_eq, fold_stream, synth_bars};

/// The headline gate: every registered indicator, on_bar-fold == vectorize, bitwise.
#[test]
fn on_bar_equals_vectorize_for_every_indicator() {
    let bars = synth_bars(400);
    for meta in registry() {
        // batch_only indicators (ichimoku/zigzag/williams_fractal) read future
        // bars in their batch — their streaming `on_bar` is a causal best-effort
        // and cannot bit-match `vectorize`. `vectorize` shape is still gated by
        // `vectorize_shape_matches_input`.
        if meta.batch_only {
            continue;
        }
        let mut ind = make(meta.name).unwrap();
        let stream = fold_stream(ind.as_mut(), &bars);
        let batch = ind.vectorize(&bars);
        assert_eq!(
            stream.len(),
            meta.outputs.len(),
            "{}: registry declares {} outputs but on_bar produced {}",
            meta.name,
            meta.outputs.len(),
            stream.len()
        );
        assert_lines_bit_eq(meta.name, &stream, &batch);
    }
}

/// `value()` returns the last streamed output, unchanged, for every indicator.
#[test]
fn value_matches_last_streamed_output() {
    let bars = synth_bars(300);
    for meta in registry() {
        let mut ind = make(meta.name).unwrap();
        let stream = fold_stream(ind.as_mut(), &bars);
        let expected: Vec<f64> = stream.iter().map(|l| *l.last().unwrap()).collect();
        let value = ind.value();
        assert_eq!(value.len(), expected.len(), "{}: value() arity", meta.name);
        for (i, (&v, &e)) in value.iter().zip(expected.iter()).enumerate() {
            assert!(bits_eq(v, e), "{} value()[{i}]: {v:?} != last on_bar {e:?}", meta.name);
        }
    }
}

/// `reset()` returns an indicator to its initial behaviour (fold again == first fold).
#[test]
fn reset_restores_initial_behaviour() {
    let bars = synth_bars(250);
    for meta in registry() {
        let mut ind = make(meta.name).unwrap();
        let first = fold_stream(ind.as_mut(), &bars);
        ind.reset();
        let second = fold_stream(ind.as_mut(), &bars);
        assert_lines_bit_eq(meta.name, &first, &second);
    }
}

/// `vectorize` yields one value per bar per output line, for every indicator.
#[test]
fn vectorize_shape_matches_input() {
    for len in [0usize, 1, 5, 33, 60, 200] {
        let bars = synth_bars(len);
        for meta in registry() {
            let ind = make(meta.name).unwrap();
            let series = ind.vectorize(&bars);
            assert_eq!(
                series.len(),
                meta.outputs.len(),
                "{} len {len}: output line count",
                meta.name
            );
            for line in &series {
                assert_eq!(line.len(), len, "{} len {len}: line length", meta.name);
            }
        }
    }
}

/// The registry is complete, uniquely named, and `make` round-trips names.
#[test]
fn registry_is_complete_and_consistent() {
    let reg = registry();
    assert_eq!(
        reg.len(),
        171,
        "expected 171 single-series: 17 base + 4 price + 29 momentum + 12 volatility \
         + 17 overlap + 12 volume + 13 statistics + 4 structure + 63 patterns"
    );
    let mut names: Vec<&str> = reg.iter().map(|m| m.name).collect();
    names.sort_unstable();
    let unique = {
        let mut u = names.clone();
        u.dedup();
        u.len()
    };
    assert_eq!(unique, names.len(), "duplicate indicator name in registry");
    for meta in reg {
        let ind = make(meta.name).unwrap();
        assert_eq!(ind.name(), meta.name, "make/name mismatch");
    }
    assert!(make("does-not-exist").is_none());
}
