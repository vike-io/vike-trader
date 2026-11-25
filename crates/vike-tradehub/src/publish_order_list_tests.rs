//! **The published order list the core keeps between publishes is the list a from-scratch rebuild publishes, row for
//! row, in order, and frame byte for frame byte** (`crates/vike-core/src/snapshot/order_window.rs`: a publish rebuilds
//! only the rows whose order moved and shares the rest).
//!
//! A random walk over two engines (a default account and a labelled one) drives the orders through the engine's own
//! paths (submit, the venue's answers, a modify in flight and its refusal, a reconcile's `apply_snapshot`, a restore
//! through `Engine::from_snapshot`, a registry collected anew over the same orders) and the core's direct writes (a
//! status written in place, a re-registration over a key, which makes an ended order live again), while the CORE clock
//! steps across the hour an ended order stays and the sweep after it, and the engine clock across a modify's time to
//! live. After EVERY step it publishes through one long-lived `OrderWindows` and checks it against the reference: the
//! window the base branch kept (`Reference`, its algorithm verbatim: which registry indices are published) over rows
//! built anew by a fresh window's publish of every registry order. A second publish with nothing moved must share
//! every row.
//!
//! FAILURE POLICY: a counterexample is a REAL finding about the shared rows; commit the seed beside this file as
//! `publish_order_list_tests.proptest-regressions`. No wall clock is read.

use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use vike_core::snapshot::{ENDED_ORDER_PUBLISH_MS, OrderWindows};
use vike_execution::testing::{EngineBuilder, RecordingClient};
use vike_execution::{Engine, EventBus, EventBusOutbox, ReconcileSnapshot};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, OrderModifyRejected,
    OrderPartiallyFilled, OrderRejected, OrderSubmitted, PositionSide, TradeId,
};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType};

use super::*;

const SYMBOL: &str = "BTCUSDT";
/// A small pool, so most steps land on an order that exists, ended or not.
const IDS: [&str; 6] = ["a", "b", "c", "d", "e", "f"];
/// The window's sweep (`crates/vike-core/src/snapshot/order_window.rs`'s private `ENDED_ORDER_SWEEP_MS`).
const SWEEP_MS: i64 = 60 * 1_000;

