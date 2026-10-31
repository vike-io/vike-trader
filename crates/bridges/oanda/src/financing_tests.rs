//! `crate::financing`'s contract: the rows are DOCS-SHAPED (OANDA v20 `DailyFinancingTransaction`,
//! `PositionFinancing`, `OrderFillTransaction`); nothing here was captured from an account.

use serde_json::{Value, json};
use vike_exec::{Account, BalanceMode};

use super::*;

/// A `DAILY_FINANCING` row: the account total, then one entry per held instrument.
fn daily(id: &str, total: &str, positions: &[(&str, &str)]) -> Value {
    json!({
        "type": "DAILY_FINANCING", "id": id, "time": "1478012400.000000000",
        "financing": total, "accountBalance": "99999.5000",
        "positionFinancings": positions.iter().map(|(instrument, financing)| json!({
            "instrument": instrument, "financing": financing,
            "openTradeFinancings": [{"tradeID": "1", "financing": financing}]
        })).collect::<Vec<_>>()
    })
}

fn funding_of(events: &[Event]) -> Vec<&FundingEvent> {
    events
        .iter()
        .map(|e| match e {
            Event::Funding(f) => f,
            other => panic!("not a funding event: {other:?}"),
        })
        .collect()
}

fn no_home() -> impl FnMut() -> Option<String> {
    || None
}

/// The sign is a decision with no measurement behind it yet: this test is the place that has to be edited,
/// on purpose, when the kit's t26 script has read the venue.
#[test]
fn the_financing_sign_is_pinned() {
    assert_eq!(
        FINANCING_BALANCE_SIGN, 1.0,
        "financing is the signed balance change (negative = paid)"
    );
    assert_eq!(GUARANTEED_FEE_COST_SIGN, 1.0, "a positive guaranteedExecutionFee is a cost");
}

#[test]
fn a_paid_rollover_is_a_negative_amount_under_the_canonical_symbol_in_the_accounts_currency() {
    let tx = daily("500", "-0.7000", &[("EUR_USD", "-0.5000"), ("GBP_JPY", "-0.2000")]);
    let events = financing_events(&tx, Some("EUR"));
    let got = funding_of(&events);
    assert_eq!(got.len(), 2, "one event per position entry");
    assert_eq!((got[0].symbol.as_str(), got[0].amount), ("EURUSD", -0.5));
    assert_eq!((got[1].symbol.as_str(), got[1].amount), ("GBPJPY", -0.2));
    for f in got {
        assert_eq!(f.venue.as_str(), "oanda");
        assert_eq!(f.ccy.as_ref().map(|c| c.as_str()), Some("EUR"));
        assert_eq!(f.ts, 1_478_012_400_000);
        assert_eq!(f.position_side, PositionSide::Both);
    }
}

#[test]
fn a_collected_rollover_is_positive_and_a_zero_entry_is_nothing() {
    let tx = daily("501", "0.3000", &[("USD_JPY", "0.3000"), ("EUR_USD", "0.0000")]);
    let events = financing_events(&tx, None);
    let got = funding_of(&events);
    assert_eq!(got.len(), 1, "the zero entry books nothing");
    assert_eq!(
        (got[0].symbol.as_str(), got[0].amount, got[0].ccy.is_none()),
        ("USDJPY", 0.3, true)
    );
}

/// An instrument that is not a currency pair keeps the venue's spelling: that is what its mount carries.
#[test]
fn a_cfd_keeps_the_venue_spelling() {
    let events = financing_events(&daily("502", "-1", &[("SPX500_USD", "-1.0")]), None);
    assert_eq!(funding_of(&events)[0].symbol.as_str(), "SPX500_USD");
}

