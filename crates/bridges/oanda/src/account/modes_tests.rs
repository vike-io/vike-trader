use super::*;
use serde_json::json;

#[test]
fn the_summary_is_read_for_a_boolean_and_nothing_else() {
    for (summary, want) in [
        (json!({"account": {"hedgingEnabled": true}}), Some(true)),
        (json!({"account": {"hedgingEnabled": false}}), Some(false)),
        (json!({"account": {}}), None),
        (json!({}), None),
        // A word is not an answer: unknown is never read as either.
        (json!({"account": {"hedgingEnabled": "true"}}), None),
        (json!({"account": {"hedgingEnabled": 1}}), None),
        (json!({"hedgingEnabled": true}), None),
    ] {
        assert_eq!(parse_hedging_enabled(&summary), want, "{summary}");
    }
}

/// Only an account the venue SAID nets makes a close that waited read the position first; unknown is not netting.
#[test]
fn only_a_stated_netting_account_nets() {
    assert!(AccountHedging::stated(false).nets());
    assert!(!AccountHedging::stated(true).nets());
    assert!(!AccountHedging::default().nets());
}
