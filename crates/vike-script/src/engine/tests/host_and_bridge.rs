//! The host reads, verbs and `param`, and the indicator bridge: bit-parity, caching, arity forms.

use super::load_bar;
use crate::ctx::{Intent, ScriptCtx, SharedCtx};
use crate::engine::bindable::RHAI_INDICATORS;
use crate::engine::builtin::{MAX_INDICATOR_ARITY, register_indicators};
use crate::engine::host::{HOST_FN_NAMES, build_engine, register_reads, register_verbs};
use crate::test_support::{bar_at, ohlcv};
use vike_indicators::IndicatorMeta;
use vike_model::Bar;

/// A deterministic, non-degenerate OHLCV series: every field moves, `high >= low`, and volume
/// varies — so an indicator reading any of them produces a CHANGING value rather than a
/// constant a broken bridge could still reproduce.
fn series(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let t = i as f64;
            let c = 100.0 + (t * 0.7).sin() * 12.0 + t * 0.05;
            ohlcv(
                i as i64 * 60_000,
                c - 0.4,
                c + 1.3,
                c - 1.7,
                c,
                1_000.0 + (t * 0.3).cos() * 400.0,
            )
        })
        .collect()
}

/// Bit-for-bit float comparison with both-NaN treated as equal — the workspace convention (see
/// vike-indicators' crate doc: never widen a tolerance, compare `to_bits`).
fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

fn engine_with_indicators(ctx: &SharedCtx) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    register_indicators(&mut engine, ctx);
    engine
}

/// Drives `src` (one indicator reference) through the bridge over `bars`, returning its value
/// at each bar. Compiles ONCE and re-evaluates the AST, so the per-bar cost is an eval rather
/// than a parse — the whole-registry test below runs this for every bound indicator.
fn bridge_series(src: &str, bars: &[Bar]) -> Vec<f64> {
    let ctx = ScriptCtx::new();
    let engine = engine_with_indicators(&ctx);
    let ast = engine.compile(src).unwrap_or_else(|e| panic!("{src} failed to compile: {e}"));
    bars.iter()
        .map(|bar| {
            load_bar(&ctx, bar.clone());
            engine.eval_ast::<f64>(&ast).unwrap_or_else(|e| panic!("{src} failed: {e}"))
        })
        .collect()
}

/// The oracle: a directly-constructed streaming instance fed each bar exactly once.
fn oracle_series(meta: &IndicatorMeta, raw: &[f64], bars: &[Bar]) -> Vec<f64> {
    let mut ind = meta.build_with(&vike_indicators::coerce(meta.params, raw));
    bars.iter().map(|b| ind.on_bar(b).first().copied().unwrap_or(f64::NAN)).collect()
}

#[test]
fn verbs_record_intents() {
    let ctx = ScriptCtx::new();
    let mut engine = rhai::Engine::new();
    register_verbs(&mut engine, &ctx);
    engine.run("buy(2.0); sell(1.0); limit(1, 3.0, 100.0); market(-1, 4.0);").unwrap();
    let got = &ctx.read().unwrap().intents;
    assert_eq!(
        got,
        &vec![
            Intent::Market { side: 1, qty: 2.0 },
            Intent::Market { side: -1, qty: 1.0 },
            Intent::Limit { side: 1, qty: 3.0, price: 100.0 },
            Intent::Market { side: -1, qty: 4.0 },
        ]
    );
}

#[test]
fn reads_reflect_snapshot() {
    let ctx = ScriptCtx::new();
    {
        let mut g = ctx.write().unwrap();
        g.cur_bar.close = 101.0;
        g.position = 0.0;
    }
    let mut engine = rhai::Engine::new();
    register_reads(&mut engine, &ctx);
    register_verbs(&mut engine, &ctx);
    engine.run("if close() > 100.0 && position() == 0.0 { buy(1.0); }").unwrap();
    assert_eq!(ctx.read().unwrap().intents.len(), 1);
    ctx.write().unwrap().intents.clear();
    ctx.write().unwrap().cur_bar.close = 99.0;
    engine.run("if close() > 100.0 && position() == 0.0 { buy(1.0); }").unwrap();
    assert_eq!(ctx.read().unwrap().intents.len(), 0);
}

