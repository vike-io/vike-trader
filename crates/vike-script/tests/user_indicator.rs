//! Integration tests for USER-WRITTEN indicators bound into a real strategy
//! (`RhaiStrategy::compile_with_indicators`).
//!
//! `indicator.rs`'s own unit tests prove a `RhaiIndicator` computes correctly in ISOLATION. These
//! prove the part isolation cannot reach: that a strategy calling `my_thing()` gets it fed exactly
//! once per bar off the real bar stream, that a fault becomes a stopped strategy rather than a
//! silent one, and that a name which would shadow a built-in never wins.

use vike_model::{Bar, Broker, Strategy};
use vike_script::{RhaiIndicator, RhaiStrategy, compile_indicator};

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
        self.bars.last().map(|b| b.close).unwrap_or(0.0)
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

/// Counts its own invocations and returns the count, so the VALUE a strategy reads is a direct
/// measurement of how many times the indicator was fed.
fn counter() -> RhaiIndicator {
    compile_indicator(
        "feed_count",
        "fn init() { #{ n: 0 } } fn on_bar(bar) { this.n += 1; this.n }",
    )
    .unwrap()
}

fn drive(strat: &mut RhaiStrategy<MockBroker>, broker: &mut MockBroker, closes: &[f64]) {
    for &c in closes {
        broker.bars.push(bar(c));
        let b = broker.bars.last().unwrap().clone();
        strat.on_bar(broker, &b);
    }
}

#[test]
fn a_user_indicator_is_callable_from_a_strategy_and_sees_the_real_bars() {
    let src = "fn on_bar() { if passthrough() > 10.0 { buy(1.0) } }";
    let ind = compile_indicator("passthrough", "fn on_bar(bar) { bar.close }").unwrap();
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[ind]).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[5.0, 20.0, 7.0, 30.0]);
    assert_eq!(b.submits, vec![(1, 1.0), (1, 1.0)], "fired on the two bars above 10");
}

/// The core streaming guarantee. Three references in one bar must feed the indicator ONCE — the
/// property `engine.rs`'s `fed_this_bar` exists for, gated here for the user-indicator path.
#[test]
fn a_user_indicator_is_fed_once_per_bar_however_many_times_it_is_referenced() {
    // `n` is the feed count. If a reference fed it, this bar's three reads would return 1,2,3 and
    // the sum would be 6 on the first bar; fed once, all three read 1 and the sum is 3.
    let src = "fn on_bar() { let t = feed_count() + feed_count() + feed_count(); buy(t) }";
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[counter()])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, 1.0, 1.0]);
    assert_eq!(
        b.submits,
        vec![(1, 3.0), (1, 6.0), (1, 9.0)],
        "bar k must read k from all three references (fed once), not 3k-2/3k-1/3k"
    );
}

/// A user indicator and a built-in must coexist: same cache, same bar, neither disturbing the
/// other. The `user:` key prefix is what keeps their namespaces apart.
#[test]
fn a_user_indicator_and_a_builtin_stream_side_by_side() {
    let src = "fn on_bar() { buy(feed_count() + sma(2)) }";
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[counter()])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[10.0, 20.0]);
    // bar 0: count 1, sma(2) still warming -> NaN
    assert_eq!(b.submits[0].0, 1);
    assert!(b.submits[0].1.is_nan(), "the built-in is warming up: {:?}", b.submits[0]);
    // bar 1: count 2, sma(2) over [10, 20] = 15
    assert_eq!(b.submits[1], (1, 17.0));
}

/// ⚠ The NaN trap, end to end. `on_bar` can only return NaN for a fault, and NaN is also warm-up —
/// so the bridge RAISES, which routes into `RhaiStrategy`'s existing fail-safe. Without this a
/// throwing indicator would look like an indicator that never warms up, forever.
///
/// Non-vacuous by construction, in the shape `binding.rs`'s own self-disable test uses: from bar 11
/// the script would `buy(1.0)` unconditionally, touching the indicator not at all. If the raise
/// never happened — or happened but never tripped the cap — that order would appear.
#[test]
fn a_faulting_user_indicator_stops_the_strategy_instead_of_looking_like_warm_up() {
    let boom = compile_indicator("boom", "fn on_bar(bar) { throw \"kaboom\" }").unwrap();
    let src = "fn on_bar() { if index() < 10 { buy(boom()) } else { buy(1.0) } }";
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[boom]).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0; 12]);
    assert!(
        b.submits.is_empty(),
        "10 raised faults must self-disable the strategy, so the later unconditional \
         buy(1.0) never runs either: {:?}",
        b.submits
    );
}

