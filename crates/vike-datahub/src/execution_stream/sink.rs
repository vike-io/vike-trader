//! **The SINK: one window of streamed records into the store's rows**, under commit keys that NAME the window, so a
//! window written again after any crash rebuilds the same keys and every series that already landed is a store
//! no-op (the store dedups by batch key, never by row: `vike_data::HistoryStore`).
//!
//! # One record alone makes a whole row
//!
//! A report carries the account, the mount, the venue order id and the order's state AFTER its event
//! (`vike_datahub_client::execution_stream::ExecutionReportWire`), so a record maps to its rows with nothing else
//! read and nothing held per order:
//!
//! | record | kind (row) | commit key, one per series per window |
//! |---|---|---|
//! | a report's `fill` | `exec_fill` (`ExecFillRow`) | `stream-fill-{journal_id}-{venue}-{symbol}-{after}-{through}` |
//! | a report's `fee_correction` | `exec_fill` (`ExecFillRow::fee_correction_row`) | the same key |
//! | a report's `order` | `exec_order` (`ExecOrderRow`, `journal_sequence` set) | `stream-order-{journal_id}-{venue}-{symbol}-{after}-{through}` |
//! | a `Forecast` | `exec_forecast` (`ExecForecastRow`) | `stream-forecast-{journal_id}-{venue}-{symbol}-{after}-{through}` |
//! | an `Unverified` | an `execution_stream_gap` row of kind `unverified` | |
//! | a `PositionSnapshot` | one `execution_account_positions` row per account (`vike.db`), under its window's generation | |
//!
//! `{symbol}` is the STORE name (`vike_paths::store_path::store_name_of_user_name`, decision 0290), and
//! `{venue}-{symbol}` stay adjacent so a series rename rewrites the key as it rewrites every other
//! (`crates/vike-store/src/datafusion_hist/rename.rs`'s `rename_in_commit_key`). The `journal_id` is in the key on
//! purpose: a journal moved aside restarts its sequences, and a key without it could collide with an older window
//! of another journal and be dropped as a no-op.
//!
//! # The rules a window keeps
//!
//! - **Every string is remote text** (the trust boundary): the venue must be a catalog venue
//!   (`vike_hub_protocol::wire_ident::validate_catalog_venue`), and the symbol's store name one the store can hold
//!   (`vike_paths::store_path::refuse_a_path_hostile_symbol`). A record that fails is SKIPPED and counted, never the
//!   window: one bad row must not stall the stream. Lengths are bounded by the frame ceiling.
//! - **Only an `Ours` report carries a mount**: a foreign report's `mount_id` is dropped here whatever it says.
//! - **First contact never lets an old row win.** A record up to the journal's first-contact boundary (its head at
//!   its first open, kept in `vike.db`, so a reconnect in the middle of the catch-up changes nothing) is dropped
//!   only when a STREAM row (`ownership` set) of the same identity is already in its series: the resend of a
//!   journal re-identified after a copy. A row written before the stream never blocks a stream row; both are stored, and
//!   `vike_data::ExecFillRow::superseded` and `vike_data::execution_index::latest_order_snapshots` decide on the read.
//! - **A fill with no venue order id and an order client id** (a late fill after a restart, on an order the OMS no
//!   longer holds) takes the venue order id of the newest `exec_order` row of that order in the same series: the
//!   window's own, else the store's, read only for such a record. With none it stays `None`, counted.
//! - **Below the free-space floor a window stores no forecast** (REVIEW-LIVE-MONEY H1: an optional record), and the
//!   session says so once.

use std::collections::HashSet;

