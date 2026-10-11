//! `alerts.json` load/save tests: disk round-trip, the never-brick load, the stated basename.

use super::*;
use crate::rule::{AlertRule, AlertRuleSet, Compare, RuleTrigger};
use crate::testkit::price_rule;

#[test]
fn disk_round_trip_and_missing_and_corrupt_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("alerts.json");

    // missing file → None (the OFF state), never an error.
    assert!(load_path(&p).is_none());

    let set = AlertRuleSet {
        rules: vec![
            price_rule("p", Compare::Above, 100_000.0),
            AlertRule::new("r", RuleTrigger::OrderRejected),
        ],
        ..Default::default()
    };
    save_path(&set, &p).unwrap();
    let back = load_path(&p).expect("saved file loads");
    assert_eq!(back, set, "disk round-trip is lossless");

    // corrupt file → None (never bricks), same never-brick contract as the workspace loader.
    std::fs::write(&p, "{ not valid json ]").unwrap();
    assert!(load_path(&p).is_none());
}

/// This module resolves NOTHING: the basename is all it states, and both entry points take a
/// caller-supplied `&Path`. The name is a cross-crate contract — `vike-tradehub`'s
/// `alerts_path` joins it onto the state directory — so a change on either side that did not
/// happen on the other would leave the daemon reading a file nobody writes.
#[test]
fn the_basename_is_all_this_module_states() {
    assert_eq!(ALERTS_FILE, "alerts.json");
}
