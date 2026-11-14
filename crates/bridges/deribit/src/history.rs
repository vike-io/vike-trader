//! Deribit candle (OHLCV) history — the venue's keyless PUBLIC bar-history read, and the fourth
//! `history.rs` in the bridge family (binance/aster ride the shared `family::klines` rung, okx has its
//! own, this is deribit's). The Deribit twin of `vike_okx::history`: a pure, fixture-tested parse
//! (`parse_deribit_klines`) plus a paged, rate-limited [`fetch_klines_range`] with the shape every
//! backfill seam expects (`symbol, interval, start_ms, end_ms`) behind the REST base it reads.
//!
//! Endpoint (keyless, no auth, no signer — like [`crate::catalog`]/[`crate::feed::options::chain`]), on the `live`
//! `venue_url` row (`history` rest, `https://www.deribit.com` in the seed):
//! `GET <base>/api/v2/public/get_tradingview_chart_data
//!      ?instrument_name=&start_timestamp=&end_timestamp=&resolution=`
//!
//! `symbol` passes through VERBATIM (`BTC-PERPETUAL`). Deribit needs NO
//! [`vike_venues::PERP_SUFFIX`] handling — its instrument names are already unambiguous
//! (`uses_perp_suffix("deribit") == false`), so a perp and its spot twin can never collide the way
//! `BTCUSDT` does on binance/bybit/aster.
//!
//! Three things about this endpoint differ from every other kline venue, and each is why this
//! module exists rather than another `family::klines` face:
//!
//! 1. **The body is COLUMNAR, and its column order is NOT `ohlc`.** A real response reads
//!    `{result:{volume:[…], ticks:[…], status:"ok", open:[…], low:[…], high:[…], cost:[…],
//!    close:[…]}}` — `volume, ticks, status, open, LOW, HIGH, cost, close`. Every column is
//!    therefore read BY KEY; a positional read would silently swap high↔low. `Bar.volume` takes
//!    `volume` (BASE units), never `cost` (quote notional), consistent with every other venue.
//! 2. **Ragged columns are an ERROR, never a zip-truncate.** A short column is a malformed
//!    response; silently dropping the tail would fabricate a gap in the stored tape that no later
//!    scan could distinguish from real market silence.
//! 3. **Paging is BACKWARD, because the endpoint is END-ANCHORED and silently truncates.** Ask for
//!    a window wider than its ~5001-row cap and it drops the OLDER tail, answering `status:"ok"`
//!    with no error at all. MEASURED live against `BTC-PERPETUAL`, `resolution=1` (re-verified
//!    2026-08-04 while writing this module):
//!
//!    | requested | rows returned | first tick |
//!    |---|---|---|
//!    | 200 min | 201 | `= start_timestamp` |
//!    | 6,000 min | 5,001 | `end - 5000 min` — **`start_timestamp` ignored** |
//!    | 14,400 min | 5,001 | `end - 5000 min` — **`start_timestamp` ignored** |
//!
//!    So a naive single request for 24 months returns the newest ~3.5 days and LOOKS successful:
//!    a backfill would report success over a store series that is 99% empty, with no error
//!    anywhere. The cure is okx's pattern, not binance's — walk `end_timestamp` DOWN
//!    (`end = oldest_returned_tick - 1`) until the window start is reached or a page comes back
//!    empty, then sort ascending and dedup. Termination is driven by OBSERVED ticks; the 5001 cap
//!    is never hardcoded (it is not documented, and the venue may change it).
//!
//!    `end = oldest - 1` is contiguous, not lossy: the endpoint returns every bucket whose start
//!    falls in `[start, end]`, so the next page's newest bucket is exactly one resolution older
//!    than this page's oldest (verified live: a page starting `1785840000000` is preceded by a page
//!    ending `1785839940000`).
//!
//! ONE accepted residual of that termination rule (documented, not silently swallowed — okx's pager
//! has the identical shape): an EMPTY page stops the walk, so a hypothetical hole in the middle of a
//! window would hide anything older than it. In practice an empty page means "older than this
//! instrument's history", which is exactly where the walk should stop; a dense 1m perp series has no
//! interior holes to trip it.
//!
//! `status` is `"ok"` with populated arrays, or `"no_data"` with all-empty arrays for a window with
//! no candles (verified against a year-2001 window). **`no_data` is a NORMAL empty result** —
//! `Ok(vec![])`, which terminates the page walk — not an error. A genuine failure arrives as the
//! JSON-RPC `error` envelope instead (`{"error":{"code":-32602,"message":"Invalid params",
//! "data":{"reason":"instrument not found"}}}`), which is an `Err`.
//!
//! The walk is the shared one — `vike_data::source::page_walk_backward`, the ONE backward page walk for
//! klines and funding alike, paced by `vike_bridge_core::klines::kline_page_walk_backward` — so the
//! truncation trap is gated WITHOUT network I/O: it drives the whole walk over an INJECTED page
//! fetcher, and a test that only exercises a single page cannot see the silent-truncation bug at all.
//!
//! `BTC-PERPETUAL` is an inverse (`instrument_type: "reversed"`) instrument. That affects PnL math
//! downstream, not this ingest, and is out of scope here.
//!
//! ## The pager MEASURES its pages (and still sleeps [`PAGE_DELAY`])
//! Deribit publishes no request-weight budget for a pager to discover, so there is nothing to pace a
//! fraction of — but that never stopped us timing our own round trips, and until now nobody had. The
//! sibling venue was MEASURED on the CI box 2026-08-04 (okx: 476–486 ms per page against a hardcoded
//! 200 ms delay) and this venue is the same shape: the constant is not even the dominant term of the
//! gap the venue actually sees.
//!
//! So the sleep now runs through [`vike_bridge_core::pacer::Pacer::fallback`] and every page is
//! wall-clocked into [`vike_bridge_core::pacer::Pacer::observe_request`] (both inside the shared
//! `kline_page_walk_backward`). Two things follow, and a third deliberately does not:
//!   * the backfill prints ONE `info` ETA line after its first page (the shared walk's, under
//!     `vike_bridge_core::klines` with a `venue` field) — and this is the venue that
//!     needed it most, since a 24-month 1m pull here is the run whose silence the one-shot
//!     `deribit_backfill` program's doc warned about;
//!   * `fetch_klines_range_paced` (private) hands the observation back so a caller could persist it
//!     and an operator could SEE what a page costs here (none has persisted it since
//!     docs/decisions/0094 deleted `vike-backfill`'s pace file);
//!   * ⚠ **the delay does not change.** `Pacer::fallback::next_delay` returns exactly [`PAGE_DELAY`]
//!     whatever is observed (pinned by that module's tests and by
//!     `the_pager_measures_but_never_changes_the_page_delay` below). With no published ceiling, a
//!     delay derived from a stopwatch alone would be the same un-checkable hardcoded guess in newer
//!     clothes. Acting on the measurement is a separate change with its own measurement.
//!
//! The ETA's page count comes from the row count the FIRST page actually returned, never from a
//! constant — the same discipline point 3 above applies to termination. This venue's ~5001 cap is
//! undocumented, so there is no constant to use even if one wanted to.

