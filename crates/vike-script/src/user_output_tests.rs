use super::*;
use crate::ctx::ScriptCtx;
use vike_model::Bar;

/// Three lines off ONE counter, so every value is a direct measurement of how many times the
/// file's `on_bar` ran: bar k reads `[k, 10k, 100k]` when fed once.
const COUNTER_BANDS: &str = r#"
        fn outputs() { ["a", "b", "c"] }
        fn init() { #{ n: 0 } }
        fn on_bar(bar) { this.n += 1; [this.n, this.n * 10, this.n * 100] }
    "#;

/// A band shape: line 0 is NOT the namesake, so the bare name must be refused.
const BANDS: &str = r#"
        let width = param("width", 2.0);
        fn outputs() { ["upper", "mid", "lower"] }
        fn on_bar(bar) { [bar.close + width, bar.close, bar.close - width] }
    "#;

fn bar(c: f64) -> Bar {
    Bar {
        ts: 0,
        open: c,
        high: c,
        low: c,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("X".into()),
    }
}

/// An engine with ONLY the user set on it, plus the fresh ctx to inspect.
fn user_engine(inds: &[crate::RhaiIndicator]) -> (SharedCtx, rhai::Engine) {
    let ctx = ScriptCtx::new();
    let mut engine = rhai::Engine::new();
    register_user_indicators(&mut engine, &ctx, inds);
    (ctx, engine)
}

/// ⚠ **The cache-key gate.** All the lines of one user indicator share ONE streaming instance,
/// fed ONCE per bar — the property `reading_three_lines_in_one_bar_feeds_the_indicator_once`
/// pins for the built-ins, and the reason `register_user_form` keys on the indicator name plus
/// the arguments while the LINE stays captured in the closure.
///
/// Non-vacuous against BOTH regressions, which a value-only test cannot separate:
///   - a line IN the key -> three instances, each fed once. The three VALUES would be
///     identical (`[1, 10, 100]`), so only `user_indicators.len()` catches it — asserted.
///   - a feed per accessor call -> one instance advanced three times, so the reads would be
///     `[1, 20, 300]` — caught by the values, and by `fed_this_bar.len()`.
#[test]
fn every_line_of_one_user_indicator_shares_one_instance_fed_once_per_bar() {
    let ind = crate::compile_indicator("cb", COUNTER_BANDS).unwrap();
    let (ctx, engine) = user_engine(std::slice::from_ref(&ind));
    let ast = engine.compile("[cb_a(), cb_b(), cb_c()]").expect("all three accessors resolve");

    for k in 1..=3usize {
        {
            let mut g = ctx.write().unwrap();
            g.cur_bar = bar(1.0);
            g.fed_this_bar.clear();
        }
        let got: rhai::Array = engine.eval_ast(&ast).expect("evaluates");
        let vals: Vec<f64> = got.iter().map(|d| d.as_float().unwrap()).collect();
        let k = k as f64;
        assert_eq!(
            vals,
            vec![k, k * 10.0, k * 100.0],
            "bar {k}: three reads of one instance fed once — not one instance fed three times"
        );
        let g = ctx.read().unwrap();
        assert_eq!(
            g.user_indicators.len(),
            1,
            "three lines must be ONE cached instance; a line in the key would make it 3"
        );
        assert_eq!(g.fed_this_bar.len(), 1, "...and ONE fed_this_bar slot");
    }
}

/// The namesake rule, applied to a user file: `my_bands()` would hand back `upper` to somebody
/// who read it as the middle — the exact refusal `bollinger` carries — so the bare name does not
/// bind, while every line does.
#[test]
fn a_user_band_indicator_has_no_bare_name_but_all_of_its_lines() {
    let ind = crate::compile_indicator("my_bands", BANDS).unwrap();
    assert!(!user_bare_name_binds("my_bands", ind.outputs()));
    assert_eq!(
        user_line_accessors(&ind),
        vec![
            ("upper".to_string(), "my_bands_upper".to_string()),
            ("mid".to_string(), "my_bands_mid".to_string()),
            ("lower".to_string(), "my_bands_lower".to_string()),
        ]
    );

    let (ctx, engine) = user_engine(&[ind]);
    ctx.write().unwrap().cur_bar = bar(100.0);
    assert!(
        engine.eval::<f64>("my_bands()").is_err(),
        "the bare name must NOT resolve — it would be the upper band"
    );
    assert_eq!(engine.eval::<f64>("my_bands_mid()").unwrap(), 100.0);
    // ...and each accessor is genuinely its OWN line, not three spellings of line 0.
    ctx.write().unwrap().fed_this_bar.clear();
    assert_eq!(engine.eval::<f64>("my_bands_upper()").unwrap(), 102.0);
    assert_eq!(engine.eval::<f64>("my_bands_lower()").unwrap(), 98.0);
    // ...and the knob reaches a LINE accessor, so its ladder is the bare name's, not a stub.
    ctx.write().unwrap().fed_this_bar.clear();
    assert_eq!(engine.eval::<f64>("my_bands_upper(5)").unwrap(), 105.0);
}

/// The other half of the namesake rule: line 0 named after the file, so the bare name binds and
/// IS line 0 — `macd()` on the user side. Without this the rule would read as "multi-output
/// means no bare name", which is the over-broad version `exclusion` already narrowed away from.
#[test]
fn a_namesake_user_indicator_binds_its_bare_name_to_line_zero() {
    let src = r#"fn outputs() { ["trend", "signal"] }
                     fn on_bar(bar) { [bar.close, bar.close * 2.0] }"#;
    let ind = crate::compile_indicator("trend", src).unwrap();
    assert!(user_bare_name_binds("trend", ind.outputs()));
    let (ctx, engine) = user_engine(&[ind]);
    ctx.write().unwrap().cur_bar = bar(7.0);
    assert_eq!(engine.eval::<f64>("trend()").unwrap(), 7.0, "the bare name is line 0");
    ctx.write().unwrap().fed_this_bar.clear();
    assert_eq!(engine.eval::<f64>("trend_signal()").unwrap(), 14.0);
}

/// ⚠ A user file's name is whatever the filesystem allowed — `Shouty.RHAI` loads today — while
/// every registry name is a bare lowercase identifier. Comparing the sanitised LINE against the
/// RAW name would refuse this file's bare name for its capital letter, which has nothing to do
/// with the hazard the rule exists for.
#[test]
fn a_capitalised_user_name_still_matches_its_own_namesake_line() {
    let lines = vec!["Shouty".to_string(), "other".to_string()];
    assert!(user_bare_name_binds("Shouty", &lines));
    // ...and the rule still bites when line 0 is genuinely a different line.
    assert!(!user_bare_name_binds("Shouty", &["upper".to_string(), "Shouty".to_string()]));
}

/// ⚠ A pre-existing hole this feature would have widened: a per-line accessor is a FUNCTION
/// name, not a registry entry, so `registry().iter().any(|m| m.name == n)` never saw it — and
/// `build_engine` registers the user set AFTER the built-ins, where `register_fn` REPLACES. A
/// file called `stochastic_k.rhai` therefore answered every `stochastic_k()` in every strategy.
///
/// Non-vacuous: the witness is DERIVED from `line_accessors` rather than spelled here, and the
/// last assertion pins that a name which merely LOOKS like one is still free — so this cannot
/// pass by refusing everything with an underscore in it.
#[test]
fn a_user_file_may_not_take_a_builtin_line_accessors_name() {
    let (_, taken) = line_accessors("bollinger").into_iter().next().expect("bollinger has lines");
    let why = user_indicator_conflict(&taken)
        .unwrap_or_else(|| panic!("`{taken}` is a built-in accessor and must be refused"));
    assert!(why.contains("per-line accessor"), "{why}");
    assert!(
        vike_indicators::get(&taken).is_none(),
        "test premise: `{taken}` is NOT a registry entry, so the older rule could not see it"
    );
    assert!(
        user_indicator_conflict("bollinger_not_a_line").is_none(),
        "an ordinary underscore name must stay free"
    );
}

/// Every [`user_line_conflict`] rule, asked with a witness that actually REACHES it — and, for
/// the one rule that cannot be reached, the fact that makes it unreachable, asserted rather
/// than assumed. A rule provable only by the data it happens to receive is a rule nobody can
/// trust, which is the same argument `every_exclusion_rule_is_reachable` makes.
///
/// ⚠ **The trap this test was written into once.** [`line_fn_name`] joins the two halves with a
/// literal `_`, so `("pos", "ition")` spells `pos_ition` — NOT the host read `position` — and
/// `("sm", "a")` spells `sm_a`, not `sma`. Both hand-written witnesses collided with nothing at
/// all, and the test failed. Only a name CONTAINING a `_` is reachable, which is why the two
/// witnesses below are derived from the registry instead of spelled here.
#[test]
fn every_user_line_rule_is_reachable_or_provably_not() {
    // 1. The line sanitises to nothing, so it spells no accessor at all.
    assert!(user_line_conflict("x", "%%").is_some());

    // 2. The accessor does not PARSE. A line cannot cause this on its own — its half is
    //    sanitised down to `[a-z0-9_]` — but the indicator half is a file stem, unsanitised,
    //    and a stem is whatever the filesystem allowed. `a)b_c()` is two tokens and a stray
    //    paren, under any reading of the grammar.
    assert!(user_line_conflict("a)b", "c").is_some(), "`a)b_c()` does not parse");

    // 3. The accessor is a host read/verb — UNREACHABLE, and provably so rather than skipped:
    //    the generated name always carries a `_` and no host name does. The rule stays because
    //    that is a property of the separator and of this list, either of which can change, and
    //    this assertion is what would then send its author here to write the real witness.
    assert!(
        HOST_FN_NAMES.iter().all(|n| !n.contains('_')),
        "a host name with a `_` in it makes the host-read rule reachable — replace this \
             assertion with a witness that trips it"
    );

    // 4. The accessor is a built-in indicator's OWN name.
    let (stem, line) = registry_name_a_user_line_could_spell();
    let why = user_line_conflict(stem, line)
        .unwrap_or_else(|| panic!("`{stem}` + `{line}` spells a built-in's name: must refuse"));
    assert!(why.contains(&line_fn_name(stem, line)) && why.contains("own name"), "{why}");

    // 5. The accessor is a built-in's per-line accessor. A DIFFERENT rule from 4 — the name it
    //    would take is a function the bridge registers, not a registry entry — so the message
    //    is asserted, not merely the refusal.
    let (ind, l, taken) = a_builtin_line_accessor();
    let why = user_line_conflict(ind, l)
        .unwrap_or_else(|| panic!("`{taken}` is a built-in accessor: must refuse"));
    assert!(why.contains(&taken) && why.contains("per-line accessor"), "{why}");

    // ...and an ordinary line on an ordinary indicator is free, so none of the above passes by
    // refusing everything with an underscore in it.
    assert!(user_line_conflict("my_bands", "mid").is_none());
}

/// ⚠ `fn outputs()` runs after the top level, so it CAN read a `param()` knob — and the
/// accessors were already registered from the prototype's list. Reading a line this call site's
/// instance does not have would be NaN forever with no fault, which is precisely the
/// warm-up-shaped silence this whole seam refuses. So it raises, naming both lists.
///
/// Non-vacuous: the SAME accessor at the default argument works two lines below, so the test
/// measures the knob-dependence and not a broken registration.
#[test]
fn outputs_that_depend_on_a_knob_raise_rather_than_reading_a_missing_line() {
    let src = r#"
            let n = param("n", 3.0);
            fn outputs() { if n > 2.0 { ["a", "b", "c"] } else { ["a", "b"] } }
            fn on_bar(bar) { if n > 2.0 { [1.0, 2.0, 3.0] } else { [1.0, 2.0] } }
        "#;
    let ind = crate::compile_indicator("wobbly", src).unwrap();
    assert_eq!(ind.outputs().len(), 3, "the prototype declares three at its default");
    let (ctx, engine) = user_engine(&[ind]);
    ctx.write().unwrap().cur_bar = bar(1.0);

    let err = engine
        .eval::<f64>("wobbly_c(1)")
        .expect_err("a knob that changes the line list must not silently drop line 2");
    let msg = err.to_string();
    assert!(msg.contains("must not depend on a `param()` knob"), "{msg}");
    // BOTH lists, in their own brackets. `contains("a, b")` alone would be satisfied by the
    // three-line list on its own — an assertion that cannot tell the two apart is not one.
    assert!(msg.contains("[a, b, c]"), "the registered list: {msg}");
    assert!(msg.contains("[a, b]"), "...and the one this call site declares: {msg}");

    ctx.write().unwrap().fed_this_bar.clear();
    assert_eq!(
        engine.eval::<f64>("wobbly_c()").unwrap(),
        3.0,
        "the default instance declares the list the accessors were built from, and works"
    );
}