/// A fault does NOT poison the rest of the run: the flag means "the last bar faulted".
#[test]
fn a_transient_fault_recovers_rather_than_disabling_the_strategy() {
    let flaky = compile_indicator(
        "flaky",
        "fn on_bar(bar) { if bar.close < 0.0 { throw \"neg\" } bar.close }",
    )
    .unwrap();
    let src = "fn on_bar() { buy(flaky()) }";
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[flaky]).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, -1.0, 2.0]);
    assert_eq!(b.submits, vec![(1, 1.0), (1, 2.0)], "the bad bar is skipped, the rest trade");
}

/// The shadowing refusal, from the strategy side: a file named `sma` must NOT take over `sma()`.
/// Binding it would make every script in the workspace mean something different on this machine.
#[test]
fn a_user_indicator_never_shadows_a_builtin_of_the_same_name() {
    // This "sma" returns 999 for any input. If it were bound, the assert below would read 999.
    let impostor = compile_indicator("sma", "fn on_bar(bar) { 999.0 }").unwrap();
    let src = "fn on_bar() { buy(sma(2)) }";
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[impostor])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[10.0, 20.0]);
    assert_eq!(b.submits[1], (1, 15.0), "the REAL sma answered, not the user's file");
}

/// ...and a host verb is equally protected — a user file called `close` would break `close()` for
/// every script.
#[test]
fn a_user_indicator_never_shadows_a_host_read() {
    let impostor = compile_indicator("close", "fn on_bar(bar) { 999.0 }").unwrap();
    let src = "fn on_bar() { buy(close()) }";
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[impostor])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[42.0]);
    assert_eq!(b.submits, vec![(1, 42.0)], "close() is still the bar's close");
}

/// Zero-argument only, and deliberately: there is nowhere to put a parameter (see
/// `register_user_indicators`). A function-not-found is the honest answer — an accepted-and-ignored
/// argument would be a knob that looks present and is not.
#[test]
fn a_user_indicator_takes_no_call_site_arguments() {
    let src = "fn on_bar() { buy(feed_count(20)) }";
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[counter()])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0]);
    assert!(b.submits.is_empty(), "the call must not resolve");
}

/// Two strategies built from ONE loaded prototype must not share streaming state — otherwise a
/// sweep running N variants in parallel would have them all reading one another's indicator.
#[test]
fn two_strategies_from_one_prototype_do_not_share_state() {
    let proto = counter();
    let src = "fn on_bar() { buy(feed_count()) }";
    let mut a = RhaiStrategy::compile_with_indicators(
        src,
        Default::default(),
        std::slice::from_ref(&proto),
    )
    .expect("compiles");
    let mut c =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[proto]).expect("compiles");
    let (mut ba, mut bc) = (MockBroker::default(), MockBroker::default());
    drive(&mut a, &mut ba, &[1.0, 1.0, 1.0]);
    drive(&mut c, &mut bc, &[1.0]);
    assert_eq!(ba.submits.last().unwrap().1, 3.0);
    assert_eq!(bc.submits, vec![(1, 1.0)], "the second strategy starts from 1, not 4");
}

/// ⚠ `compile` runs the script's top level once, with NO bar loaded. A top-level reference to a
/// user indicator must not leave a phantom zero-bar sample behind — for a user indicator that
/// would corrupt the AUTHOR'S own recurrence (a running count would start at 1), not merely shift
/// a warm-up.
#[test]
fn a_top_level_reference_leaves_no_phantom_sample_from_the_zero_bar() {
    let src = "feed_count(); fn on_bar() { buy(feed_count()) }";
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[counter()])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, 1.0]);
    assert_eq!(
        b.submits,
        vec![(1, 1.0), (1, 2.0)],
        "the first REAL bar must be the first bar the indicator ever sees"
    );
}

