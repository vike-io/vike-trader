use super::*;
use crate::materialize::testkit::{
    accepted, canceled, fill, fill_ev, order_filled, order_partial, submit, submitted,
};
use vike_data::MemHistStore;
use vike_model::OrderRequest;

/// Write a WAL with N fills, drain it once, assert Tier-2 holds exactly those fills; a second
/// drain (no new records) is a no-op; a re-drain of the SAME window never dups.
#[test]
fn materializes_wal_fills_into_tier2_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    // write a journal with 3 fills
    {
        let mut j =
            crate::CommandJournal::open(dir.path(), crate::JournalFileConfig::default()).unwrap();
        j.append_cmd(1, &fill("t1", "c1", "BTCUSDT", 0.5, 100.0, 10)).unwrap();
        j.append_cmd(2, &fill("t2", "c1", "BTCUSDT", 0.5, 101.0, 20)).unwrap();
        j.append_cmd(3, &fill("t3", "c2", "ETHUSDT", 1.0, 50.0, 30)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut orders = OrderTracker::default();
    // cold start (no checkpoint) reads the whole journal, INCLUDING the seq-0 record.
    materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

    let btc = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
    let eth = store.scan_exec_fills("binance", "ETHUSDT").unwrap();
    assert_eq!(btc.len(), 2, "two BTC fills materialized");
    assert_eq!(eth.len(), 1, "one ETH fill materialized");
    assert_eq!(btc[0].trade_id, "t1", "the seq-0 fill is NOT dropped on cold start");
    assert_eq!(eth[0].symbol, "ETHUSDT");
    assert_eq!(btc[0].liquidity_side, "taker", "LiquiditySide threaded through as its wire string");
    assert_eq!(btc[0].commission_asset, "", "empty Ustr threads through as an empty string");
    assert!(seq.load(Ordering::Relaxed) >= 2, "checkpoint advanced to the max seq read");

    // second drain: no new records past the checkpoint → no-op, no growth
    materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();
    assert_eq!(store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(), 2, "no re-append");

    // crash-before-checkpoint: lose the checkpoint, re-drain cold over the SAME window → the
    // per-window commit_key makes the re-append a store no-op (at-least-once is safe).
    std::fs::remove_file(dir.path().join("materializer.ckpt")).unwrap();
    materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut OrderTracker::default()).unwrap();
    assert_eq!(
        store.scan_exec_fills("binance", "BTCUSDT").unwrap().len(),
        2,
        "re-drain after a lost checkpoint is idempotent (commit_key)"
    );
}

