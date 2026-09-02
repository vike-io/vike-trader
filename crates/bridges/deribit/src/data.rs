//! Deribit candle (OHLCV) history — the venue's keyless PUBLIC bar-history read, and the fourth
//! `data.rs` in the bridge family (binance/aster ride the shared `family::klines` rung, okx has its
//! own, this is deribit's). The Deribit twin of `vike_okx::data`: a pure, fixture-tested parse
//! (`parse_deribit_klines`) plus a paged, rate-limited [`fetch_klines_range`] with the SAME 4-arg
//! shape every backfill seam expects (`symbol, interval, start_ms, end_ms`).
//!
//! Endpoint (keyless, no auth, no signer — like [`crate::catalog`]/[`crate::chain`]):
//! `GET https://www.deribit.com/api/v2/public/get_tradingview_chart_data
//!      ?instrument_name=&start_timestamp=&end_timestamp=&resolution=`
//!
//! `symbol` passes through VERBATIM (`BTC-PERPETUAL`). Deribit needs NO
//! [`vike_catalog::PERP_SUFFIX`] handling — its instrument names are already unambiguous
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
//! The pure seams are deliberately `pub` so the truncation trap is gated WITHOUT network I/O:
//! [`next_page_step`] is the "given this page's ticks, what is the next `end_timestamp`" decision
//! (the extraction `vike_aster::data`'s `range_target` made for the perp-host decision), and
//! [`walk_backward_pages`] drives the whole walk over an INJECTED page fetcher — a test that only
//! exercises a single page cannot see the silent-truncation bug at all.
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
//! wall-clocked into [`vike_bridge_core::pacer::Pacer::observe_request`]. Two things follow, and a
//! third deliberately does not:
//!   * the backfill prints ONE `info` ETA line after its first page — and this is the venue that
//!     needed it most, since a 24-month 1m pull here is the run whose silence the bin's own doc
//!     warns about;
//!   * [`fetch_klines_range_paced`] hands the observation back so `vike_backfill::pace_book` can
//!     persist it and an operator can SEE what a page costs here;
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

use vike_bridge_core::http::{blocking_agent, body_head, get_raw};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::pacer::{remaining_pages, Pacer};
use vike_bridge_core::retry::{retry_rate_limited, BackoffPolicy, Verdict};
use vike_model::rate_limits::PaceSample;
use vike_model::Bar;

/// Canonical venue id — the partition `venue` a backfill seam keys this history on.
pub const VENUE: &str = "deribit";

/// The keyless public chart-data endpoint (no auth, no signer, no credentials).
const REST_CHART: &str = "https://www.deribit.com/api/v2/public/get_tradingview_chart_data";

// ---- paged-backfill rate-limit knobs -----------------------------------------------------------
/// Fixed throttle between successive page requests. Deribit meters public REST out of the
/// NON-matching-engine credit pool (~20 req/s sustained — see [`crate::ratelimit`]); 200 ms keeps a
/// long backfill an order of magnitude under it while still pulling ~5000 bars/page.
///
/// The VALUE comes from `vike_model::venue_rate_limits`'s `DERIBIT` row rather than from a literal
/// here: Deribit runs no request-weight meter and publishes no machine-readable rate-limit metadata,
/// so this delay IS the whole pacing story for history on this venue. The table models that absence
/// as `History::Unweighted` (no ceiling, no soft limit) instead of inventing a number; the ~20 req/s
/// credit pool above is documented prose, never a payload.
const PAGE_DELAY: Duration = vike_model::venue_rate_limits::DERIBIT.history.page_delay();
/// Bounded retries for a single page that keeps getting rate-limited.
const MAX_RATE_LIMIT_RETRIES: u32 = 6;
/// First backoff when the server sends no `Retry-After` (doubles each retry, capped).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling on any single backoff / `Retry-After` sleep.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
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
pub fn resolution_code(interval: &str) -> Result<&'static str, String> {
    Ok(match interval {
        "1m" => "1",
        "3m" => "3",
        "5m" => "5",
        "10m" => "10",
        "15m" => "15",
        "30m" => "30",
        "1h" => "60",
        "2h" => "120",
        "3h" => "180",
        "6h" => "360",
        "12h" => "720",
        "1d" => "1D",
        other => return Err(format!("deribit: unsupported interval {other:?}")),
    })
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
    let v: Value = serde_json::from_str(body).map_err(|e| format!("deribit chart json: {e}"))?;
    bars_from_envelope(&v)
}

