//! The SOCKET backend of [`crate::account::quote_rate`]: the two reads the conversion needs from the gateway, over the
//! session's ONE data connection (`crate::feed::price::session`), asked through `crate::instrument::any_symbol::socket::FactsScope`
//! (one ask, one connection, at most one dial).
//!
//! * **A forex pair's top of book** ([`FxQuotesSocket`]): one streaming `reqMarketData` subscription on
//!   `symbol = BASE`, `secType = CASH`, `currency = QUOTE`, `exchange = IDEALPRO`, folded through the price
//!   lane's own [`QuoteAccumulator`] until it holds a usable bid and ask (with the label of the stream they
//!   arrived on), then CANCELLED by dropping the subscription. IB's "no security definition" (error 200)
//!   is [`QuoteRateError::NoPair`] (this ORDER of the two currencies is not listed); every other failure is
//!   "not now" ([`QuoteRateError::Unavailable`]).
//! * **The account's base currency** ([`base_currency`]): `reqAccountSummary` with the `NetLiquidation` tag,
//!   whose rows state the BASE currency in their currency attribute (the ibapi page of the tag list: "Net
//!   liquidation value of the account including cash and positions", a row is `account`, `tag`, `value`,
//!   `currency`: "The currency in which the value is expressed").
//!
//! ⚠ DOCS-SHAPED: neither read has run against a gateway. That a `NetLiquidation` row states the base
//! currency, that a forex pair of the account streams real-time bid and ask, and the tick types of a forex
//! quote are IBKR's documentation and `ibapi`'s, not a capture; `tests/ibkr_smokes/ibkr_fx_smoke.rs` is the
//! read-only vehicle that settles them.

use std::time::{Duration, Instant};

use ibapi::accounts::types::AccountGroup;
use ibapi::accounts::{AccountSummary, AccountSummaryResult, AccountSummaryTags};
use ibapi::client::blocking::Client;
use ibapi::market_data::realtime::TickTypes;
use ibapi::subscriptions::SubscriptionItem;
use vike_marketdata::TickQuote;
use vike_model::{QuoteRateError, StreamDelay};

use crate::account::quote_rate::FxQuotes;
use crate::feed::socket::link_gone_code;
use crate::feed::socket::map::QuoteAccumulator;
use crate::instrument::any_symbol::IbkrPropertiesError;
use crate::instrument::any_symbol::socket::fx_contract;
use crate::refusals::{IbkrSymbolRefusal, millis_of};

/// How long one pair is listened to for a usable bid and ask. Forex ticks arrive many times a second when the
/// market is open; a pair that says nothing in this time has no market to read a rate from. Six seconds bounds
/// one rate re-read of a held position (the node's once-a-minute refresh): a quiet pair costs the refresh this
/// much, not more, so a slow IBKR read cannot push another venue's refresh past the engine's 120 s window.
pub(crate) const BOOK_DEADLINE: Duration = Duration::from_secs(6);

/// How long the account summary may take to end.
pub(crate) const SUMMARY_DEADLINE: Duration = Duration::from_secs(6);

/// IB's "No security definition has been found for the request".
const NO_SECURITY_DEFINITION: i32 = 200;

/// An `ibapi` failure of a pair read in this module's words.
fn fail_of(e: &ibapi::Error) -> QuoteRateError {
    match e {
        ibapi::Error::Notice(n) if n.code == NO_SECURITY_DEFINITION => {
            QuoteRateError::NoPair(e.to_string())
        }
        other => QuoteRateError::Unavailable(other.to_string()),
    }
}

/// The first usable two-sided quote `next` (an ibapi subscription's `next_timeout`) delivers within `within`,
/// with its label. A tick that completes no pair of positive prices is read past (IB sends `-1` for a side it
/// has no price for); a stream that closes or stays silent past the deadline is "not now"; a notice that says
/// the link is gone ends it. The label is the accumulator's: delayed if either side came off a delayed tick,
/// frozen if IB said it replays the last value.
pub(crate) fn book_from(
    mut next: impl FnMut(Duration) -> Option<Result<SubscriptionItem<TickTypes>, ibapi::Error>>,
    within: Duration,
) -> Result<(TickQuote, StreamDelay), QuoteRateError> {
    let end = Instant::now() + within;
    let mut accumulator = QuoteAccumulator::default();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(QuoteRateError::Unavailable(
                IbkrSymbolRefusal::NoTwoSidedQuote { millis: millis_of(within) }.to_string(),
            ));
        }
        match next(left) {
            Some(Ok(SubscriptionItem::Data(tick))) => {
                if let Some(q) = accumulator.apply(&tick)
                    && let Some(delay) = accumulator.delay()
                    && q.bid.is_finite()
                    && q.ask.is_finite()
                    && q.bid > 0.0
                    && q.ask > 0.0
                {
                    return Ok((q, delay));
                }
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                if link_gone_code(n.code) {
                    return Err(QuoteRateError::Unavailable(
                        IbkrSymbolRefusal::QuoteLinkGone { code: n.code, message: n.message }
                            .to_string(),
                    ));
                }
            }
            Some(Err(e)) => return Err(fail_of(&e)),
            None => {
                return Err(QuoteRateError::Unavailable(
                    IbkrSymbolRefusal::QuoteStreamSilent { millis: millis_of(within) }.to_string(),
                ));
            }
        }
    }
}

