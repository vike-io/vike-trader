//! **Deribit's public option TRADE log** — the venue's half of the option-trades backfill lane: a paged,
//! keyless read of `public/get_last_trades_by_currency_and_time` that answers each trade with its implied
//! volatility, mark price and index price ([`vike_data::option_trade_log::OptionTradeRow`]), behind the one
//! seam the gatewayhub serves it through ([`OptionTradeSourceDeribit`], a `vike_data::source::OptionTradeSource`).
//!
//! Endpoint (keyless, no auth, no signer), on the `live` `history` REST row of product `trades`
//! (`https://history.deribit.com` in the seed):
//! `GET <base>/api/v2/public/get_last_trades_by_currency_and_time
//!      ?currency=&kind=option&start_timestamp=&end_timestamp=&count=&sorting=asc`
//!
//! MEASURED 2026-11-16 (a lane on the CI box, a few keyless pages):
//!
//! * **Only `history.deribit.com` serves the past.** The same request for 2024-06-03 on `www.deribit.com`
//!   answers `{"trades":[],"has_more":false}`, which would store an empty history as a success. The history
//!   host is current to the second (its newest BTC option trade was 1.4 s old), so a settled day is whole.
//! * **The log goes back to 2017** (BTC: one trade on 2017-01-01, `mark_price: null` that year); ETH's first
//!   trades are in 2019. `contracts` appears on later trades only and is not read (`amount` is the size).
//! * **`count` is at most 10,000** (`10001` is refused: `Invalid params`, `value is too high`). A full page
//!   says `has_more: true`; BTC 2024-06-03 was 14,251 trades in two pages (2.46 MB and 1.05 MB of JSON,
//!   4.3 s with the pause).
//! * **A strike with a fraction keeps the venue's lower-case `d`** (`XRP_USDC-26SEP25-1d6-P` is a 1.6
//!   strike). The one name parser, `crate::feed::options::chain::parse_instrument_name`, reads a `.`; this
//!   module hands it the name with the `d` read as the point, so its rules (the `DDMMMYY` calendar, the
//!   strike shape, the right) are the ones that judge it.
//! * A currency the venue does not know is its own `Invalid params` (`invalid currency`), passed through.
//!
//! # Paging: forward, from the last trade's millisecond, de-duplicated by trade id
//!
//! Many trades share one millisecond (a block trade's legs), so a page cannot resume one millisecond past
//! its last trade: the trades of that millisecond the page had no room for would be lost. The next page
//! starts AT the last trade's millisecond and the trades already taken there are skipped by their
//! `trade_id`. MEASURED: the venue's `start_timestamp` is INCLUSIVE (2024-06-03's second BTC page begins with
//! the first page's last trade), so without the de-duplication that day would hold 14,252 rows for its
//! 14,251 trades. A full page that ends on the millisecond it started at cannot move, and fails the fetch by
//! name, never a short answer. Pages run on the shared fallback pacer over `PAGE_DELAY` (the history
//! module's, `vike_venues::venue_rate_limits`' `DERIBIT` row): Deribit publishes no request weight, and one
//! page every 200 ms is a quarter of its public credit pool. That quarter is the process's, not the pull's: every
//! page waits for the history module's one page slot (`crate::history::history_page_fetch`), shared with the
//! candle, funding and DVOL pulls, so a backfill beside another leaves the live chain's polls their room.
//!
//! # What a row means
//!
//! The units are `vike_data::option_trade_log`'s module doc. `premium_in_coin` is the BOOK rule of
//! `crate::option_listing` (`OptionUnderlying::usd_quoted`): a trade read for currency `BTC` whose base is
//! `BTC` is priced in BTC; a trade read for `USDC` (`SOL_USDC-…`) is priced in USDC.
//!
//! # The chain's name: the live recorder's, by the live surface's own rule
//!
//! `underlying` names the chain the trade's hourly rows land in, and it is the name the LIVE chain recorder
//! gives the same book (decision 0290 names instruments and leaves an option chain's series unnamed, so the
//! live recorder's existing spelling is the one name, for both paths). That recorder serves each underlying
//! from ONE book, `crate::option_listing`'s rule over the venue's own listing: its coin book when the venue
//! lists coin-settled options on it, else the book its options settle in, under the underlying's own name
//! (`BTC`, `ETH`, `SOL` for `SOL_USDC-…`, `XRP`). So a trade's `underlying` is its base, the name the one
//! name parser gives it, for every book on that surface: every coin-settled trade, and a linear trade whose
//! underlying is listed with that linear book (or not listed at all: its traded book is the only one known).
//! A linear book the surface does NOT serve (`BTC_USDC-…`: BTC is served from its coin book) is a different
//! thing and keeps a different name, the instrument name's head (`BTC_USDC`), so it never mixes into the
//! coin book's chain ([`name_off_surface_books`]). The listing is read (`public/get_instruments`,
//! `currency=any`, `kind=option`, on the `live` `data` row) only when a fetch holds a linear trade, and held for
//! `crate::option_listing::LISTING_TTL_MS`.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use vike_bridge_core::pacer::Pacer;
use vike_bridge_core::retry::{JsonPageSpec, RetryBackoffPolicy, json_page_fetch};
use vike_data::option_trade_log::OptionTradeRow;
use vike_data::source::{OptionTradeSource, SourceError};
use vike_transport::http::blocking_agent;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::feed::options::chain::listing_of;
use crate::history::{PAGE_DELAY, VENUE, envelope_rate_limit, history_page_fetch};
use crate::option_listing::{LISTING_TTL_MS, OptionUnderlying};
use crate::refusals::DeribitHistoryRefusal;

