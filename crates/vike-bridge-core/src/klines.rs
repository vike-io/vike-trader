//! The venue-neutral kline -> [`Bar`] constructor every kline-based venue bridge builds its `Bar`s
//! through, so bar shape stays identical across venues.
//!
//! Also the PACED BACKWARD kline walk ([`kline_page_walk_backward`] on a [`kline_page_pacer`]) for an
//! END-ANCHORED, silently truncating kline endpoint (bybit's `/v5/market/kline`, okx's
//! `history-candles`, deribit's `public/get_tradingview_chart_data`) whose venue publishes no request
//! budget: the throttle between pages, the wall clock on each, the ONE pace line, over
//! `vike_data::source::page_walk_backward` (the walk's rule lives there, once, for klines and funding
//! alike). The venue hands in its page fetch, [`crate::retry::json_page_fetch`] for a JSON endpoint.

use std::time::{Duration, Instant};

use vike_data::source::{PageWalkStuck, page_walk_backward};
use vike_marketdata::Bar;
use vike_model::rate_limits::PaceSample;

use crate::pacer::{Pacer, remaining_pages};

/// One kline → a [`Bar`]. The series key carries venue/symbol/interval, so those stay `None` here.
pub fn kline_to_bar(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    Bar {
        timestamp: t,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// One kline row of decimal STRINGS, `[ts, open, high, low, close, volume, …]` (bybit's
/// `/v5/market/kline` `[startTime, …, turnover]` and okx's candles), → a [`Bar`]. Preserves each
/// string's exact f64 bit pattern via `<f64 as FromStr>` — no rounding, no arithmetic. Volume defaults
/// to 0.0 on a parse miss (mirrors the binance mapper). The error words are the venue's own: `what`
/// heads them (`"bybit kline"`, `"okx candle"`) and `timestamp_name` is field 0's name in the venue's docs
/// (`"startTime"`, `"ts"`), as in `"{what} {field} not str"` and `"{what} {field} parse: {e}"`.
pub fn string_row_to_bar(
    r: &[serde_json::Value],
    what: &str,
    timestamp_name: &str,
) -> Result<Bar, String> {
    let s = |i: usize, name: &str| -> Result<&str, String> {
        r.get(i).and_then(serde_json::Value::as_str).ok_or_else(|| format!("{what} {name} not str"))
    };
    let t = s(0, timestamp_name)?
        .parse::<i64>()
        .map_err(|e| format!("{what} {timestamp_name} parse: {e}"))?;
    let f = |i: usize, name: &str| -> Result<f64, String> {
        s(i, name)?.parse::<f64>().map_err(|e| format!("{what} {name} parse: {e}"))
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

/// The pacer a backward kline walk runs on for a venue that publishes NO request budget: a FALLBACK
/// pacer over the venue's `page_delay`, offered `seed` (a previous run's [`PaceSample`]; `None` on
/// every production call since `docs/decisions/0094-backfill-names-no-venue.md`). This IS such a
/// venue's whole pacing policy: [`Pacer::next_delay`] answers `page_delay` exactly, whatever is
/// observed or seeded (`crate::policy_pins::assert_the_pager_never_changes_its_fixed_delay`).
pub fn kline_page_pacer(venue: &str, page_delay: Duration, seed: Option<&PaceSample>) -> Pacer {
    let mut pacer = Pacer::fallback(page_delay);
    if let Some(sample) = seed {
        let applied = pacer.seed(sample);
        tracing::debug!(
            venue,
            applied,
            seed_request_ms = sample.request_ms,
            "kline backfill: persisted pace offered to the pacer (ETA only — the delay is fixed)"
        );
    }
    pacer
}

/// **The paced BACKWARD kline walk** over `[start_ms, end_ms]`:
/// `vike_data::source::page_walk_backward` (its rule: `end` one ms below each page's oldest bar, a
/// short page is no stop, a page that does not move `end` is the error
/// `vike_data::source::PageWalkStuck::text` words with `venue`, `symbol` and `interval`) with the
/// venue's `fetch_page(end)` run on `pacer`. It sleeps
/// [`Pacer::next_delay`] between pages, never before the first; wall-clocks every page (a
/// rate-limited page's retry sleeps count as request time) into [`Pacer::observe_request`], empty
/// pages too; and says ONE `info` line, after the first page that returned rows, with the page's
/// cost and the walk's ETA. Returns the bars ascending and de-duplicated, and this run's
/// [`Pacer::measured`] (`None` when no page was timed).
///
/// The venue hands in what is its own: `venue` and `symbol` (the line's fields), `interval` (the ETA's
/// bar width), the pacer ([`kline_page_pacer`]) and the page fetch, which owns the request words: the
/// URL, `start` pinned to the real window start where the venue takes one, okx's exclusive
/// `after = end + 1`.
pub fn kline_page_walk_backward(
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    mut pacer: Pacer,
    mut fetch_page: impl FnMut(i64) -> Result<Vec<Bar>, String>,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    let mut first = true;
    let mut report_pace = true;
    let stuck = |at: PageWalkStuck| at.text(venue, symbol, interval);
    let bars = page_walk_backward::<Bar, String>(start_ms, end_ms, stuck, |page_end| {
        if !std::mem::take(&mut first) {
            std::thread::sleep(pacer.next_delay());
        }
        let started = Instant::now();
        let page = fetch_page(page_end)?;
        let elapsed = started.elapsed();
        pacer.observe_request(None, elapsed);
        // The OBSERVED page size, never the venue's documented cap: a cap the venue changes shows up
        // in the ETA instead of silently multiplying it.
        if report_pace && !page.is_empty() {
            report_pace = false;
            let pages = remaining_pages(page_end.saturating_sub(start_ms), interval, page.len());
            if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
                tracing::info!(
                    venue,
                    symbol,
                    interval,
                    request_ms = elapsed.as_millis() as u64,
                    page_rows = page.len() as u64,
                    page_delay_ms = pacer.next_delay().as_millis() as u64,
                    remaining_pages = pages,
                    eta_secs = eta.as_secs(),
                    "kline backfill: measured page cost on the fixed page delay"
                );
            }
        }
        Ok(page)
    })?;
    Ok((bars, pacer.measured()))
}

#[cfg(test)]
#[path = "klines_tests.rs"]
mod klines_tests;
