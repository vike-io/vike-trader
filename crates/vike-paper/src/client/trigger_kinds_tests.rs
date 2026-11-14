//! The stop-limit and if-touched kinds at the paper book: a `stop_limit` and a `take_profit` rest
//! as their own kinds (`vike_model::OrderResting::from_request`) and fill by `vike_model`'s
//! `order_fill_price` law, never at the next open as a market order (what the book did before the
//! kinds existed); a release to the `Limit` is said with `OrderTriggered`; a trigger kind without
//! its terms is refused at the submit; an order the bracket cap leaves nothing ends in an
//! `OrderCanceled`, never silently; a modify's price moves the price the order's FSM moves;
//! `reduce_only` caps nothing (decision 0237 item 5). A trigger amend (`modify_trigger`) moves an
//! untriggered order's trigger where the FSM moves it, and a fired one refuses it in the engine's
//! words.
use super::test_support::{bar, drain};
use super::*;
use vike_execution::ExecutionClient;
use vike_execution::execution_engine::TRIGGER_AMEND_NO_TRIGGER_REASON;
use vike_model::{OrderKind, OrderRecord, OrderRequest, OrderResting, OrderStatus, OrderType};

fn req(
    order_client_id: &str,
    order_type: OrderType,
    side: i32,
    trigger: Option<f64>,
    limit: Option<f64>,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type,
        price: limit,
        trigger_price: trigger,
        ..Default::default()
    }
}

/// A bar that reaches no trigger of the rows below (high 100.5, low 99.5).
fn quiet() -> Bar {
    bar(1_000, 100.0, 100.5, 99.5, 100.0)
}

fn terms(c: &ExecutionClientPaper, order_client_id: &str) -> (OrderKind, Option<f64>, Option<f64>) {
    let (_, o) = c.pending.iter().find(|(c, _)| c == order_client_id).expect("the order rests");
    (o.kind, o.price, o.limit)
}

/// The regression: before the two kinds existed, `OrderResting::from_request` lowered both to a
/// MARKET order and the next bar filled it at its open. Now nothing fills at the submit, nothing
/// fills on a bar that reaches no trigger, and each rests as its own kind with its own terms.
#[test]
fn neither_kind_ever_fills_at_submit_as_a_market_order() {
    use OrderType::{StopLimit, TakeProfit};
    let rows = [
        ("stop-limit buy", StopLimit, 1, 101.0, Some(101.5), OrderKind::StopLimit),
        ("stop-limit sell", StopLimit, -1, 99.0, Some(98.5), OrderKind::StopLimit),
        ("market-if-touched buy", TakeProfit, 1, 99.0, None, OrderKind::TakeProfit),
        ("market-if-touched sell", TakeProfit, -1, 101.0, None, OrderKind::TakeProfit),
        ("limit-if-touched buy", TakeProfit, 1, 99.0, Some(98.5), OrderKind::TakeProfit),
    ];
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    for (case, order_type, side, trigger, limit, _) in rows.clone() {
        c.submit(&req(case, order_type, side, Some(trigger), limit));
        let events = drain(&mut c);
        assert!(
            matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
            "{case}: {events:?}"
        );
    }
    c.on_bar(&quiet());
    assert!(drain(&mut c).is_empty(), "no trigger reached: no event");
    assert!(c.fills.lock().unwrap().is_empty(), "no trigger reached: no fill");
    for (case, _, _, trigger, limit, kind) in rows {
        assert_eq!(terms(&c, case), (kind, Some(trigger), limit), "{case}: rests as its kind");
    }
}

