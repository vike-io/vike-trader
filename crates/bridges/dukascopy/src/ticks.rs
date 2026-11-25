//! **The dukascopy tick lane: the venue's half of `Fetch { Ticks }`** — [`TicksSpecDukascopy`], the
//! `vike_bridge_core::ticks::TicksSpec` the gatewayhub's tick engine (`crates/vike-gatewayhub/src/rest/ticks.rs`'s
//! `fetch_ticks`) runs it through. MOVED from the gatewayhub's `rest::ticks` (owner ruling of 2026-11-12:
//! one-venue code does not live in a shared crate): [`crate::DukascopyTicks::fetch_quotes_range`] — this
//! bridge's keyless `.bi5` CDN fetch, at the venue's history address row — answered ONE HOUR FILE AT A TIME so
//! a cancel can land between hour files.
//!
//! The datahub keeps the other half of a tick backfill: `vike_backfill::venues::dukascopy`'s
//! `backfill_quotes_then_bars` stores the ticks as quotes and resamples them to bars, and takes the
//! fetch as a closure — the closure is `vike_gatewayhub_client::history::GatewayTicks::fetcher`.
//! Only the venue half is here.
//!
//! # Why an hour at a time, and why that changes nothing
//!
//! `fetch_quotes_range(symbol, start, end)` walks the window's hour files itself (24 a day, each a
//! download that may wait out a 503 for up to 220 s, 480 s with a stated `Retry-After`) and has no point at
//! which a caller can say stop. This
//! lane calls it once per hour-aligned slice of the window instead: slice `[max(hour, start),
//! min(hour + 1h - 1 ms, end)]` selects exactly the one hour file the whole-window call would have
//! fetched for that hour and keeps the same ticks (`start <= ts <= end`), so the concatenation of the
//! slices IS the whole-window answer, hour for hour, in order. A window whose end precedes its start
//! is handed to the fetch whole, which is what the in-process path did with it.
//!
//! # The point value of each record: the account's list, never assumed
//!
//! A `.bi5` record's prices are integer points, and the point value is the instrument's own (`10^tick_scale`). The
//! production lane reads it, per fetch, off the row the account's instrument list holds for the symbol
//! (`crate::instrument::grid::history_point_value`, the list the JForex sidecar stated at its login and the process's
//! bus keeps). No list stated yet, or a name the list lacks, fails the fetch in words before the CDN is asked: the
//! vendor warns that a scale applied to the wrong instrument gives wrong prices with no error raised. The datahub's
//! backfill asks again later, as for any failed fetch.
//!
//! # What the lane refuses before it asks the CDN
//!
//! Both the window and the symbol are CLIENT-NAMED on the wire (the verb is Write-scope, but a Write key is
//! still a peer, not the daemon). The lane makes one real request per hour file, so a window of the span bound
//! the gatewayhub hands it (`vike_gatewayhub_client::proto::GATEWAY_TICKS_MAX_SPAN_MS`, 31 days, 745 hour files
//! unaligned) is the most one fetch may name: a wider one is refused with the venue untouched. (A window of
//! `0..i64::MAX` would otherwise be about 2.5e12 requests, ended only by a link drop.) The symbol is spliced
//! into the CDN URL path by the datafeed, so it must be a plain code: 1 to `MAX_SYMBOL_BYTES` ASCII letters,
//! digits, `_` or `-`; a `/`, a `.` or a `?` would let the client choose the path. The datahub's tick backfill
//! cuts its window into chunks of a day (or one bar, when a bar is longer), so a legitimate request is far
//! under both. The refusal words are the gatewayhub's (they reach its peer), so they say so.
//!
//! # One download at a time (the owner's ruling of 2026-11-16)
//!
//! The feed bans a box's IP for 17–24 min after a 429, and the only pace measured safe is one request per
//! 10 s on one connection (`crate::history`'s module doc). So the lane takes ONE download at a time: a second
//! fetch, however many fetch workers the gatewayhub runs, waits for its turn on the lane's lock and asks its
//! stop probe while it waits; a window the lane refuses is answered at once, without waiting. Under the lock
//! the history fetch keeps the process's one stream and its pace.
//!
//! # Cancellation: what stops and what cannot
//!
//! The stop probe is asked while a fetch waits for its turn and BEFORE every hour file, so a cancelled fetch
//! stops within one hour's download. It cannot stop INSIDE one: a single hour may take five requests of up to
//! 60 s and 220 s of waits between them (at most 480 s when the feed states a `Retry-After`: a longer stated
//! wait is taken as the ban and ends the download at once, `crate::history`'s module doc), with no probe, and
//! a cancel that lands there is seen when the hour returns.

