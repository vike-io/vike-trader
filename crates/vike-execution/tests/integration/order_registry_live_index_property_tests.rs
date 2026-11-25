//! Property suite for the order registry's LIVE index (`crates/vike-execution/src/execution_engine/order_registry.rs`'s
//! `OrderRegistry::live`): the per-publish margin walk and the per-submit margin, exposure, cover and cash walks
//! read it instead of the whole registry, which keeps every ended order (decision 0207).
//!
//! The laws, checked after EVERY step of a random walk over the engine's own paths (submit, the venue's answers
//! from the ack to a liquidation, amend, mass cancel, `apply_snapshot`, a rebuild through `from_snapshot`) and over
//! the core's direct registry writes (an emulated order's arm, fire and end as `vike_core`'s `emulated.rs` writes
//! them, a held order's release as `held_release.rs` writes it, a restore's in-place status write, a removal):
//!
//! 1. `live()` yields what the FULL walk keeps, `iter().filter(|(_, o)| o.status.is_live())`: the same records
//!    (by address) in the same order, so every walk over it is the full walk's, sum for sum.
//! 2. Every margin and exposure figure read off the index is bit-identical to the same figure over an index
//!    rebuilt by a full scan.
//! 3. A mass cancel sends exactly the full walk's live orders.
//!
//! FAILURE POLICY: a counterexample is a REAL finding about the index; commit the seed beside this file as
//! `order_registry_live_index_property_tests.proptest-regressions`. No wall clock is read.

use proptest::prelude::*;
use vike_execution::execution_engine::OrderRegistry;
use vike_execution::testing::{EngineBuilder, RecordingClient};
use vike_execution::{Engine, EventBus, EventBusOutbox, PriceBoardConfig, ReconcileSnapshot};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderExpired, OrderFilled, OrderLiquidated,
    OrderModified, OrderPartiallyFilled, OrderRejected, OrderSubmitted, OrderTriggered,
    PositionSide, TradeId,
};
use vike_model::{OrderCancelIntent, OrderRecord, OrderRequest, OrderStatus, OrderType};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";
/// Quoted nowhere: its market orders are unpriced (the exposure fails closed), its limits count at their own price.
const UNQUOTED: &str = "ETHUSDT";
/// A small pool, so most steps land on an order that exists, ended or not.
const IDS: [&str; 8] = ["a", "b", "c", "d", "e", "f", "g", "h"];
const RATE: f64 = 0.1;

const STATUSES: [OrderStatus; 14] = [
    OrderStatus::Initialized,
    OrderStatus::Submitted,
    OrderStatus::Accepted,
    OrderStatus::Triggered,
    OrderStatus::PartiallyFilled,
    OrderStatus::Filled,
    OrderStatus::Canceled,
    OrderStatus::Rejected,
    OrderStatus::Denied,
    OrderStatus::Expired,
    OrderStatus::PendingCancel,
    OrderStatus::Liquidated,
    OrderStatus::Emulated,
    OrderStatus::Released,
];

#[derive(Debug, Clone)]
enum Answer {
    Submitted,
    Accepted,
    Triggered,
    PartiallyFilled(f64),
    Filled,
    Canceled,
    Rejected,
    Expired,
    Liquidated,
    Modified(f64),
}

#[derive(Debug, Clone)]
enum Step {
    Submit { id: usize, side: i32, qty: f64, limit: Option<f64>, reduce_only: bool, unquoted: bool },
    Venue { id: usize, answer: Answer },
    Amend { id: usize, qty: f64, price: f64 },
    MassCancel,
    ApplySnapshot { open: Vec<usize> },
    Rebuild,
    EmulatedArm { id: usize },
    EmulatedFire { id: usize },
    EmulatedEnd { id: usize, filled: bool },
    HeldRelease { id: usize },
    Write { id: usize, status: OrderStatus },
    Remove { id: usize },
}

fn arb_answer() -> impl Strategy<Value = Answer> {
    prop_oneof![
        Just(Answer::Submitted),
        Just(Answer::Accepted),
        Just(Answer::Triggered),
        (0.1..2.0f64).prop_map(Answer::PartiallyFilled),
        Just(Answer::Filled),
        Just(Answer::Canceled),
        Just(Answer::Rejected),
        Just(Answer::Expired),
        Just(Answer::Liquidated),
        (0.5..5.0f64).prop_map(Answer::Modified),
    ]
}