/// Each kind rests through a bar that reaches none of its triggers (the first row's limit is
/// reachable on it, and still nothing fills), then fills on the bar that triggers it, at the law's
/// price, as a taker.
#[test]
fn each_kind_fills_per_the_law_on_the_bar_that_triggers_it() {
    use OrderType::{StopLimit, TakeProfit};
    #[rustfmt::skip]
    let rows = [
        ("stop-limit buy, L one tick below T", StopLimit, 1, 101.0, Some(100.0), [99.0, 102.0, 98.0, 101.0], 100.0),
        ("stop-limit sell, L below T: fills at A = T", StopLimit, -1, 99.0, Some(97.0), [101.0, 102.0, 98.0, 99.0], 99.0),
        ("market-if-touched buy, gap down past T: A = open", TakeProfit, 1, 99.0, None, [97.0, 98.0, 96.0, 97.0], 97.0),
        ("market-if-touched sell, touched: fills at T", TakeProfit, -1, 101.0, None, [99.0, 102.0, 98.0, 100.0], 101.0),
        ("limit-if-touched sell, L above T reached", TakeProfit, -1, 101.0, Some(101.5), [99.0, 102.0, 98.0, 100.0], 101.5),
    ];
    for (case, order_type, side, trigger, limit, [o, h, l, cl], px) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
        c.submit(&req(case, order_type, side, Some(trigger), limit));
        c.on_bar(&quiet());
        assert!(c.fills.lock().unwrap().is_empty(), "{case}: not triggered yet");
        drain(&mut c);
        c.on_bar(&bar(2_000, o, h, l, cl));
        let fills = c.fills.lock().unwrap().clone();
        assert_eq!(fills.len(), 1, "{case}: {fills:?}");
        assert_eq!((fills[0].side, fills[0].size, fills[0].price), (side, 1.0, px), "{case}");
        assert!(!fills[0].is_maker, "{case}: the trigger event books a taker");
        let events = drain(&mut c);
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderFilled(f) if f.order_client_id == case)),
            "{case}: OrderFilled"
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::OrderTriggered(_))),
            "{case}: a fill on the trigger event releases nothing"
        );
        assert!(c.pending.is_empty(), "{case}: filled, gone from the book");
    }
}

/// A triggered order whose limit the bar does not reach rests, in place, as the plain `Limit` at
/// L, says so with ONE `OrderTriggered` (its FSM goes `Triggered`), and fills later by the `Limit`
/// law, as a maker.
#[test]
fn a_triggered_order_short_of_its_limit_rests_as_the_limit_and_fills_later_as_a_maker() {
    use OrderType::{StopLimit, TakeProfit};
    #[rustfmt::skip]
    let rows = [
        ("stop-limit buy gaps past L", StopLimit, 101.0, 100.0, [103.0, 105.0, 102.0, 104.0], [99.0, 101.0, 98.0, 100.0], 99.0),
        ("limit-if-touched buy short of L", TakeProfit, 99.0, 98.0, [101.0, 102.0, 98.5, 100.0], [98.5, 99.0, 97.0, 98.0], 98.0),
    ];
    for (case, order_type, trigger, limit, [o1, h1, l1, c1], [o2, h2, l2, c2], px) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
        let r = req(case, order_type, 1, Some(trigger), Some(limit));
        let mut fsm = OrderRecord::new(r.clone());
        c.submit(&r);
        c.on_bar(&bar(1_000, o1, h1, l1, c1));
        assert!(c.fills.lock().unwrap().is_empty(), "{case}: triggered, limit not reached");
        assert_eq!(terms(&c, case), (OrderKind::Limit, Some(limit), None), "{case}: released");
        let events = drain(&mut c);
        let [Event::OrderSubmitted(_), Event::OrderAccepted(_), Event::OrderTriggered(t)] =
            events.as_slice()
        else {
            panic!("{case}: the release is said once: {events:?}");
        };
        assert_eq!((t.order_client_id.as_str(), t.timestamp), (case, 1_000), "{case}");
        apply_all(&mut fsm, &events);
        assert_eq!(fsm.status, OrderStatus::Triggered, "{case}");
        c.on_bar(&bar(2_000, o2, h2, l2, c2));
        let fills = c.fills.lock().unwrap().clone();
        assert_eq!(fills.len(), 1, "{case}: {fills:?}");
        assert_eq!(fills[0].price, px, "{case}");
        assert!(fills[0].is_maker, "{case}: the released Limit books a maker");
        apply_all(&mut fsm, &drain(&mut c));
        assert_eq!(fsm.status, OrderStatus::Filled, "{case}");
    }
}