use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use vike_bridge_core::ticks::{TicksError, TicksSpec};

use crate::feed::QuoteBus;
use crate::instrument::grid::history_point_value;
use crate::refusals::DukascopyHistoryRefusal;
use vike_marketdata::TickQuote;
use vike_model::MS_PER_HOUR;
use vike_venues::venue_urls::VenueUrls;

/// The longest symbol code the lane will put in a CDN path, in bytes. A Dukascopy code is a currency pair
/// or a short instrument name (`EURUSD`, `USA500IDXUSD`).
const MAX_SYMBOL_BYTES: usize = 32;

/// The venue call of a tick fetch: `(symbol, start_ms, end_ms)` to the quotes of that window, or the
/// datafeed's own words. [`TicksSpecDukascopy::from_urls`] builds the production one; a suite injects a
/// scripted one ([`TicksSpecDukascopy::with_fetch`]).
pub type QuoteFetch = Arc<dyn Fn(&str, i64, i64) -> Result<Vec<TickQuote>, String> + Send + Sync>;

/// The shape of a tick fetch as the hour walk takes it (no thread bounds: it only calls it).
pub type QuoteFn<'a> = dyn Fn(&str, i64, i64) -> Result<Vec<TickQuote>, String> + 'a;

/// How often a download queued behind another asks its stop probe while it waits for its turn.
const QUEUE_POLL: Duration = Duration::from_millis(200);

/// **dukascopy's tick lane**: the hour walk over one venue call, behind the span bound it was built with
/// and its one-download-at-a-time turn. See the module doc.
pub struct TicksSpecDukascopy {
    fetch: QuoteFetch,
    max_span_ms: i64,
    /// One download at a time (the owner's ruling of 2026-11-16: one stream per box): a second fetch
    /// waits here for its turn, its stop probe asked every `QUEUE_POLL`, and never fans out to the feed.
    one_at_a_time: Mutex<()>,
}

impl TicksSpecDukascopy {
    /// The production lane: the datafeed's keyless `.bi5` range at the venue's `dukascopy live history rest`
    /// address row (`urls`, built ONCE here), bounded at `max_span_ms` per fetch, each record scaled by the point value
    /// the account's instrument list on `bus` states for the symbol (the module doc). No address row: every fetch is
    /// refused with the lookup's words, which name the row, and the CDN is never asked.
    pub fn from_urls(urls: &VenueUrls, max_span_ms: i64, bus: Arc<QuoteBus>) -> Self {
        let fetch: QuoteFetch = match crate::DukascopyTicks::from_urls(urls) {
            Ok(feed) => Arc::new(move |symbol, start, end| {
                let point_value = history_point_value(&bus.instrument_list()?, symbol)
                    .map_err(|refusal| refusal.to_string())?;
                feed.fetch_quotes_range(symbol, point_value, start, end)
                    .map_err(|refusal| refusal.to_string())
            }),
            Err(refusal) => {
                let why = refusal.to_string();
                Arc::new(move |_, _, _| Err(why.clone()))
            }
        };
        Self::with_fetch(fetch, max_span_ms)
    }