fn arb_step() -> impl Strategy<Value = Step> {
    let id = 0..IDS.len();
    prop_oneof![
        4 => (
            id.clone(),
            any::<bool>(),
            0.5..5.0f64,
            prop::option::of(90.0..110.0f64),
            prop::bool::weighted(0.2),
            prop::bool::weighted(0.2),
        )
            .prop_map(|(id, buy, qty, limit, reduce_only, unquoted)| Step::Submit {
                id,
                side: if buy { 1 } else { -1 },
                qty,
                limit,
                reduce_only,
                unquoted,
            }),
        6 => (id.clone(), arb_answer()).prop_map(|(id, answer)| Step::Venue { id, answer }),
        1 => (id.clone(), 0.5..5.0f64, 90.0..110.0f64)
            .prop_map(|(id, qty, price)| Step::Amend { id, qty, price }),
        1 => Just(Step::MassCancel),
        1 => prop::collection::vec(id.clone(), 0..4).prop_map(|open| Step::ApplySnapshot { open }),
        1 => Just(Step::Rebuild),
        1 => id.clone().prop_map(|id| Step::EmulatedArm { id }),
        1 => id.clone().prop_map(|id| Step::EmulatedFire { id }),
        1 => (id.clone(), any::<bool>()).prop_map(|(id, filled)| Step::EmulatedEnd { id, filled }),
        1 => id.clone().prop_map(|id| Step::HeldRelease { id }),
        2 => (id.clone(), prop::sample::select(STATUSES.to_vec()))
            .prop_map(|(id, status)| Step::Write { id, status }),
        1 => id.prop_map(|id| Step::Remove { id }),
    ]
}

fn engine() -> Engine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build();
    e.price_board.set_mark(VENUE, SYMBOL, 100.0, 0);
    e
}

fn request(
    order_client_id: &str,
    side: i32,
    qty: f64,
    limit: Option<f64>,
    reduce_only: bool,
    unquoted: bool,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: if unquoted { UNQUOTED } else { SYMBOL }.into(),
        side,
        qty,
        order_type: if limit.is_some() { OrderType::Limit } else { OrderType::Market },
        price: limit,
        reduce_only,
        timestamp: 1,
        ..Default::default()
    }
}

/// The record the core writes straight into a registry (`end_emulated`, `end_held_release`).
fn record(order_client_id: &str, status: OrderStatus) -> OrderRecord {
    let mut order = OrderRecord::new(request(order_client_id, 1, 1.0, Some(100.0), false, false));
    order.status = status;
    order.created_ms = Some(1);
    order
}

fn fill(order_client_id: &str, trade: u32, symbol: &str, side: i32, qty: f64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("T", trade),
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// What the venue says about `order_client_id`, each fill as a venue lane delivers it: the bare fill, then the wrap.
fn answer(
    e: &mut Engine<RecordingClient>,
    trade: &mut u32,
    order_client_id: &str,
    answer: &Answer,
) {
    let (symbol, side, left) = e
        .registry
        .get(order_client_id)
        .map(|o| (o.request.symbol.clone(), o.request.side, o.request.qty - o.filled_qty))
        .unwrap_or((SYMBOL.into(), 1, 1.0));
    *trade += 1;
    let id = || order_client_id.to_string();
    let events = match answer {
        Answer::Submitted => {
            vec![Event::OrderSubmitted(OrderSubmitted { order_client_id: id(), timestamp: 0 })]
        }
        Answer::Accepted => vec![Event::OrderAccepted(OrderAccepted {
            order_client_id: id(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        })],
        Answer::Triggered => {
            vec![Event::OrderTriggered(OrderTriggered { order_client_id: id(), timestamp: 0 })]
        }
        Answer::PartiallyFilled(qty) => {
            let f = fill(order_client_id, *trade, &symbol, side, qty.min(left).max(0.01));
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
            let f = fill(order_client_id, *trade, &symbol, side, left.max(0.01));
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
            reason: "prop".into(),
            timestamp: 0,
            refusal_id: None,
        })],
        Answer::Expired => {
            vec![Event::OrderExpired(OrderExpired { order_client_id: id(), timestamp: 0 })]
        }
        Answer::Liquidated => vec![Event::OrderLiquidated(OrderLiquidated {
            order_client_id: id(),
            liquidation_price: 100.0,
            timestamp: 0,
        })],
        Answer::Modified(qty) => vec![Event::OrderModified(OrderModified {
            order_client_id: id(),
            venue_order_id: None,
            new_qty: Some(*qty),
            new_price: None,
            new_trigger_price: None,
            timestamp: 0,
        })],
    };
    let mut bus = EventBus::new();
    for event in events {
        bus.publish(event, e);
    }
}

/// Every margin and exposure figure, as bits.
fn figures(e: &Engine<RecordingClient>) -> [u64; 5] {
    let config = PriceBoardConfig::default();
    [
        e.live_order_margin(&config, RATE, ""),
        e.live_order_margin(&config, RATE, IDS[0]),
        e.resolved_account_exposure_excluding("", "", &config),
        e.resolved_account_exposure_excluding(SYMBOL, IDS[1], &config),
        e.resolved_account_exposure_excluding(UNQUOTED, "", &config),
    ]
    .map(f64::to_bits)
}

