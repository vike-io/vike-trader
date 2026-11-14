//! The trailing kinds at the paper book: a `trailing` and a `trailing_limit` rest as their own kinds
//! (`vike_model::OrderResting::from_request`), trail from the close of the last bar the book saw (as
//! the simulator's trail from its mark at the submit), and fire by `vike_model`'s
//! `order_fill_price` law: the trailing stop at its stop or the open a gap carried past it, the
//! trailing limit at the stop-limit's limit part with A = that fire price, resting as the plain
//! `Limit` at L when the bar does not reach it. One with an ACTIVATION is armed by the last close
//! only once that close reached the level, and is walked along every bar's path by the simulator's
//! own walk (`vike_fills::fill_model::trailing_path_fill`), on the simulator's own law table. A
//! trailing kind the book cannot trail is refused at the submit in words (no price seen yet, no
//! activation), a modify naming a price is refused, a trigger amend is refused in the engine's
//! words, and a cancel works.
use super::test_support::{bar, drain};
use super::trigger_kinds_tests::apply_all;
use super::*;
use vike_execution::ExecutionClient;
use vike_execution::execution_engine::TRAILING_AMEND_REASON;
use vike_model::OrderType::{Trailing, TrailingLimit};
use vike_model::{OrderKind, OrderRecord, OrderRequest, OrderStatus, OrderType};

/// A one-lot order trailing by 2 on BTCUSDT.
fn req(
    order_client_id: &str,
    order_type: OrderType,
    side: i32,
    offset: Option<f64>,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 1.0,
        order_type,
        trail: Some(2.0),
        limit_offset: offset,
        ..Default::default()
    }
}

/// A book that has seen one bar, closing at 100: the price a trailing order submitted now trails
/// from (its open, 99, is not).
fn seeded() -> ExecutionClientPaper {
    let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
    c.on_bar(&bar(1_000, 99.0, 101.0, 98.0, 100.0));
    c
}

/// The bar that ratchets an order trailing from 100 without firing it: a sell's high to 104 (its
/// stop 102), a buy's low to 96 (its stop 98).
fn ratchet(side: i32) -> Bar {
    if side < 0 {
        bar(2_000, 100.0, 104.0, 99.0, 103.0)
    } else {
        bar(2_000, 100.0, 101.0, 96.0, 97.0)
    }
}

fn extreme(c: &ExecutionClientPaper, order_client_id: &str) -> Option<f64> {
    c.pending.iter().find(|(c, _)| c == order_client_id).expect("the order rests").1.extreme
}