/// The order client id of the lifecycle events this book emits; a bare `Fill` is not one.
fn order_client_id_of(e: &Event) -> Option<&str> {
    let order_client_id: &String = match e {
        Event::OrderSubmitted(x) => &x.order_client_id,
        Event::OrderAccepted(x) => &x.order_client_id,
        Event::OrderRejected(x) => &x.order_client_id,
        Event::OrderTriggered(x) => &x.order_client_id,
        Event::OrderPartiallyFilled(x) => &x.order_client_id,
        Event::OrderFilled(x) => &x.order_client_id,
        Event::OrderCanceled(x) => &x.order_client_id,
        Event::OrderModified(x) => &x.order_client_id,
        _ => return None,
    };
    Some(order_client_id.as_str())
}

/// Fold `events` into `fsm`: every lifecycle event naming it must be a legal transition.
pub(super) fn apply_all(fsm: &mut OrderRecord, events: &[Event]) {
    let order_client_id = fsm.order_client_id().to_string();
    for e in events.iter().filter(|e| order_client_id_of(e) == Some(order_client_id.as_str())) {
        fsm.apply(e).unwrap();
    }
}

/// The reason of the `OrderCanceled` naming `order_client_id`, which must be the only one.
fn cancel_reason(events: &[Event], order_client_id: &str) -> String {
    let reasons: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(x) if x.order_client_id == order_client_id => {
                Some(x.reason.to_string())
            }
            _ => None,
        })
        .collect();
    let [reason] = reasons.as_slice() else {
        panic!("{order_client_id}: one cancel, got {reasons:?}")
    };
    reason.clone()
}

/// A bracket whose stop and target both hit in one bar: the stop fills, the cap leaves the target
/// nothing, and the target ends as the OCO cancel the stop's fill owes it, so every leg's FSM is
/// terminal and nothing rests. Before, the target left the book with no event and its FSM stayed
/// `Accepted` forever.
#[test]
fn a_bracket_whose_stop_and_target_hit_in_one_bar_ends_every_leg() {
    use vike_model::{OrderBracket, build_bracket};
    let spec = OrderBracket {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 110.0,
        account: None,
        order_client_id: None,
    };
    let legs = build_bracket(&spec, "entry", "sl", "tp");
    let mut fsms: Vec<OrderRecord> = legs.iter().cloned().map(OrderRecord::new).collect();
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    for leg in &legs {
        c.submit(leg);
    }
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // the entry fills; the exits arm
    c.on_bar(&bar(2_000, 100.0, 112.0, 88.0, 100.0)); // reaches the stop AND the target
    let events = drain(&mut c);
    let got: Vec<_> = c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect();
    assert_eq!(got, [(1, 1.0, 100.0), (-1, 1.0, 90.0)], "the stop first; the target capped");
    assert_eq!(cancel_reason(&events, "tp"), "oco");
    for fsm in &mut fsms {
        apply_all(fsm, &events);
        assert!(fsm.status.is_terminal(), "{} ends: {:?}", fsm.order_client_id(), fsm.status);
    }
    assert!(c.pending.is_empty(), "nothing rests");
}

/// An exit with no links that the cap leaves nothing ends in words too.
#[test]
fn an_unlinked_exit_the_cap_leaves_nothing_is_cancelled_in_words() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let mut entry = req("entry", OrderType::Market, 1, None, None);
    entry.qty = 2.0;
    c.submit(&entry);
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // long 2 at 100
    let mut stop_limit = req("sl", OrderType::StopLimit, -1, Some(95.0), Some(94.0));
    let mut take_profit = req("tp", OrderType::TakeProfit, -1, Some(105.0), None);
    stop_limit.qty = 2.0;
    take_profit.qty = 2.0;
    c.submit(&stop_limit);
    c.submit(&take_profit);
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 106.0, 93.0, 100.0)); // both exits; the stop takes all 2
    assert_eq!(cancel_reason(&drain(&mut c), "tp"), "nothing-to-fill");
    assert!(c.pending.is_empty() && c.expiry.is_empty());
}

