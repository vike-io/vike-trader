//! THE correctness gate: for every indicator, `on_bar` folded over a synthetic
//! series must equal `vectorize` on that series, BIT-FOR-BIT. This proves the
//! streaming path and the batch path are one source of truth. Bits are compared
//! via `f64::to_bits`; both-NaN counts as equal (warm-up NaNs).

use vike_indicators::{Indicator, WindowReach, make, make_with, registry};
use vike_model::Bar;

/// Bitwise float equality, with both-NaN treated as equal.
fn bits_eq(a: f64, b: f64) -> bool {
    if a.is_nan() && b.is_nan() { true } else { a.to_bits() == b.to_bits() }
}

/// Deterministic, varied OHLCV. Hourly bars span multiple UTC days (exercises the
/// VWAP session reset); every 17th bar is flat vs the previous close (exercises the
/// OBV/RSI zero-delta branches); prices oscillate so PSAR flips direction.
fn synth_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let t = i as f64;
        let mut close =
            100.0 + (t * 0.07).sin() * 8.0 + (t * 0.017).cos() * 4.0 + (t * 0.31).sin() * 1.5;
        if i > 0 && i % 17 == 0 {
            close = prev_close; // flat bar
        }
        let open = prev_close;
        let high = open.max(close) + (i % 5) as f64 * 0.3 + 0.5;
        let low = open.min(close) - (i % 7) as f64 * 0.25 - 0.5;
        let volume = 1000.0 + (i % 13) as f64 * 50.0;
        bars.push(Bar {
            ts: i as i64 * 3_600_000, // hourly
            open,
            high,
            low,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        prev_close = close;
    }
    bars
}

