//! OKX V5 REST kline (candlestick) history — the ONE kline fetch + JSON→[`Bar`] mapper for OKX.
//!
//! The OKX twin of binance's `data` module: a pure, fixture-tested `parse_okx_klines` map and a
//! paged, rate-limited `fetch_klines_range`. OKX differs from Binance/Bybit in four ways this module
//! localises:
//!   1. **Envelope + ordering.** The body is the V5 `{code, msg, data}` envelope (`code` a STRING,
//!      "0" = success), and `data` is **newest-first** arrays
//!      `[ts, open, high, low, close, volume, volCcy, volCcyQuote, confirm]`. `parse_okx_klines` maps
//!      the OHLC fields and the BASE volume of each row (`volume_column`: `vol` on a spot pair,
//!      `volCcy` on a derivative, whose `vol` counts contracts) and **reverses** to ascending-by-ts
//!      (the store's contract).
//!   2. **Endpoint + page size.** `GET /api/v5/market/history-candles` serves CLOSED historical
//!      candles, capped at **100** rows/request (far smaller than Binance's 1000).
//!   3. **Cursor paging.** There is no `start`/`end` pair — you page with `after` (records *earlier*
//!      than a ts) / `before` (records *newer*). Backfilling a `[start, end]` window means walking
//!      BACKWARD with `after`: seed `after = end+1`, then set `after` to each page's oldest ts (OKX
//!      `after` is exclusive, so no overlap) until a page reaches `start` or comes back empty. A SHORT
//!      page is no stop: termination follows the observed rows, so a venue-side change to its row cap
//!      cannot silently truncate a backfill (the funding walk's rule, and every backward walk's).
//!   4. **Interval codes + Cloudflare.** `bar` uses lowercase minutes but uppercase hour/day/week
//!      ("1m"→"1m", "1h"→"1H", "1d"→"1D"); [`bar_code`] maps the binance-style input. Requests carry
//!      the browser User-Agent OKX's Cloudflare front expects (reused from the OKX transport).
//!
//! Endpoint: `GET {base}/api/v5/market/history-candles?instId=&bar=&after=&limit=100`, where `base` is the
//! `okx` `live` `history` `rest` row ([`KlineSourceOkx::from_urls`]; the funding history reads the live `data`
//! row, [`FundingRateSourceOkx::from_urls`]). Only `fetch_klines_range` does network I/O; `parse_okx_klines` is a pure,
//! fixture-tested map.
//!
//! ## Rate-limit strategy (paged `fetch_klines_range`)
//! `history-candles` is IP-limited to ~20 req / 2 s. The pager throttles a fixed [`PAGE_DELAY`]
//! between pages and, on a rate-limit signal (code 50011/50061 — which OKX returns with HTTP 200 —
//! or HTTP 429), backs off exponentially while HONORING `Retry-After`, retrying the same page under
//! the shared history policy (`vike_bridge_core::retry::RetryBackoffPolicy::HISTORY_PAGE`: 6 retries, 1 s
//! doubling to 60 s) — one [`JsonPageSpec`] per endpoint ([`CANDLES_PAGE`], [`FUNDING_PAGE`]) carries
//! the words, the Cloudflare headers and the rate-limit statuses and codes, and
//! `vike_bridge_core::retry::json_page_fetch` asks and judges each page.
//!
//! ## The pager MEASURES its pages (and still sleeps [`PAGE_DELAY`])
//! OKX publishes no request-weight budget, so there is nothing to discover and nothing to pace a
//! fraction of — but that never stopped us timing our own round trips, and until now nobody had.
//! MEASURED on the CI box 2026-08-04: spot and perp `history-candles` pages average **476–486 ms**
//! against a hardcoded 200 ms [`PAGE_DELAY`], i.e. the constant is not even the dominant term of the
//! gap the venue actually sees.
//!
//! So the walk is the shared paced backward kline walk (`vike_bridge_core::klines`'s
//! `kline_page_walk_backward` on a `kline_page_pacer` over [`PAGE_DELAY`]): the sleep runs through
//! [`vike_bridge_core::pacer::Pacer::fallback`] and every page is wall-clocked into
//! [`vike_bridge_core::pacer::Pacer::observe_request`]. Two things follow, and a third deliberately
//! does not:
//!   * the backfill prints ONE `info` ETA line after its first page, instead of going silent for
//!     minutes in a way indistinguishable from a hang;
//!   * `fetch_klines_range_paced` (private) hands the observation back so a caller could persist it
//!     and an operator could SEE what a page costs here (none has persisted it since
//!     docs/decisions/0094 deleted `vike-backfill`'s pace file);
//!   * ⚠ **the delay does not change.** `Pacer::fallback::next_delay` returns exactly [`PAGE_DELAY`]
//!     whatever is observed (pinned by that module's tests and by this one's below). With no
//!     published ceiling, a delay derived from a stopwatch alone would be the same un-checkable
//!     hardcoded guess in newer clothes. Acting on the measurement is a separate change with its own
//!     measurement.