#[test]
fn sma_bridge_matches_direct() {
    let closes = [10.0, 11.0, 12.0, 13.0, 14.0];
    // oracle: direct streaming SMA(3)
    let mut direct = vike_indicators::make_with("sma", &[3.0]).unwrap();
    // subject: the bridge
    let ctx = ScriptCtx::new();
    let engine = engine_with_indicators(&ctx);
    for (i, &c) in closes.iter().enumerate() {
        let bar = bar_at(i as i64, c);
        load_bar(&ctx, bar.clone());
        let via_bridge: f64 = engine.eval("sma(3)").unwrap();
        let via_direct = direct.on_bar(&bar)[0];
        assert_eq!(via_bridge.to_bits(), via_direct.to_bits(), "bar {i}");
    }
}

/// Regression test for the fed-once-per-bar caching contract: a SECOND reference to
/// `sma(3)` within the same bar (no `fed_this_bar.clear()` between the two calls) must
/// return the cached `value()[0]` untouched, not feed `on_bar` again. Non-vacuous by
/// construction: the trailing assertion proves that feeding the oracle indicator twice on
/// one bar actually changes its output, so a double-feed regression in `indicator_value`
/// would necessarily desync the bridge's two same-bar reads and fail this test.
#[test]
fn sma_bridge_second_reference_same_bar_is_cached() {
    let closes = [10.0, 11.0, 12.0, 13.0, 14.0];
    let make_bar = |i: usize, c: f64| bar_at(i as i64, c);

    // oracle: direct streaming SMA(3), fed exactly once per bar
    let mut direct = vike_indicators::make_with("sma", &[3.0]).unwrap();
    // subject: the bridge
    let ctx = ScriptCtx::new();
    let engine = engine_with_indicators(&ctx);

    let last = closes.len() - 1;
    let mut oracle_single_fed = f64::NAN;
    for (i, &c) in closes.iter().enumerate() {
        let bar = make_bar(i, c);
        load_bar(&ctx, bar.clone());
        if i == last {
            // Two references to sma(3) on the SAME bar, no fed_this_bar.clear() between
            // them: the first feeds on_bar; the second must be the cached value, untouched.
            let first: f64 = engine.eval("sma(3)").unwrap();
            let second: f64 = engine.eval("sma(3)").unwrap();
            assert_eq!(
                first.to_bits(),
                second.to_bits(),
                "second same-bar reference must return the cached value without re-advancing"
            );
            oracle_single_fed = direct.on_bar(&bar)[0];
            assert_eq!(
                first.to_bits(),
                oracle_single_fed.to_bits(),
                "bridge value (fed once, read twice) must match an oracle fed once on this bar"
            );
        } else {
            let _: f64 = engine.eval("sma(3)").unwrap();
            direct.on_bar(&bar);
        }
    }

    // Non-vacuity: feeding the oracle the final bar a SECOND time (mirroring what a
    // double-feed regression in `indicator_value` would do) must diverge from the
    // single-fed value — otherwise this test could never fail even if the bridge started
    // double-feeding.
    let oracle_double_fed = direct.on_bar(&make_bar(last, closes[last]))[0];
    assert_ne!(
        oracle_single_fed.to_bits(),
        oracle_double_fed.to_bits(),
        "feeding twice must change the result, or this test cannot catch a double-feed regression"
    );
}

/// THE correctness gate for widening the bound set: for EVERY name in [`RHAI_INDICATORS`],
/// the bridge's per-bar value must equal a directly-fed streaming oracle bit-for-bit over a
/// whole series. That is simultaneously the "returns a real value" and the "advances exactly
/// once per bar" claim — a skipped feed, a double feed, a shared cache slot or a mis-coerced
/// parameter each desync the bridge from the oracle at the first affected bar.
///
/// Non-vacuous in two directions: an all-NaN pass is impossible because most of the bound set
/// must produce at least one finite value (asserted as a RELATION against
/// `RHAI_INDICATORS.len()` rather than as a count, so it cannot rot), and the set itself is
/// asserted to have grown well past the three names that used to be hand-listed.
#[test]
fn every_bound_indicator_streams_bit_identically_and_advances_once_per_bar() {
    // 150 bars is a deliberate compromise: nearly every indicator in the catalog streams by
    // history-recompute (`stream_tail`), so this loop is O(bars^2) per indicator across the
    // whole bound set, and the parity claim it checks does not get stronger with a longer
    // series — only the warm-up coverage does, which the `finite` relation below already
    // pins loosely and `previously_unbound_indicators_now_return_real_values` pins exactly.
    let bars = series(150);
    let mut finite = 0usize;
    for &name in RHAI_INDICATORS.iter() {
        let meta = vike_indicators::get(name).expect("advertised name must be in the registry");
        let got = bridge_series(&format!("{name}()"), &bars);
        let want = oracle_series(meta, &[], &bars);
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(same(*g, *w), "{name} diverged at bar {i}: bridge {g} vs oracle {w}");
        }
        if got.iter().any(|v| v.is_finite()) {
            finite += 1;
        }
    }
    assert!(
        finite * 2 > RHAI_INDICATORS.len(),
        "most of the bound set must produce a real value over {} bars, got {finite}/{}",
        bars.len(),
        RHAI_INDICATORS.len()
    );
    assert!(
        RHAI_INDICATORS.len() > 100,
        "the bound set is derived from the registry now, not the old three-name list"
    );
}