/// Each kind trails from the last close, ratchets on a bar that does not fire it, and fills on
/// the bar that does, at the law's price, as a taker, with no release said.
#[test]
fn each_trailing_kind_trails_from_the_last_close_and_fires_by_the_law() {
    #[rustfmt::skip]
    let rows = [
        ("trailing sell: at the stop", Trailing, -1, None, [103.0, 103.5, 101.0, 102.0], 102.0),
        ("trailing sell, gap past the stop: at the open", Trailing, -1, None, [100.0, 100.8, 99.0, 100.0], 100.0),
        ("trailing-limit sell: A = P, inside L", TrailingLimit, -1, Some(0.5), [103.0, 103.5, 101.0, 102.0], 102.0),
        ("trailing-limit sell, gap past P, back to L: at L", TrailingLimit, -1, Some(0.5), [101.0, 101.8, 99.0, 100.0], 101.5),
        ("trailing-limit sell, offset 0: the limit is the stop", TrailingLimit, -1, Some(0.0), [103.0, 103.5, 101.0, 102.0], 102.0),
        ("trailing buy: at the stop", Trailing, 1, None, [97.0, 99.0, 96.5, 98.5], 98.0),
        ("trailing buy, gap past the stop: at the open", Trailing, 1, None, [99.5, 100.0, 99.0, 99.8], 99.5),
        ("trailing-limit buy: A = P, inside L", TrailingLimit, 1, Some(0.5), [97.0, 99.0, 96.5, 98.5], 98.0),
        ("trailing-limit buy, gap past P, back to L: at L", TrailingLimit, 1, Some(0.5), [99.0, 99.5, 98.4, 99.2], 98.5),
        ("trailing-limit buy, offset 0: the limit is the stop", TrailingLimit, 1, Some(0.0), [97.0, 99.0, 96.5, 98.5], 98.0),
    ];
    for (case, order_type, side, offset, [o, h, l, cl], px) in rows {
        let mut c = seeded();
        c.submit(&req(case, order_type, side, offset));
        assert_eq!(extreme(&c, case), Some(100.0), "{case}: trails from the last close");
        c.on_bar(&ratchet(side));
        assert!(c.fills.lock().unwrap().is_empty(), "{case}: not fired yet");
        let ratcheted = if side < 0 { 104.0 } else { 96.0 };
        assert_eq!(extreme(&c, case), Some(ratcheted), "{case}: ratcheted");
        drain(&mut c);
        c.on_bar(&bar(3_000, o, h, l, cl));
        let fills = c.fills.lock().unwrap().clone();
        assert_eq!(fills.len(), 1, "{case}: {fills:?}");
        assert_eq!((fills[0].side, fills[0].size, fills[0].price), (side, 1.0, px), "{case}");
        assert!(!fills[0].is_maker, "{case}: the fire books a taker");
        let events = drain(&mut c);
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderFilled(f) if f.order_client_id == case)),
            "{case}: OrderFilled"
        );
        assert!(
            !events.iter().any(|e| matches!(e, Event::OrderTriggered(_))),
            "{case}: a fill on the fire releases nothing"
        );
        assert!(c.pending.is_empty(), "{case}: filled, gone from the book");
    }
}

/// A trailing limit that fires short of its limit (a gap past L) rests, in place, as the plain
/// `Limit` at L = P - offset (sell) / P + offset (buy), says so with ONE `OrderTriggered`, and
/// fills later by the `Limit` law, as a maker; its FSM goes `Triggered`, then `Filled`.
#[test]
fn a_trailing_limit_fired_short_of_its_limit_rests_as_the_limit_and_fills_later_as_a_maker() {
    #[rustfmt::skip]
    let rows = [
        ("sell gaps past L", -1, [100.0, 100.8, 99.0, 100.0], 101.5, [101.0, 102.0, 100.0, 101.0]),
        ("buy gaps past L", 1, [99.5, 100.0, 99.0, 99.8], 98.5, [99.0, 99.5, 98.0, 98.8]),
    ];
    for (case, side, [o1, h1, l1, c1], limit, [o2, h2, l2, c2]) in rows {
        let mut c = seeded();
        let r = req(case, TrailingLimit, side, Some(0.5));
        let mut fsm = OrderRecord::new(r.clone());
        c.submit(&r);
        c.on_bar(&ratchet(side));
        c.on_bar(&bar(3_000, o1, h1, l1, c1));
        assert!(c.fills.lock().unwrap().is_empty(), "{case}: fired, limit not reached");
        let (_, rests) = &c.pending[0];
        assert_eq!((rests.kind, rests.price, rests.limit), (OrderKind::Limit, Some(limit), None));
        let events = drain(&mut c);
        let [Event::OrderSubmitted(_), Event::OrderAccepted(_), Event::OrderTriggered(t)] =
            events.as_slice()
        else {
            panic!("{case}: the release is said once: {events:?}");
        };
        assert_eq!((t.order_client_id.as_str(), t.timestamp), (case, 3_000), "{case}");
        apply_all(&mut fsm, &events);
        assert_eq!(fsm.status, OrderStatus::Triggered, "{case}");
        c.on_bar(&bar(4_000, o2, h2, l2, c2));
        let fills = c.fills.lock().unwrap().clone();
        assert_eq!((fills.len(), fills[0].price, fills[0].is_maker), (1, limit, true), "{case}");
        apply_all(&mut fsm, &drain(&mut c));
        assert_eq!(fsm.status, OrderStatus::Filled, "{case}");
    }
}