use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::http::{blocking_agent, json_body};
use vike_bridge_core::klines::{kline_page_pacer, kline_page_walk_backward, kline_to_bar};
use vike_bridge_core::pacer::Pacer;
use vike_bridge_core::retry::{JsonPageSpec, RetryBackoffPolicy, json_page_fetch};
use vike_marketdata::Bar;
use vike_model::rate_limits::PaceSample;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// Canonical venue id — the partition `venue` a backfill seam keys this history on.
pub const VENUE: &str = "deribit";

/// The keyless public chart-data endpoint's path (no auth, no signer, no credentials), below the
/// `live` `history` REST base: MAINNET whatever any account's tier
/// (`crates/bridges/deribit/CLAUDE.md`'s "The two halves point at DIFFERENT NETWORKS").
const PATH_CHART: &str = "/api/v2/public/get_tradingview_chart_data";

// ---- paged-backfill rate-limit knobs -----------------------------------------------------------
/// Fixed throttle between successive page requests. Deribit meters public REST out of the
/// NON-matching-engine credit pool (~20 req/s sustained — see [`crate::transport::ratelimit`]); 200 ms keeps a
/// long backfill an order of magnitude under it while still pulling ~5000 bars/page.
///
/// The VALUE comes from `vike_venues::venue_rate_limits`'s `DERIBIT` row rather than from a literal
/// here: Deribit runs no request-weight meter and publishes no machine-readable rate-limit metadata,
/// so this delay IS the whole pacing story for history on this venue. The table models that absence
/// as `History::Unweighted` (no ceiling, no soft limit) instead of inventing a number; the ~20 req/s
/// credit pool above is documented prose, never a payload.
const PAGE_DELAY: Duration = vike_venues::venue_rate_limits::DERIBIT.history.page_delay();
/// Deribit's `too_many_requests` JSON-RPC error code. Like okx's 50011, it can arrive inside an
/// HTTP 200 body, so the classifier checks the envelope as well as the status line.
const RATE_LIMIT_CODE: i64 = 10_028;