use indexmap::IndexMap;
use vike_data::exec_log::{FeeCorrectionFields, OWNERSHIP_FOREIGN, OWNERSHIP_OURS};
use vike_data::{
    ExecFillRow, ExecForecastRow, ExecOrderRow, HistoryStore, HistoryStoreError, TimestampRange,
};
use vike_datahub_client::execution_stream::{
    ExecutionRecordBodyWire, ExecutionRecordWire, ExecutionReportWire, validate_journal_id,
};
use vike_model::orders::ExecutionOwnership;
use vike_secrets::execution_streams::{
    ExecutionAccountPositionsRow, ExecutionStreamGapRow, GAP_UNVERIFIED,
};

use super::ExecutionStreamFaultPoint;

/// One window: the records after `after` through `through`, of journal `journal_id`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window<'a> {
    pub(crate) journal_id: &'a str,
    /// The last record stored before the window; `None` for the journal's first window (its key says `start`).
    pub(crate) after: Option<u64>,
    pub(crate) through: u64,
    pub(crate) records: &'a [ExecutionRecordWire],
    /// The journal's first-contact boundary: the records up to it are checked against the rows already stored.
    pub(crate) first_contact_through: u64,
    /// The journal's generation, stamped on every order row beside its sequence.
    pub(crate) generation: i64,
}

/// What a window skipped, by rule: the session logs them, and none of them fails the window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SinkCounts {
    /// Records whose venue or symbol the store cannot name, or an order row with no order client id.
    pub(crate) unnamed: u64,
    /// First-contact records whose stream row was already in their series.
    pub(crate) already_stored: u64,
    /// Fills with an order client id whose venue order id no order row gave.
    pub(crate) venue_order_id_unknown: u64,
    /// Forecast rows not written below the free-space floor.
    pub(crate) forecasts_held_back: u64,
}

/// The rows one window makes, per series `(venue, store name)`, before anything is read or written.
#[derive(Debug, Default)]
pub(crate) struct WindowRows {
    pub(crate) fills: IndexMap<(String, String), Vec<ExecFillRow>>,
    pub(crate) orders: IndexMap<(String, String), Vec<ExecOrderRow>>,
    pub(crate) forecasts: IndexMap<(String, String), Vec<ExecForecastRow>>,
    pub(crate) unverified: Vec<ExecutionStreamGapRow>,
    pub(crate) account_positions: Vec<ExecutionAccountPositionsRow>,
    pub(crate) counts: SinkCounts,
}

/// What a written window leaves for the session: the gap rows and the position snapshots it owes `vike.db`, its
/// counts, and why the forecasts were held back, when they were.
#[derive(Debug, Default)]
pub(crate) struct WindowWritten {
    pub(crate) unverified: Vec<ExecutionStreamGapRow>,
    pub(crate) account_positions: Vec<ExecutionAccountPositionsRow>,
    pub(crate) counts: SinkCounts,
    pub(crate) forecasts_held_back: Option<String>,
}

/// Why a window did not land.
#[derive(Debug)]
pub(crate) enum SinkError {
    /// A read or an append the store refused: the session retries the SAME window.
    Store(HistoryStoreError),
    /// A test's fault point fired: the process "died" here, and nothing more is written.
    Died,
}

impl From<HistoryStoreError> for SinkError {
    fn from(e: HistoryStoreError) -> Self {
        SinkError::Store(e)
    }
}