/// A trailing order trails from its OWN symbol's close: a bar of another symbol moves nothing.
#[test]
fn a_trailing_order_trails_from_its_own_symbols_close() {
    let mut c = seeded();
    c.on_bar(&Bar { symbol: Some("ETHUSDT".into()), ..bar(1_500, 50.0, 50.0, 50.0, 50.0) });
    c.submit(&req("t", Trailing, -1, None));
    assert_eq!(extreme(&c, "t"), Some(100.0));
}

/// A trailing kind the book cannot trail is refused at the submit, in words, and never rests: the
/// terms the shared rule refuses (`vike_model::order_terms_refusal`, which runs first), and one with
/// no activation before the book has seen a price (one with an activation waits instead).
#[test]
fn a_trailing_kind_the_book_cannot_trail_is_refused_at_submit_in_words() {
    let trailing = req("t", Trailing, -1, None);
    let limited = req("t", TrailingLimit, -1, Some(0.5));
    #[rustfmt::skip]
    let rows = [
        ("no trail", OrderRequest { trail: None, ..trailing.clone() }, true, "needs a trail distance above zero"),
        ("a trail of 0", OrderRequest { trail: Some(0.0), ..limited.clone() }, true, "needs a trail distance above zero"),
        ("a price", OrderRequest { price: Some(99.0), ..trailing.clone() }, true, "takes no price"),
        ("no offset", OrderRequest { limit_offset: None, ..limited.clone() }, true, "needs a limit offset of zero or more"),
        ("an offset below zero", OrderRequest { limit_offset: Some(-0.5), ..limited.clone() }, true, "is below zero"),
        ("an offset on a trailing stop", OrderRequest { limit_offset: Some(0.5), ..trailing.clone() }, true, "belongs to a trailing_limit order"),
        ("an activation of 0", OrderRequest { trigger_price: Some(0.0), ..trailing.clone() }, true, "is not above zero"),
        ("no price seen yet", trailing.clone(), false, "has seen no price yet"),
        ("no price seen yet, trailing limit", limited.clone(), false, "has seen no price yet"),
    ];
    for (case, r, seen, words) in rows {
        let mut c = if seen {
            seeded()
        } else {
            ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0)
        };
        c.submit(&r);
        let events = drain(&mut c);
        let [Event::OrderSubmitted(_), Event::OrderRejected(reject)] = events.as_slice() else {
            panic!("{case}: Submitted then Rejected, got {events:?}");
        };
        assert_eq!(reject.cause, None, "{case}");
        assert!(reject.reason.contains(words), "{case}: {}", reject.reason);
        assert!(c.pending.is_empty(), "{case}: nothing rests");
        c.on_bar(&bar(2_000, 100.0, 110.0, 90.0, 100.0));
        assert!(drain(&mut c).is_empty(), "{case}: and the next bar has nothing to do");
    }
}

