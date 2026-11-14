//! The EMULATED orders' working kinds (`emulated.rs`, decision 0275) on the real `lower_submit`, the
//! boundary drive, the two order-outcome lanes and the restore: a pegged order and a visible size the venue
//! does not hold are armed (never sent), worked through one plain limit at a time, heard as ONE order by
//! their owner, refused whole over a ceiling, never amended, and re-adopted (never cancelled) after a
//! restart. White-box over a `RecordingClient` on bybit (no pegged kind, no visible size, a native limit
//! amend). The simulator runs the same law: `crates/vike-simulator/tests/laws/emulated_orders.rs`.

use std::sync::{Arc, Mutex};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_model::events::{FillEvent, OrderAccepted, OrderSubmitted, PositionSide, TradeId};
use vike_model::{OrderEvent, OrderEventKind, OrderPeg, OrderPegAnchor, OrderType};

use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;

const V: &str = "bybit";
const SYM: &str = "BTCUSDT";
const NOW: i64 = 1_000;

fn core() -> Core {
    core_of(engine_on(V, SYM, RecordingClient::default()), Vec::new(), CoreConfig::default())
}

fn pegged(qty: f64) -> OrderRequest {
    OrderRequest {
        venue: V.into(),
        symbol: SYM.into(),
        side: 1,
        qty,
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 }),
        ..Default::default()
    }
}

fn shown(qty: f64, price: f64, display: f64) -> OrderRequest {
    OrderRequest {
        venue: V.into(),
        symbol: SYM.into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        display_quantity: Some(display),
        ..Default::default()
    }
}

fn submit(c: &mut Core, req: OrderRequest) -> String {
    c.apply_intent(OrderIntent::Submit(Box::new(req)), NOW).pop().expect("an order client id")
}

/// A fresh touch on the board, then one boundary pass.
fn touch(c: &mut Core, ts: i64, bid: f64, ask: f64) {
    c.engine.price_board.set_quote(V, SYM, bid, ask, ts);
    c.drive_emulated(ts);
}

/// The venue's `[OrderSubmitted, OrderAccepted]` for `order_client_id`, folded and delivered (the run loop's `handle`).
fn accept(c: &mut Core, order_client_id: &str) {
    let _ = c.handle(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: 0,
    })));
    let _ = c.handle(Ingest::Event(Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(format!("v-{order_client_id}").into()),
        timestamp: 0,
    })));
}

/// A venue fill of `qty` at `px` for `order_client_id`, folded and delivered (and its `OrderFilled` when it is whole).
fn fill(c: &mut Core, order_client_id: &str, tid: &str, qty: f64, px: f64, whole: bool) {
    let f = FillEvent {
        trade_id: TradeId::prefixed("t-", tid),
        order_client_id: order_client_id.to_string(),
        venue: V.into(),
        symbol: SYM.into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: 0,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    };
    let _ = c.handle(Ingest::Event(Event::Fill(f.clone())));
    if whole {
        let _ = c.handle(Ingest::Event(Event::OrderFilled(vike_model::events::OrderFilled {
            order_client_id: order_client_id.to_string(),
            fill: f,
            timestamp: 0,
        })));
    }
}

fn ring(c: &Core, needle: &str) -> usize {
    c.recent.iter().filter(|l| l.contains(needle)).count()
}

fn last_child(c: &Core) -> String {
    c.engine.client.submissions.last().expect("a child went out").order_client_id.clone()
}

