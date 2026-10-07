//! Cleaved from `binance/data.rs` — the venue-neutral kline→[`Bar`] constructor every kline-based
//! venue bridge (binance/bybit/okx) builds its `Bar`s through, so bar shape stays identical across
//! venues (Phase 3 PR A). Promoted from `pub(crate)` to `pub` — cross-crate callers after the split.
//!
//! Also home of the BACKWARD page walk ([`PageStep`], [`next_page_step`], [`walk_backward_pages`]) a
//! venue with an END-ANCHORED, silently-truncating kline endpoint pages with — bybit's
//! `/v5/market/kline` and deribit's `public/get_tradingview_chart_data`, which each carried a
//! byte-identical copy of it until it was hoisted here. Pure: no clock, no sleeping, no network.

use vike_model::Bar;

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
/// This is the ONE decision that makes an end-anchored endpoint's truncation survivable, hoisted out
/// of the network path so it can be gated by a test. Three ways to be [`PageStep::Done`]:
///
/// - the page was empty (history ran out before the requested start, or the venue answered "no
///   data");
/// - its oldest tick already reached `start_ms` (the window is fully covered);
/// - the next `end` would NOT be strictly older than the one that produced this page — a cursor that
///   cannot move backward would otherwise spin forever (reachable if the venue ever answers with
///   ticks NEWER than the `end` asked for).
///
/// Note what is absent: no page-count / row-cap comparison. A short page is NOT a stop signal —
/// termination is driven purely by OBSERVED ticks, so a venue-side change to its per-response row cap
/// cannot silently truncate a backfill again.
///
/// Uses the page's MINIMUM tick rather than its first element, so wire order is not assumed — this
/// is what keeps the helper correct whether it is handed a venue's raw newest-first rows or the
/// ascending bars a decoder already reversed.
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
/// It never sleeps: the inter-page throttle belongs to the caller's fetcher closure (each venue's
/// `fetch_klines_range`), so a test walks many pages instantly.
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