#[derive(Debug, Clone)]
enum Answer {
    Submitted,
    Accepted,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

#[derive(Debug, Clone)]
enum Step {
    Submit { engine: usize, id: usize, limit: Option<f64> },
    Venue { engine: usize, id: usize, answer: Answer },
    Modify { engine: usize, id: usize, price: f64 },
    ModifyRefused { engine: usize, id: usize },
    Write { engine: usize, id: usize, status: OrderStatus },
    Reregister { engine: usize, id: usize, status: OrderStatus },
    Reconcile { engine: usize, open: Vec<usize> },
    Restore { engine: usize },
    Recollect { engine: usize },
    CoreClock { by: i64 },
    EngineClock { engine: usize, by: i64 },
}

fn arb_status() -> impl Strategy<Value = OrderStatus> {
    prop::sample::select(vec![
        OrderStatus::Submitted,
        OrderStatus::Accepted,
        OrderStatus::PartiallyFilled,
        OrderStatus::PendingCancel,
        OrderStatus::Filled,
        OrderStatus::Canceled,
        OrderStatus::Rejected,
        OrderStatus::Denied,
        OrderStatus::Expired,
    ])
}

fn arb_step() -> impl Strategy<Value = Step> {
    let id = 0..IDS.len();
    let engine = 0..2usize;
    let answer = prop_oneof![
        Just(Answer::Submitted),
        Just(Answer::Accepted),
        Just(Answer::PartiallyFilled),
        Just(Answer::Filled),
        Just(Answer::Canceled),
        Just(Answer::Rejected),
    ];
    prop_oneof![
        4 => (engine.clone(), id.clone(), prop::option::of(90.0..110.0f64))
            .prop_map(|(engine, id, limit)| Step::Submit { engine, id, limit }),
        6 => (engine.clone(), id.clone(), answer)
            .prop_map(|(engine, id, answer)| Step::Venue { engine, id, answer }),
        2 => (engine.clone(), id.clone(), 90.0..110.0f64)
            .prop_map(|(engine, id, price)| Step::Modify { engine, id, price }),
        1 => (engine.clone(), id.clone()).prop_map(|(engine, id)| Step::ModifyRefused { engine, id }),
        2 => (engine.clone(), id.clone(), arb_status())
            .prop_map(|(engine, id, status)| Step::Write { engine, id, status }),
        2 => (engine.clone(), id.clone(), arb_status())
            .prop_map(|(engine, id, status)| Step::Reregister { engine, id, status }),
        1 => (engine.clone(), prop::collection::vec(id, 0..3))
            .prop_map(|(engine, open)| Step::Reconcile { engine, open }),
        1 => engine.clone().prop_map(|engine| Step::Restore { engine }),
        1 => engine.clone().prop_map(|engine| Step::Recollect { engine }),
        4 => prop::sample::select(vec![
            0,
            1_000,
            SWEEP_MS / 2,
            SWEEP_MS,
            ENDED_ORDER_PUBLISH_MS - 1,
            ENDED_ORDER_PUBLISH_MS,
            ENDED_ORDER_PUBLISH_MS + SWEEP_MS,
        ])
        .prop_map(|by| Step::CoreClock { by }),
        1 => (engine, prop::sample::select(vec![1_000i64, 29_999, 30_001]))
            .prop_map(|(engine, by)| Step::EngineClock { engine, by }),
    ]
}

/// The engines a core publishes, by place: a default account first, then a labelled one (its rows carry the label).
struct Node {
    primary: Engine<RecordingClient>,
    extras: Vec<(f64, Engine<RecordingClient>)>,
}

impl Node {
    fn new() -> Self {
        let engine = |venue: &str| {
            let mut e =
                EngineBuilder { venue: venue.into(), symbol: SYMBOL.into(), ..Default::default() }
                    .build();
            e.price_board.set_mark(venue, SYMBOL, 100.0, 0);
            e.now_ms = 1;
            e
        };
        let mut labelled = engine("bybit");
        labelled.route_key = "bybit#ALT".into();
        Node { primary: engine("binance"), extras: vec![(1_000.0, labelled)] }
    }

    fn engine(&mut self, at: usize) -> &mut Engine<RecordingClient> {
        if at == 0 { &mut self.primary } else { &mut self.extras[at - 1].1 }
    }

    fn engines(&self) -> impl Iterator<Item = &Engine<RecordingClient>> {
        std::iter::once(&self.primary).chain(self.extras.iter().map(|(_, e)| e))
    }

    /// One publish through `windows`, every other input empty.
    fn publish(&self, windows: &mut OrderWindows) -> CoreSnapshot {
        vike_core::snapshot::build(
            7,
            &self.primary,
            &self.extras,
            1_000.0,
            vike_execution::PriceBoardConfig::default(),
            vike_model::MarginCallConfig::default().maintenance_margin_requirement,
            &VecDeque::new(),
            &indexmap::IndexMap::new(),
            Arc::new([]),
            &None,
            vike_execution::ReconcileBlock::default(),
            Vec::new(),
            windows,
        )
    }
}

/// **The window as the base branch kept it** (`order_window.rs` at `63f47ba18c`, verbatim but for its names): which
/// registry indices each engine publishes.
#[derive(Default)]
struct Reference {
    engines: Vec<ReferenceWindow>,
    now: Option<i64>,
}

/// One engine's: how many registry entries it took in, and the indices kept, each with the reading that found it ended.
#[derive(Default)]
struct ReferenceWindow {
    taken: usize,
    kept: Vec<(usize, Option<i64>)>,
}

impl Reference {
    fn published(&mut self, at: usize, engine: &Engine<RecordingClient>) -> Vec<usize> {
        if self.engines.len() <= at {
            self.engines.resize_with(at + 1, ReferenceWindow::default);
        }
        let now = self.now;
        let ReferenceWindow { taken, kept } = &mut self.engines[at];
        let registry = &engine.registry;
        if registry.len() < *taken {
            (*taken, *kept) = (0, Vec::new());
        }
        kept.extend((*taken..registry.len()).map(|index| (index, None)));
        *taken = registry.len();
        let mut slot = 0;
        for index in registry.live_positions() {
            while kept.get(slot).is_some_and(|(kept_index, _)| *kept_index < index) {
                slot += 1;
            }
            if kept.get(slot).is_none_or(|(kept_index, _)| *kept_index != index) {
                kept.insert(slot, (index, None));
            }
            slot += 1;
        }
        if let Some(now) = now {
            let past = |ended: &Option<i64>, by: i64| {
                ended.is_some_and(|at| now.saturating_sub(at) >= ENDED_ORDER_PUBLISH_MS + by)
            };
            let sweep = kept.iter().any(|(_, ended)| past(ended, SWEEP_MS));
            kept.retain_mut(|(index, ended)| {
                let Some((_, order)) = registry.get_index(*index) else { return false };
                let first_seen_ended = ended.unwrap_or(now);
                *ended = order.status.is_terminal().then_some(first_seen_ended);
                !(sweep && past(ended, 0))
            });
        }
        kept.iter().map(|(index, _)| *index).filter(|&i| registry.get_index(i).is_some()).collect()
    }
}

fn request(venue: &str, order_client_id: &str, limit: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: venue.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: if limit.is_some() { OrderType::Limit } else { OrderType::Market },
        price: limit,
        timestamp: 1,
        ..Default::default()
    }
}