/// Map a binance-style interval string to Deribit's `resolution`.
///
/// Deribit's resolution enum is full MINUTES as a bare number plus the daily `1D`:
/// `1, 3, 5, 10, 15, 30, 60, 120, 180, 360, 720, 1D`. Note the gaps — there is **no 4h** and no
/// weekly/monthly — so `"4h"`/`"1w"`/`"1M"` are errors here rather than a silently-wrong bucket
/// (verified live: `resolution=7` answers
/// `error.data.reason = "unsupported resolution"`).
///
/// `"1d"` maps to the documented `"1D"`. (`1440` is ALSO accepted by the live venue today —
/// verified 2026-08-04, byte-identical rows — but it is not in the published enum, so the
/// documented spelling is the one we send.)
///
/// The word table is the venue's row in `vike_catalog::venue_interval_word` (the one table, whose
/// membership is the catalog's interval row too).
pub fn resolution_code(interval: &str) -> Result<&'static str, String> {
    vike_catalog::venue_interval_word(VENUE, interval)
        .ok_or_else(|| format!("deribit: unsupported interval {interval:?}"))
}

/// The JSON-RPC `error` envelope rendered as a diagnostic, or `None` when the body carries no
/// error. Deribit puts the useful half in `error.data.reason` (`"instrument not found"`,
/// `"unsupported resolution"`), so both are surfaced.
fn envelope_error(v: &Value) -> Option<String> {
    let e = v.get("error")?;
    let code = e.get("code").and_then(Value::as_i64).unwrap_or(0);
    let msg = e.get("message").and_then(Value::as_str).unwrap_or("");
    let reason = e.get("data").and_then(|d| d.get("reason")).and_then(Value::as_str).unwrap_or("");
    Some(if reason.is_empty() {
        format!("deribit chart error {code}: {msg}")
    } else {
        format!("deribit chart error {code}: {msg} ({reason})")
    })
}

/// The JSON-RPC `error.code`, if this body is an error envelope — the rate-limit classifier's input.
fn envelope_error_code(v: &Value) -> Option<i64> {
    v.get("error")?.get("code")?.as_i64()
}

/// One numeric column, BY KEY. Every element must be a JSON number (an int coerces); a missing
/// column or a non-numeric cell is a malformed response, never a defaulted 0.0.
fn f64_column(result: &Value, name: &str) -> Result<Vec<f64>, String> {
    let arr = result
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("deribit chart: missing column {name:?}"))?;
    arr.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_f64().ok_or_else(|| format!("deribit chart: column {name}[{i}] not a number"))
        })
        .collect()
}

/// The `ticks` column (epoch-ms bucket starts), BY KEY. Same strictness as [`f64_column`].
fn i64_column(result: &Value, name: &str) -> Result<Vec<i64>, String> {
    let arr = result
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("deribit chart: missing column {name:?}"))?;
    arr.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_i64().ok_or_else(|| format!("deribit chart: column {name}[{i}] not an integer"))
        })
        .collect()
}

/// Pull [`Bar`]s out of a parsed chart-data envelope. Shared by the pure map and the pager.
///
/// Order of checks is load-bearing: the JSON-RPC `error` envelope first (it carries no `result` at
/// all), then `status` — `"no_data"` short-circuits to an EMPTY `Ok` (a normal answer for a window
/// with no candles), an unknown/missing `status` is an error (fail closed) — then the columns, read
/// BY KEY and length-checked against `ticks` BEFORE any zipping, so a ragged body is an `Err`
/// instead of a fabricated gap.
///
/// Wire order is preserved (Deribit serves ascending by ts, unlike okx's newest-first); the pager
/// sorts + dedups across pages regardless.
fn bars_from_envelope(v: &Value) -> Result<Vec<Bar>, String> {
    if let Some(err) = envelope_error(v) {
        return Err(err);
    }
    let result = v.get("result").ok_or("deribit chart: missing result")?;
    match result.get("status").and_then(Value::as_str) {
        Some("ok") => {}
        // A window with no candles: all columns empty. A normal empty answer, NOT an error — and
        // the page walk's natural terminator.
        Some("no_data") => return Ok(Vec::new()),
        other => return Err(format!("deribit chart status {:?}", other.unwrap_or("<missing>"))),
    }

    let ticks = i64_column(result, "ticks")?;
    let open = f64_column(result, "open")?;
    let high = f64_column(result, "high")?;
    let low = f64_column(result, "low")?;
    let close = f64_column(result, "close")?;
    let volume = f64_column(result, "volume")?;
    // Ragged = malformed. Checked for EVERY column against `ticks` before a single Bar is built:
    // zip-truncating to the shortest would silently drop the tail and fabricate a gap in the tape.
    for (name, len) in [
        ("open", open.len()),
        ("high", high.len()),
        ("low", low.len()),
        ("close", close.len()),
        ("volume", volume.len()),
    ] {
        if len != ticks.len() {
            return Err(format!(
                "deribit chart: ragged columns (ticks={}, {name}={len})",
                ticks.len()
            ));
        }
    }

    Ok((0..ticks.len())
        .map(|i| kline_to_bar(ticks[i], open[i], high[i], low[i], close[i], volume[i]))
        .collect())
}

