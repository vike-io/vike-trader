//! **A spot (CASH) account cannot sell more base than it holds** (crypto spot cannot short: the owner, 2026-11-08):
//! `Engine::submit_order` refuses, before the gate, a SELL on a `MarginMode::Cash` symbol (#3026's "spot as
//! Cash", the admitted row a spot mount writes) larger than the long the book holds less the live sells already out,
//! in words, and sends nothing. Pinned beside it: a sell within the holding, a buy, and the same oversell on a
//! perpetual (no `Cash` row) are judged exactly as before.

use vike_execution::testing::{EngineBuilder, RecordingClient};
use vike_execution::{Engine, EventBus, EventBusFold, EventBusOutbox, EventHandler, PositionEntry};
use vike_model::events::{
    Event, FillEvent, LiquiditySide, OrderAccepted, OrderSubmitted, PositionSide,
};
use vike_model::{
    MarginMode, OrderCombo, OrderComboLeg, OrderRequest, OrderType, TimeInForce, build_combo,
};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// A `SYMBOL` engine holding a long of `long`; `spot` writes the admitted `Cash` row a spot mount writes.
fn engine(long: f64, spot: bool) -> Engine<RecordingClient> {
    let mut e =
        EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build();
    if spot {
        e.account.admit_margin_mode(SYMBOL, Some(MarginMode::Cash));
    }
    e.account.positions.insert(
        (VENUE.into(), SYMBOL.into(), PositionSide::Both),
        PositionEntry { size: long, average_price: 100.0, ..Default::default() },
    );
    e
}

fn order(order_client_id: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: OrderType::Market,
        timestamp: 1,
        ..Default::default()
    }
}

fn denied(outbox: &mut EventBusOutbox) -> Vec<String> {
    outbox
        .0
        .drain(..)
        .filter_map(|event| match event {
            Event::OrderDenied(d) => Some(d.reason.to_string()),
            _ => None,
        })
        .collect()
}

/// The catalog ids the `OrderDenied`s in `outbox` carry, draining it.
fn denied_ids(outbox: &mut EventBusOutbox) -> Vec<Option<String>> {
    outbox
        .0
        .drain(..)
        .filter_map(|event| match event {
            Event::OrderDenied(d) => Some(d.refusal_id.map(|id| id.to_string())),
            _ => None,
        })
        .collect()
}

/// Every denial names the lane that refused it (`OrderDenied::refusal_id`): the engine's spot rule ahead of the
/// gate, and the gate's own ceiling, each by its catalog id, beside the words it always wrote.
#[test]
fn a_denial_carries_the_catalog_id_of_its_lane() {
    let mut spot = engine(2.0, true);
    let mut outbox = EventBusOutbox::default();
    spot.submit_order(&order("s1", -1, 3.0), 1, &mut outbox);
    assert_eq!(denied_ids(&mut outbox), [Some("order.risk.cash_oversell".to_string())]);

    let mut capped = engine(2.0, false);
    capped.gate.limits.max_notional_per_order = Some(1.0);
    // A limit names its own price (a market order with no mark would be judged at notional zero).
    let limit =
        OrderRequest { order_type: OrderType::Limit, price: Some(100.0), ..order("b1", 1, 1.0) };
    capped.submit_order(&limit, 1, &mut outbox);
    assert_eq!(denied_ids(&mut outbox), [Some("order.risk.over_max_notional".to_string())]);
    assert!(capped.client.submissions.is_empty(), "nothing was sent");
}

/// A sell of the whole holding goes; a second sell beside it (the first still out) finds nothing free and is refused in
/// the rule's words, nothing sent; a sell larger than the holding on a fresh spot book is refused naming both numbers.
#[test]
fn a_spot_sell_past_the_holding_is_refused_in_words_and_sends_nothing() {
    let mut e = engine(2.0, true);
    let mut outbox = EventBusOutbox::default();
    e.submit_order(&order("s1", -1, 2.0), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "the whole holding sells");
    assert!(denied(&mut outbox).is_empty());

    e.submit_order(&order("s2", -1, 2.0), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "nothing more was sent");
    assert_eq!(
        denied(&mut outbox),
        ["a spot account cannot sell more than it holds: it has 0, this sells 2; nothing was sent"]
    );

    let mut fresh = engine(2.0, true);
    fresh.submit_order(&order("s3", -1, 3.0), 1, &mut outbox);
    assert!(fresh.client.submissions.is_empty());
    assert_eq!(
        denied(&mut outbox),
        ["a spot account cannot sell more than it holds: it has 2, this sells 3; nothing was sent"]
    );
}

