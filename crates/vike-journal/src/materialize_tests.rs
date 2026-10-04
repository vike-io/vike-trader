use super::*;
use vike_data::MemHistStore;
use vike_exec::lanes::Ingest;
use vike_model::events::{Event, FillEvent, TradeId};

fn fill(trade_id: &'static str, coid: &str, symbol: &str, qty: f64, px: f64, ts: i64) -> Ingest {
    Ingest::Event(Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.1,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }))
}

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

use vike_model::events::{OrderModified, OrderSubmitted};

fn submitted_ev(coid: &str, ts: i64) -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts })
}

fn accepted_ev(coid: &str, voi: Option<&str>, ts: i64) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: voi.map(Into::into),
        ts,
    })
}

fn modified_ev(coid: &str, qty: Option<f64>, px: Option<f64>, ts: i64) -> Event {
    Event::OrderModified(OrderModified {
        client_order_id: coid.to_string(),
        venue_order_id: None,
        new_qty: qty,
        new_price: px,
        ts,
    })
}

/// Drive one order from a Submit intent through the adapter's own `OrderSubmitted` to ACCEPTED
/// — the real WAL prefix for every live order ("Submitted → REST → Accepted|Rejected"). Every
/// test below that wants a MODIFIABLE order has to walk it, because the FSM's guards are now
/// the materializer's guards.
fn seed_accepted(tracker: &mut OrderTracker, req: OrderRequest, touched: &mut HashSet<String>) {
    let coid = req.client_order_id.clone();
    tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, touched);
    tracker.fold_event(&submitted_ev(&coid, 11), touched);
    tracker.fold_event(&accepted_ev(&coid, None, 12), touched);
}

fn stop_req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "stop".to_string(),
        price: None,
        trigger_price: Some(90.0),
        ..Default::default()
    }
}

fn limit_req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(100.0),
        trigger_price: None,
        ..Default::default()
    }
}

/// Regression: an `OrderModified` on a STOP order must write its new price to the durable
/// `trigger_price` column (what the order rests on), not `price`. This used to be a hand-copied
/// `modified_price_is_trigger` branch in this module; it is now simply what the FSM did.
#[test]
fn stop_order_modify_routes_new_price_to_trigger_column() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    // STOP order resting on trigger 90.0, no limit price.
    seed_accepted(&mut tracker, stop_req("s1"), &mut touched);
    tracker.fold_event(&modified_ev("s1", None, Some(95.0), 20), &mut touched);
    let stop_row = exec_order_row(&tracker.rows["s1"]);
    assert_eq!(stop_row.trigger_price, Some(95.0), "stop modify updates the trigger column");
    assert_eq!(stop_row.price, None, "stop modify must NOT write the limit-price column");

    // Control: a LIMIT order's modify routes to the limit price, leaving trigger untouched.
    seed_accepted(&mut tracker, limit_req("l1"), &mut touched);
    tracker.fold_event(&modified_ev("l1", None, Some(105.0), 20), &mut touched);
    let limit_row = exec_order_row(&tracker.rows["l1"]);
    assert_eq!(limit_row.price, Some(105.0), "limit modify updates the limit-price column");
    assert_eq!(limit_row.trigger_price, None, "limit modify must NOT write the trigger column");
}