/// **The rows of one window**, from its records alone: pure, so a record's mapping is tested with no store.
pub(crate) fn rows_of(window: &Window<'_>, now_ms: i64) -> WindowRows {
    let mut rows = WindowRows::default();
    for record in window.records {
        match &record.body {
            ExecutionRecordBodyWire::Report(report) => {
                let Some(series) = series_of(&report.venue, &report.symbol) else {
                    rows.counts.unnamed += 1;
                    continue;
                };
                report_rows(record, report, series, window.generation, &mut rows);
            }
            ExecutionRecordBodyWire::Forecast(f) => {
                let Some(series) = series_of(&f.venue, &f.symbol) else {
                    rows.counts.unnamed += 1;
                    continue;
                };
                rows.forecasts.entry(series).or_default().push(ExecForecastRow {
                    ts: record.recorded_ms,
                    mount_id: f.mount_id.clone(),
                    value: f.value,
                    horizon_ms: f.horizon_ms,
                    confidence: f.confidence,
                });
            }
            ExecutionRecordBodyWire::Unverified(u) => {
                // A window names its journal, a moved-aside predecessor's its own (a tradehub's boot gives a journal its
                // id before it owes one). An id that is no journal id is remote text: its window is kept, as this one's.
                let journal_id = if validate_journal_id(&u.journal_id).is_ok() {
                    u.journal_id.clone()
                } else {
                    window.journal_id.to_string()
                };
                tracing::error!(
                    journal_id,
                    after_sequence = u.after_sequence,
                    through_sequence = u.through_sequence,
                    from_ms = ?u.from_ms,
                    to_ms = ?u.to_ms,
                    orders = u.order_client_ids.len(),
                    reason = %u.reason,
                    "vike-datahub: a tradehub boot could not regenerate the execution reports of a crashed journal \
                     tail: the executions of this window MAY BE INCOMPLETE (recorded as an unverified gap)"
                );
                rows.unverified.push(ExecutionStreamGapRow {
                    journal_id,
                    kind: GAP_UNVERIFIED.to_string(),
                    after_sequence: u.after_sequence,
                    resumes_at_sequence: u.through_sequence.saturating_add(1),
                    lost_from_ms: u.from_ms,
                    lost_to_ms: u.to_ms,
                    noted_ms: now_ms,
                    reason: Some(u.reason.clone()),
                    order_client_ids: u.order_client_ids.clone(),
                });
            }
            ExecutionRecordBodyWire::PositionSnapshot(s) => {
                for a in &s.accounts {
                    // remote text, the venue checked as a report's is; an account is a stored word, bounded by the frame
                    if vike_hub_protocol::wire_ident::validate_catalog_venue(&a.venue).is_err() {
                        rows.counts.unnamed += 1;
                        continue;
                    }
                    rows.account_positions.push(ExecutionAccountPositionsRow {
                        journal_id: window.journal_id.to_string(),
                        generation: window.generation,
                        sequence: record.sequence,
                        as_of_ms: record.recorded_ms,
                        venue: a.venue.clone(),
                        account: a.account.clone(),
                        unknown: a.unknown.clone(),
                        positions: a.positions.clone(),
                    });
                }
            }
        }
    }
    rows
}