use std::time::Duration;

use vike_bridge_core::http::{body_head, get_raw, json_body};
use vike_bridge_core::klines::{kline_page_pacer, kline_page_walk_backward, string_row_to_bar};
use vike_bridge_core::retry::{JsonPageSpec, RetryBackoffPolicy, json_page_fetch};
use vike_marketdata::Bar;
use vike_model::rate_limits::PaceSample;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

// The Cloudflare browser UA is shared (dodges OKX's Cloudflare 1010/403 the same way the signed
// transport does).
use crate::BROWSER_UA;
use crate::instrument::InstrumentType;

/// The closed-candle history path, on a REST base.
const PATH_HISTORY_CANDLES: &str = "/api/v5/market/history-candles";
/// OKX's hard cap on history-candles returned by a single request.
const MAX_LIMIT: usize = 100;

// ---- paged-backfill rate-limit knobs (see the module rate-limit strategy) ----------------------
/// Fixed throttle between successive page requests (keeps us under ~20 req/2 s per IP).
///
/// The VALUE comes from `vike_venues::venue_rate_limits`'s `OKX` row rather than from a literal here:
/// OKX runs no request-weight meter and publishes no machine-readable rate-limit metadata, so this
/// delay IS the whole pacing story for history on this venue. The table models that absence as
/// `History::Unweighted` (no ceiling, no soft limit) instead of inventing a number, and records the
/// one figure that makes OKX's pace unlike its siblings': [`MAX_LIMIT`] is **100** rows where
/// binance/aster/bybit page 1000, so the same 200 ms buys a tenth of the history per request.
const PAGE_DELAY: Duration = vike_venues::venue_rate_limits::OKX.history.page_delay();
/// OKX rate-limit codes (arrive with HTTP 200 in the `{code}` envelope): request-too-frequent.
const RATE_LIMIT_CODES: [i64; 2] = [50011, 50061];

/// Map a binance-style interval ("1m","1h","1d",…) to OKX's `bar` code ("1m","1H","1D",…), from the
/// venue's word table in `vike_catalog::venue_interval_word` (the one table, whose membership is the
/// catalog's interval row too). OKX uses lowercase minutes but uppercase hour/day/week/month. Errors on
/// an unsupported interval. `pub(crate)` so the live kline feed ([`crate::feed`]) builds the
/// `candle<bar>` WS channel from the same map (OKX's WS candle channel suffix is exactly this bar code).
pub(crate) fn bar_code(interval: &str) -> Result<&'static str, String> {
    vike_catalog::venue_interval_word("okx", interval)
        .ok_or_else(|| format!("okx: unsupported interval {interval:?}"))
}

/// The candle row's column that holds `inst_id`'s volume in its BASE coin, which every venue's
/// `Bar.volume` is: `vol` (5) on a spot pair; `volCcy` (6) on a derivative (a swap, a dated future,
/// an option, an id of no known shape), whose `vol` counts CONTRACTS of `ctVal`. MEASURED 2026-11-08
/// on `history-candles` (`tests/fixtures/okx_klines_btc_usdt_swap_1m.json` and `…_btc_usd_swap_1m.json`):
/// `BTC-USDT-SWAP` (`ctVal` 0.01 BTC) `vol` 989.82, `volCcy` 9.8982; `BTC-USD-SWAP` (`ctVal` 100 USD)
/// `vol` 90.5, `volCcy` 0.1195 BTC, `volCcyQuote` 9,050 USD; `BTC-USDT` `vol` 0.1157 BTC, `volCcy` 8,795
/// USDT. The REST pager and the live candle lane both read it, so a live bar and a backfilled bar agree.
pub(crate) fn volume_column(inst_id: &str) -> usize {
    if InstrumentType::of_inst_id(inst_id) == Some(InstrumentType::Spot) { 5 } else { 6 }
}

