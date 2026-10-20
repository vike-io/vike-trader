//! The socket reads behind the rate, over SCRIPTED ibapi streams: a pair's top of book (the tick types of a
//! forex quote, the label of the stream, IB's "no security definition"), and the account's base currency off
//! its `NetLiquidation` rows.
//!
//! DOCS-SHAPED, not measured (module doc of `fx_socket`): the rows are what `ibapi`'s `AccountSummary` and
//! `TickTypes` describe and IBKR's pages say (<https://interactivebrokers.github.io/tws-api/account_summary.html>,
//! <https://interactivebrokers.github.io/tws-api/message_codes.html>), constructed by hand.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use ibapi::contracts::tick_types::TickType;
use ibapi::market_data::MarketDataType;
use ibapi::market_data::realtime::{TickAttribute, TickPrice};
use ibapi::{Error, Notice};
use vike_model::DataDelay;

use super::*;

type Tick = Option<Result<SubscriptionItem<TickTypes>, Error>>;

const DEADLINE: Duration = Duration::from_millis(200);

fn script(items: Vec<Tick>) -> impl FnMut(Duration) -> Tick {
    let mut q: VecDeque<Tick> = items.into();
    move |_| q.pop_front().flatten()
}

fn price(tick_type: TickType, price: f64) -> Tick {
    Some(Ok(SubscriptionItem::Data(TickTypes::Price(TickPrice {
        tick_type,
        price,
        attributes: TickAttribute { can_auto_execute: false, past_limit: false, pre_open: false },
    }))))
}

fn notice(code: i32) -> Notice {
    Notice {
        request_id: Some(7),
        code,
        message: "scripted".into(),
        error_time: None,
        advanced_order_reject_json: String::new(),
    }
}

/// A forex pair's bid and ask arrive as separate price ticks; the book is built when both are in, and the
/// label says real-time for `Bid`/`Ask` ticks.
#[test]
fn a_bid_and_an_ask_make_a_real_time_book() {
    let got = book_from(
        script(vec![price(TickType::Bid, 150.09), price(TickType::Ask, 150.11)]),
        DEADLINE,
    );
    assert_eq!(got, Ok(FxBook { bid: 150.09, ask: 150.11, delay: DataDelay::RealTime }));
}

/// IB sends `-1` for a side it has no price for: it is read past, not taken for a price.
#[test]
fn a_side_with_no_price_is_read_past() {
    let got = book_from(
        script(vec![
            price(TickType::Bid, -1.0),
            price(TickType::Ask, -1.0),
            price(TickType::Bid, 1.0999),
            price(TickType::Ask, 1.1001),
        ]),
        DEADLINE,
    );
    assert_eq!(got, Ok(FxBook { bid: 1.0999, ask: 1.1001, delay: DataDelay::RealTime }));
}

/// A delayed tick makes a DELAYED book and a frozen announcement a FROZEN one: the label travels with the
/// prices, and `fx_rate` refuses both.
#[test]
fn a_delayed_or_frozen_stream_is_labelled() {
    let delayed = book_from(
        script(vec![price(TickType::DelayedBid, 1.0999), price(TickType::DelayedAsk, 1.1001)]),
        DEADLINE,
    )
    .expect("a book");
    assert_eq!(delayed.delay, DataDelay::DELAYED);

    let frozen = book_from(
        script(vec![
            Some(Ok(SubscriptionItem::Data(TickTypes::MarketDataType(MarketDataType::Frozen)))),
            price(TickType::Bid, 1.0999),
            price(TickType::Ask, 1.1001),
        ]),
        DEADLINE,
    )
    .expect("a book");
    assert_eq!(frozen.delay, DataDelay::Frozen);
}

/// Error 200 is "no such pair" (the other order may exist); any other failure is "not now"; a stream that
/// closes or goes silent is "not now"; a notice that says the link is gone ends the read.
#[test]
fn the_failures_of_a_pair_read_are_told_apart() {
    assert_eq!(
        book_from(script(vec![Some(Err(Error::Notice(notice(200))))]), DEADLINE),
        Err(FxFail::NoSuchPair)
    );
    for code in [354, 10_090, 162] {
        let got = book_from(script(vec![Some(Err(Error::Notice(notice(code))))]), DEADLINE);
        assert!(matches!(got, Err(FxFail::Unavailable(_))), "{code}: {got:?}");
    }
    let closed = book_from(script(vec![price(TickType::Bid, 1.0)]), DEADLINE);
    assert!(
        matches!(&closed, Err(FxFail::Unavailable(why)) if why.contains("no two-sided price")),
        "{closed:?}"
    );
    let gone = book_from(script(vec![Some(Ok(SubscriptionItem::Notice(notice(1100))))]), DEADLINE);
    assert!(
        matches!(&gone, Err(FxFail::Unavailable(why)) if why.contains("link is gone")),
        "{gone:?}"
    );
    // an advisory that does not say the link is gone is read past
    let advisory = book_from(
        script(vec![
            Some(Ok(SubscriptionItem::Notice(notice(2104)))),
            price(TickType::Bid, 1.0999),
            price(TickType::Ask, 1.1001),
        ]),
        DEADLINE,
    );
    assert!(advisory.is_ok(), "{advisory:?}");
}