/// A report's rows: its fill, its fee correction and its order, each a row of the report's series.
fn report_rows(
    record: &ExecutionRecordWire,
    report: &ExecutionReportWire,
    series: (String, String),
    generation: i64,
    rows: &mut WindowRows,
) {
    let ours = report.ownership == ExecutionOwnership::Ours;
    let ownership = Some(if ours { OWNERSHIP_OURS } else { OWNERSHIP_FOREIGN }.to_string());
    let mount_id = report.mount_id.clone().filter(|_| ours);
    if let Some(f) = &report.fill {
        rows.fills.entry(series.clone()).or_default().push(ExecFillRow {
            ts: report.at_ms,
            trade_id: f.trade_id.clone(),
            order_client_id: report.order_client_id.clone(),
            venue: report.venue.clone(),
            symbol: report.symbol.clone(),
            side: f.side,
            qty: f.qty,
            px: f.price,
            commission: f.commission,
            mark_price: f.mark_price,
            liquidity_side: f.liquidity_side.clone(),
            commission_asset: f.commission_asset.clone(),
            venue_order_id: report.venue_order_id.clone(),
            account: report.account.clone(),
            position_side: f.position_side.clone(),
            corrects_trade_id: None,
            correction_seq: None,
            mount_id: mount_id.clone(),
            received_ms: None,
            ownership: ownership.clone(),
            journal_sequence: Some(sequence_column(record.sequence)),
            journal_generation: Some(generation),
        });
    }
    // `ExecFillRow::fee_correction_row` over the wire's plain fields (no `FeeAdjustmentEvent` is built, so no remote
    // text is interned), then the stream's own stamps. `correction_seq` is the record whose fold this states, as the
    // materializer's was.
    if let Some(c) = &report.fee_correction {
        let correction = FeeCorrectionFields {
            corrects_trade_id: &c.corrects_trade_id,
            order_client_id: &report.order_client_id,
            venue: &report.venue,
            symbol: &report.symbol,
            ts: report.at_ms,
            price: c.price,
            commission: c.commission,
            commission_asset: &c.commission_asset,
        };
        rows.fills.entry(series.clone()).or_default().push(ExecFillRow {
            mount_id: mount_id.clone(),
            ownership: ownership.clone(),
            journal_sequence: Some(sequence_column(record.sequence)),
            journal_generation: Some(generation),
            ..ExecFillRow::fee_correction_row(
                correction,
                report.caused_by_sequence,
                record.recorded_ms,
                report.venue_order_id.clone(),
                report.account.clone(),
            )
        });
    }
    if let Some(o) = &report.order {
        if report.order_client_id.is_empty() {
            rows.counts.unnamed += 1;
            return;
        }
        rows.orders.entry(series).or_default().push(ExecOrderRow {
            ts: report.at_ms,
            order_client_id: report.order_client_id.clone(),
            venue: report.venue.clone(),
            symbol: report.symbol.clone(),
            side: o.side,
            qty: o.qty,
            order_type: o.order_type.clone(),
            status: o.status.clone(),
            price: o.price,
            trigger_price: o.trigger_price,
            venue_order_id: report.venue_order_id.clone(),
            filled_qty: o.filled_qty,
            avg_fill_px: o.average_fill_price,
            account: report.account.clone(),
            mount_id,
            ownership,
            journal_sequence: Some(sequence_column(record.sequence)),
            journal_generation: Some(generation),
            reason: o.reason.clone(),
        });
    }
}

/// The series `(venue, store name)` a record files under, or `None` for a venue or a symbol the store cannot name.
fn series_of(venue: &str, symbol: &str) -> Option<(String, String)> {
    // Counted by the caller and said once per window; the venue is never echoed (`wire_ident`'s rule).
    if symbol.is_empty() || vike_hub_protocol::wire_ident::validate_catalog_venue(venue).is_err() {
        return None;
    }
    let store = vike_paths::store_path::store_name_of_user_name(symbol);
    match vike_paths::store_path::refuse_a_path_hostile_symbol(&store) {
        Ok(()) => Some((venue.to_string(), store)),
        Err(why) => {
            tracing::warn!(
                venue,
                symbol = %symbol.escape_debug(),
                %why,
                "vike-datahub: a streamed execution whose symbol the store cannot name is skipped"
            );
            None
        }
    }
}

/// A journal sequence as the store's `i64` column: a sequence counts records, so it never nears `i64::MAX`.
fn sequence_column(sequence: u64) -> i64 {
    i64::try_from(sequence).unwrap_or(i64::MAX)
}