/// Pull ascending-by-ts [`Bar`]s of `inst_id` out of a parsed V5 candles envelope: require
/// `code == "0"`, map `data` (newest-first) and reverse. Shared by the pure map and the pager.
fn bars_from_envelope(v: &serde_json::Value, inst_id: &str) -> Result<Vec<Bar>, String> {
    let code = v.get("code").and_then(serde_json::Value::as_str).unwrap_or("");
    if code != "0" {
        let msg = v.get("msg").and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("okx candles code {code}: {msg}"));
    }
    let data =
        v.get("data").and_then(serde_json::Value::as_array).ok_or("okx candles: missing data")?;
    let col = volume_column(inst_id);
    let mut bars = data
        .iter()
        .map(|row| {
            let r = row.as_array().ok_or_else(|| "okx candle row not array".to_string())?;
            let mut bar = string_row_to_bar(r, "okx candle", "ts")?;
            // The base volume, parsed as the shared mapper parses `vol` (0.0 on a miss).
            bar.volume = r
                .get(col)
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            Ok(bar)
        })
        .collect::<Result<Vec<Bar>, String>>()?;
    bars.reverse(); // OKX serves newest-first; the store wants ascending by ts
    Ok(bars)
}

/// Pure map: an OKX V5 candles response body of `inst_id` → `Vec<Bar>` ascending by ts. No network —
/// this is the fixture-tested seam.
pub fn parse_okx_klines(body: &str, inst_id: &str) -> Result<Vec<Bar>, String> {
    bars_from_envelope(&json_body(body, "okx candle")?, inst_id)
}

/// Build the `GET /api/v5/market/history-candles` URL on `base`: page BACKWARD with `after` (records
/// earlier than `after_ms`).
fn candles_url(base: &str, symbol: &str, bar: &str, after_ms: i64, limit: usize) -> String {
    format!("{base}{PATH_HISTORY_CANDLES}?instId={symbol}&bar={bar}&after={after_ms}&limit={limit}")
}

/// The headers every candles GET carries, handed to the shared `get_raw` as its extra-headers
/// slice: the browser User-Agent OKX's Cloudflare front expects (the same one the signed transport
/// sends — dodges the 1010/403) plus the Accept it pairs with. Dropping these re-trips Cloudflare,
/// so they are the ONE thing this venue adds to the shared raw GET.
const CANDLE_HEADERS: [(&str, &str); 2] =
    [("User-Agent", BROWSER_UA), ("Accept", "application/json, text/plain, */*")];

/// A readable body's rate-limit `code` (one of [`RATE_LIMIT_CODES`], which OKX answers with HTTP 200 and spells
/// as a string), as the note the exhaustion error carries.
fn rate_limit_code(v: &serde_json::Value) -> Option<String> {
    let code = v
        .get("code")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(-1);
    RATE_LIMIT_CODES.contains(&code).then(|| format!("code {code} (rate limited)"))
}

/// One `history-candles` page, as `vike_bridge_core::retry::json_page_fetch` asks and judges it: HTTP 429/418
/// (Cloudflare) or a `code` in [`RATE_LIMIT_CODES`] waits (honoring `Retry-After`) and asks the same page again;
/// the Cloudflare headers ride every request.
const CANDLES_PAGE: JsonPageSpec = JsonPageSpec {
    label: "okx candles",
    json_what: "okx candle",
    headers: &CANDLE_HEADERS,
    limited: &[429, 418],
    business_limit: rate_limit_code,
    backoff: RetryBackoffPolicy::HISTORY_PAGE,
};

/// ONE page of closed candles older than `after_ms` (exclusive), ascending by ts, under [`CANDLES_PAGE`].
fn fetch_page(
    agent: &ureq::Agent,
    base: &str,
    symbol: &str,
    bar: &str,
    after_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = candles_url(base, symbol, bar, after_ms, MAX_LIMIT);
    json_page_fetch(agent, &url, &CANDLES_PAGE, |v| bars_from_envelope(v, symbol))
}

