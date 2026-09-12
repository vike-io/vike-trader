//! Characterization pin for `var` and `zscore` — the two registered indicators that
//! `src/window.rs` reroutes. Their values are USER-VISIBLE (Rhai scripts, the chart's
//! ƒx picker), so a "deduplication" that changes them is a silent regression.
//!
//! Regenerate deliberately with `VIKE_REGEN_WINDOW_PIN=1 cargo test -p vike-indicators
//! --test window_pin`. A regeneration that changes existing values must be justified in
//! the commit message, never waved through.

use std::fmt::Write as _;
use std::path::PathBuf;

use vike_indicators::make_with;
use vike_model::Bar;

const FIXTURE: &str = "tests/fixtures/window_pin.tsv";

/// Regeneration switch — see the module doc. Named so `crates/vike-ops/src/settings.rs`'s
/// `VIKE_REGEN_WINDOW_PIN` row (`Naming::Konst("REGEN_ENV")`) resolves through this constant
/// rather than a second copy of the literal.
const REGEN_ENV: &str = "VIKE_REGEN_WINDOW_PIN";

/// A bounded oscillation built from EXACTLY-ROUNDED arithmetic only — `+ - * /`, which IEEE 754
/// requires to be correctly rounded, so every platform computes the same bits.
///
/// ⚠ **This replaced a `sin`/`cos` mix, and the reason is the whole point of the fixture.** IEEE
/// 754 does NOT require the transcendental functions to be correctly rounded: `sin` and `cos` come
/// from the platform's libm, and glibc and the MSVC runtime are each entitled to their own last
/// bit. The pin below is a COMMITTED file of exact `f64` bits, so a fixture whose values are a
/// platform's opinion is a fixture the two boxes do not share — and the suite could then only ever
/// be green on whichever box last regenerated it.
///
/// ⚠ **MEASURED 2026-08-25, and the honest result is that this pin was passing by LUCK, not
/// failing.** The two raw terms DO differ across the boxes (`(t * 0.07).sin() * 8.0` hashed
/// `ff3c998eb6a37f43` on Windows against `663f9eedef16049f` on Linux; the `cos` term likewise), but
/// adding them to `100.0` rounds the ~1e-15 disagreement away at that magnitude, and all 64 closes
/// came out bit-identical on both. Nothing about that is a property: it is the arithmetic of THIS
/// sample count at THIS amplitude against THIS offset. Change `n`, either frequency, either
/// amplitude or the offset — or update a libm — and the absorption stops. The sibling fixture in
/// `crates/vike-chart/tests/common/mod.rs` is the same construction with a smaller offset ratio and
/// it already diverges by a full ULP at one index of 400. So this is a latent break being closed,
/// not an observed one being repaired.
///
/// The shape is a parabolic wave: the phase folded to a triangle, then squared. The triangle alone
/// is the workspace's usual replacement, but it is piecewise LINEAR, and a linear ramp has the
/// SAME variance in every window that sits on one of its segments — MEASURED on this fixture, the
/// `period = 2` pin collapsed from 61 distinct values to 8. One extra multiplication restores the
/// curvature, and with it the pin's reach: 61 / 60 / 45 distinct values at periods 2 / 5 / 20,
/// which is exactly what the `sin`/`cos` version produced.
///
/// `period` is the sample count of a full cycle, matching the frequency the replaced call had:
/// `(t * k).sin()` repeats every `2π/k` samples, so `0.07 -> 90` and `0.017 -> 370`.
fn wave(i: usize, period: usize) -> f64 {
    let phase = (i % period) as f64 / period as f64; // [0, 1)
    let up = 2.0 * phase - 1.0; // [-1, 1)
    let folded = if up < 0.0 { -up } else { up }; // |·|, exact
    let tri = 1.0 - 2.0 * folded; // triangle, [-1, 1]
    1.0 - 2.0 * tri * tri // parabola, [-1, 1]
}

/// Deterministic bars: a two-frequency parabolic wave (see [`wave`] for why it is not a
/// sine/cosine mix), every 17th bar flat against the previous close (exercises the zero-variance
/// branch `batch_zscore`'s `sd != 0.0` guard decides on).
fn synth_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let mut close = 100.0 + wave(i, 90) * 8.0 + wave(i, 370) * 4.0;
        if i > 0 && i % 17 == 0 {
            close = prev_close;
        }
        bars.push(Bar {
            ts: i as i64 * 3_600_000,
            open: prev_close,
            high: prev_close.max(close) + 0.5,
            low: prev_close.min(close) - 0.5,
            close,
            volume: 1000.0 + (i % 13) as f64 * 50.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        prev_close = close;
    }
    bars
}

/// `name<TAB>period<TAB>index<TAB>bits` for every non-warm-up value.
fn render(bars: &[Bar]) -> String {
    let mut out = String::new();
    for name in ["var", "zscore"] {
        for period in [2usize, 5, 20] {
            let ind = make_with(name, &[period as f64])
                .unwrap_or_else(|| panic!("indicator {name} missing"));
            let vals = ind.vectorize(bars);
            for (i, v) in vals[0].iter().enumerate() {
                writeln!(out, "{name}\t{period}\t{i}\t{:016x}", v.to_bits()).unwrap();
            }
        }
    }
    out
}

#[test]
fn var_and_zscore_match_their_pinned_values() {
    let bars = synth_bars(64);
    let rendered = render(&bars);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);

    if std::env::var(REGEN_ENV).as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
        return;
    }

    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing {FIXTURE}: {e}. Generate with {REGEN_ENV}=1."));
    let exp: Vec<&str> = expected.lines().collect();
    let got: Vec<&str> = rendered.lines().collect();
    assert_eq!(exp.len(), got.len(), "row count changed");
    for (e, g) in exp.iter().zip(got.iter()) {
        assert_eq!(e, g, "pinned value changed — the refactor altered indicator output");
    }
}