/// One-cancels-other inside one bar, on a position LARGER than the exits: whichever leg fills
/// first, its OCO sibling is cancelled (`oco`) and fills nothing. Before, the second filled what
/// the cap left it, breaking the link. Neither leg is reduce-only, so both are order-list ENTRIES
/// and fill in the bar's path order, the cap allocated in it: the high (106) is nearer than the
/// low, so the target at 105 fills first, its whole 2 of the long's 3, and the stop is cancelled.
// The owner's 2026-11-08 ruling (a plain stop is a plain order); paper and simulator never disagree.
#[test]
fn an_oco_exit_never_fills_after_its_sibling_filled_in_the_same_bar() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let mut entry = req("entry", OrderType::Market, 1, None, None);
    entry.qty = 3.0;
    c.submit(&entry);
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // long 3 at 100
    let link = |mut r: OrderRequest, sibling: &str| {
        r.qty = 2.0;
        r.contingency_type = Some(vike_model::ContingencyType::Oco);
        r.linked_order_ids = vec![sibling.into()];
        r
    };
    let stop_limit = link(req("sl", OrderType::StopLimit, -1, Some(95.0), Some(94.0)), "tp");
    let take_profit = link(req("tp", OrderType::TakeProfit, -1, Some(105.0), None), "sl");
    let mut fsms = [OrderRecord::new(stop_limit.clone()), OrderRecord::new(take_profit.clone())];
    c.submit(&stop_limit);
    c.submit(&take_profit);
    c.on_bar(&bar(2_000, 100.0, 106.0, 93.0, 100.0)); // reaches both exits
    let got: Vec<_> = c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect();
    assert_eq!(got, [(1, 3.0, 100.0), (-1, 2.0, 105.0)], "the target fills; the stop does not");
    let events = drain(&mut c);
    assert_eq!(cancel_reason(&events, "sl"), "oco");
    for fsm in &mut fsms {
        apply_all(fsm, &events);
        assert!(fsm.status.is_terminal(), "{}: {:?}", fsm.order_client_id(), fsm.status);
    }
    assert!(c.pending.is_empty());
}

/// An order with no size to fill is refused at the submit, never rested; a modify that takes a
/// resting order's size away ends it in words at its trigger.
#[test]
fn an_order_with_no_size_is_refused_and_one_modified_to_none_ends_in_words() {
    for qty in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        let mut r = req("q", OrderType::Limit, 1, None, Some(99.0));
        r.qty = qty;
        c.submit(&r);
        let events = drain(&mut c);
        let [Event::OrderSubmitted(_), Event::OrderRejected(reject)] = events.as_slice() else {
            panic!("qty {qty}: Submitted then Rejected, got {events:?}");
        };
        assert!(reject.reason.contains("is not a finite number above zero"), "{}", reject.reason);
        assert!(c.pending.is_empty(), "qty {qty}: nothing rests");
    }
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let r = req("q", OrderType::Limit, 1, None, Some(99.0));
    c.submit(&r);
    c.modify(&r, Some(0.0), None);
    drain(&mut c);
    c.on_bar(&bar(1_000, 100.0, 100.0, 98.0, 99.0)); // reaches its limit with nothing to fill
    assert_eq!(cancel_reason(&drain(&mut c), "q"), "nothing-to-fill");
    assert!(c.pending.is_empty() && c.fills.lock().unwrap().is_empty());
}

/// A trigger kind without the terms it fires on is refused at the submit
/// (`vike_model::order_terms_refusal`), in the book's refusal shape, and never rests: before, the
/// next bar panicked the book on the missing trigger.
#[test]
fn a_trigger_kind_without_its_terms_is_refused_at_submit() {
    #[rustfmt::skip]
    let rows = [
        ("stop-limit, no trigger", OrderType::StopLimit, None, Some(100.0), "needs a trigger price above zero"),
        ("take-profit, no trigger", OrderType::TakeProfit, None, None, "needs a trigger price above zero"),
        ("stop, no trigger", OrderType::Stop, None, None, "needs a trigger price above zero"),
        ("stop-limit, no limit", OrderType::StopLimit, Some(101.0), None, "needs a limit price above zero"),
    ];
    for (case, order_type, trigger, limit, words) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        c.submit(&req(case, order_type, 1, trigger, limit));
        let events = drain(&mut c);
        let [Event::OrderSubmitted(_), Event::OrderRejected(r)] = events.as_slice() else {
            panic!("{case}: Submitted then Rejected, got {events:?}");
        };
        assert_eq!(r.cause, None, "{case}");
        assert!(r.reason.contains(words), "{case}: {}", r.reason);
        assert!(c.pending.is_empty(), "{case}: nothing rests");
        c.on_bar(&bar(1_000, 100.0, 110.0, 90.0, 100.0));
        assert!(drain(&mut c).is_empty(), "{case}: and the next bar has nothing to do");
    }
}

