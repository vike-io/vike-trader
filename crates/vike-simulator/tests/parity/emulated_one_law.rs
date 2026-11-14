//! **One law, one tape** (decision 0275): the live core's emulated orders
//! (`crates/vike-core/src/runtime/emulated.rs`) and the simulator's (`crates/vike-simulator/src/engine/sim_broker/emulated.rs`)
//! send the SAME child orders, side, size and price of every place, re-price and cancel in the same order, off one
//! shared tape on bybit (a venue whose row holds neither a pegged order nor a visible size).
//!
//! The tape is the touches, the fills a filling venue makes, and the owner's own verbs. The live core is
//! `vike_core::spawn_core` over a venue double that logs what reaches it; the parent is an operator's
//! `Command::Order` submit, the touches go through the core's market lane to its engine's price board, and each fill
//! is the venue's `Fill` and `OrderFilled` for the child resting then. The simulator replays the same touches on its
//! tick lane, where its own tick law makes the fills; that they fall on the touches the tape says is asserted too.
//!
//! The core drives its book at its drain-loop boundary, once per drained batch, and the simulator once per event:
//! they agree when each message drains alone, the idle cadence. The core here runs with `batch_max: 1`, so every
//! message is its own batch, and a sync command dequeued (`CoreConfig::on_dequeued`) proves every message before it
//! folded and its boundary passed. A fill lands just before the touch that crossed the child: the simulator fills
//! a crossed child before its law hears the touch.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_execution::{Command, ExecutionClient, Ingest, OrderIntent, QuoteUpdate};
use vike_marketdata::TickQuote;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderSubmitted, PositionSide,
    TradeId,
};
use vike_model::{OrderPeg, OrderPegAnchor, OrderRequest, OrderType};
use vike_simulator::FillModelKind;

use crate::emulated_orders::{
    Child, Owner, SHOWN_VENUE as VENUE, SYM, buy_at_best, iceberg_buy, lane, place, q, reprice, run,
};

/// One step of the shared tape.
enum Step {
    /// A touch on the market lane.
    Touch(i64, f64, f64),
    /// The venue fills the child resting now, whole, at its price.
    Fill,
    /// The owner sends this parent.
    Submit(Box<OrderRequest>),
    /// The owner cancels the parent of this order client id.
    Cancel(&'static str),
}
use Step::{Fill, Submit, Touch};

/// A pegged buy of 1.0 at its own best price, under the order client id `order_client_id`.
fn pegged(order_client_id: &str) -> Step {
    Submit(Box::new(OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: VENUE.into(),
        symbol: SYM.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 }),
        ..OrderRequest::default()
    }))
}

/// A buy of 2.5 at 100 showing 1.0 (the iceberg suite's `BUY`).
fn shown(order_client_id: &str) -> Step {
    Submit(Box::new(OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: VENUE.into(),
        symbol: SYM.into(),
        side: 1,
        qty: 2.5,
        order_type: OrderType::Limit,
        price: Some(100.0),
        display_quantity: Some(1.0),
        ..OrderRequest::default()
    }))
}

/// The tape's touches, as the simulator replays them.
fn quotes(tape: &[Step]) -> Vec<vike_simulator::TickEvent> {
    tape.iter()
        .filter_map(|s| if let Touch(ts, b, a) = *s { Some(q(ts, b, a)) } else { None })
        .collect()
}

/// The ts of every touch a tape fill lands before: where the simulator's tick law must fill.
fn fill_points(tape: &[Step]) -> Vec<i64> {
    tape.windows(2)
        .filter_map(|w| match (&w[0], &w[1]) {
            (Fill, Touch(ts, ..)) => Some(*ts),
            _ => None,
        })
        .collect()
}

/// What reached the venue double: each child verb with the order client id it named, and the child resting now.
#[derive(Default)]
struct Venue {
    log: Vec<(Child, String)>,
    events: VecDeque<Event>,
    /// `(order client id, side, qty, price)` of the child resting now.
    resting: Option<(String, i32, f64, f64)>,
}

/// A venue that accepts every order, re-prices in place and cancels at once, and fills nothing on its own.
struct WireClient(Arc<Mutex<Venue>>);

