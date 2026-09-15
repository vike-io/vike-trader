//! Pair-seam correctness gate: for every pair indicator, `on_pair` folded over
//! two aligned synthetic series must equal `vectorize_pair` on those series,
//! BIT-FOR-BIT. The 2-series twin of `parity.rs`.

use vike_indicators::pairs::pair_make_with;
use vike_indicators::{PairIndicator, pair_make, pair_registry};
use vike_model::Bar;

fn bits_eq(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() { true } else { a.to_bits() == b.to_bits() }
}

/// Two deterministic, varied, positive OHLCV series. The benchmark is a distinct
/// phase-shifted series so ratio/spread/beta/correl are non-degenerate; every
/// 19th benchmark bar is flat (exercises the zero-return / gap branches).
fn synth_pair(n: usize) -> (Vec<Bar>, Vec<Bar>) {
    let mk = |i: usize, phase: f64, base: f64, flat_mod: usize, prev: f64| -> f64 {
        let t = i as f64;
        if i > 0 && flat_mod != 0 && i.is_multiple_of(flat_mod) {
            return prev;
        }
        base + (t * 0.07 + phase).sin() * 8.0 + (t * 0.017 + phase).cos() * 4.0
    };
    let bar = |i: usize, close: f64, prev: f64| Bar {
        ts: i as i64 * 3_600_000,
        open: prev,
        high: prev.max(close) + 0.5,
        low: prev.min(close) - 0.5,
        close,
        volume: 1000.0 + (i % 13) as f64 * 50.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    let (mut a, mut b) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let (mut pa, mut pb) = (100.0f64, 50.0f64);
    for i in 0..n {
        let ca = mk(i, 0.0, 100.0, 0, pa);
        let cb = mk(i, 1.3, 50.0, 19, pb);
        a.push(bar(i, ca, pa));
        b.push(bar(i, cb, pb));
        pa = ca;
        pb = cb;
    }
    (a, b)
}

fn fold_pair(ind: &mut dyn PairIndicator, a: &[Bar], b: &[Bar]) -> Vec<Vec<f64>> {
    let mut lines: Vec<Vec<f64>> = Vec::new();
    for (pa, pb) in a.iter().zip(b.iter()) {
        let out = ind.on_pair(pa, pb);
        if lines.is_empty() {
            lines = vec![Vec::with_capacity(a.len()); out.len()];
        }
        assert_eq!(out.len(), lines.len(), "on_pair output arity must be stable");
        for (li, v) in out.iter().enumerate() {
            lines[li].push(*v);
        }
    }
    lines
}

fn assert_lines_bit_eq(name: &str, stream: &[Vec<f64>], batch: &[Vec<f64>]) {
    assert_eq!(stream.len(), batch.len(), "{name}: line count mismatch");
    for (li, (s, b)) in stream.iter().zip(batch.iter()).enumerate() {
        assert_eq!(s.len(), b.len(), "{name} line {li}: length mismatch");
        for (i, (&sv, &bv)) in s.iter().zip(b.iter()).enumerate() {
            assert!(
                bits_eq(sv, bv),
                "{name} line {li} idx {i}: stream {sv:?} ({:#018x}) != batch {bv:?} ({:#018x})",
                sv.to_bits(),
                bv.to_bits(),
            );
        }
    }
}

#[test]
fn on_pair_equals_vectorize_pair_for_every_indicator() {
    let (a, b) = synth_pair(400);
    for meta in pair_registry() {
        let mut ind = pair_make(meta.name).unwrap();
        let stream = fold_pair(ind.as_mut(), &a, &b);
        let batch = ind.vectorize_pair(&a, &b);
        assert_eq!(
            stream.len(),
            meta.outputs.len(),
            "{}: registry declares {} outputs but on_pair produced {}",
            meta.name,
            meta.outputs.len(),
            stream.len()
        );
        assert_lines_bit_eq(meta.name, &stream, &batch);
    }
}

#[test]
fn on_pair_equals_vectorize_pair_across_param_grid() {
    let (a, b) = synth_pair(300);
    for meta in pair_registry() {
        if meta.params.is_empty() {
            continue;
        }
        let defaults: Vec<f64> = meta.params.iter().map(|p| p.default).collect();
        for (pi, spec) in meta.params.iter().enumerate() {
            for &raw_v in &[spec.min, spec.default, spec.max, spec.default + spec.step] {
                let mut raw = defaults.clone();
                raw[pi] = raw_v;
                let mut ind = pair_make_with(meta.name, &raw).unwrap();
                let stream = fold_pair(ind.as_mut(), &a, &b);
                let batch = ind.vectorize_pair(&a, &b);
                assert_lines_bit_eq(
                    &format!("{} param '{}'={raw_v}", meta.name, spec.name),
                    &stream,
                    &batch,
                );
            }
        }
    }
}

#[test]
fn value_matches_last_and_reset_restores() {
    let (a, b) = synth_pair(250);
    for meta in pair_registry() {
        let mut ind = pair_make(meta.name).unwrap();
        let first = fold_pair(ind.as_mut(), &a, &b);
        let expected: Vec<f64> = first.iter().map(|l| *l.last().unwrap()).collect();
        let value = ind.value();
        assert_eq!(value.len(), expected.len(), "{}: value() arity", meta.name);
        for (i, (&v, &e)) in value.iter().zip(expected.iter()).enumerate() {
            assert!(bits_eq(v, e), "{} value()[{i}]: {v:?} != last {e:?}", meta.name);
        }
        ind.reset();
        let second = fold_pair(ind.as_mut(), &a, &b);
        assert_lines_bit_eq(meta.name, &first, &second);
    }
}

#[test]
fn pair_registry_is_complete_and_consistent() {
    let reg = pair_registry();
    assert_eq!(reg.len(), 8, "expected 8 pair indicators");
    let mut names: Vec<&str> = reg.iter().map(|m| m.name).collect();
    names.sort_unstable();
    let mut unique = names.clone();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "duplicate pair indicator name");
    for meta in reg {
        assert_eq!(pair_make(meta.name).unwrap().name(), meta.name, "make/name mismatch");
    }
    assert!(pair_make("does-not-exist").is_none());
}