/// **Write one window into `store`**: its fills, its forecasts, then its orders, one append per series under the
/// window's keys. `forecast_floor` is the free-space floor (`Some(why)` while there is no room); `fault` is a test's
/// crash point (production passes one that never fires).
pub(crate) fn write_window(
    store: &(dyn HistoryStore + Send + Sync),
    window: &Window<'_>,
    now_ms: i64,
    forecast_floor: Option<&(dyn Fn() -> Option<String> + Send + Sync)>,
    fault: &dyn Fn(ExecutionStreamFaultPoint) -> bool,
) -> Result<WindowWritten, SinkError> {
    // The records up to the first-contact boundary are checked against the stored rows; the rest were journaled
    // after the first contact and are new. One window keeps one key per series, so the two halves are joined back.
    let split = window.records.partition_point(|r| r.sequence <= window.first_contact_through);
    let (checked, new) = window.records.split_at(split);
    let mut rows = rows_of(&Window { records: checked, ..*window }, now_ms);
    first_contact_filter(store, &mut rows)?;
    let WindowRows {
        mut fills,
        mut orders,
        mut forecasts,
        mut unverified,
        mut account_positions,
        mut counts,
    } = rows;
    let later = rows_of(&Window { records: new, ..*window }, now_ms);
    join(&mut fills, later.fills);
    join(&mut orders, later.orders);
    join(&mut forecasts, later.forecasts);
    unverified.extend(later.unverified);
    account_positions.extend(later.account_positions);
    counts.unnamed += later.counts.unnamed;
    let (journal_id, through) = (window.journal_id, window.through);
    // a window from nothing stored has no number before it: the journal's first record may be sequence 0
    let after = window.after.map_or_else(|| "start".to_string(), |a| a.to_string());
    let mut landed = 0usize;
    let landed_one = |landed: &mut usize| {
        *landed += 1;
        if fault(ExecutionStreamFaultPoint::AfterSeries { landed: *landed }) {
            Err(SinkError::Died)
        } else {
            Ok(())
        }
    };

    for ((venue, symbol), rows) in &mut fills {
        let window_orders = orders.get(&(venue.clone(), symbol.clone()));
        fill_in_venue_order_ids(
            store,
            (venue.as_str(), symbol.as_str()),
            rows,
            window_orders,
            &mut counts,
        )?;
        if !rows.is_empty() {
            let key = format!("stream-fill-{journal_id}-{venue}-{symbol}-{after}-{through}");
            store.append_exec_fills(venue, symbol, rows, Some(&key))?;
        }
        landed_one(&mut landed)?;
    }

    let held_back = forecast_floor.filter(|_| !forecasts.is_empty()).and_then(|floor| floor());
    if held_back.is_some() {
        counts.forecasts_held_back = forecasts.values().map(|r| r.len() as u64).sum();
        forecasts.clear();
    }
    for ((venue, symbol), rows) in &mut forecasts {
        if !rows.is_empty() {
            let key = format!("stream-forecast-{journal_id}-{venue}-{symbol}-{after}-{through}");
            store.append_forecasts(venue, symbol, rows, Some(&key))?;
        }
        landed_one(&mut landed)?;
    }

    for ((venue, symbol), rows) in orders {
        if !rows.is_empty() {
            let key = format!("stream-order-{journal_id}-{venue}-{symbol}-{after}-{through}");
            store.append_exec_orders(&venue, &symbol, &rows, Some(&key))?;
        }
        landed_one(&mut landed)?;
    }

    Ok(WindowWritten { unverified, account_positions, counts, forecasts_held_back: held_back })
}

