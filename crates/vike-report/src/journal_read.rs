//! `fills_from_journal` — read the account-affecting `FillEvent`s out of a vike-core command
//! journal directory, preserving journal (chronological) order.
//!
//! The journal records every exec-lane message; only the bare `Ingest::Event(Event::Fill(_))`
//! records affect the account (this is exactly the filter `vike_exec::Account::fold` applies —
//! `OrderSubmitted`/`Accepted`/`Filled` lifecycle events are journaled too but carry no
//! position math, so they are dropped here). The extracted fills feed
//! [`vike_analytics::reconstruct_trades`], and [`tearsheet_from_journal`] runs the whole door.
//!
//! ⚠ **This module was the whole reason the crate's `journal` feature existed**, and it is still
//! the one file here that names all three of vike-journal (the journal), vike-exec (the `Ingest`
//! the records wrap) and vike-data (the Tier-2 `kind=exec_fill` series [`fills_from_store`] reads).
//! That closure is what `crates/vike-cli/src/cmd/report.rs` and
//! `crates/vike-tradehub-client/src/proto.rs`'s `Response::Tearsheet` once named as the reason they
//! would not link this crate, and the feature was how a renderer-only consumer declined it. Since
//! 2026-09-28 the renderer half lives in `vike-analytics` instead, so a consumer that only renders
//! names that crate and never this one — and with nothing left here to decline, the feature was
//! deleted rather than kept as a switch every build turns on.

use std::fmt;
use std::path::Path;

use vike_analytics::{LiveTearsheet, equity_curve_from_trades, reconstruct_trades};
use vike_data::{ExecFillRow, HistStore};
use vike_exec::Ingest;
use vike_journal::{CommandJournal, JournalRecord};
use vike_model::events::{Event, FillEvent, TradeId};

/// Error reading fills from a journal directory.
#[derive(Debug)]
pub enum JournalReadError {
    /// The underlying [`CommandJournal::read_all`] I/O / format error.
    Io(std::io::Error),
}

impl fmt::Display for JournalReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalReadError::Io(e) => write!(f, "reading command journal: {e}"),
        }
    }
}

impl std::error::Error for JournalReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JournalReadError::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for JournalReadError {
    fn from(e: std::io::Error) -> Self {
        JournalReadError::Io(e)
    }
}

/// Read every account-affecting `FillEvent` from the journal segments under `dir`, in journal
/// (chronological) order. Non-fill records (lifecycle events, snapshots, strategy submits,
/// commands) are skipped. A torn journal tail stops the read cleanly (that is
/// `CommandJournal::read_all`'s contract) — the fills up to the tear are returned.
pub fn fills_from_journal(dir: &Path) -> Result<Vec<FillEvent>, JournalReadError> {
    let records = CommandJournal::read_all(dir)?;
    let fills = records
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Cmd { msg: Ingest::Event(Event::Fill(f)), .. } => Some(f),
            _ => None,
        })
        .collect();
    Ok(fills)
}

/// Read a command journal directory, reconstruct trades from its fill stream, build the
/// realized-only fallback equity curve from `seed`, and assemble the tearsheet.
///
/// This is the primary live-tearsheet entry point. The equity curve is realized-only (derived
/// from trade PnLs); when a stored `kind=equity` series exists, prefer
/// [`LiveTearsheet::from_result_parts`] with [`crate::equity_curve_from_store`] for a true
/// mark-to-market curve.
///
/// ⚠ **It was the inherent `LiveTearsheet::from_journal` until 2026-09-28**, and it is a FREE
/// function now for a language reason rather than a taste one: the document type moved to
/// `vike-analytics` with the rest of the renderer, and an inherent `impl` may only be written in
/// the crate that owns the type. Keeping the method would have meant keeping the journal read in
/// `vike-analytics`, which may not name `vike_journal` at all. The four steps are unchanged, and
/// `crate::tearsheet_cli`'s realized-only path inlines the same four so the curve stays in hand
/// for `--html`.
pub fn tearsheet_from_journal(
    dir: &Path,
    seed: f64,
    periods_per_year: f64,
) -> Result<LiveTearsheet, JournalReadError> {
    let fills = fills_from_journal(dir)?;
    let trades = reconstruct_trades(&fills);
    let (equity, ts) = equity_curve_from_trades(seed, &trades);
    Ok(LiveTearsheet::from_result_parts(None, trades, equity, ts, periods_per_year))
}