/// THE DIVERGENCE THIS FOLD EXISTS TO CLOSE.
///
/// The WAL is WRITE-AHEAD, so it carries events the live FSM went on to REFUSE. The reachable
/// case: a venue amend-ack lands AFTER the fill that terminalized the order (the amend is a
/// REST round trip — e.g. `BinancePerpRest::modify_order`'s `PUT /fapi/v1/order`, whose `Ok`
/// arm emits `OrderModified` — while the fill is a one-hop user-WS push; both are pushed onto
/// the same ingest lane and journaled in arrival order).
///
/// `ManagedOrder::apply` refuses it — `OrderStatus::MODIFIABLE` is {ACCEPTED, TRIGGERED,
/// PARTIALLY_FILLED} and the order is FILLED — so `ExecutionEngine::on_event` returns and the
/// live order kept qty 1.0 @ 100.0. The old unguarded copy applied it and appended an
/// `exec_order` row saying qty 5.0 @ 123.0. Now the materializer gives the FSM's answer.
#[test]
fn modify_after_terminal_is_refused_exactly_as_the_live_fsm_refuses_it() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    seed_accepted(&mut tracker, limit_req("c1"), &mut touched);
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            client_order_id: "c1".to_string(),
            fill: fill_ev("c1", "BTCUSDT", 1.0, 100.0, 13),
            ts: 13,
        }),
        &mut touched,
    );
    let filled = exec_order_row(&tracker.rows["c1"]);
    assert_eq!(filled.status, "FILLED");

    // The late amend-ack: qty 1.0 -> 5.0, price 100.0 -> 123.0. The live FSM dropped it.
    touched.clear();
    tracker.fold_event(&modified_ev("c1", Some(5.0), Some(123.0), 20), &mut touched);

    let after = exec_order_row(&tracker.rows["c1"]);
    assert_eq!(after.qty, 1.0, "a terminal order's qty is NOT rewritten (old copy wrote 5.0)");
    assert_eq!(
        after.price,
        Some(100.0),
        "a terminal order's price is NOT rewritten (old copy wrote 123.0)"
    );
    assert_eq!(after.status, "FILLED", "status untouched");
    assert_eq!(after.ts, filled.ts, "a refused event must not advance the durable row clock");
    assert!(
        touched.is_empty(),
        "a refused event marks nothing touched — no exec_order row is appended for it"
    );
    assert_eq!(tracker.dropped_invalid, 1, "the refusal is counted, not silently absorbed");
}

/// The same guard, on the other axis: a fill wrap arriving on an already-CANCELED order (the
/// cancel-vs-fill race the engine warns about as `stranded_terminal_drops`). The FSM refuses it
/// — `OrderFilled` is legal only from {ACCEPTED, TRIGGERED, PARTIALLY_FILLED} — so the durable
/// row must NOT flip to FILLED nor accumulate the qty. The bare `Event::Fill` for the same
/// execution still materializes into `exec_fill` on its own lane; only the ORDER snapshot is
/// held to what the engine actually folded.
#[test]
fn fill_wrap_after_terminal_neither_flips_status_nor_accumulates_qty() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    seed_accepted(&mut tracker, limit_req("c2"), &mut touched);
    tracker.fold_event(
        &Event::OrderCanceled(OrderCanceled {
            client_order_id: "c2".to_string(),
            reason: String::new().into(),
            ts: 20,
        }),
        &mut touched,
    );
    touched.clear();
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            client_order_id: "c2".to_string(),
            fill: fill_ev("c2", "BTCUSDT", 1.0, 100.0, 21),
            ts: 21,
        }),
        &mut touched,
    );

    let row = exec_order_row(&tracker.rows["c2"]);
    assert_eq!(row.status, "CANCELED", "the canceled order does NOT become FILLED");
    assert_eq!(row.filled_qty, 0.0, "no qty accumulated onto a terminal order");
    assert!(touched.is_empty(), "nothing touched, so no fabricated snapshot row");
    assert_eq!(tracker.dropped_invalid, 1);
}