/// **The first-contact filter** over rows of records up to the boundary: a row is dropped when a STREAM row of the
/// same identity is already in its series (fills by `ExecFillRow::execution_identity`, orders by order client id,
/// journal sequence and time, forecasts by mount, time and horizon), each series read over its rows' own time range.
/// An order's identity carries its time because a journal moved aside restarts its sequences: its successor's row
/// may share an order client id and a sequence with the old journal's, never also the time a copy shares.
fn first_contact_filter(
    store: &(dyn HistoryStore + Send + Sync),
    window_rows: &mut WindowRows,
) -> Result<(), HistoryStoreError> {
    let counts = &mut window_rows.counts;
    for ((venue, symbol), rows) in &mut window_rows.fills {
        let range = ts_range(rows.iter().map(|r| r.ts));
        let stored: HashSet<_> = store
            .scan_execution_fills_head(venue, symbol, range, usize::MAX)?
            .into_iter()
            .filter(|r| r.ownership.is_some())
            .filter_map(|r| r.execution_identity().map(owned_identity))
            .collect();
        let before = rows.len();
        rows.retain(|r| {
            r.execution_identity().is_none_or(|id| !stored.contains(&owned_identity(id)))
        });
        counts.already_stored += (before - rows.len()) as u64;
    }
    for ((venue, symbol), rows) in &mut window_rows.forecasts {
        let range = ts_range(rows.iter().map(|r| r.ts));
        let stored: HashSet<(String, i64, i64)> = store
            .scan_forecasts(venue, symbol, range)?
            .into_iter()
            .map(|r| (r.mount_id, r.ts, r.horizon_ms))
            .collect();
        let before = rows.len();
        rows.retain(|r| !stored.contains(&(r.mount_id.clone(), r.ts, r.horizon_ms)));
        counts.already_stored += (before - rows.len()) as u64;
    }
    for ((venue, symbol), rows) in &mut window_rows.orders {
        let range = ts_range(rows.iter().map(|r| r.ts));
        let stored: HashSet<(String, Option<i64>, i64)> = store
            .scan_execution_orders_head(venue, symbol, range, usize::MAX)?
            .into_iter()
            .filter(|r| r.ownership.is_some())
            .map(|r| (r.order_client_id, r.journal_sequence, r.ts))
            .collect();
        let before = rows.len();
        rows.retain(|r| !stored.contains(&(r.order_client_id.clone(), r.journal_sequence, r.ts)));
        counts.already_stored += (before - rows.len()) as u64;
    }
    Ok(())
}

/// `later`'s rows appended after `into`'s, series by series.
fn join<T>(
    into: &mut IndexMap<(String, String), Vec<T>>,
    later: IndexMap<(String, String), Vec<T>>,
) {
    for (series, rows) in later {
        into.entry(series).or_default().extend(rows);
    }
}

/// An execution's identity, owned: `ExecFillRow::execution_identity`'s.
fn owned_identity(id: (&str, Option<&str>, Option<i64>)) -> (String, Option<String>, Option<i64>) {
    (id.0.to_string(), id.1.map(str::to_string), id.2)
}

/// The closed range of `ts`, for a bounded read of a series around a window's rows.
fn ts_range(ts: impl Iterator<Item = i64>) -> TimestampRange {
    let (lo, hi) = ts.fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
    TimestampRange { start: Some(lo), end: Some(hi) }
}

/// The venue order id of every fill that lacks one and names its order, from the newest order row of that order in
/// the same series: the window's own first, else the store's, read once and only when such a fill exists.
fn fill_in_venue_order_ids(
    store: &(dyn HistoryStore + Send + Sync),
    (venue, symbol): (&str, &str),
    rows: &mut [ExecFillRow],
    window_orders: Option<&Vec<ExecOrderRow>>,
    counts: &mut SinkCounts,
) -> Result<(), HistoryStoreError> {
    let lacking = |r: &ExecFillRow| r.venue_order_id.is_none() && !r.order_client_id.is_empty();
    if !rows.iter().any(lacking) {
        return Ok(());
    }
    let mut stored: Option<Vec<ExecOrderRow>> = None;
    for row in rows.iter_mut().filter(|r| lacking(r)) {
        let newest = |orders: &[ExecOrderRow]| {
            orders
                .iter()
                .filter(|o| o.order_client_id == row.order_client_id && o.venue_order_id.is_some())
                .max_by_key(|o| (o.journal_sequence, o.ts))
                .and_then(|o| o.venue_order_id.clone())
        };
        let mut found = window_orders.and_then(|o| newest(o.as_slice()));
        if found.is_none() {
            if stored.is_none() {
                stored = Some(store.scan_exec_orders(venue, symbol)?);
            }
            found = stored.as_deref().and_then(newest);
        }
        match found {
            Some(id) => row.venue_order_id = Some(id),
            None => counts.venue_order_id_unknown += 1,
        }
    }
    Ok(())
}

#[path = "sink_tests.rs"]
#[cfg(test)]
mod sink_tests;
