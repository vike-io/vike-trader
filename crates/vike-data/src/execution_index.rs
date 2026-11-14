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

/// **The one rule for "an order's current state" over `execution_order` lifecycle snapshots**: the
/// highest-`ts` row per `order_client_id`, ordered by that `ts` (ascending; a tie is broken by the
/// order client id so the order is deterministic).
///
/// ⚠ A later row REPLACES the kept one only when its `ts` is STRICTLY greater, so on a `ts` tie the
/// FIRST row seen is kept — the rule [`recent_order_statuses`] always applied (it now calls this),
/// and the one the GUI's execution history collapses by too, so the two cannot disagree about
/// which snapshot is current. The input need not be sorted: the guard makes an out-of-order scan
/// latest-wins as well.
pub fn latest_order_snapshots(rows: impl IntoIterator<Item = ExecOrderRow>) -> Vec<ExecOrderRow> {
    let mut latest: HashMap<String, ExecOrderRow> = HashMap::new();
    for r in rows {
        match latest.get(&r.order_client_id) {
            Some(kept) if kept.ts >= r.ts => {}
            _ => {
                latest.insert(r.order_client_id.clone(), r);
            }
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
}