/// Order lifecycle materializes into `exec_order` snapshots: an explicit-coid order folds
/// submit → OrderSubmitted → accept → partial → fill (symbol from the submit, qty accumulated);
/// a server-minted (empty-coid) order resolves its symbol from the fill; and a
/// symbol-unresolvable order (empty coid, canceled without ever filling) is skipped, not
/// persisted.
///
/// The adapter's own `OrderSubmitted` records are in the WAL here because they are in the real
/// WAL (a venue emits `OrderSubmitted` BEFORE the REST round trip), and folding through the FSM
/// makes them load-bearing: `OrderAccepted` is legal only from SUBMITTED. Every status string
/// asserted below is `OrderStatus::as_str()`, unchanged from the hand-written ones it replaced.
#[test]
fn materializes_order_lifecycle_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j =
            crate::CommandJournal::open(dir.path(), crate::JournalFileConfig::default()).unwrap();
        // explicit-coid order c1: submit(limit BTCUSDT 1.0) → OrderSubmitted → accept →
        // partial 0.3@100 → fill 0.7@110
        j.append_cmd(1, &submit("c1", "binance", "BTCUSDT", 1, 1.0)).unwrap();
        j.append_cmd(2, &submitted("c1", 10)).unwrap();
        j.append_cmd(2, &accepted("c1", "v1", 11)).unwrap();
        j.append_cmd(3, &order_partial("c1", fill_ev("c1", "BTCUSDT", 0.3, 100.0, 12), 12))
            .unwrap();
        j.append_cmd(4, &order_filled("c1", fill_ev("c1", "BTCUSDT", 0.7, 110.0, 13), 13)).unwrap();
        // server-minted order cm: only an OrderFilled arrives (symbol resolved from the fill)
        j.append_cmd(5, &order_filled("cm", fill_ev("cm", "ETHUSDT", 2.0, 50.0, 20), 20)).unwrap();
        // server-minted order m1 that terminalizes with NO fill (the minted-coid gap case):
        // the empty-coid Submit write-ahead (skipped) → a MintedSubmit carrying the RESOLVED
        // request (minted coid m1, SOLUSDT limit) → accept → cancel. It must now be PERSISTED
        // with its symbol/terms from the MintedSubmit and its terminal status CANCELED.
        j.append_cmd(6, &submit("", "binance", "SOLUSDT", 1, 1.0)).unwrap();
        j.append_minted_submit(
            7,
            &OrderRequest {
                client_order_id: "m1".to_string(),
                venue: "binance".to_string(),
                symbol: "SOLUSDT".to_string(),
                side: 1,
                qty: 1.0,
                order_type: "limit".to_string(),
                price: Some(100.0),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        j.append_cmd(8, &submitted("m1", 30)).unwrap();
        j.append_cmd(8, &accepted("m1", "vm1", 31)).unwrap();
        j.append_cmd(9, &canceled("m1", 32)).unwrap();
        // truly symbol-unresolvable order: a canceled event on a coid with no submit, no
        // MintedSubmit and no fill — still unlearnable, still skipped.
        j.append_cmd(10, &canceled("cx", 40)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut orders = OrderTracker::default();
    materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

    // c1: latest snapshot is FILLED, symbol/side/type from the submit, qty accumulated 0.3+0.7,
    // avg_fill_px qty-weighted (0.3*100 + 0.7*110)/1.0 = 107, venue_order_id from the accept.
    let btc = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
    let last = btc.last().expect("c1 order materialized");
    assert_eq!(last.client_order_id, "c1");
    assert_eq!(last.status, "FILLED");
    assert_eq!(last.order_type, "limit");
    assert!((last.filled_qty - 1.0).abs() < 1e-9, "0.3 + 0.7 accumulated");
    assert!((last.avg_fill_px - 107.0).abs() < 1e-9, "qty-weighted avg");
    assert_eq!(last.venue_order_id.as_deref(), Some("v1"));

    // cm: symbol resolved from the fill, status FILLED
    let eth = store.scan_exec_orders("binance", "ETHUSDT").unwrap();
    assert_eq!(eth.last().unwrap().status, "FILLED");
    assert_eq!(eth.last().unwrap().client_order_id, "cm");

    // server-minted, never-filled order m1: NOW persisted under SOLUSDT (minted-coid gap fix) —
    // symbol/side/type from the MintedSubmit, terminal status CANCELED, venue_order_id from the
    // accept. This is exactly the case the module doc used to say was dropped.
    let sol = store.scan_exec_orders("binance", "SOLUSDT").unwrap();
    let m1 = sol.last().expect("server-minted no-fill order m1 is persisted");
    assert_eq!(m1.client_order_id, "m1");
    assert_eq!(m1.status, "CANCELED");
    assert_eq!(m1.symbol, "SOLUSDT");
    assert_eq!(m1.order_type, "limit");
    assert_eq!(m1.side, 1);
    assert!((m1.qty - 1.0).abs() < 1e-9);
    assert_eq!(m1.price, Some(100.0));
    assert_eq!(m1.venue_order_id.as_deref(), Some("vm1"));
    assert!((m1.filled_qty - 0.0).abs() < 1e-9, "m1 never filled");

    // truly symbol-unresolvable order cx: still never persisted (counted skipped)
    assert!(orders.skipped_unresolved >= 1, "the symbol-less canceled coid was skipped");

    // the recon read helper collapses to latest-status-per-coid
    let statuses =
        vike_data::exec_index::recent_order_statuses(&store, "binance", "BTCUSDT", 0).unwrap();
    assert_eq!(statuses.get("c1").map(String::as_str), Some("FILLED"));

    // and the recon read path now sees the server-minted, never-filled order m1 too — the
    // JournalView.orders visibility the minted-coid gap used to deny.
    let sol_statuses =
        vike_data::exec_index::recent_order_statuses(&store, "binance", "SOLUSDT", 0).unwrap();
    assert_eq!(sol_statuses.get("m1").map(String::as_str), Some("CANCELED"));

    // idempotent re-drain over the same window (lost checkpoint) → no duplicate order rows
    let btc_before = store.scan_exec_orders("binance", "BTCUSDT").unwrap().len();
    std::fs::remove_file(dir.path().join("materializer.ckpt")).unwrap();
    materialize_once(dir.path(), &store, &AtomicU64::new(0), &mut OrderTracker::default()).unwrap();
    assert_eq!(
        store.scan_exec_orders("binance", "BTCUSDT").unwrap().len(),
        btc_before,
        "re-drain is idempotent for orders too (per-window commit_key)"
    );
}

/// The Bracket-arm counterpart of `materializes_order_lifecycle_snapshots`: entry/SL/TP coids
/// are ALL minted inline in `apply_intent`'s `OrderIntent::Bracket` arm, never through
/// `Submit`/`SubmitBatch` — so before that arm gained its own `MintedSubmit` append, none of
/// the three legs had a symbol-resolving record other than a fill, and a leg that terminalized
/// WITHOUT ever filling (exactly the OCO-sibling-fills-so-I-get-canceled case a bracket exists
/// for) was silently dropped, same as the original Submit-arm gap. Here the SL leg is accepted
/// then canceled with no fill ever arriving; it must still be persisted, terms/symbol resolved
/// from its `MintedSubmit`.
#[test]
fn bracket_leg_canceled_without_filling_is_materialized() {
    let dir = tempfile::tempdir().unwrap();
    let spec = vike_model::BracketSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 2.0,
        entry_price: Some(100.0),
        stop_loss: 95.0,
        take_profit: 110.0,
    };
    let [entry, sl, tp] = vike_model::build_bracket(&spec, "e1", "sl1", "tp1");
    {
        let mut j =
            crate::CommandJournal::open(dir.path(), crate::JournalFileConfig::default()).unwrap();
        // the three inline-minted legs, journaled exactly as apply_intent's Bracket arm now does
        j.append_minted_submit(1, &entry, None).unwrap();
        j.append_minted_submit(1, &sl, None).unwrap();
        j.append_minted_submit(1, &tp, None).unwrap();
        // SL is released to the venue (adapter's OrderSubmitted), accepted, then canceled (its
        // OCO sibling TP filled instead) — no fill event for SL ever arrives.
        j.append_cmd(2, &submitted("sl1", 10)).unwrap();
        j.append_cmd(2, &accepted("sl1", "vsl1", 11)).unwrap();
        j.append_cmd(3, &canceled("sl1", 12)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut orders = OrderTracker::default();
    materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

    let rows = store.scan_exec_orders("binance", "BTCUSDT").unwrap();
    let sl_row = rows
        .iter()
        .find(|r| r.client_order_id == "sl1")
        .expect("SL leg persisted from its MintedSubmit despite never filling");
    assert_eq!(sl_row.status, "CANCELED");
    assert_eq!(sl_row.symbol, "BTCUSDT");
    assert_eq!(sl_row.order_type, "stop");
    assert_eq!(sl_row.side, -1, "SL is the opposite side of the long entry");
    assert!((sl_row.qty - 2.0).abs() < 1e-9);
    assert_eq!(sl_row.trigger_price, Some(95.0));
    assert_eq!(sl_row.venue_order_id.as_deref(), Some("vsl1"));
    assert!((sl_row.filled_qty - 0.0).abs() < 1e-9, "SL never filled");

    // the entry and TP legs are ALSO persisted purely from their own MintedSubmit — the fix
    // closes the gap for every minted leg, not just the one under test. They sit at
    // INITIALIZED, not SUBMITTED: a leg with a registration record and no venue event yet is
    // EXACTLY what `gate_and_register` holds in the live registry (the adapter's own
    // `OrderSubmitted` is what advances it), and a held bracket leg has not been sent anywhere.
    let e1 = rows.iter().find(|r| r.client_order_id == "e1").expect("entry leg persisted too");
    let tp = rows.iter().find(|r| r.client_order_id == "tp1").expect("TP leg persisted too");
    assert_eq!(e1.status, "INITIALIZED");
    assert_eq!(tp.status, "INITIALIZED");
}

/// The skip guard is `symbol.is_empty() || venue.is_empty()`, and its `||` was pinned by
/// nothing: the existing `cx` case in `materializes_order_lifecycle_snapshots` leaves BOTH
/// fields empty, which `&&` skips just as happily.
///
/// The one-empty shape is not contrived — production MAKES it. `vike_model::orders::order`'s
/// `build_combo` sets `venue` from the spec and leaves `symbol` for the adapter to resolve, so
/// a combo leg that terminalizes before its symbol is known arrives here with venue set and
/// symbol empty. Under `&&` that row is not skipped: it is written into a partition keyed
/// `symbol=`, which `scan_exec_orders` can never address again. A durable row nothing can read
/// is worse than no row — recon asks this store what it knows.
#[test]
fn a_venue_without_a_symbol_is_skipped_not_written_to_an_unaddressable_partition() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j =
            crate::CommandJournal::open(dir.path(), crate::JournalFileConfig::default()).unwrap();
        // The `build_combo` shape: venue known, symbol left to the adapter, then terminal.
        j.append_cmd(1, &submit("cb1", "deribit", "", 1, 1.0)).unwrap();
        j.append_cmd(2, &submitted("cb1", 10)).unwrap();
        j.append_cmd(3, &accepted("cb1", "vcb1", 11)).unwrap();
        j.append_cmd(4, &canceled("cb1", 12)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut orders = OrderTracker::default();
    materialize_once(dir.path(), &store, &seq, &mut orders).unwrap();

    assert!(
        store.scan_exec_orders("deribit", "").unwrap().is_empty(),
        "a symbol-less row must not be written; that partition is unaddressable"
    );
    assert!(
        orders.skipped_unresolved >= 1,
        "and the skip must be COUNTED — that counter is the only observability this has"
    );
}

/// The FILL twin of the test above, and the one with teeth.
///
/// `append_exec_fills` refuses an empty symbol (#1311 — such a row is unattributable, and its
/// trade_ids would be missing from the reconcile seen-fill set, which turns a booked fill into
/// a `MissingFill` that `hybrid` auto-applies). That append is reached through `?`. So without
/// a skip in the fill loop, ONE unattributable fill fails the whole pass — every good fill and
/// order in the batch with it — and because the checkpoint never advances, the next pass reads
/// the same records and fails again. A permanently stuck materializer persists nothing.
///
/// The good rows in the SAME batch are the point of this test: it is not enough that the bad
/// row is dropped, the rest must still land.
#[test]
fn a_symbol_less_fill_is_skipped_without_failing_the_pass_for_the_good_rows() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut j =
            crate::CommandJournal::open(dir.path(), crate::JournalFileConfig::default()).unwrap();
        // One unattributable fill, and one perfectly good one in the same window.
        j.append_cmd(1, &fill("t-bad", "cx", "", 1.0, 100.0, 10)).unwrap();
        j.append_cmd(2, &fill("t-good", "c1", "BTCUSDT", 2.0, 200.0, 11)).unwrap();
        j.flush().unwrap();
    }
    let store = MemHistStore::new();
    let seq = AtomicU64::new(0);
    let mut orders = OrderTracker::default();

    // The pass must SUCCEED. Before the skip existed this was `Err` and nothing was persisted.
    materialize_once(dir.path(), &store, &seq, &mut orders)
        .expect("one unattributable fill must not fail the pass");

    let good = store.scan_exec_fills("binance", "BTCUSDT").unwrap();
    assert_eq!(good.len(), 1, "the attributable fill still lands: {good:?}");
    assert_eq!(good[0].trade_id, "t-good");
    assert!(
        store.scan_exec_fills("binance", "").unwrap().is_empty(),
        "and the unattributable one is not written to an unaddressable partition"
    );
    assert!(orders.skipped_unresolved >= 1, "the skip is counted, not silent");
}
