//! The constant-window fixture: the zero-variance guards decide the same with and without history.

use vike_indicators::{make, registry};
use vike_marketdata::Bar;

use super::{bits_eq, synth_bars};

/// The close a [`flat_tailed_bars`] tail sits at when the fixture must BITE. `20` copies of `0.1`
/// summed and divided by `20` is `3fb999999999999b`, and `0.1` is `3fb999999999999a` — so the naive
/// mean does NOT round back, every `x - mean` is ~1.4e-17 instead of `0`, and a two-pass variance
/// that lacks the constant-window predicate returns a rounding-sized number instead of zero.
///
/// ⚠ **The fixture below used [`ROUND_TRIP_LEVEL`] alone, and that is why three registered
/// indicators shipped this defect.** `100.0` is one of the values whose naive mean DOES round back,
/// so every assertion in `a_constant_window_decides_the_zero_variance_guards_the_same_way_with_and_without_history`
/// passed on kernels that had no zero-variance decision in them at all. Both levels run now:
/// `NON_ROUND_TRIP_LEVEL` is the gate, `ROUND_TRIP_LEVEL` is the control that keeps the old
/// coverage, and [`the_constant_window_fixture_actually_bites`] asserts the difference between them
/// so the biting value cannot be quietly swapped back.
const NON_ROUND_TRIP_LEVEL: f64 = 0.1;

/// The control level — its naive mean rounds back, so a broken kernel looks correct here.
const ROUND_TRIP_LEVEL: f64 = 100.0;