/// The live feed's warmup seed: the newest `limit` CLOSED candles (no cursor), ascending by ts.
/// `history-candles` serves COMPLETED candles only, so — unlike binance/bybit — there is NO
/// still-forming tail here (the WS `candle` channel delivers the in-progress bar). A single request
/// carrying the Cloudflare browser UA (via [`get_raw`]). `limit` must be ≤ 100 (OKX's history-candles
/// cap). `base` is the REST base of the live `history` row. Network I/O. Errors on an unsupported
/// `interval`.
pub fn fetch_klines_latest(
    base: &str,
    symbol: &str,
    interval: &str,
    limit: usize,
) -> Result<Vec<Bar>, String> {
    let bar = bar_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let url = format!("{base}{PATH_HISTORY_CANDLES}?instId={symbol}&bar={bar}&limit={limit}");
    let raw = get_raw(&agent, &url, "okx candles", &CANDLE_HEADERS)?;
    if !(200..300).contains(&raw.status) {
        return Err(format!("okx candles HTTP {}: {}", raw.status, body_head(&raw.body)));
    }
    parse_okx_klines(&raw.body, symbol)
}

/// Fetch closed-candle history for the inclusive `[start_ms, end_ms]` window, paging BACKWARD through
/// OKX's 100-rows/response `history-candles` cap under the module's rate-limit policy. Walks `after`
/// down from `end+1` to `start`, then returns bars ascending by ts, de-duplicated. Network I/O.
/// Errors on an unsupported `interval`.
///
/// `fetch_klines_range_paced` with nothing seeded and the measurement dropped, so this shape — the
/// one every backfill seam binds — issues the identical requests on the identical sleeps. `base` is the
/// REST base of the live `history` row ([`KlineSourceOkx::from_urls`]).
pub fn fetch_klines_range(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_paced(base, symbol, interval, start_ms, end_ms, None)
        .map(|(bars, _pace)| bars)
}

/// This venue's row in the kline registry (`KLINE_SOURCES`): the store-free fetch half of a
/// backfill. The ingest it feeds is `crates/vike-backfill/src/kline_source.rs`'s
/// `backfill_kline_source`. It holds the REST base it pages on, resolved once by [`Self::from_urls`].
pub struct KlineSourceOkx {
    base: String,
}

impl KlineSourceOkx {
    /// The history source over `urls`' `okx` `live` `history` `rest` row (public history is
    /// tier-less: one mainnet base).
    ///
    /// # Errors
    /// The lookup's refusal (no such row).
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let base = urls.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")?;
        Ok(KlineSourceOkx { base: base.to_string() })
    }
}

impl vike_data::source::KlineSource for KlineSourceOkx {
    fn venue(&self) -> &str {
        "okx"
    }

