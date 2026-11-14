//! `funding_record` - the FUNDING RECORDER: each settlement the live funding-rate lane reports, for a key the
//! strategy lane holds, is written into the store's `interval=funding` series (the one the funding backfill
//! fills, `vike_data::source::FUNDING_INTERVAL`), carrying the settlement time, the rate the venue stated last
//! for it and the price it was valued at. A restarted paper engine catches up the settlements it missed from
//! that series (decision 0262's amendment of 2026-11-08), so nobody has to fetch history before a restart.
//!
//! # What a settlement is here
//!
//! The venue's feed states its current rate and the symbol's NEXT settlement time about once a second
//! (`vike_marketdata::FundingRate`). This keeps one clock per key, the paper engine's rule
//! (`vike_execution::Engine::note_funding_rate`): the first observation starts it, a later `next_timestamp` means
//! the venue ROLLED and the held settlement happened, at the last rate stated for it; an earlier one still past
//! the last recorded settlement is a settlement brought forward (binance and okx at a capped rate) and moves
//! the clock; one at or before the last recorded settlement changes nothing. The price is the funding price the
//! venue states when it is not the mark (Hyperliquid's oracle), else the last mark the lane carried for the key,
//! else none (the store's `close` of `0.0`). A settlement the lane did not see roll (the datahub was down) is
//! not recorded; the backfill fills it.
//!
//! # Where it sits, and what it never touches
//!
//! * **Nothing slow on a venue client's thread.** `DataClientSinkStrategyLane`'s `funding` and `mark_tick` call
//!   [`FundingRateRecorder::observe`] and [`FundingRateRecorder::mark`]: one uncontended lock and a map lookup (a key's
//!   first observation allocates its entry), and a roll is a `try_send` to one worker thread, which writes. A
//!   full queue drops the settlement with a warning (the backfill fills it).
//! * **The store's existing writer.** The production writer (`store_writer`, `backfill-serve` only) is
//!   `vike_backfill::funding_rate::backfill_funding_rate` over a one-point source, the funding backfill's own
//!   ingest (its bar shape, its hour floor, its commit key over the settlement's own window, and its rule of ONE
//!   rate bar per settlement hour: a settlement the series already holds, backfilled or recorded, is not written
//!   again, and a backfill over recorded settlements writes only the ones the series lacks).
//! * **Armed where the start history is** (`crate::datahub_cli`'s `arm_start_history`): in a process that
//!   compacts its store (one part per settlement until a pass merges them) and in a `backfill-serve` build.

use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};

use vike_marketdata::{FundingRate, FundingRatePoint};

/// Writes one settlement of `(venue, symbol)`: `Ok(rows written)`, `0` when the store holds it already. The
/// production writer is `store_writer` (`backfill-serve` only, so no doc link). A settlement is a
/// [`FundingRatePoint`]: `timestamp_ms` the settlement time (the `next_timestamp` the venue announced for it), `rate` the rate
/// the venue stated last for it, `mark` the price it was valued at (the stated funding price, else the lane's
/// last mark; `None` for none seen), and no `premium` (the live feed states none).
pub type FundingRateWriter =
    Box<dyn Fn(&str, &str, FundingRatePoint) -> Result<usize, String> + Send + 'static>;

/// How many settlements may wait for the writer. A settlement is at most one per key an hour; the bound is
/// for a store that stopped answering.
const QUEUE_CAP: usize = 256;

/// One key's clock: what the venue said last, and the newest settlement recorded.
#[derive(Debug, Clone, Copy)]
struct Clock {
    next_timestamp: i64,
    rate: f64,
    price: Option<f64>,
    mark: Option<f64>,
    recorded: i64,
}

/// See the module doc. Cheap to share (an `Arc`); attached once by the composition root.
pub struct FundingRateRecorder {
    clocks: Mutex<HashMap<String, HashMap<String, Clock>>>,
    tx: SyncSender<(String, String, FundingRatePoint)>,
}

impl std::fmt::Debug for FundingRateRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FundingRateRecorder").finish_non_exhaustive()
    }
}

impl FundingRateRecorder {
    /// A recorder writing through `writer` on a worker thread of its own.
    pub fn spawn(writer: FundingRateWriter) -> std::io::Result<Arc<Self>> {
        let (tx, rx) = sync_channel::<(String, String, FundingRatePoint)>(QUEUE_CAP);
        std::thread::Builder::new().name("datahub-funding-record".into()).spawn(move || {
            for (venue, symbol, settled) in rx {
                match writer(&venue, &symbol, settled) {
                    Ok(rows) => tracing::info!(
                        venue,
                        symbol,
                        ts = settled.timestamp_ms,
                        rate = settled.rate,
                        mark = ?settled.mark,
                        rows,
                        "vike-datahub: a live funding settlement recorded into the funding series"
                    ),
                    Err(error) => tracing::warn!(
                        venue,
                        symbol,
                        ts = settled.timestamp_ms,
                        %error,
                        "vike-datahub: a live funding settlement could NOT be recorded; the funding \
                         backfill (`vike-cli data hist fetch <venue>:<symbol>:funding`) fills it"
                    ),
                }
            }
        })?;
        Ok(Arc::new(Self { clocks: Mutex::default(), tx }))
    }