#[test]
fn an_order_fill_books_only_its_own_non_zero_financing() {
    let fill = |financing: &str| {
        json!({"type": "ORDER_FILL", "id": "700", "time": "1478012400.5", "instrument": "EUR_USD",
               "units": "-1000", "price": "1.1", "financing": financing, "commission": "0.0"})
    };
    let events = financing_events(&fill("-0.0420"), Some("USD"));
    let got = funding_of(&events);
    assert_eq!((got.len(), got[0].symbol.as_str(), got[0].amount), (1, "EURUSD", -0.042));
    assert_eq!(got[0].ts, 1_478_012_400_500);
    assert!(financing_events(&fill("0.0000"), None).is_empty(), "a fill with no financing");
    assert!(
        financing_events(&json!({"type": "ORDER_FILL", "id": "1", "instrument": "EUR_USD"}), None)
            .is_empty()
    );
    assert!(
        financing_events(&json!({"type": "ORDER_CANCEL", "id": "2", "financing": "-1"}), None)
            .is_empty()
    );
}

/// A total with nothing to route it by is the one thing a DAILY_FINANCING can state and not book.
#[test]
fn a_total_with_no_position_entry_is_refused_not_guessed() {
    let tx = daily("503", "-0.9000", &[]);
    assert_eq!(events_of(&tx, None), Err(-0.9));
    assert!(financing_events(&tx, None).is_empty());
    assert_eq!(
        events_of(&daily("504", "0.0000", &[]), None),
        Ok(Vec::new()),
        "nothing owed, nothing said"
    );
}

/// The reconnect overlap: the stream and the sinceid backfill both carry a rollover, and `apply_funding`
/// is not idempotent, so the lane books each transaction once.
#[test]
fn a_transaction_seen_twice_is_booked_once() {
    let mut lane = FinancingLane::default();
    let tx = daily("600", "-0.5", &[("EUR_USD", "-0.5")]);
    assert_eq!(lane.events(&tx, &mut no_home()).len(), 1);
    assert!(lane.events(&tx, &mut no_home()).is_empty(), "the backfill's copy");
    let other = daily("601", "-0.5", &[("EUR_USD", "-0.5")]);
    assert_eq!(
        lane.events(&other, &mut no_home()).len(),
        1,
        "the next day's row is another transaction"
    );
}

#[test]
fn the_seen_set_is_bounded_and_the_oldest_id_leaves_first() {
    let mut lane = FinancingLane::default();
    for i in 0..SEEN_CAP + 5 {
        let tx = daily(&format!("{}", 10_000 + i), "-0.1", &[("EUR_USD", "-0.1")]);
        assert_eq!(lane.events(&tx, &mut no_home()).len(), 1);
    }
    assert_eq!(lane.seen.len(), SEEN_CAP);
    let oldest = daily("10000", "-0.1", &[("EUR_USD", "-0.1")]);
    assert_eq!(
        lane.events(&oldest, &mut no_home()).len(),
        1,
        "forgotten, so a replay would book it again"
    );
    let newest = daily(&format!("{}", 10_000 + SEEN_CAP + 4), "-0.1", &[("EUR_USD", "-0.1")]);
    assert!(lane.events(&newest, &mut no_home()).is_empty());
}

/// The currency is asked only when there is something to book, and asked again until the venue answers.
#[test]
fn the_currency_is_read_lazily_and_until_it_answers() {
    let mut lane = FinancingLane::default();
    let mut asks = 0;
    let none = lane.events(&daily("700", "0", &[("EUR_USD", "0.0")]), &mut || {
        asks += 1;
        Some("EUR".to_string())
    });
    assert_eq!((none.len(), asks), (0, 0), "nothing to book: the venue is not asked");

    let first = lane.events(&daily("701", "-1", &[("EUR_USD", "-1")]), &mut || {
        asks += 1;
        None
    });
    assert_eq!(
        (asks, funding_of(&first)[0].ccy.is_none()),
        (1, true),
        "the venue did not answer: booked unstated"
    );
    let second = lane.events(&daily("702", "-1", &[("EUR_USD", "-1")]), &mut || {
        asks += 1;
        Some("EUR".to_string())
    });
    assert_eq!((asks, funding_of(&second)[0].ccy.as_ref().map(|c| c.as_str())), (2, Some("EUR")));
    let third = lane.events(&daily("703", "-1", &[("EUR_USD", "-1")]), &mut || {
        asks += 1;
        Some("USD".to_string())
    });
    assert_eq!(asks, 2, "answered once: never asked again");
    assert_eq!(funding_of(&third)[0].ccy.as_ref().map(|c| c.as_str()), Some("EUR"));
}