/// UNCHANGED: a sell within the holding, a buy on a flat spot book, and on a PERPETUAL (no `Cash` row) a sell past the
/// position, which opens a short there as it always did.
#[test]
fn a_spot_sell_within_the_holding_a_buy_and_a_perp_oversell_are_judged_as_before() {
    let mut outbox = EventBusOutbox::default();
    let mut spot = engine(2.0, true);
    spot.submit_order(&order("w1", -1, 1.5), 1, &mut outbox);
    spot.submit_order(&order("w2", -1, 0.5), 1, &mut outbox);
    assert_eq!(spot.client.submissions.len(), 2, "within the holding: {:?}", denied(&mut outbox));

    let mut flat = engine(0.0, true);
    flat.submit_order(&order("b1", 1, 1.0), 1, &mut outbox);
    assert_eq!(flat.client.submissions.len(), 1, "a buy is not this rule's");

    let mut perp = engine(2.0, false);
    perp.submit_order(&order("p1", -1, 2.0), 1, &mut outbox);
    perp.submit_order(&order("p2", -1, 2.0), 1, &mut outbox);
    perp.submit_order(&order("p3", -1, 3.0), 1, &mut outbox);
    assert_eq!(perp.client.submissions.len(), 3, "a perp is untouched");
    assert!(denied(&mut outbox).iter().all(|r| !r.contains("spot account")));
}

/// **A COMBO's sold legs meet the same rule** (`Engine::combo_cash_denial`, which the core's combo lowering asks
/// ahead of the gate): over a spot long of 2, a combo that sells 3 of it is refused naming the leg and one that sells 2 is
/// not; selling the combo flips the legs, so it buys that leg and sells one with no `Cash` row; on a perpetual nothing
/// changes.
#[test]
fn a_combo_selling_a_spot_leg_past_the_holding_is_refused_naming_the_leg() {
    let combo = |side: i32, qty: f64| {
        let spec = OrderCombo {
            venue: VENUE.into(),
            side,
            qty,
            legs: vec![
                OrderComboLeg { symbol: SYMBOL.into(), ratio: -1, limit: None },
                OrderComboLeg { symbol: "ETHUSDT".into(), ratio: 1, limit: None },
            ],
            net_limit: None,
            time_in_force: TimeInForce::Gtc,
            account: None,
            order_client_id: None,
        };
        build_combo(&spec, "c1").expect("a valid combo")
    };
    let spot = engine(2.0, true);
    assert_eq!(
        spot.combo_cash_denial(&combo(1, 3.0)).as_deref(),
        Some(
            "leg BTCUSDT: a spot account cannot sell more than it holds: it has 2, this sells 3; nothing was \
             sent"
        )
    );
    assert_eq!(spot.combo_cash_denial(&combo(1, 2.0)), None, "within the holding");
    assert_eq!(
        spot.combo_cash_denial(&combo(-1, 3.0)),
        None,
        "it buys BTCUSDT, sells ETHUSDT (margin)"
    );
    assert_eq!(engine(2.0, false).combo_cash_denial(&combo(1, 3.0)), None, "a perp is untouched");
}

/// **A MODIFY that raises a spot sell is judged by the same rule**, its own quantity counted as already out, so only the
/// increase is judged: over a long of 2, a resting sell of 1 raised to 3 is refused NON-terminally in the rule's words (the
/// order keeps its terms, nothing reaches the venue), and raised to 2 it goes.
#[test]
fn a_modify_raising_a_spot_sell_past_the_holding_is_refused_and_one_within_it_passes() {
    let mut e = engine(2.0, true);
    let mut outbox = EventBusOutbox::default();
    let mut sell = order("r1", -1, 1.0);
    (sell.order_type, sell.price) = (OrderType::Limit, Some(110.0));
    e.submit_order(&sell, 0, &mut outbox);
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "r1".into(), timestamp: 0 }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            order_client_id: "r1".into(),
            venue_order_id: Some("v1".into()),
            timestamp: 1,
        }),
        &mut e,
    );
    let rejected = |outbox: &mut EventBusOutbox| -> Vec<String> {
        outbox
            .0
            .drain(..)
            .filter_map(|event| match event {
                Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
                _ => None,
            })
            .collect()
    };

    e.modify_order("r1", Some(3.0), None, 2, &mut outbox);
    assert_eq!(
        rejected(&mut outbox),
        [
            "risk: a spot account cannot sell more than it holds: it has 2, this sells 3; nothing was sent"
        ]
    );
    assert!(e.client.modifies.is_empty(), "nothing reached the venue");
    assert_eq!(e.registry.get("r1").map(|m| m.request.qty), Some(1.0), "the order keeps its terms");

    e.modify_order("r1", Some(2.0), None, 3, &mut outbox);
    assert!(rejected(&mut outbox).is_empty());
    assert_eq!(e.client.modifies.len(), 1, "within the holding the modify goes");
}