    /// One funding observation of `(venue, symbol)`: the key's clock, and a settlement the venue rolled past
    /// queued for the store.
    pub(crate) fn observe(&self, venue: &str, symbol: &str, funding: &FundingRate) {
        let Some(next_ts) = funding.next_timestamp else { return };
        if !funding.rate.is_finite() {
            return;
        }
        let price = funding.price.filter(|p| p.is_finite() && *p > 0.0);
        let settled = {
            let mut clocks = self.clocks.lock().unwrap_or_else(PoisonError::into_inner);
            match clocks.get_mut(venue).and_then(|by_symbol| by_symbol.get_mut(symbol)) {
                Some(clock) => roll(clock, next_ts, funding.rate, price),
                None => {
                    let clock = Clock {
                        next_timestamp: next_ts,
                        rate: funding.rate,
                        price,
                        mark: None,
                        recorded: 0,
                    };
                    clocks.entry(venue.to_string()).or_default().insert(symbol.to_string(), clock);
                    None
                }
            }
        };
        let Some(settled) = settled else { return };
        match self.tx.try_send((venue.to_string(), symbol.to_string(), settled)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => tracing::warn!(
                venue,
                symbol,
                ts = settled.timestamp_ms,
                "vike-datahub: the funding recorder's queue is full; this settlement is not recorded"
            ),
            Err(TrySendError::Disconnected(_)) => tracing::warn!(
                venue,
                symbol,
                ts = settled.timestamp_ms,
                "vike-datahub: the funding recorder's writer is gone; this settlement is not recorded"
            ),
        }
    }

    /// The lane carried `px` as `(venue, symbol)`'s mark: the price a settlement of a key with a clock is
    /// valued at when the venue states no funding price. A key with no clock is not remembered.
    pub(crate) fn mark(&self, venue: &str, symbol: &str, px: f64) {
        if !(px.is_finite() && px > 0.0) {
            return;
        }
        let mut clocks = self.clocks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(clock) = clocks.get_mut(venue).and_then(|by_symbol| by_symbol.get_mut(symbol)) {
            clock.mark = Some(px);
        }
    }
}

/// The settlement an observation announcing `next_timestamp` (at `rate`, funding price `price`) says happened, and the
/// clock moved (the module doc's rule).
fn roll(
    clock: &mut Clock,
    next_ts: i64,
    rate: f64,
    price: Option<f64>,
) -> Option<FundingRatePoint> {
    if next_ts <= clock.recorded {
        return None;
    }
    let settled =
        (next_ts > clock.next_timestamp && clock.next_timestamp > clock.recorded).then(|| {
            FundingRatePoint {
                timestamp_ms: clock.next_timestamp,
                rate: clock.rate,
                premium: None,
                mark: clock.price.or(clock.mark),
            }
        });
    if let Some(s) = settled {
        clock.recorded = s.timestamp_ms;
    }
    clock.next_timestamp = next_ts;
    clock.rate = rate;
    clock.price = price;
    settled
}

/// One settlement as a funding source: the funding backfill's ingest takes its points from a source, and a
/// settlement the lane saw roll is the whole answer. It touches no network.
#[cfg(feature = "backfill-serve")]
struct OneSettlement {
    venue: String,
    point: FundingRatePoint,
}

#[cfg(feature = "backfill-serve")]
impl vike_data::source::FundingRateSource for OneSettlement {
    fn venue(&self) -> &str {
        &self.venue
    }

    fn fetch(
        &self,
        _symbol: &str,
        _start_ms: i64,
        _end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, vike_data::source::SourceError> {
        Ok(vec![self.point])
    }
}

/// The production [`FundingRateWriter`]: one settlement through `vike_backfill::funding_rate::backfill_funding_rate`,
/// the funding backfill's own ingest, under the commit key of the settlement's own window; it writes nothing when
/// the series already holds a settlement in that hour (that ingest's one-bar-per-settlement-hour rule).
#[cfg(feature = "backfill-serve")]
pub fn store_writer(store: Arc<vike_store::HistoryStoreDataFusion>) -> FundingRateWriter {
    Box::new(move |venue, symbol, settled| {
        let source = OneSettlement { venue: venue.to_string(), point: settled };
        vike_backfill::funding_rate::backfill_funding_rate(
            &store,
            &source,
            symbol,
            settled.timestamp_ms,
            settled.timestamp_ms,
        )
        .map(|written| written.rate_rows)
        .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
#[path = "funding_record_tests.rs"]
mod funding_record_tests;
