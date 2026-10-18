//! **The dukascopy tick lane**: `vike_dukascopy::fetch_quotes_range` — the bridge's keyless `.bi5`
//! CDN fetch — answered as a `Fetch { Ticks }`, fetched ONE HOUR FILE AT A TIME so a cancel can land
//! between hour files.
//!
//! The datahub keeps the other half of a tick backfill: `vike_backfill::venues::dukascopy`'s
//! `backfill_quotes_then_bars` stores the ticks as quotes and resamples them to bars, and takes the
//! fetch as a closure — the closure is now `vike_gatewayhub_client::history::RemoteTicks::fetcher`.
//! Only the venue half is here.
//!
//! # Why an hour at a time, and why that changes nothing
//!
//! `fetch_quotes_range(symbol, start, end)` walks the window's hour files itself (24 a day, each a
//! download with a retry schedule of about 74 s) and has no point at which a caller can say stop. This
//! lane calls it once per hour-aligned slice of the window instead: slice `[max(hour, start),
//! min(hour + 1h - 1 ms, end)]` selects exactly the one hour file the whole-window call would have
//! fetched for that hour and keeps the same ticks (`start <= ts <= end`), so the concatenation of the
//! slices IS the whole-window answer, hour for hour, in order. A window whose end precedes its start
//! is handed to the bridge whole, which is what the in-process path did with it.
//!
//! # What the lane refuses before it asks the CDN
//!
//! Both the window and the symbol are CLIENT-NAMED on the wire (the verb is Write-scope, but a Write key is still a peer, not this daemon). The
//! lane makes one real request per hour file, so a window of `GW_TICKS_MAX_SPAN_MS` (31 days, 745 hour
//! files unaligned) is the most one fetch may name: a wider one is `Refused` with the venue untouched.
//! (A window of `0..i64::MAX` would otherwise be about 2.5e12 requests, ended only by a link drop.) The
//! symbol is spliced into the CDN URL path by the bridge, so it must be a plain code: 1 to
//! `MAX_SYMBOL_BYTES` ASCII letters, digits, `_` or `-`; a `/`, a `.` or a `?` would let the client
//! choose the path. The datahub's tick backfill cuts its window into chunks of a day (or one bar, when
//! a bar is longer), so a legitimate request is far under both.
//!
//! # Cancellation: what stops and what cannot
//!
//! `FetchCtx::is_cancelled` is asked BEFORE every hour file, so a cancelled fetch stops within one
//! hour's download. It cannot stop INSIDE one: the bridge's retry schedule for a single hour is up to
//! about 74 s and exposes no probe, and a cancel that lands there is seen when the hour returns.

use std::sync::Arc;

use vike_gatewayhub_client::proto::{FetchFailure, GW_TICKS_MAX_SPAN_MS};
use vike_marketdata::QuoteTick;

use super::{FetchCtx, cancelled};

/// One hour, in epoch milliseconds: the bridge's own `HOUR_MS` is private to it, so a slice boundary
/// is spelled here.
const HOUR_MS: i64 = 3_600_000;

/// The longest symbol code the lane will put in a CDN path, in bytes. A Dukascopy code is a currency pair
/// or a short instrument name (`EURUSD`, `USA500IDXUSD`).
const MAX_SYMBOL_BYTES: usize = 32;

/// The venue half of a tick fetch: `(symbol, start_ms, end_ms)` to the quotes of that window, or the
/// bridge's own words. `vike_dukascopy::fetch_quotes_range` is the production one; a suite injects a
/// scripted one.
pub type QuoteFetch = Arc<dyn Fn(&str, i64, i64) -> Result<Vec<QuoteTick>, String> + Send + Sync>;

/// The shape of a tick fetch as [`fetch_quotes`] takes it (no thread bounds: it only calls it).
pub type QuoteFn<'a> = dyn Fn(&str, i64, i64) -> Result<Vec<QuoteTick>, String> + 'a;

/// The production tick fetch: the bridge's keyless `.bi5` range.
pub fn dukascopy_quotes() -> QuoteFetch {
    Arc::new(vike_dukascopy::fetch_quotes_range)
}

/// Why this window or symbol is refused before the CDN is asked, or `None`. A window whose end
/// precedes its start has no span to bound.
fn refusal(symbol: &str, start_ms: i64, end_ms: i64) -> Option<String> {
    let plain = !symbol.is_empty()
        && symbol.len() <= MAX_SYMBOL_BYTES
        && symbol.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !plain {
        return Some(format!(
            "gatewayhub: a tick symbol is 1 to {MAX_SYMBOL_BYTES} ASCII letters, digits, `_` or \
             `-`; {} bytes were sent; nothing was asked of the venue",
            symbol.len()
        ));
    }
    let span = i128::from(end_ms) - i128::from(start_ms) + 1;
    if span > i128::from(GW_TICKS_MAX_SPAN_MS) {
        return Some(format!(
            "gatewayhub: a tick fetch spans {} hours and one request may name at most {} (one \
             hour file is one CDN request); ask in chunks; nothing was asked of the venue",
            span / i128::from(HOUR_MS),
            GW_TICKS_MAX_SPAN_MS / HOUR_MS
        ));
    }
    None
}

/// `Fetch { Ticks }`: `fetch` over `[start_ms, end_ms]`, an hour file at a time, asking `ctx` before
/// each. A bridge failure is `FetchFailure::Fetch` with its text unchanged (the in-process path's
/// `Err(String)`), and a failed hour fails the whole fetch — never a short answer, because the datahub
/// stores what it is given and spends the commit key on it.
pub fn fetch_quotes(
    fetch: &QuoteFn<'_>,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    ctx: &FetchCtx,
) -> Result<Vec<QuoteTick>, FetchFailure> {
    if let Some(why) = refusal(symbol, start_ms, end_ms) {
        return Err(FetchFailure::Refused(why));
    }
    if start_ms > end_ms {
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        return fetch(symbol, start_ms, end_ms).map_err(FetchFailure::Fetch);
    }
    let mut out = Vec::new();
    let mut hour = start_ms.div_euclid(HOUR_MS).saturating_mul(HOUR_MS);
    while hour <= end_ms {
        if ctx.is_cancelled() {
            return Err(cancelled("between hour files"));
        }
        let from = hour.max(start_ms);
        let to = hour.saturating_add(HOUR_MS - 1).min(end_ms);
        out.extend(fetch(symbol, from, to).map_err(FetchFailure::Fetch)?);
        let Some(next) = hour.checked_add(HOUR_MS) else { break };
        hour = next;
    }
    Ok(out)
}

#[cfg(test)]
#[path = "ticks_tests.rs"]
mod ticks_tests;