/// The same bit-parity claim, for the ARGUMENT forms — which the zero-arity gate above cannot
/// reach.
///
/// ⚠ `{name}()` and `{name}(p)` are SEPARATE rhai registrations, so proving one says nothing
/// about the other: a wrong argument order, a dropped parameter, or a `Dynamic` conversion that
/// silently coerced would all pass the zero-arity gate untouched. Every parameterised indicator
/// is driven here at its OWN declared default, so the call is spelled the way a script would
/// spell it and the oracle is fed the identical slice.
#[test]
fn every_argument_form_streams_bit_identically_too() {
    let bars = series(150);
    let mut checked = 0usize;
    for &name in RHAI_INDICATORS.iter() {
        let meta = vike_indicators::get(name).expect("advertised name must be in the registry");
        let arity = meta.params.len().min(MAX_INDICATOR_ARITY);
        if arity == 0 {
            continue;
        }
        let params: Vec<f64> = meta.params.iter().take(arity).map(|p| p.default).collect();
        let args = params.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>().join(", ");
        let got = bridge_series(&format!("{name}({args})"), &bars);
        let want = oracle_series(meta, &params, &bars);
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(same(*g, *w), "{name}({args}) diverged at bar {i}: bridge {g} vs oracle {w}");
        }
        checked += 1;
    }
    assert!(
        checked > 40,
        "the parameterised set must be non-trivial; only {checked} indicators took an argument"
    );
}

/// A wrong-TYPE argument is an ERROR, not a NaN.
///
/// ⚠ This is the regression the `Dynamic` migration introduced and `bad_arg` exists to undo.
/// NaN is what an indicator returns while WARMING UP, and every shipped template opens
/// `if h.is_nan() { return; }` — so a NaN here makes a typo look like a warm-up that never
/// finishes, and the strategy silently never trades. An error self-disables the strategy after
/// the consecutive-failure cap, which the author can actually see.
#[test]
fn a_wrong_typed_argument_raises_rather_than_returning_a_warmup_shaped_nan() {
    let ctx = ScriptCtx::new();
    let engine = build_engine(&ctx);
    let err = engine
        .eval::<f64>(r#"sma("20")"#)
        .expect_err("a string argument must not resolve to a value");
    let msg = err.to_string();
    assert!(msg.contains("must be a number"), "unhelpful message: {msg}");
    assert!(msg.contains("sma"), "the message must name the indicator: {msg}");
    // ...and the correct spelling still works, so the test cannot pass by breaking `sma`.
    assert!(engine.eval::<f64>("sma(20)").is_ok(), "a numeric argument must still resolve");
}

/// The user-visible claim, spelled out on indicators that were NOT callable before: each
/// returns a finite value, that value CHANGES across bars (so it is really streaming, not a
/// warm-up constant), and it matches a directly-fed oracle. `obv` is here deliberately — it is
/// parameterless, so `obv()` is its only spelling and it exercises the 0-arity form on an
/// indicator that has no parameters at all.
#[test]
fn previously_unbound_indicators_now_return_real_values() {
    let bars = series(120);
    for (src, name, raw) in [
        ("wma(10)", "wma", vec![10.0]),
        ("atr(14)", "atr", vec![14.0]),
        ("cci(20)", "cci", vec![20.0]),
        ("obv()", "obv", vec![]),
        ("tema(12)", "tema", vec![12.0]),
    ] {
        let meta = vike_indicators::get(name).unwrap();
        let got = bridge_series(src, &bars);
        let want = oracle_series(meta, &raw, &bars);
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(same(*g, *w), "{src} diverged at bar {i}: {g} vs {w}");
        }
        let tail: Vec<f64> = got.iter().rev().take(20).copied().collect();
        assert!(tail.iter().all(|v| v.is_finite()), "{src} must be warm and finite at the tail");
        assert!(
            tail.windows(2).any(|w| w[0].to_bits() != w[1].to_bits()),
            "{src} must CHANGE across bars — a constant would pass a broken bridge too"
        );
    }
}