impl ExecutionClient for WireClient {
    fn submit(&mut self, r: &OrderRequest) {
        let price = r.price.expect("a child is a plain limit");
        assert_eq!((&r.order_type, r.peg, r.display_quantity), (&OrderType::Limit, None, None));
        let order_client_id = r.order_client_id.clone();
        let mut v = self.0.lock().unwrap();
        v.log.push((place(r.side, r.qty, price), order_client_id.clone()));
        v.resting = Some((order_client_id.clone(), r.side, r.qty, price));
        v.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.clone(),
            timestamp: 0,
        }));
        v.events.push_back(Event::OrderAccepted(OrderAccepted {
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            order_client_id,
            timestamp: 0,
        }));
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        assert_eq!(new_qty, None, "a child is never re-sized");
        let price = new_price.expect("a re-price names its price");
        let mut v = self.0.lock().unwrap();
        v.log.push((reprice(price), order.order_client_id.clone()));
        if let Some(rest) = v.resting.as_mut() {
            rest.3 = price;
        }
    }
    fn cancel(&mut self, order_client_id: &str) {
        let mut v = self.0.lock().unwrap();
        v.log.push((Child::Cancel, order_client_id.to_string()));
        v.resting = None;
        v.events.push_back(Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: "venue".to_string().into(),
            timestamp: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.0.lock().unwrap().events.pop_front()
    }
}

/// The sync command's order client id stem: a cancel of an order nobody placed, which reaches no venue.
const SYNC: &str = "sync-";

/// The live core on bybit, every message its own batch, the last sync command it dequeued counted in `synced`.
fn live_core(venue: &Arc<Mutex<Venue>>, synced: &Arc<AtomicU64>) -> CoreHandle {
    let engine = vike_execution::testing::engine_on(VENUE, SYM, WireClient(Arc::clone(venue)));
    let (t, seen) = (Arc::new(AtomicI64::new(0)), Arc::clone(synced));
    let on_dequeued = move |msg: &Ingest| {
        if let Ingest::Command(Command::Order(OrderIntent::Cancel(order_client_id))) = msg
            && let Some(Ok(n)) = order_client_id.strip_prefix(SYNC).map(str::parse)
        {
            seen.store(n, Ordering::SeqCst);
        }
    };
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        batch_max: 1,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        on_dequeued: Some(Box::new(on_dequeued)),
        ..CoreConfig::default()
    };
    spawn_core(engine, config)
}

