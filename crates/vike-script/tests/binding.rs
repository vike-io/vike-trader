//! Integration tests for the public `RhaiStrategy<B>` binding (Task 6, including its review-fix
//! round): compile, the snapshot->call_fn->drain flow via a mock `Broker`, compile-error
//! surfacing, runtime fail-safety (a script error discards any partially-recorded intent and,
//! after 10 CONSECUTIVE errors, self-disables the strategy for good), the order-safety fix that a
//! script's top-level statements (e.g. a bare `buy(1.0);` outside any `fn`) run AT MOST ONCE ever
//! rather than re-firing on every hook call, and the const-visibility check that a top-level
//! `const` stays readable from inside a script `fn` despite the top level itself never running
//! again after `compile`.

use vike_model::{Bar, Broker, Strategy};
use vike_script::RhaiStrategy;

#[derive(Default)]
struct MockBroker {
    pos: f64,
    bars: Vec<Bar>,
    submits: Vec<(i32, f64)>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        self.pos
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

#[test]
fn on_bar_buys_when_close_above_100_once() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(
        "fn on_bar() { if close() > 100.0 && position() == 0.0 { buy(1.0); } }",
    )
    .unwrap();
    let mut b = MockBroker::default();
    for &c in &[99.0, 101.0] {
        b.bars.push(bar(c));
        strat.on_bar(&mut b, &bar(c));
    }
    assert_eq!(b.submits, vec![(1, 1.0)]); // only the 101.0 bar fires
}

#[test]
fn compile_error_is_returned() {
    assert!(RhaiStrategy::<MockBroker>::compile("fn on_bar( { buy(").is_err());
}

/// Fail-safe test (Task 6 Fix 3): the error fires AFTER `buy(1.0)` has already recorded an
/// intent, so this exercises the actual "discard a partially-recorded intent on error" contract
/// (`run_hook`'s error arm clears `ctx.intents` before returning) rather than merely "no panic
/// when the error happens before any verb call ever runs" — a strictly weaker property the
/// original ordering (error-then-buy) left unexercised.
#[test]
fn runtime_error_fails_safe_no_panic() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(
        "fn on_bar() { buy(1.0); let x = [1]; x[99]; }", // buy recorded, THEN index out of bounds
    )
    .unwrap();
    let mut b = MockBroker::default();
    b.bars.push(bar(1.0));
    strat.on_bar(&mut b, &bar(1.0)); // must not panic
    assert!(
        b.submits.is_empty(),
        "the buy(1.0) recorded before the error must be discarded, not submitted"
    );
}

/// CRITICAL de-risking check (Task 6 brief), now covering BOTH branches: a later parity task
/// mounts a script with a top-level `const` referenced inside `fn on_bar()`. `RhaiStrategy` now
/// runs the AST's top level exactly once (in `compile`) and clones the resulting `Scope` into
/// every hook call (see `strategy.rs`'s module doc), so this proves the top-level `const` is
/// still visible from inside the function body under that mechanism, and that it actually drives
/// the branch both ways — not merely that the script compiles or that one hard-coded arm happens
/// to fire.
///
/// This did NOT work with the brief's original `engine.call_fn(&mut Scope::new(), ...)` call:
/// empirically (against `rhai` 1.25.1), `call_fn`'s default options evaluate the AST's top-level
/// statements (here, `const K = 7.0;`) into the passed scope, but then immediately REWIND the
/// scope back to its pre-eval length before the target function is ever invoked — popping `K`
/// back off, so a bare reference to it inside `on_bar` failed with `ErrorVariableNotFound`. See
/// `strategy.rs`'s module doc for the full explanation of the current (fixed) mechanism.
#[test]
fn top_level_const_is_visible_inside_on_bar_hook() {
    // close = 5.0 < K = 7.0 -> condition true -> buy must fire.
    let mut buys = RhaiStrategy::<MockBroker>::compile(
        "const K = 7.0; fn on_bar() { if K > close() { buy(1.0); } }",
    )
    .unwrap();
    let mut b1 = MockBroker::default();
    b1.bars.push(bar(5.0));
    buys.on_bar(&mut b1, &bar(5.0));
    assert_eq!(
        b1.submits,
        vec![(1, 1.0)],
        "top-level const K must resolve inside on_bar() and let close()=5.0 < K=7.0 pass"
    );

    // close = 9.0 > K = 7.0 -> condition false -> buy must NOT fire. Proves K actually gates the
    // branch (rather than the test only ever exercising the true arm).
    let mut no_buy = RhaiStrategy::<MockBroker>::compile(
        "const K = 7.0; fn on_bar() { if K > close() { buy(1.0); } }",
    )
    .unwrap();
    let mut b2 = MockBroker::default();
    b2.bars.push(bar(9.0));
    no_buy.on_bar(&mut b2, &bar(9.0));
    assert!(
        b2.submits.is_empty(),
        "top-level const K must block the buy when close()=9.0 exceeds it"
    );
}

/// Fix 1 acceptance test (Task 6 review, order-safety): a top-level order-verb call (anything
/// outside a `fn`) must run AT MOST ONCE ever, never once per hook call. `on_bar` here is an
/// empty body, so any submission recorded after driving it can only have come from the top-level
/// `buy(1.0);` re-firing. Non-vacuous by construction: this is exactly the bug the old default
/// `eval_ast: true` produced (see `strategy.rs`'s module doc) — confirmed by temporarily reverting
/// `run_hook` to that default while developing this fix, which reproduces 3 submissions here
/// (documented in the Task 6 report; not left in the tree since it would defeat the fix).
#[test]
fn top_level_statement_does_not_refire_per_hook_call() {
    let mut strat = RhaiStrategy::<MockBroker>::compile("buy(1.0); fn on_bar() {}").unwrap();
    let mut b = MockBroker::default();
    for _ in 0..3 {
        b.bars.push(bar(1.0));
        strat.on_bar(&mut b, &bar(1.0));
    }
    assert_eq!(
        b.submits.len(),
        0,
        "a top-level buy(1.0) must run at most once (at compile time, discarded there), never per on_bar call"
    );
}