/// The first usable price `next` (an ibapi subscription's `next_timeout`) delivers within `within`, or `None`: a bid, an
/// ask, a last or a closing price, real-time or delayed ([`crate::feed::socket::map::reference_price_of`]). Only the BAND
/// a stock's price falls in is read off it (`crate::instrument::any_symbol`, "The grid"), so a frozen close is as good as a live
/// tick. A refused stream, a link-gone notice and a stream that stays silent past the deadline are all `None`: the
/// caller then takes the finest band.
pub(crate) fn price_from(
    mut next: impl FnMut(Duration) -> Option<Result<SubscriptionItem<TickTypes>, ibapi::Error>>,
    within: Duration,
) -> Option<f64> {
    let end = Instant::now() + within;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match next(left) {
            Some(Ok(SubscriptionItem::Data(tick))) => {
                if let Some(price) = crate::feed::socket::map::reference_price_of(&tick) {
                    return Some(price);
                }
            }
            Some(Ok(SubscriptionItem::Notice(n))) => {
                if link_gone_code(n.code) {
                    return None;
                }
            }
            Some(Err(e)) => {
                tracing::debug!(error = %e, "ibkr facts: the price read of the stock ended; its grid is the finest band");
                return None;
            }
            // A timeout, or a stream that ended cleanly (which answers at once, so it is not spun on).
            None => std::thread::sleep(left.min(Duration::from_millis(50))),
        }
    }
}

/// The pairs of one ibapi connection.
pub(crate) struct FxQuotesSocket<'a> {
    pub(crate) client: &'a Client,
}

impl FxQuotes for FxQuotesSocket<'_> {
    fn book(&self, base: &str, quote: &str) -> Result<(TickQuote, StreamDelay), QuoteRateError> {
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
) -> Result<String, IbkrPropertiesError> {
    let account = account.trim();
    let tag = AccountSummaryTags::NET_LIQUIDATION;
    let mut seen: Vec<String> = Vec::new();
    for row in rows.iter().filter(|r| r.tag == tag && r.account.trim() == account) {
        let currency = row.currency.trim().to_ascii_uppercase();
        if currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(IbkrPropertiesError::Unavailable(
                IbkrSymbolRefusal::SummaryCurrencyNotACode {
                    tag: tag.to_string(),
                    currency: row.currency.clone(),
                },
            ));
        }
        if !seen.contains(&currency) {
            seen.push(currency);
        }
    }
    match seen.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::SummaryNoRow {
            tag: tag.to_string(),
        })),
        many => Err(IbkrPropertiesError::Refused(IbkrSymbolRefusal::SummaryManyCurrencies {
            count: many.len(),
            currencies: many.join(", "),
            tag: tag.to_string(),
        })),
    }
}

/// The summary rows `next` (an ibapi subscription's `next_timeout`) delivers up to the end marker. A close or a
/// silence before the marker is "not now": a short list would read as a missing row.
fn summary_rows(
    mut next: impl FnMut(
        Duration,
    ) -> Option<Result<SubscriptionItem<AccountSummaryResult>, ibapi::Error>>,
    within: Duration,
) -> Result<Vec<AccountSummary>, IbkrPropertiesError> {
    let end = Instant::now() + within;
    let mut rows = Vec::new();
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::SummaryDidNotEnd {
                millis: millis_of(within),
            }));
        }
        match next(left) {
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::Summary(row)))) => rows.push(row),
            Some(Ok(SubscriptionItem::Data(AccountSummaryResult::End))) => return Ok(rows),
            Some(Ok(SubscriptionItem::Notice(_))) => {}
            Some(Err(e)) => {
                return Err(IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::SummaryFailed {
                    why: e.to_string(),
                }));
            }
            None => return Err(IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::SummaryCut)),
        }
    }
}

/// The base currency of `account`, asked of the gateway behind `client`.
pub(crate) fn base_currency(client: &Client, account: &str) -> Result<String, IbkrPropertiesError> {
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
/// subscriptions are allowed at a time", `.../account-summary/requesting-account-summary`), and measured
/// 2026-10-20 a SESSION (one TCP connection) is allowed two requests in all, a cancelled one included
/// (`crates/bridges/vike-ibkr/src/reconcile/summary.rs`'s module doc). The data connection, where this read runs, has
/// its OWN session count under `data_client_id`: it is not shared with the reconcile client's kept `$LEDGER:ALL`
/// subscription, which is on its own connection. So this code holds at most ONE at a time and only for as long as it
/// reads, and ⚠ each read is a request that stays counted on THIS connection: a third on one data connection is
/// refused (inferred from the probe, never observed).
fn base_currency_via<S>(
    open: impl FnOnce() -> Result<S, ibapi::Error>,
    mut next: impl FnMut(
        &S,
        Duration,
    ) -> Option<Result<SubscriptionItem<AccountSummaryResult>, ibapi::Error>>,
    cancel: impl FnOnce(&S),
    account: &str,
) -> Result<String, IbkrPropertiesError> {
    let sub = open().map_err(|e| {
        IbkrPropertiesError::Unavailable(IbkrSymbolRefusal::SummaryFailed { why: e.to_string() })
    })?;
    let rows = summary_rows(|t| next(&sub, t), SUMMARY_DEADLINE);
    cancel(&sub);
    base_currency_of(&rows?, account)
}

#[path = "socket_tests.rs"]
#[cfg(test)]
mod socket_tests;