/// A stop-limit exit and a take-profit exit triggered in one bar: the stop-limit (adverse) fills
/// first and the take-profit is capped to what is left of the position (`resolve_intrabar_fills`).
#[test]
fn a_stop_limit_and_a_take_profit_exit_in_one_bar_keep_the_bracket_cap() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    let mut entry = req("entry", OrderType::Market, 1, None, None);
    entry.qty = 3.0;
    c.submit(&entry);
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // long 3 at 100
    let mut stop_limit = req("sl", OrderType::StopLimit, -1, Some(95.0), Some(94.0));
    let mut take_profit = req("tp", OrderType::TakeProfit, -1, Some(105.0), None);
    stop_limit.qty = 2.0;
    take_profit.qty = 2.0;
    c.submit(&stop_limit);
    c.submit(&take_profit);
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 106.0, 93.0, 100.0)); // reaches both triggers
    let got: Vec<_> = c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect();
    assert_eq!(got, [(1, 3.0, 100.0), (-1, 2.0, 95.0), (-1, 1.0, 105.0)]);
    let events = drain(&mut c);
    assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(f) if f.order_client_id == "sl")));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderPartiallyFilled(f) if f.order_client_id == "tp"))
    );
}

/// A modify's price moves what the order's FSM moves (`vike_model::modified_price_is_trigger`): a
/// stop's trigger, every other kind's limit. The book is driven with its own events into a
/// `OrderRecord`, and the resting order is the lowering of the FSM's request after the modify.
#[test]
fn a_modify_moves_the_price_the_state_machine_moves() {
    use OrderKind as K;
    use OrderType::{Limit, Stop, StopLimit, TakeProfit};
    #[rustfmt::skip]
    let rows = [
        ("stop-limit: the limit moves, the trigger stays", StopLimit, Some(101.0), Some(100.0), 100.5, (K::StopLimit, Some(101.0), Some(100.5))),
        ("limit-if-touched: the limit moves", TakeProfit, Some(99.0), Some(98.5), 98.75, (K::TakeProfit, Some(99.0), Some(98.75))),
        ("market-if-touched: gains a limit, as the FSM's request does", TakeProfit, Some(99.0), None, 98.5, (K::TakeProfit, Some(99.0), Some(98.5))),
        ("stop: the trigger moves", Stop, Some(101.0), None, 101.5, (K::Stop, Some(101.5), None)),
        ("limit: the limit moves", Limit, None, Some(90.0), 95.0, (K::Limit, Some(95.0), None)),
    ];
    for (case, order_type, trigger, limit, new_price, after) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        let r = req(case, order_type, 1, trigger, limit);
        let mut fsm = OrderRecord::new(r.clone());
        c.submit(&r);
        for e in drain(&mut c) {
            fsm.apply(&e).unwrap();
        }
        c.modify(&fsm.request, None, Some(new_price));
        let events = drain(&mut c);
        assert!(matches!(events.as_slice(), [Event::OrderModified(_)]), "{case}: {events:?}");
        fsm.apply(&events[0]).unwrap();
        assert_eq!(terms(&c, case), after, "{case}");
        let (_, resting) = &c.pending[0];
        assert_eq!(*resting, OrderResting::from_request(&fsm.request), "{case}: book == FSM");
    }
}

