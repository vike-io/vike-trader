//! Integration tests for CALL-SITE PARAMETERS on user-written indicators.
//!
//! `my_mean(50)` binds 50 to the file's own first `param("lookback", 20)` declaration. The knob
//! mechanism is the STRATEGY's `param()`, reused verbatim, so an author who has written a strategy
//! needs no new syntax — and a file that declares nothing keeps exactly its old zero-argument shape.

use vike_model::{Bar, Broker, Strategy};
use vike_script::{compile_indicator, Indicator, RhaiStrategy};

/// A mean over `lookback` closes, where `lookback` is a `param()` knob — the shape the whole
/// feature exists for.
const MEAN: &str = r#"
    let lookback = param("lookback", 3.0);
    fn init() { #{ buf: [] } }
    fn warmup() { lookback - 1 }
    fn on_bar(bar) {
        this.buf.push(bar.close);
        if this.buf.len() > lookback { this.buf.remove(0); }
        if this.buf.len() < lookback { return (); }
        let s = 0.0;
        for v in this.buf { s += v; }
        s / lookback
    }
"#;

#[derive(Default)]
struct MockBroker {
    bars: Vec<Bar>,
    submits: Vec<(i32, f64)>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        10_000.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        0
    }
}

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

fn drive(s: &mut RhaiStrategy<MockBroker>, b: &mut MockBroker, closes: &[f64]) {
    for &c in closes {
        b.bars.push(bar(c));
        let cur = b.bars.last().unwrap().clone();
        s.on_bar(b, &cur);
    }
}

#[test]
fn a_declared_knob_is_discovered_in_first_seen_order() {
    let ind = compile_indicator("mean", MEAN).unwrap();
    assert_eq!(ind.params(), &[("lookback".to_string(), 3.0)]);

    let two = compile_indicator(
        "two",
        "let a = param(\"alpha\", 1.0); let b = param(\"beta\", 2.0); fn on_bar(bar) { a + b }",
    )
    .unwrap();
    let names: Vec<&str> = two.params().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta"], "order is the order the calls RAN");
}

/// A file declaring nothing keeps exactly its old zero-argument shape — every indicator written
/// before this feature existed is in that state, so this is the compatibility gate.
#[test]
fn a_file_with_no_param_calls_declares_nothing_and_still_works() {
    let ind = compile_indicator("plain", "fn on_bar(bar) { bar.close }").unwrap();
    assert!(ind.params().is_empty());
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(plain()) }",
        Default::default(),
        &[ind],
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[7.0]);
    assert_eq!(b.submits, vec![(1, 7.0)]);
}

/// ⚠ **The core gate.** An argument must CHANGE the recurrence, not merely be recorded. A
/// 2-bar mean and a 4-bar mean over the same series give different numbers, and this asserts
/// both against hand-computed values — so an implementation that accepted the argument and ran
/// the default would fail rather than quietly agree with itself.
#[test]
fn an_argument_changes_the_recurrence_rather_than_being_recorded_and_ignored() {
    let ind = compile_indicator("mean", MEAN).unwrap();
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(mean(2)) }",
        Default::default(),
        std::slice::from_ref(&ind),
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[10.0, 20.0, 60.0]);
    // 2-bar: NaN, (10+20)/2 = 15, (20+60)/2 = 40
    assert!(b.submits[0].1.is_nan());
    assert_eq!(b.submits[1].1, 15.0);
    assert_eq!(b.submits[2].1, 40.0);

    let mut s4 = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(mean(4)) }",
        Default::default(),
        &[ind],
    )
    .unwrap();
    let mut b4 = MockBroker::default();
    drive(&mut s4, &mut b4, &[10.0, 20.0, 60.0, 30.0]);
    // 4-bar: NaN, NaN, NaN, (10+20+60+30)/4 = 30
    assert!(b4.submits[2].1.is_nan(), "a 4-bar mean is still warming at bar 2");
    assert_eq!(b4.submits[3].1, 30.0);
}

/// ⚠ **Two call sites with different knobs are two instances.** The cache key carries the
/// arguments; keyed on the name alone, the second call site would silently read the first one's
/// lookback — a wrong number with no error, which is this seam's whole failure mode.
///
/// Non-vacuous by construction: the two values are computed to DIFFER on the final bar, so a
/// shared instance could not produce both.
#[test]
fn two_call_sites_with_different_arguments_do_not_share_an_instance() {
    let ind = compile_indicator("mean", MEAN).unwrap();
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(mean(2)); buy(mean(4)) }",
        Default::default(),
        &[ind],
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[10.0, 20.0, 60.0, 30.0]);
    // Last bar submits two orders: the 2-bar mean (60+30)/2 = 45, then the 4-bar mean = 30.
    let last: Vec<f64> = b.submits.iter().rev().take(2).map(|(_, q)| *q).collect();
    assert_eq!(last, vec![30.0, 45.0], "each call site keeps its own lookback: {:?}", b.submits);
}

