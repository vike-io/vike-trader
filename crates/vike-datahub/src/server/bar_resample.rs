//! A bar read FOLDED into a coarser interval in the datahub (`Request::LoadBars`' `resample_to`,
//! `FEATURE_BAR_RESAMPLE`): the stored bars of `interval` over `[start, end]`, read a page at a time and
//! folded by `vike_marketdata::resample`, the one fold (per epoch-aligned window: first open, highest
//! high, lowest low, last close, summed volume). A chart zoomed out over years reads a thousand candles
//! instead of a million rows, and only this daemon reads the store (decision 0084).
//!
//! ⚠ **The window a page ends inside is CARRIED into the next page, never folded twice.** A page's last
//! window may go on in the next page, so its rows wait and are folded with the next page's. Only `resample`
//! says where a window starts (the stamp of the bar it gives the window), so the carry is cut at that
//! stamp, and the answer is `resample` of the whole range's rows, bit for bit
//! (`crates/vike-datahub-client/tests/remote_bar_resample.rs`).
//!
//! Answered WHOLE: both bounds are required, `limit` is refused, and the windows the range touches are
//! counted against `RESAMPLED_BARS_CEILING` before anything is read. The rows read are bounded by
//! `RESAMPLE_SCAN_CEILING`, each page by the page a columns read takes, all under the read's range-read place.

use vike_data::{HistoryStoreError, TimestampRange};
use vike_datahub_client::proto::RESAMPLED_BARS_CEILING;
use vike_datahub_client::refusals::DatahubRefusal;
use vike_marketdata::interval::{bar_open, bar_step_ms};
use vike_marketdata::{Bar, resample};

/// The bars of `interval` over `range` folded into bars of `to`, or the refusal the read answers instead.
/// `read(window, n)` is the bounded store read of one page (`HistoryStore::load_bars_head`'s contract: a
/// complete prefix of `window` holding at least `n` rows unless it is the whole window).
pub(super) fn resampled_bars(
    interval: &str,
    range: TimestampRange,
    limit: Option<u32>,
    to: &str,
    page_rows: usize,
    scan_ceiling: usize,
    mut read: impl FnMut(TimestampRange, usize) -> Result<Vec<Bar>, HistoryStoreError>,
) -> Result<Vec<Bar>, DatahubRefusal> {
    let to = to.to_string();
    if limit.is_some() {
        return Err(DatahubRefusal::ResampleWithLimit { to });
    }
    let (Some(start), Some(end)) = (range.start, range.end) else {
        return Err(DatahubRefusal::ResampleOpenRange { to });
    };
    let interval = interval.to_string();
    let (Some(native), Some(step)) = (bar_step_ms(&interval), bar_step_ms(&to)) else {
        return Err(DatahubRefusal::ResampleUnmeasurable { interval, to });
    };
    if step < native || step % native != 0 {
        return Err(DatahubRefusal::ResampleNotAMultiple { interval, to });
    }
    if end < start {
        return Ok(Vec::new()); // an empty range holds no bar, the store's own answer
    }
    let windows = (bar_open(end, step) - bar_open(start, step)) / step + 1;
    if windows > RESAMPLED_BARS_CEILING as i64 {
        return Err(DatahubRefusal::ResampleOverWindows {
            to,
            start,
            end,
            windows,
            ceiling: RESAMPLED_BARS_CEILING,
        });
    }
    let (mut out, mut carry, mut scanned) = (Vec::new(), Vec::<Bar>::new(), 0usize);
    let mut from = start;
    loop {
        let page =
            read(TimestampRange::of(from, end), page_rows).map_err(|e| DatahubRefusal::from(&e))?;
        let Some(last) = page.last().map(|b| b.timestamp) else { break };
        scanned += page.len();
        if scanned > scan_ceiling {
            return Err(DatahubRefusal::ResampleOverScan {
                interval,
                to,
                start,
                end,
                scan_ceiling,
            });
        }
        let whole = page.len() < page_rows;
        carry.extend(page);
        let mut folded = resample(&carry, step);
        // The last window may go on past this page: its rows are folded with the next page's.
        if let Some(open) = folded.pop() {
            let cut = carry.partition_point(|b| b.timestamp < open.timestamp);
            out.extend(folded);
            carry.drain(..cut);
        }
        if whole || last >= end {
            break;
        }
        from = last + 1;
    }
    out.extend(resample(&carry, step));
    Ok(out)
}

#[cfg(test)]
#[path = "bar_resample_tests.rs"]
mod bar_resample_tests;
