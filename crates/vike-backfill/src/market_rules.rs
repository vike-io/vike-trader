//! **The market-rules ingest**: a prediction-market venue's archived RULES texts into `kind=text` (text kind
//! `vike_data::text_log::TEXT_KIND_POLYMARKET_RULES`), ONE settled UTC day of scheduled market ends at a
//! time — the store half of the datahub's market-rules backfill lane (`polymarket:archive:rules`, the archive
//! series `vike_data::text_log::POLYMARKET_RULES_ARCHIVE_COLLECTOR`). The fetch is
//! a `vike_data::source::MarketRulesSource` (the gatewayhub's, through its client: decision 0164); each row
//! arrives with its own received time (a market's close for its rules, its resolution for its outcome:
//! `crates/bridges/polymarket/src/catalog/rules.rs`'s module doc), which this ingest keeps.
//!
//! **Any other collector names ONE recurring series** (`polymarket:btc-updown-5m:rules`, the series
//! `btc-updown-5m`): its day is that series' closed markets whose window STARTS in it
//! (`vike_data::source::MarketRulesSource::closed_of_series`), the same rows, committed under
//! `market-rules-series:{venue}:{series}:{day}`, a prefix of its own so it never meets the archive's keys.
//! Every rule below holds for it unchanged.
//!
//! # A day of market ENDS is the unit
//!
//! * **The window is rounded OUTWARD to whole UTC days**, and a day is the closed markets whose scheduled
//!   end falls in it, written as ONE commit under `market-rules:{venue}:{day}`. A repeat or an overlapping
//!   window stores nothing twice, and **a stopped or failed request resumes where it ended**: a day whose
//!   key is spent is skipped with one manifest read and no venue request.
//! * **Settled days only**: a day is fetched once its last millisecond is [`MARKET_RULES_SETTLE_MS`] old,
//!   time for its markets to resolve (a proposal's challenge window, most disputes). The first unsettled
//!   day ends the request, `Ok`, with the info log saying where storage stops. shortcut: a market whose
//!   resolution outlasts the settle time is missing from its day for good (the day's key is spent);
//!   counting the day's still-open markets before spending it would close that, if one is ever seen.
//! * **An empty day spends no key** and is asked again by the next request (one page): nothing proves an
//!   empty answer final, and the cost of asking is one request.
//! * **A row the source hands back under another text kind is refused**, never stored under a series that
//!   does not name it.
//! * **`should_stop` is asked at the top of every day**, never inside one; a stop answers
//!   [`CollectError::Stopped`] naming the day and what was written, never `Ok`.
//! * **Progress**: one info line per stored day (its markets' rows, the days left and an ETA from the
//!   request's own pace), and one summary line at the end.

use std::time::Instant;

use vike_data::source::MarketRulesSource;
use vike_data::text_log::POLYMARKET_RULES_ARCHIVE_COLLECTOR;
use vike_data::{HistoryStore, SeriesId};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_model::time::epoch_ms_to_utc_date;
use vike_store::HistoryStoreDataFusion;

use crate::error::CollectError;

/// How old a day's last millisecond must be before its markets are fetched: a week, time for a market
/// that ended that day to close and resolve.
pub const MARKET_RULES_SETTLE_MS: i64 = 7 * MS_PER_DAY;

