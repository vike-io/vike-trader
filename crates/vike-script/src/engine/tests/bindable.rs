//! The bound-name rules: the advertised set IS the bound set, and every exclusion rule bites.

use crate::ctx::ScriptCtx;
use crate::engine::bindable::{RHAI_INDICATORS, exclusion, unbound_reason};
use crate::engine::host::build_engine;

/// Every registry entry is in exactly one of two states, and both are checked against a real
/// engine: advertised in [`RHAI_INDICATORS`] AND callable, or carrying an [`unbound_reason`]
/// AND genuinely not callable. This is the anti-drift property [`RHAI_INDICATORS`]' doc
/// promises, now over the whole registry rather than over a hand-written const.
#[test]
fn the_advertised_set_is_exactly_the_bound_set() {
    let ctx = ScriptCtx::new();
    let engine = build_engine(&ctx);
    for meta in vike_indicators::registry() {
        let advertised = RHAI_INDICATORS.contains(&meta.name);
        let reason = unbound_reason(meta.name);
        assert_eq!(
            advertised,
            reason.is_none(),
            "{} is advertised={advertised} but unbound_reason={reason:?}",
            meta.name
        );
        // A `Dynamic` result accepts whatever the call returns; only OK-ness is under test.
        let call = engine.eval::<rhai::Dynamic>(&format!("{}()", meta.name));
        assert_eq!(
            call.is_ok(),
            advertised,
            "{}: advertised={advertised} but calling it was {:?}",
            meta.name,
            call.err().map(|e| e.to_string())
        );
    }
}

/// The exclusions, named. Each of these is a real, constructible registry indicator that a
/// script must NOT be able to call, for the reason [`exclusion`] documents — and every one is
/// a silent-wrong-answer hazard, so the test asserts both halves: a reason exists, and the
/// call genuinely fails.
#[test]
fn excluded_indicators_are_real_but_not_callable() {
    let ctx = ScriptCtx::new();
    let engine = build_engine(&ctx);
    // ⚠ `macd` and `adx` USED to be on this list and are deliberately off it now. They were
    // excluded for being multi-output, and that rule was over-broad: line 0 of each is its own
    // namesake line, so `macd()` was always the macd line and was refused for a hazard that
    // does not apply to it. One name per surviving rule, so this stays a rule-coverage check
    // rather than a snapshot of whatever happened to be refused:
    //   bollinger  — multi-output, line 0 is `upper` (NOT the namesake)
    //   stochastic — same, line 0 is `%K`
    //   ichimoku   — batch_only AND multi-output
    //   zigzag     — batch_only
    //   var        — a Rhai reserved word
    //   kst        — 9 parameters, more than the bridge can express
    for name in ["bollinger", "stochastic", "ichimoku", "zigzag", "var", "kst"] {
        assert!(
            vike_indicators::make_with(name, &[]).is_some(),
            "test premise: {name} is a real registry indicator"
        );
        assert!(unbound_reason(name).is_some(), "{name} must carry an exclusion reason");
        assert!(!RHAI_INDICATORS.contains(&name), "{name} must not be advertised as callable");
        assert!(
            engine.eval::<rhai::Dynamic>(&format!("{name}()")).is_err(),
            "{name} must not be callable from a script"
        );
    }
    // ...and the counterpart, without which the narrowing above goes unverified: a namesake
    // multi-output indicator IS callable under its bare name now.
    for name in ["macd", "adx"] {
        assert!(unbound_reason(name).is_none(), "{name}'s line 0 is its namesake — it binds");
        assert!(RHAI_INDICATORS.contains(&name), "{name} must be advertised as callable");
    }
}

/// Pins the premise behind the `var` exclusion, which is about RHAI'S GRAMMAR and nothing
/// about the indicator: `var(..)` cannot parse, so no binding scheme can make it callable.
/// Without this the exclusion looks like a preference somebody could "clean up".
#[test]
fn var_is_a_rhai_reserved_word_and_cannot_be_called_at_all() {
    let raw = rhai::Engine::new_raw();
    assert!(raw.compile("var()").is_err(), "premise: `var(..)` is a Rhai parse error");
    assert!(raw.compile("variance()").is_ok(), "premise: an ordinary name parses fine");
    assert!(vike_indicators::get("var").is_some(), "premise: `var` is a real indicator");
}

/// Each of [`exclusion`]'s four rules, exercised directly — including the ones that exclude
/// nothing in today's registry (a rule proven only by the data it happens to receive is a rule
/// nobody can trust). The `false`/`true` flags spell out the fact under test in each case.
#[test]
fn every_exclusion_rule_is_reachable() {
    // 1. not callable from Rhai (a reserved word).
    assert!(exclusion("var", false, Some("var"), 1, 1, false).is_some());
    // 2. collides with a host function — vacuous against today's registry, live as a rule.
    assert!(exclusion("close", false, Some("close"), 1, 1, true).is_some());
    assert!(exclusion("param", false, Some("param"), 1, 1, true).is_some());
    // 3. batch_only.
    assert!(exclusion("zigzag", true, Some("zigzag"), 1, 1, true).is_some());
    // 4. multi-output whose line 0 is NOT the namesake line -> no BARE name.
    assert!(exclusion("bollinger", false, Some("upper"), 3, 2, true).is_some());
    // ...but multi-output whose line 0 IS the namesake binds its bare name. This is the half
    // of rule 4 that was previously refused for a hazard that does not apply to it: `macd()`
    // has always been the macd line.
    assert!(exclusion("macd", false, Some("macd"), 3, 3, true).is_none());
    // ...and the namesake test reads the SANITISED line name, so `%K` is compared as `k`.
    assert!(exclusion("stochastic", false, Some("%K"), 2, 3, true).is_some());
    // ...and the accepting case, which every bound indicator takes.
    assert!(exclusion("sma", false, Some("sma"), 1, 1, true).is_none());
    // Rule ORDER: an unparseable name reports the parse problem, not a later rule.
    assert_eq!(
        exclusion("var", true, Some("upper"), 3, 3, false),
        exclusion("var", false, Some("var"), 1, 1, false)
    );
}
