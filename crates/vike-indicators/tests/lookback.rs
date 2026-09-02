//! THE warm-up gate: for EVERY registry indicator, at several parameter sets, the
//! measured first-non-NaN index of `vectorize` over a long synthetic series must
//! agree with [`Indicator::lookback`] — `==` for real (exact) overrides, `>=` for
//! the conservative `0` default. Failure messages print the measured value so
//! coverage can be ratcheted: flip an indicator to a real override, rerun, read
//! the number the gate prints.
//!
//! Mirrors `tests/parity.rs`'s synthetic-series harness (same generator shape) so
//! the two gates see the same data.

use vike_indicators::{pair_registry, registry};
use vike_model::Bar;

/// Deterministic, varied OHLCV — same generator as `tests/parity.rs`.
fn synth_bars(n: usize) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(n);
    let mut prev_close = 100.0f64;
    for i in 0..n {
        let t = i as f64;
        let mut close =
            100.0 + (t * 0.07).sin() * 8.0 + (t * 0.017).cos() * 4.0 + (t * 0.31).sin() * 1.5;
        if i > 0 && i % 17 == 0 {
            close = prev_close;
        }
        let open = prev_close;
        let high = open.max(close) + (i % 5) as f64 * 0.3 + 0.5;
        let low = open.min(close) - (i % 7) as f64 * 0.25 - 0.5;
        let volume = 1000.0 + (i % 13) as f64 * 50.0;
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

/// The measured FULL warm-up: the first index at which EVERY output line is
/// non-NaN. `None` when no such index exists on this series.
fn first_all_non_nan(cols: &[Vec<f64>]) -> Option<usize> {
    let len = cols.iter().map(|c| c.len()).min().unwrap_or(0);
    (0..len).find(|&i| cols.iter().all(|c| !c[i].is_nan()))
}

/// The measured warm-up: the first index at which ANY output line is non-NaN.
/// `None` when the whole batch is NaN (an indicator that never warmed up on this
/// series — data-dependent structure series can do this).
fn first_non_nan(cols: &[Vec<f64>]) -> Option<usize> {
    let len = cols.iter().map(|c| c.len()).min().unwrap_or(0);
    (0..len).find(|&i| cols.iter().any(|c| !c[i].is_nan()))
}

/// Parameter sets to probe: the declared defaults, plus each param scaled down and
/// up, so an override that merely hardcodes the default's answer cannot pass.
fn param_sets(specs: &[vike_indicators::ParamSpec]) -> Vec<Vec<f64>> {
    let defaults: Vec<f64> = specs.iter().map(|s| s.default).collect();
    if specs.is_empty() {
        return vec![vec![]];
    }
    let mut sets = vec![defaults.clone()];
    for (i, spec) in specs.iter().enumerate() {
        for factor in [0.5f64, 1.7f64] {
            let mut p = defaults.clone();
            let v = (spec.default * factor).round().clamp(spec.min, spec.max);
            // Keep non-period params (fractional steps) at their default — scaling a
            // multiplier does not move warm-up and can leave the range.
            if spec.step >= 1.0 && v != spec.default {
                p[i] = v;
                sets.push(p);
            }
        }
    }
    // CORNER SETS: one-at-a-time probes only ever move a multi-param formula around
    // the regime its defaults already sit in — a formula that hardcodes WHICH
    // component dominates (instead of taking a max) passes them all. Push every
    // param to its spec min with the others at max, and vice-versa, so the dominant
    // component actually changes.
    let mins: Vec<f64> = specs.iter().map(|s| s.min).collect();
    let maxs: Vec<f64> = specs.iter().map(clamp_probe).collect();
    if specs.len() > 1 {
        sets.push(mins.clone());
        sets.push(maxs.clone());
        for i in 0..specs.len() {
            let mut p = maxs.clone();
            p[i] = mins[i];
            sets.push(p);
            let mut q = mins.clone();
            q[i] = maxs[i];
            sets.push(q);
        }
    }
    sets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sets.dedup();
    sets
}

/// A spec's upper probe value: the spec max, but capped so the resulting warm-up
/// still lands inside the synthetic series (a 200-bar ROC on a 900-bar series is
/// fine; four of them stacked is not).
fn clamp_probe(spec: &vike_indicators::ParamSpec) -> f64 {
    spec.max.min(60.0).max(spec.min)
}

/// The headline gate.
#[test]
fn lookback_matches_first_non_nan_for_every_indicator() {
    let bars = synth_bars(900);
    let mut failures: Vec<String> = Vec::new();
    let mut exact = 0usize;
    let mut defaulted = 0usize;

    for meta in registry() {
        if meta.lookback_exact() {
            exact += 1;
        } else {
            defaulted += 1;
        }
        for raw in param_sets(meta.params) {
            let ind = meta.build_with(&raw);
            let cols = ind.vectorize(&bars);
            let claimed = ind.lookback();
            let Some(measured) = first_non_nan(&cols) else {
                // Never warmed up on this series: only tolerable for a defaulted
                // (data-dependent) indicator.
                if meta.lookback_exact() {
                    failures.push(format!(
                        "{} params {raw:?}: claims EXACT lookback {claimed} but vectorize is all-NaN over {} bars",
                        meta.name,
                        bars.len()
                    ));
                }
                continue;
            };
            let claimed_full = ind.lookback_full();
            assert!(
                claimed_full >= claimed,
                "{} params {raw:?}: lookback_full() {claimed_full} < lookback() {claimed}",
                meta.name
            );
            let measured_full = first_all_non_nan(&cols);
            if meta.lookback_exact() {
                if measured != claimed {
                    failures.push(format!(
                        "{} params {raw:?}: EXACT lookback() = {claimed} but measured first-non-NaN = {measured}",
                        meta.name
                    ));
                }
                // The ALL-LINES twin: a staggered multi-line indicator that leaves
                // `lookback_full` at its forwarding default fails HERE, so the
                // default can never silently under-report a seed size.
                match measured_full {
                    Some(mf) if mf != claimed_full => failures.push(format!(
                        "{} params {raw:?}: EXACT lookback_full() = {claimed_full} but measured all-lines index = {mf}",
                        meta.name
                    )),
                    None => failures.push(format!(
                        "{} params {raw:?}: EXACT lookback_full() = {claimed_full} but no index has ALL lines defined over {} bars",
                        meta.name,
                        bars.len()
                    )),
                    _ => {}
                }
            } else {
                if claimed > measured {
                    failures.push(format!(
                        "{} params {raw:?}: conservative lookback() = {claimed} EXCEEDS measured first-non-NaN = {measured}",
                        meta.name
                    ));
                }
                if let Some(mf) = measured_full {
                    if claimed_full > mf {
                        failures.push(format!(
                            "{} params {raw:?}: conservative lookback_full() = {claimed_full} EXCEEDS measured all-lines index = {mf}",
                            meta.name
                        ));
                    }
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "lookback gate: {} failure(s) ({exact} exact / {defaulted} defaulted of {} indicators)\n{}",
        failures.len(),
        registry().len(),
        failures.join("\n")
    );
}

/// Coverage ratchet: the exact-override count may only ever GROW. Bump the floor
/// when new overrides land; a drop means an override was silently lost.
#[test]
fn exact_override_coverage_floor() {
    let exact = registry().iter().filter(|m| m.lookback_exact()).count();
    assert!(
        exact >= EXACT_FLOOR,
        "exact lookback overrides dropped to {exact}, floor is {EXACT_FLOOR}"
    );
}

const EXACT_FLOOR: usize = 105;

/// The same gate for the 6 two-series (benchmark) [`PairIndicator`]s. The
/// benchmark series is the primary shifted/scaled so the pair math is non-degenerate.
#[test]
fn pair_lookback_matches_first_non_nan() {
    let primary = synth_bars(600);
    let benchmark: Vec<Bar> = primary
        .iter()
        .enumerate()
        .map(|(i, b)| Bar {
            close: b.close * 0.5 + 40.0 + (i as f64 * 0.03).sin(),
            open: b.open * 0.5 + 40.0,
            high: b.high * 0.5 + 40.5,
            low: b.low * 0.5 + 39.5,
            ..b.clone()
        })
        .collect();

    let mut failures: Vec<String> = Vec::new();
    for meta in pair_registry() {
        for raw in param_sets(meta.params) {
            let ind = (meta.make_with)(&raw);
            let cols = ind.vectorize_pair(&primary, &benchmark);
            let claimed = ind.lookback();
            let Some(measured) = first_non_nan(&cols) else {
                if meta.lookback_exact() {
                    failures.push(format!("{} params {raw:?}: all-NaN batch", meta.name));
                }
                continue;
            };
            if meta.lookback_exact() {
                if measured != claimed {
                    failures.push(format!(
                        "{} params {raw:?}: EXACT lookback() = {claimed} but measured = {measured}",
                        meta.name
                    ));
                }
            } else if claimed > measured {
                failures.push(format!(
                    "{} params {raw:?}: conservative lookback() = {claimed} EXCEEDS measured = {measured}",
                    meta.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "pair lookback gate:\n{}", failures.join("\n"));
}

/// Probe helper (ignored) for the pair seam — see `probe_measured_lookbacks`.
#[test]
#[ignore]
fn probe_measured_pair_lookbacks() {
    let primary = synth_bars(600);
    let benchmark: Vec<Bar> =
        primary.iter().map(|b| Bar { close: b.close * 0.5 + 40.0, ..b.clone() }).collect();
    for meta in pair_registry() {
        for (i, spec) in meta.params.iter().enumerate() {
            let mut raw: Vec<f64> = meta.params.iter().map(|s| s.default).collect();
            let base = first_non_nan(&(meta.make_with)(&raw).vectorize_pair(&primary, &benchmark));
            raw[i] = (spec.default + 1.0).clamp(spec.min, spec.max);
            let bump = first_non_nan(&(meta.make_with)(&raw).vectorize_pair(&primary, &benchmark));
            println!("{:<16} {} base={base:?} +1={bump:?}", meta.name, spec.name);
        }
        if meta.params.is_empty() {
            let base = first_non_nan(&(meta.make_with)(&[]).vectorize_pair(&primary, &benchmark));
            println!("{:<16} (no params) base={base:?}", meta.name);
        }
    }
}

/// A dozen-plus representative EXACT overrides, pinned to their derived formula
/// at non-default params — a hardcoded second opinion on the families the gate
/// checks generically (one per warm-up shape: `p-1`, `p`, `max(a,b)-1`, additive,
/// multiplied, and the `sqrt` and constant outliers).
#[test]
fn representative_exact_overrides() {
    // (name, params, expected lookback)
    let cases: &[(&str, &[f64], usize)] = &[
        ("sma", &[50.0], 49),                         // p - 1
        ("ema", &[7.0], 6),                           // p - 1
        ("cci", &[33.0], 32),                         // p - 1
        ("rsi", &[21.0], 21),                         // p (one bar spent on the delta)
        ("roc", &[9.0], 9),                           // p
        ("atr", &[10.0], 9),                          // p - 1
        ("macd", &[8.0, 40.0, 5.0], 39),              // max(fast, slow) - 1
        ("awesome", &[5.0, 20.0], 19),                // max(fast, slow) - 1
        ("stochastic", &[10.0, 4.0, 3.0], 12),        // (len-1) + (smooth_k-1)
        ("keltner", &[30.0, 10.0, 2.0], 29),          // ema_length - 1 (mid = the EMA)
        ("dema", &[12.0], 22),                        // 2p - 2
        ("tema", &[12.0], 33),                        // 3p - 3
        ("t3", &[8.0, 0.7], 42),                      // 6p - 6
        ("trix", &[10.0], 28),                        // 3(p-1) + 1
        ("adxr", &[10.0], 29),                        // 3p - 1
        ("hma", &[16.0], 18),                         // p + floor(sqrt p) - 2
        ("tsi", &[30.0, 10.0], 38),                   // long + short - 2
        ("mass", &[20.0, 5.0], 27),                   // p + 2*ema - 3
        ("kvo", &[20.0, 40.0, 13.0], 40),             // max(fast, slow)
        ("volume_osc", &[4.0, 12.0], 11),             // max(short, long) - 1
        ("relative_volatility", &[10.0], 18),         // 2p - 2
        ("connors_rsi", &[3.0, 2.0, 50.0], 50),       // rank_p
        ("chande_kroll_stop", &[6.0, 1.0, 20.0], 19), // max(p, q-1)
        ("ac", &[], 37),                              // constant
        ("alligator", &[], 7),                        // constant
        ("vwap", &[], 0),                             // no warm-up
        ("avgprice", &[], 0),                         // no warm-up
    ];
    let bars = synth_bars(900);
    for &(name, params, expect) in cases {
        let meta = vike_indicators::get(name).unwrap_or_else(|| panic!("{name} not registered"));
        assert!(meta.lookback_exact(), "{name} should be an EXACT override");
        assert_eq!(meta.lookback(params), expect, "{name} lookback at {params:?}");
        // ...and the pinned constant is MEASURED, not just restated arithmetic —
        // otherwise this is the same formula asserting itself.
        let cols = meta.build_with(params).vectorize(&bars);
        assert_eq!(
            first_non_nan(&cols),
            Some(expect),
            "{name}: measured first-non-NaN disagrees with the pinned {expect} at {params:?}"
        );
    }
}

/// REGRESSION (review findings 1 + 2): a multi-param warm-up formula that hardcodes
/// WHICH component dominates instead of taking a max passes at the defaults and
/// lies everywhere else. These are the exact param sets from the review — each puts
/// a NON-default component in charge — measured against `vectorize`.
#[test]
fn multi_param_dominance_regressions() {
    let bars = synth_bars(900);
    // (name, params) — all inside their ParamSpec ranges.
    let cases: &[(&str, &[f64])] = &[
        // roc1/sma1 dominate (200 + 200 - 1 = 399), not roc4/sma4 (1 + 2 - 1 = 2).
        ("kst", &[200.0, 200.0, 15.0, 10.0, 20.0, 10.0, 1.0, 2.0, 9.0]),
        // rsi_p (100) dominates, not rank_p (10).
        ("connors_rsi", &[100.0, 100.0, 10.0]),
        // streak_p dominates.
        ("connors_rsi", &[3.0, 100.0, 10.0]),
        // atr_length dominates keltner's bands, ema_length its mid line.
        ("keltner", &[1.0, 60.0, 2.0]),
        // slow < fast: the max must be taken, not the "slow" name.
        ("kvo", &[55.0, 20.0, 13.0]),
        ("macd", &[40.0, 8.0, 5.0]),
    ];
    for &(name, params) in cases {
        let meta = vike_indicators::get(name).unwrap();
        let ind = meta.build_with(params);
        let cols = ind.vectorize(&bars);
        assert_eq!(
            first_non_nan(&cols),
            Some(ind.lookback()),
            "{name} at {params:?}: lookback() lies outside the default-dominant regime"
        );
        assert_eq!(
            first_all_non_nan(&cols),
            Some(ind.lookback_full()),
            "{name} at {params:?}: lookback_full() lies outside the default-dominant regime"
        );
    }
}

/// REGRESSION (review finding 3): `lookback()` is the ANY-line index and is NOT a
/// seed size on a staggered multi-line indicator; `lookback_full()` is. `adx` is the
/// canonical case — its namesake line (output 0) is still NaN at `lookback()`.
#[test]
fn lookback_full_is_the_all_lines_seed_size() {
    let bars = synth_bars(900);
    for (name, params) in [
        ("adx", &[14.0][..]),
        ("macd", &[12.0, 26.0, 9.0][..]),
        ("stochastic", &[14.0, 3.0, 3.0][..]),
    ] {
        let ind = vike_indicators::get(name).unwrap().build_with(params);
        let cols = ind.vectorize(&bars);
        let (lb, full) = (ind.lookback(), ind.lookback_full());
        assert!(full > lb, "{name} is supposed to be staggered");
        assert!(
            cols.iter().any(|c| c[lb].is_nan()),
            "{name}: some line must still be NaN at lookback() = {lb} (that is why it is not a seed size)"
        );
        for (i, c) in cols.iter().enumerate() {
            assert!(!c[full].is_nan(), "{name}: line {i} is NaN at lookback_full() = {full}");
        }
    }
    // adx specifically: the PRIMARY line is the late one.
    let adx = vike_indicators::get("adx").unwrap().build_with(&[14.0]);
    let cols = adx.vectorize(&bars);
    assert!(cols[0][adx.lookback()].is_nan(), "adx line 0 should still be NaN at lookback()");
    assert_eq!(adx.lookback_full(), 27, "adx full warm-up is 2*period - 1");
}

/// REGRESSION (review finding 4): the path-dependent set is flagged, so a caller
/// cannot read `lookback() == 0` as "no warm-up needed" for these.
#[test]
fn path_dependent_indicators_are_flagged() {
    for name in ["psar", "vwap", "mcginley", "obv", "ad", "nvi", "pvi", "pvt", "net_volume"] {
        let meta = vike_indicators::get(name).unwrap();
        assert_eq!(meta.lookback(&[]), 0, "{name}");
        assert!(meta.warmup_path_dependent(), "{name} must be flagged path-dependent");
    }
    // ...and a bounded-window indicator is NOT flagged.
    for name in ["sma", "rsi", "adx", "bollinger"] {
        assert!(!vike_indicators::get(name).unwrap().warmup_path_dependent(), "{name}");
    }
}

/// REGRESSION (the "believes it is warm" hazard): the ONLY way an indicator can
/// tell a caller "I am warm from bar zero" is `lookback() == 0` with
/// `lookback_exact() == true` and NO path-dependent flag. That claim is only
/// truthful for a genuinely STATELESS indicator — one whose value at bar `i`
/// depends on bar `i` alone.
///
/// This gate proves statelessness BEHAVIORALLY rather than trusting a hand-written
/// allowlist: vectorize the full series, then vectorize a suffix of it. A stateless
/// indicator produces a bit-identical tail; anything carrying state across the cut
/// (a cumulative sum, a recurrence, a session accumulator) diverges — and must then
/// be flagged [`warmup_path_dependent`], or it would let a strategy act on an
/// unwarmed value while believing it is warm.
///
/// Unlike `path_dependent_set_is_pinned` (which restates today's list), this fails
/// on a NEWLY ADDED zero-lookback indicator that forgets the flag.
#[test]
fn zero_lookback_exact_means_genuinely_stateless_or_flagged() {
    const CUT: usize = 400;
    let bars = synth_bars(900);
    let suffix = &bars[CUT..];
    let same = |a: f64, b: f64| a == b || (a.is_nan() && b.is_nan());

    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for meta in registry() {
        let ind = meta.build_with(&[]);
        if !(meta.lookback_exact() && ind.lookback() == 0) {
            continue;
        }
        checked += 1;
        let full = ind.vectorize(&bars);
        let tail = ind.vectorize(suffix);
        let stateless = full.len() == tail.len()
            && full.iter().zip(&tail).all(|(f, t)| {
                t.len() == suffix.len() && (0..t.len()).all(|k| same(f[CUT + k], t[k]))
            });
        if !stateless && !ind.warmup_path_dependent() {
            failures.push(format!(
                "{}: claims EXACT lookback() == 0 (i.e. \"warm at bar zero\") but its output \
                 depends on bars before the cut — it must be flagged warmup_path_dependent()",
                meta.name
            ));
        }
        if stateless && ind.warmup_path_dependent() {
            failures.push(format!(
                "{}: flagged warmup_path_dependent() but is provably stateless — the flag is noise",
                meta.name
            ));
        }
    }
    assert!(checked > 0, "the gate probed nothing — the zero-lookback exact set vanished");
    assert!(failures.is_empty(), "zero-lookback warmth gate:\n{}", failures.join("\n"));
}

/// The by-name FULL entry point.
#[test]
fn by_name_lookback_full_entry_point() {
    assert_eq!(vike_indicators::lookback_full("adx", &[14.0]), Some(27));
    assert_eq!(vike_indicators::lookback_full("sma", &[30.0]), Some(29));
    assert_eq!(vike_indicators::lookback_full("nope", &[]), None);
}

/// A degenerate param can never wrap the `usize` (the `saturating_sub` floor).
#[test]
fn tiny_params_never_underflow() {
    for meta in registry() {
        for probe in [&[1.0f64][..], &[1.0, 1.0, 1.0][..], &[0.0, 0.0, 0.0][..]] {
            let lb = meta.lookback(probe);
            assert!(lb < 100_000, "{} lookback {lb} at {probe:?} looks wrapped", meta.name);
        }
    }
}

/// The by-name entry point agrees with the meta accessor, and is `None` for junk.
#[test]
fn by_name_lookback_entry_point() {
    assert_eq!(vike_indicators::lookback("sma", &[30.0]), Some(29));
    assert_eq!(vike_indicators::lookback("sma", &[]), Some(19)); // defaults
    assert_eq!(vike_indicators::lookback("nope", &[]), None);
    assert_eq!(vike_indicators::pair_lookback("correl", &[10.0]), Some(9));
    assert_eq!(vike_indicators::pair_lookback("nope", &[]), None);
}

/// `coerce` runs before the override, so out-of-range params clamp identically.
#[test]
fn lookback_respects_param_coercion() {
    let meta = vike_indicators::get("sma").unwrap();
    let max = meta.params[0].max;
    assert_eq!(meta.lookback(&[1e9]), max as usize - 1, "clamped to the spec max");
    assert_eq!(meta.lookback(&[f64::NAN]), 19, "NaN falls back to the default");
    assert_eq!(meta.lookback(&[-5.0]), 0, "clamped to the spec min (length 1)");
}

/// Probe helper (ignored): prints the measured warm-up for every indicator at its
/// defaults, so overrides can be derived from real behavior. Run with
/// `cargo test -p vike-indicators --test lookback -- --ignored --nocapture probe`.
#[test]
#[ignore]
fn probe_measured_lookbacks() {
    let bars = synth_bars(900);
    for meta in registry() {
        let ind = meta.build_with(&[]);
        let cols = ind.vectorize(&bars);
        let base = first_non_nan(&cols);
        let ps: Vec<String> =
            meta.params.iter().map(|s| format!("{}={}", s.name, s.default)).collect();
        // Also probe each period-ish param bumped by +1, to expose the coefficient.
        let mut deltas: Vec<String> = Vec::new();
        for (i, spec) in meta.params.iter().enumerate() {
            if spec.step < 1.0 {
                continue;
            }
            let mut raw: Vec<f64> = meta.params.iter().map(|s| s.default).collect();
            raw[i] = (spec.default + 1.0).clamp(spec.min, spec.max);
            let m = first_non_nan(&meta.build_with(&raw).vectorize(&bars));
            deltas.push(format!("{}+1 -> {m:?}", spec.name));
        }
        println!(
            "{:<24} exact={} base={:?} [{}] {}",
            meta.name,
            meta.lookback_exact(),
            base,
            ps.join(","),
            deltas.join(" ")
        );
    }
}