/// **One logic, the core's half of the equivalence** (the deleted executor suite's
/// `the_same_quotes_give_the_same_child_orders_on_the_engine_and_the_core`, its five touches): a pegged
/// BUY at the best bid is accepted and armed, never sent; the first fresh touch places ONE plain limit at the
/// bid, every move of the bid re-prices it in place (the price alone), an unchanged touch sends nothing, and a
/// cancel of the parent pulls the child and ends it once. The simulator's twin asserts the same children.
#[test]
fn the_same_quotes_give_the_same_child_orders_as_the_simulator() {
    let mut c = core();
    let parent = submit(&mut c, pegged(1.0));
    assert!(c.engine.client.submissions.is_empty(), "the parent is never sent");
    assert!(!c.engine.registry.contains_key(&parent), "and is in no registry");
    assert_eq!(ring(&c, &format!("order {parent} EMULATED: bybit holds no such pegged order")), 1);
    let views = c.emulated_views();
    assert_eq!((views.len(), views[0].kind, views[0].id.as_str()), (1, "chase", parent.as_str()));

    touch(&mut c, 1, 100.0, 101.0);
    let child = last_child(&c);
    let sent = &c.engine.client.submissions[0];
    assert_eq!(
        (sent.order_type.clone(), sent.side, sent.qty, sent.price, sent.peg, sent.display_quantity),
        (OrderType::Limit, 1, 1.0, Some(100.0), None, None),
        "a plain limit at the bid"
    );
    accept(&mut c, &child);
    for (ts, bid, ask) in
        [(2, 100.5, 101.0), (3, 101.0, 101.5), (4, 100.5, 101.5), (5, 100.5, 101.0)]
    {
        touch(&mut c, ts, bid, ask);
    }
    let reprices: Vec<_> =
        c.engine.client.modifies.iter().map(|(id, q, p)| (id.clone(), *q, *p)).collect();
    assert_eq!(
        reprices,
        vec![
            (child.clone(), None, Some(100.5)),
            (child.clone(), None, Some(101.0)),
            (child.clone(), None, Some(100.5)),
        ],
        "re-priced in place, never re-sized"
    );
    assert_eq!(c.engine.client.submissions.len(), 1, "one child only");

    c.apply_intent(OrderIntent::Cancel(parent.clone()), NOW);
    assert_eq!(c.engine.client.cancels, vec![child], "the cancel pulls the child");
    assert!(c.emulated.working.is_empty(), "and ends the parent");
    let ended = c.engine.registry.get(&parent).expect("the parent is listed once more, finished");
    assert_eq!(ended.status, vike_model::OrderStatus::Canceled);
}

/// **An iceberg** rests one slice of its visible size at its price; a slice's whole fill frees it and the
/// next market message rests the next one, the last slice what remains; the parent's progress is its
/// children's fills, and it ends FILLED once.
#[test]
fn an_iceberg_rests_one_slice_at_a_time_and_ends_filled() {
    let mut c = core();
    let parent = submit(&mut c, shown(2.5, 100.0, 1.0));
    assert!(c.engine.client.submissions.is_empty());
    let sizes = |c: &Core| c.engine.client.submissions.iter().map(|r| r.qty).collect::<Vec<_>>();
    touch(&mut c, 1, 99.0, 101.0);
    assert_eq!(sizes(&c), vec![1.0]);
    let first = last_child(&c);
    accept(&mut c, &first);
    fill(&mut c, &first, "a", 1.0, 100.0, true);
    touch(&mut c, 2, 99.0, 101.0);
    assert_eq!(sizes(&c), vec![1.0, 1.0], "the next slice after the last filled");
    let second = last_child(&c);
    accept(&mut c, &second);
    fill(&mut c, &second, "b", 1.0, 100.0, true);
    touch(&mut c, 3, 99.0, 101.0);
    assert_eq!(sizes(&c), vec![1.0, 1.0, 0.5], "the last slice is what remains");
    let third = last_child(&c);
    accept(&mut c, &third);
    fill(&mut c, &third, "c", 0.5, 100.0, true);
    touch(&mut c, 4, 99.0, 101.0);
    assert!(c.emulated.working.is_empty(), "done");
    let ended = c.engine.registry.get(&parent).expect("listed once more, finished");
    assert_eq!((ended.status, ended.filled_qty), (vike_model::OrderStatus::Filled, 2.5));
    assert!(c.engine.client.submissions.iter().all(|r| r.price == Some(100.0)));
}

