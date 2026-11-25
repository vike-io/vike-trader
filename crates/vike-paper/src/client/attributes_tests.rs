//! The attributes wave at the paper book: a `trailing_take_profit`, a percent trail and a stop's
//! adjustment rest as their own kinds and fire by `vike_model`'s `order_fill_price` law (a percent
//! trail by `OrderResting::trail_distance`, walked along the bar's path when it has an activation);
//! an adjustment says its new trigger with one `OrderModified`, which the FSM folds. All-or-none,
//! sweep-to-fill and a hidden order (a display of 0) rest as the plain limit of their full size,
//! which this book fills whole, and an all-or-none order the bracket cap or an OUO sibling would cut
//! in a bar fills nothing there and rests on whole; a discretionary limit is judged at L ± d and
//! books as a taker. Refused at the submit, in words:
//! what the shared rules refuse (`order_terms_refusal`, `attributes_refusal`, `stop_adjust_refusal`), a
//! trailing take-profit before any price, an opening auction's market-to-limit (no session
//! calendar), and a release whose price condition names a venue. A price amend and a trigger amend
//! of a trailing take-profit or an adjustable stop are refused; a size amend moves.
use super::test_support::{bar, drain};
use super::trigger_kinds_tests::apply_all;
use super::*;
use vike_execution::ExecutionClient;
use vike_execution::execution_engine::{ADJUSTABLE_AMEND_REASON, TRAILING_AMEND_REASON};
use vike_model::OrderType::{Limit, Market, Stop, StopLimit, Trailing, TrailingLimit};
use vike_model::OrderType::{MarketToLimit, TrailingTakeProfit};
use vike_model::{ContingencyType, OrderRecord, OrderStatus};
use vike_model::{
    OrderAttributes, OrderKind, OrderRelease, OrderReleasePrice, OrderReleasePriceSide,
    OrderRequest, OrderStopAdjust, OrderStopAdjustTo, OrderType, TimeInForce,
};

/// A one-lot BTCUSDT order of `order_type`, no terms.
fn req(order_client_id: &str, order_type: OrderType, side: i32) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type,
        ..Default::default()
    }
}

/// A book with no slippage that has seen one bar, closing at 100: the price a trailing order
/// submitted now trails from.
fn seeded() -> ExecutionClientPaper {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
    c.on_bar(&bar(1_000, 99.0, 101.0, 98.0, 100.0));
    c
}

fn at(ts: i64, [o, h, l, c]: [f64; 4]) -> Bar {
    bar(ts, o, h, l, c)
}

/// `(ts, px, is_maker)` of every fill.
fn fills(c: &ExecutionClientPaper) -> Vec<(i64, f64, bool)> {
    c.fills.lock().unwrap().iter().map(|f| (f.timestamp, f.price, f.is_maker)).collect()
}

fn said_triggered(events: &[Event]) -> usize {
    events.iter().filter(|e| matches!(e, Event::OrderTriggered(_))).count()
}

/// Bars 2_000, 3_000, ... after the submit; what the book said meanwhile.
fn run(c: &mut ExecutionClientPaper, bars: &[[f64; 4]]) -> Vec<Event> {
    let mut events = drain(c);
    for (i, b) in bars.iter().enumerate() {
        c.on_bar(&at(2_000 + 1_000 * i as i64, *b));
        events.extend(drain(c));
    }
    events
}