    /// The bridge's own paged `fetch_klines_range` verbatim — `history-candles`, closed-candle-only,
    /// which is why the shared ingest's still-forming guard is a harmless no-op on this venue. An
    /// interval [`bar_code`] has no code for is refused (`Refused`: the venue is not asked).
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        bar_code(interval).map_err(vike_data::source::SourceError::Refused)?;
        fetch_klines_range(&self.base, symbol, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
}

/// [`fetch_klines_range`] with the run's PACE measurement carried in and out: `seed` is a previous
/// run's observation (or `None`, which is byte-identical), and the second element of the return is
/// THIS run's — `None` unless a page was actually timed, so a run that fetched nothing cannot report
/// a fabricated pace.
///
/// ⚠ Neither direction moves a single sleep. A seed on a `Fixed` pacer buys exactly one thing: an
/// ETA from page ZERO instead of page one (`Pacer::next_delay` still answers [`PAGE_DELAY`] — see
/// this module's "The pager MEASURES its pages"). The reported sample's `per_request_weight` is
/// likewise inert here: OKX publishes no weight counter, so the field carries the pacer's seed
/// constant and steers nothing — `request_ms` is the number an operator wants.
///
/// The CALLER owns any persistence; nothing in a bridge crate reads or writes operator state.
/// Private while [`fetch_klines_range`] is its one caller: nothing persists a pace since
/// docs/decisions/0094, so a public seed door would be a door nobody walks through.
fn fetch_klines_range_paced(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let bar = bar_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let pacer = kline_page_pacer("okx", PAGE_DELAY, seed);
    // A page ending at `page_end` INCLUSIVE asks `after = page_end + 1`: OKX's `after` is exclusive, so the first
    // page includes the end bar and the next one (`page_end` = the oldest ts - 1) starts strictly older, with no
    // overlap.
    kline_page_walk_backward("okx", symbol, interval, start_ms, end_ms, pacer, |page_end| {
        fetch_page(&agent, base, symbol, bar, page_end.saturating_add(1))
    })
}

// --- funding-rate source -------------------------------------------------------------------------

/// OKX V5 funding-rate history path (keyless public GET), paged with `after` like the candles.
const PATH_FUNDING_HISTORY: &str = "/api/v5/public/funding-rate-history";
/// Rows asked per funding page: the venue's documented maximum. MEASURED 2026-11-08: `limit=150`
/// answered 150 rows and `limit=400` the whole retained history of `BTC-USDT-SWAP` (283 rows). The
/// walk never stops on a short page, so a smaller real cap costs a page, never a row.
const FUNDING_LIMIT: usize = 400;

/// Pull ascending-by-ts funding points out of a parsed `funding-rate-history` envelope: `code == "0"`
/// or an `Err`, then `data` (newest first) of `{instId, fundingTime (epoch ms as a STRING),
/// realizedRate, fundingRate, …}`. The rate is `realizedRate`, the venue's "actual funding rate"
/// (`fundingRate` is its predicted one; the two were equal on all 283 rows measured 2026-11-08, and
/// `fundingRate` stands in for a row without a realized one). A row missing the time or both rates is
/// skipped. No premium, no mark.
fn funding_from_envelope(
    v: &serde_json::Value,
) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    let code = v.get("code").and_then(serde_json::Value::as_str).unwrap_or("");
    if code != "0" {
        let msg = v.get("msg").and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("okx funding code {code}: {msg}"));
    }
    let data =
        v.get("data").and_then(serde_json::Value::as_array).ok_or("okx funding: missing data")?;
    let rate_of = |row: &serde_json::Value, key: &str| {
        row.get(key).and_then(serde_json::Value::as_str).and_then(|s| s.parse::<f64>().ok())
    };
    let mut points: Vec<vike_marketdata::FundingRatePoint> = data
        .iter()
        .filter_map(|row| {
            let ts_ms = row.get("fundingTime")?.as_str()?.parse::<i64>().ok()?;
            let rate = rate_of(row, "realizedRate").or_else(|| rate_of(row, "fundingRate"))?;
            Some(vike_marketdata::FundingRatePoint {
                timestamp_ms: ts_ms,
                rate,
                premium: None,
                mark: None,
            })
        })
        .collect();
    points.reverse(); // OKX serves newest-first; the store wants ascending by ts
    Ok(points)
}

/// Pure map: a `funding-rate-history` body → funding points ascending by ts. No network — the
/// fixture-tested seam.
pub fn parse_okx_funding(body: &str) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    funding_from_envelope(&json_body(body, "okx funding")?)
}

/// One `funding-rate-history` page: [`CANDLES_PAGE`]'s rules under the funding words.
const FUNDING_PAGE: JsonPageSpec =
    JsonPageSpec { label: "okx funding", json_what: "okx funding", ..CANDLES_PAGE };

/// One funding page of points at or before `end_ms` (`after` is exclusive, so it is `end_ms + 1`),
/// under [`FUNDING_PAGE`].
fn fetch_funding_page(
    agent: &ureq::Agent,
    base: &str,
    inst_id: &str,
    end_ms: i64,
) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    let url = format!(
        "{base}{PATH_FUNDING_HISTORY}?instId={inst_id}&after={}&limit={FUNDING_LIMIT}",
        end_ms.saturating_add(1)
    );
    json_page_fetch(agent, &url, &FUNDING_PAGE, funding_from_envelope)
}

/// The suffix OKX's perpetual swaps carry (`BTC-USDT-SWAP` linear, `BTC-USD-SWAP` inverse): the only
/// instruments with a funding rate.
const SWAP_SUFFIX: &str = "-SWAP";

/// This venue's market funding-rate source, a row of the gatewayhub's `FUNDING_SOURCES`: the
/// perpetual swaps of both margins, named by their `instId` verbatim (the store key the kline lane
/// files them under). A spot pair (`BTC-USDT`) or a dated future has no funding and is REFUSED before
/// anything is asked.
///
/// ⚠ **The venue keeps about THREE MONTHS.** MEASURED 2026-11-08: the oldest `BTC-USDT-SWAP` row it
/// answers is 2026-08-06, and `after=` that row answers nothing. A window older than that comes back
/// empty, which the datahub writes as nothing (no commit key spent), never as an error.
///
/// The endpoint pages BACKWARD (`after` = records older than a time, exclusive, newest first), so the
/// walk is `vike_data::source::page_walk_backward`. Cadence: 8h on most swaps; the history lists
/// each settlement, so a symbol on another cadence is right as listed. It holds the REST base it pages
/// on, resolved once by [`Self::from_urls`].
pub struct FundingRateSourceOkx {
    base: String,
}