/// A stream that never answers is "not now" at the deadline (the fake waits out the time it is given, as
/// `next_timeout` does).
#[test]
fn a_silent_stream_is_not_now_at_the_deadline() {
    let began = Instant::now();
    let got = book_from(
        |t: Duration| -> Tick {
            std::thread::sleep(t);
            None
        },
        Duration::from_millis(30),
    );
    assert!(matches!(got, Err(FxFail::Unavailable(_))), "{got:?}");
    assert!(began.elapsed() < Duration::from_secs(2));
}

/// The pair is the IDEALPRO CASH contract with the BASE currency as the symbol and the QUOTE as the
/// currency: `USD.JPY` is `symbol USD`, `currency JPY` (IBKR's contract page).
#[test]
fn the_pair_contract_names_the_base_as_the_symbol() {
    let c = fx_contract("USD", "JPY");
    assert_eq!(
        (c.symbol.0.as_str(), c.currency.0.as_str(), c.exchange.0.as_str()),
        ("USD", "JPY", "IDEALPRO")
    );
    assert_eq!(c.security_type, SecurityType::ForexPair);
    assert_eq!(c.contract_id, 0, "resolved by IB from the pair, not by a conId");
}

fn nl(account: &str, tag: &str, value: &str, ccy: &str) -> AccountSummary {
    AccountSummary {
        account: account.into(),
        tag: tag.into(),
        value: value.into(),
        currency: ccy.into(),
    }
}

/// The base currency is the currency of the account's `NetLiquidation` row; another account's rows and other
/// tags are not read.
#[test]
fn the_base_currency_is_the_net_liquidation_rows_currency() {
    let rows = [
        nl("DU1", "NetLiquidation", "1000000", "EUR"),
        nl("DU1", "TotalCashValue", "5", "USD"),
        nl("DU2", "NetLiquidation", "9", "JPY"),
    ];
    assert_eq!(base_currency_of(&rows, "DU1").as_deref(), Ok("EUR"));
    assert_eq!(base_currency_of(&rows, " DU2 ").as_deref(), Ok("JPY"));
    // the row's code is read trimmed and upper-cased
    assert_eq!(
        base_currency_of(&[nl("DU1", "NetLiquidation", "1", " usd ")], "DU1").as_deref(),
        Ok("USD")
    );
    // the same code twice is one currency
    let twice = [nl("DU1", "NetLiquidation", "1", "USD"), nl("DU1", "NetLiquidation", "1", "USD")];
    assert_eq!(base_currency_of(&twice, "DU1").as_deref(), Ok("USD"));
}

/// No row for the account, a row naming no currency, a code that is not three letters, and two rows that
/// disagree are none of them a base currency, and none is read as USD.
#[test]
fn a_summary_that_names_no_single_currency_is_no_base_currency() {
    let none = base_currency_of(&[nl("DU2", "NetLiquidation", "1", "USD")], "DU1");
    assert!(
        matches!(&none, Err(FactsError::Unavailable(w)) if w.contains("no NetLiquidation row")),
        "{none:?}"
    );
    assert!(matches!(base_currency_of(&[], "DU1"), Err(FactsError::Unavailable(_))));
    for bad in ["", "  ", "US", "USDX", "US1", "BASE"] {
        let got = base_currency_of(&[nl("DU1", "NetLiquidation", "1", bad)], "DU1");
        assert!(
            matches!(&got, Err(FactsError::Unavailable(w)) if w.contains("no currency code")),
            "{bad:?}: {got:?}"
        );
    }
    let split = [nl("DU1", "NetLiquidation", "1", "USD"), nl("DU1", "NetLiquidation", "1", "EUR")];
    let got = base_currency_of(&split, "DU1");
    assert!(matches!(&got, Err(FactsError::Refused(w)) if w.contains("USD, EUR")), "{got:?}");
}

type Row = Option<Result<SubscriptionItem<AccountSummaryResult>, Error>>;

fn summary(row: AccountSummary) -> Row {
    Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row))))
}