/// A trailing take-profit, trail 2, trails from the last close (100): a SELL tracks the low and
/// fires when the price rises its trail above it, a BUY tracks the high and fires on a fall, each
/// at its trigger or the open a gap carried past it, a taker; with a limit offset (IB `TRAIL LIT`)
/// its limit sits that far past the trigger, which a bar that reached the trigger always reaches.
#[test]
fn a_trailing_take_profit_trails_from_the_last_close_and_fires_by_the_law() {
    const SELL_RATCHET: [f64; 4] = [100.0, 101.0, 97.0, 98.0]; // low 97: trigger 99
    const BUY_RATCHET: [f64; 4] = [100.0, 103.0, 99.0, 102.0]; // high 103: trigger 101
    #[rustfmt::skip]
    let rows = [
        ("sell: at the trigger", -1, None, [SELL_RATCHET, [98.0, 99.5, 97.5, 99.0]], 99.0, 97.0),
        ("sell, a gap up past the trigger: at the open", -1, None, [SELL_RATCHET, [100.0, 101.0, 99.5, 100.0]], 100.0, 97.0),
        ("sell, offset 0.5: its limit 98.5 reached, at the trigger", -1, Some(0.5), [SELL_RATCHET, [98.0, 99.5, 97.5, 99.0]], 99.0, 97.0),
        ("buy: at the trigger", 1, None, [BUY_RATCHET, [102.0, 102.5, 100.5, 101.0]], 101.0, 103.0),
        ("buy, a gap down past the trigger: at the open", 1, None, [BUY_RATCHET, [100.0, 100.5, 99.0, 100.0]], 100.0, 103.0),
        ("buy, offset 0.5: its limit 101.5 reached, at the trigger", 1, Some(0.5), [BUY_RATCHET, [102.0, 102.5, 100.5, 101.0]], 101.0, 103.0),
    ];
    for (case, side, offset, [ratchet, fire], px, ratcheted) in rows {
        let mut c = seeded();
        let r = OrderRequest {
            trail: Some(2.0),
            limit_offset: offset,
            ..req(case, TrailingTakeProfit, side)
        };
        c.submit(&r);
        let (_, o) = &c.pending[0];
        assert_eq!((o.kind, o.extreme), (OrderKind::TrailingTakeProfit, Some(100.0)), "{case}");
        let mut events = run(&mut c, &[ratchet]);
        assert!(fills(&c).is_empty(), "{case}: not fired yet");
        assert_eq!(c.pending[0].1.extreme, Some(ratcheted), "{case}: ratcheted");
        c.on_bar(&at(3_000, fire));
        events.extend(drain(&mut c));
        assert_eq!(fills(&c), [(3_000, px, false)], "{case}");
        assert_eq!(said_triggered(&events), 0, "{case}: a fill on the fire releases nothing");
        assert!(c.pending.is_empty(), "{case}: filled, gone from the book");
    }
}

/// A trailing take-profit has no price to move: a modify naming one is refused whole in the
/// trailing words, a trigger amend too, and its size alone moves.
#[test]
fn a_trailing_take_profit_refuses_a_price_amend_and_a_trigger_amend() {
    let mut c = seeded();
    let r = OrderRequest { trail: Some(2.0), ..req("ttp", TrailingTakeProfit, -1) };
    c.submit(&r);
    drain(&mut c);
    c.modify(&r, Some(2.0), Some(99.0));
    let events = drain(&mut c);
    let [Event::OrderModifyRejected(m)] = events.as_slice() else {
        panic!("one OrderModifyRejected, got {events:?}");
    };
    assert_eq!(m.reason.to_string(), TRAILING_AMEND_REASON);
    assert_eq!(c.modify_trigger(&r, None, None, 105.0), Err(TRAILING_AMEND_REASON));
    c.modify(&r, Some(2.0), None);
    assert!(matches!(drain(&mut c).as_slice(), [Event::OrderModified(_)]));
    assert_eq!(c.pending[0].1.size, 2.0, "its size moved");
}

