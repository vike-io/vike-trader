//! Hyperliquid historical candle backfill — pages the keyless `candleSnapshot` `/info` endpoint
//! into [`vike_marketdata::Bar`]s (the offline twin of the live [`crate::feed`] candle stream).
//!
//! Endpoint: `POST /info` with `{"type":"candleSnapshot","req":{"coin","interval","startTime",
//! "endTime"}}` → a JSON array of candles sharing the SAME field shape as the live WS `candle`
//! frames (`t`/`T`/`s`/`i`/`o`/`c`/`h`/`l`/`v`/`n`, decimal-string OHLCV). The candle→`Bar` mapping
//! REUSES [`crate::feed::decode::candle_data_to_bar`] verbatim (the same fn the live feed decodes
//! each `candle` frame with), so an offline-backfilled bar is byte-identical to the live-streamed
//! one.
//!
//! [`fetch_candles_range`] is the entry point: HL caps a snapshot at [`MAX_CANDLES`] rows, so it
//! PAGES by advancing `startTime` past each page's last candle CLOSE (`T`) until `endTime` (the
//! shared `vike_data::source::page_walk_forward`, with this venue's cursor as its `PageWalkRow`
//! override), de-duping the boundary candle, and returns [`Bar`]s ascending by open-time. Keyless mainnet reads (no
//! credentials); the transport's own `RateGate` self-throttles and retries transient failures.

use serde_json::Value;

use vike_data::source::{PageWalkRow, PageWalkStuck, page_walk_forward};
use vike_marketdata::Bar;
use vike_utility::errors::CarriedRefusal;

use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal};

use crate::feed::decode::candle_data_to_bar;
use crate::refusals::HyperliquidHistoryRefusal;
use crate::transport::HyperliquidTransport;
use vike_model::VenueNetwork;

/// Max candles Hyperliquid returns in one `candleSnapshot` response (the venue caps a snapshot at
/// 5000 rows), so a wider `[start, end]` window must be paged.
const MAX_CANDLES: usize = 5000;

/// The `candleSnapshot` `/info` request body for one page: `{"type":"candleSnapshot","req":{coin,
/// interval,startTime,endTime}}`. `startTime`/`endTime` are epoch-ms. Pure — the network-free half,
/// unit-tested.
fn candle_snapshot_body(coin: &str, interval: &str, start_ms: i64, end_ms: i64) -> Value {
    serde_json::json!({
        "type": "candleSnapshot",
        "req": {
            "coin": coin,
            "interval": interval,
            "startTime": start_ms,
            "endTime": end_ms,
        }
    })
}

/// The last candle's CLOSE time (`T`, epoch-ms) in a `candleSnapshot` response array — the paging
/// cursor advances PAST this so the next page begins at the following candle. `None` for an
/// empty/malformed array (which ends the paging loop).
fn last_close_ms(response: &Value) -> Option<i64> {
    response.as_array()?.last()?.get("T").and_then(Value::as_i64)
}

/// ONE RAW `candleSnapshot` row as the shared forward walk reads it (`vike_data::source::PageWalkRow`):
/// its [`Bar`] (`None` for a malformed candle, dropped AFTER the walk), and the page's next cursor.
///
/// The walk counts these RAW rows against [`MAX_CANDLES`], BEFORE the tolerant per-row parse: a full
/// page with one bad row would parse to `MAX_CANDLES − 1` bars, and comparing THAT filtered count
/// would misread the full page as "history ran out" and silently truncate every older page.
///
/// The cursor is this venue's own (the trait's override): one ms past the page's LAST candle CLOSE
/// (`T`, [`last_close_ms`]), stamped on every row of the page, so the page's greatest cursor IS the
/// last row's; a page whose last row states no readable close carries `i64::MIN`, and the walk stops
/// there defensively (never re-fetches the same page), as this pager always has.
struct CandleRow {
    bar: Option<Bar>,
    page_next_ms: i64,
}

impl PageWalkRow for CandleRow {
    /// A malformed candle sits outside every window, so the walk keeps it from nothing it returns.
    fn ts_ms(&self) -> i64 {
        self.bar.as_ref().map_or(i64::MIN, |b| b.timestamp)
    }

