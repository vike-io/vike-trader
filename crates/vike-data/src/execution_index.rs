//! Tier-3 reconciliation read helpers — bounded recent-window queries over the Tier-2 execution
//! trade log (`kind=exec_fill`). Reconciliation does NOT need the millions of lifetime fills; per
//! pass it needs only the recent `trade_id`s within its lookback window (cold-start dedup) plus the
//! current open-order set. These helpers serve that, so `vike_execution::recon::ReconcileJournalView` reads a
//! bounded slice instead of the full history (unified-journaling #2, Tier 3).
//!
//! Pure over the `HistoryStore` trait (no DataFusion dep) — testable with the `HistoryStoreMemory` double.
//!
//! PERF NOTE (spec's "start with a query; promote to an index only if slow"): `recent_seen_trade_ids`
//! currently scans the series and filters in memory. At millions-of-fills scale this should push the
//! `ts >= since` predicate into a date-partition-pruned scan (a `scan_exec_fills_since`); that is a
//! follow-up optimization, gated on a measured slow lookback. The signature here is stable across
//! that change.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::store::hist::{HistoryStore, HistoryStoreError};

/// Recent execution `trade_id`s for `(venue, symbol)` with `ts >= since_timestamp` — the cold-start dedup
/// set reconciliation needs, scoped to its lookback window (never the full history).
pub fn recent_seen_trade_ids(
    store: &(dyn HistoryStore + Send + Sync),
    venue: &str,
    symbol: &str,
    since_timestamp: i64,
) -> Result<HashSet<String>, HistoryStoreError> {
    Ok(store
        .scan_exec_fills(venue, symbol)?
        .into_iter()
        .filter(|f: &ExecFillRow| f.ts >= since_timestamp)
        .map(|f| f.trade_id)
        .collect())
}

/// Latest known status per order for `(venue, symbol)` with `ts >= since_timestamp`, from the `execution_order`
/// lifecycle snapshots — the order-side cross-check input for `vike_execution::recon::ReconcileJournalView.orders`.
///
/// The series holds one snapshot row per materialized lifecycle step (submit → accept → … →
/// terminal), ts-ascending; the CURRENT status of an order is its highest-ts row, so this collapses
/// to `order_client_id -> latest status string`. The caller maps the string back to its own
/// `OrderStatus` (this crate is below vike-execution and cannot name that type). Window-scoped like
/// [`recent_seen_trade_ids`] so a reconcile pass reads a bounded slice, never the full history.
pub fn recent_order_statuses(
    store: &(dyn HistoryStore + Send + Sync),
    venue: &str,
    symbol: &str,
    since_timestamp: i64,
) -> Result<HashMap<String, String>, HistoryStoreError> {
    let rows =
        store.scan_exec_orders(venue, symbol)?.into_iter().filter(|r| r.ts >= since_timestamp);
    Ok(latest_order_snapshots(rows).into_iter().map(|r| (r.order_client_id, r.status)).collect())
}

/// [`recent_order_statuses`], TYPED: the latest status per order as the `vike_model::OrderStatus` it
/// spells ([`ExecOrderRow::order_status`]), the same rows, window and "current" rule.
///
/// ⚠ **A status word this build has no state for is the forward-compatibility guard**, moved here
/// from its one caller: a NEWER writer may store a state an older reader lacks, so that order is
/// LEFT OUT and a `warn!` names it (venue, symbol, order, word), never read as another state and
/// never dropped in silence.
///
/// The temporary twin of the string reading beside it: once its caller reads this one, the string
/// reading goes and this takes its name.
pub fn recent_order_statuses_typed(
    store: &(dyn HistoryStore + Send + Sync),
    venue: &str,
    symbol: &str,
    since_timestamp: i64,
) -> Result<HashMap<String, vike_model::OrderStatus>, HistoryStoreError> {
    let rows =
        store.scan_exec_orders(venue, symbol)?.into_iter().filter(|r| r.ts >= since_timestamp);
    let mut statuses = HashMap::new();
    for row in latest_order_snapshots(rows) {
        match row.order_status() {
            Some(status) => {
                statuses.insert(row.order_client_id, status);
            }
            // The target its one caller logged under before the guard moved here (decision 0157):
            // subscribers filter on it.
            None => tracing::warn!(
                target: "vike_core::journal_view",
                venue = %venue,
                symbol = %symbol,
                order_client_id = %row.order_client_id,
                status = %row.status,
                "journal order status is not a status this build knows; the order is omitted from \
                 the cross-check"
            ),
        }
    }
    Ok(statuses)
}