/// A percent trail of 2 trails by 2% of the extreme it reads (`OrderResting::trail_distance`), for
/// each trailing kind: from a close of 100 it fires where a price trail of 2 fires, and once the
/// extreme ratcheted its distance moved with it. One with an activation is walked along the bar's
/// path and arms and fires inside one bar.
#[test]
fn a_percent_trail_trails_by_its_percent_of_the_extreme() {
    let pct = |extreme: f64| extreme * 2.0 / 100.0;
    const UP: [f64; 4] = [100.0, 104.0, 99.0, 103.0]; // a sell's high to 104
    const DOWN: [f64; 4] = [100.0, 101.0, 96.0, 97.0]; // a buy's low to 96
    const TTP_DOWN: [f64; 4] = [100.0, 101.0, 97.0, 98.0]; // a sell take-profit's low to 97
    #[rustfmt::skip]
    let rows = [
        ("trailing sell", Trailing, -1, None, None, vec![UP, [103.0, 103.5, 101.0, 102.0]], (3_000, 104.0 - pct(104.0))),
        ("trailing buy", Trailing, 1, None, None, vec![DOWN, [97.0, 99.0, 96.5, 98.5]], (3_000, 96.0 + pct(96.0))),
        ("trailing-limit sell, offset 0.5", TrailingLimit, -1, Some(0.5), None, vec![UP, [103.0, 103.5, 101.0, 102.0]], (3_000, 104.0 - pct(104.0))),
        ("trailing take-profit sell", TrailingTakeProfit, -1, None, None, vec![TTP_DOWN, [98.0, 99.5, 97.5, 99.0]], (3_000, 97.0 + pct(97.0))),
        // the path 100.5 -> 100 -> 104 (armed) -> 101, through the stop 104 - 2.08
        ("trailing sell, activated at 103: armed and fired in one bar", Trailing, -1, None, Some(103.0), vec![[100.5, 104.0, 100.0, 101.0]], (2_000, 104.0 - pct(104.0))),
    ];
    for (case, order_type, side, offset, activation, bars, fill) in rows {
        let r = OrderRequest {
            trail_percent: Some(2.0),
            limit_offset: offset,
            trigger_price: activation,
            ..req(case, order_type, side)
        };
        let mut c = seeded();
        c.submit(&r);
        let events = run(&mut c, &bars);
        assert_eq!(fills(&c), [(fill.0, fill.1, false)], "{case}");
        assert_eq!(said_triggered(&events), 0, "{case}");
    }
    // at the extreme it trails from, a percent trail of 2 IS the price trail of 2 there
    let first = |trail, trail_percent| {
        let mut c = seeded();
        c.submit(&OrderRequest { trail, trail_percent, ..req("t", Trailing, -1) });
        c.on_bar(&at(2_000, [99.0, 99.5, 97.0, 98.0]));
        fills(&c)
    };
    assert_eq!(first(None, Some(2.0)), first(Some(2.0), None));
    assert_eq!(first(None, Some(2.0)), [(2_000, 98.0, false)]);
}