/// **A hand trade on a spot pair stays out of the strategy's position** (the spot rule's answer 2, owner
/// 2026-11-11): a fill with NO order client id (the venue's app places an order with none, and each spot bridge
/// gives such a fill none) on a `Cash` symbol is not folded, whichever way it trades, so a hand sale of the user's
/// own coins can no longer leave the strategy short on spot. A fill of one of our orders, a settlement and a
/// reconcile leg (each carries an id) fold as before, and so does any fill on a perpetual.
#[test]
fn a_hand_trade_on_a_spot_pair_stays_out_of_the_strategys_position() {
    let fill = |tid: &'static str, order_client_id: &str, side: i32, qty: f64| {
        Event::Fill(FillEvent {
            trade_id: tid.into(),
            order_client_id: order_client_id.into(),
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            side,
            last_qty: qty,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: 1,
            mark_price: None,
            position_side: PositionSide::Both,
        })
    };
    let held = |e: &Engine<RecordingClient>| {
        let key = (
            vike_marketdata::Symbol::from(VENUE),
            vike_marketdata::Symbol::from(SYMBOL),
            PositionSide::Both,
        );
        e.account.positions.get(&key).map_or(0.0, |p| p.size)
    };
    let mut e = engine(2.0, true);
    for (tid, side, qty) in [("h1", -1, 3.0), ("h2", 1, 1.0)] {
        assert_eq!(
            e.on_event(&fill(tid, "", side, qty), &mut EventBusOutbox::default()),
            EventBusFold::Dropped
        );
    }
    assert_eq!(held(&e), 2.0, "the hand sale and the hand buy are the user's");
    for (tid, order_client_id) in
        [("o1", "abc123"), ("s1", "resolution:0xcond"), ("r1", "EXT-binance-DROP-x")]
    {
        let did = e.on_event(&fill(tid, order_client_id, -1, 0.5), &mut EventBusOutbox::default());
        assert_eq!(
            did,
            EventBusFold::Applied,
            "{order_client_id}: an order of ours, a settlement, a reconcile leg"
        );
    }
    assert_eq!(held(&e), 0.5);
    let mut perp = engine(2.0, false);
    let did = perp.on_event(&fill("p1", "", -1, 3.0), &mut EventBusOutbox::default());
    assert_eq!(did, EventBusFold::Applied, "a perpetual's fill with no client id folds as ever");
    assert_eq!(held(&perp), -1.0);
}

/// A PAPER `SYMBOL` book (its tier STATED) of `seed`, the `Cash` row, holding `long` at 100 with the mark at 100 and a
/// 0.2 % taker fee.
fn paper(seed: f64, long: f64) -> Engine<RecordingClient> {
    let mut e = engine(long, true);
    e.set_tier(vike_model::AccountTier::Paper);
    e.equity_seed = seed;
    e.fee_schedule = Some(vike_model::FeeSchedule::from_fractions(0.001, 0.002));
    e.price_board.set_mark(VENUE, SYMBOL, 100.0, 0);
    e
}

/// **A paper spot book cannot buy more than its free cash pays for, the fee included** (owner 2026-11-16): 1000 of
/// equity holding 2 coins at 100 has 800 free; a buy of 7.98 (798 and its 1.596 fee) goes; with it out, 2 is free and a
/// buy of 1 (100.20 with its fee) is refused in words, nothing sent. A sell is the oversell rule's alone.
#[test]
fn a_paper_spot_buy_past_its_free_cash_is_refused_with_its_fee_in_it() {
    let mut e = paper(1000.0, 2.0);
    let mut outbox = EventBusOutbox::default();
    e.submit_order(&order("b1", 1, 7.98), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "the buy the cash pays for goes");
    assert!(denied(&mut outbox).is_empty());

    e.submit_order(&order("b2", 1, 1.0), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "nothing more was sent");
    let words = vike_model::OrderDenial::CashOverbuy.template();
    assert_eq!(
        denied(&mut outbox),
        [format!("{words}: it has 2.00, this costs 100.20; nothing was sent")]
    );

    e.submit_order(&order("s1", -1, 1.0), 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 2, "a sell within the holding goes");
}

/// A resting limit buy `id` of `qty` at 100 on `e`, submitted and accepted.
fn rest_buy(e: &mut Engine<RecordingClient>, id: &str, qty: f64) {
    let mut buy = order(id, 1, qty);
    (buy.order_type, buy.price) = (OrderType::Limit, Some(100.0));
    let mut outbox = EventBusOutbox::default();
    e.submit_order(&buy, 0, &mut outbox);
    assert!(denied(&mut outbox).is_empty(), "the resting buy goes");
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { order_client_id: id.into(), timestamp: 0 }),
        e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            order_client_id: id.into(),
            venue_order_id: Some(format!("v-{id}").into()),
            timestamp: 1,
        }),
        e,
    );
}