/// **The one rule for "an order's current state" over `execution_order` lifecycle snapshots**: one row
/// per `order_client_id`, ordered by its `ts` (ascending; a tie is broken by the order client id so the
/// order is deterministic). Which row is current:
///
/// - **A row the execution stream wrote (`ownership` set) supersedes every row written before the
///   stream** (the retired journal materializer's), never a silent merge.
/// - **Among stream rows, the highest `(journal_generation, journal_sequence)` is current**: the stream
///   writes one row per applied event, two of them can share a millisecond, and their `ts` mixes two
///   clocks (a fill's VENUE time, otherwise the time vike recorded the event), so a fill's row can carry
///   an earlier `ts` than the accept before it, and `ts` cannot order them. The journal sequence is the
///   order they were applied in, and the generation says which journal it counts in: a journal moved
///   aside restarts its sequences, so an order resting across the move is current at its NEW journal's
///   row, however low that sequence is.
/// - **Among rows from before the stream, the highest `ts`**, and on a tie the FIRST row seen — the rule
///   [`recent_order_statuses`] always applied (it calls this), and the one the GUI's execution history
///   collapses by too, so the two cannot disagree about which snapshot is current.
///
/// The input need not be sorted: each rule replaces the kept row only when the new one is STRICTLY later.
pub fn latest_order_snapshots(rows: impl IntoIterator<Item = ExecOrderRow>) -> Vec<ExecOrderRow> {
    let mut latest: HashMap<String, ExecOrderRow> = HashMap::new();
    for r in rows {
        let current = match latest.get(&r.order_client_id) {
            None => true,
            Some(kept) => match (r.ownership.is_some(), kept.ownership.is_some()) {
                (true, false) => true,
                (false, true) => false,
                (true, true) => {
                    (r.journal_generation, r.journal_sequence)
                        > (kept.journal_generation, kept.journal_sequence)
                }
                (false, false) => r.ts > kept.ts,
            },
        };
        if current {
            latest.insert(r.order_client_id.clone(), r);
        }
    }
    let mut out: Vec<ExecOrderRow> = latest.into_values().collect();
    out.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.order_client_id.cmp(&b.order_client_id)));
    out
}

