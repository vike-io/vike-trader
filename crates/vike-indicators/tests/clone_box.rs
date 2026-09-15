//! `Indicator::clone_box` — cloneable streaming state for speculative evaluation.
//!
//! Contract: cloning a folded indicator yields an INDEPENDENT instance whose
//! future outputs are bit-identical to the original's when fed the same bars,
//! and whose mutations never leak back into the original. This is what lets a
//! GUI (or backtest) evaluate a live *forming* bar without committing it to the
//! persistent fold.

use vike_indicators::registry;
use vike_model::Bar;

fn mk_bar(i: usize) -> Bar {
    let base = 100.0 + (i as f64 * 0.7).sin() * 5.0;
    Bar {
        ts: 1_700_000_000_000 + i as i64 * 60_000,
        open: base,
        high: base + 1.0,
        low: base - 1.0,
        close: base + 0.4,
        volume: 1000.0 + i as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// For every registered indicator: fold N bars, clone, then (a) same next bar →
/// bit-identical output, (b) mutating the clone leaves the original untouched.
#[test]
fn clone_box_is_independent_and_bit_identical() {
    let bars: Vec<Bar> = (0..64).map(mk_bar).collect();
    let next = mk_bar(64);
    let other = mk_bar(65);

    for meta in registry() {
        let mut orig = meta.build();
        for b in &bars {
            orig.on_bar(b);
        }

        // (a) clone produces the same next output, bit-for-bit
        let mut clone = orig.clone_box();
        let v_clone: Vec<f64> = clone.on_bar(&next);
        // (b) advancing the clone must NOT have advanced the original: feeding the
        // original the same bar now must reproduce the clone's output exactly.
        let v_orig: Vec<f64> = orig.on_bar(&next);
        assert_eq!(v_orig.len(), v_clone.len(), "{}: output arity diverged after clone", meta.name);
        for (k, (a, b)) in v_orig.iter().zip(v_clone.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "{}: output[{k}] diverged: orig {a} vs clone {b}",
                meta.name
            );
        }

        // (c) further independent evolution: feed DIFFERENT bars, states must not alias.
        let v1 = clone.on_bar(&other);
        let v2 = orig.on_bar(&next); // original sees `next` twice, clone saw next+other
        // No assertion on values (they legitimately differ) — just prove both still
        // produce well-formed output of the registered arity.
        assert_eq!(v1.len(), meta.outputs.len(), "{}: clone arity", meta.name);
        assert_eq!(v2.len(), meta.outputs.len(), "{}: orig arity", meta.name);
    }
}
