//! OKX V5 REST kline (candlestick) history — the ONE kline fetch + JSON→[`Bar`] mapper for OKX.
//!
//! The OKX twin of binance's `data` module: a pure, fixture-tested `parse_okx_klines` map and a
//! paged, rate-limited `fetch_klines_range`. OKX differs from Binance/Bybit in four ways this module
//! localises:
//!   1. **Envelope + ordering.** The body is the V5 `{code, msg, data}` envelope (`code` a STRING,
//!      "0" = success), and `data` is **newest-first** arrays
//!      `[ts, open, high, low, close, volume, volCcy, volCcyQuote, confirm]`. `parse_okx_klines` maps
//!      the first six fields of each row and **reverses** to ascending-by-ts (the store's contract).
//!   2. **Endpoint + page size.** `GET /api/v5/market/history-candles` serves CLOSED historical
//!      candles, capped at **100** rows/request (far smaller than Binance's 1000).
//!   3. **Cursor paging.** There is no `start`/`end` pair — you page with `after` (records *earlier*
//!      than a ts) / `before` (records *newer*). Backfilling a `[start, end]` window means walking
//!      BACKWARD with `after`: seed `after = end+1`, then set `after` to each page's oldest ts (OKX
//!      `after` is exclusive, so no overlap) until the page reaches `start` or history runs short.
//!   4. **Interval codes + Cloudflare.** `bar` uses lowercase minutes but uppercase hour/day/week
//!      ("1m"→"1m", "1h"→"1H", "1d"→"1D"); [`bar_code`] maps the binance-style input. Requests carry
//!      the browser User-Agent OKX's Cloudflare front expects (reused from the OKX transport).
//!
//! Endpoint: `GET https://www.okx.com/api/v5/market/history-candles?instId=&bar=&after=&limit=100`.
//! Only `fetch_klines_range` does network I/O; `parse_okx_klines` is a pure, fixture-tested map.
//!
//! ## Rate-limit strategy (paged `fetch_klines_range`)
//! `history-candles` is IP-limited to ~20 req / 2 s. The pager throttles a fixed [`PAGE_DELAY`]
//! between pages and, on a rate-limit signal (code 50011/50061 — which OKX returns with HTTP 200 —
//! or HTTP 429), backs off exponentially while HONORING `Retry-After`, retrying the same page up to
//! [`MAX_RATE_LIMIT_RETRIES`].
//!
//! ## The pager MEASURES its pages (and still sleeps [`PAGE_DELAY`])
//! OKX publishes no request-weight budget, so there is nothing to discover and nothing to pace a
//! fraction of — but that never stopped us timing our own round trips, and until now nobody had.
//! MEASURED on the CI box 2026-08-04: spot and perp `history-candles` pages average **476–486 ms**
//! against a hardcoded 200 ms [`PAGE_DELAY`], i.e. the constant is not even the dominant term of the
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
//!     whatever is observed (pinned by that module's tests and by this one's below). With no
//!     published ceiling, a delay derived from a stopwatch alone would be the same un-checkable
//!     hardcoded guess in newer clothes. Acting on the measurement is a separate change with its own
//!     measurement.

use std::time::Duration;

use vike_bridge_core::http::{body_head, get_raw};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::pacer::{Pacer, remaining_pages};
use vike_bridge_core::retry::{BackoffPolicy, Verdict, retry_rate_limited};
use vike_model::Bar;
use vike_model::rate_limits::PaceSample;

// The Cloudflare browser UA is shared (dodges OKX's Cloudflare 1010/403 the same way the signed
// transport does).
use crate::BROWSER_UA;

const REST_CANDLES: &str = "https://www.okx.com/api/v5/market/history-candles";
/// OKX's hard cap on history-candles returned by a single request.
const MAX_LIMIT: usize = 100;