/// Wait until the core has dequeued sync command `n`: every message sent before it has folded and its boundary
/// drive has run (30 s at most).
fn sync(handle: &CoreHandle, synced: &AtomicU64, n: u64) {
    handle.send_command(Command::Order(OrderIntent::Cancel(format!("{SYNC}{n}"))));
    let deadline = Instant::now() + Duration::from_secs(30);
    while synced.load(Ordering::SeqCst) < n {
        assert!(Instant::now() < deadline, "the core never dequeued sync {n}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The venue's whole fill of the child resting now, as two messages: the money and the order's terminal.
fn fill_of((order_client_id, side, qty, price): (String, i32, f64, f64)) -> [Event; 2] {
    let fill = FillEvent {
        trade_id: TradeId::prefixed("t-", &order_client_id),
        order_client_id: order_client_id.clone(),
        venue: VENUE.to_string().into(),
        symbol: SYM.to_string().into(),
        side,
        last_qty: qty,
        last_price: price,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    [
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled { order_client_id, fill, timestamp: 0 }),
    ]
}

/// The tape on the live core: the child orders that reached the venue. Every re-price and cancel must name the
/// child placed last (a verb on another order would move or pull a live order by mistake).
fn on_the_core(tape: &[Step]) -> Vec<Child> {
    let venue = Arc::new(Mutex::new(Venue::default()));
    let synced = Arc::new(AtomicU64::new(0));
    let handle = live_core(&venue, &synced);
    let mut syncs = 0;
    for step in tape {
        match step {
            Touch(ts, bid, ask) => {
                let quote = TickQuote {
                    timestamp: *ts,
                    local_timestamp: 0,
                    bid: *bid,
                    ask: *ask,
                    bid_size: 1.0,
                    ask_size: 1.0,
                    symbol: SYM.to_string(),
                };
                let update =
                    QuoteUpdate { venue: VENUE.to_string(), symbol: SYM.to_string(), quote };
                handle.tick_sender().quote(update).expect("the core is running");
            }
            Fill => {
                syncs += 1;
                sync(&handle, &synced, syncs);
                let resting = venue.lock().unwrap().resting.take().expect("a child rests to fill");
                for event in fill_of(resting) {
                    handle.event_sender().blocking_send(event).expect("the core is running");
                }
            }
            Submit(req) => handle.send_command(Command::Order(OrderIntent::Submit(req.clone()))),
            Step::Cancel(order_client_id) => handle
                .send_command(Command::Order(OrderIntent::Cancel(order_client_id.to_string()))),
        }
        syncs += 1;
        sync(&handle, &synced, syncs);
    }
    handle.shutdown_and_join();
    let log = std::mem::take(&mut venue.lock().unwrap().log);
    let mut last: Option<&str> = None;
    for (child, order_client_id) in &log {
        match child {
            Child::Place { .. } => last = Some(order_client_id.as_str()),
            _ => assert_eq!(
                last,
                Some(order_client_id.as_str()),
                "a verb named another order: {log:?}"
            ),
        }
    }
    log.into_iter().map(|(child, _)| child).collect()
}

/// The tape on the simulator's tick lane with the strategy `owner` (sending and cancelling as the tape's owner
/// does): its child orders, asserted to fill where the tape's venue fills.
fn on_the_simulator(tape: &[Step], owner: Owner) -> Vec<Child> {
    let engine = run(owner, quotes(tape), lane(FillModelKind::Tick, VENUE));
    let fills: Vec<i64> = engine.strategy.fills.iter().map(|f| f.0).collect();
    assert_eq!(fills, fill_points(tape), "the simulator fills where the tape's venue fills");
    engine.strategy.log.clone()
}

/// The equivalence suite's five touches: a pegged buy at its own best price follows the bid in place and is
/// pulled on the fifth.
#[test]
fn the_core_and_the_simulator_chase_the_same_five_touches_with_the_same_children() {
    let tape = [
        pegged("peg-1"),
        Touch(1, 100.0, 101.0),
        Touch(2, 100.5, 101.0),
        Touch(3, 101.0, 101.5),
        Touch(4, 100.5, 101.5),
        Touch(5, 100.5, 101.0),
        Step::Cancel("peg-1"),
    ];
    let want =
        [place(1, 1.0, 100.0), reprice(100.5), reprice(101.0), reprice(100.5), Child::Cancel];
    assert_eq!(on_the_core(&tape), want, "the live core");
    assert_eq!(
        on_the_simulator(&tape, Owner::new(buy_at_best(1.0)).pull_on(5)),
        want,
        "the simulator"
    );
}

/// The chase's child fills at the ask that came down to it, the parent is done with no cancel, and the parent
/// the owner sends next rests its child at the touch that filled the first, follows the bid, and is pulled.
#[test]
fn a_chase_whose_child_fills_is_done_and_the_next_one_rests_the_same_on_both() {
    let tape = [
        pegged("peg-1"),
        Touch(1, 100.0, 101.0),
        Touch(2, 100.5, 101.0),
        Touch(3, 100.5, 101.0),
        Fill,
        Touch(4, 100.0, 100.5),
        pegged("peg-2"),
        Touch(5, 99.5, 100.5),
        Touch(6, 99.5, 100.5),
        Step::Cancel("peg-2"),
    ];
    let want =
        [place(1, 1.0, 100.0), reprice(100.5), place(1, 1.0, 100.0), reprice(99.5), Child::Cancel];
    assert_eq!(on_the_core(&tape), want, "the live core");
    let owner = Owner::new(buy_at_best(1.0)).resend().pull_on(6);
    assert_eq!(on_the_simulator(&tape, owner), want, "the simulator");
}

/// The iceberg rests one slice of its visible size at its price, the next after each fills, the last what remains,
/// to completion.
#[test]
fn an_iceberg_sends_the_same_slices_to_completion_on_both() {
    let tape = [
        shown("ice-1"),
        Touch(1, 99.5, 100.5),
        Fill,
        Touch(2, 99.0, 99.5),
        Fill,
        Touch(3, 99.0, 99.5),
        Fill,
        Touch(4, 99.0, 99.5),
        Touch(5, 99.0, 99.5),
    ];
    let want = [place(1, 1.0, 100.0), place(1, 1.0, 100.0), place(1, 0.5, 100.0)];
    assert_eq!(on_the_core(&tape), want, "the live core");
    assert_eq!(on_the_simulator(&tape, Owner::new(iceberg_buy(100.0))), want, "the simulator");
}
