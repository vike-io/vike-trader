//! Bybit V5 REST kline history (spot AND linear perp) — the ONE kline fetch + JSON→[`Bar`] mapper
//! for Bybit.
//!
//! The Bybit twin of binance's `data` module: a pure, fixture-tested `parse_bybit_klines` map and a
//! paged, rate-limited `fetch_klines_range`. Bybit differs from Binance in four ways this module
//! localises:
//!   1. **Envelope + ordering.** The body is the V5 `{retCode, retMsg, result:{list}}` envelope, and
//!      `result.list` is sorted **newest-first** as 7-element string arrays
//!      `[startTime, open, high, low, close, volume, turnover]`. `parse_bybit_klines` maps each row
//!      and **reverses** to ascending-by-startTime — the store's contract (and Binance's order).
//!   2. **Interval codes.** Bybit encodes intervals as bare minute counts / letter codes
//!      ("1m"→"1", "1h"→"60", "1d"→"D"); [`interval_code`] maps the binance-style input. The store
//!      key keeps the ORIGINAL interval string so series line up across venues.
//!   3. **Rate-limit signalling.** Bybit returns HTTP 200 for business errors, so a throttle trip
//!      shows up as `retCode` 10006 ("too many visits") / 10018 ("IP rate limit") in the body — plus
//!      HTTP 429/403 for a hard Cloudflare/IP block. Both are treated as retryable.
//!   4. **END-ANCHORED paging.** `/v5/market/kline` does NOT honor `start` once the window exceeds
//!      `limit`: it silently returns the NEWEST `limit` rows ending at `end` and answers
//!      `retCode: 0`, so the truncation looks like a healthy success. MEASURED live 2026-08-04
//!      against `category=linear&symbol=BTCUSDT&interval=1`: a 60-minute window (fits under the cap)
//!      came back as 60 rows whose oldest row IS the requested `start`; a 30-day window (43,200
//!      minutes) came back as exactly 1000 rows whose oldest row was ~16.6 h before `end` — the
//!      requested `start` ignored entirely. A binance-style FORWARD pager therefore self-terminates
//!      after ONE page: its first request comes back at the far END of the window, the cursor jumps
//!      past `end_ms`, and a 30-day backfill silently writes ~2% of the data.
//!
//!      The cure is deribit's/okx's pattern, not binance's — walk `end` DOWN
//!      (`end = oldest_returned_kline - 1`) until the window start is covered or a page comes back
//!      empty, then sort ascending and dedup. `start` stays the REAL window start on every page (the
//!      venue clamps it itself; that clamp is exactly what the backward walk undoes). Termination is
//!      driven by OBSERVED ticks — [`MAX_LIMIT`] is a request parameter, never a stop condition.
//!      `end = oldest - 1` is contiguous, not lossy (verified live: the page after one whose oldest
//!      row was `1785803400000`, requested with `end=1785803399999`, came back newest-first from
//!      `1785803340000` — exactly one minute older, no gap).
//!
//!      ONE accepted residual of that termination rule (documented, not silently swallowed —
//!      deribit's and okx's pagers have the identical shape): an EMPTY page stops the walk, so a
//!      hypothetical hole in the middle of a window would hide anything older than it. In practice an
//!      empty page means "older than this symbol's listing", which is exactly where the walk should
//!      stop.
//!
//!      The pure seams are `pub` so the truncation trap is gated WITHOUT network I/O:
//!      [`next_page_step`] is the "given this page's ticks, what is the next `end`" decision, and
//!      [`walk_backward_pages`] drives the whole walk over an INJECTED page fetcher — a test that
//!      only exercises a single page cannot see this bug at all.
//!
//! Endpoint: `GET https://api.bybit.com/v5/market/kline?category=&symbol=&interval=&start=&end=&limit=1000`
//! — `category` is `spot` for a bare symbol and `linear` for a `.P`-suffixed one (Bybit V5 unifies
//! spot/linear/inverse under ONE REST base, unlike Binance's separate fapi host).
//! Only `fetch_klines_range` does network I/O; `parse_bybit_klines` is a pure, fixture-tested map.
//!
//! ## Rate-limit strategy (paged `fetch_klines_range`)
//! Bybit's public REST is IP-limited (~600 req / 5 s per IP — generous), so the pager stays well
//! under it: a fixed [`PAGE_DELAY`] throttle between pages, and on a rate-limit signal (retCode
//! 10006/10018 or HTTP 429/403) an exponential backoff that HONORS `Retry-After`, retrying the same
//! page up to [`MAX_RATE_LIMIT_RETRIES`]. Pages BACKWARD (see point 4 above): `limit=1000` with
//! `start` pinned to the real window start, moving `end` one ms below each page's oldest kline.
//!
//! ## The pager MEASURES its pages (and still sleeps [`PAGE_DELAY`])
//! Bybit publishes no request-weight budget, so there is nothing to discover and nothing to pace a
//! fraction of — but that never stopped us timing our own round trips, and until now nobody had. The
//! sibling venue was MEASURED on the CI box 2026-08-04 (okx: 476–486 ms per page against a hardcoded
//! 200 ms delay) and this venue is the same shape: the constant is not even the dominant term of the
//! gap the venue actually sees.
//!
//! So the sleep now runs through [`vike_bridge_core::pacer::Pacer::fallback`] and every page is
//! wall-clocked into [`vike_bridge_core::pacer::Pacer::observe_request`]. Two things follow, and a
//! third deliberately does not:
//!   * the backfill prints ONE `info` ETA line after its first page, instead of going silent for
//!     minutes in a way indistinguishable from a hang;
//!   * [`fetch_klines_range_paced`] hands the observation back so `vike_backfill::pace_book` can
//!     persist it and an operator can SEE what a page costs here;
//!   * ⚠ **the delay does not change.** `Pacer::fallback::next_delay` returns exactly [`PAGE_DELAY`]
//!     whatever is observed (pinned by that module's tests and by
//!     `the_pager_measures_but_never_changes_the_page_delay` below). With no published ceiling, a
//!     delay derived from a stopwatch alone would be the same un-checkable hardcoded guess in newer
//!     clothes. Acting on the measurement is a separate change with its own measurement.