#[test]
fn a_row_without_an_id_is_not_booked() {
    let mut tx = daily("", "-1", &[("EUR_USD", "-1")]);
    assert!(FinancingLane::default().events(&tx, &mut no_home()).is_empty());
    tx.as_object_mut().expect("an object").remove("id");
    assert!(FinancingLane::default().events(&tx, &mut no_home()).is_empty());
}

/// The fee of a guaranteed stop rides on the fill's commission (one booking, the fill's own trade id).
#[test]
fn the_guaranteed_fee_is_added_to_the_fills_commission() {
    let before = unmeasured_bookings();
    let both = json!({"commission": "0.0200", "guaranteedExecutionFee": "0.1000"});
    assert!((fill_charges(&both) - 0.12).abs() < 1e-12);
    assert_eq!(fill_charges(&json!({"commission": "0.0200"})), 0.02);
    assert_eq!(fill_charges(&json!({"guaranteedExecutionFee": "0.1"})), 0.1);
    assert_eq!(
        fill_charges(&json!({"commission": "0.0200", "guaranteedExecutionFee": "0.0000"})),
        0.02
    );
    assert_eq!(fill_charges(&json!({})), 0.0);
    assert_eq!(fill_charges(&json!({"commission": "junk", "guaranteedExecutionFee": "nan"})), 0.0);
    assert!(unmeasured_bookings() >= before + 2, "each non-zero fee is counted as unmeasured");
}

/// A conversion mark-up is a cost whatever sign the venue states it with. Captured closes (all losses) state it
/// negative; a close at a gain was never captured, and a positive figure there must not CREDIT the cost.
#[test]
fn the_conversion_cost_is_a_charge_by_magnitude_whatever_its_sign() {
    for stated in ["-0.3422", "0.3422"] {
        let fill = json!({"commission": "0.0000", "plHomeConversionCost": stated});
        assert!((fill_charges(&fill) - 0.3422).abs() < 1e-12, "{stated}");
    }
}

/// The venue's own cost of converting a close's quote-currency result into the account's currency rides on
/// the fill's commission beside the other two charges; an absent, zero or unreadable figure books nothing.
#[test]
fn the_home_conversion_cost_is_added_to_the_fills_commission() {
    let all = json!({
        "commission": "0.0200", "guaranteedExecutionFee": "0.1000", "plHomeConversionCost": "-0.3000"
    });
    assert!((fill_charges(&all) - 0.42).abs() < 1e-12);
    let captured = json!({"commission": "0.0000", "plHomeConversionCost": "-0.34218998530854"});
    assert!((fill_charges(&captured) - 0.34218998530854).abs() < 1e-15);
    for none in [
        json!({"plHomeConversionCost": "0.00000000000000"}),
        json!({"plHomeConversionCost": "junk"}),
        json!({"plHomeConversionCost": "nan"}),
        json!({"homeConversionCost": "-0.3"}),
        json!({}),
    ] {
        assert_eq!(fill_charges(&none), 0.0, "{none}");
    }
}

/// The warning fires on the 1st, 10th, 100th ... booking, not on every one.
#[test]
fn the_unmeasured_sign_is_said_at_powers_of_ten() {
    let points: Vec<u64> = (1..=1_000).filter(|n| is_log_point(*n)).collect();
    assert_eq!(points, [1, 10, 100, 1_000]);
}

/// Through a REAL account: a rollover the venue debited lowers the balance and shows in `funding_paid`,
/// and the engine's canonical symbol is the one the event carries.
#[test]
fn a_paid_rollover_folds_into_the_accounts_balance_and_funding_paid() {
    let mut account = Account::new(1.0, "oanda", None, BalanceMode::Delta);
    account.balance = 1_000.0;
    let events = financing_events(&daily("800", "-0.5", &[("EUR_USD", "-0.5")]), None);
    let Event::Funding(f) = &events[0] else { panic!("a funding event") };
    account.apply_funding(f);
    assert!((account.balance - 999.5).abs() < 1e-12, "balance {}", account.balance);
    assert!((account.funding_paid + 0.5).abs() < 1e-12, "funding_paid {}", account.funding_paid);
}