/// A coid first seen via a bare lifecycle event — the post-restart tail whose submit/accept sits
/// below the checkpoint — is ADOPTED into the state that event's allowed-from set requires, so
/// the tail still terminalizes instead of stalling at INITIALIZED. The adoption is one-shot:
/// the NEXT event is guarded like any other.
#[test]
fn bare_first_sighting_is_adopted_then_guarded_like_any_other_order() {
    let mut tracker = OrderTracker::default();
    let mut touched = HashSet::new();

    // No submit, no MintedSubmit: the first record for this coid is a fill wrap.
    tracker.fold_event(
        &Event::OrderFilled(OrderFilled {
            client_order_id: "orphan".to_string(),
            fill: fill_ev("orphan", "ETHUSDT", 2.0, 50.0, 30),
            ts: 30,
        }),
        &mut touched,
    );
    let row = exec_order_row(&tracker.rows["orphan"]);
    assert_eq!(row.status, "FILLED", "adopted at ACCEPTED, so the fill wrap applies");
    assert_eq!(row.symbol, "ETHUSDT", "partition key learned from the fill");
    assert_eq!(row.filled_qty, 2.0);
    assert_eq!(tracker.dropped_invalid, 0, "the adopting event is never a refusal");

    // …and the adoption does not disable the guards: a modify on the now-terminal order is
    // refused exactly as it is for a fully-seeded order.
    touched.clear();
    tracker.fold_event(&modified_ev("orphan", Some(9.0), None, 31), &mut touched);
    assert_eq!(exec_order_row(&tracker.rows["orphan"]).qty, 0.0, "terms not rewritten");
    assert_eq!(tracker.dropped_invalid, 1);
}

use vike_model::events::{OrderAccepted, OrderCanceled, OrderFilled, OrderPartiallyFilled};

fn submit(coid: &str, venue: &str, symbol: &str, side: i32, qty: f64) -> Ingest {
    Ingest::Command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side,
        qty,
        order_type: "limit".to_string(),
        price: Some(100.0),
        ..Default::default()
    }))))
}

fn fill_ev(coid: &str, symbol: &str, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        // minted by this helper — same `tr-<ts>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("tr-", ts),
        client_order_id: coid.to_string(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn accepted(coid: &str, voi: &str, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(voi.into()),
        ts,
    }))
}

/// The adapter's own `OrderSubmitted`, emitted BEFORE the venue round trip and journaled on the
/// ingest lane like every other venue event ("Submitted → REST → Accepted|Rejected" — the
/// emitter-split contract `bridge_conformance.rs` machine-checks for every covered venue).
///
/// The WAL-writing tests below carry it because the REAL WAL carries it, and folding through
/// the FSM means it is now load-bearing: `OrderAccepted` is legal only from SUBMITTED. A venue
/// that skipped it would have its accept dropped by the LIVE engine too — which is exactly the
/// agreement this fold buys.
fn submitted(coid: &str, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts }))
}

fn order_filled(coid: &str, f: FillEvent, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderFilled(OrderFilled {
        client_order_id: coid.to_string(),
        fill: f,
        ts,
    }))
}

fn order_partial(coid: &str, f: FillEvent, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: coid.to_string(),
        fill: f,
        ts,
    }))
}

