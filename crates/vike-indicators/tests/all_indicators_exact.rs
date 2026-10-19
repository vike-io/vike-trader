//! EVERY registry indicator, folded one bar at a time, equals its batch computation bit for bit —
//! at the smallest, the default and the largest value of every parameter, on three awkward series.
//!
//! `tests/parity.rs` holds the same contract at the registry defaults on one series. This is the
//! wider net: parameter extremes (where windows are 2 bars or 400, and where a streamer's ring
//! sizes are most likely to be wrong) and series built to hit the degenerate branches — a flat
//! stretch, a zero-volume stretch, a bar with no range, a close of exactly zero, price gaps, and a
//! price level a thousand times larger. It also checks the shape every consumer relies on: one
//! output line per declared output, each as long as the input, and a clone that carries on
//! identically.
//!
//! The three lines of a `batch_only` indicator that read the future (`ichimoku`'s chikou, `zigzag`,
//! `williams_fractal`) are skipped, as in `tests/parity.rs`: `vectorize` is authoritative there.

use vike_indicators::{make_with, registry};
use vike_marketdata::Bar;

fn lcg(seed: u64) -> impl FnMut() -> f64 {
    let mut state = seed;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// `shape`: 0 = ordinary, 1 = degenerate stretches, 2 = large price level with tiny ranges.
fn series(shape: u8, n: usize) -> Vec<Bar> {
    let mut next = lcg(0x9e37_79b9_7f4a_7c15 ^ u64::from(shape));
    let scale = if shape == 2 { 50_000.0 } else { 100.0 };
    let mut price = scale;
    (0..n)
        .map(|i| {
            let x = i as f64;
            let flat = shape == 1 && (100..140).contains(&i);
            let step = if flat {
                0.0
            } else {
                ((next() - 0.5) * 2.0 + 0.4 * (x * 0.05).sin()) * scale / 100.0
            };
            price = (price + step).max(scale / 100.0);
            if shape == 1 && i == 250 {
                price *= 2.5;
            }
            if shape == 1 && i == 300 {
                price *= 0.3;
            }
            let open = price - 0.3 * (x * 0.4).sin() * scale / 100.0;
            let wick = if shape == 2 { 0.0001 * scale } else { 0.5 * scale / 100.0 };
            let (high, low) = if flat || (shape == 1 && i == 200) {
                (price, price)
            } else {
                (price.max(open) + wick + next() * wick, price.min(open) - wick - next() * wick)
            };
            let close = if shape == 1 && i == 320 { 0.0 } else { price };
            let volume =
                if shape == 1 && (150..190).contains(&i) { 0.0 } else { 500.0 + 400.0 * next() };
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

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// The registry is cut into this many shards, one `#[test]` each (indicator `i` belongs to shard
/// `i % SHARDS`, so every indicator is in exactly one and the shards are about equally heavy).
///
/// Why shards and not a smaller input: the work is pure CPU in a debug build (~0.6 s on a quiet
/// lane, 203 indicators), so on a box running many tests at once its wall time is the box's
/// contention times that, and once it ran past nextest's 240 s per-test kill (CI, 2026-10-14). Four
/// tests run side by side and are killed separately, so each one is a quarter of the exposure and
/// the same input still pins the property.
const SHARDS: usize = 4;

#[test]
fn every_indicator_streams_exactly_what_it_computes_in_batch_shard_0() {
    streams_exactly_what_it_computes_in_batch(0);
}

#[test]
fn every_indicator_streams_exactly_what_it_computes_in_batch_shard_1() {
    streams_exactly_what_it_computes_in_batch(1);
}

#[test]
fn every_indicator_streams_exactly_what_it_computes_in_batch_shard_2() {
    streams_exactly_what_it_computes_in_batch(2);
}

#[test]
fn every_indicator_streams_exactly_what_it_computes_in_batch_shard_3() {
    streams_exactly_what_it_computes_in_batch(3);
}

fn streams_exactly_what_it_computes_in_batch(shard: usize) {
    // The default parameters run on the long series; the smallest and largest parameter sets on a
    // short one. The degenerate events of series 1 (flat, zero volume, no-range bar, gaps, a zero
    // close) all sit in its first 330 bars. This test runs in CI's debug build, where the
    // history-recompute indicators at their largest periods dominate; the sizes keep it under a
    // minute there (a longer series ran past nextest's 240 s limit).
    const N_DEFAULT: usize = 900;
    const N_EXTREME: usize = 360;
    let long: Vec<Vec<Bar>> = (0..3u8).map(|s| series(s, N_DEFAULT)).collect();
    let short: Vec<Vec<Bar>> = (0..3u8).map(|s| series(s, N_EXTREME)).collect();
    let mut failures: Vec<String> = Vec::new();
    let mut folded = 0usize;

    let mine = registry().iter().enumerate().filter(|(i, _)| i % SHARDS == shard);
    let mut owned = 0usize;
    for (_, meta) in mine {
        owned += 1;
        let sets: [(&str, Vec<f64>); 3] = [
            ("default", Vec::new()),
            ("min", meta.params.iter().map(|p| p.min).collect()),
            ("max", meta.params.iter().map(|p| p.max).collect()),
        ];
        for (label, raw) in &sets {
            let all = if *label == "default" { &long } else { &short };
            for (shape, bars) in all.iter().enumerate() {
                let tag = format!("{} [{label}] series {shape}", meta.name);
                let mut ind = make_with(meta.name, raw).expect("a registry name builds");
                let batch = ind.vectorize(bars);
                if batch.len() != meta.outputs.len() {
                    failures.push(format!(
                        "{tag}: {} lines, the row declares {}",
                        batch.len(),
                        meta.outputs.len()
                    ));
                    continue;
                }
                if let Some(short) = batch.iter().position(|c| c.len() != bars.len()) {
                    failures.push(format!(
                        "{tag}: line {short} has {} values for {} bars",
                        batch[short].len(),
                        bars.len()
                    ));
                    continue;
                }
                // A clone taken halfway must carry on exactly like the original.
                let mut twin = None;
                for (i, bar) in bars.iter().enumerate() {
                    if i == bars.len() / 2 {
                        twin = Some(ind.clone_box());
                    }
                    let out = ind.on_bar(bar);
                    if out.len() != batch.len() {
                        failures.push(format!("{tag}: on_bar gave {} lines at bar {i}", out.len()));
                        break;
                    }
                    for (line, v) in out.iter().enumerate() {
                        let future_line = meta.batch_only;
                        if !future_line && !same(*v, batch[line][i]) {
                            failures.push(format!(
                                "{tag}: line {line} bar {i}: stream {v:e} vs batch {:e}",
                                batch[line][i]
                            ));
                        }
                    }
                    if let Some(t) = twin.as_mut().filter(|_| i >= bars.len() / 2) {
                        let o2 = t.on_bar(bar);
                        if !out.iter().zip(&o2).all(|(a, b)| same(*a, *b)) {
                            failures.push(format!("{tag}: the clone diverged at bar {i}"));
                            break;
                        }
                    }
                    if failures.len() > 60 {
                        break;
                    }
                }
                folded += 1;
            }
        }
    }
    assert!(owned > 0, "shard {shard} owns no indicator");
    assert!(folded >= owned * 9, "folded only {folded} (indicator, set, series) cases");
    assert!(failures.is_empty(), "{} failure(s):\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn a_reset_indicator_replays_identically() {
    let bars = series(1, 700);
    let mut failures: Vec<String> = Vec::new();
    for meta in registry() {
        let mut ind = make_with(meta.name, &[]).expect("a registry name builds");
        let first: Vec<Vec<f64>> = bars.iter().map(|b| ind.on_bar(b)).collect();
        ind.reset();
        for (i, b) in bars.iter().enumerate() {
            let again = ind.on_bar(b);
            if !again.iter().zip(&first[i]).all(|(a, c)| same(*a, *c)) {
                failures.push(format!("{}: differs after reset() at bar {i}", meta.name));
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
