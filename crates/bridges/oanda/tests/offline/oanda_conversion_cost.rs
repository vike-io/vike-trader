//! The engine's cash follows the venue's balance across a close in a pair quoted in ANOTHER currency than the
//! account's: the cost OANDA charges for converting the quote-currency result is booked with the fill.
//!
//! The bodies are CAPTURED (`tests/fixtures/practice_order_fills_2026-10-28.json`: the practice account's own
//! `ORDER_FILL` transactions, a 20000 unit `USD_ZAR` round trip and two `EUR_GBP` ones). On the practice
//! account the venue converts a loss with `lossQuoteHomeConversionFactor`, exactly 1.01 x the mid, and states
//! the cost of that on the fill as `plHomeConversionCost`; the balance moves by `pl`, which includes it. The
//! engine books a close at the mid (decision 0183), so before this was read the engine's cash ran about one
//! percent of the PnL ahead of the venue's balance, and the gap grew with every close.

use serde_json::Value;
use vike_exec::{Account, BalanceMode};
use vike_model::events::{Event, FillEvent};
use vike_oanda::recon_client::parse_fill_reports;
use vike_oanda::{SubmittedSymbols, decode_transaction_events, map_order_response};

const CAPTURE: &str = include_str!("../fixtures/practice_order_fills_2026-10-28.json");

/// The captured `ORDER_FILL` of transaction `id`.
fn captured(id: &str) -> Value {
    let all: Value = serde_json::from_str(CAPTURE).expect("the capture is JSON");
    all["transactions"]
        .as_array()
        .and_then(|t| t.iter().find(|t| t["id"] == id))
        .unwrap_or_else(|| panic!("transaction {id} is in the capture"))
        .clone()
}

fn figure(tx: &Value, key: &str) -> f64 {
    tx[key].as_str().and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("{key} in {tx}"))
}

/// The bare `Fill` a transactions-stream line decodes to, relabelled the way a submitted order's would be
/// (`symbols.record` is the crate's own: what the engine folds is the canonical `symbol`).
fn stream_fill(tx: &Value, symbol: &str) -> FillEvent {
    match decode_transaction_events(tx, &SubmittedSymbols::default()).into_iter().next() {
        Some(Event::Fill(f)) => FillEvent { symbol: symbol.into(), ..f },
        other => panic!("a bare Fill first: {other:?}"),
    }
}

/// The cost the venue states for the close, as the booking must carry it: a positive charge.
fn stated_cost(tx: &Value) -> f64 {
    -figure(tx, "plHomeConversionCost")
}

#[test]
fn a_stream_close_in_a_foreign_quote_carries_the_venues_conversion_cost_as_a_charge() {
    for (id, symbol) in [("132", "USDZAR"), ("128", "USDZAR"), ("124", "EURGBP"), ("120", "EURGBP")]
    {
        let tx = captured(id);
        let fill = stream_fill(&tx, symbol);
        assert!(stated_cost(&tx) > 0.0, "{id}: the capture states a cost");
        assert!(
            (fill.commission - stated_cost(&tx)).abs() < 1e-12,
            "{id}: the fill carries {} where the venue states {}",
            fill.commission,
            stated_cost(&tx)
        );
    }
}

/// The cost is the venue's own figure, not an inference from the factors: at one unit it is below the
/// venue's four decimals and still booked as stated.
#[test]
fn a_one_unit_close_books_the_stated_cost_however_small() {
    let tx = captured("57");
    let fill = stream_fill(&tx, "USDJPY");
    assert!((fill.commission - 0.00000119509518).abs() < 1e-15, "{}", fill.commission);
}

#[test]
fn the_opening_fill_and_a_pair_quoted_in_the_accounts_currency_book_no_conversion_cost() {
    // The opening fill of the USD_ZAR round trip (`pl` 0, `plHomeConversionCost` 0) ...
    assert_eq!(stream_fill(&captured("130"), "USDZAR").commission, 0.0);
    // ... and a close in EUR_USD on a USD account (the venue states a zero: nothing is converted).
    let eurusd = captured("108");
    assert_eq!(figure(&eurusd, "pl"), -0.14);
    assert_eq!(
        stream_fill(&eurusd, "EURUSD").commission,
        0.0,
        "a USD-quoted pair is booked as before"
    );
}

#[test]
fn the_order_post_reply_and_the_reconcile_report_carry_the_same_cost_as_the_stream() {
    let tx = captured("132");
    let want = stated_cost(&tx);
    // The inline fill of a market order: the POST reply's `orderFillTransaction`.
    let symbols = SubmittedSymbols::default();
    let coid = tx["clientOrderID"].as_str().expect("a coid");
    let resp = serde_json::json!({
        "orderCreateTransaction": {"id": "131", "type": "MARKET_ORDER"},
        "orderFillTransaction": tx,
        "lastTransactionID": "132"
    });
    let inline = map_order_response(coid, 1, &resp, &symbols)
        .into_iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .expect("the reply carries the fill");
    assert!((inline.commission - want).abs() < 1e-12, "POST reply: {}", inline.commission);
    // The reconcile pass reads the same transaction over `sinceid`: a `MissingFill` books what the stream would.
    let body =
        serde_json::json!({"transactions": [captured("130"), tx], "lastTransactionID": "132"});
    let reports = parse_fill_reports(&body, "USD_ZAR", "USDZAR", "USD");
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].commission, 0.0, "the opening fill");
    assert!((reports[1].commission - want).abs() < 1e-12, "reconcile: {}", reports[1].commission);
}

/// THE POINT, through a REAL account: the 20000 unit `USD_ZAR` round trip of 2026-10-28 07:09-07:35 UTC. The
/// engine's rate at the close was 0.060017 (measured: its realized PnL over the venue's `quotePL`); the
/// venue's balance went 99967.7909 -> 99932.4599. Before the venue's cost was booked the engine ended 0.35
/// USD ahead of the venue (one percent of the loss); with it the two agree to the rate's age and the unmarked
/// conversion (about 0.01 USD, 0.03 percent), and the engine's REALIZED PnL is unchanged: the cost is a charge.
#[test]
fn the_engines_cash_follows_the_venues_balance_across_the_captured_usdzar_round_trip() {
    let (open, close) = (captured("130"), captured("132"));
    let (start, venue_end) = (figure(&open, "accountBalance"), figure(&close, "accountBalance"));
    let mut account = Account::new(1.0, "oanda", None, BalanceMode::Delta);
    account.balance = start;
    let now = 1_793_000_000_000;
    account
        .admit_quoted("USDZAR", "ZAR", None, Some((0.060017, now)), "USD", now)
        .expect("ZAR admitted at the engine's measured rate");
    account.apply_fill(&stream_fill(&open, "USDZAR"));
    account.apply_fill(&stream_fill(&close, "USDZAR"));
    let quote_pnl = figure(&close, "quotePL");
    assert!(
        (account.realized_pnl - quote_pnl * 0.060017).abs() < 1e-6,
        "the realized PnL is still booked at the engine's rate: {}",
        account.realized_pnl
    );
    assert!(
        (account.fees_paid - stated_cost(&close)).abs() < 1e-9,
        "the venue's cost is in the fees: {}",
        account.fees_paid
    );
    // The engine's cash: the seed less its fees, plus the realized PnL the account keeps beside it.
    let cash = account.balance + account.realized_pnl;
    let gap = (cash - venue_end).abs();
    assert!(
        gap < 0.02,
        "the engine's cash {cash} is {gap:.4} USD from the venue's {venue_end} (the old gap was 0.35)"
    );
}
