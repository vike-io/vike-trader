//! A fingerprint of what every registry indicator computes, for comparing two versions of the crate.
//!
//! `#[ignore]`d and asserting nothing: it prints one line per (indicator, parameter set) —
//! `name|set|hash` — where the hash covers every bit of `vectorize` over a fixed adversarial series
//! (NaNs are folded to one pattern, because a NaN's payload is not a value). Run it on two commits
//! and `diff` the sorted outputs: a changed hash on an indicator both commits have means its
//! numbers moved. The file uses only the long-stable public API and a `Bar` struct literal, so it
//! compiles on a commit that predates `Bar::ohlcv` and `compute`.
//!
//! `cargo test -p vike-indicators --release --test vectorize_hash -- --ignored --nocapture`

use vike_indicators::{make_with, registry};
use vike_marketdata::Bar;

/// FNV-1a over the bits, NaN canonicalised.
fn hash(cols: &[Vec<f64>]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for col in cols {
        h ^= col.len() as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        for v in col {
            let bits = if v.is_nan() { 0x7ff8_0000_0000_0000 } else { v.to_bits() };
            h ^= bits;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// 1,200 bars: a trend plus waves, a flat stretch, a zero-volume stretch, two gaps, one zero-range
/// bar and one bar closing at zero.
fn series() -> Vec<Bar> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut price = 100.0_f64;
    (0..1200usize)
        .map(|i| {
            let x = i as f64;
            let flat = (300..340).contains(&i);
            let step = if flat { 0.0 } else { (next() - 0.5) * 2.0 + 0.4 * (x * 0.05).sin() };
            price = (price + step).max(1.0);
            if i == 700 {
                price *= 2.5;
            }
            if i == 900 {
                price *= 0.3;
            }
            let open = price - 0.3 * (x * 0.4).sin();
            let (high, low) = if i == 500 || flat {
                (price, price)
            } else {
                (price.max(open) + 0.5 + next(), price.min(open) - 0.5 - next())
            };
            let close = if i == 1100 { 0.0 } else { price };
            let volume = if (600..650).contains(&i) { 0.0 } else { 500.0 + 400.0 * next() };
            Bar {
                ts: i as i64 * 60_000,
                open,
                high,
                low,
                close,
                volume,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

#[test]
#[ignore = "a fingerprint printer, run by hand on two commits"]
fn print_vectorize_fingerprints() {
    let bars = series();
    for meta in registry() {
        let sets: [(&str, Vec<f64>); 3] = [
            ("default", Vec::new()),
            ("min", meta.params.iter().map(|p| p.min).collect()),
            ("max", meta.params.iter().map(|p| p.max).collect()),
        ];
        for (label, raw) in sets {
            let ind = make_with(meta.name, &raw).expect("a registry name builds");
            let cols = ind.vectorize(&bars);
            println!("FP|{}|{}|{:016x}", meta.name, label, hash(&cols));
        }
    }
}