impl FundingRateSourceOkx {
    /// The funding-RATE source over `urls`' `okx` `live` `data` `rest` row (the public rate is
    /// tier-less; an account's OWN funding payments are `crate::execution::funding`'s, a `trade` read).
    ///
    /// # Errors
    /// The lookup's refusal (no such row).
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let base = urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?;
        Ok(FundingRateSourceOkx { base: base.to_string() })
    }
}

impl vike_data::source::FundingRateSource for FundingRateSourceOkx {
    fn venue(&self) -> &str {
        "okx"
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        if !symbol.ends_with(SWAP_SUFFIX) {
            return Err(SourceError::Refused(format!(
                "okx: {symbol:?} is not a perpetual swap, and funding is a swap's series — a swap's \
                 instId ends in `{SWAP_SUFFIX}` (`BTC-USDT-SWAP`, `BTC-USD-SWAP`)"
            )));
        }
        let agent = vike_bridge_core::http::blocking_agent();
        let mut first = true;
        let stuck = |at: vike_data::source::PageWalkStuck| {
            SourceError::Fetch(at.text("okx", symbol, vike_data::source::FUNDING_INTERVAL))
        };
        vike_data::source::page_walk_backward(start_ms, end_ms, stuck, |page_end| {
            if !first {
                std::thread::sleep(PAGE_DELAY);
            }
            first = false;
            fetch_funding_page(&agent, &self.base, symbol, page_end).map_err(SourceError::Fetch)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::pacer::remaining_pages;
    use vike_data::source::FundingRateSource;

    /// The committed venue answer (`instId=BTC-USDT-SWAP&limit=3`, 2026-11-08): newest first, the
    /// times and both rates STRINGS. Reversed to ascending, the rate the realized one, bit for bit.
    #[test]
    fn the_funding_fixture_maps_ascending_with_the_realized_rate() {
        let body = include_str!("../tests/fixtures/okx_funding_btc_usdt_swap.json");
        let got = parse_okx_funding(body).unwrap();
        let ts: Vec<i64> = got.iter().map(|p| p.timestamp_ms).collect();
        assert_eq!(ts, vec![1_794_096_000_000, 1_794_124_800_000, 1_794_153_600_000]);
        assert_eq!(got[0].rate.to_bits(), 0.000_091_402_872_521_4_f64.to_bits());
        assert_eq!(got[2].rate.to_bits(), 0.000_012_798_456_465_8_f64.to_bits());
        assert!(got.iter().all(|p| p.premium.is_none() && p.mark.is_none()));
        // 8h apart: the cadence is the spacing.
        assert!(got.windows(2).all(|w| w[1].timestamp_ms - w[0].timestamp_ms == 8 * 3_600_000));
    }

    #[test]
    fn a_realized_rate_wins_and_the_predicted_one_stands_in() {
        let body = r#"{"code":"0","msg":"","data":[
            {"fundingTime":"2","fundingRate":"0.0002","realizedRate":""},
            {"fundingTime":"1","fundingRate":"0.0003","realizedRate":"0.0001"},
            {"fundingRate":"0.0004","realizedRate":"0.0004"}]}"#;
        let got = parse_okx_funding(body).unwrap();
        assert_eq!(
            got.iter().map(|p| (p.timestamp_ms, p.rate)).collect::<Vec<_>>(),
            vec![(1, 0.0001), (2, 0.0002)]
        );
        let err = parse_okx_funding(r#"{"code":"51000","data":[],"msg":"Parameter instId error"}"#)
            .unwrap_err();
        assert!(err.contains("51000") && err.contains("instId"), "{err}");
    }

    /// A spot pair or a dated future is refused BEFORE any request: only a swap has funding.
    #[test]
    fn a_non_swap_is_refused_before_the_venue_is_asked() {
        let funding =
            FundingRateSourceOkx::from_urls(&VenueUrls::seed("okx")).expect("the seed's data row");
        for symbol in ["BTC-USDT", "BTC-USDT-251226", "BTCUSDT.P"] {
            let err = funding.fetch(symbol, 0, 1).unwrap_err();
            assert!(
                matches!(&err, vike_data::source::SourceError::Refused(t) if t.contains("-SWAP")),
                "{symbol}: {err:?}"
            );
        }
    }

    /// A minimal newest-first candles envelope with two rows (so the reversal is exercised).
    const TWO_ROWS: &str = r#"{"code":"0","msg":"","data":[
        ["1700000060000","27010.25","27100.00","27000.00","27080.10","8.10000000","219000.00","5913000.00","1"],
        ["1700000000000","27000.10","27050.50","26980.00","27010.25","12.34567800","333012.50","8991000.00","1"]
    ]}"#;