/// [`backfill_market_rules_at`] against the wall clock, read ONCE per request.
pub fn backfill_market_rules(
    hist: &HistoryStoreDataFusion,
    source: &dyn MarketRulesSource,
    (text_kind, collector): (&str, &str),
    start_ms: i64,
    end_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<usize, CollectError> {
    backfill_market_rules_at(
        hist,
        source,
        (text_kind, collector),
        (start_ms, end_ms),
        vike_model::now_ms(),
        should_stop,
    )
}

/// **The ingest** (the module doc) into the series `(text_kind, collector)`, against an explicit `now_ms`
/// so a test can stand on a settle boundary. Returns the rows written; `0` when every day was stored
/// already, empty or unsettled.
///
/// # Errors
///
/// An inverted window is refused before anything is read. A failed fetch names its day and what the days
/// before it wrote, which stays written; a refusal from the source and a store error pass through.
pub fn backfill_market_rules_at(
    hist: &HistoryStoreDataFusion,
    source: &dyn MarketRulesSource,
    (text_kind, collector): (&str, &str),
    (start_ms, end_ms): (i64, i64),
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<usize, CollectError> {
    if start_ms > end_ms {
        return Err(CollectError::Refused(format!(
            "window [{start_ms}, {end_ms}] is inverted (start after end); nothing was fetched or written"
        ).into()));
    }
    let venue = source.venue().to_string();
    let series = SeriesId::per_symbol(StoreKindId::Text, text_kind, collector, None);
    // Any collector but the archive's names ONE recurring series (the module doc).
    let one_series = (collector != POLYMARKET_RULES_ARCHIVE_COLLECTOR).then_some(collector);
    let what = one_series.map_or_else(|| venue.clone(), |name| format!("{venue} {name}"));
    let first = start_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let last = end_ms.div_euclid(MS_PER_DAY) * MS_PER_DAY;
    let total = (last - first) / MS_PER_DAY + 1;
    let started = Instant::now();
    let (mut written, mut skipped, mut empty, mut fetched) = (0, 0, 0, 0);
    let mut stops_before: Option<i64> = None;
    for (n, d0) in (first..=last).step_by(MS_PER_DAY as usize).enumerate() {
        let d1 = d0 + (MS_PER_DAY - 1);
        let day = epoch_ms_to_utc_date(d0);
        if should_stop() {
            return Err(CollectError::Stopped(format!(
                "market rules {what}: stopped before {day} (day {} of {total}); {written} rows were \
                 written and stay stored, and repeating the request resumes at {day}",
                n + 1
            )));
        }
        if d1.saturating_add(MARKET_RULES_SETTLE_MS) > now_ms {
            stops_before = Some(d0);
            break;
        }
        let key = match one_series {
            None => format!("market-rules:{venue}:{day}"),
            Some(series) => format!("market-rules-series:{venue}:{series}:{day}"),
        };
        if hist.series_has_commits(&series, &[key.as_str()])?[0] {
            skipped += 1;
            continue;
        }
        let answer = match one_series {
            None => source.closed_ending(d0, d1),
            Some(series) => source.closed_of_series(series, d0, d1),
        };
        let rows = answer.map_err(|e| match CollectError::from(e) {
            CollectError::Fetch(why) => CollectError::fetch_reworded(&why, format!(
                "market rules {what} {day} (day {} of {total}) failed after {written} rows were \
                 written (the days before it stay stored; repeating the request resumes at {day}): {why}",
                n + 1
            )),
            other => other,
        })?;
        if let Some(stray) = rows.iter().find(|r| r.kind != text_kind) {
            return Err(CollectError::Refused(format!(
                "market rules {what} {day}: the source answered a `{}` row for the `{text_kind}` series; \
                 nothing of the day was written",
                stray.kind
            ).into()));
        }
        if rows.is_empty() {
            empty += 1;
            continue;
        }
        let day_rows = hist.append_texts(text_kind, collector, &rows, Some(&key))?;
        written += day_rows;
        fetched += 1;
        let left = total - (n as i64 + 1);
        let eta_secs = started.elapsed().as_secs_f64() / fetched as f64 * left as f64;
        tracing::info!(
            "market rules {what} {day}: {day_rows} rows; day {} of {total}, {left} left, about \
             {eta_secs:.0}s to go",
            n + 1
        );
    }
    if let Some(d0) = stops_before {
        tracing::info!(
            "market rules {what}: storage stops before {} — a day of markets is written once it \
             is {} days old; asking again later stores it",
            epoch_ms_to_utc_date(d0),
            MARKET_RULES_SETTLE_MS / MS_PER_DAY
        );
    }
    tracing::info!(
        "market rules {what} [{}, {}]: {total} days, {fetched} stored ({written} rows), {skipped} \
         already stored, {empty} empty, in {:.1}s",
        epoch_ms_to_utc_date(first),
        epoch_ms_to_utc_date(last),
        started.elapsed().as_secs_f64()
    );
    Ok(written)
}

#[path = "market_rules_tests.rs"]
#[cfg(test)]
mod market_rules_tests;