/// Pure map: one `public/get_tradingview_chart_data` response body → `Vec<Bar>`. No network — this
/// is the fixture-tested seam (`tests/fixtures/deribit_chart_btc_perpetual_1m.json`).
pub fn parse_deribit_klines(body: &str) -> Result<Vec<Bar>, String> {
    bars_from_envelope(&json_body(body, "deribit chart")?)
}

/// Build the chart-data GET URL. `start_timestamp` stays the REAL window start on every page (the
/// venue clamps it itself when the window exceeds its cap — that clamp is precisely what the
/// backward walk exists to undo); only `end_timestamp` moves.
fn chart_url(base: &str, symbol: &str, resolution: &str, start_ms: i64, end_ms: i64) -> String {
    format!(
        "{base}{PATH_CHART}?instrument_name={symbol}&start_timestamp={start_ms}&end_timestamp={end_ms}&resolution={resolution}"
    )
}

/// The business-level rate limit: Deribit can answer HTTP 200 with the `too_many_requests` envelope.
fn envelope_rate_limit(v: &Value) -> Option<String> {
    (envelope_error_code(v) == Some(RATE_LIMIT_CODE))
        .then(|| format!("code {RATE_LIMIT_CODE} (rate limited)"))
}

/// ONE chart page's facts for the shared JSON page fetch (`vike_bridge_core::retry`'s `json_page_fetch`):
/// HTTP 429/418 or the `too_many_requests` envelope code is a rate limit, retried on the history
/// pages' shared backoff (`RetryBackoffPolicy::HISTORY_PAGE`: honor `Retry-After`, else 1 s doubling to
/// 60 s, 6 retries of the same page). The backoff CADENCE is the shared driver; the CLASSIFICATION
/// (which statuses/codes mean rate-limited) stays Deribit-specific here, exactly as in okx's pager.
const CHART_PAGE: JsonPageSpec = JsonPageSpec {
    label: "deribit chart",
    json_what: "deribit chart",
    headers: &[],
    limited: &[429, 418],
    business_limit: envelope_rate_limit,
    backoff: RetryBackoffPolicy::HISTORY_PAGE,
};

/// Fetch ONE chart page on [`CHART_PAGE`]'s rate-limit policy.
fn fetch_page_rate_limited(
    agent: &ureq::Agent,
    base: &str,
    symbol: &str,
    resolution: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = chart_url(base, symbol, resolution, start_ms, end_ms);
    json_page_fetch(agent, &url, &CHART_PAGE, bars_from_envelope)
}

/// Fetch candle history for the inclusive `[start_ms, end_ms]` window, walking `end_timestamp`
/// BACKWARD through Deribit's end-anchored, silently-truncating cap. Returns bars ascending by ts,
/// de-duplicated. Network I/O; keyless (no credentials, no live gate). Errors on an unsupported
/// `interval` ([`resolution_code`]).
///
/// After `base`, the 4-arg shape is identical to `vike_binance::history::fetch_klines_range` /
/// `vike_okx::history::fetch_klines_range`, so a backfill seam binds it the same way. `symbol` reaches
/// the wire VERBATIM — Deribit instrument names (`BTC-PERPETUAL`) need no `.P` split.
///
/// `fetch_klines_range_paced` with nothing seeded and the measurement dropped, so this shape issues
/// the identical requests on the identical sleeps. `base` is the `live` `history` REST base.
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
/// backfill, on the `live` `history` row ([`KlineSourceDeribit::from_urls`]). The ingest it feeds is
/// `crates/vike-backfill/src/kline_source.rs`'s `backfill_kline_source`.
pub struct KlineSourceDeribit {
    base: String,
}

impl KlineSourceDeribit {
    /// The source on deribit's address rows `urls`: the public `history` REST base, mainnet's.
    ///
    /// # Errors
    /// The row's refusal.
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(Self {
            base: urls.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")?.to_string(),
        })
    }
}

impl vike_data::source::KlineSource for KlineSourceDeribit {
    fn venue(&self) -> &str {
        VENUE
    }

    /// The bridge's own paged `fetch_klines_range` verbatim — `symbol` is a Deribit instrument name
    /// (`BTC-PERPETUAL`) and is passed through untouched, dated futures and option names included. An
    /// interval [`resolution_code`] has no code for is refused (`Refused`: the venue is not asked).
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, vike_data::source::SourceError> {
        resolution_code(interval).map_err(vike_data::source::SourceError::Refused)?;
        fetch_klines_range(&self.base, symbol, interval, start_ms, end_ms)
            .map_err(vike_data::source::SourceError::Fetch)
    }
}

