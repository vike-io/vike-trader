//! The SOCKET backend of [`crate::fx_rate`]: the two reads the conversion needs from the gateway, over the
//! session's ONE data connection (`crate::data_session`), asked through `crate::any_symbol_socket::FactsScope`
//! (one ask, one connection, at most one dial).
//!
//! * **A forex pair's top of book** ([`SocketFx`]): one streaming `reqMktData` subscription on
//!   `symbol = BASE`, `secType = CASH`, `currency = QUOTE`, `exchange = IDEALPRO`, folded through the price
//!   lane's own [`QuoteAccumulator`] until it holds a usable bid and ask (with the label of the stream they
//!   arrived on), then CANCELLED by dropping the subscription. IB's "no security definition" (error 200)
//!   is [`FxFail::NoSuchPair`]; every other failure is "not now".
//! * **The account's base currency** ([`base_currency`]): `reqAccountSummary` with the `NetLiquidation` tag,
//!   whose rows state the BASE currency in their currency attribute (the ibapi page of the tag list: "Net
//!   liquidation value of the account including cash and positions", a row is `account`, `tag`, `value`,
//!   `currency`: "The currency in which the value is expressed").
//!
//! ⚠ DOCS-SHAPED: neither read has run against a gateway. That a `NetLiquidation` row states the base
//! currency, that a forex pair of the account streams real-time bid and ask, and the tick types of a forex
//! quote are IBKR's documentation and `ibapi`'s, not a capture; `tests/ibkr_fx_smoke.rs` is the
//! read-only vehicle that settles them.

use std::time::{Duration, Instant};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummary, AccountSummaryResult, AccountSummaryTags};
use ibapi::client::blocking::Client;
use ibapi::contracts::{Contract, Currency, Exchange, SecurityType, Symbol};
use ibapi::market_data::realtime::TickTypes;
use ibapi::subscriptions::SubscriptionItem;

use crate::any_symbol::FactsError;
use crate::fx_rate::{FxBook, FxFail, FxQuotes};
use crate::market_feed::link_gone_code;
use crate::market_feed::map::QuoteAccumulator;

/// How long one pair is listened to for a usable bid and ask. Forex ticks arrive many times a second when the
/// market is open; a pair that says nothing in this time has no market to read a rate from. Six seconds bounds
/// one rate re-read of a held position (the node's once-a-minute refresh): a quiet pair costs the refresh this
/// much, not more, so a slow IBKR read cannot push another venue's refresh past the engine's 120 s window.
pub(crate) const BOOK_DEADLINE: Duration = Duration::from_secs(6);

/// How long the account summary may take to end.
pub(crate) const SUMMARY_DEADLINE: Duration = Duration::from_secs(6);

/// IB's "No security definition has been found for the request".
const NO_SECURITY_DEFINITION: i32 = 200;

/// The IDEALPRO forex contract `base.quote`: `symbol` is the BASE currency and `currency` the QUOTE, so its
/// price is quote units per base unit.
pub(crate) fn fx_contract(base: &str, quote: &str) -> Contract {
    Contract {
        symbol: Symbol::from(base),
        security_type: SecurityType::ForexPair,
        exchange: Exchange::from("IDEALPRO"),
        currency: Currency::from(quote),
        ..Contract::default()
    }
}

/// An `ibapi` failure of a pair read in this module's words.
fn fail_of(e: &ibapi::Error) -> FxFail {
    match e {
        ibapi::Error::Notice(n) if n.code == NO_SECURITY_DEFINITION => FxFail::NoSuchPair,
        other => FxFail::Unavailable(other.to_string()),
    }
}

/// The first usable two-sided book `next` (an ibapi subscription's `next_timeout`) delivers within `within`.
/// A tick that completes no pair of positive prices is read past (IB sends `-1` for a side it has no price
/// for); a stream that closes or stays silent past the deadline is "not now"; a notice that says the link is
/// gone ends it. The label is the accumulator's: delayed if either side came off a delayed tick, frozen if IB
/// said it replays the last value.
pub(crate) fn book_from(
    mut next: impl FnMut(Duration) -> Option<Result<SubscriptionItem<TickTypes>, ibapi::Error>>,
    within: Duration,
) -> Result<FxBook, FxFail> {
    let end = Instant::now() + within;
    let mut acc = QuoteAccumulator::default();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(FxFail::Unavailable(format!(
                "no two-sided quote arrived within {} ms",
                within.as_millis()
            )));
        }
        match next(left) {
            Some(Ok(SubscriptionItem::Data(tick))) => {
                if let Some(q) = acc.apply(&tick)
                    && let Some(delay) = acc.delay()
                    && q.bid.is_finite()
                    && q.ask.is_finite()
                    && q.bid > 0.0
                    && q.ask > 0.0
                {
                    return Ok(FxBook { bid: q.bid, ask: q.ask, delay });
                }
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                if link_gone_code(n.code) {
                    return Err(FxFail::Unavailable(format!(
                        "the gateway's link is gone (notice {}: {})",
                        n.code, n.message
                    )));
                }
            }
            Some(Err(e)) => return Err(fail_of(&e)),
            None => {
                return Err(FxFail::Unavailable(format!(
                    "the quote stream ended or stayed silent for {} ms with no two-sided price",
                    within.as_millis()
                )));
            }
        }
    }
}