/// What the backward page walk does after one page — the pure decision [`next_page_step`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStep {
    /// Request the next (older) page with this `end_timestamp`.
    Next(i64),
    /// The walk is complete: the window start was reached, the page was empty, or the cursor
    /// failed to move backward.
    Done,
}

/// Pure: given the ticks a page returned, the window `start_ms`, and the `end_timestamp`
/// (`page_end_ms`) that produced it, decide the next `end_timestamp` — or stop.
///
/// This is the extraction the module doc argues for (the twin of `vike_aster::data`'s
/// `range_target`): the ONE decision that makes the end-anchored truncation survivable, hoisted out
/// of the network path so it can be gated by a test. Three ways to be [`PageStep::Done`]:
///
/// - the page was empty (history ran out, or `status:"no_data"`);
/// - its oldest tick already reached `start_ms` (the window is fully covered);
/// - the next end would NOT be strictly older than the one that produced this page — a cursor that
///   cannot move backward would otherwise spin forever (reachable only if the venue answers with
///   ticks NEWER than the `end_timestamp` asked for).
///
/// Uses the page's MINIMUM tick rather than its first element, so wire order is not assumed.
pub fn next_page_step(page_ticks: &[i64], start_ms: i64, page_end_ms: i64) -> PageStep {
    let Some(&oldest) = page_ticks.iter().min() else {
        return PageStep::Done; // empty page — nothing older to walk toward
    };
    if oldest <= start_ms {
        return PageStep::Done; // the window start is covered
    }
    let next_end = oldest.saturating_sub(1);
    if next_end >= page_end_ms {
        return PageStep::Done; // the cursor must move strictly backward
    }
    PageStep::Next(next_end)
}

/// The BACKWARD page walk itself, over an INJECTED page fetcher — pure, and the reason the
/// truncation trap is testable without network I/O.
///
/// `fetch_page(end_timestamp)` returns one page's bars (any order). The walk seeds
/// `end_timestamp = end_ms`, folds each page through [`next_page_step`], keeps only bars inside the
/// inclusive `[start_ms, end_ms]` window, and finally sorts ascending + dedups by `ts` (pages
/// arrive newest-block first, and a boundary bar can repeat if the venue's bucketing ever
/// overlaps).
///
/// It never sleeps: the inter-page throttle belongs to the caller's fetcher closure (see
/// [`fetch_klines_range`]), so a test walks many pages instantly.
pub fn walk_backward_pages<F>(
    start_ms: i64,
    end_ms: i64,
    mut fetch_page: F,
) -> Result<Vec<Bar>, String>
where
    F: FnMut(i64) -> Result<Vec<Bar>, String>,
{
    let mut out: Vec<Bar> = Vec::new();
    if end_ms < start_ms {
        return Ok(out); // empty window: never touch the venue
    }
    let mut page_end = end_ms;
    loop {
        let page = fetch_page(page_end)?;
        if page.is_empty() {
            break;
        }
        let ticks: Vec<i64> = page.iter().map(|b| b.ts).collect();
        for b in page {
            if start_ms <= b.ts && b.ts <= end_ms {
                out.push(b);
            }
        }
        match next_page_step(&ticks, start_ms, page_end) {
            PageStep::Next(next_end) => page_end = next_end,
            PageStep::Done => break,
        }
    }
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok(out)
}

/// Build the chart-data GET URL. `start_timestamp` stays the REAL window start on every page (the
/// venue clamps it itself when the window exceeds its cap — that clamp is precisely what the
/// backward walk exists to undo); only `end_timestamp` moves.
fn chart_url(symbol: &str, resolution: &str, start_ms: i64, end_ms: i64) -> String {
    format!(
        "{REST_CHART}?instrument_name={symbol}&start_timestamp={start_ms}&end_timestamp={end_ms}&resolution={resolution}"
    )
}