/// An untriggered trailing order has no price to move: a modify naming one is refused whole
/// (non-terminal, nothing moved, its size included), a size-only modify moves its size, a trigger
/// amend is refused in the engine's trailing words (`TRAILING_AMEND_REASON`), and a cancel ends it as
/// any resting order.
#[test]
fn a_trailing_order_refuses_a_price_amend_moves_its_size_and_cancels() {
    for (order_type, offset) in [(Trailing, None), (TrailingLimit, Some(0.5))] {
        let mut c = seeded();
        let r = req("t", order_type.clone(), -1, offset);
        c.submit(&r);
        drain(&mut c);
        c.modify(&r, Some(2.0), Some(99.0));
        let events = drain(&mut c);
        let [Event::OrderModifyRejected(m)] = events.as_slice() else {
            panic!("{order_type}: one OrderModifyRejected, got {events:?}");
        };
        assert_eq!(m.reason.to_string(), TRAILING_AMEND_REASON, "{order_type}");
        let rests = |c: &ExecutionClientPaper| {
            let (_, o) = &c.pending[0];
            (o.kind, o.size, o.price, o.limit, o.extreme)
        };
        let kind =
            if order_type == Trailing { OrderKind::Trailing } else { OrderKind::TrailingLimit };
        assert_eq!(rests(&c), (kind, 1.0, None, None, Some(100.0)), "{order_type}: nothing moved");
        c.modify(&r, Some(2.0), None);
        assert!(matches!(drain(&mut c).as_slice(), [Event::OrderModified(_)]), "{order_type}");
        assert_eq!(rests(&c), (kind, 2.0, None, None, Some(100.0)), "{order_type}: its size");
        assert_eq!(
            c.modify_trigger(&r, None, None, 105.0),
            Err(TRAILING_AMEND_REASON),
            "{order_type}"
        );
        assert!(drain(&mut c).is_empty(), "{order_type}: the refusal is the caller's to publish");
        c.cancel("t");
        let events = drain(&mut c);
        let [Event::OrderCanceled(x)] = events.as_slice() else {
            panic!("{order_type}: one OrderCanceled, got {events:?}");
        };
        assert_eq!(x.reason.as_str(), "user", "{order_type}");
        assert!(c.pending.is_empty(), "{order_type}: gone");
    }
}

// ---- an activation level ----

fn at(ts: i64, [o, h, l, c]: [f64; 4]) -> Bar {
    bar(ts, o, h, l, c)
}

/// A resting order's `(kind, price, limit, extreme)`.
type Resting = (OrderKind, Option<f64>, Option<f64>, Option<f64>);

/// What rests, in book order.
fn resting(c: &ExecutionClientPaper) -> Vec<Resting> {
    c.pending.iter().map(|(_, o)| (o.kind, o.price, o.limit, o.extreme)).collect()
}

/// An activated order is armed at the submit by the last close only when that close has reached
/// its level (a sell's from below, a buy's from above), exactly as the simulator arms it by its
/// mark; else it rests unarmed, and so does one submitted before the book has seen any price.
#[test]
fn an_activated_order_is_armed_at_the_submit_only_when_the_last_close_reached_its_level() {
    #[rustfmt::skip]
    let rows = [
        ("sell, the close above its level: armed at it", -1, 99.0, true, Some(100.0)),
        ("sell, the close below its level: not yet", -1, 103.0, true, None),
        ("buy, the close below its level: armed at it", 1, 101.0, true, Some(100.0)),
        ("buy, the close above its level: not yet", 1, 97.0, true, None),
        ("no price seen yet: waits, unarmed", -1, 99.0, false, None),
    ];
    for (case, side, level, seen, armed) in rows {
        for (order_type, offset) in [(Trailing, None), (TrailingLimit, Some(0.5))] {
            let mut c = if seen {
                seeded()
            } else {
                ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0)
            };
            c.submit(&OrderRequest {
                trigger_price: Some(level),
                ..req(case, order_type, side, offset)
            });
            let events = drain(&mut c);
            assert!(
                matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
                "{case}: {events:?}"
            );
            let (_, o) = &c.pending[0];
            assert_eq!((o.activation, o.extreme), (Some(level), armed), "{case}");
        }
    }
}