// Gated on `test-support` (not just `test`): these tests use `crate::test_support::HistoryStoreMemory`,
// itself `#[cfg(feature = "test-support")]`. Every test build of this crate has the feature on,
// `-p vike-data` alone too: the self dev-dependency in its manifest turns it on. The gate is what
// keeps a build that lost that edge from failing to resolve the import (E0432).
#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::test_support::{HistoryStoreMemory, execution_order_row};

    fn fill(trade_id: &'static str, ts: i64) -> ExecFillRow {
        ExecFillRow {
            ts,
            trade_id: trade_id.into(),
            order_client_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            px: 100.0,
            commission: 0.0,
            mark_price: None, // irrelevant to this module's trade-id/window tests
            liquidity_side: String::new(), // irrelevant to this module's trade-id/window tests
            commission_asset: String::new(), // irrelevant to this module's trade-id/window tests
            venue_order_id: None,
            account: None,
            position_side: None,
            corrects_trade_id: None,
            correction_seq: None,
            mount_id: None,
            received_ms: None,
            ownership: None,
            journal_sequence: None,
            journal_generation: None,
        }
    }

    #[test]
    fn recent_ids_are_window_scoped() {
        let store = HistoryStoreMemory::new();
        let rows = vec![fill("old", 10), fill("mid", 50), fill("new", 90)];
        store.append_exec_fills("binance", "BTCUSDT", &rows, Some("k1")).unwrap();

        // lookback since ts=50 → {mid, new}, NOT old
        let seen = recent_seen_trade_ids(&store, "binance", "BTCUSDT", 50).unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.contains("mid") && seen.contains("new"));
        assert!(!seen.contains("old"), "the pre-window fill is excluded");

        // since 0 → all; since past-the-end → none
        assert_eq!(recent_seen_trade_ids(&store, "binance", "BTCUSDT", 0).unwrap().len(), 3);
        assert!(recent_seen_trade_ids(&store, "binance", "BTCUSDT", 1000).unwrap().is_empty());
        // unknown symbol → empty (no fills)
        assert!(recent_seen_trade_ids(&store, "binance", "ETHUSDT", 0).unwrap().is_empty());
    }

    #[test]
    fn latest_status_per_order_client_id_wins_and_is_window_scoped() {
        let store = HistoryStoreMemory::new();
        // c1: SUBMITTED@10 → ACCEPTED@20 → FILLED@30 (latest = FILLED); c2: CANCELED@40
        let rows = vec![
            execution_order_row("c1", "SUBMITTED", 10),
            execution_order_row("c1", "ACCEPTED", 20),
            execution_order_row("c1", "FILLED", 30),
            execution_order_row("c2", "CANCELED", 40),
        ];
        store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k1")).unwrap();

        let st = recent_order_statuses(&store, "binance", "BTCUSDT", 0).unwrap();
        assert_eq!(st.get("c1").map(String::as_str), Some("FILLED"), "highest-ts row wins");
        assert_eq!(st.get("c2").map(String::as_str), Some("CANCELED"));

        // window since ts=25 drops c1's pre-window snapshots but keeps its FILLED@30 + c2@40
        let st = recent_order_statuses(&store, "binance", "BTCUSDT", 25).unwrap();
        assert_eq!(st.get("c1").map(String::as_str), Some("FILLED"));
        assert!(st.contains_key("c2"));
        // window past the end → empty; unknown symbol → empty
        assert!(recent_order_statuses(&store, "binance", "BTCUSDT", 1000).unwrap().is_empty());
        assert!(recent_order_statuses(&store, "binance", "ETHUSDT", 0).unwrap().is_empty());
    }

    /// The typed reading answers what the string one does, as states, and leaves out an order whose
    /// latest word no state spells (a newer writer's), keeping the others.
    #[test]
    fn the_typed_statuses_are_the_string_ones_and_skip_an_unknown_word() {
        use vike_model::OrderStatus;
        let store = HistoryStoreMemory::new();
        let rows = vec![
            execution_order_row("c1", "SUBMITTED", 10),
            execution_order_row("c1", "FILLED", 30),
            execution_order_row("c2", "CANCELED", 40),
            execution_order_row("c3", "ACCEPTED", 45),
            execution_order_row("c3", "SOME_FUTURE_STATUS", 50),
        ];
        store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k1")).unwrap();

        let typed = recent_order_statuses_typed(&store, "binance", "BTCUSDT", 0).unwrap();
        assert_eq!(typed.get("c1"), Some(&OrderStatus::Filled), "highest-ts row wins");
        assert_eq!(typed.get("c2"), Some(&OrderStatus::Canceled));
        assert_eq!(typed.get("c3"), None, "an unknown latest word is left out, not an older state");
        assert_eq!(typed.len(), 2);
        let words = recent_order_statuses(&store, "binance", "BTCUSDT", 0).unwrap();
        for (order, status) in &typed {
            assert_eq!(words.get(order).map(String::as_str), Some(status.as_str()), "{order}");
        }
        assert!(
            recent_order_statuses_typed(&store, "binance", "BTCUSDT", 1000).unwrap().is_empty()
        );
    }

    /// The highest-ts row per order client id, ordered by that ts; out-of-order input is still latest-wins, and
    /// on a ts tie the FIRST row seen is kept (the rule `recent_order_statuses` always had).
    #[test]
    fn latest_order_snapshots_keeps_the_highest_timestamp_per_order_client_id() {
        let rows = vec![
            execution_order_row("c2", "CANCELED", 40),
            execution_order_row("c1", "FILLED", 30),
            execution_order_row("c1", "SUBMITTED", 10),
            execution_order_row("c1", "PARTIALLY_FILLED", 30),
            execution_order_row("c3", "ACCEPTED", 5),
        ];
        let got = latest_order_snapshots(rows);
        let seen: Vec<(&str, &str, i64)> =
            got.iter().map(|r| (r.order_client_id.as_str(), r.status.as_str(), r.ts)).collect();
        assert_eq!(seen, vec![("c3", "ACCEPTED", 5), ("c1", "FILLED", 30), ("c2", "CANCELED", 40)]);
    }

    /// A row of the execution stream at `(ts, journal_sequence)`, in the datahub's first journal.
    fn streamed(order_client_id: &str, status: &str, ts: i64, sequence: i64) -> ExecOrderRow {
        ExecOrderRow {
            ownership: Some(crate::exec_log::OWNERSHIP_OURS.into()),
            journal_sequence: Some(sequence),
            journal_generation: Some(1),
            account: Some("ALT".into()),
            ..execution_order_row(order_client_id, status, ts)
        }
    }

    /// **A stream row supersedes the rows from before the stream** of its order, whatever their `ts`: the
    /// materializer's later snapshot, which carries no account, never wins over the stream's state, and an
    /// order the stream never wrote keeps its old rows' rule.
    #[test]
    fn a_stream_order_row_supersedes_a_row_from_before_the_stream() {
        let rows = vec![
            execution_order_row("c1", "PARTIALLY_FILLED", 50),
            streamed("c1", "FILLED", 40, 7),
            execution_order_row("c1", "ACCEPTED", 60),
            execution_order_row("c2", "ACCEPTED", 10),
            execution_order_row("c2", "CANCELED", 20),
        ];
        let got = latest_order_snapshots(rows);
        let seen: Vec<(&str, &str, Option<&str>)> = got
            .iter()
            .map(|r| (r.order_client_id.as_str(), r.status.as_str(), r.account.as_deref()))
            .collect();
        assert_eq!(seen, vec![("c2", "CANCELED", None), ("c1", "FILLED", Some("ALT"))]);
    }

    /// **A journal moved aside restarts its sequences, and the newer journal's row still wins**: an order's row
    /// in the old journal at sequence 900 and its newer row in the new journal at sequence 3, in either order.
    #[test]
    fn a_newer_journals_row_wins_over_a_higher_sequence_of_the_old_journal() {
        let old = streamed("c1", "ACCEPTED", 500, 900);
        let new = ExecOrderRow { journal_generation: Some(2), ..streamed("c1", "FILLED", 400, 3) };
        for rows in [vec![old.clone(), new.clone()], vec![new.clone(), old.clone()]] {
            let got = latest_order_snapshots(rows);
            let seen: Vec<(&str, Option<i64>, Option<i64>)> = got
                .iter()
                .map(|r| (r.status.as_str(), r.journal_generation, r.journal_sequence))
                .collect();
            assert_eq!(seen, vec![("FILLED", Some(2), Some(3))]);
        }
    }

    /// **Stream rows collapse by their journal sequence**: two events of one order in one millisecond, and a
    /// fill whose VENUE time is earlier than the accept vike recorded before it, both end at the event applied
    /// last, in any input order.
    #[test]
    fn stream_order_rows_collapse_by_journal_sequence() {
        let one_millisecond =
            [streamed("c1", "ACCEPTED", 100, 11), streamed("c1", "FILLED", 100, 12)];
        let venue_clock = [streamed("c2", "ACCEPTED", 200, 21), streamed("c2", "FILLED", 197, 22)];
        for flip in [false, true] {
            let mut rows: Vec<ExecOrderRow> =
                one_millisecond.iter().chain(&venue_clock).cloned().collect();
            if flip {
                rows.reverse();
            }
            let got = latest_order_snapshots(rows);
            let seen: Vec<(&str, &str, Option<i64>)> = got
                .iter()
                .map(|r| (r.order_client_id.as_str(), r.status.as_str(), r.journal_sequence))
                .collect();
            assert_eq!(
                seen,
                vec![("c1", "FILLED", Some(12)), ("c2", "FILLED", Some(22))],
                "reversed input: {flip}"
            );
        }
    }
}