/// Bars with a `prefix` of [`synth_bars`] followed by `flat` bars whose close is EXACTLY `level`.
/// The highs/lows keep moving so this is not degenerate for range-based kernels; only the closes are
/// identical, which is what the zero-variance guards key on.
///
/// ⚠ The wiggle is PROPORTIONAL to `level`. It was a flat `± 0.5`, which is unremarkable at
/// `ROUND_TRIP_LEVEL` and puts every low NEGATIVE at `NON_ROUND_TRIP_LEVEL` — a bar no venue can
/// produce. Nothing this fixture is asserted on reads the highs or lows, so the old form would have
/// passed; a fixture that is visibly impossible teaches the next reader to distrust the fixture
/// instead of the kernel.
fn flat_tailed_bars(prefix: usize, flat: usize, level: f64) -> Vec<Bar> {
    let mut bars = synth_bars(prefix);
    for i in 0..flat {
        let t = (prefix + i) as i64;
        bars.push(Bar {
            ts: t * 3_600_000,
            open: level,
            high: level * (1.005 + (i % 3) as f64 * 0.001),
            low: level * (0.995 - (i % 4) as f64 * 0.001),
            close: level,
            volume: 1000.0 + (i % 13) as f64 * 50.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    bars
}

/// ⚠ **The worst member of the accumulator bug class: history deciding NaN-vs-NUMBER.**
///
/// On a window whose values are bit-identical the population variance is exactly zero, and two
/// kernels branch on that — `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_zscore`
/// (`if sd != 0.0`) and `crates/vike-indicators/src/indicators/volatility.rs`'s `batch_bbands_pctb`
/// (`if bw != 0.0`). Under the old `run_sum2/period - mean*mean` form, the residue those
/// accumulators carried from bar 0 made `sd`/`bw` a tiny NONZERO number, so a full run took the
/// branch and a truncated run did not. The output did not drift by an ulp; it changed KIND.
///
/// A pure numeric-drift test cannot catch that, and the registry-wide gate above catches it only by
/// luck — `synth_bars` has no bit-identical run long enough to zero a variance, so the guard is
/// never armed there.
///
/// NON-VACUOUS, and MEASURED rather than assumed: replicating both kernels over exactly this series
/// (3,000 synth bars then 1,000 flat, `period = 20`), the OLD accumulator forms return
/// `var = 2.9e-11`, `stddev = 5.4e-6`, `zscore = 5.0e-8` (a NUMBER), `bbands_pctb = 0.25` (a
/// NUMBER) and `bbands_width = 1.8e-14` on the FULL run — every absolute assertion below fails —
/// while the same kernels over a retained tail of 64/96/127 bars return `var = 0.0`, `zscore = NaN`
/// and `bbands_pctb = NaN`, so the invariance assertion fails too. Both halves therefore die with
/// the change reverted, and neither restates the other: one pins the VALUE, the other pins that
/// history cannot change it.
///
/// ⚠ **This test ran at `ROUND_TRIP_LEVEL` only, and was green for a year against three kernels
/// that never decided anything.** `stddev`, `bbands_width` and `bbands_pctb` do not route through
/// `crates/vike-indicators/src/window.rs` and had no constant-window predicate; they passed because
/// `100.0` is a value whose naive mean happens to round back, so their two-pass fold reached zero by
/// arithmetic luck. At [`NON_ROUND_TRIP_LEVEL`] the same kernels returned
/// `stddev = 1.3877787807814457e-17`, `bbands_width = 5.551115123125782e-16` and — the one that
/// matters — `bbands_pctb = 0.25`, a finite, plausible, entirely meaningless mid-band reading for a
/// band with no width. That is the shape this whole file exists to catch, wearing the fixture as
/// its disguise. Both levels run now; the biting one is protected by
/// [`the_constant_window_fixture_actually_bites`].
#[test]
fn a_constant_window_decides_the_zero_variance_guards_the_same_way_with_and_without_history() {
    for level in [NON_ROUND_TRIP_LEVEL, ROUND_TRIP_LEVEL] {
        let bars = flat_tailed_bars(3_000, 1_000, level);
        let last = bars.len() - 1;

        // `None` == "must be NaN": the guard declined, because the quantity it divides by is
        // exactly zero. `Some(x)` == "must be exactly x". Both are properties of the WINDOW alone,
        // and in particular neither depends on `level` — which is the point of running two.
        for (name, expected) in [
            ("var", Some(0.0)),          // Σ(x - mean)² over identical values
            ("stddev", Some(0.0)),       // ...and its root
            ("bbands_width", Some(0.0)), // (upper - lower) / mid, with sd == 0
            ("hvol", Some(0.0)),         // ln(v/v) == 0 across the window, so its sd is 0 too
            ("zscore", None),            // sd == 0 → undefined
            ("bbands_pctb", None),       // band width == 0 → undefined
        ] {
            let ind = make(name).unwrap();
            let full = ind.vectorize(&bars);
            let got = full[0][last];
            match expected {
                Some(want) => assert!(
                    bits_eq(got, want),
                    "{name} at level {level}: a window of 20 bit-identical closes has variance \
                     exactly zero, so this must be {want:?} — got {got:?}. A nonzero value here \
                     means the kernel is hoping `Σx / n` rounds back to `x` instead of deciding \
                     the constant case, and whether that hope holds depends on the price."
                ),
                None => assert!(
                    got.is_nan(),
                    "{name} at level {level}: a window of 20 bit-identical closes divides by a \
                     zero spread, so this must be NaN — got {got:?}. A finite number here is an \
                     invented reading: there is no band, so there is no position within it."
                ),
            }

            // ...and the same answer once `hist_indicator!` has trimmed, at every retained length
            // the drain can leave. This is the invariance half: even a kernel that got the value
            // "right" by accident must get the SAME one from a truncated tail.
            let keep = vike_indicators::keep_for(ind.lookback_full(), ind.window_reach());
            for r in [keep, keep + keep / 2, (2 * keep).saturating_sub(1)] {
                assert!(r < bars.len(), "{name}: retained length {r} exceeds the fixture");
                let cut = ind.vectorize(&bars[bars.len() - r..]);
                let tail = cut[0][r - 1];
                assert!(
                    bits_eq(tail, got),
                    "{name} at level {level}: retaining {r} bars gives {tail:?} but the full run \
                     gives {got:?} — history decided which BRANCH this kernel took, which is the \
                     NaN-vs-number swap this test exists for"
                );
            }
        }
    }
}

/// ⚠ **The fixture-integrity gate: proof that [`NON_ROUND_TRIP_LEVEL`] can actually fail.**
///
/// The test above is only as strong as its constant. Substituting `100.0` back would leave every
/// assertion in it passing against kernels with no constant-window decision at all — which is
/// precisely the state this crate shipped in. So the arithmetic property the fixture depends on is
/// asserted directly, in both directions: the biting level's naive mean must MISS the repeated
/// value, and the control level's must HIT it. If someone changes the constant, this test tells
/// them what they broke instead of leaving the suite quietly toothless.
///
/// The period is read from the registry rather than written down, so the fixture cannot drift away
/// from the window the indicators actually take.
#[test]
fn the_constant_window_fixture_actually_bites() {
    let period = registry()
        .iter()
        .find(|m| m.name == "stddev")
        .and_then(|m| m.params.first())
        .map(|p| p.default.round() as usize)
        .expect("`stddev` must exist and carry a period parameter");
    assert!(period >= 2, "a constant-window fixture needs a window, got {period}");

    let naive_mean = |v: f64| (0..period).map(|_| v).sum::<f64>() / period as f64;

    let biting = naive_mean(NON_ROUND_TRIP_LEVEL);
    assert!(
        biting.to_bits() != NON_ROUND_TRIP_LEVEL.to_bits(),
        "NON_ROUND_TRIP_LEVEL ({NON_ROUND_TRIP_LEVEL}) no longer bites at period {period}: its \
         naive mean is {biting:?} ({:016x}) and the value is {:016x}. The constant-window tests \
         are now vacuous — every kernel reaches zero by arithmetic luck and none of them has to \
         DECIDE anything. Pick a value whose mean misses, do not relax this.",
        biting.to_bits(),
        NON_ROUND_TRIP_LEVEL.to_bits()
    );

    let control = naive_mean(ROUND_TRIP_LEVEL);
    assert!(
        control.to_bits() == ROUND_TRIP_LEVEL.to_bits(),
        "ROUND_TRIP_LEVEL ({ROUND_TRIP_LEVEL}) was supposed to be the level where the naive mean \
         rounds back — the control that shows the two cases differ — but its mean is {control:?}. \
         Both levels biting is not a failure of the code, it is a failure of this fixture to \
         demonstrate the contrast it claims."
    );
}

/// ⚠ **The other half of the predicate: it must NOT be an epsilon.**
///
/// `is_constant_window` is an EXACT equality over the window's INPUTS. The tempting cheaper cure —
/// clamping a small OUTPUT to zero, `if var < 1e-30 { 0.0 }` — passes every assertion in
/// [`a_constant_window_decides_the_zero_variance_guards_the_same_way_with_and_without_history`]
/// while being a second law with a number in it, and it would swallow a window whose variance is
/// genuinely tiny but real.
///
/// So: take the biting fixture and move ONE close by a SINGLE ULP. The window now holds two
/// distinct values, its true variance is above zero, and every one of these must stay finite and
/// non-zero. MEASURED on this fixture: `stddev = 1.3526394372366473e-17` (variance ~1.8e-34, three
/// orders of magnitude BELOW the `1e-30` an epsilon would plausibly use), `bbands_width =
/// 5.551115123125782e-16`, `bbands_pctb = 0.25`.
///
/// ⚠ The assertion is "finite and non-zero", NOT a pinned value, and the difference is deliberate.
/// At this level the rounding noise and one ULP are the same magnitude, so the exact figures above
/// are not an accurate variance and pinning them would assert precision nobody has. What is being
/// gated is the BRANCH: a window holding two distinct values must fold, not shortcut.
#[test]
fn a_variance_that_is_small_but_real_still_yields_a_nonzero_band() {
    let mut bars = flat_tailed_bars(3_000, 1_000, NON_ROUND_TRIP_LEVEL);
    let last = bars.len() - 1;

    // One ULP up, on the bar before the last: inside every window a period >= 2 can take, and not
    // the bar `bbands_pctb` uses as its numerator, so the band — not the price — is what moves.
    let moved = f64::from_bits(NON_ROUND_TRIP_LEVEL.to_bits() + 1);
    assert!(moved != NON_ROUND_TRIP_LEVEL, "one ULP up must be a different float");
    bars[last - 1].close = moved;

    for name in ["stddev", "bbands_width", "bbands_pctb"] {
        let got = make(name).unwrap().vectorize(&bars)[0][last];
        assert!(
            got.is_finite(),
            "{name}: a window holding two DISTINCT values has a real, positive variance, so this \
             must be finite — got {got:?}. NaN here means the constant-window shortcut fired on a \
             window that is not constant."
        );
        if name != "bbands_pctb" {
            assert!(
                got != 0.0,
                "{name}: got exactly {got:?} on a window whose values are NOT all equal. That is \
                 the signature of a magnitude THRESHOLD on the output rather than an exact \
                 equality on the inputs — an epsilon cannot tell a real 1e-34 variance from a \
                 rounding artifact, which is why the predicate reads the window instead."
            );
        }
    }
}
