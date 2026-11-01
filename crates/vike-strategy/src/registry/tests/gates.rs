//! `PARAM_GATES`: every row names a declared key, and the `Gate` predicate can fail.

use crate::registry::gates::{Gate, PARAM_GATES};
use crate::registry::keys::{ParamKeys, param_keys};

/// [`PARAM_GATES`]' structural direction: a row may only name keys that exist, on a name that
/// exists, once. Same construction as `every_route_key_is_a_declared_key_of_the_right_type` and
/// for the same reason — a gate over a key no reader reads would exempt a knob that never
/// existed from the class-closer, and a gate READING a key [`resolved_params`] does not carry
/// would render `(absent)` forever, exempting its key unconditionally.
#[test]
fn every_gate_names_a_declared_key_of_the_same_strategy() {
    for (name, key, gate) in PARAM_GATES {
        let Some(ParamKeys::Declared(declared)) = param_keys(name) else {
            panic!("PARAM_GATES names {name}, which declares no params keys")
        };
        assert!(
            declared.iter().any(|(n, _)| n == key),
            "{name}'s gate is over `{key}`, which is not a declared PARAM_KEYS key"
        );
        for read in gate.keys() {
            assert!(
                declared.iter().any(|(n, _)| *n == read),
                "{name}'s gate on `{key}` reads `{read}`, which {name} does not declare — \
                     `resolved_params` would never carry it and the gate would read `(absent)` \
                     forever"
            );
            assert_ne!(
                read, *key,
                "{name}'s gate on `{key}` reads `{key}` — a key whose OWN value picks a branch \
                     is CONSUMED in both branches, which is the opposite of this table's claim"
            );
        }
        assert_eq!(
            PARAM_GATES.iter().filter(|(n, k, _)| n == name && k == key).count(),
            1,
            "{name}'s `{key}` has more than one gate row — say All(&[..])"
        );
    }
}

/// The gate's own mutation self-test — [`Gate::unmet`] is pure, so prove it says NOTHING for a
/// met gate and names the OFFENDING key for an unmet one, in every variant. Without this the
/// class-closer's exemptions could be vacuously green: a gate that never reports an unmet
/// conjunct exempts its key from direction 2 while direction 1 has nothing to measure.
#[test]
fn the_gate_predicate_can_actually_fail() {
    let rows = |pairs: &[(&'static str, &str)]| -> Vec<(&'static str, String)> {
        pairs.iter().map(|(k, v)| (*k, (*v).to_string())).collect()
    };
    let fixed = rows(&[("anchor", "fixed")]);
    let first = rows(&[("anchor", "first")]);
    assert!(Gate::Is("anchor", &["fixed"]).unmet(&fixed).is_empty());
    assert_eq!(Gate::Is("anchor", &["fixed"]).unmet(&first), vec!["anchor=first".to_string()]);
    // Positive is a NUMERIC test, so `0` and a negative both close it and a non-number does too.
    assert!(Gate::Positive("rungs").unmet(&rows(&[("rungs", "3")])).is_empty());
    assert_eq!(Gate::Positive("rungs").unmet(&rows(&[("rungs", "0")])), vec!["rungs=0"]);
    assert_eq!(Gate::Positive("size").unmet(&rows(&[("size", "-2")])), vec!["size=-2"]);
    assert_eq!(Gate::Positive("size").unmet(&rows(&[("size", "n/a")])), vec!["size=n/a"]);
    // `All` reports EVERY failing conjunct, so the operator sees all the reasons at once.
    let both = Gate::All(&[Gate::Positive("rungs"), Gate::Positive("size")]);
    assert!(both.unmet(&rows(&[("rungs", "3"), ("size", "1")])).is_empty());
    assert_eq!(
        both.unmet(&rows(&[("rungs", "0"), ("size", "0")])),
        vec!["rungs=0".to_string(), "size=0".to_string()]
    );
    // A key the echo does not carry is LOUD rather than silently "consumed".
    assert_eq!(Gate::Positive("nope").unmet(&fixed), vec!["nope=(absent)".to_string()]);
    // ...and `keys` reaches through `All`, which is what the structural gate walks.
    assert_eq!(both.keys(), vec!["rungs", "size"]);
}