/// Fetch ONE page with the rate-limit policy: on HTTP 429/418 or the `too_many_requests` envelope
/// code, honor `Retry-After` (else exponential backoff) and retry the same page up to
/// [`MAX_RATE_LIMIT_RETRIES`]. The backoff CADENCE is the shared driver; the CLASSIFICATION (which
/// statuses/codes mean rate-limited) stays Deribit-specific here, exactly as in okx's pager.
fn fetch_page_rate_limited(
    agent: &ureq::Agent,
    symbol: &str,
    resolution: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = chart_url(symbol, resolution, start_ms, end_ms);
    let policy = BackoffPolicy {
        max_retries: MAX_RATE_LIMIT_RETRIES,
        initial: INITIAL_BACKOFF,
        max: MAX_BACKOFF,
    };
    retry_rate_limited(policy, "deribit chart", || {
        let raw = get_raw(agent, &url, "deribit chart", &[])?;
        if matches!(raw.status, 429 | 418) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("HTTP {} (rate limited)", raw.status),
            });
        }
        if !(200..300).contains(&raw.status) {
            return Err(format!("deribit chart HTTP {}: {}", raw.status, body_head(&raw.body)));
        }
        let v: Value =
            serde_json::from_str(&raw.body).map_err(|e| format!("deribit chart json: {e}"))?;
        // Business-level rate limit (Deribit can answer HTTP 200 with the error envelope).
        if envelope_error_code(&v) == Some(RATE_LIMIT_CODE) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("code {RATE_LIMIT_CODE} (rate limited)"),
            });
        }
        bars_from_envelope(&v).map(Verdict::Done)
    })
}

/// Fetch candle history for the inclusive `[start_ms, end_ms]` window, walking `end_timestamp`
/// BACKWARD through Deribit's end-anchored, silently-truncating cap. Returns bars ascending by ts,
/// de-duplicated. Network I/O; keyless (no credentials, no live gate). Errors on an unsupported
/// `interval` ([`resolution_code`]).
///
/// The 4-arg shape is identical to `vike_binance::data::fetch_klines_range` /
/// `vike_okx::data::fetch_klines_range`, so a backfill seam binds it the same way. `symbol` reaches
/// the wire VERBATIM — Deribit instrument names (`BTC-PERPETUAL`) need no `.P` split.
///
/// [`fetch_klines_range_paced`] with nothing seeded and the measurement dropped, so this 4-arg shape
/// issues the identical requests on the identical sleeps.
pub fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_paced(symbol, interval, start_ms, end_ms, None).map(|(bars, _pace)| bars)
}

/// The pacer the pager runs on: a FALLBACK pacer over [`PAGE_DELAY`] (Deribit publishes no budget to
/// discover), optionally seeded with a previous run's observation.
///
/// Hoisted out of [`fetch_klines_range_paced`] for exactly the reason binance's `pacer_for` is: this
/// two-line function IS the venue's whole pacing policy, and a test must be able to prove it never
/// speeds the venue up — under any seed, after any observation — without opening a socket.
fn page_pacer(seed: Option<&PaceSample>) -> Pacer {
    let mut pacer = Pacer::fallback(PAGE_DELAY);
    if let Some(sample) = seed {
        let applied = pacer.seed(sample);
        tracing::debug!(
            target: "vike_deribit::data",
            applied,
            seed_request_ms = sample.request_ms,
            "deribit chart: persisted pace offered to the pacer (ETA only — the delay is fixed)"
        );
    }
    pacer
}

/// [`fetch_klines_range`] with the run's PACE measurement carried in and out: `seed` is a previous
/// run's observation (or `None`, which is byte-identical), and the second element of the return is
/// THIS run's — `None` unless a page was actually timed, so a run that fetched nothing cannot report
/// a fabricated pace.
///
/// ⚠ Neither direction moves a single sleep. A seed on a `Fixed` pacer buys exactly one thing: an
/// ETA from page ZERO instead of page one (`Pacer::next_delay` still answers [`PAGE_DELAY`] — see
/// this module's "The pager MEASURES its pages"). The persisted record's `per_request_weight` is
/// likewise inert here: Deribit publishes no weight counter, so the field carries the pacer's seed
/// constant and steers nothing — `request_ms` is the number an operator wants.
///
/// The CALLER owns the persistence (`vike_backfill::pace_book`); nothing in a bridge crate reads or
/// writes operator state.
pub fn fetch_klines_range_paced(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    seed: Option<&PaceSample>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let resolution = resolution_code(interval)?;
    let agent = blocking_agent();
    let mut pacer = page_pacer(seed);
    let mut first = true;
    // The pace report is emitted ONCE, after the first page that actually returned rows.
    let mut report_pace = true;
    let bars = walk_backward_pages(start_ms, end_ms, |page_end| {
        if !first {
            // Identical to the `PAGE_DELAY` this line used to sleep directly — `Pacer::fallback`
            // exists here to MEASURE, not to steer.
            std::thread::sleep(pacer.next_delay()); // throttle between pages, never before the first
        }
        first = false;
        // Wall-clock the request: it is the half of the real inter-request gap the pacer cannot see
        // for itself. It spans `fetch_page_rate_limited`, so a page that was rate-limited and
        // retried reports its retry sleeps as "request time" too — bounded and self-correcting (see
        // `Pacer::observe_request`), and un-separable without reaching inside the shared retry
        // driver.
        let started = std::time::Instant::now();
        let page = fetch_page_rate_limited(&agent, symbol, resolution, start_ms, page_end)?;
        let elapsed = started.elapsed();
        // Observe unconditionally: the time was spent whether or not rows came back.
        pacer.observe_request(None, elapsed);
        // ONE line per backfill, not per page — and the page count comes from the rows this page
        // actually returned, since this venue's cap is undocumented and deliberately never hardcoded
        // (module doc, point 3).
        if report_pace && !page.is_empty() {
            report_pace = false;
            let pages = remaining_pages(page_end.saturating_sub(start_ms), interval, page.len());
            if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
                tracing::info!(
                    target: "vike_deribit::data",
                    symbol,
                    interval,
                    request_ms = elapsed.as_millis() as u64,
                    page_rows = page.len() as u64,
                    page_delay_ms = pacer.next_delay().as_millis() as u64,
                    remaining_pages = pages,
                    eta_secs = eta.as_secs(),
                    "deribit candle backfill: measured page cost on the fixed page delay"
                );
            }
        }
        Ok(page)
    })?;
    Ok((bars, pacer.measured()))
}