/// A trailing STOP with an activation, trail 2, submitted on a close of 100: walked along each
/// bar's path (open, the nearer extreme, the farther one, close), it waits for its level however
/// far the price runs the other way, arms at the first point that reaches it (the open when the
/// bar opened past it), fires inside that same bar when the path comes back through its stop, and
/// once armed fills at the open a gap carried past its stop. A taker every time.
#[test]
fn an_activated_trailing_stop_waits_for_its_level_and_is_walked_along_each_bar() {
    const FLAT: [f64; 4] = [100.0; 4];
    #[rustfmt::skip]
    let rows = [
        // from the close, a plain trailing stop would fire on this bar at 98
        ("sell, the level not reached: nothing fires however far it falls", -1, 103.0, [[100.0, 102.5, 90.0, 95.0], FLAT], None),
        // 100.5 -> 100 -> 104 (armed) -> 101, through the stop 102
        ("sell, armed and fired inside one bar: at the stop", -1, 103.0, [[100.5, 104.0, 100.0, 101.0], FLAT], Some((2_000, 102.0))),
        // 105 (armed at the open) -> 105.5 -> 102, through the stop 103.5
        ("sell, the bar opened past its level: armed at the open", -1, 103.0, [[105.0, 105.5, 102.0, 102.5], FLAT], Some((2_000, 103.5))),
        ("sell, armed, then a gap past the stop: at the open", -1, 103.0, [[100.5, 104.0, 100.0, 103.0], [100.0, 100.5, 99.0, 100.0]], Some((3_000, 100.0))),
        ("buy, the level not reached: nothing fires however far it rises", 1, 97.0, [[100.0, 110.0, 97.5, 105.0], FLAT], None),
        // 99.5 -> 100 -> 96 (armed) -> 99, through the stop 98
        ("buy, armed and fired inside one bar: at the stop", 1, 97.0, [[99.5, 100.0, 96.0, 99.0], FLAT], Some((2_000, 98.0))),
        // 96 (armed at the open) -> 95.5 -> 98.5, through the stop 97.5
        ("buy, the bar opened past its level: armed at the open", 1, 97.0, [[96.0, 98.5, 95.5, 98.0], FLAT], Some((2_000, 97.5))),
        ("buy, armed, then a gap past the stop: at the open", 1, 97.0, [[99.5, 100.0, 96.0, 97.5], [99.0, 99.5, 98.5, 99.0]], Some((3_000, 99.0))),
    ];
    for (case, side, level, then, fill) in rows {
        let mut c = seeded();
        c.submit(&OrderRequest { trigger_price: Some(level), ..req(case, Trailing, side, None) });
        assert_eq!(extreme(&c, case), None, "{case}: not armed at the submit");
        for (i, b) in then.into_iter().enumerate() {
            c.on_bar(&at(2_000 + 1_000 * i as i64, b));
        }
        let fills: Vec<_> =
            c.fills.lock().unwrap().iter().map(|f| (f.timestamp, f.price, f.is_maker)).collect();
        match fill {
            Some((ts, px)) => assert_eq!(fills, [(ts, px, false)], "{case}"),
            None => {
                assert!(fills.is_empty(), "{case}: {fills:?}");
                let unarmed = (OrderKind::Trailing, None, None, None);
                assert_eq!(resting(&c), [unarmed], "{case}: rests, unarmed");
            }
        }
    }
}