use std::time::Duration;

use vike_bridge_core::http::{body_head, get_raw};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::pacer::{Pacer, remaining_pages};
use vike_bridge_core::retry::{BackoffPolicy, Verdict, retry_rate_limited};
use vike_model::Bar;
use vike_model::rate_limits::PaceSample;

const REST_KLINES: &str = "https://api.bybit.com/v5/market/kline";
/// Bybit spot category — the default for a bare symbol; a `.P`-suffixed one routes to `"linear"`
/// through [`rest_category`].
const CATEGORY: &str = "spot";
/// Bybit's hard cap on klines returned by a single request.
const MAX_LIMIT: usize = 1000;

// ---- paged-backfill rate-limit knobs (see the module rate-limit strategy) ----------------------
/// Fixed throttle between successive page requests (keeps us far under ~600 req/5 s per IP).
///
/// The VALUE comes from `vike_model::venue_rate_limits`'s `BYBIT` row rather than from a literal
/// here: Bybit runs no request-weight meter and publishes no machine-readable rate-limit metadata,
/// so this delay IS the whole pacing story for history on this venue — which makes it a per-venue
/// fact worth declaring beside the other venues' rather than a local constant nothing can compare.
/// The table models that absence as `History::Unweighted` (no ceiling, no soft limit) instead of
/// inventing a number; the ~600 req/5 s figure above is documented prose, never a payload.
const PAGE_DELAY: Duration = vike_model::venue_rate_limits::BYBIT.history.page_delay();
/// Bounded retries for a single page that keeps getting rate-limited.
const MAX_RATE_LIMIT_RETRIES: u32 = 6;
/// First backoff when the server sends no `Retry-After` (doubles each retry, capped).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling on any single backoff / `Retry-After` sleep.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// Bybit rate-limit business codes (arrive with HTTP 200): "too many visits" / "IP rate limit".
const RATE_LIMIT_CODES: [i64; 2] = [10006, 10018];

/// Map a binance-style interval ("1m","1h","1d",…) to Bybit's V5 interval code ("1","60","D",…).
/// Errors on an unsupported interval rather than guessing. Note "1M" (month) vs "1m" (minute) is
/// case-sensitive, matching the binance-style input convention. `pub(crate)` so the live kline feed
/// ([`crate::market_feed`]) builds the `kline.<code>.<SYMBOL>` WS topic from the same map.
pub(crate) fn interval_code(interval: &str) -> Result<&'static str, String> {
    Ok(match interval {
        "1m" => "1",
        "3m" => "3",
        "5m" => "5",
        "15m" => "15",
        "30m" => "30",
        "1h" => "60",
        "2h" => "120",
        "4h" => "240",
        "6h" => "360",
        "12h" => "720",
        "1d" => "D",
        "1w" => "W",
        "1M" => "M",
        other => return Err(format!("bybit: unsupported interval {other:?}")),
    })
}

/// Map one Bybit kline row (`[startTime, open, high, low, close, volume, turnover]`, all decimal
/// strings) → a [`Bar`]. Preserves each string's exact f64 bit pattern via `<f64 as FromStr>` — no
/// rounding, no arithmetic. Volume defaults to 0.0 on a parse miss (mirrors the binance mapper).
fn row_to_bar(r: &[serde_json::Value]) -> Result<Bar, String> {
    let s = |i: usize, name: &str| -> Result<&str, String> {
        r.get(i)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("bybit kline {name} not str"))
    };
    let t = s(0, "startTime")?
        .parse::<i64>()
        .map_err(|e| format!("bybit kline startTime parse: {e}"))?;
    let f = |i: usize, name: &str| -> Result<f64, String> {
        s(i, name)?.parse::<f64>().map_err(|e| format!("bybit kline {name} parse: {e}"))
    };
    let o = f(1, "open")?;
    let h = f(2, "high")?;
    let l = f(3, "low")?;
    let c = f(4, "close")?;
    let v = r
        .get(5)
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(kline_to_bar(t, o, h, l, c, v))
}