/// Laws 1 and 2.
fn check(e: &mut Engine<RecordingClient>) -> Result<(), TestCaseError> {
    let full: Vec<(&String, &OrderRecord)> =
        e.registry.iter().filter(|(_, o)| o.status.is_live()).collect();
    let live: Vec<(&String, &OrderRecord)> = e.registry.live().collect();
    let ids = |orders: &[(&String, &OrderRecord)]| {
        orders.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>()
    };
    prop_assert_eq!(ids(&live), ids(&full), "the live index and the full walk disagree");
    for ((_, indexed), (_, walked)) in live.iter().zip(&full) {
        prop_assert!(std::ptr::eq(*indexed, *walked), "the index yields another record");
    }
    let indexed = figures(e);
    let scanned: OrderRegistry = e.registry.iter().map(|(k, o)| (k.clone(), o.clone())).collect();
    let kept = std::mem::replace(&mut e.registry, scanned);
    let scanned = figures(e);
    e.registry = kept;
    prop_assert_eq!(indexed, scanned, "a figure over the index is not the full scan's");
    Ok(())
}

fn step(
    e: &mut Engine<RecordingClient>,
    trade: &mut u32,
    step: &Step,
) -> Result<(), TestCaseError> {
    let mut outbox = EventBusOutbox::default();
    match *step {
        Step::Submit { id, side, qty, limit, reduce_only, unquoted } => e.submit_order(
            &request(IDS[id], side, qty, limit, reduce_only, unquoted),
            1,
            &mut outbox,
        ),
        Step::Venue { id, ref answer } => self::answer(e, trade, IDS[id], answer),
        Step::Amend { id, qty, price } => {
            e.modify_order(IDS[id], Some(qty), Some(price), 1, &mut outbox)
        }
        Step::MassCancel => {
            let expected: Vec<String> = e
                .registry
                .iter()
                .filter(|(_, o)| o.status.is_live())
                .map(|(id, _)| id.clone())
                .collect();
            let before = e.client.cancels.len();
            e.mass_cancel_with_intent(OrderCancelIntent::Unspecified);
            prop_assert_eq!(&e.client.cancels[before..], &expected[..], "law 3: the mass cancel");
        }
        Step::ApplySnapshot { ref open } => {
            let open_orders = open.iter().map(|&i| record(IDS[i], OrderStatus::Accepted)).collect();
            e.apply_snapshot(&ReconcileSnapshot { open_orders, ..Default::default() });
        }
        Step::Rebuild => {
            *e = Engine::from_snapshot(&e.snapshot_state(), RecordingClient::default());
            e.price_board.set_mark(VENUE, SYMBOL, 100.0, 0);
        }
        Step::EmulatedArm { id } => {
            e.registry.insert(IDS[id].into(), record(IDS[id], OrderStatus::Emulated));
        }
        Step::EmulatedFire { id } => {
            if let Some(order) = e.registry.get_mut(IDS[id])
                && order.status == OrderStatus::Emulated
            {
                order.status = OrderStatus::Released;
            }
        }
        Step::EmulatedEnd { id, filled } => {
            let mut order =
                record(IDS[id], if filled { OrderStatus::Filled } else { OrderStatus::Canceled });
            order.filled_qty = if filled { order.request.qty } else { 0.0 };
            e.registry.insert(IDS[id].into(), order);
        }
        Step::HeldRelease { id } => {
            // Registered as the order the node accepted and held, read once, then canceled through the fold.
            e.registry.insert(IDS[id].into(), record(IDS[id], OrderStatus::Accepted));
            check(e)?;
            self::answer(e, trade, IDS[id], &Answer::Canceled);
        }
        Step::Write { id, status } => {
            if let Some(order) = e.registry.get_mut(IDS[id]) {
                order.status = status;
            }
        }
        Step::Remove { id } => {
            e.registry.shift_remove(IDS[id]);
        }
    }
    Ok(())
}

proptest! {
    #[test]
    fn the_live_index_walks_exactly_what_the_full_walk_keeps(
        steps in prop::collection::vec(arb_step(), 1..64)
    ) {
        let mut e = engine();
        let mut trade = 0;
        for s in &steps {
            step(&mut e, &mut trade, s)?;
            check(&mut e)?;
        }
    }
}

/// The case the index exists to get right, by hand: an ENDED order made live again over its own key (a held
/// release) is walked again, and leaves the walk when it ends.
#[test]
fn an_ended_order_made_live_again_is_walked_again() {
    let mut e = engine();
    let mut trade = 0;
    let config = PriceBoardConfig::default();
    let mut outbox = EventBusOutbox::default();
    e.submit_order(&request("a", 1, 2.0, Some(100.0), false, false), 1, &mut outbox);
    for a in [Answer::Submitted, Answer::Accepted, Answer::Filled] {
        answer(&mut e, &mut trade, "a", &a);
    }
    assert_eq!(e.registry["a"].status, OrderStatus::Filled, "premise: the order ended");
    assert_eq!(e.live_order_margin(&config, RATE, ""), 0.0, "an ended order commits nothing");

    e.registry.insert("a".into(), record("a", OrderStatus::Accepted));
    assert_eq!(e.registry.live().count(), 1);
    assert_eq!(e.live_order_margin(&config, RATE, ""), 100.0 * RATE, "1 at 100, at the rate");

    answer(&mut e, &mut trade, "a", &Answer::Canceled);
    assert_eq!(e.registry.live().count(), 0);
    assert_eq!(e.live_order_margin(&config, RATE, ""), 0.0);
}