    fn next_cursor_ms(&self) -> i64 {
        self.page_next_ms
    }
}

/// Map a `candleSnapshot` response body (a JSON ARRAY of candle objects — each the SAME shape as a
/// live WS `candle` frame's `data`: `t`/`T`/`s`/`i`/`o`/`c`/`h`/`l`/`v`/`n`) → one [`CandleRow`] per
/// RAW row, in wire order (ascending by open-time). REUSES the live mapper [`candle_data_to_bar`]
/// verbatim, so an offline-backfilled bar is byte-identical to the live-streamed one. A non-array
/// body yields no rows; a malformed candle keeps its row with no bar (mirrors the live feed's
/// per-frame tolerance).
fn candle_rows(response: &Value) -> Vec<CandleRow> {
    let page_next_ms = last_close_ms(response).map_or(i64::MIN, |close| close.saturating_add(1));
    response
        .as_array()
        .map(|rows| {
            rows.iter().map(|r| CandleRow { bar: candle_data_to_bar(r), page_next_ms }).collect()
        })
        .unwrap_or_default()
}

/// The candle walk over an injected `fetch(cursor)` of one `candleSnapshot` page: the shared
/// `vike_data::source::page_walk_forward` over [`candle_rows`] at [`MAX_CANDLES`] rows a page (a short
/// RAW page ends it, so does an empty one; a full one whose cursor would not move forward is the
/// error `stuck` words), its bars clipped to `[start_ms, end_ms]`, ascending and de-duplicated by
/// open time. Pure — the network-free paging, unit-tested.
fn walk_candle_pages<E>(
    start_ms: i64,
    end_ms: i64,
    stuck: impl FnOnce(PageWalkStuck) -> E,
    mut fetch: impl FnMut(i64) -> Result<Value, E>,
) -> Result<Vec<Bar>, E> {
    let rows = page_walk_forward(start_ms, end_ms, MAX_CANDLES, stuck, |cursor| {
        fetch(cursor).map(|response| candle_rows(&response))
    })?;
    Ok(rows.into_iter().filter_map(|row| row.bar).collect())
}