/// A stop with an adjustment, its level reached on a bar that does not fire it, becomes its target
/// IN PLACE on that bar, says so with ONE `OrderModified` (its new trigger; a stop-limit target's
/// limit as `new_price`), which moves the FSM's trigger, and is judged as the target from the next
/// bar: a stop moved, a stop-limit, a trailing stop by a price or a percent. It is no release (no
/// `OrderTriggered`). One whose level is never reached stays the stop it was and says nothing.
#[test]
fn an_adjustable_stop_becomes_its_target_once_its_level_is_reached() {
    const REACHES: [f64; 4] = [100.0, 106.0, 99.0, 104.0]; // a sell stop's level 105 reached
    let stop = |stop| OrderStopAdjustTo::Stop { stop };
    let trailing =
        |trail, trail_percent| OrderStopAdjustTo::Trailing { stop: 100.0, trail, trail_percent };
    #[rustfmt::skip]
    let rows = [
        ("sell stop to a stop at 100", Stop, -1, None, stop(100.0), vec![REACHES, [103.0, 104.0, 99.5, 100.0]], OrderKind::Stop, Some((3_000, 100.0))),
        ("sell stop to a stop-limit 100 / 99.5", Stop, -1, None, OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.5 }, vec![REACHES, [103.0, 104.0, 99.8, 100.0]], OrderKind::StopLimit, Some((3_000, 100.0))),
        ("sell stop to a trailing stop at 100 by 2", Stop, -1, None, trailing(Some(2.0), None), vec![REACHES, [103.0, 108.0, 102.5, 107.0], [107.0, 107.0, 105.0, 105.5]], OrderKind::Trailing, Some((4_000, 106.0))),
        ("sell stop to a trailing stop at 100 by 2%", Stop, -1, None, trailing(None, Some(2.0)), vec![REACHES, [103.0, 108.0, 102.5, 107.0], [107.0, 107.0, 105.0, 105.5]], OrderKind::Trailing, Some((4_000, 108.0 - 108.0 * 2.0 / 100.0))),
        ("sell stop-limit to a stop at 100", StopLimit, -1, Some(94.5), stop(100.0), vec![REACHES, [103.0, 104.0, 99.5, 100.0]], OrderKind::Stop, Some((3_000, 100.0))),
        ("buy stop to a stop at 100", Stop, 1, None, stop(100.0), vec![[100.0, 101.0, 94.0, 96.0], [97.0, 100.5, 96.0, 100.0]], OrderKind::Stop, Some((3_000, 100.0))),
        ("sell stop, its level not reached: the stop at 95", Stop, -1, None, stop(100.0), vec![[100.0, 104.5, 99.0, 104.0], [103.0, 104.0, 99.5, 100.0]], OrderKind::Stop, None),
    ];
    for (case, order_type, side, limit, to, bars, after, fill) in rows {
        let (trigger, level) = if side < 0 { (95.0, 105.0) } else { (105.0, 95.0) };
        let r = OrderRequest {
            trigger_price: Some(trigger),
            price: limit,
            adjust: Some(OrderStopAdjust { at: level, to }),
            ..req(case, order_type, side)
        };
        let mut fsm = OrderRecord::new(r.clone());
        let mut c = seeded();
        c.submit(&r);
        let first = run(&mut c, &bars[..1]);
        let (_, o) = &c.pending[0];
        let reached = fill.is_some();
        assert_eq!((o.kind, o.adjust.is_none()), (after, reached), "{case}: after the first bar");
        if !reached {
            assert_eq!(o.price, Some(trigger), "{case}: the stop it was");
        }
        let modified: Vec<_> = first
            .iter()
            .filter_map(|e| match e {
                Event::OrderModified(m) => {
                    Some((m.timestamp, m.new_qty, m.new_price, m.new_trigger_price))
                }
                _ => None,
            })
            .collect();
        let limit = match to {
            OrderStopAdjustTo::StopLimit { limit, .. } => Some(limit),
            _ => None,
        };
        let said: Vec<_> = reached
            .then_some((2_000_i64, None::<f64>, limit, Some(to.stop())))
            .into_iter()
            .collect();
        assert_eq!(modified, said, "{case}: the adjustment is said once, or not at all");
        apply_all(&mut fsm, &first);
        let state_machine_trigger = if reached { to.stop() } else { trigger };
        assert_eq!(
            fsm.request.trigger_price,
            Some(state_machine_trigger),
            "{case}: the FSM's trigger"
        );
        assert_eq!(fsm.status, OrderStatus::Accepted, "{case}: still resting");
        let mut rest = Vec::new();
        for (i, b) in bars.iter().enumerate().skip(1) {
            c.on_bar(&at(2_000 + 1_000 * i as i64, *b));
            rest.extend(drain(&mut c));
        }
        let want: Vec<_> = fill.map(|(ts, px)| (ts, px, false)).into_iter().collect();
        assert_eq!(fills(&c), want, "{case}");
        assert_eq!(said_triggered(&first) + said_triggered(&rest), 0, "{case}: no release");
        apply_all(&mut fsm, &rest);
        let status = if reached { OrderStatus::Filled } else { OrderStatus::Accepted };
        assert_eq!(fsm.status, status, "{case}");
    }
}