#[cfg(test)]
mod tests {
    //! URL shaping + the interval map. The parse contract (by-KEY columns, `no_data`, ragged) and
    //! the BACKWARD walk are gated from the fixture-carrying integration test
    //! (`tests/offline/deribit_klines.rs`), against this module's public seams.
    use super::*;

    #[test]
    fn resolution_code_maps_full_minutes_and_the_daily_code() {
        assert_eq!(resolution_code("1m").unwrap(), "1");
        assert_eq!(resolution_code("5m").unwrap(), "5");
        assert_eq!(resolution_code("15m").unwrap(), "15");
        assert_eq!(resolution_code("1h").unwrap(), "60");
        assert_eq!(resolution_code("12h").unwrap(), "720");
        assert_eq!(resolution_code("1d").unwrap(), "1D");
    }

    /// The gaps are deliberate: Deribit's enum has no 4h, no weekly, no monthly. Erroring beats
    /// coercing to a neighbouring bucket and storing mislabelled bars.
    #[test]
    fn resolution_code_errors_on_intervals_deribit_does_not_serve() {
        for bad in ["4h", "1w", "1M", "7s", "", "1min"] {
            assert!(resolution_code(bad).is_err(), "{bad:?} must not map");
        }
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
        let mut p = page_pacer(None);
        assert!(!p.is_discovered(), "deribit publishes no budget to discover");
        assert_eq!(p.next_delay(), PAGE_DELAY);
        assert_eq!(p.eta(1_000), None, "nothing timed ⇒ no ETA, not one guessed from the sleep");

        for _ in 0..4 {
            p.observe_request(None, Duration::from_millis(480));
        }
        assert_eq!(p.next_delay(), PAGE_DELAY, "the measured 480ms must not move the sleep");
        assert!(!p.should_cool_down(), "and no budget ⇒ no target to cross");

        // A seed is likewise ETA-only: it can never make this pager faster.
        let mut seeded = page_pacer(Some(&deribit_record(480)));
        assert_eq!(seeded.next_delay(), PAGE_DELAY);
        seeded.observe_request(Some(9_999), Duration::from_secs(30));
        assert_eq!(seeded.next_delay(), PAGE_DELAY, "nor can any observation, at any scale");
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

    #[test]
    fn chart_url_carries_every_query_param() {
        assert_eq!(
            chart_url("BTC-PERPETUAL", "1", 1_785_840_000_000, 1_785_840_120_000),
            "https://www.deribit.com/api/v2/public/get_tradingview_chart_data?instrument_name=BTC-PERPETUAL&start_timestamp=1785840000000&end_timestamp=1785840120000&resolution=1"
        );
    }

    /// The symbol reaches the wire verbatim — no `.P` split on this venue (its names are already
    /// unambiguous, which is why `vike_catalog::uses_perp_suffix("deribit")` is false).
    #[test]
    fn chart_url_passes_the_symbol_through_verbatim() {
        let url = chart_url("ETH-PERPETUAL", "60", 1, 2);
        assert!(url.contains("instrument_name=ETH-PERPETUAL&"), "{url}");
        assert!(!vike_catalog::uses_perp_suffix(VENUE), "deribit takes no .P suffix");
    }
}