/// Convert a Tier-2 `ExecFillRow` back into the `FillEvent` the tearsheet path consumes. The one
/// field the exec-fill series does not persist (`position_side`) defaults — it does not affect
/// trade reconstruction or any metric, so a store-sourced tearsheet equals a journal-sourced one
/// over the same fills. `mark_price` / `liquidity_side` / `commission_asset` DO round-trip (the
/// series persists all three — `mark_price` for perp markout analysis, the other two for MM
/// fill-rate/adverse-selection analytics), so they are carried through here too.
///
/// **Returns `None` for a row whose `trade_id` is empty**, which is why this is fallible at all.
/// `ExecFillRow::trade_id` stays a `String` — the STORED schema must keep reading Parquet written
/// before [`TradeId`] existed — so validation belongs HERE, at the point a stored row becomes a
/// `FillEvent` again. A legacy empty id is precisely the un-dedupable garbage `TradeId` exists to
/// forbid, and one such row must not make a whole journal unreadable: the caller SKIPS the row and
/// keeps the batch.
pub fn exec_fill_to_event(r: ExecFillRow) -> Option<FillEvent> {
    let trade_id = match TradeId::new(&r.trade_id) {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                venue = %r.venue,
                symbol = %r.symbol,
                ts = r.ts,
                client_order_id = %r.client_order_id,
                error = %e,
                "skipping a stored exec fill with an empty trade_id (legacy row; it can never dedup)"
            );
            return None;
        }
    };
    Some(FillEvent {
        trade_id,
        client_order_id: r.client_order_id,
        venue: r.venue.into(),
        symbol: r.symbol.into(),
        side: r.side,
        last_qty: r.qty,
        last_px: r.px,
        commission: r.commission,
        commission_asset: r.commission_asset.into(),
        liquidity_side: r.liquidity_side.into(),
        ts: r.ts,
        mark_price: r.mark_price,
        position_side: "BOTH".into(),
    })
}

/// Read fills for `(venue, symbol)` from the Tier-2 execution trade log (`kind=exec_fill`) — the
/// unified-journaling read path that supersedes re-scanning the WAL. ts-ascending, matching the
/// journal reader's chronological order. Callers preferring the store fall back to
/// [`fills_from_journal`] when the store has no rows yet (the migration compat window).
pub fn fills_from_store(
    store: &(dyn HistStore + Send + Sync),
    venue: &str,
    symbol: &str,
) -> Result<Vec<FillEvent>, vike_data::DataError> {
    let mut rows = store.scan_exec_fills(venue, symbol)?;
    rows.sort_by_key(|r| r.ts);
    // `filter_map`, not `map`: a legacy row with an empty `trade_id` is dropped (with a warn from
    // `exec_fill_to_event`) rather than failing the whole scan — see that function's doc.
    Ok(rows.into_iter().filter_map(exec_fill_to_event).collect())
}

#[cfg(test)]
mod store_migration_tests {
    use super::*;
    use vike_analytics::DAILY_PERIODS_PER_YEAR;
    use vike_data::{ExecFillRow, MemHistStore};

    fn row(trade_id: &'static str, side: i32, qty: f64, px: f64, ts: i64) -> ExecFillRow {
        ExecFillRow {
            ts,
            trade_id: trade_id.into(),
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side,
            qty,
            px,
            commission: 0.1,
            mark_price: None, // irrelevant to this migration-equivalence test's tearsheet metrics
            liquidity_side: String::new(), // irrelevant to this migration-equivalence test's metrics
            commission_asset: String::new(), // irrelevant to this migration-equivalence test's metrics
        }
    }

    /// `exec_fill_to_event` carries `mark_price`/`liquidity_side`/`commission_asset` through
    /// verbatim (only `position_side` is not persisted by the exec-fill series and defaults).
    #[test]
    fn exec_fill_to_event_round_trips_maker_taker_and_fee_asset() {
        let mut r = row("t1", 1, 1.0, 100.0, 10);
        r.mark_price = Some(100.5);
        r.liquidity_side = "maker".into();
        r.commission_asset = "USDT".into();
        let ev = exec_fill_to_event(r).expect("a non-empty trade_id converts");
        assert_eq!(ev.mark_price, Some(100.5));
        assert!(ev.liquidity_side.is_maker());
        assert_eq!(ev.commission_asset.as_str(), "USDT");
    }