/// **The ceiling judges the WHOLE parent**: an iceberg whose total is over the per-order notional cap is
/// refused at accept, though each slice would pass; nothing is armed or sent.
#[test]
fn a_parent_over_the_per_order_ceiling_is_refused_whole() {
    let mut c = core();
    c.engine.gate.limits.max_notional_per_order = Some(250.0);
    let parent = submit(&mut c, shown(10.0, 100.0, 1.0));
    assert!(c.emulated.working.is_empty(), "not armed");
    assert!(c.engine.client.submissions.is_empty());
    let refused = c.engine.registry.get(&parent).expect("refused by name");
    assert_eq!(refused.status, vike_model::OrderStatus::Rejected);
    assert_eq!(ring(&c, "EMULATED_PARENT_DENIED"), 1, "{:?}", c.recent);
}

/// **No amend**: a modify of an emulated order is refused in the words the simulator refuses it in, and
/// nothing reaches the venue.
#[test]
fn an_emulated_order_is_never_amended() {
    let mut c = core();
    let parent = submit(&mut c, pegged(1.0));
    let modify = OrderIntent::Modify {
        order_client_id: parent.clone(),
        new_qty: Some(2.0),
        new_price: None,
        new_trigger_price: None,
    };
    c.apply_intent(modify, NOW);
    let words = vike_execution::emulated::modify_refusal(&parent);
    assert_eq!(ring(&c, &words), 1, "{:?}", c.recent);
    assert_eq!(c.emulated.working[&parent].request.qty, 1.0, "unchanged");
}

/// **No emulation where none is honest**: a hidden order and an IOC peg keep the row's refusal; nothing is
/// armed.
#[test]
fn where_no_emulation_exists_the_rows_refusal_stands() {
    let mut c = core();
    submit(&mut c, OrderRequest { display_quantity: Some(0.0), ..shown(1.0, 100.0, 0.0) });
    submit(&mut c, OrderRequest { time_in_force: vike_model::TimeInForce::Ioc, ..pegged(1.0) });
    assert!(c.emulated.working.is_empty());
    assert!(c.engine.client.submissions.is_empty());
    assert_eq!(ring(&c, "REFUSED"), 2, "{:?}", c.recent);
}

/// **A restart never cancels**: a parent restored with a live child places nothing and cancels nothing,
/// however the touch moves, until the venue confirms the child (the reconcile pass's synthetic `Accepted`);
/// then it re-prices THAT child in place. Restored with no live child, the first hook gives the slot up
/// (no cancel reaches the venue) and the next one places.
#[test]
fn a_restored_parent_readopts_its_child_and_never_cancels_it() {
    let mut a = core();
    let parent = submit(&mut a, pegged(1.0));
    touch(&mut a, 1, 100.0, 101.0);
    let child = last_child(&a);
    accept(&mut a, &child);
    let snap = a.emulated_snap();
    assert_eq!(snap[0].child.as_deref(), Some(child.as_str()));

    let mut b = core_of(
        engine_on(V, SYM, RecordingClient::default()),
        Vec::new(),
        CoreConfig { emulated: snap, ..CoreConfig::default() },
    );
    assert_eq!(ring(&b, "RESTORE EMULATED: 1 emulated order(s) worked again"), 1);
    // The child the restore pass re-registers (`reregister_orders`) as the venue reports it.
    let mut mo = vike_model::OrderRecord::new(a.engine.registry[&child].request.clone());
    mo.status = vike_model::OrderStatus::Accepted;
    b.engine.registry.insert(child.clone(), mo);
    for (ts, bid) in [(2, 100.5), (3, 101.0)] {
        touch(&mut b, ts, bid, 102.0);
    }
    let client = &b.engine.client;
    assert!(
        client.submissions.is_empty() && client.cancels.is_empty(),
        "no second child and no cancel before the venue confirms it (a restored live child may be \
         re-priced in place meanwhile, the law's own rule)"
    );
    assert!(client.modifies.iter().all(|(id, q, _)| *id == child && q.is_none()));
    assert!(b.emulated_views()[0].waiting.as_deref().is_some_and(|w| w.contains(&child)));

    b.queue_order_event(0, V, SYM, &child, OrderEventKind::Accepted);
    b.dispatch_applied_fills();
    touch(&mut b, 4, 100.5, 102.0);
    let client = &b.engine.client;
    assert!(client.submissions.is_empty() && client.cancels.is_empty(), "adopted, never doubled");
    assert_eq!(
        client.modifies.last(),
        Some(&(child.clone(), None, Some(100.5))),
        "re-priced in place"
    );
    assert!(b.emulated.working.contains_key(&parent));

    // Restored between two children: the slot is given up at the first hook, and the next one places.
    let mut fresh = a.emulated_snap();
    fresh[0].child = None;
    fresh[0].children.clear();
    let mut d = core_of(
        engine_on(V, SYM, RecordingClient::default()),
        Vec::new(),
        CoreConfig { emulated: fresh, ..CoreConfig::default() },
    );
    touch(&mut d, 2, 100.0, 101.0);
    assert!(d.engine.client.submissions.is_empty() && d.engine.client.cancels.is_empty());
    touch(&mut d, 3, 100.0, 101.0);
    assert_eq!(d.engine.client.submissions.len(), 1, "the next hook places");
}