/// An adjustable stop's price may already be its target's, so a modify naming a price is refused
/// whole (before the adjustment and after), a trigger amend too; its size alone moves.
#[test]
fn an_adjustable_stop_refuses_a_price_amend_and_a_trigger_amend() {
    let r = OrderRequest {
        trigger_price: Some(95.0),
        adjust: Some(OrderStopAdjust {
            at: 105.0,
            to: OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.5 },
        }),
        ..req("adj", Stop, -1)
    };
    for adjusted in [false, true] {
        let mut c = seeded();
        c.submit(&r);
        if adjusted {
            c.on_bar(&at(2_000, [100.0, 106.0, 99.0, 104.0]));
            assert_eq!(c.pending[0].1.kind, OrderKind::StopLimit, "adjusted");
        }
        drain(&mut c);
        c.modify(&r, Some(2.0), Some(97.0));
        let events = drain(&mut c);
        let [Event::OrderModifyRejected(m)] = events.as_slice() else {
            panic!("adjusted {adjusted}: one OrderModifyRejected, got {events:?}");
        };
        assert_eq!(m.reason.to_string(), ADJUSTABLE_AMEND_REASON, "adjusted {adjusted}");
        assert_eq!(
            c.modify_trigger(&r, None, None, 97.0),
            Err(ADJUSTABLE_AMEND_REASON),
            "adjusted {adjusted}"
        );
        c.modify(&r, Some(2.0), None);
        assert!(matches!(drain(&mut c).as_slice(), [Event::OrderModified(_)]));
        let (_, o) = &c.pending[0];
        let price = if adjusted { 100.0 } else { 95.0 };
        assert_eq!((o.size, o.price), (2.0, Some(price)), "adjusted {adjusted}: its size alone");
    }
}

/// All-or-none, sweep-to-fill and a hidden order (a display of 0) rest as the plain limit of their
/// full size: the bar that reaches it fills every unit at once, as a maker, exactly as the same
/// limit with none of them does (this book fills whole).
#[test]
fn all_or_none_sweep_and_hidden_rest_as_the_plain_limit_and_fill_whole() {
    let aon = OrderAttributes { all_or_none: true, ..Default::default() };
    let sweep = OrderAttributes { sweep_to_fill: true, ..Default::default() };
    let fills_of = |attributes: Option<OrderAttributes>, display_qty: Option<f64>| {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
        let r = OrderRequest {
            qty: 5.0,
            price: Some(99.0),
            attributes,
            display_quantity: display_qty,
            ..req("l", Limit, 1)
        };
        c.submit(&r);
        let (_, o) = &c.pending[0];
        assert_eq!((o.kind, o.price, o.size), (OrderKind::Limit, Some(99.0), 5.0));
        c.on_bar(&bar(1_000, 100.0, 100.5, 99.5, 100.0)); // above the limit: rests
        c.on_bar(&bar(2_000, 100.0, 100.0, 98.0, 99.0)); // reaches it
        c.fills
            .lock()
            .unwrap()
            .iter()
            .map(|f| (f.timestamp, f.size, f.price, f.is_maker))
            .collect::<Vec<_>>()
    };
    let plain = fills_of(None, None);
    assert_eq!(plain, [(2_000, 5.0, 99.0, true)], "full size, a maker");
    assert_eq!(fills_of(Some(aon), None), plain, "all-or-none");
    assert_eq!(fills_of(Some(sweep), None), plain, "sweep-to-fill");
    assert_eq!(fills_of(None, Some(0.0)), plain, "hidden");
}

/// A book long 3 at 100 (no slippage, no fees).
fn long_three() -> ExecutionClientPaper {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&OrderRequest { qty: 3.0, ..req("entry", Market, 1) });
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0));
    drain(&mut c);
    c
}

fn aon_sell(order_client_id: &str, qty: f64, price: f64) -> OrderRequest {
    let attributes = Some(OrderAttributes { all_or_none: true, ..Default::default() });
    OrderRequest { qty, price: Some(price), attributes, ..req(order_client_id, Limit, -1) }
}