/// The arity forms follow each indicator's own parameter count: `0 ..= params.len()` exists,
/// anything beyond it does not. The negative half is the point — a parameterless indicator
/// must REJECT `doji(5)` rather than accept and discard the argument, which is the shape that
/// invites an author to believe the number meant something.
#[test]
fn arity_forms_follow_the_parameter_count() {
    let ctx = ScriptCtx::new();
    let engine = build_engine(&ctx);
    let ok = |src: &str| engine.eval::<f64>(src).is_ok();
    // sma: exactly one parameter.
    assert!(ok("sma()"), "0-arg form (all defaults) must exist");
    assert!(ok("sma(20)"), "an integer argument must resolve");
    assert!(ok("sma(20.0)"), "a float argument must resolve too");
    assert!(!ok("sma(20, 3)"), "sma has ONE parameter: a 2-arg form must not exist");
    // doji: no parameters at all.
    assert!(ok("doji()"), "a parameterless indicator is callable with no arguments");
    assert!(!ok("doji(5)"), "a parameterless indicator must REJECT an argument, not ignore it");
    // alma: three parameters, and the natural call mixes integers and floats.
    assert!(ok("alma(9)"), "short forms take registry defaults for the rest");
    assert!(ok("alma(9, 0.85)"), "two of three parameters");
    assert!(ok("alma(9, 0.85, 6)"), "MIXED int/float arguments must resolve");
    assert!(!ok("alma(9, 0.85, 6, 1)"), "alma has THREE parameters, not four");
}

/// Arguments really drive the instance (they are not decoration), and the 0-arg form really
/// means "registry defaults" — including a FRACTIONAL default no integer argument can express.
/// `psar`'s `step` defaults to 0.02 within `[0.001, 0.5]`, so before the 0-arg form existed the
/// indicator was unreachable at any sane setting.
#[test]
fn the_zero_arg_form_means_registry_defaults_including_fractional_ones() {
    let bars = series(60);
    // sma's default length is 20: sma() and sma(20) must be the same instance's values...
    let default_form = bridge_series("sma()", &bars);
    let explicit_20 = bridge_series("sma(20)", &bars);
    assert!(
        default_form.iter().zip(&explicit_20).all(|(a, b)| same(*a, *b)),
        "sma() must equal sma(20), its registry default"
    );
    // ...while a different argument must genuinely differ, or the argument is decoration.
    let explicit_3 = bridge_series("sma(3)", &bars);
    assert!(
        default_form.iter().zip(&explicit_3).any(|(a, b)| !same(*a, *b)),
        "sma(3) must differ from sma(): the argument has to reach the instance"
    );
    // psar: the fractional default, reachable only through the 0-arg form.
    let meta = vike_indicators::get("psar").unwrap();
    let got = bridge_series("psar()", &bars);
    let want = oracle_series(meta, &[], &bars);
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert!(same(*g, *w), "psar() diverged at bar {i}: {g} vs {w}");
    }
    let clamped = bridge_series("psar(1)", &bars);
    assert!(
        got.iter().zip(&clamped).any(|(a, b)| !same(*a, *b)),
        "psar(1) clamps step to its 0.5 maximum and must differ from the 0.02 default"
    );
}