/// The reasons of the modifies refused, drained from `outbox`.
fn modify_refusals(outbox: &mut EventBusOutbox) -> Vec<String> {
    outbox
        .0
        .drain(..)
        .filter_map(|event| match event {
            Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect()
}

/// **A MODIFY that makes a paper spot buy cost more is judged as a new buy**, its own cost left out of the buys already
/// out: 1000 of equity holding 2 coins at 100 has 800 free. A resting limit buy of 5 raised to 9 (901.80 with its fee) is
/// refused NON-terminally in words and keeps its terms, raised to 7 (701.40) it goes; a resting buy of 7 whose limit is
/// raised to 120 (841.68) is refused too.
#[test]
fn a_modify_raising_a_paper_spot_buy_past_its_free_cash_is_refused_and_one_within_it_passes() {
    let words = vike_model::OrderDenial::CashOverbuy.template();
    let mut e = paper(1000.0, 2.0);
    rest_buy(&mut e, "m1", 5.0);
    let mut outbox = EventBusOutbox::default();
    e.modify_order("m1", Some(9.0), None, 2, &mut outbox);
    assert_eq!(
        modify_refusals(&mut outbox),
        [format!("risk: {words}: it has 800.00, this costs 901.80; nothing was sent")]
    );
    assert!(e.client.modifies.is_empty(), "nothing reached the venue");
    assert_eq!(e.registry.get("m1").map(|m| m.request.qty), Some(5.0), "the order keeps its terms");
    e.modify_order("m1", Some(7.0), None, 3, &mut outbox);
    assert!(modify_refusals(&mut outbox).is_empty());
    assert_eq!(e.client.modifies.len(), 1, "within the free cash the modify goes");

    let mut dearer = paper(1000.0, 2.0);
    rest_buy(&mut dearer, "m2", 7.0);
    dearer.modify_order("m2", None, Some(120.0), 2, &mut outbox);
    assert_eq!(
        modify_refusals(&mut outbox),
        [format!("risk: {words}: it has 800.00, this costs 841.68; nothing was sent")]
    );
    assert!(dearer.client.modifies.is_empty());
}

/// **A paper account that also holds a derivative judges a spot buy against its FREE cash**, its equity less the margin
/// the derivative holds: 1000 of equity with a perpetual long of 5 at 100 (500 of margin at the full rate) has 500 free,
/// so a spot buy of 5 (501.00 with its fee) is refused and one of 4 (400.80) goes.
#[test]
fn a_paper_spot_buy_beside_a_derivative_is_judged_against_the_cash_its_margin_leaves() {
    let mut e = paper(1000.0, 0.0);
    e.account.positions.insert(
        (VENUE.into(), "BTCUSDT.P".into(), PositionSide::Both),
        PositionEntry { size: 5.0, average_price: 100.0, ..Default::default() },
    );
    e.price_board.set_mark(VENUE, "BTCUSDT.P", 100.0, 0);
    let mut outbox = EventBusOutbox::default();
    e.submit_order(&order("b1", 1, 5.0), 1, &mut outbox);
    let words = vike_model::OrderDenial::CashOverbuy.template();
    assert_eq!(
        denied(&mut outbox),
        [format!("{words}: it has 500.00, this costs 501.00; nothing was sent")]
    );
    e.submit_order(&order("b2", 1, 4.0), 1, &mut outbox);
    assert!(denied(&mut outbox).is_empty());
    assert_eq!(e.client.submissions.len(), 1, "the buy the free cash pays for goes");
}

/// The same buy past the cash on an engine whose tier is no STATED paper (a live venue refuses from its own wallet),
/// and on a paper perpetual (no `Cash` row), is judged as before: sent.
#[test]
fn a_buy_past_the_cash_off_a_paper_spot_book_is_judged_as_before() {
    let mut demo = paper(1000.0, 2.0);
    demo.set_tier(vike_model::AccountTier::Demo);
    let mut perp = engine(2.0, false);
    perp.set_tier(vike_model::AccountTier::Paper);
    perp.equity_seed = 1000.0;
    perp.price_board.set_mark(VENUE, SYMBOL, 100.0, 0);
    for e in [&mut demo, &mut perp] {
        let mut outbox = EventBusOutbox::default();
        e.submit_order(&order("b1", 1, 50.0), 1, &mut outbox);
        assert_eq!(e.client.submissions.len(), 1);
        assert!(denied(&mut outbox).is_empty());
    }
}