/// Pull ascending-by-startTime [`Bar`]s out of a parsed V5 kline envelope: require `retCode == 0`,
/// map `result.list` (newest-first) and reverse. Shared by the pure map and the pager.
fn bars_from_envelope(v: &serde_json::Value) -> Result<Vec<Bar>, String> {
    let ret_code = v.get("retCode").and_then(serde_json::Value::as_i64);
    if ret_code != Some(0) {
        let msg = v.get("retMsg").and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("bybit klines retCode {ret_code:?}: {msg}"));
    }
    let list = v
        .get("result")
        .and_then(|r| r.get("list"))
        .and_then(serde_json::Value::as_array)
        .ok_or("bybit klines: missing result.list")?;
    let mut bars = list
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| "bybit kline row not array".to_string())
                .and_then(|r| row_to_bar(r))
        })
        .collect::<Result<Vec<Bar>, String>>()?;
    bars.reverse(); // Bybit serves newest-first; the store wants ascending by startTime
    Ok(bars)
}

/// Pure map: a Bybit V5 klines response body → `Vec<Bar>` ascending by startTime. No network — this
/// is the fixture-tested seam.
pub fn parse_bybit_klines(body: &str) -> Result<Vec<Bar>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("bybit kline json: {e}"))?;
    bars_from_envelope(&v)
}

/// Build the `GET /v5/market/kline` URL for the given bounds and limit. `category` is `"spot"` or
/// `"linear"` — a PARAMETER, not the const, so a perp backfill and the spot one cannot drift.
fn klines_url(
    category: &str,
    symbol: &str,
    interval_code: &str,
    start_ms: i64,
    end_ms: i64,
    limit: usize,
) -> String {
    format!(
        "{REST_KLINES}?category={category}&symbol={symbol}&interval={interval_code}&start={start_ms}&end={end_ms}&limit={limit}"
    )
}

/// Fetch ONE page with the rate-limit policy: on HTTP 429/418/403 or retCode 10006/10018, honor
/// `Retry-After` (else exponential backoff) and retry the same page up to
/// [`MAX_RATE_LIMIT_RETRIES`]. Returns the page's bars ascending by startTime.
fn fetch_page_rate_limited(
    agent: &ureq::Agent,
    category: &str,
    symbol: &str,
    interval_code: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = klines_url(category, symbol, interval_code, start_ms, end_ms, MAX_LIMIT);
    let policy = BackoffPolicy {
        max_retries: MAX_RATE_LIMIT_RETRIES,
        initial: INITIAL_BACKOFF,
        max: MAX_BACKOFF,
    };
    // The backoff cadence is the shared driver; the classification (which statuses/retCodes mean
    // rate-limited) stays Bybit-specific here.
    retry_rate_limited(policy, "bybit klines", || {
        let raw = get_raw(agent, &url, "bybit klines", &[])?;
        // HTTP-level rate limit / IP block (Bybit fronts REST with Cloudflare).
        if matches!(raw.status, 429 | 418 | 403) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("HTTP {} (rate limited)", raw.status),
            });
        }
        if !(200..300).contains(&raw.status) {
            return Err(format!("bybit klines HTTP {}: {}", raw.status, body_head(&raw.body)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&raw.body).map_err(|e| format!("bybit kline json: {e}"))?;
        // Business-level rate limit (Bybit returns HTTP 200 for these).
        let code = v.get("retCode").and_then(serde_json::Value::as_i64).unwrap_or(-1);
        if RATE_LIMIT_CODES.contains(&code) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("retCode {code} (rate limited)"),
            });
        }
        bars_from_envelope(&v).map(Verdict::Done)
    })
}

/// Spot vs linear-perp category for both REST kline calls ([`fetch_klines_latest`]'s warmup seed and
/// [`fetch_klines_range`]'s pager) — the ONE place that decides spot-vs-linear so the URL-building
/// tests below and the real fetches never drift. Mirrors binance's `rest_klines_base(perp)`.
fn rest_category(perp: bool) -> &'static str {
    if perp { "linear" } else { CATEGORY }
}