/// The pacer the pager runs on: the shared FALLBACK pacer over [`PAGE_DELAY`]
/// (`vike_bridge_core::klines`'s `kline_page_pacer`: Deribit publishes no budget to discover),
/// optionally seeded with a previous run's observation. This one line IS the venue's whole pacing
/// policy, and a test proves it never speeds the venue up — under any seed, after any observation —
/// without opening a socket.
fn page_pacer(seed: Option<&PaceSample>) -> Pacer {
    kline_page_pacer(VENUE, PAGE_DELAY, seed)
}

/// [`fetch_klines_range`] with the run's PACE measurement carried in and out: `seed` is a previous
/// run's observation (or `None`, which is byte-identical), and the second element of the return is
/// THIS run's — `None` unless a page was actually timed, so a run that fetched nothing cannot report
/// a fabricated pace.
///
/// ⚠ Neither direction moves a single sleep. A seed on a `Fixed` pacer buys exactly one thing: an
/// ETA from page ZERO instead of page one (`Pacer::next_delay` still answers [`PAGE_DELAY`] — see
/// this module's "The pager MEASURES its pages"). The reported sample's `per_request_weight` is
/// likewise inert here: Deribit publishes no weight counter, so the field carries the pacer's seed
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
    let resolution = resolution_code(interval)?;
    let agent = blocking_agent();
    // The shared paced walk: the throttle between pages (never before the first), the wall clock on
    // each (a rate-limited page's retry sleeps count as request time), and ONE pace line after the
    // first page with rows, its page count from the rows that page returned (this venue's cap is
    // undocumented and deliberately never hardcoded: module doc, point 3).
    kline_page_walk_backward(
        VENUE,
        symbol,
        interval,
        start_ms,
        end_ms,
        page_pacer(seed),
        |page_end| fetch_page_rate_limited(&agent, base, symbol, resolution, start_ms, page_end),
    )
}

// --- funding-rate source -------------------------------------------------------------------------

/// The keyless public funding-rate history endpoint's path, below the `live` `data` rest base.
const PATH_FUNDING: &str = "/api/v2/public/get_funding_rate_history";

/// How long one published funding record accrues over: an HOUR. MEASURED 2026-11-08 against
/// `BTC-PERPETUAL`: records are one hour apart, each on the hour, and each row's `prev_index_price` is
/// the previous row's `index_price`, so a row is the hour that ENDS at its `timestamp`.
pub const FUNDING_RECORD_MS: i64 = 3_600_000;

/// Pull ascending-by-ts funding points out of a parsed `public/get_funding_rate_history` envelope:
/// the JSON-RPC `error` envelope is an `Err`, else `result` is an array of `{timestamp,
/// interest_1h, interest_8h, index_price, prev_index_price}` (numbers). The point is the hour ending
/// at `timestamp`: its rate is `interest_1h`, the funding the venue ACCRUED over that hour (not
/// `interest_8h`, an 8h-equivalent of a rolling rate that no position is charged as such), and its
/// mark is `index_price`, the index the venue converts the payment at. A row missing either number
/// is skipped. No premium.
fn funding_from_envelope(v: &Value) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    if let Some(e) = v.get("error") {
        let code = e.get("code").and_then(Value::as_i64).unwrap_or(0);
        let msg = e.get("message").and_then(Value::as_str).unwrap_or("");
        let reason =
            e.get("data").and_then(|d| d.get("reason")).and_then(Value::as_str).unwrap_or("");
        // An empty reason is left out, as `envelope_error` leaves it out.
        return Err(if reason.is_empty() {
            format!("deribit funding error {code}: {msg}")
        } else {
            format!("deribit funding error {code}: {msg} ({reason})")
        });
    }
    let rows =
        v.get("result").and_then(Value::as_array).ok_or("deribit funding: missing result array")?;
    let mut points: Vec<vike_marketdata::FundingRatePoint> = rows
        .iter()
        .filter_map(|row| {
            let ts_ms = row.get("timestamp")?.as_i64()?;
            let rate = row.get("interest_1h")?.as_f64()?;
            let mark = vike_data::source::positive_price(row.get("index_price"));
            Some(vike_marketdata::FundingRatePoint {
                timestamp_ms: ts_ms,
                rate,
                premium: None,
                mark,
            })
        })
        .collect();
    points.sort_by_key(|p| p.timestamp_ms);
    Ok(points)
}

/// Pure map: a `get_funding_rate_history` body → funding points ascending by ts. No network — the
/// fixture-tested seam.
pub fn parse_deribit_funding(body: &str) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    funding_from_envelope(&json_body(body, "deribit funding")?)
}