/// Fetch closed-candle history for the inclusive `[start_ms, end_ms]` window from Hyperliquid's
/// keyless `candleSnapshot` `/info` endpoint on **mainnet** (public reads), returning [`Bar`]s
/// ascending by open-time, de-duplicated.
///
/// HL caps a snapshot at [`MAX_CANDLES`], so this PAGES: each request asks `[cursor, end_ms]`; the
/// cursor then advances to one ms past the last returned candle's CLOSE (`T`) so the next page begins
/// at the following candle (no boundary re-fetch), and the loop stops once a page comes back short —
/// judged on the RAW response row count, BEFORE the tolerant per-row parse (see [`CandleRow`] for
/// why the filtered count would truncate history) — or the cursor passes `end_ms`. The walk's sort +
/// de-dup drops any boundary duplicate regardless of the venue's start/end inclusivity.
/// Network I/O (the transport's own `RateGate` self-throttles and retries transient rate-limit/server
/// failures); the pure paging [`walk_candle_pages`] (over the pure map [`candle_rows`]) is the
/// fixture-tested seam. A transport error is surfaced as its refusal, its id kept (the sibling
/// `fetch_klines_range`s the collect layer binds surface a `String`). `rest` is the mainnet `history` REST row
/// ([`KlineSourceHyperliquid::from_urls`]); the transport signs nothing, so its network word is mainnet's.
pub fn fetch_candles_range(
    rest: &str,
    coin: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, CarriedRefusal> {
    let transport = HyperliquidTransport::new(VenueNetwork::Mainnet, rest);
    let stuck = |at: PageWalkStuck| at.text(crate::transport::consts::VENUE, coin, interval).into();
    walk_candle_pages(start_ms, end_ms, stuck, |cursor| {
        let body = candle_snapshot_body(coin, interval, cursor, end_ms);
        transport.info(&body).map_err(|e| {
            let (coin, interval, why) = (coin.to_string(), interval.to_string(), e.to_string());
            HyperliquidHistoryRefusal::CandlesUnread { coin, interval, why }.carried()
        })
    })
}

// --- the one-symbol DISPATCH source, and its refusal --------------------------------------------
//
// The ONE dispatch registry (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s
// `KLINE_SOURCES`, whose order the datahub's `real_backfill_table` folds through the gatewayhub
// client's `KLINE_VENUES`) carries ONE symbol per job: the series' STORE name. The venue is asked by
// the coin the instrument's KEPT row carries (decision 0290), never by a name it was guessed from.

/// **The venue `coin` a stored series is fetched by** (decision 0290): the series' STORE name (`BTCUSDC.P`,
/// `KPEPEUSDC.P`, the spot pair `HYPEUSDC`, a builder-dex coin `XYZ-TSLAUSDC.P`) finds its KEPT row
/// (`vike_catalog::kept_list`, the datahub's `<project>/instruments/hyperliquid.json`: the MAINNET listing this
/// history reads), and the row's wire coin (`vike_catalog::wire_name_of_row`) is what `candleSnapshot` and
/// `fundingHistory` are asked for: `BTC`, `kPEPE`, `@107`, `xyz:TSLA`. A spot pair's history asks for its `@N`
/// (audit flag HL-2: asked as `HYPE-USDC` the venue answered HTTP 500).
///
/// No network, no store touch: the kept rows this process reads.
///
/// # Errors
/// No list is held, none of its rows is stored under `store_name` (an old spelling, `BTC`, `HYPE/USDC`, `@107`,
/// included), or two are: in the translator's words, before anything is asked of the venue. A wrongly-refused
/// name costs an error message; a wrongly-admitted one would write rows that a spent commit key makes permanent.
pub fn history_coin_for(store_name: &str) -> Result<String, HyperliquidHistoryRefusal> {
    let venue = crate::transport::consts::VENUE;
    let rows = vike_catalog::kept_list(venue);
    let rows = rows.as_deref().map_or(&[][..], Vec::as_slice);
    vike_catalog::names_of_store_name(rows, venue, store_name)
        .map(|names| names.wire)
        .map_err(|why| HyperliquidHistoryRefusal::UnstoredName { why })
}

/// This venue's row in the ONE kline registry (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s
/// `KLINE_SOURCES`) — the fetch half, store-free, and a row whose `fetch` can REFUSE before
/// the network.
///
/// ⚠ The refusal is why this row is more than the plain pager: [`fetch_candles_range`] pages a
/// venue `coin`, which [`history_coin_for`] reads off the series' kept row. Guessing one would fetch
/// the wrong book, store it under the requested symbol, and spend the commit key that makes a
/// corrective re-fetch a silent zero-row success.
pub struct KlineSourceHyperliquid {
    rest: String,
}

impl KlineSourceHyperliquid {
    /// The source on hyperliquid's address rows `urls`: the `live` `history` REST row, MAINNET for
    /// every tier (a demo account's backfill reads mainnet, as it always did; there is no testnet
    /// history path).
    ///
    /// # Errors
    /// The row's refusal.
    pub fn from_urls(urls: &vike_venues::venue_urls::VenueUrls) -> Result<Self, UrlRefusal> {
        let rest = crate::config::base_url(
            VenueNetwork::Mainnet,
            urls,
            UrlChannel::History,
            UrlProtocol::Rest,
        )?;
        Ok(Self { rest: rest.to_string() })
    }
}

impl vike_data::source::KlineSource for KlineSourceHyperliquid {
    fn venue(&self) -> &str {
        crate::transport::consts::VENUE
    }

    /// [`history_coin_for`] first — a `SourceError::Refused`, NOT a `Fetch`, because nothing was
    /// asked of the venue (see that variant's own doc) — then the bridge's paged
    /// `candleSnapshot` pager, asked for the row's coin.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        let coin =
            history_coin_for(symbol).map_err(|refusal| SourceError::Refused(refusal.carried()))?;
        fetch_candles_range(&self.rest, &coin, interval, start_ms, end_ms)
            .map_err(SourceError::Fetch)
    }
}

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;