// ---- paged-backfill rate-limit knobs (see the module rate-limit strategy) ----------------------
/// Fixed throttle between successive page requests (keeps us under ~20 req/2 s per IP).
///
/// The VALUE comes from `vike_model::venue_rate_limits`'s `OKX` row rather than from a literal here:
/// OKX runs no request-weight meter and publishes no machine-readable rate-limit metadata, so this
/// delay IS the whole pacing story for history on this venue. The table models that absence as
/// `History::Unweighted` (no ceiling, no soft limit) instead of inventing a number, and records the
/// one figure that makes OKX's pace unlike its siblings': [`MAX_LIMIT`] is **100** rows where
/// binance/aster/bybit page 1000, so the same 200 ms buys a tenth of the history per request.
const PAGE_DELAY: Duration = vike_model::venue_rate_limits::OKX.history.page_delay();
/// Bounded retries for a single page that keeps getting rate-limited.
const MAX_RATE_LIMIT_RETRIES: u32 = 6;
/// First backoff when the server sends no `Retry-After` (doubles each retry, capped).
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Ceiling on any single backoff / `Retry-After` sleep.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// OKX rate-limit codes (arrive with HTTP 200 in the `{code}` envelope): request-too-frequent.
const RATE_LIMIT_CODES: [i64; 2] = [50011, 50061];

/// Map a binance-style interval ("1m","1h","1d",…) to OKX's `bar` code ("1m","1H","1D",…). OKX uses
/// lowercase minutes but uppercase hour/day/week/month. Errors on an unsupported interval. `pub(crate)`
/// so the live kline feed ([`crate::market_feed`]) builds the `candle<bar>` WS channel from the
/// same map (OKX's WS candle channel suffix is exactly this bar code).
pub(crate) fn bar_code(interval: &str) -> Result<&'static str, String> {
    Ok(match interval {
        "1m" => "1m",
        "3m" => "3m",
        "5m" => "5m",
        "15m" => "15m",
        "30m" => "30m",
        "1h" => "1H",
        "2h" => "2H",
        "4h" => "4H",
        "6h" => "6H",
        "12h" => "12H",
        "1d" => "1D",
        "1w" => "1W",
        "1M" => "1M",
        other => return Err(format!("okx: unsupported interval {other:?}")),
    })
}

/// Map one OKX candle row (`[ts, open, high, low, close, volume, …]`, all decimal strings) → a
/// [`Bar`]. Preserves each string's exact f64 bit pattern via `<f64 as FromStr>` — no rounding, no
/// arithmetic. Volume defaults to 0.0 on a parse miss (mirrors the binance mapper).
fn row_to_bar(r: &[serde_json::Value]) -> Result<Bar, String> {
    let s = |i: usize, name: &str| -> Result<&str, String> {
        r.get(i)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("okx candle {name} not str"))
    };
    let t = s(0, "ts")?.parse::<i64>().map_err(|e| format!("okx candle ts parse: {e}"))?;
    let f = |i: usize, name: &str| -> Result<f64, String> {
        s(i, name)?.parse::<f64>().map_err(|e| format!("okx candle {name} parse: {e}"))
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

/// Pull ascending-by-ts [`Bar`]s out of a parsed V5 candles envelope: require `code == "0"`, map
/// `data` (newest-first) and reverse. Shared by the pure map and the pager.
fn bars_from_envelope(v: &serde_json::Value) -> Result<Vec<Bar>, String> {
    let code = v.get("code").and_then(serde_json::Value::as_str).unwrap_or("");
    if code != "0" {
        let msg = v.get("msg").and_then(serde_json::Value::as_str).unwrap_or("");
        return Err(format!("okx candles code {code}: {msg}"));
    }
    let data =
        v.get("data").and_then(serde_json::Value::as_array).ok_or("okx candles: missing data")?;
    let mut bars = data
        .iter()
        .map(|row| {
            row.as_array()
                .ok_or_else(|| "okx candle row not array".to_string())
                .and_then(|r| row_to_bar(r))
        })
        .collect::<Result<Vec<Bar>, String>>()?;
    bars.reverse(); // OKX serves newest-first; the store wants ascending by ts
    Ok(bars)
}

/// Pure map: an OKX V5 candles response body → `Vec<Bar>` ascending by ts. No network — this is the
/// fixture-tested seam.
pub fn parse_okx_klines(body: &str) -> Result<Vec<Bar>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("okx candle json: {e}"))?;
    bars_from_envelope(&v)
}