/// The pairs of one ibapi connection.
pub(crate) struct SocketFx<'a> {
    pub(crate) client: &'a Client,
}

impl FxQuotes for SocketFx<'_> {
    fn book(&self, base: &str, quote: &str) -> Result<FxBook, FxFail> {
        let contract = fx_contract(base, quote);
        let sub =
            self.client.market_data(&contract).streaming().subscribe().map_err(|e| fail_of(&e))?;
        // Dropping `sub` on return cancels the line, which frees one of the account's market-data lines.
        book_from(|t| sub.next_timeout(t), BOOK_DEADLINE)
    }
}

/// The account's BASE currency from the `NetLiquidation` rows of `rows` for `account`: the one three-letter
/// code they state. No row, a row that states no code and rows that disagree are not a currency.
pub(crate) fn base_currency_of(
    rows: &[AccountSummary],
    account: &str,
) -> Result<String, FactsError> {
    let account = account.trim();
    let tag = AccountSummaryTags::NET_LIQUIDATION;
    let mut seen: Vec<String> = Vec::new();
    for row in rows.iter().filter(|r| r.tag == tag && r.account.trim() == account) {
        let ccy = row.currency.trim().to_ascii_uppercase();
        if ccy.len() != 3 || !ccy.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(FactsError::Unavailable(format!(
                "ibkr: the account summary's {tag} row names `{}` as its currency, which is no \
                 currency code, so the account's base currency is not known",
                row.currency
            )));
        }
        if !seen.contains(&ccy) {
            seen.push(ccy);
        }
    }
    match seen.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(FactsError::Unavailable(format!(
            "ibkr: the account summary carries no {tag} row for the configured account, so its base \
             currency is not known"
        ))),
        many => Err(FactsError::Refused(format!(
            "ibkr: the account summary states {} different currencies ({}) for {tag}, so no single \
             base currency is known",
            many.len(),
            many.join(", ")
        ))),
    }
}

/// The summary rows `next` (an ibapi subscription's `next_timeout`) delivers up to the end marker. A close or a
/// silence before the marker is "not now": a short list would read as a missing row.
fn summary_rows(
    mut next: impl FnMut(
        Duration,
    ) -> Option<Result<SubscriptionItem<AccountSummaryResult>, ibapi::Error>>,
    within: Duration,
) -> Result<Vec<AccountSummary>, FactsError> {
    let end = Instant::now() + within;
    let mut rows = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(FactsError::Unavailable(format!(
                "ibkr: the account summary did not end within {} ms",
                within.as_millis()
            )));
        }
        match next(left) {
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row)))) => rows.push(row),
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => return Ok(rows),
            Some(Ok(SubscriptionItem::Notice(_))) => {}
            Some(Err(e)) => {
                return Err(FactsError::Unavailable(format!("ibkr: the account summary: {e}")));
            }
            None => {
                return Err(FactsError::Unavailable(
                    "ibkr: the account summary ended before its end marker".to_string(),
                ));
            }
        }
    }
}

/// The base currency of `account`, asked of the gateway behind `client`.
pub(crate) fn base_currency(client: &Client, account: &str) -> Result<String, FactsError> {
    let group = AccountGroup("All".to_string());
    base_currency_via(
        || client.account_summary(&group, &[AccountSummaryTags::NET_LIQUIDATION]),
        |sub, t| sub.next_timeout(t),
        |sub| sub.cancel(),
        account,
    )
}

/// [`base_currency`] over a subscription seam: `open` makes the ONE account-summary subscription, `next` reads
/// it and `cancel` ends it. **The subscription is cancelled the moment the read is over**, whether it ended on its
/// marker, failed or ran out of time, before its rows are looked at and not left to a drop at the end of the
/// scope. IBKR allows two active account-summary subscriptions at a time ("only two active summary
/// subscriptions are allowed at a time", `.../account-summary/requesting-account-summary`), and the reconcile
/// client's `$LEDGER:ALL` read and the drill's figures read use the same allowance, so this code holds at most
/// ONE and only for as long as it reads.
fn base_currency_via<S>(
    open: impl FnOnce() -> Result<S, ibapi::Error>,
    mut next: impl FnMut(
        &S,
        Duration,
    ) -> Option<Result<SubscriptionItem<AccountSummaryResult>, ibapi::Error>>,
    cancel: impl FnOnce(&S),
    account: &str,
) -> Result<String, FactsError> {
    let sub =
        open().map_err(|e| FactsError::Unavailable(format!("ibkr: the account summary: {e}")))?;
    let rows = summary_rows(|t| next(&sub, t), SUMMARY_DEADLINE);
    cancel(&sub);
    base_currency_of(&rows?, account)
}

#[path = "fx_socket_tests.rs"]
#[cfg(test)]
mod fx_socket_tests;
