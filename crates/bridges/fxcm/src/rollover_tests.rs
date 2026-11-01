//! `crate::rollover`'s pure rule. The envelopes are in the grammar `fcshim.cpp`'s `pushFill` writes; no row here was
//! captured (the demo account has never held a trade overnight: every rollover it reported read 0.00).

use serde_json::{Value, json};

use super::*;

fn closing(trade_id: &str, rollover: Value) -> Value {
    json!({"kind": "fill", "order_id": "o-close", "trade_id": trade_id, "instrument": "EUR/USD",
           "side": "S", "amount": 10_000, "rate": 1.1, "commission": 0.0, "rollover": rollover, "ts": 7})
}

/// The sign is the vendor's word and nobody's measurement: this test is the place that has to be edited, on purpose,
/// when the kit's t26 FXCM script has read a closed trade that held a rollover.
#[test]
fn the_rollover_sign_is_pinned() {
    assert_eq!(
        ROLLOVER_BALANCE_SIGN, 1.0,
        "the closed row's figure is the signed balance change (funds ADDED)"
    );
}

#[test]
fn a_closing_fill_with_a_rollover_is_one_funding_event_in_the_balance_sign() {
    let paid =
        rollover_funding(&closing("101-close-o9", json!(-0.14)), Some("EUR")).expect("an event");
    assert_eq!((paid.symbol.as_str(), paid.amount, paid.ts), ("EURUSD", -0.14, 7));
    assert_eq!((paid.venue.as_str(), paid.ccy.as_ref().map(|c| c.as_str())), ("fxcm", Some("EUR")));
    assert_eq!(paid.position_side, PositionSide::Both);
    let earned = rollover_funding(&closing("101-close-o9", json!(0.31)), None).expect("an event");
    assert_eq!((earned.amount, earned.ccy.is_none()), (0.31, true));
}

#[test]
fn nothing_else_is_a_rollover() {
    assert!(rollover_funding(&closing("101-close-o9", json!(0.0)), None).is_none(), "zero");
    assert!(
        rollover_funding(&closing("101", json!(-0.14)), None).is_none(),
        "an opening fill (no close mark)"
    );
    assert!(rollover_funding(&closing("101-close-o9", json!("x")), None).is_none(), "not a number");
    assert!(rollover_funding(&closing("101-close-o9", Value::Null), None).is_none(), "null");
    let mut no_key = closing("101-close-o9", json!(1.0));
    no_key.as_object_mut().expect("an object").remove("rollover");
    assert!(rollover_funding(&no_key, None).is_none(), "an envelope of an older shim");
    let mut no_instrument = closing("101-close-o9", json!(1.0));
    no_instrument["instrument"] = json!("");
    assert!(rollover_funding(&no_instrument, None).is_none());
    let mut canceled = closing("101-close-o9", json!(1.0));
    canceled["kind"] = json!("canceled");
    assert!(rollover_funding(&canceled, None).is_none());
    assert!(rollover_funding(&json!([1, 2]), None).is_none());
    assert!(rollover_funding(&json!({"kind": "fill", "trade_id": 5}), None).is_none());
}

#[test]
fn a_closing_fill_is_booked_once_and_the_seen_set_is_bounded() {
    let before = unmeasured_bookings();
    let mut booked = RolloverBooked::default();
    let env = closing("101-close-o9", json!(-0.14));
    assert_eq!(booked.events(&env).len(), 1);
    assert!(booked.events(&env).is_empty(), "the re-delivered row");
    assert!(unmeasured_bookings() > before, "each booking is counted as unmeasured");
    for i in 0..SEEN_CAP + 3 {
        assert_eq!(booked.events(&closing(&format!("{i}-close-o"), json!(-0.01))).len(), 1);
    }
    assert_eq!(booked.seen.len(), SEEN_CAP);
    assert_eq!(
        booked.events(&env).len(),
        1,
        "forgotten after the cap, so a replay that late would book again"
    );
}

/// One closed row's figures: the row's own `net_pl` is what the venue says the trade realized.
fn closed_row(rollover: f64, gross: f64, net: f64, commission: f64) -> Value {
    let mut v = closing("101-close-o9", json!(rollover));
    v["gross_pl"] = json!(gross);
    v["net_pl"] = json!(net);
    v["commission"] = json!(commission);
    v
}

/// The venue's own arithmetic settles the sign when the row states gross and net: net = gross + rollover - commission
/// confirms it, net = gross - rollover - commission contradicts it, anything else proves nothing.
#[test]
fn the_rows_own_net_figure_confirms_or_contradicts_the_sign() {
    use SignCheck::*;
    assert_eq!(
        rollover_sign_check(&closed_row(-0.14, 1.00, 0.86, 0.0)),
        Confirmed,
        "a paid rollover"
    );
    assert_eq!(
        rollover_sign_check(&closed_row(0.31, 1.00, 1.21, 0.10)),
        Confirmed,
        "an earned one, a commission"
    );
    assert_eq!(
        rollover_sign_check(&closed_row(-0.14, 1.00, 1.14, 0.0)),
        Contradicted,
        "net = gross - rollover"
    );
    assert_eq!(rollover_sign_check(&closed_row(0.31, 1.00, 0.59, 0.10)), Contradicted);
    assert_eq!(
        rollover_sign_check(&closed_row(-0.14, 1.00, 5.00, 0.0)),
        Unexplained,
        "neither identity"
    );
    assert_eq!(
        rollover_sign_check(&closed_row(-0.14, 1.00, 0.861, 0.0)),
        Confirmed,
        "within the cent"
    );
    assert_eq!(rollover_sign_check(&closed_row(0.0, 1.00, 1.00, 0.0)), Nothing, "no rollover");
    assert_eq!(rollover_sign_check(&closing("101", json!(-0.14))), Nothing, "an opening fill");
    assert_eq!(
        rollover_sign_check(&closing("101-close-o9", json!(-0.14))),
        Nothing,
        "an old shim: no gross, no net"
    );
    assert_eq!(rollover_sign_check(&json!("x")), Nothing);
}

#[test]
fn the_currency_is_wanted_only_for_an_unbooked_rollover_while_unknown() {
    let mut booked = RolloverBooked::default();
    let env = closing("101-close-o9", json!(-0.14));
    assert!(booked.wants_ccy(&env));
    assert!(!booked.wants_ccy(&closing("101-close-o9", json!(0.0))), "no rollover to book");
    assert!(!booked.wants_ccy(&closing("101", json!(-0.14))), "an opening fill");
    booked.ccy = Some("EUR".into());
    assert!(!booked.wants_ccy(&env), "known");
    booked.ccy = None;
    let _ = booked.events(&env);
    assert!(!booked.wants_ccy(&env), "already booked");
}

#[test]
fn only_a_stated_currency_code_is_the_accounts_currency() {
    assert_eq!(stated_account_ccy(r#"{"BASE_CRNCY":"EUR","X":"1"}"#), Some("EUR".to_string()));
    assert_eq!(stated_account_ccy(r#"{"BASE_CRNCY":" gbp "}"#), Some("GBP".to_string()));
    assert_eq!(stated_account_ccy(r#"{"BASE_CRNCY":"EURO"}"#), None, "not a three-letter code");
    assert_eq!(stated_account_ccy(r#"{"BASE_CRNCY":"E1R"}"#), None);
    assert_eq!(stated_account_ccy(r#"{"OTHER":"EUR"}"#), None, "absent: never the assumed USD");
    assert_eq!(stated_account_ccy("not json"), None);
    assert_eq!(stated_account_ccy("[]"), None);
}