/// The modified limit is the one the next bar fills at: untriggered (the old book moved the
/// TRIGGER to 100.5 and filled at the old limit, 100), and once released as the `Limit`.
#[test]
fn a_modified_stop_limit_fills_at_its_new_limit_before_and_after_the_release() {
    let stop_limit = req("sl", OrderType::StopLimit, 1, Some(101.0), Some(100.0));
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&stop_limit);
    c.modify(&stop_limit, None, Some(100.5));
    c.on_bar(&bar(1_000, 99.0, 102.0, 98.0, 101.0));
    assert_eq!(c.fills.lock().unwrap()[0].price, 100.5, "min(L = 100.5, A = 101)");

    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&stop_limit);
    c.on_bar(&bar(1_000, 103.0, 105.0, 102.0, 104.0)); // triggered past L: released at 100
    c.modify(&stop_limit, None, Some(101.0));
    assert_eq!(terms(&c, "sl"), (OrderKind::Limit, Some(101.0), None));
    c.on_bar(&bar(2_000, 102.0, 103.0, 100.5, 101.0));
    let fills = c.fills.lock().unwrap().clone();
    assert_eq!((fills.len(), fills[0].price, fills[0].is_maker), (1, 101.0, true));
}

/// `reduce_only` caps nothing (decision 0237 item 5; it only tells an order-list entry from an
/// exit inside one bar): on a flat book a reduce-only stop-limit or take-profit fires exactly as
/// the plain one does, opening a position.
#[test]
fn reduce_only_is_unread_and_a_reduce_only_order_fires_as_the_plain_one() {
    #[rustfmt::skip]
    let rows = [
        (OrderType::StopLimit, Some(99.0), Some(98.0), [101.0, 102.0, 97.0, 98.0]),
        (OrderType::TakeProfit, Some(101.0), None, [99.0, 102.0, 98.0, 100.0]),
    ];
    for (order_type, trigger, limit, [o, h, l, cl]) in rows {
        let fills_of = |reduce_only: bool| {
            let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
            let mut r = req("x", order_type.clone(), -1, trigger, limit);
            r.reduce_only = reduce_only;
            c.submit(&r);
            c.on_bar(&quiet());
            c.on_bar(&bar(2_000, o, h, l, cl));
            c.fills.lock().unwrap().clone()
        };
        let plain = fills_of(false);
        assert_eq!(plain.len(), 1, "{order_type}: {plain:?}");
        assert_eq!(fills_of(true), plain, "{order_type}: the flag changes nothing");
    }
}

/// A trigger amend moves an untriggered order's trigger, and its size and limit when named, where
/// the FSM moves them (`vike_model::modified_prices`), and says so with ONE `OrderModified`
/// carrying the trigger: the resting order is the lowering of the FSM's request after the event. A
/// stop's one price is its trigger, so a `new_price` beside the trigger is not read.
#[test]
fn a_trigger_amend_moves_an_untriggered_orders_trigger_where_the_state_machine_moves_it() {
    use OrderKind as K;
    use OrderType::{Stop, StopLimit, TakeProfit};
    #[rustfmt::skip]
    let rows = [
        ("stop-limit: the trigger and the limit", StopLimit, 101.0, Some(100.5), None, Some(101.5), 102.0, (K::StopLimit, Some(102.0), Some(101.5))),
        ("stop-limit: the trigger alone, and the size", StopLimit, 101.0, Some(100.5), Some(2.0), None, 102.0, (K::StopLimit, Some(102.0), Some(100.5))),
        ("limit-if-touched: the trigger", TakeProfit, 99.0, Some(98.5), None, None, 98.0, (K::TakeProfit, Some(98.0), Some(98.5))),
        ("market-if-touched: the trigger", TakeProfit, 99.0, None, None, None, 98.0, (K::TakeProfit, Some(98.0), None)),
        ("stop: the trigger, its new_price unread", Stop, 101.0, None, None, Some(100.0), 101.5, (K::Stop, Some(101.5), None)),
    ];
    for (case, order_type, trigger, limit, new_qty, new_price, new_trigger, after) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        let r = req(case, order_type, 1, Some(trigger), limit);
        let mut fsm = OrderRecord::new(r.clone());
        c.submit(&r);
        apply_all(&mut fsm, &drain(&mut c));
        assert_eq!(
            c.modify_trigger(&fsm.request, new_qty, new_price, new_trigger),
            Ok(()),
            "{case}"
        );
        let events = drain(&mut c);
        let [Event::OrderModified(m)] = events.as_slice() else {
            panic!("{case}: one OrderModified, got {events:?}");
        };
        assert_eq!(
            (m.new_qty, m.new_price, m.new_trigger_price),
            (new_qty, new_price, Some(new_trigger)),
            "{case}"
        );
        fsm.apply(&events[0]).unwrap();
        assert_eq!(terms(&c, case), after, "{case}");
        assert_eq!(c.pending[0].1, OrderResting::from_request(&fsm.request), "{case}: book == FSM");
    }
}