/// A user-written BAND indicator, end to end: three lines declared by `fn outputs()`, each reachable
/// from a strategy through its own accessor, all off ONE streaming instance.
///
/// Non-vacuous by construction: `n` counts the file's own `on_bar` invocations and every line is
/// derived from it, so a strategy reading all three lines on bar k must submit `k + 10k + 100k`. A
/// bridge that fed the indicator once per accessor would submit `1 + 20 + 300` on the first bar, and
/// one that truncated to line 0 would submit `3k` — both are values this asserts against.
#[test]
fn a_user_band_indicator_reaches_a_strategy_line_by_line_off_one_instance() {
    let bands = compile_indicator(
        "cb",
        r#"fn outputs() { ["a", "b", "c"] }
           fn init() { #{ n: 0 } }
           fn on_bar(bar) { this.n += 1; [this.n, this.n * 10, this.n * 100] }"#,
    )
    .unwrap();
    let src = "fn on_bar() { buy(cb_a() + cb_b() + cb_c()) }";
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[bands]).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, 1.0]);
    assert_eq!(b.submits, vec![(1, 111.0), (1, 222.0)], "bar k reads k, 10k, 100k off one feed");
}

/// ...and its BARE name is refused from a strategy, for the reason `bollinger` is: line 0 is the
/// upper band, and a caller reading `my_bands()` as the middle would get a wrong number with no
/// error. A function-not-found is the honest answer.
///
/// Non-vacuous: the accessor for the SAME indicator resolves in the second half, so this cannot pass
/// by failing to bind the indicator at all.
#[test]
fn a_user_band_indicators_bare_name_does_not_resolve_from_a_strategy() {
    let src = "fn on_bar() { buy(my_bands()) }";
    let bands = || {
        compile_indicator(
            "my_bands",
            r#"fn outputs() { ["upper", "mid", "lower"] }
               fn on_bar(bar) { [bar.close + 2.0, bar.close, bar.close - 2.0] }"#,
        )
        .unwrap()
    };
    let mut s = RhaiStrategy::compile_with_indicators(src, Default::default(), &[bands()])
        .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[50.0]);
    assert!(b.submits.is_empty(), "the bare name must not resolve: {:?}", b.submits);

    let mut s = RhaiStrategy::compile_with_indicators(
        "fn on_bar() { buy(my_bands_mid()) }",
        Default::default(),
        &[bands()],
    )
    .expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[50.0]);
    assert_eq!(b.submits, vec![(1, 50.0)], "...while the middle band is one call away");
}

/// ⚠ `compile` runs the script's top level once with NO bar loaded, and clears whatever that
/// recorded. That clearing must reach a multi-output user indicator too — a phantom zero-bar sample
/// would corrupt the AUTHOR's own recurrence, and a per-line binding is a new way to reach it.
#[test]
fn a_top_level_line_accessor_leaves_no_phantom_sample_either() {
    let ind = compile_indicator(
        "cb",
        r#"fn outputs() { ["a", "b"] }
           fn init() { #{ n: 0 } }
           fn on_bar(bar) { this.n += 1; [this.n, this.n * 10] }"#,
    )
    .unwrap();
    let src = "cb_b(); fn on_bar() { buy(cb_a()) }";
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[ind]).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[1.0, 1.0]);
    assert_eq!(
        b.submits,
        vec![(1, 1.0), (1, 2.0)],
        "the first REAL bar must be the first bar the indicator ever sees"
    );
}

/// Binding none is the default path every existing caller takes — it must stay untouched.
#[test]
fn compile_with_no_user_indicators_is_the_plain_binding() {
    let src = "fn on_bar() { buy(sma(2)) }";
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), &[]).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, &[10.0, 20.0]);
    assert_eq!(b.submits[1], (1, 15.0));
}
