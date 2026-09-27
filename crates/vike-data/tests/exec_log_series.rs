//! `kind=exec_fill` / `kind=exec_order` HistStore series — the ACCOUNT fill/order trade-log (Tier-2),
//! NOT market prints. Mirrors the `kind=equity` precedent (`equity_series.rs`) with the two exec kinds.
//!
//! The namespace-guard tests are the point of this file: the store already holds `kind=trade` (market
//! trade ticks). These tests prove account fills (`kind=exec_fill`) and a symbol's market prints
//! (`kind=trade`) NEVER collide even when `(venue, symbol)` are identical — distinct `kind=` partition
//! roots. Split by feature exactly like `equity_series.rs`.
#![cfg(any(feature = "hist-datafusion", feature = "test-support"))]

use vike_data::{ExecFillRow, ExecOrderRow};

/// One account-fill literal: `ts`/`px` vary per call; the rest stay fixed so the round-trip
/// assertions read tersely (mirrors the equity tests' `sample` helper). `mark_price` defaults to
/// `Some(65_001.0)` (a perp venue surfacing it) and `liquidity_side`/`commission_asset` default to
/// "maker"/"BNB" (a venue surfacing both); callers covering the "not surfaced" states (a venue that
/// doesn't surface mark price / maker-taker / fee asset) overwrite the fields afterward, mirroring
/// how the `order()` helper's nullable `price` is handled below.
fn fill(ts: i64, px: f64) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{ts}"),
        client_order_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        px,
        commission: 0.01,
        mark_price: Some(65_001.0),
        liquidity_side: "maker".to_string(),
        commission_asset: "BNB".to_string(),
    }
}

/// One order-lifecycle literal: `ts`/`status`/`filled_qty` vary per call.
fn order(ts: i64, status: &str, filled_qty: f64) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        client_order_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: -1,
        qty: 1.0,
        order_type: "LIMIT".to_string(),
        status: status.to_string(),
        price: Some(65_000.0),
        trigger_price: None,
        venue_order_id: Some("v1".to_string()),
        filled_qty,
        avg_fill_px: if filled_qty > 0.0 { 65_000.0 } else { 0.0 },
    }
}

#[cfg(feature = "hist-datafusion")]
mod datafusion_tests {
    use super::{fill, order};
    use vike_data::{CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, TsRange};
    use vike_model::TradeTick;