/// Build the `GET /api/v5/market/history-candles` URL: page BACKWARD with `after` (records earlier
/// than `after_ms`).
fn candles_url(symbol: &str, bar: &str, after_ms: i64, limit: usize) -> String {
    format!("{REST_CANDLES}?instId={symbol}&bar={bar}&after={after_ms}&limit={limit}")
}

/// The headers every candles GET carries, handed to the shared `get_raw` as its extra-headers
/// slice: the browser User-Agent OKX's Cloudflare front expects (the same one the signed transport
/// sends — dodges the 1010/403) plus the Accept it pairs with. Dropping these re-trips Cloudflare,
/// so they are the ONE thing this venue adds to the shared raw GET.
const CANDLE_HEADERS: [(&str, &str); 2] =
    [("User-Agent", BROWSER_UA), ("Accept", "application/json, text/plain, */*")];

/// Fetch ONE page with the rate-limit policy: on HTTP 429/418 or code 50011/50061, honor
/// `Retry-After` (else exponential backoff) and retry the same page up to
/// [`MAX_RATE_LIMIT_RETRIES`]. Returns the page's bars ascending by ts.
fn fetch_page_rate_limited(
    agent: &ureq::Agent,
    symbol: &str,
    bar: &str,
    after_ms: i64,
) -> Result<Vec<Bar>, String> {
    let url = candles_url(symbol, bar, after_ms, MAX_LIMIT);
    let policy = BackoffPolicy {
        max_retries: MAX_RATE_LIMIT_RETRIES,
        initial: INITIAL_BACKOFF,
        max: MAX_BACKOFF,
    };
    // The backoff cadence is the shared driver; the classification (which statuses/`code`s mean rate
    // limited) stays OKX-specific here.
    retry_rate_limited(policy, "okx candles", || {
        let raw = get_raw(agent, &url, "okx candles", &CANDLE_HEADERS)?;
        // HTTP-level rate limit (Cloudflare 429).
        if matches!(raw.status, 429 | 418) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("HTTP {} (rate limited)", raw.status),
            });
        }
        if !(200..300).contains(&raw.status) {
            return Err(format!("okx candles HTTP {}: {}", raw.status, body_head(&raw.body)));
        }
        let v: serde_json::Value =
            serde_json::from_str(&raw.body).map_err(|e| format!("okx candle json: {e}"))?;
        // Business-level rate limit (OKX returns HTTP 200 for these; `code` is a string).
        let code = v
            .get("code")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(-1);
        if RATE_LIMIT_CODES.contains(&code) {
            return Ok(Verdict::RateLimited {
                retry_after: raw.retry_after,
                note: format!("code {code} (rate limited)"),
            });
        }
        bars_from_envelope(&v).map(Verdict::Done)
    })
}

/// The live feed's warmup seed: the newest `limit` CLOSED candles (no cursor), ascending by ts.
/// `history-candles` serves COMPLETED candles only, so — unlike binance/bybit — there is NO
/// still-forming tail here (the WS `candle` channel delivers the in-progress bar). A single request
/// carrying the Cloudflare browser UA (via [`get_raw`]). `limit` must be ≤ 100 (OKX's history-candles
/// cap). Network I/O. Errors on an unsupported `interval`.
pub fn fetch_klines_latest(symbol: &str, interval: &str, limit: usize) -> Result<Vec<Bar>, String> {
    let bar = bar_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let url = format!("{REST_CANDLES}?instId={symbol}&bar={bar}&limit={limit}");
    let raw = get_raw(&agent, &url, "okx candles", &CANDLE_HEADERS)?;
    if !(200..300).contains(&raw.status) {
        return Err(format!("okx candles HTTP {}: {}", raw.status, body_head(&raw.body)));
    }
    parse_okx_klines(&raw.body)
}

/// The pacer the pager runs on: a FALLBACK pacer over [`PAGE_DELAY`] (OKX publishes no budget to
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
            target: "vike_okx::data",
            applied,
            seed_request_ms = sample.request_ms,
            "okx candles: persisted pace offered to the pacer (ETA only — the delay is fixed)"
        );
    }
    pacer
}

/// Fetch closed-candle history for the inclusive `[start_ms, end_ms]` window, paging BACKWARD through
/// OKX's 100-rows/response `history-candles` cap under the module's rate-limit policy. Walks `after`
/// down from `end+1` to `start`, then returns bars ascending by ts, de-duplicated. Network I/O.
/// Errors on an unsupported `interval`.
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