/// Fold `on_bar` over the series into per-line columns (same shape as `vectorize`).
fn fold_stream(ind: &mut dyn Indicator, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut lines: Vec<Vec<f64>> = Vec::new();
    for bar in bars {
        let out = ind.on_bar(bar);
        if lines.is_empty() {
            lines = vec![Vec::with_capacity(bars.len()); out.len()];
        }
        assert_eq!(out.len(), lines.len(), "on_bar output arity must be stable");
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

/// The bar count at which `hist_indicator!`'s history trim first fires for the DEEPEST retention
/// family: `WindowReach::Smoothed` floors at `KEEP_FLOOR` and the drain compacts at `keep * 2`.
/// Every other test in this file runs at 400 or fewer bars, which is BELOW it — so before this test
/// existed, the trim was never executed under test at all.
///
/// ⚠ The `WindowReach::Finite` family floors at `FINITE_FLOOR` instead, so its drain fires an order
/// of magnitude earlier and `on_bar_equals_vectorize_for_every_indicator`'s own 400 bars now cross
/// it. That is a gain, not a reason to lower this: the number here has to clear the DEEPEST family,
/// which is the one no shorter test reaches.
const PAST_THE_TRIM: usize = 1_200;

/// The whole point of [`PAST_THE_TRIM`] is that it EXCEEDS the trim threshold, so that is asserted
/// at COMPILE time rather than inside one test: a runtime `assert!` on two constants is a lint
/// (`clippy::assertions_on_constants`) precisely because it can never fire at a useful moment.
///
/// ⚠ It is DERIVED from the crate's own floor rather than restating it. This line used to read
/// `assert!(PAST_THE_TRIM > 512)` with a comment re-deriving 512 from `KEEP_FLOOR` by hand — a copy
/// that survives exactly until someone changes the floor, which is what `WindowReach` just did.
const _: () = assert!(PAST_THE_TRIM > 2 * vike_indicators::KEEP_FLOOR);

/// ⚠ **The gate that was missing when a real bug shipped.**
///
/// `hist_indicator!` drops history the batch kernel "provably cannot read". That is sound for a
/// bounded-window kernel and WRONG for a path-dependent one, which reads everything since the mount
/// — and six of the nine indicators `is_path_dependent` names are generated by that very macro
/// (`mcginley`, `ad`, `nvi`, `pvi`, `pvt`, `net_volume`). The macro's own comment asserted the
/// opposite, so the exemption was never applied and each of those six recomputed a cumulative sum
/// from a truncated start.
///
/// It shipped green because `on_bar_equals_vectorize_for_every_indicator` runs 400 bars and the trim
/// first fires at 512: the gate passed by never reaching the code. Length is the whole point of this
/// test, so it runs at [`PAST_THE_TRIM`] and asserts the trim was genuinely crossed.
#[test]
fn path_dependent_indicators_survive_a_series_past_the_trim() {
    let bars = synth_bars(PAST_THE_TRIM);
    let mut checked = 0usize;
    for meta in registry() {
        if meta.batch_only {
            continue;
        }
        let mut ind = make(meta.name).unwrap();
        if !ind.warmup_path_dependent() {
            continue;
        }
        let stream = fold_stream(ind.as_mut(), &bars);
        let batch = ind.vectorize(&bars);
        for (line, (s, b)) in stream.iter().zip(batch.iter()).enumerate() {
            for (i, (sv, bv)) in s.iter().zip(b.iter()).enumerate() {
                assert_eq!(
                    sv.to_bits(),
                    bv.to_bits(),
                    "{} line {line} bar {i}: streamed {sv} != batch {bv} — a path-dependent kernel \
                     had its history truncated, so it recomputed from a truncated start",
                    meta.name
                );
            }
        }
        checked += 1;
    }
    assert!(
        checked >= 6,
        "expected at least the six macro-generated path-dependent indicators (mcginley, ad, nvi, \
         pvi, pvt, net_volume); checked {checked} — if this dropped, the exemption may have been \
         narrowed and this gate is no longer watching them"
    );
}

/// The FULL registry at a length that actually crosses the trim.
///
/// `on_bar_equals_vectorize_for_every_indicator` is the same assertion at 400 bars — below the
/// threshold — so it proves the kernels agree while proving nothing about the trim. This is the
/// same property where the trim is live, for every indicator rather than only the path-dependent
/// ones: a bounded-window kernel whose declared `lookback_full` UNDER-reports its true reach would
/// be silently truncated too, and nothing else in this file would notice.
#[test]
fn on_bar_equals_vectorize_past_the_trim_for_every_indicator() {
    let bars = synth_bars(PAST_THE_TRIM);
    for meta in registry() {
        if meta.batch_only {
            continue;
        }
        let mut ind = make(meta.name).unwrap();
        let stream = fold_stream(ind.as_mut(), &bars);
        let batch = ind.vectorize(&bars);
        for (line, (s, b)) in stream.iter().zip(batch.iter()).enumerate() {
            for (i, (sv, bv)) in s.iter().zip(b.iter()).enumerate() {
                assert_eq!(
                    sv.to_bits(),
                    bv.to_bits(),
                    "{} line {line} bar {i} (past the history trim): {sv} != {bv}",
                    meta.name
                );
            }
        }
    }
}

/// The exemption must stay MEASURED, not assumed: every name `is_path_dependent` declares has to be
/// a real registry indicator that actually reports `warmup_path_dependent()`.
///
/// The bug this file now guards against was a claim about which indicators were affected that
/// nothing checked. A declared-but-absent name would make the exemption silently narrower than its
/// author believed — the identical failure, one level up.
#[test]
fn every_declared_path_dependent_indicator_exists_and_reports_itself() {
    for name in ["psar", "vwap", "mcginley", "obv", "ad", "nvi", "pvi", "pvt", "net_volume"] {
        let ind = make(name).unwrap_or_else(|| {
            panic!(
                "`{name}` is declared path-dependent but is \
             not in the registry — the declaration and the roster have drifted"
            )
        });
        assert!(
            ind.warmup_path_dependent(),
            "`{name}` is declared path-dependent but does not report it, so the macro's trim \
             exemption will not apply to it"
        );
    }
}

/// Accumulator kernels that are NOT truncation-invariant, and are therefore wrong past their own
/// drain threshold on any long series.
///
/// ⚠ **A RATCHET: it may shrink, never grow — and it is now EMPTY.** It held seventeen names, every
/// one of which routed through a batch kernel carrying a RUNNING ACCUMULATOR: a sliding
/// `sum += c[i] - c[i-n]` in `crates/vike-indicators/src/math.rs`'s `sma`, or a `run_sum`/`run_sum2`
/// pair in `crates/vike-indicators/src/indicators/statistics.rs`'s `batch_var`/`batch_zscore` and
/// `crates/vike-indicators/src/indicators/volatility.rs`'s `stddev_series`/`batch_hvol`, or the
/// `run_pv`/`run_v` and `run_clvv`/`run_v` pairs in
/// `crates/vike-indicators/src/indicators/overlap.rs`'s `batch_vwma` and
/// `crates/vike-indicators/src/indicators/volume.rs`'s `batch_cmf`.
///
/// Such a kernel's value at bar `i` carried rounding error from bar 0, so its window was
/// mathematically finite while its ARITHMETIC was not. Truncating history changed its bits, and **no
/// `KEEP_FACTOR` could fix that** — the error is a random walk, not a decaying tail. That is the
/// whole difference from the EMA/Wilder family, where a large enough factor genuinely is bit-exact.
/// All six kernels now RECOMPUTE their window, which makes the value a pure function of the window,
/// which is precisely the drain's precondition. Nothing pinned the old values: this crate ships no
/// golden fixtures and no downstream crate asserts an indicator value, so the change was free to
/// make.
///
/// ⚠ **The list must stay, empty, rather than being deleted with its assertions.** The gate below
/// asserts it in BOTH directions, so an empty list is what turns "no accumulator kernels exist" from
/// an unexamined belief into a merge gate: any future kernel that carries state across bars fails
/// `newly_broken` by name.
///
/// ⚠ `crates/vike-indicators/src/pairs.rs` still holds `run_sum`/`run_sum2` pairs and is
/// deliberately untouched: the pair seam's streaming macro pushes to `hist_a`/`hist_b` and NEVER
/// drains them, so `on_pair` and `vectorize_pair` both fold from bar zero and the truncation
/// question does not arise there. It would the day that seam grows a drain.
const NOT_TRUNCATION_INVARIANT: &[&str] = &[];

/// ⚠ **The trim's PRECONDITION, asserted directly — the gate #1183 needed and did not have.**
///
/// `hist_indicator!` drains to `keep` and re-runs the batch kernel over the retained tail, reading
/// only its last element (`indicators::stream_tail`). So the property the drain actually depends on
/// is not "streaming equals batch over 1200 bars"; it is:
///
/// ```text
///     last(vectorize(&bars[len - retained ..]))  ==  vectorize(&bars)[len - 1]
/// ```
///
/// for every `retained` the drain can leave behind, which is `keep ..= 2*keep - 1`.
///
/// ⚠ **`on_bar_equals_vectorize_past_the_trim_for_every_indicator` can only reach this by BRUTE
/// LENGTH, and it does not reach most of it.** The drain first fires at `2 * keep`, i.e. at
/// `128 * lookback_full + 2` bars, so at `PAST_THE_TRIM` it executes only for the shallowest
/// indicators — the rest of the registry it names is never truncated at all, and it passes by not
/// running the code. That is exactly how #1183's bug shipped, and raising the length is not the
/// answer: full coverage needs tens of thousands of bars at O(len × keep) per indicator, while
/// three `vectorize` calls at each indicator's OWN `keep` prove the same thing for the whole
/// registry in a fraction of a second.
///
/// NON-VACUOUS: `NOT_TRUNCATION_INVARIANT` is asserted to be exactly the failing set, in BOTH
/// directions — a name that starts passing must be removed (the ratchet), and a new failure fails
/// the test rather than being absorbed. With the list empty and the kernels unfixed, this test
/// fails; that is the state it was written in.
#[test]
fn every_trimmed_indicator_is_truncation_invariant() {
    let bars = synth_bars(4_000);
    let mut broken: Vec<String> = Vec::new();

    for meta in registry() {
        if meta.batch_only {
            continue;
        }
        let ind = make(meta.name).unwrap();
        // Only the macro-generated impls truncate. A hand-written incremental one folds from bar 0
        // and never drops a bar, so the question does not arise — and asking it anyway would report
        // a "failure" nothing could act on.
        if !ind.trims_history() || ind.warmup_path_dependent() {
            continue;
        }

        // ⚠ Read BOTH inputs off the indicator. `keep_for` grew a `WindowReach` argument when the
        // retention split in two, and a gate that reconstructed which family an indicator belongs
        // to would be testing its own copy of the answer rather than the drain's.
        let keep = vike_indicators::keep_for(ind.lookback_full(), ind.window_reach());
        let full = ind.vectorize(&bars);
        // The three retained lengths the drain can leave: the bound itself, one short of the next
        // compaction, and a midpoint. A kernel that is invariant at `keep` but not at `2*keep - 1`
        // is still wrong on a real chart, because the buffer spends most of its life in between.
        let retained = [keep, keep + keep / 2, (2 * keep).saturating_sub(1)];

        let mut bad = false;
        for r in retained {
            if r >= bars.len() {
                continue;
            }
            let tail = &bars[bars.len() - r..];
            let cut = ind.vectorize(tail);
            for (line, col) in cut.iter().enumerate() {
                let (a, b) = (col[col.len() - 1], full[line][bars.len() - 1]);
                // NaN == NaN by bits here, deliberately: a kernel that emits NaN where the full run
                // emits a number (or the reverse) is the WORST version of this bug, not an exempt
                // case. `bbands_pctb`'s `if bw != 0.0` guard does exactly that at tight params.
                if a.to_bits() != b.to_bits() {
                    bad = true;
                }
            }
        }
        if bad {
            broken.push(meta.name.to_string());
        }
    }

    let mut expected: Vec<String> =
        NOT_TRUNCATION_INVARIANT.iter().map(|s| (*s).to_string()).collect();
    broken.sort();
    expected.sort();

    let newly_broken: Vec<&String> = broken.iter().filter(|n| !expected.contains(n)).collect();
    let newly_fixed: Vec<&String> = expected.iter().filter(|n| !broken.contains(n)).collect();

    assert!(
        newly_broken.is_empty(),
        "{} indicator(s) are NOT truncation-invariant and are not declared: {newly_broken:?}\n\n\
         The `hist_indicator!` drain re-runs the batch kernel over a retained tail, so a kernel \
         that carries a running accumulator across bars returns a DIFFERENT number once history is \
         dropped — silently, past a threshold no short test reaches. Either make the kernel \
         recompute its window, or add the name to `NOT_TRUNCATION_INVARIANT` with the reason.",
        newly_broken.len()
    );
    assert!(
        newly_fixed.is_empty(),
        "{newly_fixed:?} are declared NOT truncation-invariant but now pass — the list is a \
         RATCHET, so delete those rows. Leaving them would let a real regression hide behind a \
         stale exception, which is how the bug this gate exists for reached `main`."
    );
}

/// The gate above is only as good as its SCOPE, so this pins the scope itself.
///
/// If `trims_history` ever returned `false` for the macro-generated impls, the gate would iterate
/// nothing and pass forever — the failure mode it exists to prevent, one level up. This asserts the
/// flag genuinely partitions the registry: the macro-generated majority trims, the hand-written
/// incremental ones do not, and both sides are non-empty.
#[test]
fn the_truncation_gate_actually_has_indicators_to_check() {
    let (mut trims, mut folds) = (0usize, 0usize);
    for meta in registry() {
        let ind = make(meta.name).unwrap();
        if ind.trims_history() {
            trims += 1;
        } else {
            folds += 1;
        }
    }
    assert!(
        trims > 100,
        "only {trims} indicators report trims_history — the gate has lost its scope"
    );
    assert!(
        folds > 5,
        "only {folds} hand-written impls — `trims_history` may have become universal"
    );

    // ...and the two shapes are what they claim. `sma` is hand-written incremental; `dpo` is
    // macro-generated. If either flipped, the gate would be asking the wrong population.
    assert!(!make("sma").unwrap().trims_history(), "`sma` is a hand-written incremental impl");
    assert!(make("dpo").unwrap().trims_history(), "`dpo` is generated by hist_indicator!");
}

// =============================================================================================
// The shrunken retention (`WindowReach::Finite`) and its backstops.
// =============================================================================================

/// A tiny deterministic LCG. This crate has no `rand` dev-dep, and the gates below need SEVERAL
/// DATA SHAPES rather than one closed-form oscillation — see
/// [`finite_window_indicators_are_truncation_invariant_across_shapes_and_params`] for why one shape
/// is a weak verifier here.
struct Lcg(u64);
impl Lcg {
    fn unit(&mut self) -> f64 {
        // The Knuth/MMIX constants; the top 53 bits give a uniform in [0, 1).
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The data shapes the finite-retention gate runs. `synth_bars` is ONE smooth oscillation with no
/// zero prices and no long identical runs, and a kernel can be invariant on it for reasons that do
/// not generalise.
const SHAPES: &[&str] = &["random-walk", "trend", "mostly-flat", "high-vol"];

/// Deterministic OHLCV in a named [`SHAPES`] shape. Every shape keeps prices and volumes strictly
/// POSITIVE on purpose: a zero or negative close is what puts an INTERIOR NaN into a `roc`/`ln`
/// series, and the indicators that window over those are deliberately NOT declared
/// `WindowReach::Finite` (see `crates/vike-indicators/src/indicators/mod.rs`'s `window_reach`), so
/// generating one here would be testing a claim nobody made.
fn shaped_bars(shape: &str, n: usize) -> Vec<Bar> {
    let mut rng = Lcg(0x5eed_1234_9876_abcd);
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let u = rng.unit() - 0.5;
        let close = match shape {
            "random-walk" => (prev_close + u * 2.0).max(1.0),
            "trend" => (100.0 + i as f64 * 0.05 + u * 0.4).max(1.0),
            // Long runs of BIT-IDENTICAL closes — the shape that drives every zero-variance and
            // zero-range guard, and the one a smooth oscillation never produces.
            "mostly-flat" => {
                if i % 23 < 18 {
                    prev_close
                } else {
                    (prev_close + u * 3.0).max(1.0)
                }
            }
            _ => (prev_close * (1.0 + u * 0.08)).max(1.0),
        };
        let open = prev_close;
        let high = open.max(close) + rng.unit() * 0.6;
        let low = (open.min(close) - rng.unit() * 0.6).max(0.005);
        let volume = 500.0 + rng.unit() * 5_000.0;
        bars.push(Bar {
            ts: i as i64 * 3_600_000,
            open,
            high,
            low,
            close,
            volume,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        prev_close = close;
    }
    bars
}

/// ⚠ **The backstop for the `WindowReach::Finite` declarations — and it is a BACKSTOP, not the
/// authority.**
///
/// `WindowReach::Finite` cuts an indicator's retained history from `64 * lookback_full` to
/// `lookback_full + 33`, which is what makes the de-accumulated kernels 0.07x-0.91x main's cost per
/// streamed bar instead of 1.1x-36x. It is also an UNVERIFIABLE claim in the general case:
/// `stochrsi` was MEASURED to look truncation-invariant from 30 retained bars on
/// `every_trimmed_indicator_is_truncation_invariant`'s own 4,000-bar `synth_bars`, while the Wilder
/// `rsi_vals` beneath it needs ~493 bars on every data shape tried. A wrong declaration for it would
/// have been greenlit. So the declaration is structural — see
/// `crates/vike-indicators/src/indicators/mod.rs`'s `window_reach`, which cites the kernel for each
/// row — and this test only widens the net.
///
/// What it adds over the registry-wide gate above: FOUR data shapes instead of one — including
/// `mostly-flat`, whose bit-identical runs drive the zero-variance guards a smooth oscillation never
/// reaches — and MIN/DEFAULT/MAX params instead of defaults only, because the retention scales with
/// `lookback_full` and the margin is thinnest where the period is smallest.
///
/// The series length is DERIVED as `2 * keep`, the exact count at which the drain first fires, so
/// this stays cheap as periods grow instead of needing a hard-coded length that would silently stop
/// reaching the deep-period cases.
///
/// NON-VACUOUS: the assertion compares bits including NaN-ness, and these kernels were measured to
/// FAIL it before the de-accumulation. Over the retained range this very gate walks, at
/// `period` 20 and 200 respectively: `zscore` and `cmf` mismatched at 64 of 64 and 232 of 232
/// retained lengths, `var` and `stddev` at 64 of 64 and 231 of 232, `vwma` at 52 of 64 and 230 of
/// 232. `names`/`checked` guard the other direction: if `window_reach` stopped returning `Finite`
/// the loop would iterate nothing and pass.
#[test]
fn finite_window_indicators_are_truncation_invariant_across_shapes_and_params() {
    let mut checked = 0usize;
    let mut names: Vec<&str> = Vec::new();

    for meta in registry() {
        if meta.batch_only {
            continue;
        }
        if make(meta.name).unwrap().window_reach() != WindowReach::Finite {
            continue;
        }
        names.push(meta.name);

        // All-min / defaults / all-max. The `ParamSpec` bounds are legal by construction, so no
        // `coerce` is needed to reach `make_with`.
        let settings: Vec<Vec<f64>> = if meta.params.is_empty() {
            vec![Vec::new()]
        } else {
            vec![
                meta.params.iter().map(|p| p.min).collect(),
                meta.params.iter().map(|p| p.default).collect(),
                meta.params.iter().map(|p| p.max).collect(),
            ]
        };

        for raw in &settings {
            let ind = make_with(meta.name, raw).unwrap();
            assert!(
                ind.trims_history() && !ind.warmup_path_dependent(),
                "{}: declared WindowReach::Finite but the drain never reaches it — a retention \
                 policy on an indicator that does not truncate is a claim nobody checks",
                meta.name
            );
            let keep = vike_indicators::keep_for(ind.lookback_full(), ind.window_reach());
            let n = 2 * keep; // the exact length at which `hist_indicator!` first compacts
            let retained = [keep, keep + keep / 2, (2 * keep).saturating_sub(1)];

            for shape in SHAPES {
                let bars = shaped_bars(shape, n);
                let full = ind.vectorize(&bars);
                for r in retained {
                    let cut = ind.vectorize(&bars[n - r..]);
                    for (line, col) in cut.iter().enumerate() {
                        let (a, b) = (col[col.len() - 1], full[line][n - 1]);
                        assert!(
                            bits_eq(a, b),
                            "{} line {line} params {raw:?} shape {shape}: retaining {r} of {n} \
                             bars gives {a:?} but the full run gives {b:?} — the \
                             `WindowReach::Finite` declaration claims this kernel reaches at most \
                             `lookback_full + 1` bars, and it does not",
                            meta.name
                        );
                    }
                }
                checked += 1;
            }
        }
    }

    assert!(
        names.len() >= 10 && checked >= 40,
        "only {} indicator(s) / {checked} (indicator, params, shape) combinations declare \
         WindowReach::Finite — if that collapsed, this gate is watching nothing and the retention \
         split silently reverted to the conservative multiple: {names:?}",
        names.len()
    );
}

/// The `WindowReach::Finite` roster, pinned — the reviewable record of which kernels were granted
/// the shrunken retention.
///
/// ⚠ **A pin records an answer; it cannot derive one.** Its job is to make ADDING a row a visible
/// event in a diff, because the gate above cannot tell a correct grant from an incorrect one (see
/// its doc). The argument for each row lives with the declaration, in
/// `crates/vike-indicators/src/indicators/mod.rs`'s `window_reach`, next to the kernel it cites.
///
/// The four deliberately WITHHELD rows are asserted too, and they matter more than the granted
/// ones: each is truncation-invariant at today's retention and would look fine in any short test,
/// so nothing but this assertion records that leaving them out was a decision rather than an
/// oversight.
#[test]
fn the_finite_window_roster_is_pinned() {
    let mut granted: Vec<&str> = registry()
        .iter()
        .filter(|m| make(m.name).unwrap().window_reach() == WindowReach::Finite)
        .map(|m| m.name)
        .collect();
    granted.sort_unstable();
    // ⚠ The 63 candlestick patterns are asserted as a FAMILY, derived, not spelled. They share one
    // kernel (`patterns.rs`'s `avg_body`), so they are one decision — and a 63-name wall here would
    // be a second roster to keep in step with the first, which is the rot this repo removes counts
    // for. What is pinned is the PROPERTY: every pattern is Finite, and the family is non-empty.
    let patterns: Vec<&str> = registry()
        .iter()
        .filter(|m| m.category == vike_indicators::Category::Pattern)
        .map(|m| m.name)
        .collect();
    assert!(
        patterns.len() > 50,
        "the pattern family collapsed to {} — the filter is wrong, and a          vacuous family would make the assertion below prove nothing",
        patterns.len()
    );
    for name in &patterns {
        assert_eq!(
            make(name).unwrap().window_reach(),
            WindowReach::Finite,
            "`{name}` reads through `avg_body` = `sma(|close - open|, CTX)`: no division, no `ln`,              no `smooth_defined`, no recursive term — a finite window, and `patterns.rs` contains              no counter-example to that"
        );
    }

    // ...and the individually-reasoned rows, each granted for its own kernel.
    let mut granted: Vec<&str> = registry()
        .iter()
        .filter(|m| {
            make(m.name).unwrap().window_reach() == WindowReach::Finite
                && m.category != vike_indicators::Category::Pattern
        })
        .map(|m| m.name)
        .collect();
    granted.sort_unstable();
    assert_eq!(
        granted,
        vec![
            "ac",
            "alma",
            "aroon",
            "aroonosc",
            "bbands_pctb",
            "bbands_width",
            "bop",
            "chop",
            "cmf",
            "cmo",
            "donchian_width",
            "dpo",
            "envelopes",
            "eom",
            "high_low_52w",
            "hma",
            "kurtosis",
            "linearreg",
            "linearreg_angle",
            "linearreg_intercept",
            "linearreg_slope",
            "mad",
            "mfi",
            "midpoint",
            "midprice",
            "mom",
            "pivot_points",
            "rank_correlation",
            "rocp",
            "rocr",
            "rocr100",
            "skew",
            "std_error",
            "std_error_bands",
            "stddev",
            "stochf",
            "trima",
            "true_range",
            "tsf",
            "ulcer",
            "ultosc",
            "var",
            "volume_profile_poc",
            "vortex",
            "vwma",
            "williams_fractal",
            "zscore",
        ]
    );

    // ⚠ The two IIR rows are DECAY-bounded now, not blanket-bounded. Their reach is set by the
    // SMOOTHING period, which `lookback_full` overstates — so retention fell from 1665/1857 to
    // `37 * 14 = 518` while `every_trimmed_indicator_is_truncation_invariant` stayed green. The
    // period asserted here is the one the RECURRENCE uses, not the indicator's warm-up.
    for (name, period, why) in [
        ("relative_volatility", 14usize, "smooth_defined(.., ema, period) over its FIRST param"),
        ("stochrsi", 14usize, "rsi_vals(.., rsi_p) is a Wilder recurrence over its FIRST param"),
    ] {
        assert_eq!(
            make(name).unwrap().window_reach(),
            WindowReach::SmoothedOver(period),
            "`{name}` is bounded by its smoothing period, not by `lookback_full`: {why}"
        );
    }

    // ...and the two whose reach is unbounded for a NON-decay reason keep the conservative rule.
    // No factor can help an interior NaN gap, so a `SmoothedOver` here would be silently wrong.
    for (name, why) in [
        ("hvol", "windows over DEFINED log returns; ln() is undefined at a non-positive close"),
        ("kst", "windows over DEFINED roc values; roc is undefined at a zero prior close"),
    ] {
        assert_eq!(
            make(name).unwrap().window_reach(),
            WindowReach::Smoothed,
            "`{name}` must keep the conservative retention: {why}. It left \
             NOT_TRUNCATION_INVARIANT on the kernel change alone and is invariant at the 64x \
             retention — which is exactly why granting it a smaller one would look correct."
        );
    }
}

/// ⚠ **A `SmoothedOver` row MUST name a real period, or it silently under-retains.**
///
/// `crate::indicators::window_reach` returns `SmoothedOver(0)` as a placeholder — it is keyed on a
/// `&str` and cannot see an indicator's runtime params — and `hist_indicator!` fills the real value
/// in from `smoothing_period`. A key marked `SmoothedOver` with no `smoothing_period` arm keeps the
/// `0`, and `keep_for` then floors it to `KEEP_FLOOR`: a 64-bar retention on an IIR kernel, which is
/// a WRONG NUMBER rather than a slow one.
///
/// NON-VACUOUS: it reads the period off the CONSTRUCTED indicator, i.e. after the macro's fill-in,
/// so a missing arm shows up as `0` and fails here. Reverting that fill-in makes every row report 0.
#[test]
fn every_smoothed_over_row_names_its_period() {
    let mut checked = 0usize;
    for meta in registry() {
        let ind = make(meta.name).unwrap();
        if let WindowReach::SmoothedOver(period) = ind.window_reach() {
            assert!(
                period > 0,
                "`{}` is marked SmoothedOver but reports period 0 — `smoothing_period` has no arm \
                 for it, so `keep_for` would floor its retention to KEEP_FLOOR on an IIR kernel",
                meta.name
            );
            // ...and the retention must actually BE the decay bound, not the floor it collapses to.
            assert!(
                vike_indicators::keep_for(ind.lookback_full(), ind.window_reach())
                    >= period * vike_indicators::SMOOTHED_FACTOR,
                "`{}`'s retention fell below its own decay bound",
                meta.name
            );
            checked += 1;
        }
    }
    assert!(checked >= 2, "expected the two decay-bounded rows, saw {checked}");
}

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

// =============================================================================================
// The constant-window law: one spelling, and a declared disposition for every site of the fold
// =============================================================================================

/// The squared-deviation SHAPE, and the two spellings it wears in this crate: a call to
/// `crates/vike-indicators/src/math.rs`'s `sq`, or an explicit self-multiplication `X * X` whose
/// two operands are the SAME source text. [`is_variance_fold_line`] is the one place that decides;
/// this constant only names the shape for the failure messages below.
///
/// ⚠ **WIDENED 2026-08-25, because the previous marker was self-defeating.** It read
/// `const VARIANCE_FOLD_MARKER: &str = "| sq(";` — the CLOSURE form alone — and before that
/// `".powi(2)).sum::<f64>()"`, and its doc claimed "every rolling variance in this crate is
/// spelled exactly this way, which is what makes the set of sites enumerable by reading the
/// source". That claim was false. Both spellings were derived by reading the folds that existed on
/// the day they were written, `"| sq("` matched exactly the eight sites [`VARIANCE_FOLD_SITES`]
/// then declared, and the table therefore passed while SEVEN live squared-deviation folds sat
/// outside it — `pairs.rs`'s `batch_spread_zscore`, `batch_correl`, `batch_correl_log` and
/// `batch_half_life`, and `statistics.rs`'s `std_error_series`, `batch_skew` and `batch_kurtosis`.
/// Every one of them carries the `== 0.0` / `!= 0.0` output guard the `Residual` rows warn about,
/// which is to say the gate was blind to precisely the population it was built to enumerate.
///
/// ⚠ **And it was getting BLINDER, not stabler.** The sibling source gate
/// `crates/vike-indicators/tests/libm_platform_probe.rs`'s
/// `production_code_calls_libm_not_the_platform` now bans `powi` outright, so the spelling an
/// author reaches for when writing a new squared deviation by hand is exactly `(x - m) * (x - m)`
/// — the one shape the closure marker could not see. A gate that goes blind in the direction its
/// sibling gate pushes authors is worse than no gate, because it certifies.
///
/// ⚠ **A THIRD spelling joined the shape on 2026-08-26: `powi(`, at every arity and in every
/// qualification.** [`is_variance_fold_line`] carries the argument. In one line: the enumeration
/// was resting on the sibling gate for the `powi` case, that gate had a hole (it banned the method
/// spelling only, so `f64::powi(d, 2)` walked past both), and an enumeration that depends on
/// another file's configuration is not an enumeration. It adds no row today.
const VARIANCE_FOLD_SHAPE: &str = "a `sq(` call, a `powi(` call, or a self-multiplied `X * X`";

/// Is this line a `fn` DECLARATION rather than a body line?
///
/// Declaration lines are excluded from the scan for two independent reasons, and both are load
/// bearing. `crates/vike-indicators/src/math.rs`'s `pub(crate) fn sq(x: f64) -> f64` contains the
/// literal `sq(` and would otherwise match the first half of the shape; and [`enclosing_fn`]
/// searches STRICTLY ABOVE the hit line, so a hit landing on a declaration would resolve to the
/// PREVIOUS function and file its row under the wrong name.
///
/// The prefix list is the same one [`enclosing_fn`] recognises, deliberately: a visibility this
/// list misses is a function this gate cannot name either way, so the two must agree.
fn is_fn_decl_line(trimmed: &str) -> bool {
    ["fn ", "pub fn ", "pub(crate) fn ", "pub(super) fn "].iter().any(|p| trimmed.starts_with(p))
}

/// Does this line multiply some expression by ITSELF — `d * d`, `sq(x - m) * sq(x - m)`,
/// `(lag[j] - mean_l) * (lag[j] - mean_l)`?
///
/// The rule is deliberately TEXTUAL and deliberately narrow: both operands must be the same source
/// text, character for character. `a * b` with different operands is ordinary arithmetic and must
/// not redden this gate — `crates/vike-indicators/src/pairs.rs`'s `batch_half_life` folds its
/// covariance as `(lag[j] - mean_l) * (dif[j] - mean_d)` on the line directly above the variance
/// this gate wants, and a matcher that could not tell those two apart would have made the whole
/// table noise.
///
/// An operand is scanned outward from the `*`: a run of identifier characters (`[A-Za-z0-9_.[]]`),
/// or a BALANCED parenthesised group optionally preceded by a callee name. The parenthesis walk is
/// what buys the widening its most important hit — `batch_half_life` spells its `var_l` as
/// `(lag[j] - mean_l) * (lag[j] - mean_l)`, which an identifier-only scan stops at the closing
/// paren and misses entirely.
///
/// ⚠ **What it CANNOT see, stated rather than implied:**
/// - **A fold wrapped across two lines.** The scan is line-by-line, so `(x - mean)` on one line and
///   `* (x - mean)` on the next is invisible. Measured on the tree this widening was written
///   against: no production line under `src/` ends in a bare `*` or begins with a binary `*`, so
///   nothing is hidden by it today — but a `max_width` change or a longer identifier could wrap one
///   tomorrow, and nothing here would notice.
/// - **The same VALUE under two names.** `d * d_copy`, `x * y` where `y = x`, or `a[i] * a[j]` with
///   `i == j` at runtime are all squared deviations this returns `false` for. Textual equality is
///   the whole rule.
/// - **A square reached through anything but infix `*`, `sq(` or `powi(`.** `x.mul(x)`,
///   `f64::mul(x, x)`, `iter.product()` over a doubled iterator, a `dot(v, v)` helper.
/// - ⚠ **A square through a general POWER — `libm::pow(d, 2.0)`.** This is the blind spot worth
///   knowing about, because it is the only one that is simultaneously invisible HERE and PERMITTED
///   by the sibling source gate: `libm::pow` is the sanctioned spelling of a power in this
///   workspace, so `crates/vike-indicators/tests/libm_platform_probe.rs`'s
///   `production_code_calls_libm_not_the_platform` will never object to it, and it contains
///   neither `sq(` nor `powi(` nor a repeated operand. Every OTHER power spelling of a square —
///   `.powf(2.0)`, `f64::powf(d, 2.0)` — is banned outright by that gate, so it cannot come back
///   silently. Measured 2026-08-26: `crates/vike-indicators/src` contains no `libm::pow` call at
///   all, so this is a gap rather than an omission from the table.
/// - **Higher moments.** `crates/vike-indicators/src/math.rs`'s `cube` and `quart` CALL sites are
///   not matched; only those two functions' own bodies are, through their `x * x`. Today every
///   third- and fourth-moment fold in the crate sits inside a function that ALSO folds an `m2`
///   (`crates/vike-indicators/src/indicators/statistics.rs`'s `batch_skew` and `batch_kurtosis`),
///   so each already has a row — but a future kernel folding only a cube would be invisible here.
///   ⚠ `powi(` is the ONE exception now that [`is_variance_fold_line`] carries it: a `powi(3)` or
///   `powi(4)` matches at every arity and demands a row, exactly as the sibling gate bans `powi`
///   at every arity. That asymmetry against `cube(`/`quart(` is deliberate — those two are the
///   crate's own reviewed helpers, and `powi` is the spelling that must not reappear.
///
/// ⚠ **CORRECTED 2026-08-26 — the `.powi(2)` bullet above used to read "`.powi(2)` is the one such
/// spelling that CANNOT come back silently, and only because a different gate bans it".** Both
/// halves have moved. The claim named only the METHOD spelling, and until the same day nothing
/// banned the fully-qualified `f64::powi(x, 2)` at all — so the one spelling this list called safe
/// had a twin that was neither banned there nor visible here. And the dependency the sentence
/// admitted to ("only because a different gate bans it") is now gone in the direction that matters:
/// [`is_variance_fold_line`] matches `powi(` itself, so this table's enumeration no longer rests on
/// a gate in another file staying configured a particular way.
fn self_multiplies(line: &str) -> bool {
    let c: Vec<char> = line.chars().collect();
    if c.len() < 3 {
        return false;
    }
    let is_operand = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '[' | ']');
    for i in 1..c.len() - 1 {
        if c[i] != '*' {
            continue;
        }
        // `*/` and `/*` are comment delimiters and `**` is not Rust at all; a leading `*` deref is
        // excluded by starting the sweep at 1. None of the four is a product.
        if c[i - 1] == '*' || c[i + 1] == '*' || c[i - 1] == '/' || c[i + 1] == '/' {
            continue;
        }

        // ---- left operand, scanned right-to-left from the `*`.
        let mut j = i;
        while j > 0 && c[j - 1] == ' ' {
            j -= 1;
        }
        let end = j;
        if j > 0 && c[j - 1] == ')' {
            let mut depth = 0usize;
            while j > 0 {
                match c[j - 1] {
                    ')' => depth += 1,
                    '(' => {
                        depth -= 1;
                        if depth == 0 {
                            j -= 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j -= 1;
            }
            // ...and the callee name in front of it, so `sq(x - m)` is one operand, not two.
            while j > 0 && is_operand(c[j - 1]) {
                j -= 1;
            }
        } else {
            while j > 0 && is_operand(c[j - 1]) {
                j -= 1;
            }
        }
        let left: String = c[j..end].iter().collect();
        if left.is_empty() {
            continue;
        }

        // ---- right operand, scanned left-to-right from the `*`.
        let mut k = i + 1;
        while k < c.len() && c[k] == ' ' {
            k += 1;
        }
        let start = k;
        while k < c.len() && is_operand(c[k]) {
            k += 1;
        }
        if k < c.len() && c[k] == '(' {
            let mut depth = 0usize;
            while k < c.len() {
                match c[k] {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            k += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
        }
        let right: String = c[start..k].iter().collect();
        if right.is_empty() {
            continue;
        }

        if left == right {
            return true;
        }
    }
    false
}

/// The whole predicate: a production line carrying a squared deviation in any of its spellings.
///
/// The `sq(` half is a bare `contains`, on purpose. It matches the closure form the old marker
/// keyed on, the statement form `residuals_sq += sq(...)` it missed, and any path-qualified
/// spelling (`math::sq(`, `crate::math::sq(`) — which is why [`self_multiplies`]'s operand
/// alphabet does not need to carry `:`. `.sqrt(` does not contain `sq(`, so the obvious
/// false-positive does not arise.
///
/// ⚠ **The `powi(` half is new as of 2026-08-26, and it is a bare `contains` for the SAME reason
/// the `sq(` half is** — it has to see `x.powi(2)`, `f64::powi(x, 2)` and any path-qualified
/// variant with one needle, and anchoring it on a leading `.` would see only the first of those.
/// That matters because the fully-qualified spelling is exactly the one that was, until the same
/// day, banned by nothing: `crates/vike-indicators/tests/libm_platform_probe.rs`'s `BANNED_FNS`
/// covered `.powi(` alone, so `f64::powi(d, 2)` passed the sibling gate AND was invisible here.
/// Both holes are closed, and closing them in two places rather than one is deliberate — this
/// table's job is to ENUMERATE every squared-deviation fold, and an enumeration that is only
/// complete while a gate in another crate's test directory stays configured a particular way is
/// not an enumeration.
///
/// It matches at EVERY arity, mirroring the sibling gate's decision to ban `powi` at every arity;
/// [`self_multiplies`]'s blind-spot list states what that does to the higher-moment case.
/// Measured 2026-08-26: every `powi(` under `crates/vike-indicators/src` sits in a `//` comment
/// (`math.rs`'s three "never `x.powi(k)`" doc lines and `statistics.rs`'s module header), and the
/// scan drops comment lines before consulting this predicate — so the widening adds no row to
/// [`VARIANCE_FOLD_SITES`] and changes no verdict. It removes a gap, as the `sq(`-only marker's
/// own history says a matcher in this file eventually needs.
fn is_variance_fold_line(line: &str) -> bool {
    line.contains("sq(") || line.contains("powi(") || self_multiplies(line)
}

/// The index of the first line that begins a file's `#[cfg(test)]` section, or `lines.len()`.
///
/// ⚠ **Excluding test code is a HARD requirement of the widened matcher, not tidiness.** Test
/// fixtures multiply values constantly, and one of them is an exact copy of a production kernel:
/// `crates/vike-indicators/src/pairs.rs`'s `spread_zscore_beta_one_is_bit_identical_to_unhedged`
/// keeps a `legacy` reimplementation of `batch_spread_zscore` — `run_sum2 += s[i] * s[i]` and
/// `var = run_sum2 / period as f64 - mean * mean` — inside its `#[cfg(test)] mod tests`. Without
/// this cut that pin would demand two more rows describing a function that ships nowhere.
///
/// Comment lines are skipped BEFORE the cut is looked for, which is the difference between this
/// and the byte-offset `text.find("#[cfg(test)]")` its sibling
/// `crates/vike-indicators/tests/libm_platform_probe.rs`'s
/// `production_code_calls_libm_not_the_platform` uses: a doc comment that merely NAMES the
/// attribute — this very paragraph would be one, were it under `src/` — would otherwise truncate
/// the scan at the mention and let everything below it pass unread.
///
/// The cut is per FILE and takes everything below the first marker, so a production function
/// placed after a test module is invisible. Measured on the tree this was written against: of the
/// ten files under `src/` carrying a `#[cfg(test)]`, none declares a function below it.
fn production_cut(lines: &[&str]) -> usize {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        if t.starts_with("#[cfg(test)]") {
            return i;
        }
    }
    lines.len()
}

/// What a given fold site does about a window whose values are all bit-equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FoldGuard {
    /// Calls `window::is_constant_window` before folding — the one spelling of the law.
    Guarded,
    /// Exactly zero on a constant window WITHOUT the predicate, for a STRUCTURAL reason stated in
    /// the row. Not "measured zero once": a reason that cannot stop being true.
    ExactByConstruction,
    /// Carries the defect today. Declared with the reasoning that makes it a defect, and with its
    /// measured magnitude WHERE ONE HAS BEEN TAKEN. Deliberately not fixed here.
    ///
    /// ⚠ This doc used to read "declared with its measured value", full stop. That was accurate
    /// while the table held three residuals, all of them measured by hand before it was written;
    /// it stopped being accurate when the widened matcher above surfaced seven more in one sweep.
    /// Requiring a measurement per row would have meant either withholding the finding until seven
    /// numbers existed, or — far worse — declaring the sites as something they are not so the
    /// table could go green. A row that states the structural argument and admits the magnitude is
    /// unmeasured is worth strictly more than a row that does not exist.
    Residual,
    /// The SHAPE matched, but the site is not a fold over a window's values at all, so the
    /// constant-window question does not arise for it.
    ///
    /// Two families land here. The squaring PRIMITIVES themselves —
    /// `crates/vike-indicators/src/math.rs`'s `sq` / `cube` / `quart` — are the definitions every
    /// other row folds THROUGH; the matcher cannot tell a definition from a use, and they are
    /// declared here rather than special-cased in the scanner so the exclusion is visible in the
    /// table instead of buried in a predicate. And a squared quantity computed from the window's
    /// SHAPE rather than its VALUES — an integer index grid, a period, a fixed parameter — cannot
    /// be made rounding-sized by a constant window, because the window's values never enter it.
    NotAWindowFold,
}

/// ⚠ **One row per `(file, enclosing fn)` carrying an [`is_variance_fold_line`] hit under `src/`,
/// the NUMBER of hit lines in that function, and what it does about a constant window.**
///
/// This table exists because the defect it guards lived in the gap between two files that agreed in
/// prose. `crates/vike-indicators/src/window.rs` gained `is_constant_window` and documented the law
/// at length; `crates/vike-indicators/src/indicators/volatility.rs` carried three hand-rolled
/// copies of the same fold that never called it, and nothing anywhere compared those two facts.
///
/// ⚠ **The claim this doc used to make here was "a new fold — or a new hand-rolled copy of an
/// existing one — now reddens this test until it is classified, which is the only mechanism here
/// that scales past the sites known today." It did not scale, and it was not true.** It held only
/// for a fold spelled through a `|x| sq(` closure; a fold spelled as an explicit multiplication was
/// invisible, and seven of them already existed when that sentence was written. The claim is true
/// again as of the widening — for the three spellings [`self_multiplies`] and
/// [`is_variance_fold_line`] enumerate (`sq(`, `powi(`, and a self-multiplied `X * X`), and no
/// further; read their blind-spot lists before trusting it a third time. The one to read first is
/// `libm::pow(d, 2.0)`: it is the only square spelling this file cannot see that the sibling
/// source gate also permits.
///
/// The hit COUNT is the third field for the same reason the table exists at all. Keyed on
/// `(file, fn)` alone, a second hand-rolled fold added inside an ALREADY-declared function would
/// change nothing the gate compares — which is the failure mode this whole file is about, one level
/// down. `crates/vike-indicators/src/pairs.rs`'s `batch_correl` folds five such lines and
/// `batch_correl_log` three, so the count is doing real work rather than pinning a formatting
/// detail. A count that moves is a real change to how many squared deviations that function
/// computes: open the line and classify it before editing the number.
///
/// ⚠ Rows are keyed on the function's NAME within its file, so two same-named functions in one file
/// that BOTH fold — several `on_bar` impls live in
/// `crates/vike-indicators/src/indicators/base.rs` — would collapse into one row with a summed
/// count. Exactly one of them folds today. The narrow-marker table had the same property and never
/// stated it.
///
/// The `Residual` rows are the honest part: they are live instances of the same defect, left
/// because each needs a change this one does not carry.
const VARIANCE_FOLD_SITES: &[(&str, &str, usize, FoldGuard, &str)] = &[
    // ---- src/indicators/base.rs -------------------------------------------------------------
    (
        "src/indicators/base.rs",
        "batch_bollinger",
        1,
        FoldGuard::Residual,
        "`bollinger`'s three band LEVELS. At a 20-bar window flat at 0.1, `upper - lower` is \
         5.551115123125783e-17 rather than 0. Nothing divides by that gap and no NaN-vs-number \
         guard keys on it, so the consequence is a band narrower than the display precision rather \
         than an invented reading — unlike `bbands_pctb`, which is why this one waited. Fixing it \
         also has to move the streaming twin below in the same commit, so it is two coordinated \
         edits rather than one.",
    ),
    (
        "src/indicators/base.rs",
        "on_bar",
        1,
        FoldGuard::Residual,
        "The streaming twin of `batch_bollinger` above, folding the same two-pass variance over a \
         `VecDeque` window. It has to gain the predicate in the same commit as the batch kernel: \
         `on_bar_equals_vectorize_for_every_indicator` compares the two bit-for-bit, so fixing \
         either one alone turns this declared residual into a red test.",
    ),
    // ---- src/indicators/overlap.rs ----------------------------------------------------------
    (
        "src/indicators/overlap.rs",
        "batch_alma",
        1,
        FoldGuard::NotAWindowFold,
        "ALMA's Gaussian weight kernel, `exp(-(k - m)² / 2s²)`, which matches BOTH halves of the \
         shape on one line — `sq(k as f64 - m)` and `s * s`. Neither square reads a bar: `k` is a \
         loop index, `m` the offset centre and `s` the sigma, all three functions of `period` / \
         `offset` / `sigma` alone. The weights are identical for every window the indicator will \
         ever see, constant or not, so there is no rounding-sized non-zero for a predicate to \
         catch. Newly visible under the widened matcher; the closure marker never saw it because \
         the `sq` call sits inside an `exp`, not directly behind the `|k|`.",
    ),
    // ---- src/indicators/statistics.rs -------------------------------------------------------
    (
        "src/indicators/statistics.rs",
        "ols",
        1,
        FoldGuard::NotAWindowFold,
        "The OLS normal-equation denominator `p·Σx² − (Σx)²`, spelled `p as f64 * sx2 - sx * sx`. \
         It is a variance in form, but of the integer index grid `x = 0..p-1` rather than of the \
         data: `sx` and `sx2` are exact integer closed forms cast to f64 and depend only on \
         `period`. The window's values never enter it, so its `if denom == 0.0` branch is a real \
         degeneracy test on the grid rather than an epsilon a constant window can fool.",
    ),
    (
        "src/indicators/statistics.rs",
        "std_error_series",
        1,
        FoldGuard::Residual,
        "`std_error`, and `std_error_bands` through it: `Σ(window[j] − (a + b·j))²` around the OLS \
         fit, spelled `residuals_sq += sq(...)` — a STATEMENT rather than a closure, which is the \
         entire reason the old `| sq(` marker walked past it. On a constant window the fitted \
         slope is only APPROXIMATELY zero (`p·Σxy` and `Σx·Σy` are different fold orders of the \
         same quantity and cancel to rounding, not exactly), so every residual is rounding-sized \
         and `se` is a small positive number where zero is correct. Nothing divides by it, so the \
         consequence is `batch_bollinger`'s rather than `bbands_pctb`'s: bands narrower than the \
         display precision, not an invented reading. Magnitude not yet measured.",
    ),
    (
        "src/indicators/statistics.rs",
        "batch_skew",
        1,
        FoldGuard::Residual,
        "`skew`'s second moment, `m2 = Σd²/p` over `d = x − mean`, guarded by `if m2 == 0.0` — the \
         output epsilon the law exists to replace, in its purest form. Sharper than it looks: the \
         guard being fooled does not merely perturb the answer, it DIVIDES rounding by rounding. \
         `m3 / cube(sd)` has a numerator of order ε³ and a denominator of order ε³, so a window \
         that never moved reports an order-1 skew indistinguishable from a real one. Newly \
         visible: `|d| d * d` is the explicit multiplication the narrow marker could not see. \
         Magnitude not yet measured.",
    ),
    (
        "src/indicators/statistics.rs",
        "batch_kurtosis",
        2,
        FoldGuard::Residual,
        "`kurtosis`'s twin of `batch_skew` above, same `|d| d * d` fold and same `if m2 == 0.0` \
         guard, dividing `m4 / quart(sd)` — order ε⁴ over order ε⁴. The second hit line is the \
         bias-correction term `((p - 1) * (p - 1))`, integer period arithmetic that matches the \
         shape and folds nothing; it is counted rather than filtered because a matcher narrow \
         enough to drop it would be narrow enough to drop a real fold. Magnitude not yet measured.",
    ),
    (
        "src/indicators/statistics.rs",
        "batch_rank_correlation",
        2,
        FoldGuard::ExactByConstruction,
        "Spearman's `sum_d2 += d * d` over INTEGER rank differences, plus `p2 = p * p`. On a \
         constant window every comparison is a tie, the stable sort therefore leaves the original \
         index order, `price_rank[j]` is exactly `j + 1`, every `d` is exactly `0i64` and the sum \
         is exactly zero — in `i64`, with no float anywhere in the fold to round. That is a \
         property of the reduction and of the sort's documented stability, not a measurement that \
         could quietly stop holding.",
    ),
    // ---- src/indicators/volatility.rs -------------------------------------------------------
    (
        "src/indicators/volatility.rs",
        "stddev_series",
        1,
        FoldGuard::Guarded,
        "`stddev`, and `relative_volatility` through it.",
    ),
    (
        "src/indicators/volatility.rs",
        "bollinger_vals",
        1,
        FoldGuard::Guarded,
        "`bbands_width` and `bbands_pctb`.",
    ),
    (
        "src/indicators/volatility.rs",
        "batch_hvol",
        1,
        FoldGuard::Guarded,
        "`hvol`, over the LOG RETURNS rather than the closes.",
    ),
    (
        "src/indicators/volatility.rs",
        "batch_ulcer",
        1,
        FoldGuard::ExactByConstruction,
        "`ulcer` centres its deviations on `peak`, the window MAXIMUM. A maximum is a SELECTION \
         from the window, not an average of it, so on a constant window `peak` IS the repeated \
         value bit-for-bit and every `c - peak` is exactly 0 with no rounding to cancel. That is a \
         property of the reduction, not a measurement that could quietly stop holding.",
    ),
    // ---- src/math.rs ------------------------------------------------------------------------
    (
        "src/math.rs",
        "sq",
        1,
        FoldGuard::NotAWindowFold,
        "The crate's squaring PRIMITIVE itself — `x * x` is the definition every `sq(` row in this \
         table folds THROUGH, not a fold over any window. It appears here because the matcher \
         cannot tell a definition from a use, and it is declared rather than filtered out in the \
         scanner so that the exclusion is a row a reader can see instead of a predicate they have \
         to go find.",
    ),
    (
        "src/math.rs",
        "cube",
        1,
        FoldGuard::NotAWindowFold,
        "`x³` spelled from the square outward, `let s = x * x; s * x` — matched on the first of \
         those two lines. Same disposition and same reason as `sq` above.",
    ),
    (
        "src/math.rs",
        "quart",
        2,
        FoldGuard::NotAWindowFold,
        "`x⁴` as `(x²)²`, `let s = x * x; s * s` — BOTH lines match, which is why the count is 2. \
         Same disposition and same reason as `sq` above. That the second line is itself a \
         self-multiplication is the clearest small proof that the widened matcher keys on the \
         SHAPE rather than on a variance idiom.",
    ),
    // ---- src/pairs.rs -----------------------------------------------------------------------
    (
        "src/pairs.rs",
        "batch_spread_zscore",
        2,
        FoldGuard::Residual,
        "`spread_zscore`, and the worst-behaved fold in this table: an ACCUMULATED variance, \
         `run_sum2 / period − mean * mean`, guarded by `if sd != 0.0` and then DIVIDED by \
         (`(s[i] − mean) / sd`). The accumulated form does not merely fail to reach zero on a \
         constant spread, it subtracts two nearly-equal large numbers, which is why the code \
         already carries `.max(0.0)` — the difference can come out NEGATIVE. So the guard is \
         fooled and a spread that never moved reports an arbitrary z-score. Newly visible: both \
         hit lines (`s[i] * s[i]` and `mean * mean`) are explicit multiplications. Magnitude not \
         yet measured, and it is price-level-dependent by construction.",
    ),
    (
        "src/pairs.rs",
        "batch_beta",
        1,
        FoldGuard::Residual,
        "`beta`'s denominator variance, guarded by `if vb != 0.0` — the SAME NaN-vs-number branch \
         `bbands_pctb` has. A benchmark whose returns are bit-identical across the window — a \
         stalled or halted feed, the case a beta is least meaningful in — gives `vb` a \
         rounding-sized value instead of zero, so the guard is fooled and `cov / vb` returns an \
         arbitrary large number where NaN is correct. ⚠ This row used to call itself \"the \
         sharpest of the three residuals\"; that was true only of the three the narrow marker \
         could SEE. The widened scan puts four more of the same `!= 0.0` shape beside it, three of \
         them in this very file, and `batch_spread_zscore` above is sharper still.",
    ),
    (
        "src/pairs.rs",
        "batch_correl",
        5,
        FoldGuard::Residual,
        "`correl`'s accumulated closed form, `p·Σa² − (Σa)²` per leg, guarded by \
         `if denom != 0.0`. Five hit lines: the two running `+=` squares, the two `-=` squares \
         that evict the leaving bar, and the denominator that closes both. Same catastrophic \
         cancellation as `batch_spread_zscore`, with one mitigation the others lack — the result \
         is `.clamp(-1.0, 1.0)`, so a fooled guard yields a bounded nonsense correlation rather \
         than an unbounded one. Magnitude not yet measured.",
    ),
    (
        "src/pairs.rs",
        "batch_correl_log",
        3,
        FoldGuard::Residual,
        "`correl_log`, the log-return twin of `batch_correl` above: the same `p·Σa² − (Σa)²` \
         closed form and the same `if denom != 0.0` guard, folded over a `Vec` buffer per window \
         instead of a running accumulator, so it is three hit lines rather than five. Also \
         `.clamp(-1.0, 1.0)`-bounded. Magnitude not yet measured.",
    ),
    (
        "src/pairs.rs",
        "batch_half_life",
        1,
        FoldGuard::Residual,
        "`half_life`'s regressor variance `var_l`, and the single most valuable thing the widening \
         found. It is spelled `(lag[j] - mean_l) * (lag[j] - mean_l)` — the parenthesised \
         self-multiplication, the exact spelling the narrow marker could not see and the one \
         `crates/vike-indicators/tests/libm_platform_probe.rs`'s \
         `production_code_calls_libm_not_the_platform` ban on `.powi(` now steers authors \
         toward. It is guarded by `if var_l != 0.0` and then divided by, twice over: \
         `lambda = cov / var_l`, reported as \
         `−ln 2 / lambda`. ⚠ And this function's own doc comment PROMISES the opposite behaviour \
         — \"a degenerate window (`var(s_lag) == 0`) has no slope at all\", both it and a \
         non-reverting window yielding \"NaN — the signal to stand down rather than a number to \
         trade on\". A rounding-sized `var_l` turns that documented refusal into a finite \
         half-life. Magnitude not yet measured.",
    ),
    // ---- src/window.rs ----------------------------------------------------------------------
    (
        "src/window.rs",
        "rolling_var",
        1,
        FoldGuard::Guarded,
        "`var` and `zscore`, plus the research feature presets. The original home of the law.",
    ),
];

/// Recursively collect `.rs` files under `dir`.
fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The index of the `fn` item enclosing `line_idx`, and its name.
fn enclosing_fn(lines: &[&str], line_idx: usize) -> (usize, String) {
    for j in (0..line_idx).rev() {
        let t = lines[j].trim_start();
        let after = t
            .strip_prefix("fn ")
            .or_else(|| t.strip_prefix("pub fn "))
            .or_else(|| t.strip_prefix("pub(crate) fn "))
            .or_else(|| t.strip_prefix("pub(super) fn "));
        if let Some(rest) = after {
            let name = rest.split(['(', '<']).next().unwrap_or(rest).trim().to_string();
            return (j, name);
        }
    }
    (0, "<no enclosing fn>".to_string())
}

/// ⚠ **The constant-window law must have exactly ONE spelling, and every fold must be classified.**
///
/// Scans the production half of every file under `src/` for [`is_variance_fold_line`], resolves
/// each hit to its enclosing `fn`, counts the hits per function, and decides `Guarded` by looking
/// for an `is_constant_window` call between that `fn` and the FIRST hit inside it. The set of
/// `(file, fn, hit count)` triples found must equal [`VARIANCE_FOLD_SITES`] exactly — a new fold, a
/// moved fold, a deleted fold, an extra fold inside an already-declared function, or a changed
/// disposition all redden it.
///
/// Taking the guard from the FIRST hit is the conservative direction rather than a shortcut: the
/// call has to precede every hit in the function in order to precede the first one, so a function
/// that guards one fold and forgets another scans as `Residual` and has to be classified by hand.
///
/// ⚠ **Non-vacuity, and exactly how much of it has been re-verified.** The narrow-marker version of
/// this gate was mutation-tested in both directions: deleting the `is_constant_window` call from
/// any `Guarded` row flipped it to `Residual` and failed, and adding a hand-rolled variance fold
/// anywhere under `src/` failed until its row existed. The mechanism is unchanged by the widening
/// and those two arms still hold. The widened matcher's OWN arms — a self-multiplied fold added
/// under `src/`, and a second hit added inside an already-declared function so the count moves —
/// have NOT been re-run through mutation, because no `cargo` invocation was available where the
/// widening was written. Re-run them; do not assume them from this paragraph.
///
/// The two emptiness assertions guard the degenerate case where the matcher is refactored away and
/// the scan silently checks nothing while still passing — a shape this repo has shipped before, and
/// the shape the narrow marker was one refactor from re-entering.
#[test]
fn is_constant_window_is_the_only_spelling_of_the_constant_window_law() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    files.sort();

    let mut found: Vec<(String, String, usize, FoldGuard)> = Vec::new();
    let mut self_mul_hits = 0usize;
    for path in &files {
        let text = std::fs::read_to_string(path).expect("read source");
        let lines: Vec<&str> = text.lines().collect();
        let cut = production_cut(&lines);
        let rel = path
            .strip_prefix(&root)
            .expect("under manifest dir")
            .to_string_lossy()
            .replace('\\', "/");

        // `(fn name, hit count, disposition)`, in first-hit order within the file.
        let mut per_fn: Vec<(String, usize, FoldGuard)> = Vec::new();
        for (i, line) in lines.iter().enumerate().take(cut) {
            let t = line.trim_start();
            // A comment naming the shape is prose; an import naming `sq` is not a fold site; a
            // `fn` declaration line is skipped for the two reasons `is_fn_decl_line`'s doc gives.
            if t.starts_with("//") || t.starts_with("use ") || is_fn_decl_line(t) {
                continue;
            }
            if !is_variance_fold_line(line) {
                continue;
            }
            if self_multiplies(line) {
                self_mul_hits += 1;
            }
            let (start, name) = enclosing_fn(&lines, i);
            match per_fn.iter_mut().find(|(n, _, _)| *n == name) {
                Some(entry) => entry.1 += 1,
                None => {
                    // Everything between the enclosing `fn` and the fold is where the guard sits.
                    let guarded = lines[start..i]
                        .iter()
                        .any(|l| l.contains("is_constant_window(") && !l.contains(" fn "));
                    let disposition =
                        if guarded { FoldGuard::Guarded } else { FoldGuard::Residual };
                    per_fn.push((name, 1, disposition));
                }
            }
        }
        for (name, count, guard) in per_fn {
            found.push((rel.clone(), name, count, guard));
        }
    }

    assert!(
        !found.is_empty(),
        "no site matching {VARIANCE_FOLD_SHAPE} was found under src/ — the shape this gate keys \
         on was refactored away, so the gate is now checking nothing while still passing. \
         Re-derive it from the current spelling of the two-pass variance fold; do not delete this \
         test."
    );
    assert!(
        self_mul_hits > 0,
        "the `sq(` half of the shape matched but `self_multiplies` matched NOTHING under src/. \
         That half is the whole point of the 2026-08-25 widening — `pairs.rs`'s `batch_half_life` \
         spells its variance `(lag[j] - mean_l) * (lag[j] - mean_l)` and is seen by no other rule \
         here — so zero hits means the operand scanner is broken and the gate has silently \
         reverted to the blind spot it was written to close."
    );

    let mut declared: Vec<(String, String, usize, FoldGuard)> = VARIANCE_FOLD_SITES
        .iter()
        .map(|(f, n, c, g, _)| (f.to_string(), n.to_string(), *c, *g))
        .collect();
    declared.sort();
    let mut seen = found.clone();
    seen.sort();

    let declared_keys: Vec<(&String, &String, usize)> =
        declared.iter().map(|(f, n, c, _)| (f, n, *c)).collect();
    let seen_keys: Vec<(&String, &String, usize)> =
        seen.iter().map(|(f, n, c, _)| (f, n, *c)).collect();
    assert_eq!(
        seen_keys, declared_keys,
        "the set of squared-deviation folds under src/ changed. Every site needs a row in \
         VARIANCE_FOLD_SITES — `(file, enclosing fn, hit lines, disposition, why)` — saying what \
         it does about a constant window: `Guarded` (calls `window::is_constant_window`), \
         `ExactByConstruction` (with a structural reason), `NotAWindowFold` (the shape matched but \
         nothing folds a window's VALUES) or `Residual` (with the reasoning, and the magnitude if \
         one was measured). A new fold is not a formatting detail: it is a new copy of a law that \
         has already shipped this bug three times, and a count that moved means a function grew \
         one."
    );

    // `ExactByConstruction`, `NotAWindowFold` and `Residual` are indistinguishable to a scanner —
    // all three simply lack the call — so the disposition is only asserted where the scan can
    // actually tell: either side claiming `Guarded`.
    for ((f, n, _, want), (_, _, _, got)) in declared.iter().zip(seen.iter()) {
        if *want == FoldGuard::Guarded || *got == FoldGuard::Guarded {
            assert_eq!(
                want, got,
                "{f}'s `{n}` is declared {want:?} but scans as {got:?}. A `Guarded` row that lost \
                 its `is_constant_window` call is the exact regression this gate exists for; a row \
                 that gained one should be promoted to `Guarded` in the table."
            );
        }
    }
}