/// The keyless public trade-log method's path, below the `history` REST base of product
/// [`HISTORY_PRODUCT`].
const PATH_TRADES: &str = "/api/v2/public/get_last_trades_by_currency_and_time";

/// The `venue_url` product word of the trade log's host: the history REST row that is NOT the chart
/// data's (`www.deribit.com` answers the past with nothing; the module doc).
pub const HISTORY_PRODUCT: &str = "trades";

/// The most trades one page asks for: the venue's own ceiling (the module doc).
pub const PAGE_ROWS: usize = 10_000;

/// The trade-log page's facts for the shared JSON page fetch: HTTP 429/418 or the `too_many_requests`
/// envelope code is a rate limit, retried on the history pages' shared backoff.
const TRADES_PAGE: JsonPageSpec = JsonPageSpec {
    label: "deribit option trades",
    json_what: "deribit option trades",
    headers: &[],
    limited: &[429, 418],
    business_limit: envelope_rate_limit,
    backoff: RetryBackoffPolicy::HISTORY_PAGE,
};

/// One parsed page: its trades in the venue's order, and whether the venue holds more after them.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionTradesPage {
    /// The page's trades.
    pub trades: Vec<OptionTradeRow>,
    /// The venue's `has_more`.
    pub has_more: bool,
}

/// One trade of a page read for `currency`, or why it cannot be one. Every field a row needs is
/// required; `iv` and `mark_price` may be absent or `null`.
fn trade_row(currency: &str, t: &Value) -> Result<OptionTradeRow, DeribitHistoryRefusal> {
    let field = |key: &str| t.get(key).filter(|v| !v.is_null());
    let without = |field: &'static str| DeribitHistoryRefusal::TradeWithoutField {
        field,
        trade: t.to_string(),
    };
    let number = |key: &str| {
        field(key).and_then(Value::as_f64).ok_or_else(|| {
            DeribitHistoryRefusal::TradeWithoutNumber { key: key.to_string(), trade: t.to_string() }
        })
    };
    let instrument = field("instrument_name")
        .and_then(Value::as_str)
        .ok_or_else(|| without("instrument_name"))?;
    // The one name parser, which reads the venue's `d` decimal mark itself (the module doc).
    let (base, date_iso, strike, kind) =
        crate::feed::options::chain::parse_instrument_name(instrument).ok_or_else(|| {
            DeribitHistoryRefusal::NotAnOptionName { instrument: instrument.to_string() }
        })?;
    let direction = field("direction").and_then(Value::as_str);
    let is_buyer_maker = match direction {
        Some("sell") => true,
        Some("buy") => false,
        other => {
            return Err(DeribitHistoryRefusal::TradeDirection {
                direction: other.map(str::to_string),
                trade: t.to_string(),
            });
        }
    };
    Ok(OptionTradeRow {
        ts: field("timestamp").and_then(Value::as_i64).ok_or_else(|| without("timestamp"))?,
        // The live surface's name for the chain; a linear book OFF that surface is renamed after the walk
        // ([`name_off_surface_books`]).
        underlying: base.clone(),
        instrument: instrument.to_string(),
        expiry_ms: vike_options::expiry_ms(&date_iso),
        strike,
        kind,
        trade_id: field("trade_id")
            .and_then(Value::as_str)
            .ok_or_else(|| without("trade_id"))?
            .to_string(),
        price: number("price")?,
        amount: number("amount")?,
        is_buyer_maker,
        // The venue publishes percent; the row holds a decimal.
        iv: field("iv").and_then(Value::as_f64).map(crate::feed::options::iv_from_percent),
        mark_price: field("mark_price").and_then(Value::as_f64),
        index_price: number("index_price")?,
        premium_in_coin: base == currency,
    })
}