    /// A spot pair: its `vol` is the base and its `volCcy` the quote, the shape of [`TWO_ROWS`].
    const SPOT: &str = "BTC-USDT";

    /// A derivative's `Bar.volume` is its BASE coin, `volCcy`, never `vol`, which counts CONTRACTS: the
    /// recorded swap pages (`ctVal` 0.01 BTC on the linear one, 100 USD on the inverse one).
    #[test]
    fn a_swap_bar_carries_the_base_volume_not_the_contract_count() {
        let linear = include_str!("../tests/fixtures/okx_klines_btc_usdt_swap_1m.json");
        let bars = parse_okx_klines(linear, "BTC-USDT-SWAP").unwrap();
        let got: Vec<f64> = bars.iter().map(|b| b.volume).collect();
        assert_eq!(got, vec![12.3272, 15.9949, 9.8982], "volCcy, ascending");
        // ...which is the contract count times `ctVal` (989.82 contracts of 0.01 BTC).
        assert!((989.82_f64 * 0.01 - 9.8982).abs() < 1e-12);
        let inverse = include_str!("../tests/fixtures/okx_klines_btc_usd_swap_1m.json");
        let bars = parse_okx_klines(inverse, "BTC-USD-SWAP").unwrap();
        let got: Vec<f64> = bars.iter().map(|b| b.volume).collect();
        assert_eq!(got, vec![0.1694, 0.1685, 0.1195], "the coin, not 128.4 contracts of 100 USD");
        // A dated future and an option are derivatives too; a spot pair keeps `vol`.
        assert_eq!(volume_column("BTC-USD-261030"), 6);
        assert_eq!(volume_column("BTC-USD-261106-76000-C"), 6);
        assert_eq!(volume_column(SPOT), 5);
    }

    #[test]
    fn parse_reverses_to_ascending_and_maps_exactly() {
        let bars = parse_okx_klines(TWO_ROWS, SPOT).unwrap();
        assert_eq!(bars.len(), 2);
        // newest-first data → ascending bars: the LAST data row is bars[0].
        assert_eq!(bars[0].timestamp, 1_700_000_000_000);
        assert_eq!(bars[1].timestamp, 1_700_000_060_000);
        assert!(bars[0].timestamp < bars[1].timestamp, "ascending by ts after reversal");
        assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
        assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
        assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
        assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
        assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
        assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
    }

