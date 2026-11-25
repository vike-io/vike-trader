//! `kind=exec_fill` / `kind=execution_order` HistoryStore series — the ACCOUNT fill/order trade-log (Tier-2),
//! NOT market prints. Mirrors the `kind=equity` precedent (`equity_series.rs`) with the two exec kinds.
//!
//! The namespace-guard tests are the point of this file: the store already holds `kind=trade` (market
//! trade ticks). These tests prove account fills (`kind=exec_fill`) and a symbol's market prints
//! (`kind=trade`) NEVER collide even when `(venue, symbol)` are identical — distinct `kind=` partition
//! roots. Split into `datafusion_tests` / `memory_tests` exactly like `equity_series.rs`.

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
        order_client_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 0.5,
        px,
        commission: 0.01,
        mark_price: Some(65_001.0),
        liquidity_side: "maker".to_string(),
        commission_asset: "BNB".to_string(),
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

/// One order-lifecycle literal: `ts`/`status`/`filled_qty` vary per call.
fn order(ts: i64, status: &str, filled_qty: f64) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        order_client_id: "c1".to_string(),
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
        account: None,
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    }
}

mod datafusion_tests {
    use super::{fill, order};
    use vike_data::{HistoryStore, TimestampRange};
    use vike_marketdata::TickTrade;
    use vike_store::{CompactionConfig, HistoryStoreDataFusion, MaintenanceConfig};