/// A mount's owner side: the strategy's tagged pegged order is ONE order to it, `Accepted` once, every
/// child fill under ITS tag (never a child's lifecycle), and the child is the mount's own order (its ledger).
struct Pegs {
    sent: bool,
    heard: Arc<Mutex<Vec<String>>>,
}

impl Strategy<LiveBroker> for Pegs {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        if !std::mem::replace(&mut self.sent, true) {
            broker.submit_pegged_tagged(
                "entry",
                1,
                1.0,
                OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 },
            );
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, e: &OrderEvent) {
        self.heard.lock().unwrap().push(format!("{:?} {:?}", e.tag, e.kind));
    }
    fn on_fill(&mut self, _broker: &mut LiveBroker, f: &Fill) {
        self.heard.lock().unwrap().push(format!("fill {} {:?}", f.size, f.tag));
    }
}

#[test]
fn a_strategys_pegged_order_is_one_order_to_it_and_its_child_is_its_own() {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let mount = Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some("peg".to_string()),
        underlying_symbol: None,
        venue: V.into(),
        symbol: SYM.into(),
        interval: "1m".into(),
        strategy: Box::new(Pegs { sent: false, heard: Arc::clone(&heard) }),
    };
    let mut c = core_of(
        engine_on(V, SYM, RecordingClient::default()),
        Vec::new(),
        CoreConfig { strategy: Some(mount), ..CoreConfig::default() },
    );
    c.engine.collect_applied_fills = true;
    c.drive_strategy_feed_status(V, SYM, FeedStatus::Disconnected);
    c.dispatch_applied_fills();
    assert_eq!(c.emulated.working.len(), 1, "armed for the mount");
    touch(&mut c, 1, 100.0, 101.0);
    let child = last_child(&c);
    assert_eq!(
        c.order_client_id_mount.get(&child).copied(),
        Some(0),
        "the child is the mount's own order"
    );
    accept(&mut c, &child);
    fill(&mut c, &child, "a", 1.0, 100.0, true);
    touch(&mut c, 2, 100.0, 101.0);
    c.dispatch_applied_fills();
    let heard = heard.lock().unwrap().clone();
    assert_eq!(
        heard,
        vec![
            "Some(\"entry\") Accepted".to_string(),
            "fill 1 Some(\"entry\")".to_string(),
            "Some(\"entry\") Filled".to_string(),
        ],
        "one order: its acceptance, its fill under its tag, its end"
    );
}