/// Build the `GET /v5/market/kline` URL for the live feed's latest-N warmup (no start/end bounds).
/// `category` is `"spot"` or `"linear"` — pulled out so the URL-building test below and the real
/// fetch can never drift apart.
fn latest_url(category: &str, symbol: &str, interval_code: &str, limit: usize) -> String {
    format!(
        "{REST_KLINES}?category={category}&symbol={symbol}&interval={interval_code}&limit={limit}"
    )
}

/// The live feed's warmup seed: the newest `limit` klines (no time bound), the last of which is the
/// still-forming candle — Bybit serves the in-progress candle as the newest `result.list` row, so
/// after [`parse_bybit_klines`] reverses to ascending it is the last bar. A single un-throttled
/// request — the Bybit twin of binance's `fetch_klines_latest`. `perp = true` routes to the
/// `category=linear` klines (the same endpoint host, just a different query category — Bybit V5
/// unifies spot/linear/inverse under one REST base, unlike Binance's separate fapi host); the
/// response shape is identical either way, so `parse_bybit_klines` is unchanged. Network I/O. Errors
/// on an unsupported `interval`.
pub fn fetch_klines_latest(
    symbol: &str,
    interval: &str,
    limit: usize,
    perp: bool,
) -> Result<Vec<Bar>, String> {
    let code = interval_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let url = latest_url(rest_category(perp), symbol, code, limit);
    let raw = get_raw(&agent, &url, "bybit klines", &[])?;
    if !(200..300).contains(&raw.status) {
        return Err(format!("bybit klines HTTP {}: {}", raw.status, body_head(&raw.body)));
    }
    parse_bybit_klines(&raw.body)
}

/// The `(category, wire_symbol)` pair a range fetch routes to — extracted out of
/// [`fetch_klines_range`] so the routing DECISION is assertable without network I/O.
///
/// Testing [`rest_category`] directly proves nothing about this: that helper has understood `perp`
/// since the live warmup seed was written, and the bug was that `fetch_klines_range` never asked
/// it. This function is what the fetcher actually calls, so a test against it fails if the fetcher
/// is ever re-pinned to `CATEGORY` — and if the fetcher stops calling it, `dead_code` trips the
/// `-D warnings` merge gate.
fn range_target(symbol: &str) -> (&'static str, &str) {
    let (wire, perp) = vike_catalog::split_perp(symbol);
    (rest_category(perp), wire)
}

/// What the backward page walk does after one page — the pure decision [`next_page_step`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageStep {
    /// Request the next (older) page with this `end`.
    Next(i64),
    /// The walk is complete: the window start was reached, the page was empty, or the cursor failed
    /// to move backward.
    Done,
}

/// Pure: given the startTimes a page returned, the window `start_ms`, and the `end` (`page_end_ms`)
/// that produced it, decide the next `end` — or stop.
///
/// This is the ONE decision that makes Bybit's end-anchored truncation survivable, hoisted out of
/// the network path so it can be gated by a test (the twin of deribit's `next_page_step` and of the
/// extraction [`range_target`] made for the perp-category decision). Three ways to be
/// [`PageStep::Done`]:
///
/// - the page was empty (history ran out before the requested start);
/// - its oldest kline already reached `start_ms` (the window is fully covered);
/// - the next `end` would NOT be strictly older than the one that produced this page — a cursor that
///   cannot move backward would otherwise spin forever (reachable if the venue ever answers with
///   klines NEWER than the `end` asked for).
///
/// Note what is absent: no page-count / `MAX_LIMIT` comparison. A short page is NOT a stop signal —
/// termination is driven purely by OBSERVED startTimes, so a venue-side change to the 1000-row cap
/// cannot silently truncate a backfill again.
///
/// Uses the page's MINIMUM startTime rather than its first element, so wire order is not assumed —
/// this is what keeps the helper correct whether it is handed Bybit's raw newest-first rows or the
/// ascending bars [`bars_from_envelope`] already reversed.
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
/// `fetch_page(end)` returns one page's bars (any order). The walk seeds `end = end_ms`, folds each
/// page through [`next_page_step`], keeps only bars inside the inclusive `[start_ms, end_ms]`
/// window, and finally sorts ascending + dedups by `ts` (pages arrive newest-block first, and a
/// boundary bar can repeat if the venue's bucketing ever overlaps).
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

/// Fetch closed-kline history for the inclusive `[start_ms, end_ms]` window, walking `end` BACKWARD
/// through Bybit's end-anchored, silently-truncating 1000-rows/response cap (module doc, point 4).
/// Returns bars ascending by startTime, de-duplicated.
///
/// A `symbol` carrying [`vike_catalog::PERP_SUFFIX`] routes to `category=linear` and is stripped
/// before it reaches the wire; the CALLER's suffixed symbol remains the store/series key, so a
/// perp's series can never collide with its spot twin's. Network I/O. Errors on an unsupported
/// `interval`.
///
/// [`fetch_klines_range_paced`] with nothing seeded and the measurement dropped, so this 4-arg shape
/// — the one every backfill seam binds — issues the identical requests on the identical sleeps.
pub fn fetch_klines_range(
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    fetch_klines_range_paced(symbol, interval, start_ms, end_ms, None).map(|(bars, _pace)| bars)
}