/// **The simulator's own table, run through this book**: every row of
/// `crates/vike-simulator/tests/laws/trailing_limit_law.rs`'s `ROWS` (a trailing limit with an activation,
/// trail 2, submitted on a close of 100), with the same bars, gives the same fill at the same bar,
/// price and fee class, and leaves the same order resting. A release to the `Limit` is said once.
#[test]
fn the_simulators_activated_trailing_limit_table_gives_the_same_fills_here() {
    const FLAT: [f64; 4] = [100.0; 4];
    const ARM_SELL: [f64; 4] = [104.5, 106.0, 104.5, 105.5];
    const ARM_BUY: [f64; 4] = [95.5, 95.5, 94.0, 94.5];
    #[rustfmt::skip]
    let rows = [
        ("sell, the activation not reached: nothing fires however far it falls", -1, 1.0, 104.0, [[100.0, 103.0, 90.0, 95.0], FLAT, FLAT], None, Some((OrderKind::TrailingLimit, None))),
        ("sell, activated and fired in one bar: a taker at P", -1, 1.0, 104.0, [[104.5, 106.0, 100.0, 101.0], FLAT, FLAT], Some((2_000, 104.0, false)), None),
        ("sell, a gap past P within L: A = the open", -1, 1.0, 104.0, [ARM_SELL, [103.5, 103.8, 101.0, 102.0], FLAT], Some((3_000, 103.5, false)), None),
        ("sell, a gap through L that comes back: filled at L", -1, 1.0, 104.0, [ARM_SELL, [102.0, 103.2, 100.0, 101.0], FLAT], Some((3_000, 103.0, false)), None),
        ("sell, a gap through L never back: rests as the Limit at L, a maker next bar", -1, 1.0, 104.0, [ARM_SELL, [102.0, 102.5, 100.0, 101.0], [101.0, 103.5, 100.5, 103.0]], Some((4_000, 103.0, true)), None),
        ("sell, offset 0, no gap: the limit is the stop, filled there", -1, 0.0, 104.0, [[104.5, 106.0, 100.0, 101.0], FLAT, FLAT], Some((2_000, 104.0, false)), None),
        ("sell, offset 0, a gap past P: rests as the Limit at P", -1, 0.0, 104.0, [ARM_SELL, [103.5, 103.8, 101.0, 102.0], FLAT], None, Some((OrderKind::Limit, Some(104.0)))),
        ("buy, the activation not reached: nothing fires however far it rises", 1, 1.0, 96.0, [[100.0, 110.0, 97.0, 105.0], FLAT, FLAT], None, Some((OrderKind::TrailingLimit, None))),
        ("buy, activated and fired in one bar: a taker at P", 1, 1.0, 96.0, [[95.5, 100.0, 94.0, 99.0], FLAT, FLAT], Some((2_000, 96.0, false)), None),
        ("buy, a gap past P within L: A = the open", 1, 1.0, 96.0, [ARM_BUY, [96.5, 99.0, 96.2, 98.0], FLAT], Some((3_000, 96.5, false)), None),
        ("buy, a gap through L that comes back: filled at L", 1, 1.0, 96.0, [ARM_BUY, [98.0, 99.0, 96.8, 98.5], FLAT], Some((3_000, 97.0, false)), None),
        ("buy, a gap through L never back: rests as the Limit at L, a maker next bar", 1, 1.0, 96.0, [ARM_BUY, [98.0, 99.0, 97.5, 98.5], [97.5, 98.0, 96.5, 97.0]], Some((4_000, 97.0, true)), None),
        ("buy, offset 0, a gap past P: rests as the Limit at P", 1, 0.0, 96.0, [ARM_BUY, [96.5, 99.0, 96.2, 98.0], FLAT], None, Some((OrderKind::Limit, Some(96.0)))),
    ];
    for (case, side, offset, level, then, fill, rest) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.001, 0.002);
        c.on_bar(&at(1_000, FLAT));
        let r = req(case, TrailingLimit, side, Some(offset));
        c.submit(&OrderRequest { trigger_price: Some(level), ..r });
        for (i, b) in then.into_iter().enumerate() {
            c.on_bar(&at(2_000 + 1_000 * i as i64, b));
        }
        let fills: Vec<_> =
            c.fills.lock().unwrap().iter().map(|f| (f.timestamp, f.price, f.is_maker)).collect();
        assert_eq!(fills, Vec::from_iter(fill), "{case}");
        let want: Vec<_> = rest.map(|(kind, px)| (kind, px, None)).into_iter().collect();
        let got: Vec<_> = resting(&c).into_iter().map(|(k, p, l, _)| (k, p, l)).collect();
        assert_eq!(got, want, "{case}: what rests");
        let released = fill.is_some_and(|(.., maker)| maker)
            || rest.is_some_and(|(kind, _)| kind == OrderKind::Limit);
        let triggered =
            drain(&mut c).iter().filter(|e| matches!(e, Event::OrderTriggered(_))).count();
        assert_eq!(triggered, usize::from(released), "{case}: the release is said once");
    }
}
