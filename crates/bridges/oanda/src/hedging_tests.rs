use super::*;
use serde_json::json;
use vike_model::OrderType;

fn order(kind: &str, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: "coid-1".into(),
        venue: VENUE.into(),
        symbol: "EURUSD".into(),
        side: -1,
        qty: 1000.0,
        order_type: OrderType::from_stored(kind),
        price: Some(1.09),
        trigger_price: Some(1.08),
        reduce_only,
        time_in_force: vike_model::TimeInForce::Gtc,
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

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

#[test]
fn an_order_that_does_not_reduce_never_carries_the_word_whatever_the_account_says() {
    for hedging in [Some(true), Some(false), None] {
        for kind in ["market", "limit", "stop"] {
            assert_eq!(fill_for(hedging, &order(kind, false)), Ok(None), "{hedging:?} {kind}");
        }
    }
}

#[test]
fn a_reduce_only_order_on_a_hedging_account_reduces_and_on_a_netting_one_is_left_alone() {
    for kind in ["market", "limit", "stop"] {
        assert_eq!(fill_for(Some(true), &order(kind, true)), Ok(Some("REDUCE_ONLY")), "{kind}");
        assert_eq!(fill_for(Some(false), &order(kind, true)), Ok(None), "{kind}");
    }
}

/// Unknown: the market order that opens the locked pair is refused by name; the resting orders (a protective
/// stop among them) go out as they did before.
#[test]
fn an_unknown_account_refuses_a_reduce_only_market_by_name_and_lets_a_resting_order_through() {
    let why = fill_for(None, &order("market", true)).expect_err("refused");
    for needle in ["reduce-only market order", "EUR_USD", "hedging setting is unknown", "locked"] {
        assert!(why.contains(needle), "`{needle}` is missing from: {why}");
    }
    assert_eq!(fill_for(None, &order("limit", true)), Ok(None));
    assert_eq!(fill_for(None, &order("stop", true)), Ok(None));
}