/// One funding page ending at `end_ms`. ⚠ The endpoint answers rows with `start < timestamp <= end`
/// (MEASURED 2026-11-08: `start_timestamp` equal to a row's own timestamp leaves that row out), so
/// the request asks from `start_ms - 1` and the walk keeps `[start_ms, end_ms]`.
fn fetch_funding_page(
    agent: &ureq::Agent,
    base: &str,
    instrument: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<vike_marketdata::FundingRatePoint>, String> {
    let url = format!(
        "{base}{PATH_FUNDING}?instrument_name={instrument}&start_timestamp={}&end_timestamp={end_ms}",
        start_ms.saturating_sub(1)
    );
    json_page_fetch(agent, &url, &FUNDING_PAGE, funding_from_envelope)
}

/// ONE funding page's facts for the shared JSON page fetch: the chart page's rate-limit rules
/// ([`CHART_PAGE`]) under the funding endpoint's words.
const FUNDING_PAGE: JsonPageSpec =
    JsonPageSpec { label: "deribit funding", json_what: "deribit funding", ..CHART_PAGE };

/// This venue's market funding-rate source, a row of the gatewayhub's `FUNDING_SOURCES`: a perpetual
/// (`BTC-PERPETUAL` inverse, `ETH_USDC-PERPETUAL` linear), named verbatim like the kline lane names it.
/// A dated future or an option has no funding (the venue answers `instrument is not perpetual`) and is
/// REFUSED before anything is asked.
///
/// ⚠ **Deribit's funding ACCRUES CONTINUOUSLY**, every millisecond a position is open, and is paid out
/// at the daily settlement; there is no funding instant. What the venue publishes is the rate accrued
/// per HOUR ([`FUNDING_RECORD_MS`]): each stored point is the hour ENDING at its timestamp, its rate
/// `interest_1h`, its mark `index_price`. **A backtest must spread each point over its hour**, so a
/// position pays the share of the hour it was held, valued at the index: that is
/// `crates/vike-backtest/src/harness/run/funding.rs`'s `FundingAccrual` (its `Continuous` arm),
/// never the settlement rule the other venues take (which charges the whole rate to whatever is
/// held at the timestamp).
///
/// ⚠ **The endpoint is END-ANCHORED.** MEASURED 2026-11-08: a window of 31 days answers its 744 hours;
/// any wider window answers the NEWEST 744 rows ending at `end_timestamp`, `start_timestamp` ignored. So
/// it pages BACKWARD (`vike_data::source::page_walk_backward`).
pub struct FundingRateSourceDeribit {
    base: String,
}

impl FundingRateSourceDeribit {
    /// The source on deribit's address rows `urls`: the public `data` REST base (the public RATE is
    /// market data), mainnet's.
    ///
    /// # Errors
    /// The row's refusal.
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(Self {
            base: urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string(),
        })
    }
}

impl vike_data::source::FundingRateSource for FundingRateSourceDeribit {
    fn venue(&self) -> &str {
        VENUE
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<vike_marketdata::FundingRatePoint>, vike_data::source::SourceError> {
        use vike_data::source::SourceError;
        if !symbol.ends_with("-PERPETUAL") {
            return Err(SourceError::Refused(format!(
                "deribit: {symbol:?} is not a perpetual, and funding is a perpetual's series — a \
                 perpetual's name ends in `-PERPETUAL` (`BTC-PERPETUAL`, `ETH_USDC-PERPETUAL`)"
            )));
        }
        let agent = blocking_agent();
        let mut first = true;
        let stuck = |at: vike_data::source::PageWalkStuck| {
            SourceError::Fetch(at.text(VENUE, symbol, vike_data::source::FUNDING_INTERVAL))
        };
        vike_data::source::page_walk_backward(start_ms, end_ms, stuck, |page_end| {
            if !first {
                std::thread::sleep(PAGE_DELAY);
            }
            first = false;
            fetch_funding_page(&agent, &self.base, symbol, start_ms, page_end)
                .map_err(SourceError::Fetch)
        })
    }
}

#[cfg(test)]
mod tests {
    //! URL shaping + the interval map. The parse contract (by-KEY columns, `no_data`, ragged) and
    //! the BACKWARD walk are gated from the fixture-carrying integration test
    //! (`tests/offline/deribit_klines.rs`), against this module's public seams.
    use super::*;
    use vike_bridge_core::pacer::remaining_pages;

    /// Every arm, verbatim.
    #[test]
    fn resolution_code_maps_full_minutes_and_the_daily_code() {
        for (interval, code) in [
            ("1m", "1"),
            ("3m", "3"),
            ("5m", "5"),
            ("10m", "10"),
            ("15m", "15"),
            ("30m", "30"),
            ("1h", "60"),
            ("2h", "120"),
            ("3h", "180"),
            ("6h", "360"),
            ("12h", "720"),
            ("1d", "1D"),
        ] {
            assert_eq!(resolution_code(interval), Ok(code), "{interval}");
        }
    }