/// The events naming `order_client_id` among those a fill pass emits (a cut, a cancel, a fill, a release).
fn about<'a>(events: &'a [Event], order_client_id: &str) -> Vec<&'a Event> {
    let named = |e: &&Event| match e {
        Event::OrderModified(x) => x.order_client_id == order_client_id,
        Event::OrderCanceled(x) => x.order_client_id == order_client_id,
        Event::OrderFilled(x) => x.order_client_id == order_client_id,
        Event::OrderPartiallyFilled(x) => x.order_client_id == order_client_id,
        Event::OrderTriggered(x) => x.order_client_id == order_client_id,
        _ => false,
    };
    events.iter().filter(named).collect()
}

/// `(side, qty, px)` of every fill.
fn sized(c: &ExecutionClientPaper) -> Vec<(i32, f64, f64)> {
    c.fills.lock().unwrap().iter().map(|f| (f.side, f.size, f.price)).collect()
}

/// An all-or-none exit the bracket cap would cut (a stop-limit takes 2 of a long's 3 in the same
/// bar, leaving it 1 of its 2) fills NOTHING in that bar and rests on whole, with no event; the next
/// bar that reaches it fills all of it. The same exit without the flag fills 1 and ends its rest
/// (`trigger_kinds_tests`' `a_stop_limit_and_a_take_profit_exit_in_one_bar_keep_the_bracket_cap`).
#[test]
fn an_all_or_none_exit_the_bracket_cap_would_cut_rests_on_whole() {
    let mut c = long_three();
    let stop_limit = OrderRequest {
        qty: 2.0,
        trigger_price: Some(95.0),
        price: Some(94.0),
        ..req("sl", StopLimit, -1)
    };
    c.submit(&stop_limit);
    c.submit(&aon_sell("tp", 2.0, 105.0));
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 106.0, 93.0, 100.0)); // reaches both
    assert_eq!(sized(&c), [(1, 3.0, 100.0), (-1, 2.0, 95.0)], "the stop fills; tp nothing");
    let events = drain(&mut c);
    assert!(about(&events, "tp").is_empty(), "tp says nothing: {events:?}");
    let (order_client_id, o) = &c.pending[0];
    assert_eq!((order_client_id.as_str(), o.size, o.all_or_none), ("tp", 2.0, true), "rests whole");
    c.on_bar(&bar(3_000, 104.0, 106.0, 103.0, 105.0));
    assert_eq!(sized(&c)[2..], [(-1, 2.0, 105.0)], "all of it, on the next bar that reaches it");
}

/// An all-or-none leg of an OUO pair whose sibling fills first in the same bar (the path reaches 104
/// before 105) is cut to what its group leaves it (said with the `OrderModified` every OUO leg gets)
/// and fills NOTHING in that bar: it rests on at that size, whole, and the next bar fills it.
#[test]
fn an_all_or_none_leg_an_ouo_sibling_cuts_rests_on_at_what_is_left() {
    let mut c = long_three();
    let ouo = |mut r: OrderRequest, sibling: &str| {
        r.contingency_type = Some(ContingencyType::Ouo);
        r.linked_order_ids = vec![sibling.into()];
        r
    };
    c.submit(&ouo(OrderRequest { qty: 2.0, price: Some(104.0), ..req("x", Limit, -1) }, "y"));
    c.submit(&ouo(aon_sell("y", 3.0, 105.0), "x"));
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 106.0, 99.0, 100.0)); // the path: 100, 99, 106: x first
    assert_eq!(sized(&c), [(1, 3.0, 100.0), (-1, 2.0, 104.0)], "x fills; y nothing");
    let events = drain(&mut c);
    let said: Vec<_> = about(&events, "y")
        .into_iter()
        .map(|e| match e {
            Event::OrderModified(m) => Some(m.new_qty),
            _ => None,
        })
        .collect();
    assert_eq!(said, [Some(Some(1.0))], "y is cut to 1, and nothing else: {events:?}");
    let (order_client_id, o) = &c.pending[0];
    assert_eq!((order_client_id.as_str(), o.size), ("y", 1.0), "rests on at what is left");
    c.on_bar(&bar(3_000, 104.0, 106.0, 103.0, 105.0));
    assert_eq!(sized(&c)[2..], [(-1, 1.0, 105.0)], "all of what is left, on the next bar");
}