    #[test]
    fn parse_empty_data_is_empty() {
        assert!(parse_okx_klines(r#"{"code":"0","msg":"","data":[]}"#, SPOT).unwrap().is_empty());
    }

    #[test]
    fn parse_nonzero_code_errors() {
        assert!(
            parse_okx_klines(r#"{"code":"50011","msg":"Too Many Requests","data":[]}"#, SPOT)
                .is_err()
        );
    }

    #[test]
    fn parse_rejects_short_row() {
        assert!(
            parse_okx_klines(r#"{"code":"0","msg":"","data":[["1700000000000","1","2"]]}"#, SPOT)
                .is_err()
        );
    }

    /// Every arm, verbatim, and the refusal's words.
    #[test]
    fn bar_code_maps_or_errors() {
        for (interval, code) in [
            ("1m", "1m"),
            ("3m", "3m"),
            ("5m", "5m"),
            ("15m", "15m"),
            ("30m", "30m"),
            ("1h", "1H"),
            ("2h", "2H"),
            ("4h", "4H"),
            ("6h", "6H"),
            ("12h", "12H"),
            ("1d", "1D"),
            ("1w", "1W"),
            ("1M", "1M"),
        ] {
            assert_eq!(bar_code(interval), Ok(code), "{interval}");
        }
        for refused in ["7s", "1s", "3d", "8h", "1H", "1D", ""] {
            let want = format!("okx: unsupported interval {refused:?}");
            assert_eq!(bar_code(refused), Err(want), "{refused:?}");
        }
    }

    /// A previous run's record, shaped like the MEASURED okx page (the CI box, 2026-08-04: ~480 ms).
    /// `budget_per_min: None` because this venue publishes none — which is also what confines the
    /// record to another fallback pacer.
    fn okx_record(request_ms: u64) -> PaceSample {
        PaceSample { request_ms, per_request_weight: 1.0, budget_per_min: None, samples: 20 }
    }

    /// ⚠ THE load-bearing test of this change: the pager MEASURES and the delay does not move.
    /// [`PAGE_DELAY`] is what the loop slept before the pacer existed, and it is bit-for-bit what
    /// `next_delay()` answers after — unseeded, seeded, and after observations at every scale.
    ///
    /// Asserted against the CONST rather than a literal `200`, so retuning the constant (a separate
    /// change, with its own measurement) moves this test with it instead of leaving it stale.
    #[test]
    fn the_pager_measures_but_never_changes_the_page_delay() {
        vike_bridge_core::policy_pins::assert_the_pager_never_changes_its_fixed_delay(
            |seed| kline_page_pacer("okx", PAGE_DELAY, seed),
            PAGE_DELAY,
            &okx_record(480),
            "okx",
        );
    }

    /// What the measurement BUYS, on the shape that motivated it: one day of 1m candles is 14 of
    /// OKX's 100-row pages, and the real cost of that is ~9.5s of request time on top of 2.8s of
    /// sleep — a number the 200ms constant alone could never have produced, and the one the pager's
    /// ETA line now prints after page one.
    #[test]
    fn a_measured_page_yields_the_eta_the_fixed_delay_could_not() {
        let mut p = kline_page_pacer("okx", PAGE_DELAY, None);
        p.observe_request(None, Duration::from_millis(480));
        let pages = remaining_pages(86_400_000, "1m", MAX_LIMIT).expect("1m parses");
        assert_eq!(pages, 14, "a day of 1m candles at 100 rows/page");
        let eta = p.eta(pages).expect("a timed page yields an ETA");
        // 14 x (480ms + 200ms) = 9.52s. The sleep-only view would have claimed 2.8s.
        assert!(
            eta > Duration::from_millis(9_400) && eta < Duration::from_millis(9_700),
            "expected ~9.5s for the measured shape, got {eta:?}"
        );
        // A SEEDED pacer answers the same before page one has even been fetched.
        assert_eq!(
            kline_page_pacer("okx", PAGE_DELAY, Some(&okx_record(480))).eta(pages),
            Some(eta)
        );
        // ...and this run, having measured, hands its own observation back out to be persisted.
        let m = p.measured().expect("a fixed pager still times its pages");
        assert_eq!(m.request_ms, 480);
        assert_eq!(m.budget_per_min, None, "nothing discovered ⇒ nothing claimed");
        assert!(m.is_usable());
    }

    /// The history and funding sources page on the seed's live rows, and the dialled candles URL is
    /// byte-for-byte the one the code held before the addresses became rows.
    #[test]
    fn candles_url_pages_with_after() {
        let klines =
            KlineSourceOkx::from_urls(&VenueUrls::seed("okx")).expect("the seed's history row");
        let url = candles_url(&klines.base, "BTC-USDT", "1m", 20, 100);
        assert_eq!(
            url,
            "https://www.okx.com/api/v5/market/history-candles?instId=BTC-USDT&bar=1m&after=20&limit=100"
        );
        let funding =
            FundingRateSourceOkx::from_urls(&VenueUrls::seed("okx")).expect("the seed's data row");
        assert_eq!(funding.base, "https://www.okx.com");
        // No row, no source: the refusal names the row it lacks.
        let none = VenueUrls::default();
        assert!(
            KlineSourceOkx::from_urls(&none)
                .err()
                .expect("refused")
                .to_string()
                .contains("history")
        );
        assert!(
            FundingRateSourceOkx::from_urls(&none)
                .err()
                .expect("refused")
                .to_string()
                .contains("data")
        );
    }

    /// An interval okx has no `bar` code for is `Refused` (the venue was not asked), never a
    /// `Fetch`, with the table's own words.
    #[test]
    fn an_unsupported_interval_is_refused_before_the_venue_is_asked() {
        use vike_data::source::{KlineSource, SourceError};
        let klines =
            KlineSourceOkx::from_urls(&VenueUrls::seed("okx")).expect("the seed's history row");
        let err = klines.fetch("BTC-USDT", "7m", 0, 1).unwrap_err();
        assert_eq!(err, SourceError::Refused("okx: unsupported interval \"7m\"".into()));
    }
}