/// Fix 3 acceptance test (Task 6 review, self-disable coverage): the SAME script errors on the
/// first 10 calls (an out-of-bounds array index gated on `index() < 10`) and would `buy(1.0)` on
/// an 11th call if its body ever ran again — proving that after 10 CONSECUTIVE runtime errors the
/// strategy self-disables and stops running the body entirely, rather than merely "keeps failing
/// safe forever". Non-vacuous by construction: if self-disable were broken or absent, the 11th
/// call's `else` branch would execute (index() == 10 by then) and submit an order, failing the
/// final assertion.
#[test]
fn self_disables_after_ten_consecutive_errors_then_stays_inert() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(
        "fn on_bar() { if index() < 10 { let x = [1]; x[99]; } else { buy(1.0); } }",
    )
    .unwrap();
    let mut b = MockBroker::default();
    for i in 0..10 {
        b.bars.push(bar(1.0));
        strat.on_bar(&mut b, &bar(1.0));
        assert!(b.submits.is_empty(), "erroring call {i} must submit nothing");
    }
    // 11th call: index() == 10 now, so the script's `else { buy(1.0); }` WOULD fire if the
    // strategy were still running the body -- but 10 consecutive errors must have disabled it.
    b.bars.push(bar(1.0));
    strat.on_bar(&mut b, &bar(1.0));
    assert!(
        b.submits.is_empty(),
        "strategy must self-disable after 10 consecutive errors and stay inert on the 11th call"
    );
}

/// Task 7: op-cap fail-safe test. A script with an infinite loop in `on_bar` must be caught
/// by the engine's `set_max_operations` cap, return cleanly (not hang), and submit zero orders.
#[test]
fn infinite_loop_trips_op_cap_no_hang() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(
        "fn on_bar() { let i = 0; while true { i += 1; } buy(1.0); }",
    )
    .unwrap();
    let mut b = MockBroker::default();
    b.bars.push(bar(1.0));
    strat.on_bar(&mut b, &bar(1.0)); // op cap trips -> error -> fail-safe; must return, not hang
    assert!(b.submits.is_empty());
}

/// Task 7: `Send` guard test. Proves `RhaiStrategy<MockBroker>` is `Send`, required for the
/// live mount to run on separate threads.
#[test]
fn rhai_strategy_is_send() {
    fn assert_send<T: Send>() {}
    // RhaiStrategy<B>: Send is INDEPENDENT of B — carried via PhantomData<fn(&mut B)> (see
    // strategy.rs's struct doc) — not because MockBroker itself happens to be Send.
    assert_send::<RhaiStrategy<MockBroker>>();
}

/// Task 2 (SP5a): `discover_params` is broker-agnostic (a free fn, not a `RhaiStrategy` method)
/// and lists each `param(name, default)` call's name+default in DECLARATION order (first-seen),
/// for the sweep UI to enumerate a script's knobs before mounting it.
#[test]
fn discover_lists_params_in_declaration_order() {
    let src = r#"
        let fast = param("fast", 5.0);
        let slow = param("slow", 20.0);
        fn on_bar() {}
    "#;
    let ps = vike_script::discover_params(src).unwrap();
    assert_eq!(ps, vec![("fast".to_string(), 5.0), ("slow".to_string(), 20.0)]);
}

/// Task 2 (SP5a): `compile_with_params` injects `overrides` for `param(name, default)` calls
/// BEFORE the one-time top-level run, so a swept value is baked into the persisted `scope`/state
/// for the strategy's whole lifetime — not merely a per-hook-call override. `compile(src)` (no
/// overrides) must still see the script's own default (5.0 -> gate not > 10.0 -> no buy), while
/// `compile_with_params(src, {"gate": 20.0})` must see the override (20.0 -> buy fires).
#[test]
fn compile_with_params_bakes_the_override() {
    // on_bar buys only when `gate` > 10; default 5 -> no buy, override 20 -> buys.
    let src = r#"
        let gate = param("gate", 5.0);
        fn on_bar() { if gate > 10.0 { buy(1.0); } }
    "#;
    // default: no intent
    let mut d = RhaiStrategy::<MockBroker>::compile(src).unwrap();
    let mut b = MockBroker::default();
    d.on_bar(&mut b, &bar(1.0));
    assert!(b.submits.is_empty(), "default gate=5.0 must not pass the > 10.0 gate");

    // override gate=20: buys
    let mut o = indexmap::IndexMap::new();
    o.insert("gate".to_string(), 20.0);
    let mut s = RhaiStrategy::<MockBroker>::compile_with_params(src, o).unwrap();
    let mut b2 = MockBroker::default();
    s.on_bar(&mut b2, &bar(1.0));
    assert!(!b2.submits.is_empty(), "override gate=20.0 must pass the > 10.0 gate and buy");
}

/// Task 2 (SP5a): a script with no `param()` calls at all must still compile fine and
/// `discover_params` must report an empty list (not error) — the sweep UI shows "no knobs"
/// rather than failing to load the script.
#[test]
fn no_param_script_still_compiles_and_discovers_nothing() {
    let src = "fn on_bar() { buy(1.0); }";
    assert!(vike_script::discover_params(src).unwrap().is_empty());
    assert!(RhaiStrategy::<MockBroker>::compile(src).is_ok());
}