/// Two all-or-none exits alike in side, kind and price but not in size (`a` 2, `b` 1, sell limits at
/// 105) in one bar whose cap leaves them 2 after a stop and a 104 exit. `a` is an order-list entry
/// (an OCO leg) and `b` is not, so the path puts the 104 entry ahead of `a` and the resolver hands
/// `b` back before `a`. Each is paired with its OWN order client id: `b` fills its 1 whole, and `a` (2, with 1
/// left) fills nothing and rests on whole. Paired by their terms, the two swapped and neither filled.
#[test]
fn two_all_or_none_exits_alike_but_in_size_each_keep_their_own_order() {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
    c.submit(&OrderRequest { qty: 4.0, ..req("entry", Market, 1) });
    c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // long 4 at 100
    let oco = |mut r: OrderRequest, sibling: &str| {
        r.contingency_type = Some(ContingencyType::Oco);
        r.linked_order_ids = vec![sibling.into()];
        r
    };
    let far_buy = |order_client_id: &str| OrderRequest {
        price: Some(50.0),
        ..req(order_client_id, Limit, 1)
    };
    c.submit(&OrderRequest { trigger_price: Some(95.0), ..req("sl", Stop, -1) });
    c.submit(&oco(aon_sell("a", 2.0, 105.0), "d"));
    c.submit(&aon_sell("b", 1.0, 105.0));
    c.submit(&oco(OrderRequest { price: Some(104.0), ..req("c", Limit, -1) }, "e"));
    c.submit(&oco(far_buy("d"), "a"));
    c.submit(&oco(far_buy("e"), "c"));
    drain(&mut c);
    c.on_bar(&bar(2_000, 100.0, 106.0, 93.0, 100.0)); // the stop, 104 and 105 all reached
    let exits = [(-1, 1.0, 95.0), (-1, 1.0, 104.0), (-1, 1.0, 105.0)];
    assert_eq!(sized(&c)[1..], exits, "sl, c, then b's 1");
    let events = drain(&mut c);
    let b_filled =
        events.iter().any(|e| matches!(e, Event::OrderFilled(f) if f.order_client_id == "b"));
    assert!(b_filled, "b filled whole: {events:?}");
    assert!(about(&events, "a").is_empty(), "a says nothing: {events:?}");
    let size_of = |order_client_id: &str| {
        c.pending.iter().find(|(id, _)| id == order_client_id).map(|(_, o)| o.size)
    };
    assert_eq!((size_of("a"), size_of("b")), (Some(2.0), None), "a rests on whole; b is gone");
}

/// A discretionary limit at L with a discretion d is judged at L + d for a buy and L − d for a sell
/// (a gap through the open improves it), and books as a TAKER; the same limit without it rests.
#[test]
fn a_discretionary_limit_fills_within_its_discretion_as_a_taker() {
    #[rustfmt::skip]
    let rows = [
        ("buy 99 + 0.5", 1, 99.0, [100.0, 100.5, 99.2, 100.0], 99.5),
        ("buy 99 + 0.5, a gap down to 99.3: the open", 1, 99.0, [99.3, 99.8, 99.2, 99.5], 99.3),
        ("sell 101 - 0.5", -1, 101.0, [100.0, 100.8, 99.5, 100.0], 100.5),
    ];
    for (case, side, limit, b, px) in rows {
        let fills_of = |discretionary_amount: Option<f64>| {
            let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
            let attributes = discretionary_amount
                .map(|d| OrderAttributes { discretionary_amount: Some(d), ..Default::default() });
            c.submit(&OrderRequest { price: Some(limit), attributes, ..req(case, Limit, side) });
            c.on_bar(&at(2_000, b));
            fills(&c)
        };
        assert_eq!(fills_of(Some(0.5)), [(2_000, px, false)], "{case}");
        assert!(fills_of(None).is_empty(), "{case}: the plain limit rests");
    }
}