/// The moved trigger is the one the next bar fires at: the old trigger is reached and nothing
/// happens, the new one fires the order by the stop-limit law.
#[test]
fn the_moved_trigger_is_the_one_the_next_bar_fires_at() {
    let stop_limit = req("sl", OrderType::StopLimit, 1, Some(101.0), Some(101.5));
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&stop_limit);
    assert_eq!(c.modify_trigger(&stop_limit, None, Some(103.0), 102.5), Ok(()));
    c.on_bar(&bar(1_000, 100.0, 102.0, 99.0, 101.0)); // reaches the old trigger only
    assert!(c.fills.lock().unwrap().is_empty(), "the old trigger fires nothing");
    assert_eq!(terms(&c, "sl"), (OrderKind::StopLimit, Some(102.5), Some(103.0)));
    c.on_bar(&bar(2_000, 102.0, 104.0, 101.0, 103.0)); // T 102.5: A = 102.5, L 103 reached
    let fills = c.fills.lock().unwrap().clone();
    assert_eq!((fills.len(), fills[0].price, fills[0].is_maker), (1, 102.5, false));
}

/// A stop-limit whose trigger has fired rests as the plain `Limit` it released, and a limit has no
/// trigger: a trigger amend of either is refused in the engine's words
/// (`TRIGGER_AMEND_NO_TRIGGER_REASON`), with nothing moved and no event.
#[test]
fn a_fired_stop_limit_refuses_a_trigger_amend_in_the_engines_words() {
    let stop_limit = req("sl", OrderType::StopLimit, 1, Some(101.0), Some(100.0));
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&stop_limit);
    c.on_bar(&bar(1_000, 103.0, 105.0, 102.0, 104.0)); // fired past L: released as the Limit at 100
    let limit = req("limit", OrderType::Limit, 1, None, Some(90.0));
    c.submit(&limit);
    drain(&mut c);
    for (order_client_id, r, rests) in
        [("sl", &stop_limit, Some(100.0)), ("limit", &limit, Some(90.0))]
    {
        assert_eq!(
            c.modify_trigger(r, Some(3.0), Some(99.0), 102.0),
            Err(TRIGGER_AMEND_NO_TRIGGER_REASON),
            "{order_client_id}"
        );
        assert_eq!(
            terms(&c, order_client_id),
            (OrderKind::Limit, rests, None),
            "{order_client_id}: nothing moved"
        );
        assert_eq!(
            c.pending.iter().find(|(x, _)| x == order_client_id).unwrap().1.size,
            1.0,
            "{order_client_id}"
        );
    }
    assert!(drain(&mut c).is_empty(), "the refusal is the caller's to publish");
}

/// The multi-symbol book routes a trigger amend to the order's book by hand (the trait's default
/// would refuse it there), and one for a symbol no book holds is the no-op `modify` makes.
#[test]
fn the_multi_book_routes_a_trigger_amend_to_the_orders_book() {
    let mut m = crate::ExecutionClientPaperMulti::new();
    m.add_book(ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0));
    let stop_limit = req("sl", OrderType::StopLimit, 1, Some(101.0), Some(100.5));
    m.submit(&stop_limit);
    assert_eq!(m.modify_trigger(&stop_limit, None, None, 102.0), Ok(()));
    let events: Vec<Event> = std::iter::from_fn(|| m.poll_events()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::OrderModified(x) if x.new_trigger_price == Some(102.0))),
        "{events:?}"
    );
    let elsewhere = OrderRequest { symbol: "ETHUSDT".into(), ..stop_limit.clone() };
    assert_eq!(m.modify_trigger(&elsewhere, None, None, 103.0), Ok(()));
    assert!(m.poll_events().is_none(), "no book holds it: nothing moves");
}
