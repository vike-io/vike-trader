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

use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal};

use crate::feed::decode::candle_data_to_bar;
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
fn walk_candle_pages(
    start_ms: i64,
    end_ms: i64,
    stuck: impl FnOnce(PageWalkStuck) -> String,
    mut fetch: impl FnMut(i64) -> Result<Value, String>,
) -> Result<Vec<Bar>, String> {
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
/// fixture-tested seam. A transport error is surfaced as a `String` (matching the sibling
/// `fetch_klines_range`s the collect layer binds). `rest` is the mainnet `history` REST row
/// ([`KlineSourceHyperliquid::from_urls`]); the transport signs nothing, so its network word is mainnet's.
pub fn fetch_candles_range(
    rest: &str,
    coin: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let transport = HyperliquidTransport::new(VenueNetwork::Mainnet, rest);
    let stuck = |at: PageWalkStuck| at.text(crate::transport::consts::VENUE, coin, interval);
    walk_candle_pages(start_ms, end_ms, stuck, |cursor| {
        let body = candle_snapshot_body(coin, interval, cursor, end_ms);
        transport
            .info(&body)
            .map_err(|e| format!("hyperliquid candleSnapshot {coin} {interval}: {e}"))
    })
}

// --- the one-symbol DISPATCH source, and its refusal --------------------------------------------
//
// The ONE dispatch registry (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s
// `KLINE_SOURCES`, whose order the datahub's `real_backfill_table` folds through the gatewayhub
// client's `KLINE_VENUES`) carries ONE symbol per job, so the
// only vike→HL mapping it can express is the identity `coin == symbol`. The one-shot
// `hyperliquid_backfill` program's `--symbols vikeSymbol=coin` grammar could express any mapping;
// docs/decisions/0094 deleted it, so a non-identity symbol has no path into the store until it has
// a canonical spelling — and this is where that is decided rather than guessed at.

/// The HL `coin` for a unified store `symbol` **when, and only when, the two are the same string**
/// — otherwise the reason the identity does not hold.
///
/// PURE: no network, no `spotMeta` read, no store touch. The rule is read off the venue's own
/// symbology rather than invented — `crates/bridges/hyperliquid/src/instrument/symbology.rs`'s `load_perps`
/// and `extend_with_perp_dex` both set `coin: name` and `symbol: name` from the SAME field, so
/// **every perp is identity-mapped**, core and HIP-3 alike (`BTC`, `kPEPE`, `test:BTC`).
/// `load_spot` is the one that breaks it: a spot pair's unified `symbol` is `BASE/QUOTE`
/// (`HYPE/USDC`) while its `coin` is `@<pairIndex>` (`@107`). That crate has stated the two
/// namespaces are DISJOINT since its catalog was written —
/// `crates/bridges/hyperliquid/src/catalog.rs` says so in its own module doc — which is what makes
/// a SYNTACTIC test sufficient here:
///
/// * **a `/`** ⇒ a spot pair. Its coin is `@N`, which this seam cannot resolve without a
///   `spotMeta` fetch it does not do. ⚠ `PURR/USDC` is the ONE pair whose coin IS its name
///   (`symbology.rs`'s `spot_asset_id_is_10000_plus_pair_index` pins it), so identity would be
///   correct for it — and it is refused anyway, for a second reason that applies to every pair:
///   `vike_store::HistoryStoreDataFusion`'s `series_dir` interpolates the symbol into
///   `format!("symbol={symbol}")`, so a `/` becomes a NESTED DIRECTORY in the store. Carving out
///   one venue-data literal would buy one symbol and hand back a partition-path defect.
/// * **an `@`** ⇒ a raw venue coin, not a unified symbol. The fetch would succeed and the rows
///   would land under `symbol=@107`, a partition no other producer and no catalog row ever spells
///   (`symbology.rs`'s `symbol_for_coin("@107")` answers `"HYPE/USDC"`), so the series would be
///   orphaned from every reader that looks it up by name.
///
/// Both refusals are one-sided in the safe direction: a wrongly-refused symbol costs an error
/// message, while a wrongly-admitted one writes rows that `commit_rows`' spent commit key makes
/// permanent (a corrective re-fetch of the same window answers `Ok(0)`).
pub fn identity_coin_for(symbol: &str) -> Result<&str, String> {
    if symbol.contains('/') {
        return Err(format!(
            "hyperliquid: {symbol:?} is a SPOT pair (`BASE/QUOTE`), whose venue `coin` is \
             `@<pairIndex>` rather than the pair name — this seam carries one symbol and can \
             only express `coin == symbol`, so it refuses rather than guessing a coin. A spot \
             pair has no store spelling yet either: the store refuses a `/` in a symbol \
             (`vike_paths::store_path::refuse_a_path_hostile_symbol`). Give the pair a canonical \
             spelling in `crates/vike-catalog/src/symbol.rs` first."
        ));
    }
    if symbol.contains('@') {
        return Err(format!(
            "hyperliquid: {symbol:?} is a raw venue `coin`, not a unified symbol — fetching it \
             would succeed and store the rows under a `symbol=` partition nothing else in the \
             store spells. Ask for the unified symbol instead."
        ));
    }
    if symbol.is_empty() {
        return Err("hyperliquid: empty symbol".to_string());
    }
    Ok(symbol)
}

/// This venue's row in the ONE kline registry (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s
/// `KLINE_SOURCES`) — the fetch half, store-free, and the only row whose `fetch` can REFUSE before
/// the network.
///
/// ⚠ The refusal is why this row is more than the plain pager: [`fetch_candles_range`] pages a
/// venue `coin`, and a one-symbol seam can only express `coin == symbol`. Guessing one would fetch
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

    /// [`identity_coin_for`] first — a `SourceError::Refused`, NOT a `Fetch`, because nothing was
    /// asked of the venue (see that variant's own doc) — then the bridge's paged
    /// `candleSnapshot` pager.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        let coin = identity_coin_for(symbol).map_err(vike_data::source::SourceError::Refused)?;
        fetch_candles_range(&self.rest, coin, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
}

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;
