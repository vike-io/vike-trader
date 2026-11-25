//! **The option-trades ingest**: an options venue's public trade log into `kind=option_trade`, ONE settled
//! UTC day at a time, and that day's hourly chain rows DERIVED from it into `kind=chain` — the store half of
//! the datahub's option-trades backfill lane (`deribit:BTC:option-trades`). The fetch is a
//! `vike_data::source::OptionTradeSource` (the gatewayhub's, through its client: decision 0164).
//!
//! # A day is the unit
//!
//! * **The window is rounded OUTWARD to whole UTC days**, and a day is written under its own keys:
//!   `option_trade:{venue}:{currency}:{day}` for the trades, and one `option_trade_chain:…` key per underlying
//!   the day's trades name, for the derived chain rows (`vike_data::store::store_kind`'s `option_trade` and
//!   `chain` rows declare both). Two requests that cover one day share its keys, so a repeat or an
//!   overlapping window stores nothing twice, and **a stopped or failed request resumes where it ended**:
//!   a day whose trade key is spent is skipped with one manifest read and no venue request.
//! * **Settled days only**: a day is written once its last millisecond is `SETTLE_MARGIN_MS` old (the kline
//!   ingest's slack, `crate::klines`), so a day still trading is never stored short. The first unsettled
//!   day ends the request, `Ok`, with the info log saying where storage stops.
//! * **The chain rows are written BEFORE the trades**, and the trade key is the day's "done": a request that
//!   died between the two finds the trade key unspent, fetches the day again, and its chain appends are
//!   no-ops under their spent keys. The reverse order could leave a day's trades stored and its chain never
//!   derived.
//! * **An empty day spends no key** and is asked again by the next request (one page): nothing proves an
//!   empty answer final, and the cost of asking is one request.
//! * **`should_stop` is asked at the top of every day**, never inside one; a stop answers
//!   [`CollectError::Stopped`] naming the day and what was written, never `Ok`.
//! * **Progress**: one info line per stored day (its trades, its chain rows, the days left and an ETA from
//!   the request's own pace), and one summary line at the end.

use std::collections::BTreeMap;
use std::time::Instant;

use vike_data::chain_log::ChainRow;
use vike_data::option_trade_log::hourly_chain_rows;
use vike_data::source::OptionTradeSource;
use vike_data::{HistoryStore, SeriesId};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::error::CollectError;
use crate::klines::SETTLE_MARGIN_MS;

/// [`backfill_option_trades_at`] against the wall clock, read ONCE per request.
pub fn backfill_option_trades(
    hist: &HistoryStoreDataFusion,
    source: &dyn OptionTradeSource,
    currency: &str,
    start_ms: i64,
    end_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<usize, CollectError> {
    backfill_option_trades_at(
        hist,
        source,
        currency,
        (start_ms, end_ms),
        vike_model::now_ms(),
        should_stop,
    )
}

/// **The ingest** (the module doc), against an explicit `now_ms` so a test can stand on a settle
/// boundary. Returns the TRADES written (the chain rows are logged); `0` when every day was stored already,
/// empty or unsettled.
///
/// # Errors
///
/// An inverted window or a currency that cannot be a directory name is refused before anything is read.
/// A failed fetch names its day and what the days before it wrote, which stays written; a refusal from the
/// source and a store error pass through.
pub fn backfill_option_trades_at(
    hist: &HistoryStoreDataFusion,
    source: &dyn OptionTradeSource,
    currency: &str,
    (start_ms, end_ms): (i64, i64),
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<usize, CollectError> {
    if start_ms > end_ms {
        return Err(CollectError::Refused(format!(
            "window [{start_ms}, {end_ms}] is inverted (start after end); nothing was fetched or written"
        ).into()));
    }
    vike_paths::store_path::refuse_a_path_hostile_symbol(currency)
        .map_err(|why| CollectError::Refused(why.into()))?;
    let venue = source.venue().to_string();
    let series = SeriesId::per_symbol(StoreKindId::OptionTrade, &venue, currency, None);
    let first = start_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let last = end_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let total = (last - first) / MS_PER_DAY + 1;
    let started = Instant::now();
    let (mut trades_written, mut chain_written, mut skipped, mut empty, mut fetched) =
        (0, 0, 0, 0, 0);
    let mut stops_before: Option<i64> = None;
    for (n, d0) in (first..=last).step_by(MS_PER_DAY as usize).enumerate() {
        let d1 = d0 + (MS_PER_DAY - 1);
        let day = epoch_ms_to_utc_date(d0);
        if should_stop() {
            return Err(CollectError::Stopped(format!(
                "option trades {venue} {currency}: stopped before {day} (day {} of {total}); {trades_written} \
                 trades and {chain_written} chain rows were written and stay stored, and repeating the \
                 request resumes at {day}",
                n + 1
            )));
        }
        if d1.saturating_add(SETTLE_MARGIN_MS) > now_ms {
            stops_before = Some(d0);
            break;
        }
        let key = format!("option_trade:{venue}:{currency}:{day}");
        if hist.series_has_commits(&series, &[key.as_str()])?[0] {
            skipped += 1;
            continue;
        }
        let mut trades = source.fetch(currency, d0, d1).map_err(|e| match CollectError::from(e) {
            CollectError::Fetch(why) => CollectError::fetch_reworded(&why, format!(
                "option trades {venue} {currency} {day} (day {} of {total}) failed after {trades_written} \
                 trades were written (the days before it stay stored; repeating the request resumes at \
                 {day}): {why}",
                n + 1
            )),
            other => other,
        })?;
        trades.retain(|t| (d0..=d1).contains(&t.ts));
        if trades.is_empty() {
            empty += 1;
            continue;
        }
        // The chain first, the trades last: the trade key is the day's "done" (the module doc).
        let mut by_underlying: BTreeMap<String, Vec<ChainRow>> = BTreeMap::new();
        for row in hourly_chain_rows(&trades) {
            by_underlying.entry(row.underlying.clone()).or_default().push(row);
        }
        let mut day_chain = 0;
        for (underlying, rows) in &by_underlying {
            let chain_key = format!("option_trade_chain:{venue}:{currency}:{underlying}:{day}");
            day_chain += hist.append_chain_snapshot(&venue, underlying, rows, Some(&chain_key))?;
        }
        let written = hist.append_option_trades(&venue, currency, &trades, Some(&key))?;
        trades_written += written;
        chain_written += day_chain;
        fetched += 1;
        let left = total - (n as i64 + 1);
        let eta_secs = started.elapsed().as_secs_f64() / fetched as f64 * left as f64;
        tracing::info!(
            "option trades {venue} {currency} {day}: {written} trades, {day_chain} chain rows ({} underlyings); \
             day {} of {total}, {left} left, about {eta_secs:.0}s to go",
            by_underlying.len(),
            n + 1
        );
    }
    if let Some(d0) = stops_before {
        tracing::info!(
            "option trades {venue} {currency}: storage stops before {} — a day is written once it is over and \
             {} minutes old; asking again later stores it",
            epoch_ms_to_utc_date(d0),
            SETTLE_MARGIN_MS / 60_000
        );
    }
    tracing::info!(
        "option trades {venue} {currency} [{}, {}]: {total} days, {fetched} stored ({trades_written} trades, \
         {chain_written} chain rows), {skipped} already stored, {empty} empty, in {:.1}s",
        epoch_ms_to_utc_date(first),
        epoch_ms_to_utc_date(last),
        started.elapsed().as_secs_f64()
    );
    Ok(trades_written)
}

#[path = "option_trades_tests.rs"]
#[cfg(test)]
mod option_trades_tests;