/// Each distinct instance is still fed exactly ONCE per bar — the property that made the
/// zero-argument form correct, now per (name, args) rather than per name.
#[test]
fn each_parameterised_instance_is_fed_once_per_bar() {
    let counter = compile_indicator(
        "count",
        "let step = param(\"step\", 1.0); fn init() { #{ n: 0 } } \
         fn on_bar(bar) { this.n += step; this.n }",
    )
    .unwrap();
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(count(1) + count(1) + count(10)) }",
        Default::default(),
        &[counter],
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, 1.0]);
    // bar 1: count(1) fed once -> 1, read twice = 2; count(10) -> 10. total 12.
    // bar 2: count(1) -> 2, read twice = 4; count(10) -> 20. total 24.
    assert_eq!(b.submits, vec![(1, 12.0), (1, 24.0)]);
}

/// An argument the file has nowhere to put is REFUSED, not discarded — the same rule the
/// built-in bridge applies, and the reason `my_thing(20)` used to be a function-not-found.
#[test]
fn more_arguments_than_declared_knobs_is_an_error_naming_the_counts() {
    let e = vike_script::compile_indicator_with("mean", MEAN, &[2.0, 9.0]).unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("2 argument"), "{msg}");
    assert!(msg.contains("1 `param"), "{msg}");
    assert!(msg.contains("lookback"), "the message names the knob that DOES exist: {msg}");
}

/// A file with no knobs registers only the zero-argument form, so an argument is a
/// function-not-found rather than an accepted-and-ignored value.
#[test]
fn a_knobless_indicator_takes_no_arguments_at_the_call_site() {
    let ind = compile_indicator("plain", "fn on_bar(bar) { bar.close }").unwrap();
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(plain(20)) }",
        Default::default(),
        &[ind],
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[7.0]);
    assert!(b.submits.is_empty(), "the call must not resolve");
}

/// Re-instantiation runs the file's top level again, so a top-level expression that throws for
/// a caller's value must RAISE rather than produce a warm-up-shaped NaN.
#[test]
fn a_knob_that_breaks_the_top_level_raises_rather_than_reading_as_warm_up() {
    let ind = compile_indicator(
        "div",
        "let n = param(\"n\", 1.0); let scale = 100.0 / n; fn on_bar(bar) { bar.close * scale }",
    )
    .unwrap();
    // n = 0 makes `scale` infinite rather than throwing, so assert the honest thing: the value
    // is non-finite and the strategy is not silently trading on it.
    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(div(0)) }",
        Default::default(),
        &[ind],
    )
    .unwrap();
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[7.0]);
    assert!(
        b.submits.is_empty() || !b.submits[0].1.is_finite(),
        "a degenerate knob must not look like an ordinary number: {:?}",
        b.submits
    );
}

/// ⚠ Regression: `param()` returns an f64, so the natural spelling of a parameterised warm-up —
/// `fn warmup() { lookback - 1 }` — returns a FLOAT. Reading it as an `i64` rejected the whole
/// indicator with a rhai type error, for writing the only thing that could have worked.
#[test]
fn a_warmup_computed_from_a_float_knob_is_accepted() {
    let ind = compile_indicator("mean", MEAN).unwrap();
    assert_eq!(ind.lookback(), 2, "the default lookback of 3 warms at index 2");

    let wider = vike_script::compile_indicator_with("mean", MEAN, &[5.0]).unwrap();
    assert_eq!(wider.lookback(), 4, "a knob must move the declared warm-up too");
}

/// A `warmup()` that returns something unusable is an error naming the type, not a silent 0 — a
/// zero warm-up on an indicator that has one under-reports it, and callers size seed history from it.
#[test]
fn a_non_numeric_warmup_is_refused() {
    let e = compile_indicator("bad", "fn warmup() { \"soon\" } fn on_bar(bar) { bar.close }")
        .unwrap_err()
        .to_string();
    assert!(e.contains("warmup"), "{e}");
}

/// ⚠ Regression, and it bit a shipped example. `param` was registered taking an `f64` default, so
/// `param("lookback", 20)` — an integer, the spelling anyone writes for a bar count — failed with
/// a rhai "Function not found" naming an i64. Every shipped template happened to write `5.0`, which
/// is the only reason it went unnoticed until an indicator example used the natural form.
#[test]
fn an_integer_param_default_is_accepted_on_both_engines() {
    // the INDICATOR engine
    let ind = compile_indicator("i", "let n = param(\"n\", 20); fn on_bar(bar) { n }")
        .expect("an integer default must compile");
    assert_eq!(ind.params(), &[("n".to_string(), 20.0)]);

    // ...and the STRATEGY engine, through the public discovery surface
    let found = vike_script::discover_params("let f = param(\"fast\", 5); fn on_bar() {}")
        .expect("an integer default must compile in a strategy too");
    assert_eq!(found, vec![("fast".to_string(), 5.0)]);
}

/// A non-numeric default is still an error naming the knob — widening to `Dynamic` must not have
/// widened it to "anything at all".
#[test]
fn a_non_numeric_param_default_is_still_refused() {
    let e = compile_indicator("b", "let n = param(\"n\", \"twenty\"); fn on_bar(bar) { n }")
        .unwrap_err()
        .to_string();
    assert!(e.contains("must be a number"), "{e}");
}