/// A non-numeric argument must NOT resolve to the registry default — the same claim the test
/// this replaced made, now checked against the behaviour that superseded it.
///
/// ⚠ Its predecessor asserted the argument reads as NaN. That was true and it was the wrong
/// design: omitting an argument ALREADY means "take the default", so a typo had to be
/// distinguishable from both — and NaN is not, because NaN is what an indicator returns while
/// WARMING UP. `bad_arg` raises instead; this test keeps the original guarantee (a typo never
/// silently becomes a plausible number) while
/// `a_wrong_typed_argument_raises_rather_than_returning_a_warmup_shaped_nan` pins the new
/// louder shape.
#[test]
fn a_non_numeric_argument_never_resolves_to_the_registry_default() {
    let ctx = ScriptCtx::new();
    let engine = engine_with_indicators(&ctx);
    assert!(
        engine.eval::<f64>(r#"sma("20")"#).is_err(),
        "a string argument must raise, not quietly take sma's default period"
    );
}

/// [`HOST_FN_NAMES`] must have no stale entry: every name in it is genuinely registered by
/// `build_engine`, so the collision rule in [`exclusion`] protects real functions rather than
/// reserving names nothing uses. (The other direction — a newly registered host fn that was
/// never added to the list — is not machine-checkable here; rhai exposes no registration
/// query without its `metadata` feature. `HOST_FN_NAMES`' doc carries that caveat.)
#[test]
fn host_fn_names_are_all_callable() {
    let ctx = ScriptCtx::new();
    let engine = build_engine(&ctx);
    // Arguments per host fn, chosen to match its registered signature.
    let call = |name: &str| -> String {
        match name {
            "buy" | "sell" => format!("{name}(1.0)"),
            "market" => format!("{name}(1, 1.0)"),
            "limit" => format!("{name}(1, 1.0, 100.0)"),
            "param" => format!(r#"{name}("k", 1.0)"#),
            _ => format!("{name}()"),
        }
    };
    for &name in HOST_FN_NAMES {
        let src = call(name);
        assert!(
            engine.eval::<rhai::Dynamic>(&src).is_ok(),
            "{name} is listed in HOST_FN_NAMES but `{src}` did not resolve"
        );
    }
}

/// Guards the one hand-written number in the binding: [`register_indicators`] writes out forms
/// for arities 1..=[`MAX_INDICATOR_ARITY`], and a registry indicator with more parameters than
/// that would bind SILENTLY with its extra parameters pinned to their defaults. Fail here
/// instead, naming the arm to add. This is not hypothetical: `kst` already carries far more
/// parameters than the cap — it is simply excluded today for an unrelated reason (multi-output).
#[test]
fn bound_indicator_arity_never_exceeds_the_registered_forms() {
    for &name in RHAI_INDICATORS.iter() {
        let meta = vike_indicators::get(name).unwrap();
        assert!(
            meta.params.len() <= MAX_INDICATOR_ARITY,
            "{name} takes {} parameters but register_indicators only writes forms up to \
                 MAX_INDICATOR_ARITY={MAX_INDICATOR_ARITY} — add the next arm (and raise the \
                 constant) rather than letting its tail parameters silently take defaults",
            meta.params.len()
        );
    }
}

/// Two references to the SAME indicator with DIFFERENT parameters are independent instances,
/// each fed once per bar — the cache key is `(name, coerced params)`, not the name. Without
/// this a two-moving-average script (the single most common shape there is) would read one
/// average twice.
#[test]
fn different_parameters_are_independent_instances() {
    let bars = series(80);
    let ctx = ScriptCtx::new();
    let engine = engine_with_indicators(&ctx);
    let fast_ast = engine.compile("sma(5)").unwrap();
    let slow_ast = engine.compile("sma(30)").unwrap();
    let mut fast_oracle = vike_indicators::make_with("sma", &[5.0]).unwrap();
    let mut slow_oracle = vike_indicators::make_with("sma", &[30.0]).unwrap();
    for (i, bar) in bars.iter().enumerate() {
        load_bar(&ctx, bar.clone());
        let fast: f64 = engine.eval_ast(&fast_ast).unwrap();
        let slow: f64 = engine.eval_ast(&slow_ast).unwrap();
        assert!(same(fast, fast_oracle.on_bar(bar)[0]), "sma(5) diverged at bar {i}");
        assert!(same(slow, slow_oracle.on_bar(bar)[0]), "sma(30) diverged at bar {i}");
    }
}

#[test]
fn param_uses_override_then_default_and_records_seen() {
    let ctx = ScriptCtx::new();
    ctx.write().unwrap().overrides.insert("fast".to_string(), 3.0);
    let engine = build_engine(&ctx);

    // present in overrides -> the override
    let v: f64 = engine.eval(r#"param("fast", 5.0)"#).unwrap();
    assert_eq!(v, 3.0);
    // absent -> the default, and recorded (first-seen) in params_seen
    let d: f64 = engine.eval(r#"param("slow", 20.0)"#).unwrap();
    assert_eq!(d, 20.0);
    let g = ctx.read().unwrap();
    assert_eq!(g.params_seen.get("fast"), Some(&5.0)); // default recorded even when overridden
    assert_eq!(g.params_seen.get("slow"), Some(&20.0));
}