fn record(venue: &str, order_client_id: &str, status: OrderStatus) -> OrderRecord {
    let mut order = OrderRecord::new(request(venue, order_client_id, Some(100.0)));
    order.status = status;
    order.created_ms = Some(1);
    order
}

/// What the venue says about `order_client_id`, each fill as a venue lane delivers it: the bare fill, then the wrap.
fn answer(
    e: &mut Engine<RecordingClient>,
    trade: &mut u32,
    order_client_id: &str,
    answer: &Answer,
) {
    let left = e.registry.get(order_client_id).map_or(1.0, |o| o.request.qty - o.filled_qty);
    *trade += 1;
    let id = || order_client_id.to_string();
    let fill = |qty: f64| FillEvent {
        trade_id: TradeId::prefixed("T", *trade),
        order_client_id: id(),
        venue: e.venue.as_str().into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    let events = match answer {
        Answer::Submitted => {
            vec![Event::OrderSubmitted(OrderSubmitted { order_client_id: id(), timestamp: 0 })]
        }
        Answer::Accepted => vec![Event::OrderAccepted(OrderAccepted {
            order_client_id: id(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        })],
        Answer::PartiallyFilled => {
            let f = fill((left / 2.0).max(0.01));
            vec![
                Event::Fill(f.clone()),
                Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    order_client_id: id(),
                    fill: f,
                    timestamp: 0,
                }),
            ]
        }
        Answer::Filled => {
            let f = fill(left.max(0.01));
            vec![
                Event::Fill(f.clone()),
                Event::OrderFilled(OrderFilled { order_client_id: id(), fill: f, timestamp: 0 }),
            ]
        }
        Answer::Canceled => vec![Event::OrderCanceled(OrderCanceled {
            order_client_id: id(),
            reason: "prop".into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        })],
        Answer::Rejected => vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: id(),
            reason: format!("refused #{trade}").into(),
            timestamp: 0,
            refusal_id: None,
        })],
    };
    let mut bus = EventBus::new();
    for event in events {
        bus.publish(event, e);
    }
}

