//! Tests for the named-run resolver and roster — moved out of `named_run.rs` verbatim.

use super::*;
use vike_model::{Bar, Broker};

/// A ~35-line `HftBroker` double. Copied from `tests/pipeline.rs`'s for the reason that file's
/// own comment gives: `vike_model::MockBroker` implements only `Broker`, not the tagged-verb
/// extension the portable registry's bound requires, and this crate declares no dev-dependency
/// that could carry a shared one (the manifest's dependency set IS the user-strategy API
/// surface, and widening it for a test double would widen that surface).
#[derive(Default)]
struct TestBroker;

impl Broker for TestBroker {
    fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {}
    fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
    fn position(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &[]
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        0
    }
}

impl HftBroker for TestBroker {
    fn position(&self) -> f64 {
        0.0
    }
    fn submit_limit_tagged(&mut self, _tag: &str, _side: i32, _qty: f64, _price: f64) {}
    fn modify_tagged(&mut self, _tag: &str, _new_qty: Option<f64>, _new_price: Option<f64>) {}
    fn cancel_tagged(&mut self, _tag: &str) {}
}

/// **THE FENCE, stated as a test.** The script path is not resolvable here, and — the half that
/// matters — it is not resolvable here WITH a `src` param either, which is exactly the shape
/// `vike_backtest::harness::registry`'s `"rhai"` arm accepts one line away from this closure.
///
/// ⚠ This test is evidence, not the fence. The fence is the DEPENDENCY CLOSURE
/// (`crates/vike-ops/tests/architecture/named_run_closure_gate.rs`): a test proves what today's code does, a
/// closure proves what tomorrow's can.
#[test]
fn a_param_cannot_reach_a_compiler_through_the_named_run_resolver() {
    let with_src: Value = toml::from_str("src = \"fn on_bar() { buy(1.0); }\"\nqty = 2.0\n")
        .expect("fixture params parse");
    // The SAME (name, params) pair `rhai_resolves_through_the_registry_with_inline_src` proves
    // DOES compile in `vike-backtest`. Here it is unknown, and the params are unread.
    match resolve::<TestBroker>("rhai", &with_src) {
        Err(NamedRunError::Unknown(n)) => assert_eq!(n, "rhai"),
        Err(other) => panic!("the script path must not resolve here, got {other:?}"),
        Ok(_) => panic!("the script path RESOLVED in a closure that holds no compiler"),
    }
    // …and a `src` riding along with a name that DOES resolve changes nothing about the run: it
    // is unread, the way any unrecognised key is unread.
    let mut params = with_src.clone();
    if let Some(t) = params.as_table_mut() {
        t.insert("size".to_string(), Value::Float(1.0));
    }
    assert!(
        resolve::<TestBroker>("buy_hold", &params).is_ok(),
        "a `src` key must be UNREAD rather than fatal — it is not a checked field here"
    );
}

/// Every name the roster advertises actually resolves WITH EMPTY PARAMS — the property
/// `vike_backtest::harness::registry`'s `registry_lists_every_match_arm` holds for its own
/// roster, and the one that makes a named-only verb possible at all.
#[test]
fn every_rostered_name_resolves_with_default_params() {
    let empty = Value::Table(Default::default());
    for name in roster() {
        assert!(
            resolve::<TestBroker>(name, &empty).is_ok(),
            "{name} is on the named-run roster but does not resolve"
        );
    }
}

/// …and the other direction: the roster carries no SCRIPT_ONLY name, and no simulator-only one
/// either. The second half is the DECLARED COST of 0064's decision 2, pinned so that admitting
/// one of those arms reddens a test that names the record rather than passing silently.
#[test]
fn the_roster_excludes_the_script_arm_and_the_deferred_simulator_arms() {
    let names = roster();
    for (script, _) in vike_strategy::SCRIPT_ONLY {
        assert!(!names.contains(script), "{script} must not be on the named-run roster");
    }
    for (sim, why) in vike_strategy::SIMULATOR_ONLY {
        assert!(
            !names.contains(sim),
            "{sim} is DEFERRED from the named-run roster by \
             docs/decisions/0064-a-named-run-carries-no-source.md's decision 2 — it lives in \
             vike-backtest, which CAN name vike-script, so it is outside the compiler-free \
             closure. Its own reason for living there: {why}. Admitting it is a REOPENER of \
             0064; the route that reopens nothing is moving the arm below vike-script's layer \
             rank."
        );
    }
}

/// The roster is free of duplicates and non-empty — a duplicate would be a picker showing one
/// strategy twice, and an empty roster would make every named run a refusal nobody could act on.
#[test]
fn the_roster_is_non_empty_and_free_of_duplicates() {
    let names = roster();
    assert!(!names.is_empty(), "the named-run roster must never be empty");
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "duplicate on the named-run roster: {names:?}");
}

/// An ordinary typo is `Unknown` and carries the name back, so the server can answer with its
/// roster rather than with a shrug.
#[test]
fn an_unknown_name_names_itself() {
    let empty = Value::Table(Default::default());
    // ⚠ `err()` rather than a `match` over the whole `Result`: the `Ok` arm is a
    // `Box<dyn Strategy<..> + Send>`, which implements no `Debug`, so a panic message naming
    // the whole result does not compile.
    match resolve::<TestBroker>("nope", &empty).err() {
        Some(NamedRunError::Unknown(n)) => assert_eq!(n, "nope"),
        other => panic!("expected Unknown, got {other:?}"),
    }
}