/// Pure map: one `get_last_trades_by_currency_and_time` body read for `currency` → its page. The error
/// envelope is an `Err`, and so is any trade a row cannot be made of: a short answer would be stored.
pub fn parse_option_trades_page(
    currency: &str,
    v: &Value,
) -> Result<OptionTradesPage, DeribitHistoryRefusal> {
    if let Some(error) = crate::history::envelope_error(v, "deribit option trades") {
        return Err(error);
    }
    let missing = |what| DeribitHistoryRefusal::OptionTradesMissing { what };
    let result = v.get("result").ok_or(missing("result"))?;
    let trades = result.get("trades").and_then(Value::as_array).ok_or(missing("result.trades"))?;
    let has_more =
        result.get("has_more").and_then(Value::as_bool).ok_or(missing("result.has_more"))?;
    let trades = trades
        .iter()
        .map(|t| trade_row(currency, t))
        .collect::<Result<Vec<_>, DeribitHistoryRefusal>>()
        .map_err(|why| DeribitHistoryRefusal::OptionTradesBadTrade {
            currency: currency.to_string(),
            why: Box::new(why),
        })?;
    Ok(OptionTradesPage { trades, has_more })
}

/// **The forward walk over `[start_ms, end_ms]`** (the module doc's paging): `fetch_page(cursor)` answers
/// the page starting at `cursor`. Trades outside the window are dropped; the answer is ts-ascending (a
/// stable sort, so one millisecond keeps the venue's order) with each `trade_id` once.
pub fn walk_option_trades(
    start_ms: i64,
    end_ms: i64,
    mut fetch_page: impl FnMut(i64) -> Result<OptionTradesPage, String>,
) -> Result<Vec<OptionTradeRow>, DeribitHistoryRefusal> {
    let mut out: Vec<OptionTradeRow> = Vec::new();
    let mut cursor = start_ms;
    // The ids already taken at `cursor`, the one millisecond two pages can share.
    let mut taken_at_cursor: HashSet<String> = HashSet::new();
    while cursor <= end_ms {
        let page = fetch_page(cursor).map_err(|why| DeribitHistoryRefusal::Unread { why })?;
        let last = page.trades.iter().map(|t| t.ts).max();
        let from = || vike_model::time::epoch_ms_to_utc_timestamp_ms(cursor);
        let stuck = match last {
            _ if !page.has_more => None,
            None => Some(DeribitHistoryRefusal::OptionTradesEmptyPage { from: from() }),
            Some(last) if last <= cursor => {
                Some(DeribitHistoryRefusal::OptionTradesPageStuck { from: from() })
            }
            Some(_) => None,
        };
        if let Some(why) = stuck {
            return Err(why);
        }
        let next_taken: HashSet<String> =
            page.trades.iter().filter(|t| Some(t.ts) == last).map(|t| t.trade_id.clone()).collect();
        for trade in page.trades {
            if !(start_ms..=end_ms).contains(&trade.ts)
                || (trade.ts == cursor && taken_at_cursor.contains(&trade.trade_id))
            {
                continue;
            }
            out.push(trade);
        }
        match last {
            Some(last) if page.has_more => {
                cursor = last;
                taken_at_cursor = next_taken;
            }
            _ => break,
        }
    }
    out.sort_by_key(|t| t.ts);
    Ok(out)
}

/// The trade-log GET URL for one page.
fn trades_url(base: &str, currency: &str, start_ms: i64, end_ms: i64) -> String {
    format!(
        "{base}{PATH_TRADES}?currency={currency}&kind=option&start_timestamp={start_ms}&end_timestamp={end_ms}\
         &count={PAGE_ROWS}&sorting=asc"
    )
}