fn step(
    node: &mut Node,
    windows: &mut OrderWindows,
    reference: &mut Reference,
    trade: &mut u32,
    step: &Step,
) {
    let mut outbox = EventBusOutbox::default();
    match *step {
        Step::Submit { engine, id, limit } => {
            let e = node.engine(engine);
            let req = request(&e.venue, IDS[id], limit);
            e.submit_order(&req, 1, &mut outbox);
        }
        Step::Venue { engine, id, ref answer } => {
            self::answer(node.engine(engine), trade, IDS[id], answer);
        }
        Step::Modify { engine, id, price } => {
            node.engine(engine).modify_order(IDS[id], None, Some(price), 1, &mut outbox);
        }
        Step::ModifyRefused { engine, id } => {
            *trade += 1;
            let refused = Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: IDS[id].into(),
                reason: format!("modify refused #{trade}").into(),
                timestamp: 0,
                refusal_id: None,
            });
            EventBus::new().publish(refused, node.engine(engine));
        }
        Step::Write { engine, id, status } => {
            if let Some(order) = node.engine(engine).registry.get_mut(IDS[id]) {
                order.status = status;
            }
        }
        Step::Reregister { engine, id, status } => {
            let e = node.engine(engine);
            let order = record(&e.venue, IDS[id], status);
            e.registry.insert(IDS[id].into(), order);
        }
        Step::Reconcile { engine, ref open } => {
            let e = node.engine(engine);
            let open_orders =
                open.iter().map(|&i| record(&e.venue, IDS[i], OrderStatus::Accepted)).collect();
            e.apply_snapshot(&ReconcileSnapshot { open_orders, ..Default::default() });
        }
        Step::Restore { engine } => {
            let e = node.engine(engine);
            let (venue, route_key, now_ms) = (e.venue.clone(), e.route_key.clone(), e.now_ms);
            *e = Engine::from_snapshot(&e.snapshot_state(), RecordingClient::default());
            e.route_key = route_key;
            e.now_ms = now_ms;
            e.price_board.set_mark(&venue, SYMBOL, 100.0, 0);
        }
        Step::Recollect { engine } => {
            let e = node.engine(engine);
            e.registry = e.registry.iter().map(|(k, o)| (k.clone(), o.clone())).collect();
        }
        Step::CoreClock { by } => {
            let now = reference.now.unwrap_or(1_000) + by;
            reference.now = Some(now);
            windows.clock(now);
        }
        Step::EngineClock { engine, by } => node.engine(engine).now_ms += by,
    }
}

/// One publish through the kept `windows` against the reference: the same rows in the same order, the same frame bytes
/// in every body codec, and a second publish with nothing moved sharing every row.
fn check(
    node: &Node,
    windows: &mut OrderWindows,
    reference: &mut Reference,
) -> Result<(), TestCaseError> {
    let shared = node.publish(windows);
    // A fresh window publishes every registry order of every engine, each row built anew.
    let fresh = node.publish(&mut OrderWindows::default());
    let mut rebuilt = Vec::new();
    let mut offset = 0;
    for (at, e) in node.engines().enumerate() {
        rebuilt.extend(
            reference
                .published(at, e)
                .into_iter()
                .map(|index| Arc::clone(&fresh.orders[offset + index])),
        );
        offset += e.registry.len();
    }
    let ids = |orders: &[Arc<OrderView>]| {
        orders.iter().map(|o| format!("{}:{}", o.venue, o.order_client_id)).collect::<Vec<_>>()
    };
    prop_assert_eq!(
        ids(&shared.orders),
        ids(&rebuilt),
        "the published list is not the rebuilt one"
    );
    for (got, want) in shared.orders.iter().zip(&rebuilt) {
        prop_assert_eq!(&**got, &**want, "a kept row is not the rebuilt row");
    }
    let mut from_scratch = fresh;
    from_scratch.orders = rebuilt;
    for codec in [BodyCodec::Json, BodyCodec::Msgpack] {
        prop_assert_eq!(
            frame_wire(&project(&shared, None, &[]), codec),
            frame_wire(&project(&from_scratch, None, &[]), codec),
            "the frame moved"
        );
    }
    let again = node.publish(windows);
    for (at, e) in node.engines().enumerate() {
        reference.published(at, e);
    }
    prop_assert_eq!(again.orders.len(), shared.orders.len());
    prop_assert!(
        again.orders.iter().zip(&shared.orders).all(|(a, b)| Arc::ptr_eq(a, b)),
        "nothing moved, yet a row was rebuilt"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("proptest-regressions"))),
        ..ProptestConfig::default()
    })]

    #[test]
    fn the_kept_order_list_is_the_one_a_rebuild_publishes(steps in prop::collection::vec(arb_step(), 1..80)) {
        let mut node = Node::new();
        let mut windows = OrderWindows::default();
        let mut reference = Reference::default();
        let mut trade = 0;
        check(&node, &mut windows, &mut reference)?;
        for s in &steps {
            step(&mut node, &mut windows, &mut reference, &mut trade, s);
            check(&node, &mut windows, &mut reference)?;
        }
    }
}