    /// The lane over `fetch` (a suite's scripted venue call), bounded at `max_span_ms` per fetch.
    pub fn with_fetch(fetch: QuoteFetch, max_span_ms: i64) -> Self {
        Self { fetch, max_span_ms, one_at_a_time: Mutex::new(()) }
    }

    /// This download's turn on the feed: the lane's one lock, `stop` asked every `QUEUE_POLL` while another
    /// download holds it. A lock a panicking download left poisoned is still a turn: it guards no data.
    fn turn(&self, stop: &dyn Fn() -> bool) -> Result<MutexGuard<'_, ()>, TicksError> {
        loop {
            match self.one_at_a_time.try_lock() {
                Ok(turn) => return Ok(turn),
                Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) if stop() => {
                    return Err(TicksError::Stopped("while queued behind another download"));
                }
                Err(TryLockError::WouldBlock) => std::thread::sleep(QUEUE_POLL),
            }
        }
    }
}

impl TicksSpec for TicksSpecDukascopy {
    /// A refused window is answered at once; any other waits for its turn (the lane's one lock) and then walks.
    fn fetch_until(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<TickQuote>, TicksError> {
        if let Some(why) = refusal(symbol, start_ms, end_ms, self.max_span_ms) {
            return Err(TicksError::Refused(why.to_string()));
        }
        let _turn = self.turn(stop)?;
        walk(self.fetch.as_ref(), symbol, start_ms, end_ms, self.max_span_ms, stop)
    }
}

impl std::fmt::Debug for TicksSpecDukascopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicksSpecDukascopy")
            .field("max_span_ms", &self.max_span_ms)
            .finish_non_exhaustive()
    }
}

/// Why this window or symbol is refused before the CDN is asked, or `None`. A window whose end
/// precedes its start has no span to bound.
fn refusal(
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    max_span_ms: i64,
) -> Option<DukascopyHistoryRefusal> {
    let plain = !symbol.is_empty()
        && symbol.len() <= MAX_SYMBOL_BYTES
        && symbol.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !plain {
        return Some(DukascopyHistoryRefusal::SymbolNotPlain {
            max: MAX_SYMBOL_BYTES,
            bytes: symbol.len(),
        });
    }
    let span = i128::from(end_ms) - i128::from(start_ms) + 1;
    if span > i128::from(max_span_ms) {
        return Some(DukascopyHistoryRefusal::SpanTooWide {
            hours: span / i128::from(MS_PER_HOUR),
            max_hours: max_span_ms / MS_PER_HOUR,
        });
    }
    None
}

/// `fetch` over `[start_ms, end_ms]`, an hour file at a time, asking `stop` before each. A venue failure is
/// `TicksError::Fetch` with its text unchanged (the in-process path's `Err(String)`), and a failed hour fails
/// the whole fetch — never a short answer, because the datahub stores what it is given and spends the commit
/// key on it.
fn walk(
    fetch: &QuoteFn<'_>,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    max_span_ms: i64,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<TickQuote>, TicksError> {
    if let Some(why) = refusal(symbol, start_ms, end_ms, max_span_ms) {
        return Err(TicksError::Refused(why.to_string()));
    }
    if start_ms > end_ms {
        if stop() {
            return Err(TicksError::Stopped("before the venue was asked"));
        }
        return fetch(symbol, start_ms, end_ms).map_err(TicksError::Fetch);
    }
    let mut out = Vec::new();
    let mut hour = start_ms.div_euclid(MS_PER_HOUR).saturating_mul(MS_PER_HOUR);
    while hour <= end_ms {
        if stop() {
            return Err(TicksError::Stopped("between hour files"));
        }
        let from = hour.max(start_ms);
        let to = hour.saturating_add(MS_PER_HOUR - 1).min(end_ms);
        out.extend(fetch(symbol, from, to).map_err(TicksError::Fetch)?);
        let Some(next) = hour.checked_add(MS_PER_HOUR) else { break };
        hour = next;
    }
    Ok(out)
}

#[cfg(test)]
#[path = "ticks_tests.rs"]
mod ticks_tests;