/// What this book cannot rest is refused at the submit, in words, and nothing rests: the shared
/// rules' refusals (an attribute, an adjustment, a percent trail, a trailing take-profit's terms),
/// a trailing take-profit before any price, an opening auction's market-to-limit (no session
/// calendar), and a release whose price condition names a venue (this book reads its own price).
#[test]
fn what_the_book_cannot_rest_is_refused_at_submit_in_words() {
    let aon = Some(OrderAttributes { all_or_none: true, ..Default::default() });
    let limit = OrderRequest { price: Some(99.0), ..req("r", Limit, 1) };
    let sell_stop = OrderRequest { trigger_price: Some(95.0), ..req("r", Stop, -1) };
    let ttp = OrderRequest { trail: Some(2.0), ..req("r", TrailingTakeProfit, -1) };
    let adjust = Some(OrderStopAdjust { at: 105.0, to: OrderStopAdjustTo::Stop { stop: 100.0 } });
    let on_binance = OrderReleasePrice {
        symbol: "BTCUSDT".into(),
        level: 100.0,
        side: OrderReleasePriceSide::Above,
        venue: Some("binance".into()),
    };
    #[rustfmt::skip]
    let rows = [
        ("an attribute on a market order", OrderRequest { attributes: aon, ..req("r", Market, 1) }, true, "belongs to a limit order"),
        ("all-or-none at fok", OrderRequest { attributes: aon, time_in_force: TimeInForce::Fok, ..limit.clone() }, true, "all-or-none at once is `fok`"),
        ("sweep-to-fill post-only", OrderRequest { attributes: Some(OrderAttributes { sweep_to_fill: true, ..Default::default() }), time_in_force: TimeInForce::PostOnly, ..limit.clone() }, true, "takes liquidity at once"),
        ("a discretion of 0", OrderRequest { attributes: Some(OrderAttributes { discretionary_amount: Some(0.0), ..Default::default() }), ..limit.clone() }, true, "is not a finite number above zero"),
        ("an adjustment on a limit", OrderRequest { adjust, ..limit.clone() }, true, "belongs to a stop or a stop_limit order"),
        ("an adjustment level below a sell stop", OrderRequest { adjust: Some(OrderStopAdjust { at: 90.0, to: OrderStopAdjustTo::Stop { stop: 92.0 } }), ..sell_stop.clone() }, true, "not on the protected side"),
        ("a percent trail on a limit", OrderRequest { trail_percent: Some(2.0), ..limit.clone() }, true, "a trail percent belongs to a trailing order"),
        ("a percent of 100", OrderRequest { trail: None, trail_percent: Some(100.0), ..req("r", Trailing, -1) }, true, "is not above 0 and below 100"),
        ("a trail and a percent", OrderRequest { trail: Some(2.0), trail_percent: Some(2.0), ..req("r", Trailing, -1) }, true, "not both"),
        ("a trailing take-profit's activation", OrderRequest { trigger_price: Some(103.0), ..ttp.clone() }, true, "takes no activation price"),
        ("a trailing take-profit before any price", ttp.clone(), false, "has seen no price yet"),
        ("an opening auction's market-to-limit", OrderRequest { time_in_force: TimeInForce::AtOpen, ..req("r", MarketToLimit, 1) }, true, "no session calendar"),
        ("a price condition on another venue", OrderRequest { release: Some(OrderRelease { at: 1_000, price: Some(on_binance) }), ..req("r", Market, 1) }, true, "reads BTCUSDT on binance, and this paper book reads only BTCUSDT's price on its own venue"),
    ];
    for (case, r, seen, words) in rows {
        let mut c = if seen {
            seeded()
        } else {
            ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0)
        };
        drain(&mut c);
        c.submit(&r);
        let events = drain(&mut c);
        let [Event::OrderSubmitted(_), Event::OrderRejected(reject)] = events.as_slice() else {
            panic!("{case}: Submitted then Rejected, got {events:?}");
        };
        assert_eq!(reject.cause, None, "{case}");
        assert!(reject.reason.contains(words), "{case}: {}", reject.reason);
        assert!(c.pending.is_empty(), "{case}: nothing rests");
    }
}