    /// **An exec append of a slashed symbol is refused, and leaves no series** (audit flag HL-1): hyperliquid's
    /// spot `PURR/USDC` was filed INSIDE `symbol=PURR`, where no listing, maintenance pass or reconcile walk
    /// finds it. Both exec verbs carry the guard every other write verb does.
    #[test]
    fn an_exec_append_of_a_slashed_symbol_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let fills = store.append_exec_fills("hyperliquid", "PURR/USDC", &[fill(1, 1.0)], Some("k"));
        let orders = store.append_exec_orders(
            "hyperliquid",
            "PURR/USDC",
            &[order(1, "NEW", 0.0)],
            Some("k"),
        );
        for refused in [fills, orders] {
            let text = refused.expect_err("a slash is no store key").to_string();
            assert!(text.contains("PURR/USDC") && text.contains('/'), "{text}");
        }
        assert!(store.list_series().unwrap().is_empty(), "nothing was written");
    }

    #[test]
    fn execution_fill_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
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
    fn execution_order_series_roundtrips_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        // Cover BOTH nullable states: an accepted (unfilled, market — None price) then a filled order.
        let mut open = order(1, "ACCEPTED", 0.0);
        open.price = None;
        open.venue_order_id = None;
        let rows = vec![open, order(2, "PARTIALLY_FILLED", 0.5), order(3, "FILLED", 1.0)];
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k1")).unwrap(), 3);

        let got = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
        assert_eq!(got, rows, "ts-ascending, nullable price/venue_order_id preserved");
    }

    /// The two additive id columns through the REAL store: a fill carrying the venue's order id
    /// and an account, one carrying neither (written NULL, read `None`), and an order's account.
    #[test]
    fn execution_venue_order_id_and_account_round_trip_datafusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let mut known = fill(1, 100.0);
        known.venue_order_id = Some("100234".to_string());
        known.account = Some("ALT".to_string());
        let fills = vec![known, fill(2, 101.0)];
        store.append_exec_fills("binance", "BTCUSDT", &fills, Some("f")).unwrap();
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap(), fills);

        let mut routed = order(1, "ACCEPTED", 0.0);
        routed.account = Some("ALT".to_string());
        let orders = vec![routed, order(2, "FILLED", 1.0)];
        store.append_exec_orders("binance", "BTCUSDT", &orders, Some("o")).unwrap();
        assert_eq!(store.scan_exec_orders("binance", "BTCUSDT").unwrap(), orders);
    }

    /// **Why a fee correction row carries its journal seq**, on the REAL store. Every correction of a
    /// fill is stamped with the fill's own time, so they tie on `ts`, and a read orders the parts by
    /// their `ts` span: a part that also holds a later fill reads AFTER one that holds the next
    /// correction alone, so the first correction reads last. The fold still takes the last one.
    #[test]
    fn the_last_fee_correction_wins_though_its_part_reads_first() {
        use vike_data::ExecFillRow;
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let correction = |extra: f64, seq: i64| ExecFillRow {
            trade_id: String::new(),
            qty: 0.0,
            commission: extra,
            corrects_trade_id: Some("t1000".to_string()),
            correction_seq: Some(seq),
            received_ms: Some(2_000 + seq),
            ..fill(1_000, 100.0)
        };
        let mut long = fill(1_000, 100.0);
        long.position_side = Some("LONG".to_string());
        let append = |rows: &[ExecFillRow], key: &str| {
            store.append_exec_fills("binance", "BTCUSDT", rows, Some(key)).unwrap();
        };
        append(&[long.clone()], "fill");
        append(&[correction(1.00, 5), fill(1_005, 101.0)], "pass-1");
        append(&[correction(1.25, 9)], "pass-2");

        let rows = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
        let read: Vec<i64> = rows.iter().filter_map(|r| r.correction_seq).collect();
        assert_eq!(read, [9, 5], "the store's order of a tie is not the journal's");
        let received: Vec<i64> = rows.iter().filter_map(|r| r.received_ms).collect();
        assert_eq!(received, [2_009, 2_005], "the receipt time round-trips through a real part");
        assert_eq!(rows[0], long, "the additive columns round-trip through a real part");

        let fills = ExecFillRow::fill_events(&rows);
        assert_eq!(fills.len(), 2, "a correction is no fill of its own");
        assert_eq!(fills[0].commission, 0.01 + 1.25);
        assert_eq!(fills[0].position_side, vike_model::events::PositionSide::Long);
        assert_eq!(fills[1].commission, 0.01);
    }

    /// `scan_execution_orders_head` keeps `scan_execution_fills_head`'s contract on the real block walk: a
    /// ts-ascending COMPLETE PREFIX of the rows in `range` — a range starting MID-part, timestamps
    /// TIED across order client ids, rows spread over several parts — holding at least `n` rows unless it is
    /// all of them, and never splitting a timestamp.
    #[test]
    fn execution_orders_head_is_a_complete_prefix_in_range() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        // three parts; ts 20 and 40 each carry two order client ids (ties)
        let at = |ts: i64, order_client_id: &str| {
            let mut o = order(ts, "ACCEPTED", 0.0);
            o.order_client_id = order_client_id.to_string();
            o
        };
        store
            .append_exec_orders(
                "binance",
                "BTCUSDT",
                &[at(10, "a"), at(20, "a"), at(20, "b")],
                None,
            )
            .unwrap();
        store.append_exec_orders("binance", "BTCUSDT", &[at(30, "c"), at(40, "c")], None).unwrap();
        store.append_exec_orders("binance", "BTCUSDT", &[at(40, "d"), at(50, "d")], None).unwrap();

        let all = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
        let range = TimestampRange { start: Some(15), end: Some(45) };
        let in_range: Vec<i64> =
            all.iter().map(|r| r.ts).filter(|ts| (15..=45).contains(ts)).collect();
        for n in 0..=in_range.len() + 1 {
            let head: Vec<i64> = store
                .scan_execution_orders_head("binance", "BTCUSDT", range, n)
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect();
            if n == 0 {
                assert!(head.is_empty(), "n == 0 asks for nothing");
                continue;
            }
            assert_eq!(head[..], in_range[..head.len()], "n={n}: a PREFIX of the range");
            assert!(head.len() >= n.min(in_range.len()), "n={n}: at least n rows: {head:?}");
            if let Some(last) = head.last() {
                let whole = in_range.iter().filter(|ts| *ts == last).count();
                let taken = head.iter().filter(|ts| *ts == last).count();
                assert_eq!(taken, whole, "n={n}: ts {last} is never split: {head:?}");
            }
        }
    }

    /// The fills twin of the range half: a head over a range starting MID-part answers only rows
    /// inside it.
    #[test]
    fn execution_fills_head_honours_a_range_starting_mid_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let rows: Vec<_> = (1..=6).map(|ts| fill(ts * 10, 100.0)).collect();
        store.append_exec_fills("binance", "BTCUSDT", &rows, None).unwrap();
        let range = TimestampRange { start: Some(25), end: Some(45) };
        let head = store.scan_execution_fills_head("binance", "BTCUSDT", range, 10).unwrap();
        assert_eq!(head.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![30, 40]);
    }

    #[test]
    fn execution_fill_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
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
    fn execution_order_commit_key_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let rows = vec![order(1, "ACCEPTED", 0.0)];
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 1);
        assert_eq!(store.append_exec_orders("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_exec_orders("binance", "BTCUSDT").unwrap().len(), 1);
    }

    /// The unaddressable-partition guard, the per-symbol twin of `append_book_updates_grouped`'s:
    /// an empty `symbol` argument would commit order rows under a `symbol=` leaf that
    /// `parse_series_id` reads back as the GROUPED-series sentinel
    /// (`crates/vike-data/src/store/series.rs`'s `SeriesId::group`), i.e. a durable row that the store's
    /// own inventory cannot tell from a grouped series and that reconciliation therefore never
    /// learns about. `append_exec_orders` refuses the whole batch instead.
    ///
    /// Both halves matter: the refusal AND the control below proving a real symbol still appends,
    /// so a guard that had degraded into a blanket refusal would fail here rather than pass.
    #[test]
    fn execution_order_append_refuses_an_empty_symbol_partition() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
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

    /// **HL-1**: the two exec verbs refuse a symbol the store cannot name, as the other writing verbs do. A
    /// hyperliquid spot `PURR/USDC` used to land under the PERP's leaf (`symbol=PURR/USDC/`), a series no walk
    /// names; nothing is written now, and the perp's own series still appends.
    #[test]
    fn the_exec_verbs_refuse_a_path_hostile_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
        let fills = store.append_exec_fills("hyperliquid", "PURR/USDC", &[fill(1, 1.0)], None);
        let orders = store.append_exec_orders(
            "hyperliquid",
            "PURR/USDC",
            &[order(1, "ACCEPTED", 0.0)],
            None,
        );
        for refused in [fills.unwrap_err().to_string(), orders.unwrap_err().to_string()] {
            assert!(refused.contains("PURR/USDC") && refused.contains("'/'"), "{refused}");
        }
        assert!(!dir.path().join("kind=exec_fill").join("venue=hyperliquid").exists());
        assert!(!dir.path().join("kind=exec_order").join("venue=hyperliquid").exists());
        assert_eq!(
            store.append_exec_fills("hyperliquid", "PURR", &[fill(1, 1.0)], None).unwrap(),
            1
        );
    }

    /// The namespace guard: account fills (`kind=exec_fill`) and a symbol's MARKET prints
    /// (`kind=trade`) share `(venue, symbol)` but live under distinct `kind=` roots — neither leaks
    /// into the other's scan.
    #[test]
    fn execution_fills_do_not_collide_with_market_trades() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();

        store.append_exec_fills("binance", "BTCUSDT", &[fill(1, 100.0)], Some("f")).unwrap();
        let market = TickTrade {
            timestamp: 1,
            local_timestamp: 0,
            price: 100.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTCUSDT".into(),
        };
        store
            .append_trades("binance", "BTCUSDT", std::slice::from_ref(&market), Some("t"))
            .unwrap();

        // account fill is NOT visible as a market trade print …
        assert_eq!(
            store.scan_trades("binance", "BTCUSDT", TimestampRange::all()).unwrap(),
            vec![market]
        );
        // … and the market print is NOT visible as an account fill.
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap(), vec![fill(1, 100.0)]);
    }

    /// Regression guard for the hardcoded `compact_roundtrip` kind match, over the `"exec_fill"` /
    /// `"exec_order"` arms.
    ///
    /// ⚠ HISTORY: the compaction dispatch now matches a `StoreKindId` exhaustively, so a missing arm
    /// is a COMPILE error; this paragraph records what one cost while it could still happen.
    ///
    /// ⚠ **The symptom of a missing arm is NOT an error.** This doc said the first
    /// `run_maintenance()` over exec data "errors the WHOLE store" — true before
    /// `HistoryStoreDataFusion::run_maintenance` grew per-series isolation, and stale for every kind since.
    /// What actually happens, measured through the `"cohort"` arm on the CI box and pinned generally by
    /// `crates/vike-store/tests/store/compaction.rs`'s
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
    fn run_maintenance_handles_execution_series() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
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

        let config = MaintenanceConfig {
            compaction: CompactionConfig {
                target_bytes: 1 << 20,
                min_parts: 1,
                ..Default::default()
            },
            retention: None,
            retention_by_kind: Default::default(),
        };
        let report = store.run_maintenance(&config).unwrap();
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

mod memory_tests {
    use super::{fill, order};
    use vike_data::{HistoryStore, HistoryStoreMemory};

    #[test]
    fn execution_series_roundtrips_memory() {
        let store = HistoryStoreMemory::default();
        let fills = vec![fill(1, 100.0), fill(2, 101.0)];
        store.append_exec_fills("binance", "BTCUSDT", &fills, None).unwrap();
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap(), fills);

        let orders = vec![order(1, "ACCEPTED", 0.0)];
        store.append_exec_orders("binance", "BTCUSDT", &orders, None).unwrap();
        assert_eq!(store.scan_exec_orders("binance", "BTCUSDT").unwrap(), orders);
    }

    #[test]
    fn execution_series_commit_key_is_idempotent_memory() {
        let store = HistoryStoreMemory::default();
        let fills = vec![fill(1, 100.0)];
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &fills, Some("k")).unwrap(), 1);
        assert_eq!(store.append_exec_fills("binance", "BTCUSDT", &fills, Some("k")).unwrap(), 0);
        assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 1);
    }
}