    /// A tearsheet built from the Tier-2 store equals one built from the equivalent journal fills:
    /// `exec_fill_to_event` round-trips the account-affecting fields, so `reconstruct_trades` +
    /// every metric agree. This is the migration-correctness gate (#345 reader reconciliation).
    #[test]
    fn store_sourced_tearsheet_equals_journal_sourced() {
        // a round-trip trade sequence (buy then sell, twice) so metrics are finite
        let rows = vec![
            row("t1", 1, 1.0, 100.0, 10),
            row("t2", -1, 1.0, 110.0, 20),
            row("t3", 1, 1.0, 105.0, 30),
            row("t4", -1, 1.0, 102.0, 40),
        ];
        // "journal" side: the same fills as FillEvents directly
        let journal_fills: Vec<FillEvent> =
            rows.iter().cloned().filter_map(exec_fill_to_event).collect();

        // "store" side: append to Tier-2, read back via fills_from_store
        let store = MemHistStore::new();
        store.append_exec_fills("binance", "BTCUSDT", &rows, Some("k1")).unwrap();
        let store_fills = fills_from_store(&store, "binance", "BTCUSDT").unwrap();

        let a = LiveTearsheet::from_result_parts(
            None,
            reconstruct_trades(&journal_fills),
            equity_curve_from_trades(1_000.0, &reconstruct_trades(&journal_fills)).0,
            equity_curve_from_trades(1_000.0, &reconstruct_trades(&journal_fills)).1,
            DAILY_PERIODS_PER_YEAR,
        );
        let b = LiveTearsheet::from_result_parts(
            None,
            reconstruct_trades(&store_fills),
            equity_curve_from_trades(1_000.0, &reconstruct_trades(&store_fills)).0,
            equity_curve_from_trades(1_000.0, &reconstruct_trades(&store_fills)).1,
            DAILY_PERIODS_PER_YEAR,
        );
        // ⚠ Bit-for-bit over the WHOLE catalog, not over four hand-named fields. It used to be
        // `n_trades`/`net_profit`/`win_rate`/`sharpe`/`max_drawdown` — a five-row sample of a
        // twenty-five-row type — which could not have caught a journal/store divergence in any of
        // the other twenty. `LiveTearsheet` is keyed on `vike_analytics::metric_catalog::METRICS`
        // now, so the loop IS the roster and a metric added there joins this assertion for free.
        for m in vike_analytics::metric_catalog::METRICS {
            match (a.metric(m.id), b.metric(m.id)) {
                (Some(x), Some(y)) => assert_eq!(
                    x.to_bits(),
                    y.to_bits(),
                    "{} differs between the journal and store reads",
                    m.id
                ),
                (x, y) => {
                    assert_eq!(x.is_none(), y.is_none(), "{} is recorded on one side only", m.id)
                }
            }
        }
    }

    /// A PERSISTED row with an empty `trade_id` (written before `TradeId` existed, when the field
    /// was a bare string a venue mapper could reach through `unwrap_or_default()`) is SKIPPED, and
    /// the surrounding good rows still come back. The alternative — failing the scan — would make
    /// one un-dedupable legacy row render a whole exec-fill series unreadable.
    #[test]
    fn a_stored_fill_with_an_empty_trade_id_is_skipped_not_fatal() {
        let mut bad = row("placeholder", 1, 1.0, 100.0, 20);
        bad.trade_id = String::new(); // only reachable by writing the row directly, as legacy did
        let rows = vec![row("t1", 1, 1.0, 100.0, 10), bad, row("t2", -1, 1.0, 110.0, 30)];

        let store = MemHistStore::new();
        store.append_exec_fills("binance", "BTCUSDT", &rows, Some("k1")).unwrap();
        let fills = fills_from_store(&store, "binance", "BTCUSDT")
            .expect("one bad row must not fail the whole scan");

        let ids: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
        assert_eq!(ids, vec!["t1", "t2"], "the good rows survive; the empty-id row is dropped");
    }
}