/// A double of the account-summary seam that counts the subscriptions open at once. Its `cancel` is the ONLY
/// thing that closes one, so a read that left a subscription to a later drop shows as one still open.
#[derive(Default)]
struct Subscriptions {
    open: AtomicUsize,
    most: AtomicUsize,
    opened: AtomicUsize,
}

impl Subscriptions {
    /// One `base_currency_via` read whose stream says `script`, or whose open fails (`None`).
    fn read(&self, script: Option<Vec<Row>>, account: &str) -> Result<String, FactsError> {
        let opens = script.is_some();
        let mut q: VecDeque<Row> = script.unwrap_or_default().into();
        base_currency_via(
            || {
                if !opens {
                    return Err(Error::NotImplemented);
                }
                let now = self.open.fetch_add(1, Ordering::SeqCst) + 1;
                self.most.fetch_max(now, Ordering::SeqCst);
                self.opened.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |(), _| q.pop_front().flatten(),
            |()| {
                self.open.fetch_sub(1, Ordering::SeqCst);
            },
            account,
        )
    }
}

/// **The base-currency read holds one account-summary subscription and gives it back at once** (V3 of the
/// vendor-doc verification: IBKR allows two active at a time, shared with the reconcile client's `$LEDGER:ALL` and
/// the drill's figures read): on a read that ended on its marker, on one that was cut short, on one that failed,
/// on rows that name no single currency and on an open that failed, no subscription is left open, and in all of
/// them together never more than one was open at once.
#[test]
fn the_base_currency_read_holds_one_summary_subscription_and_cancels_it_on_every_path() {
    let ok = || {
        vec![
            summary(nl("DU1", "NetLiquidation", "1", "EUR")),
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))),
        ]
    };
    let split = || {
        vec![
            summary(nl("DU1", "NetLiquidation", "1", "USD")),
            summary(nl("DU1", "NetLiquidation", "1", "EUR")),
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))),
        ]
    };
    let subs = Subscriptions::default();
    assert_eq!(subs.read(Some(ok()), "DU1").as_deref(), Ok("EUR"));
    assert_eq!(subs.open.load(Ordering::SeqCst), 0, "answered: cancelled");
    assert!(matches!(subs.read(Some(split()), "DU1"), Err(FactsError::Refused(_))));
    assert_eq!(subs.open.load(Ordering::SeqCst), 0, "rows that name no single currency: cancelled");
    let cut = vec![summary(nl("DU1", "NetLiquidation", "1", "USD"))];
    assert!(matches!(subs.read(Some(cut), "DU1"), Err(FactsError::Unavailable(_))));
    assert_eq!(subs.open.load(Ordering::SeqCst), 0, "cut short before the marker: cancelled");
    let failed = vec![Some(Err(Error::NotImplemented))];
    assert!(matches!(subs.read(Some(failed), "DU1"), Err(FactsError::Unavailable(_))));
    assert_eq!(subs.open.load(Ordering::SeqCst), 0, "failed mid-read: cancelled");
    assert!(matches!(subs.read(None, "DU1"), Err(FactsError::Unavailable(_))));
    assert_eq!(subs.open.load(Ordering::SeqCst), 0, "never opened, nothing to cancel");
    assert_eq!(subs.opened.load(Ordering::SeqCst), 4);
    assert_eq!(subs.most.load(Ordering::SeqCst), 1, "never more than one open at once");
}

/// The summary is read up to its end marker; a close before it is "not now" (a short list would read as a
/// missing row), and so is an error.
#[test]
fn the_summary_is_read_to_its_end_marker_or_not_at_all() {
    let mut q: VecDeque<Row> = vec![
        summary(nl("DU1", "NetLiquidation", "1", "USD")),
        summary(nl("DU1", "TotalCashValue", "1", "USD")),
        Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))),
        summary(nl("DU1", "Late", "1", "USD")),
    ]
    .into();
    let rows = summary_rows(move |_| q.pop_front().flatten(), DEADLINE).expect("ended");
    assert_eq!(rows.len(), 2, "the end marker stops the read");

    let mut cut: VecDeque<Row> = vec![summary(nl("DU1", "NetLiquidation", "1", "USD"))].into();
    let got = summary_rows(move |_| cut.pop_front().flatten(), DEADLINE);
    assert!(
        matches!(&got, Err(FactsError::Unavailable(w)) if w.contains("before its end marker")),
        "{got:?}"
    );

    let mut failed: VecDeque<Row> = vec![Some(Err(Error::NotImplemented))].into();
    let got = summary_rows(move |_| failed.pop_front().flatten(), DEADLINE);
    assert!(matches!(got, Err(FactsError::Unavailable(_))));
}
