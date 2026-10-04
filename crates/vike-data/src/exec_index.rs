//! Tier-3 reconciliation read helpers — bounded recent-window queries over the Tier-2 execution
//! trade log (`kind=exec_fill`). Reconciliation does NOT need the millions of lifetime fills; per
//! pass it needs only the recent `trade_id`s within its lookback window (cold-start dedup) plus the
//! current open-order set. These helpers serve that, so `vike_exec::recon::JournalView` reads a
//! bounded slice instead of the full history (unified-journaling #2, Tier 3).
//!
//! Pure over the `HistStore` trait (no DataFusion dep) — testable with the `MemHistStore` double.
//!
//! PERF NOTE (spec's "start with a query; promote to an index only if slow"): `recent_seen_trade_ids`
//! currently scans the series and filters in memory. At millions-of-fills scale this should push the
//! `ts >= since` predicate into a date-partition-pruned scan (a `scan_exec_fills_since`); that is a
//! follow-up optimization, gated on a measured slow lookback. The signature here is stable across
//! that change.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::exec_log::ExecFillRow;
use crate::hist::{DataError, HistStore};

/// Recent execution `trade_id`s for `(venue, symbol)` with `ts >= since_ts` — the cold-start dedup
/// set reconciliation needs, scoped to its lookback window (never the full history).
pub fn recent_seen_trade_ids(
    store: &(dyn HistStore + Send + Sync),
    venue: &str,
    symbol: &str,
    since_ts: i64,
) -> Result<HashSet<String>, DataError> {
    Ok(store
        .scan_exec_fills(venue, symbol)?
        .into_iter()
        .filter(|f: &ExecFillRow| f.ts >= since_ts)
        .map(|f| f.trade_id)
        .collect())
}

/// Latest known status per order for `(venue, symbol)` with `ts >= since_ts`, from the `exec_order`
/// lifecycle snapshots — the order-side cross-check input for `vike_exec::recon::JournalView.orders`.
///
/// The series holds one snapshot row per materialized lifecycle step (submit → accept → … →
/// terminal), ts-ascending; the CURRENT status of an order is its highest-ts row, so this collapses
/// to `client_order_id -> latest status string`. The caller maps the string back to its own
/// `OrderStatus` (this crate is below vike-exec and cannot name that type). Window-scoped like
/// [`recent_seen_trade_ids`] so a reconcile pass reads a bounded slice, never the full history.
pub fn recent_order_statuses(
    store: &(dyn HistStore + Send + Sync),
    venue: &str,
    symbol: &str,
    since_ts: i64,
) -> Result<HashMap<String, String>, DataError> {
    // scan is ts-ascending; keep the highest-ts row per coid (guarded, so an out-of-order scan is
    // still latest-wins).
    let mut latest: HashMap<String, (i64, String)> = HashMap::new();
    for r in store.scan_exec_orders(venue, symbol)?.into_iter().filter(|r| r.ts >= since_ts) {
        match latest.get(&r.client_order_id) {
            Some((ts, _)) if *ts >= r.ts => {}
            _ => {
                latest.insert(r.client_order_id.clone(), (r.ts, r.status));
            }
        }
    }
    Ok(latest.into_iter().map(|(coid, (_, status))| (coid, status)).collect())
}

// Gated on `test-support` (not just `test`): these tests use `crate::test_support::MemHistStore`,
// itself `#[cfg(feature = "test-support")]`. The `hist-datafusion` CI job builds vike-data's tests
// WITHOUT test-support (its isolated `-p vike-data -p vike-backfill` selection pulls in no bridge
// dev-deps to unify the feature), so an un-gated `#[cfg(test)]` here fails to resolve the import
// (E0432). The default `test` job has test-support unified in, so these still compile and run there.
#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::exec_log::ExecOrderRow;
    use crate::test_support::MemHistStore;

    fn fill(trade_id: &'static str, ts: i64) -> ExecFillRow {
        ExecFillRow {
            ts,
            trade_id: trade_id.into(),
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            px: 100.0,
            commission: 0.0,
            mark_price: None, // irrelevant to this module's trade-id/window tests
            liquidity_side: String::new(), // irrelevant to this module's trade-id/window tests
            commission_asset: String::new(), // irrelevant to this module's trade-id/window tests
        }
    }

    #[test]
    fn recent_ids_are_window_scoped() {
        let store = MemHistStore::new();
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

    fn order(coid: &str, status: &str, ts: i64) -> ExecOrderRow {
        ExecOrderRow {
            ts,
            client_order_id: coid.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "LIMIT".into(),
            status: status.into(),
            price: Some(100.0),
            trigger_price: None,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        }
    }

    #[test]
    fn latest_status_per_coid_wins_and_is_window_scoped() {
        let store = MemHistStore::new();
        // c1: SUBMITTED@10 → ACCEPTED@20 → FILLED@30 (latest = FILLED); c2: CANCELED@40
        let rows = vec![
            order("c1", "SUBMITTED", 10),
            order("c1", "ACCEPTED", 20),
            order("c1", "FILLED", 30),
            order("c2", "CANCELED", 40),
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
}