/// [`fetch_klines_range`] with the run's PACE measurement carried in and out: `seed` is a previous
/// run's observation (or `None`, which is byte-identical), and the second element of the return is
/// THIS run's — `None` unless a page was actually timed, so a run that fetched nothing cannot report
/// a fabricated pace.
///
/// ⚠ Neither direction moves a single sleep. A seed on a `Fixed` pacer buys exactly one thing: an
/// ETA from page ZERO instead of page one (`Pacer::next_delay` still answers [`PAGE_DELAY`] — see
/// this module's "The pager MEASURES its pages"). The persisted record's `per_request_weight` is
/// likewise inert here: OKX publishes no weight counter, so the field carries the pacer's seed
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
    let bar = bar_code(interval)?;
    let agent = vike_bridge_core::http::blocking_agent();
    let mut pacer = page_pacer(seed);
    let mut out: Vec<Bar> = Vec::new();
    // Seed the cursor one ms past end so the first page includes the end bar (`after` is exclusive).
    let mut after = end_ms.saturating_add(1);
    let mut first = true;
    // The pace report is emitted ONCE, after the first page that actually returned rows.
    let mut report_pace = true;
    while after > start_ms {
        if !first {
            // Identical to the `PAGE_DELAY` this line used to sleep directly — `Pacer::fallback`
            // exists here to MEASURE, not to steer.
            std::thread::sleep(pacer.next_delay()); // throttle between page requests
        }
        first = false;

        // Wall-clock the request: it is the half of the real inter-request gap the pacer cannot see
        // for itself, and on this venue it DOMINATES the sleep (~480 ms vs 200 ms). It spans
        // `fetch_page_rate_limited`, so a page that was rate-limited and retried reports its retry
        // sleeps as "request time" too — bounded and self-correcting (see `Pacer::observe_request`),
        // and un-separable without reaching inside the shared retry driver.
        let started = std::time::Instant::now();
        let page = fetch_page_rate_limited(&agent, symbol, bar, after)?;
        let elapsed = started.elapsed();
        // Observe BEFORE the empty-page break: the time was spent whether or not rows came back.
        pacer.observe_request(None, elapsed);
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        // ONE line per backfill, not per page. `remaining_pages` takes the OBSERVED page size rather
        // than `MAX_LIMIT`: a venue-side change to the cap then shows up in the ETA instead of
        // silently multiplying it.
        if report_pace {
            report_pace = false;
            let pages = remaining_pages(after.saturating_sub(start_ms), interval, page_len);
            if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
                tracing::info!(
                    target: "vike_okx::data",
                    symbol,
                    interval,
                    request_ms = elapsed.as_millis() as u64,
                    page_rows = page_len as u64,
                    page_delay_ms = pacer.next_delay().as_millis() as u64,
                    remaining_pages = pages,
                    eta_secs = eta.as_secs(),
                    "okx candle backfill: measured page cost on the fixed page delay"
                );
            }
        }
        // page is ascending; its OLDEST candle is the first one.
        let oldest_ts = page.first().map(|b| b.ts).unwrap_or(start_ms);
        for b in page {
            if start_ms <= b.ts && b.ts <= end_ms {
                out.push(b);
            }
        }

        // Stop once the page reached the window start, or history ran short (fewer than the cap).
        if oldest_ts <= start_ms || page_len < MAX_LIMIT {
            break;
        }
        // Next page: strictly older than this page's oldest (OKX `after` is exclusive). Guard against
        // a cursor that fails to move backward.
        if oldest_ts >= after {
            break;
        }
        after = oldest_ts;
    }
    // Pages arrive newest-block first; sort to a single ascending series and drop any boundary dup.
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok((out, pacer.measured()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal newest-first candles envelope with two rows (so the reversal is exercised).
    const TWO_ROWS: &str = r#"{"code":"0","msg":"","data":[
        ["1700000060000","27010.25","27100.00","27000.00","27080.10","8.10000000","219000.00","5913000.00","1"],
        ["1700000000000","27000.10","27050.50","26980.00","27010.25","12.34567800","333012.50","8991000.00","1"]
    ]}"#;

    #[test]
    fn parse_reverses_to_ascending_and_maps_exactly() {
        let bars = parse_okx_klines(TWO_ROWS).unwrap();
        assert_eq!(bars.len(), 2);
        // newest-first data → ascending bars: the LAST data row is bars[0].
        assert_eq!(bars[0].ts, 1_700_000_000_000);
        assert_eq!(bars[1].ts, 1_700_000_060_000);
        assert!(bars[0].ts < bars[1].ts, "ascending by ts after reversal");
        assert_eq!(bars[0].open.to_bits(), 27000.10_f64.to_bits());
        assert_eq!(bars[0].high.to_bits(), 27050.50_f64.to_bits());
        assert_eq!(bars[0].low.to_bits(), 26980.00_f64.to_bits());
        assert_eq!(bars[0].close.to_bits(), 27010.25_f64.to_bits());
        assert_eq!(bars[0].volume.to_bits(), 12.345678_f64.to_bits());
        assert!(bars[0].symbol.is_none() && bars[0].funding.is_none());
    }

    #[test]
    fn parse_empty_data_is_empty() {
        assert!(parse_okx_klines(r#"{"code":"0","msg":"","data":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn parse_nonzero_code_errors() {
        assert!(
            parse_okx_klines(r#"{"code":"50011","msg":"Too Many Requests","data":[]}"#).is_err()
        );
    }

    #[test]
    fn parse_rejects_short_row() {
        assert!(
            parse_okx_klines(r#"{"code":"0","msg":"","data":[["1700000000000","1","2"]]}"#)
                .is_err()
        );
    }

    #[test]
    fn bar_code_maps_or_errors() {
        assert_eq!(bar_code("1m").unwrap(), "1m");
        assert_eq!(bar_code("1h").unwrap(), "1H");
        assert_eq!(bar_code("1d").unwrap(), "1D");
        assert_eq!(bar_code("1M").unwrap(), "1M");
        assert!(bar_code("7s").is_err());
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
        let mut p = page_pacer(None);
        assert!(!p.is_discovered(), "okx publishes no budget to discover");
        assert_eq!(p.next_delay(), PAGE_DELAY);
        assert_eq!(p.eta(1_000), None, "nothing timed ⇒ no ETA, not one guessed from the sleep");

        for _ in 0..4 {
            p.observe_request(None, Duration::from_millis(480));
        }
        assert_eq!(p.next_delay(), PAGE_DELAY, "the measured 480ms must not move the sleep");
        assert!(!p.should_cool_down(), "and no budget ⇒ no target to cross");

        // A seed is likewise ETA-only: it can never make this pager faster.
        let mut seeded = page_pacer(Some(&okx_record(480)));
        assert_eq!(seeded.next_delay(), PAGE_DELAY);
        seeded.observe_request(Some(9_999), Duration::from_secs(30));
        assert_eq!(seeded.next_delay(), PAGE_DELAY, "nor can any observation, at any scale");
    }

    /// What the measurement BUYS, on the shape that motivated it: one day of 1m candles is 14 of
    /// OKX's 100-row pages, and the real cost of that is ~9.5s of request time on top of 2.8s of
    /// sleep — a number the 200ms constant alone could never have produced, and the one the pager's
    /// ETA line now prints after page one.
    #[test]
    fn a_measured_page_yields_the_eta_the_fixed_delay_could_not() {
        let mut p = page_pacer(None);
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
        assert_eq!(page_pacer(Some(&okx_record(480))).eta(pages), Some(eta));
        // ...and this run, having measured, hands its own observation back out to be persisted.
        let m = p.measured().expect("a fixed pager still times its pages");
        assert_eq!(m.request_ms, 480);
        assert_eq!(m.budget_per_min, None, "nothing discovered ⇒ nothing claimed");
        assert!(m.is_usable());
    }

    #[test]
    fn candles_url_pages_with_after() {
        let url = candles_url("BTC-USDT", "1m", 20, 100);
        assert_eq!(
            url,
            "https://www.okx.com/api/v5/market/history-candles?instId=BTC-USDT&bar=1m&after=20&limit=100"
        );
    }
}