fn canceled(coid: &str, ts: i64) -> Ingest {
    Ingest::Event(Event::OrderCanceled(OrderCanceled {
        client_order_id: coid.to_string(),
        reason: String::new().into(),
        ts,
    }))
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

use vike_model::events::{
    OrderDenied, OrderExpired, OrderLiquidated, OrderRejected, OrderTriggered,
};

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

/// `fold_event`'s match ends in `_ => return`, so DELETING one of its arms compiles clean and
/// silently stops folding that event — the durable row simply keeps whatever status it had.
/// A mutation sweep found five arms no test pinned; this pins the two that matter, and the
/// other three ride along because a table costs nothing to widen.
///
/// Why these two are worth a test and the rest are not:
///
/// - `OrderRejected` is half of every submit's outcome ("Submitted → REST → Accepted|Rejected",
///   the WAL prefix `seed_accepted` walks). Unfolded, the durable row rests at SUBMITTED
///   forever for an order the venue refused outright.
/// - `OrderExpired` leaves the row at ACCEPTED permanently, and that one is not merely stale:
///   `recon/diff.rs` asks the journal whether an order is live, so an expired order that still
///   reads ACCEPTED manufactures a `JournalDivergence` — the one divergence kind held for
///   operator confirm before the policy is even consulted. A silent fold gap becomes a
///   quarantine an operator has to clear by hand.
///
/// `OrderTriggered`/`OrderLiquidated`/`OrderDenied` are asserted here only because they share
/// the loop. Do not read this test as a claim that their inputs are reachable in this seam.
#[test]
fn every_lifecycle_event_folds_into_the_durable_status() {
    // Each arm gets its OWN baseline, because the FSM's entry states differ per event and a
    // single shared prefix would silently test nothing: `transition_for` admits
    // `OrderRejected` only from {Initialized, Submitted} and `OrderDenied` only from
    // {Initialized}, so seeding everything to ACCEPTED would have `apply` REFUSE those two,
    // leave the row at ACCEPTED, and pass just as happily with the fold arm deleted.
    //
    // (coid, seed depth, event, expected durable status).
    let cases: &[FoldCase] = &[
        (
            "rejected",
            Seed::Submitted,
            |c, ts| {
                Event::OrderRejected(OrderRejected {
                    client_order_id: c.to_string(),
                    reason: "insufficient margin".into(),
                    ts,
                })
            },
            "REJECTED",
        ),
        (
            "expired",
            Seed::Accepted,
            |c, ts| Event::OrderExpired(OrderExpired { client_order_id: c.to_string(), ts }),
            "EXPIRED",
        ),
        (
            "triggered",
            Seed::AcceptedStop,
            |c, ts| Event::OrderTriggered(OrderTriggered { client_order_id: c.to_string(), ts }),
            "TRIGGERED",
        ),
        (
            "liquidated",
            Seed::Accepted,
            |c, ts| {
                Event::OrderLiquidated(OrderLiquidated {
                    client_order_id: c.to_string(),
                    liq_price: 88.0,
                    ts,
                })
            },
            "LIQUIDATED",
        ),
        (
            "denied",
            Seed::Initialized,
            |c, ts| {
                Event::OrderDenied(OrderDenied {
                    client_order_id: c.to_string(),
                    reason: "risk gate".into(),
                    ts,
                })
            },
            "DENIED",
        ),
    ];

    for (coid, seed, make, expected) in cases {
        let mut tracker = OrderTracker::default();
        let mut touched = HashSet::new();

        let req = if matches!(seed, Seed::AcceptedStop) { stop_req(coid) } else { limit_req(coid) };
        tracker.seed_from_intent(&OrderIntent::Submit(Box::new(req)), 10, &mut touched);
        if !matches!(seed, Seed::Initialized) {
            tracker.fold_event(&submitted_ev(coid, 11), &mut touched);
        }
        if matches!(seed, Seed::Accepted | Seed::AcceptedStop) {
            tracker.fold_event(&accepted_ev(coid, None, 12), &mut touched);
        }
        let baseline = exec_order_row(&tracker.rows[*coid]).status;
        assert_eq!(
            baseline,
            seed.status(),
            "{coid}: the baseline itself must be the state this event is admitted FROM — \
                 otherwise `apply` refuses and the assertion below proves nothing"
        );

        touched.clear();
        tracker.fold_event(&make(coid, 30), &mut touched);

        assert_eq!(
            exec_order_row(&tracker.rows[*coid]).status,
            *expected,
            "{coid}: the durable row must carry the FSM's status after the fold — a deleted \
                 `fold_event` arm leaves it at {baseline} and says nothing"
        );
        assert!(
            touched.contains(*coid),
            "{coid}: a folded event must mark the row dirty, or it never reaches the store"
        );
    }
}

/// One row of the fold table: the order's coid, how far its baseline is driven, the event to
/// fold, and the durable status that must come out.
type FoldCase = (&'static str, Seed, fn(&str, i64) -> Event, &'static str);

/// How far down the WAL prefix a case's baseline order is driven before the event under test.
#[derive(Clone, Copy)]
enum Seed {
    Initialized,
    Submitted,
    Accepted,
    /// ACCEPTED, but seeded from a STOP request — the only shape `OrderTriggered` applies to.
    AcceptedStop,
}

impl Seed {
    fn status(self) -> &'static str {
        match self {
            Seed::Initialized => "INITIALIZED",
            Seed::Submitted => "SUBMITTED",
            Seed::Accepted | Seed::AcceptedStop => "ACCEPTED",
        }
    }
}