    /// The gaps are deliberate: Deribit's enum has no 4h, no weekly, no monthly. Erroring beats
    /// coercing to a neighbouring bucket and storing mislabelled bars.
    #[test]
    fn resolution_code_errors_on_intervals_deribit_does_not_serve() {
        for bad in ["4h", "1w", "1M", "7s", "", "1min", "1s", "3d", "1H", "1D"] {
            let want = format!("deribit: unsupported interval {bad:?}");
            assert_eq!(resolution_code(bad), Err(want), "{bad:?} must not map");
        }
    }

    /// An interval deribit has no resolution for is `Refused` (the venue was not asked), never a
    /// `Fetch`, with the table's own words.
    #[test]
    fn an_unsupported_interval_is_refused_before_the_venue_is_asked() {
        use vike_data::source::{KlineSource, SourceError};
        let klines = KlineSourceDeribit::from_urls(&VenueUrls::seed(VENUE)).expect("seeded");
        let err = klines.fetch("BTC-PERPETUAL", "4h", 0, 1).unwrap_err();
        assert_eq!(err, SourceError::Refused("deribit: unsupported interval \"4h\"".into()));
    }

    /// The row count a full `resolution=1` page returns, MEASURED live (module doc's table) — used
    /// here only as the ETA's divisor in a test. The pager itself reads the page it actually got,
    /// which is why this number is nowhere in `src` outside this test module.
    const OBSERVED_PAGE_ROWS: usize = 5_001;

    /// A previous run's record, shaped like the MEASURED sibling page (the CI box, 2026-08-04, okx:
    /// ~480 ms). `budget_per_min: None` because this venue publishes none — which is also what
    /// confines the record to another fallback pacer.
    fn deribit_record(request_ms: u64) -> PaceSample {
        PaceSample { request_ms, per_request_weight: 1.0, budget_per_min: None, samples: 20 }
    }

    /// ⚠ THE load-bearing test of this change: the pager MEASURES and the delay does not move.
    /// [`PAGE_DELAY`] is what the walk slept before the pacer existed, and it is bit-for-bit what
    /// `next_delay()` answers after — unseeded, seeded, and after observations at every scale.
    ///
    /// Asserted against the CONST rather than a literal `200`, so retuning the constant (a separate
    /// change, with its own measurement) moves this test with it instead of leaving it stale.
    #[test]
    fn the_pager_measures_but_never_changes_the_page_delay() {
        vike_bridge_core::policy_pins::assert_the_pager_never_changes_its_fixed_delay(
            page_pacer,
            PAGE_DELAY,
            &deribit_record(480),
            "deribit",
        );
    }

    /// What the measurement BUYS, on the run the bin's own doc warns is "a LOT of requests": 24
    /// months of 1m candles is 210 of this venue's ~5001-row pages, whose real cost is ~100s of
    /// request time on top of 42s of sleep — a number the 200ms constant alone could never have
    /// produced, and the one the pager's ETA line now prints after page one.
    #[test]
    fn a_measured_page_yields_the_eta_the_fixed_delay_could_not() {
        let mut p = page_pacer(None);
        p.observe_request(None, Duration::from_millis(480));
        let two_years = 730 * 86_400_000i64;
        let pages = remaining_pages(two_years, "1m", OBSERVED_PAGE_ROWS).expect("1m parses");
        assert_eq!(pages, 210, "24 months of 1m candles at ~5001 rows/page");
        let eta = p.eta(pages).expect("a timed page yields an ETA");
        // 210 x (480ms + 200ms) = 142.8s. The sleep-only view would have claimed 42s.
        assert!(
            eta > Duration::from_secs(140) && eta < Duration::from_secs(146),
            "expected ~2m23s for the measured shape, got {eta:?}"
        );
        // A SEEDED pacer answers the same before page one has even been fetched.
        assert_eq!(page_pacer(Some(&deribit_record(480))).eta(pages), Some(eta));
        // ...and this run, having measured, hands its own observation back out to be persisted.
        let m = p.measured().expect("a fixed pager still times its pages");
        assert_eq!(m.request_ms, 480);
        assert_eq!(m.budget_per_min, None, "nothing discovered ⇒ nothing claimed");
        assert!(m.is_usable());
    }

    /// The release's `history` row for the public backfill (`VENUE_URL_SEED`).
    fn seed_history() -> &'static str {
        static SEED: std::sync::OnceLock<VenueUrls> = std::sync::OnceLock::new();
        SEED.get_or_init(|| VenueUrls::seed(VENUE))
            .url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")
            .expect("the seed holds deribit's live rest row")
    }

    /// On the seed's row the URL is, byte for byte, the one dialled before the address moved into rows.
    #[test]
    fn chart_url_carries_every_query_param() {
        assert_eq!(
            chart_url(seed_history(), "BTC-PERPETUAL", "1", 1_785_840_000_000, 1_785_840_120_000),
            "https://www.deribit.com/api/v2/public/get_tradingview_chart_data?instrument_name=BTC-PERPETUAL&start_timestamp=1785840000000&end_timestamp=1785840120000&resolution=1"
        );
    }