    #[test]
    fn exec_fill_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // Cover BOTH states for all three "venue may not surface it" fields: a perp/maker venue
        // surfacing mark_price/liquidity_side/commission_asset (the `fill()` default), and one that
        // surfaces none of them (mark_price=None, liquidity_side/commission_asset="") — the
        // nullable/additive-column round-trip this test exists to prove.
        let mut not_surfaced = fill(1, 100.0);
        not_surfaced.mark_price = None;
        not_surfaced.liquidity_side = String::new();
        not_surfaced.commission_asset = String::new();
        let rows = vec![not_surfaced, fill(2, 101.0), fill(3, 102.0)];
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
        assert_eq!(
            got, rows,
            "ts-ascending, every field preserved (incl. nullable mark_price and the two additive \
             liquidity_side/commission_asset string columns)"
        );
    }

    #[test]
    fn exec_order_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // Cover BOTH nullable states: an accepted (unfilled, market — None price) then a filled order.
        let mut open = order(1, "ACCEPTED", 0.0);
        open.price = None;
        open.venue_order_id = None;
        let rows = vec![open, order(2, "PARTIALLY_FILLED", 0.5), order(3, "FILLED", 1.0)];
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
        assert_eq!(got, rows, "ts-ascending, nullable price/venue_order_id preserved");
    }

    #[test]
    fn exec_fill_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![fill(1, 100.0), fill(2, 101.0)];
        // same key twice → second is a no-op; None key always appends.
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 2);
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 2);
        assert_eq!(
            store.append_exec_fills("binance", "BTCUSDT", &[fill(3, 102.0)], None).unwrap(),
            1
        );
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 3);
    }

    #[test]
    fn exec_order_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![order(1, "ACCEPTED", 0.0)];
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_exec_orders("binance", "BTCUSDT").unwrap().len(), 1);
    }

    /// The unaddressable-partition guard, the per-symbol twin of `append_book_updates_grouped`'s:
    /// an empty `symbol` argument would commit order rows under a `symbol=` leaf that
    /// `parse_series_id` reads back as the GROUPED-series sentinel
    /// (`crates/vike-data/src/series.rs`'s `SeriesId::group`), i.e. a durable row that the store's
    /// own inventory cannot tell from a grouped series and that reconciliation therefore never
    /// learns about. `append_exec_orders` refuses the whole batch instead.
    ///
    /// Both halves matter: the refusal AND the control below proving a real symbol still appends,
    /// so a guard that had degraded into a blanket refusal would fail here rather than pass.
    #[test]
    fn exec_order_append_refuses_an_empty_symbol_partition() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let rows = vec![order(1, "ACCEPTED", 0.0)];

        // (1) REFUSED, and the error names both the verb and the venue — an operator reading a
        // materializer failure has to be able to find the call that produced it.
        let msg =
            store.append_exec_orders("binance", "", &rows, Some("k")).unwrap_err().to_string();
        assert!(msg.contains("append_exec_orders"), "names the verb: {msg}");
        assert!(msg.contains("binance"), "names the venue it was called for: {msg}");

        // (2) NOTHING durable was written — not the rows, and not the leaf directory itself
        // (`SeriesLock::acquire` create_dir_all's it, so a guard placed any deeper would still
        // leave `symbol=` on disk).
        let leaf = dir.path().join("kind=exec_order").join("venue=binance").join("symbol=");
        assert!(!leaf.exists(), "no `symbol=` leaf on disk: {}", leaf.display());
        assert!(store.list_series().unwrap().is_empty(), "the refused append left no series");

        // (3) An EMPTY batch under an empty symbol is refused too: the guard is on the ARGUMENT,
        // and `commit_rows` creates the leaf before it can notice a batch has no rows.
        assert!(store.append_exec_orders("binance", "", &[], None).is_err());

        // (4) CONTROL: a real symbol still appends and still reads back — including under the SAME
        // commit key, since the refusal never reached the manifest that registers one.
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.scan_exec_orders("binance", "BTCUSDT").unwrap(), rows);
    }

    /// The namespace guard: account fills (`kind=exec_fill`) and a symbol's MARKET prints
    /// (`kind=trade`) share `(venue, symbol)` but live under distinct `kind=` roots — neither leaks
    /// into the other's scan.
    #[test]
    fn exec_fills_do_not_collide_with_market_trades() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();

        store.append_exec_fills("binance", "BTCUSDT", &[fill(1, 100.0)], Some("f")).unwrap();
        let market = TradeTick {
            ts: 1,
            local_ts: 0,
            price: 100.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTCUSDT".to_string(),
        };
        store
            .append_trades("binance", "BTCUSDT", std::slice::from_ref(&market), Some("t"))
            .unwrap();

        // account fill is NOT visible as a market trade print …
        assert_eq!(store.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap(), vec![market]);
        // … and the market print is NOT visible as an account fill.
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap(), vec![fill(1, 100.0)]);
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match, over the `"exec_fill"` /
    /// `"exec_order"` arms.
    ///
    /// ⚠ **The symptom of a missing arm is NOT an error.** This doc said the first
    /// `run_maintenance()` over exec data "errors the WHOLE store" — true before
    /// `DataFusionHist::run_maintenance` grew per-series isolation, and stale for every kind since.
    /// What actually happens, measured through the `"cohort"` arm on the CI box and pinned generally by
    /// `crates/vike-data/tests/hist_datafusion.rs`'s
    /// `one_broken_series_does_not_abort_maintenance_for_the_others`:
    /// `compact_roundtrip` returns its `unknown series kind` `Err`, `run_maintenance` catches it PER
    /// SERIES, pushes a `MaintenanceReport::failed` row, logs one `warn!` and returns **`Ok`**.
    ///
    /// That is WORSE than the old claim, not milder. A store-wide `Err` stops the pass loudly and
    /// at once; this reports success while the exec series are never compacted again — the "parts
    /// piling up forever" failure the isolation's own comment describes, visible only to somebody
    /// who reads `report.failed` or greps a log. So both halves are asserted below: the part count
    /// says the work did not happen, the `failed` row says why.
    #[test]
    fn run_maintenance_handles_exec_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // One fill with mark_price/liquidity_side/commission_asset all surfaced, one with none of
        // them — the compaction decode→re-encode round-trip (`ExecFillCodec` via
        // `compact_roundtrip_generic`) must preserve both states of all three columns.
        let mut not_surfaced = fill(2, 101.0);
        not_surfaced.mark_price = None;
        not_surfaced.liquidity_side = String::new();
        not_surfaced.commission_asset = String::new();
        let fills = vec![fill(1, 100.0), not_surfaced];
        store.append_exec_fills("binance", "BTCUSDT", &fills, Some("kf")).unwrap();
        store
            .append_exec_orders("binance", "BTCUSDT", &[order(1, "FILLED", 1.0)], Some("ko"))
            .unwrap();

        let cfg = MaintenanceConfig {
            compaction: CompactionConfig {
                target_bytes: 1 << 20,
                min_parts: 1,
                ..Default::default()
            },
            retention: None,
        };
        let report = store.run_maintenance(&cfg).unwrap();
        assert!(
            report.failed.is_empty(),
            "an exec series was SKIPPED by maintenance, and the pass still returned Ok: {:?}",
            report.failed
        );
        assert_eq!(report.compaction.parts_written, 2, "both exec series were compacted");

        // data survives the compaction round-trip (venue+symbol preserved through the ctx="" re-encode),
        // including the nullable mark_price column and the two additive string columns (surfaced AND
        // not-surfaced states).
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap(), fills);
        assert_eq!(
            store.scan_exec_orders("binance", "BTCUSDT").unwrap(),
            vec![order(1, "FILLED", 1.0)]
        );
    }
}

#[cfg(feature = "test-support")]
mod mem_tests {
    use super::{fill, order};
    use vike_data::{HistStore, MemHistStore};

    #[test]
    fn exec_series_roundtrips_mem() {
        let store = MemHistStore::default();
        let fills = vec![fill(1, 100.0), fill(2, 101.0)];
        store.append_exec_fills("binance", "BTCUSDT", &fills, None).unwrap();
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap(), fills);

        let orders = vec![order(1, "ACCEPTED", 0.0)];
        store.append_exec_orders("binance", "BTCUSDT", &orders, None).unwrap();
        assert_eq!(store.scan_exec_orders("binance", "BTCUSDT").unwrap(), orders);
    }

    #[test]
    fn exec_series_commit_key_is_idempotent_mem() {
        let store = MemHistStore::default();
        let fills = vec![fill(1, 100.0)];
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &fills, Some("k")).unwrap(), 1);
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &fills, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 1);
    }
}
