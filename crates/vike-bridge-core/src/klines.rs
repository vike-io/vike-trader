//! The venue-neutral kline -> [`Bar`] constructor every kline-based venue bridge builds its `Bar`s
//! through, so bar shape stays identical across venues.
//!
//! Also the BACKWARD page walk ([`PageStep`], [`next_page_step`], [`walk_backward_pages`]) for an
//! END-ANCHORED, silently truncating kline endpoint (bybit's `/v5/market/kline`, deribit's
//! `public/get_tradingview_chart_data`). Pure: no clock, no sleeping, no network.

use vike_marketdata::Bar;

/// One kline → a [`Bar`]. The series key carries venue/symbol/interval, so those stay `None` here.
pub fn kline_to_bar(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    Bar {
        ts: t,
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
/// heads them (`"bybit kline"`, `"okx candle"`) and `ts_name` is field 0's name in the venue's docs
/// (`"startTime"`, `"ts"`), as in `"{what} {field} not str"` and `"{what} {field} parse: {e}"`.
pub fn string_row_to_bar(
    r: &[serde_json::Value],
    what: &str,
    ts_name: &str,
) -> Result<Bar, String> {
    let s = |i: usize, name: &str| -> Result<&str, String> {
        r.get(i).and_then(serde_json::Value::as_str).ok_or_else(|| format!("{what} {name} not str"))
    };
    let t = s(0, ts_name)?.parse::<i64>().map_err(|e| format!("{what} {ts_name} parse: {e}"))?;
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
/// that produced it, decide the next `end`, or stop. Three ways to be [`PageStep::Done`]:
///
/// - the page was empty (history ran out, or the venue answered "no data");
/// - its oldest tick already reached `start_ms` (the window is covered);
/// - the next `end` would NOT be strictly older than this page's: a cursor that cannot move
///   backward would spin forever (reachable if the venue answers with ticks NEWER than `end`).
///
/// ⚠ No page-count / row-cap comparison: a short page is NOT a stop signal. Termination follows
/// OBSERVED ticks only, so a venue-side change to its row cap cannot silently truncate a backfill.
/// Uses the page's MINIMUM tick, not its first, so wire order is not assumed (raw newest-first rows
/// or already-reversed ascending bars).
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

/// The BACKWARD page walk over an INJECTED page fetcher, so the truncation trap is testable without
/// network I/O.
///
/// `fetch_page(end)` returns one page's bars (any order). The walk seeds `end = end_ms`, folds each
/// page through [`next_page_step`], keeps only bars inside the inclusive `[start_ms, end_ms]`, and
/// finally sorts ascending + dedups by `ts` (pages arrive newest block first, and a boundary bar
/// can repeat). It never sleeps: the inter-page throttle belongs to the caller's fetcher closure.
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

#[cfg(test)]
#[path = "klines_tests.rs"]
mod klines_tests;