/// Every option trade of `currency` over `[start_ms, end_ms]` from the trade-log host `base`, paged and
/// paced (the module doc). Network I/O; keyless.
pub fn fetch_option_trades_range(
    base: &str,
    currency: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<OptionTradeRow>, String> {
    let agent = blocking_agent();
    let mut pacer = Pacer::fallback(PAGE_DELAY);
    let mut first = true;
    walk_option_trades(start_ms, end_ms, |cursor| {
        if !std::mem::take(&mut first) {
            std::thread::sleep(pacer.next_delay());
        }
        let started = Instant::now();
        let url = trades_url(base, currency, cursor, end_ms);
        let page = history_page_fetch(&agent, &url, &TRADES_PAGE, |v| {
            parse_option_trades_page(currency, v).map_err(|e| e.to_string())
        });
        pacer.observe_request(None, started.elapsed());
        page
    })
    .map_err(|why| why.to_string())
}

/// **Name each trade of a linear book the live surface does not serve after its own book** (the module
/// doc's chain name): `trades` were read for `currency`, `listed` is the venue's listing
/// (`crate::option_listing`'s rule). A coin-settled trade, and a linear one whose underlying is listed with
/// the `currency` book or not listed at all, keeps its base (`SOL`); a linear one whose underlying the
/// surface serves from another book (`BTC_USDC-…`, BTC served from its coin book) takes the name's head
/// (`BTC_USDC`).
pub fn name_off_surface_books(
    trades: &mut [OptionTradeRow],
    currency: &str,
    listed: &[OptionUnderlying],
) {
    for trade in trades.iter_mut().filter(|t| !t.premium_in_coin) {
        let on_the_surface =
            listed.iter().find(|u| u.name == trade.underlying).is_none_or(|u| u.book == currency);
        if !on_the_surface {
            let head = trade.instrument.split('-').next().unwrap_or_default().to_string();
            trade.underlying = head;
        }
    }
}

/// The listing read's page facts: the trade page's classification and backoff.
const LISTING_PAGE: JsonPageSpec = JsonPageSpec {
    label: "deribit option listing",
    json_what: "deribit option listing",
    ..TRADES_PAGE
};

/// **This venue's row of the option-trades lane**: the store-free fetch half, on the `live` `history` REST
/// row of product [`HISTORY_PRODUCT`] ([`OptionTradeSourceDeribit::from_urls`]), with the venue's option
/// listing read on the `live` `data` row when a fetch holds a linear trade (the chain name). The ingest it
/// feeds is `crates/vike-backfill/src/option_trades.rs`'s `backfill_option_trades`.
#[derive(Debug)]
pub struct OptionTradeSourceDeribit {
    base: String,
    /// `public/get_instruments?currency=any&kind=option` on the `live` `data` row.
    listing_url: String,
    /// The listing as last read, and when: read again once it is `LISTING_TTL_MS` old.
    listing: Mutex<Option<(Instant, Vec<OptionUnderlying>)>>,
}

impl OptionTradeSourceDeribit {
    /// The source on deribit's address rows `urls`: the trade log's history REST base and the public
    /// `data` REST base, mainnet's.
    ///
    /// # Errors
    /// A row's refusal (the seed's `history` `trades` row, or the `data` row, is missing).
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let data = urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?;
        Ok(Self {
            base: urls
                .url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, HISTORY_PRODUCT)?
                .to_string(),
            listing_url: crate::catalog::instruments_url(data, "any", "option"),
            listing: Mutex::new(None),
        })
    }

    /// The venue's option listing: the one held while it is younger than `LISTING_TTL_MS`, else read now
    /// (`crate::feed::options::chain::listing_of`, the live provider's reading). A failed read fails the
    /// fetch: a chain is never named by a guess.
    fn listed(&self) -> Result<Vec<OptionUnderlying>, String> {
        let mut held = self.listing.lock().unwrap_or_else(PoisonError::into_inner);
        let ttl = Duration::from_millis(LISTING_TTL_MS.unsigned_abs());
        if let Some((read, listed)) = held.as_ref()
            && read.elapsed() < ttl
        {
            return Ok(listed.clone());
        }
        let agent = blocking_agent();
        let listed = json_page_fetch(&agent, &self.listing_url, &LISTING_PAGE, |v| {
            listing_of(v).map_err(|e| e.to_string())
        })?;
        *held = Some((Instant::now(), listed.clone()));
        Ok(listed)
    }
}

impl OptionTradeSource for OptionTradeSourceDeribit {
    fn venue(&self) -> &str {
        VENUE
    }

    /// [`fetch_option_trades_range`] on this source's base. `currency` reaches the wire as the venue's own
    /// word, upper case (`BTC`, `ETH`, `USDC`); a word with anything but letters is refused before the
    /// venue is asked, since it is put into a URL.
    fn fetch(
        &self,
        currency: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<OptionTradeRow>, SourceError> {
        if currency.is_empty() || !currency.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(SourceError::Refused(
                DeribitHistoryRefusal::NotACurrencyWord { currency: currency.to_string() }
                    .carried(),
            ));
        }
        let mut trades = fetch_option_trades_range(&self.base, currency, start_ms, end_ms)
            .map_err(|why| SourceError::Fetch(why.into()))?;
        if trades.iter().any(|t| !t.premium_in_coin) {
            let listed = self.listed().map_err(|why| SourceError::Fetch(why.into()))?;
            name_off_surface_books(&mut trades, currency, &listed);
        }
        Ok(trades)
    }
}

#[path = "option_trades_tests.rs"]
#[cfg(test)]
mod option_trades_tests;