    /// Both public sources read the live row: the funding RATE on `data`, the backfill on `history`,
    /// one host in the seed whatever the asking tier (a demo account's data is mainnet's).
    #[test]
    fn the_public_sources_read_the_live_rows() {
        let seed = VenueUrls::seed(VENUE);
        assert_eq!(KlineSourceDeribit::from_urls(&seed).unwrap().base, "https://www.deribit.com");
        assert_eq!(
            FundingRateSourceDeribit::from_urls(&seed).unwrap().base,
            "https://www.deribit.com"
        );
        let none = VenueUrls::from_rows(VENUE, std::iter::empty());
        assert!(
            KlineSourceDeribit::from_urls(&none).is_err()
                && FundingRateSourceDeribit::from_urls(&none).is_err()
        );
    }

    /// The symbol reaches the wire verbatim — no `.P` split on this venue (its names are already
    /// unambiguous, which is why `vike_catalog::uses_perp_suffix("deribit")` is false).
    #[test]
    fn chart_url_passes_the_symbol_through_verbatim() {
        let url = chart_url(seed_history(), "ETH-PERPETUAL", "60", 1, 2);
        assert!(url.contains("instrument_name=ETH-PERPETUAL&"), "{url}");
        assert!(!vike_catalog::uses_perp_suffix(VENUE), "deribit takes no .P suffix");
    }

    /// The committed venue answer (`BTC-PERPETUAL`, five hours of 2026-11-01): one point per hour
    /// ending at its timestamp, the rate `interest_1h` and the mark `index_price`, bit for bit; each
    /// row's `prev_index_price` is the row before's index, which is what makes a row an HOUR.
    #[test]
    fn the_funding_fixture_is_the_hourly_accrual_at_the_index() {
        let body = include_str!("../tests/fixtures/deribit_funding_btc_perpetual.json");
        let got = parse_deribit_funding(body).unwrap();
        assert_eq!(got.len(), 5);
        assert!(got.windows(2).all(|w| w[1].timestamp_ms - w[0].timestamp_ms == FUNDING_RECORD_MS));
        assert_eq!(got[1].timestamp_ms, 1_793_667_600_000);
        assert_eq!(got[1].rate.to_bits(), 5.19581312253964e-8_f64.to_bits(), "interest_1h, not 8h");
        assert_eq!(got[1].mark.map(f64::to_bits), Some(77_013.63_f64.to_bits()), "index_price");
        assert!(got.iter().all(|p| p.premium.is_none()));
        let v: Value = serde_json::from_str(body).unwrap();
        let rows = v["result"].as_array().unwrap();
        for w in rows.windows(2) {
            assert_eq!(
                w[1]["prev_index_price"], w[0]["index_price"],
                "a row is the hour before it"
            );
        }
    }

    #[test]
    fn a_funding_error_envelope_is_an_error_naming_the_reason() {
        let err = parse_deribit_funding(
            r#"{"jsonrpc":"2.0","error":{"code":-32602,"data":{"reason":"instrument is not perpetual","param":"instrument_name"},"message":"Invalid params"}}"#,
        )
        .unwrap_err();
        assert!(err.contains("-32602") && err.contains("instrument is not perpetual"), "{err}");
        assert!(parse_deribit_funding(r#"{"jsonrpc":"2.0","result":[]}"#).unwrap().is_empty());
    }

    /// The whole sentence, with a reason and without one: an empty reason leaves out the "()".
    #[test]
    fn a_funding_error_without_a_reason_prints_no_empty_brackets() {
        let with = r#"{"jsonrpc":"2.0","error":{"code":10028,"message":"too_many_requests","data":{"reason":"slow down"}}}"#;
        let without = r#"{"jsonrpc":"2.0","error":{"code":10028,"message":"too_many_requests"}}"#;
        assert_eq!(
            parse_deribit_funding(with).unwrap_err(),
            "deribit funding error 10028: too_many_requests (slow down)"
        );
        assert_eq!(
            parse_deribit_funding(without).unwrap_err(),
            "deribit funding error 10028: too_many_requests"
        );
    }

    /// A dated future or an option is refused BEFORE any request: only a perpetual has funding.
    #[test]
    fn a_non_perpetual_is_refused_before_the_venue_is_asked() {
        use vike_data::source::{FundingRateSource, SourceError};
        for symbol in ["BTC-27NOV26", "BTC-5OCT26-74000-C", "BTCUSDT.P"] {
            let err = FundingRateSourceDeribit::from_urls(&VenueUrls::seed(VENUE))
                .unwrap()
                .fetch(symbol, 0, 1)
                .unwrap_err();
            assert!(
                matches!(&err, SourceError::Refused(t) if t.contains("-PERPETUAL")),
                "{symbol}: {err:?}"
            );
        }
    }
}