/// The pacer the pager runs on: a FALLBACK pacer over [`PAGE_DELAY`] (Bybit publishes no budget to
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
            target: "vike_bybit::data",
            applied,
            seed_request_ms = sample.request_ms,
            "bybit klines: persisted pace offered to the pacer (ETA only — the delay is fixed)"
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
/// likewise inert here: Bybit publishes no weight counter, so the field carries the pacer's seed
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
    let (category, wire) = range_target(symbol);
    let code = interval_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
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
        // for itself, and on this venue it DOMINATES the sleep. It spans `fetch_page_rate_limited`,
        // so a page that was rate-limited and retried reports its retry sleeps as "request time"
        // too — bounded and self-correcting (see `Pacer::observe_request`), and un-separable without
        // reaching inside the shared retry driver.
        //
        // `start_ms` is pinned on every page: the venue clamps it, and undoing that clamp is the
        // whole point of moving `page_end`.
        let started = std::time::Instant::now();
        let page = fetch_page_rate_limited(&agent, category, wire, code, start_ms, page_end)?;
        let elapsed = started.elapsed();
        // Observe unconditionally: the time was spent whether or not rows came back.
        pacer.observe_request(None, elapsed);
        // ONE line per backfill, not per page. `remaining_pages` takes the OBSERVED page size rather
        // than `MAX_LIMIT` — the same discipline `next_page_step` applies to termination, where a
        // row-count comparison is deliberately absent: a venue-side change to the cap shows up in
        // the ETA instead of silently multiplying it.
        if report_pace && !page.is_empty() {
            report_pace = false;
            let pages = remaining_pages(page_end.saturating_sub(start_ms), interval, page.len());
            if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
                tracing::info!(
                    target: "vike_bybit::data",
                    symbol,
                    interval,
                    request_ms = elapsed.as_millis() as u64,
                    page_rows = page.len() as u64,
                    page_delay_ms = pacer.next_delay().as_millis() as u64,
                    remaining_pages = pages,
                    eta_secs = eta.as_secs(),
                    "bybit kline backfill: measured page cost on the fixed page delay"
                );
            }
        }
        Ok(page)
    })?;
    Ok((bars, pacer.measured()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FETCHER's own routing decision — the gate the URL-shaping tests cannot provide, because
    /// they build a URL from `rest_category` directly and that helper was already perp-capable
    /// before this fix. Re-pinning `fetch_klines_range` to `CATEGORY` fails HERE.
    #[test]
    fn range_target_routes_a_perp_symbol_to_linear_with_the_suffix_stripped() {
        assert_eq!(range_target("BTCUSDT.P"), ("linear", "BTCUSDT"));
    }

    /// A bare symbol still resolves the spot category and is passed through untouched.
    #[test]
    fn range_target_routes_a_bare_symbol_to_spot_unchanged() {
        assert_eq!(range_target("BTCUSDT"), (CATEGORY, "BTCUSDT"));
    }

    /// A previous run's record, shaped like the MEASURED sibling page (the CI box, 2026-08-04, okx:
    /// ~480 ms). `budget_per_min: None` because this venue publishes none — which is also what
    /// confines the record to another fallback pacer.
    fn bybit_record(request_ms: u64) -> PaceSample {
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
        assert!(!p.is_discovered(), "bybit publishes no budget to discover");
        assert_eq!(p.next_delay(), PAGE_DELAY);
        assert_eq!(p.eta(1_000), None, "nothing timed ⇒ no ETA, not one guessed from the sleep");

        for _ in 0..4 {
            p.observe_request(None, Duration::from_millis(480));
        }
        assert_eq!(p.next_delay(), PAGE_DELAY, "the measured 480ms must not move the sleep");
        assert!(!p.should_cool_down(), "and no budget ⇒ no target to cross");

        // A seed is likewise ETA-only: it can never make this pager faster.
        let mut seeded = page_pacer(Some(&bybit_record(480)));
        assert_eq!(seeded.next_delay(), PAGE_DELAY);
        seeded.observe_request(Some(9_999), Duration::from_secs(30));
        assert_eq!(seeded.next_delay(), PAGE_DELAY, "nor can any observation, at any scale");
    }

    /// What the measurement BUYS: 30 days of 1m klines is 43 of Bybit's 1000-row pages, whose real
    /// cost is ~20s of request time on top of 8.6s of sleep — a number the 200ms constant alone
    /// could never have produced, and the one the pager's ETA line now prints after page one.
    #[test]
    fn a_measured_page_yields_the_eta_the_fixed_delay_could_not() {
        let mut p = page_pacer(None);
        p.observe_request(None, Duration::from_millis(480));
        let month = 30 * 86_400_000i64;
        let pages = remaining_pages(month, "1m", MAX_LIMIT).expect("1m parses");
        assert_eq!(pages, 43, "30 days of 1m klines at 1000 rows/page");
        let eta = p.eta(pages).expect("a timed page yields an ETA");
        // 43 x (480ms + 200ms) = 29.24s. The sleep-only view would have claimed 8.6s.
        assert!(
            eta > Duration::from_millis(29_000) && eta < Duration::from_millis(29_500),
            "expected ~29s for the measured shape, got {eta:?}"
        );
        // A SEEDED pacer answers the same before page one has even been fetched.
        assert_eq!(page_pacer(Some(&bybit_record(480))).eta(pages), Some(eta));
        // ...and this run, having measured, hands its own observation back out to be persisted.
        let m = p.measured().expect("a fixed pager still times its pages");
        assert_eq!(m.request_ms, 480);
        assert_eq!(m.budget_per_min, None, "nothing discovered ⇒ nothing claimed");
        assert!(m.is_usable());
    }

    /// A minimal newest-first V5 envelope with two klines (so the reversal is exercised).
    const TWO_ROWS: &str = r#"{"retCode":0,"retMsg":"OK","result":{"category":"spot","symbol":"BTCUSDT","list":[
        ["1700000060000","27010.25","27100.00","27000.00","27080.10","8.10000000","219000.00"],
        ["1700000000000","27000.10","27050.50","26980.00","27010.25","12.34567800","333012.50"]
    ]},"retExtInfo":{},"time":1700000200000}"#;

    #[test]
    fn parse_reverses_to_ascending_and_maps_exactly() {
        let bars = parse_bybit_klines(TWO_ROWS).unwrap();
        assert_eq!(bars.len(), 2);
        // newest-first list → ascending bars: the LAST list row is bars[0].
        assert_eq!(bars[0].ts, 1_700_000_000_000);
        assert_eq!(bars[1].ts, 1_700_000_060_000);
        assert!(bars[0].ts < bars[1].ts, "ascending by startTime after reversal");
        assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
        assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
        assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
        assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
        assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
        assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
    }

    #[test]
    fn parse_empty_list_is_empty() {
        let body = r#"{"retCode":0,"retMsg":"OK","result":{"category":"spot","symbol":"BTCUSDT","list":[]},"retExtInfo":{},"time":1}"#;
        assert!(parse_bybit_klines(body).unwrap().is_empty());
    }

    #[test]
    fn parse_nonzero_retcode_errors() {
        let body =
            r#"{"retCode":10001,"retMsg":"params error","result":{},"retExtInfo":{},"time":1}"#;
        assert!(parse_bybit_klines(body).is_err());
    }

    #[test]
    fn parse_rejects_short_row() {
        let body = r#"{"retCode":0,"retMsg":"OK","result":{"list":[["1700000000000","1","2","3"]]},"time":1}"#;
        assert!(parse_bybit_klines(body).is_err());
    }

    #[test]
    fn interval_code_maps_or_errors() {
        assert_eq!(interval_code("1m").unwrap(), "1");
        assert_eq!(interval_code("1h").unwrap(), "60");
        assert_eq!(interval_code("1d").unwrap(), "D");
        assert_eq!(interval_code("1M").unwrap(), "M");
        assert!(interval_code("7s").is_err());
    }

    #[test]
    fn klines_url_has_category_and_bounds() {
        let url = klines_url(CATEGORY, "BTCUSDT", "1", 10, 20, 1000);
        assert_eq!(
            url,
            "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&start=10&end=20&limit=1000"
        );
    }

    /// The perp suffix selects category=linear AND is stripped from the wire symbol.
    #[test]
    fn perp_suffix_selects_linear_and_strips_the_wire_symbol() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
        assert_eq!(wire, "BTCUSDT");
        assert!(perp);
        let url = klines_url(rest_category(perp), wire, "1", 10, 20, 1000);
        assert!(url.contains("category=linear"), "perp must use linear: {url}");
        assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
        assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
    }

    /// A bare symbol is unchanged — spot URLs stay byte-identical to the pre-change form.
    #[test]
    fn a_bare_symbol_still_builds_the_spot_url() {
        let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
        assert!(!perp);
        assert_eq!(
            klines_url(rest_category(perp), wire, "1", 10, 20, 1000),
            "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&start=10&end=20&limit=1000"
        );
    }

    // ---- the end-anchored truncation trap (module doc, point 4) --------------------------------

    const MIN: i64 = 60_000;
    const T0: i64 = 1_700_000_000_000;
    /// 2.5 pages at the 1000-row cap — enough that a single-page test would pass while the window
    /// came back 60% empty.
    const BARS: i64 = 2_500;

    fn synthetic_bar(ts: i64) -> Bar {
        kline_to_bar(ts, 100.0, 101.0, 99.0, 100.5, 1.0)
    }

    /// One page from a fake venue that behaves EXACTLY like Bybit's `/v5/market/kline`: it **ignores
    /// `start` entirely** and answers the newest [`MAX_LIMIT`] klines at or below `page_end`, out of
    /// a synthetic series of [`BARS`] one-minute klines beginning at [`T0`].
    ///
    /// Returned ASCENDING — i.e. already reversed, exactly what [`bars_from_envelope`] hands the
    /// walk — so these tests cannot accidentally paper over a double-reverse. The newest-first RAW
    /// order is covered separately by `the_walk_is_independent_of_wire_order_within_a_page`.
    fn end_anchored_page(page_end: i64) -> Vec<Bar> {
        if page_end < T0 {
            return Vec::new(); // older than the series' first kline
        }
        let newest_i = ((page_end - T0) / MIN).min(BARS - 1);
        let oldest_i = (newest_i - MAX_LIMIT as i64 + 1).max(0);
        (oldest_i..=newest_i).map(|i| synthetic_bar(T0 + i * MIN)).collect()
    }

    /// THE load-bearing test: a multi-page window over a venue that reproduces the live failure.
    ///
    /// The pre-fix forward pager walked `start` upward, so its FIRST request came back at the far
    /// END of the window, the cursor jumped past `end_ms`, and the loop exited after one page —
    /// MEASURED on the CI box 2026-08-04 as 1,001 rows for a 43,200-minute request. A single-page test
    /// cannot see any of that; this one fails outright against the old pager.
    #[test]
    fn the_walk_goes_backward_and_returns_a_multi_page_window_whole() {
        let start = T0;
        let end = T0 + (BARS - 1) * MIN;

        // First prove the FAKE really is faithful: one end-anchored request for the whole window
        // answers only the newest cap-many rows, and its oldest row is nowhere near `start`.
        let naive = end_anchored_page(end);
        assert_eq!(naive.len(), MAX_LIMIT, "the venue truncates to its cap");
        assert_eq!(
            naive[0].ts,
            T0 + (BARS - MAX_LIMIT as i64) * MIN,
            "the newest page only — this is the truncation signature"
        );
        assert!(naive[0].ts > start, "the requested start is ignored; the older tail is dropped");

        let mut pages = 0usize;
        let bars = walk_backward_pages(start, end, |page_end| {
            pages += 1;
            assert!(pages < 100, "runaway walk");
            Ok(end_anchored_page(page_end))
        })
        .expect("walk");

        assert_eq!(pages, 3, "2500 klines at a 1000-row cap is three pages");
        assert!(bars.len() > MAX_LIMIT, "strictly more than one end-anchored page");
        assert_eq!(bars.len(), BARS as usize, "the window must come back WHOLE");
        assert_eq!(bars.first().expect("non-empty").ts, start, "reaches the requested start");
        assert_eq!(bars.last().expect("non-empty").ts, end, "reaches the requested end");
        assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "ascending and de-duplicated");
        for (i, b) in bars.iter().enumerate() {
            assert_eq!(b.ts, T0 + i as i64 * MIN, "contiguous — no gap at the page seams (i={i})");
        }
    }

    /// The double-reverse guard: [`next_page_step`] keys off the page MINIMUM, not its first or last
    /// element, so the same window comes back identically whether pages arrive ascending (what
    /// [`bars_from_envelope`] produces) or newest-first (Bybit's raw `result.list` order). An
    /// implementation that read `page[0].ts` or `page.last()` would walk the wrong direction on one
    /// of the two and fail here.
    #[test]
    fn the_walk_is_independent_of_wire_order_within_a_page() {
        let start = T0;
        let end = T0 + (BARS - 1) * MIN;
        let ascending =
            walk_backward_pages(start, end, |pe| Ok(end_anchored_page(pe))).expect("asc walk");
        let newest_first = walk_backward_pages(start, end, |pe| {
            let mut p = end_anchored_page(pe);
            p.reverse(); // Bybit's RAW result.list order, before `bars_from_envelope` reverses it
            Ok(p)
        })
        .expect("desc walk");

        assert_eq!(ascending.len(), BARS as usize);
        assert_eq!(ascending.len(), newest_first.len(), "same window either way");
        assert!(
            ascending.iter().zip(&newest_first).all(|(a, b)| a.ts == b.ts),
            "identical ts sequence regardless of intra-page wire order"
        );
        assert_eq!(newest_first.first().expect("non-empty").ts, start);
    }

    /// A venue that keeps answering the SAME newest block regardless of `end` must stop, not spin.
    /// This is the guard the task calls for: termination cannot depend on the cursor advancing.
    #[test]
    fn a_non_advancing_cursor_stops_instead_of_looping_forever() {
        let mut pages = 0usize;
        let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_page_end| {
            pages += 1;
            assert!(pages < 50, "the walk must not loop forever on a stuck cursor");
            // Always the same block, ignoring `page_end` entirely.
            Ok(vec![synthetic_bar(T0 + 9 * MIN), synthetic_bar(T0 + 10 * MIN)])
        })
        .expect("walk");
        assert_eq!(pages, 2, "one page, then one whose oldest failed to move — then stop");
        assert_eq!(bars.len(), 2, "deduped to the two distinct klines");
    }

    /// Cross-page overlap is deduped, out-of-window klines are clipped, and the result is sorted.
    #[test]
    fn pages_are_deduped_sorted_and_clipped_to_the_window() {
        let start = T0 + 2 * MIN;
        let end = T0 + 4 * MIN;
        let mut call = 0usize;
        let bars = walk_backward_pages(start, end, |_page_end| {
            call += 1;
            Ok(match call {
                // newest page: carries one kline ABOVE the window end
                1 => vec![
                    synthetic_bar(T0 + 3 * MIN),
                    synthetic_bar(T0 + 4 * MIN),
                    synthetic_bar(T0 + 5 * MIN),
                ],
                // older page: overlaps T0+3MIN and reaches BELOW the window start
                2 => vec![
                    synthetic_bar(T0 + MIN),
                    synthetic_bar(T0 + 2 * MIN),
                    synthetic_bar(T0 + 3 * MIN),
                ],
                _ => Vec::new(),
            })
        })
        .expect("walk");
        let ts: Vec<i64> = bars.iter().map(|b| b.ts).collect();
        assert_eq!(ts, vec![T0 + 2 * MIN, T0 + 3 * MIN, T0 + 4 * MIN]);
        assert_eq!(call, 2, "the second page reached the start and ended the walk");
    }

    #[test]
    fn an_empty_page_terminates_the_walk_and_an_empty_window_never_asks() {
        let mut pages = 0usize;
        let bars = walk_backward_pages(T0, T0 + 10 * MIN, |_| {
            pages += 1;
            Ok(Vec::new())
        })
        .expect("walk");
        assert_eq!(pages, 1, "an empty page stops the walk immediately");
        assert!(bars.is_empty());

        let mut asked = false;
        let bars = walk_backward_pages(T0 + MIN, T0, |_| {
            asked = true;
            Ok(vec![synthetic_bar(T0)])
        })
        .expect("walk");
        assert!(!asked, "end < start must never touch the venue");
        assert!(bars.is_empty());
    }

    #[test]
    fn a_page_error_aborts_the_walk() {
        let err =
            walk_backward_pages(T0, T0 + 10 * MIN, |_| Err("bybit klines HTTP 500".to_string()))
                .expect_err("the error must propagate, not silently truncate");
        assert!(err.contains("HTTP 500"), "{err}");
    }

    #[test]
    fn next_page_step_decisions() {
        let end = T0 + 10 * MIN;
        // Normal: step to one ms below the page's OLDEST kline.
        assert_eq!(
            next_page_step(&[T0 + 5 * MIN, T0 + 6 * MIN], T0, end),
            PageStep::Next(T0 + 5 * MIN - 1)
        );
        // Wire order is irrelevant — the MINIMUM decides, not the first element.
        assert_eq!(
            next_page_step(&[T0 + 6 * MIN, T0 + 5 * MIN], T0, end),
            PageStep::Next(T0 + 5 * MIN - 1)
        );
        // The window start is covered (exactly, then overshot).
        assert_eq!(next_page_step(&[T0, T0 + MIN], T0, end), PageStep::Done);
        assert_eq!(next_page_step(&[T0 - MIN], T0, end), PageStep::Done);
        // Empty page.
        assert_eq!(next_page_step(&[], T0, end), PageStep::Done);
        // Klines NEWER than the `end` asked for cannot move the cursor backward.
        assert_eq!(next_page_step(&[end + MIN], T0, end), PageStep::Done);
        // A SHORT page is NOT a stop signal: termination is by observed ticks, never a page count.
        assert_eq!(next_page_step(&[T0 + 9 * MIN], T0, end), PageStep::Next(T0 + 9 * MIN - 1));
    }

    #[test]
    fn latest_url_uses_linear_category_for_perp_spot_for_spot() {
        let perp = latest_url(rest_category(true), "BTCUSDT", "1", 1000);
        assert_eq!(
            perp,
            "https://api.bybit.com/v5/market/kline?category=linear&symbol=BTCUSDT&interval=1&limit=1000"
        );
        let spot = latest_url(rest_category(false), "BTCUSDT", "1", 1000);
        assert_eq!(
            spot,
            "https://api.bybit.com/v5/market/kline?category=spot&symbol=BTCUSDT&interval=1&limit=1000"
        );
    }
}
