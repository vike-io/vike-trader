//! The market-to-limit, auction, display and release terms at the paper book: a market-to-limit
//! fills whole at the next open exactly as a market order does (this book fills whole, so its
//! remainder law never comes into play) and one stating a limit price is refused at the submit; an
//! auction lifetime is refused by name, because this book has no session calendar, and is never
//! priced on the next bar; an iceberg is the plain limit of its full size; an order with a release
//! takes part in no bar before its release holds (`vike_model::OrderResting::is_held`), and a
//! combo with one is refused, as the combo book could not hold it. A post-only order with a
//! release is judged at its release, and a `day` order with one takes its day from the release. A
//! release's price condition reads the open of this book's own symbol's bar, and one naming another
//! symbol is refused at the submit.
use super::test_support::{bar, drain};
use super::*;
use vike_execution::ExecutionClient;
use vike_model::events::OrderRejected;
use vike_model::{
    OrderComboLeg, OrderKind, OrderRecord, OrderRelease, OrderReleasePrice, OrderReleasePriceSide,
    OrderRequest, OrderStatus, OrderType, TimeInForce,
};

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

fn book() -> ExecutionClientPaper {
    ExecutionClientPaper::new("sim", "BTCUSDT", 0.001, 0.001, 0.002)
}

/// The one refusal `submit` answered with, after its `OrderSubmitted`; nothing rests.
fn refusal(c: &mut ExecutionClientPaper, case: &str) -> OrderRejected {
    let events = drain(c);
    let [Event::OrderSubmitted(_), Event::OrderRejected(r)] = events.as_slice() else {
        panic!("{case}: Submitted then Rejected, got {events:?}");
    };
    assert!(c.pending.is_empty() && c.combos.is_empty(), "{case}: nothing rests");
    r.clone()
}

/// A bar that would fill every order of the rows below, and the one after it.
const FIRST: [f64; 4] = [100.0, 102.0, 98.0, 101.0];
const NEXT: [f64; 4] = [101.0, 103.0, 99.0, 102.0];

fn at(ts: i64, [o, h, l, c]: [f64; 4]) -> Bar {
    bar(ts, o, h, l, c)
}

/// A market-to-limit rests as its own kind until the next bar, which fills it WHOLE at its open,
/// as a taker: the very fill a market order gets, slippage and fee included, with no release to a
/// limit and nothing left resting.
#[test]
fn a_market_to_limit_fills_whole_at_the_next_open_exactly_as_a_market_order_does() {
    let fills_of = |order_type: OrderType| {
        let mut c = book();
        c.submit(&req("o", order_type.clone(), 1, None, None));
        let events = drain(&mut c);
        assert!(
            matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
            "{order_type}: {events:?}"
        );
        c.on_bar(&at(1_000, FIRST));
        assert!(c.pending.is_empty(), "{order_type}: filled whole, nothing rests");
        let events = drain(&mut c);
        assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{events:?}");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::OrderTriggered(_) | Event::OrderPartiallyFilled(_))),
            "{order_type}: {events:?}"
        );
        c.fills.lock().unwrap().clone()
    };
    let mut c = book();
    c.submit(&req("o", OrderType::MarketToLimit, 1, None, None));
    assert_eq!((c.pending[0].1.kind, c.pending[0].1.price), (OrderKind::MarketToLimit, None));
    let mtl = fills_of(OrderType::MarketToLimit);
    assert_eq!(mtl, fills_of(OrderType::Market), "the market order's own fill");
    assert_eq!((mtl.len(), mtl[0].timestamp, mtl[0].is_maker), (1, 1_000, false), "one taker fill");
}

/// A market-to-limit's limit is its first fill's price, so one that states a limit price is
/// refused in the shared rule's words (`vike_model::order_terms_refusal`), never rested with the
/// price dropped.
#[test]
fn a_market_to_limit_stating_a_limit_price_is_refused_at_submit() {
    let mut c = book();
    c.submit(&req("mtl", OrderType::MarketToLimit, 1, None, Some(100.0)));
    let r = refusal(&mut c, "mtl");
    assert_eq!(r.cause, None);
    assert!(r.reason.contains("takes no limit price"), "{}", r.reason);
}

/// A pegged order follows the venue's book, which this book does not hold: refused by name at the
/// submit, never lowered to the market order its kind would otherwise fall to.
#[test]
fn a_pegged_order_is_refused_by_name_and_never_filled_as_a_market_order() {
    let mut c = book();
    let peg = vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Mid, offset: 0.0 };
    c.submit(&OrderRequest { peg: Some(peg), ..req("peg", OrderType::Pegged, 1, None, None) });
    let r = refusal(&mut c, "peg");
    assert!(r.reason.contains("holds no pegged order"), "{}", r.reason);
    c.on_bar(&at(1_000, FIRST));
    assert!(c.fills.lock().unwrap().is_empty(), "nothing filled");
}

/// The regression: an auction lifetime used to rest like `Gtc` and fill on the next bar, at a
/// price no auction set. It is refused by name at the submit, a market and a limit order alike and
/// a combo too, and the next bar has nothing to do.
#[test]
fn an_auction_lifetime_is_refused_by_name_and_never_priced_on_the_next_bar() {
    for tif in [TimeInForce::AtOpen, TimeInForce::AtClose] {
        let combo = OrderRequest {
            symbol: String::new(),
            combo_legs: vec![
                OrderComboLeg { symbol: "NEAR".into(), ratio: 1, limit: None },
                OrderComboLeg { symbol: "FAR".into(), ratio: -1, limit: None },
            ],
            ..req("combo", OrderType::Limit, 1, None, Some(1.0))
        };
        let rows = [
            req("market", OrderType::Market, 1, None, None),
            req("limit", OrderType::Limit, -1, None, Some(99.0)),
            combo,
        ];
        for mut r in rows {
            r.time_in_force = tif;
            let case = format!("{} {}", r.order_client_id, tif.as_word());
            let mut c = book();
            c.submit(&r);
            let reject = refusal(&mut c, &case);
            assert_eq!(reject.cause, None, "{case}");
            let word = format!("`{}`", tif.as_word());
            assert!(reject.reason.contains(word.as_str()), "{case}: {}", reject.reason);
            assert!(reject.reason.contains("no session calendar"), "{case}: {}", reject.reason);
            c.on_bar(&at(1_000, FIRST));
            assert!(drain(&mut c).is_empty(), "{case}: the next bar has nothing to do");
            assert!(c.fills.lock().unwrap().is_empty(), "{case}: nothing fills");
        }
    }
}

/// An iceberg (a `display_quantity`) is priced as the plain limit of its FULL size: it rests as the
/// `Limit`, and the bar that reaches it fills every unit at once, as a maker, exactly as the same
/// limit without a display does.
#[test]
fn an_iceberg_is_priced_as_the_plain_limit_of_its_full_size() {
    let fills_of = |display_qty: Option<f64>| {
        let mut c = book();
        let mut r = req("ice", OrderType::Limit, 1, None, Some(99.0));
        r.qty = 5.0;
        r.display_quantity = display_qty;
        c.submit(&r);
        let resting = &c.pending[0].1;
        assert_eq!(
            (resting.kind, resting.price, resting.size),
            (OrderKind::Limit, Some(99.0), 5.0)
        );
        c.on_bar(&bar(1_000, 100.0, 100.5, 99.5, 100.0)); // above the limit: rests
        c.on_bar(&bar(2_000, 100.0, 100.0, 98.0, 99.0)); // reaches it
        c.fills.lock().unwrap().clone()
    };
    let ice = fills_of(Some(1.0));
    assert_eq!(ice, fills_of(None), "the plain limit's fill");
    assert_eq!((ice.len(), ice[0].size, ice[0].is_maker), (1, 5.0, true), "full size, a maker");
}

/// An order with a release takes part in NO bar before its release holds, whatever its kind: a
/// release inside a bar waits for the next bar, and one stamped at a bar's ts takes part in that
/// bar. Held, it emits nothing (no fill, no `OrderTriggered`) and rests with its terms and its
/// release; released, it fills exactly as the same order submitted then would.
#[test]
fn an_order_with_a_release_takes_part_in_no_bar_before_it() {
    use OrderType::{Limit, Market, MarketToLimit, Stop, StopLimit, TakeProfit};
    #[rustfmt::skip]
    let rows = [
        ("market", Market, None, None),
        ("limit buy", Limit, None, Some(101.5)),
        ("stop buy", Stop, Some(101.5), None),
        ("stop-limit buy", StopLimit, Some(101.5), Some(102.0)),
        ("market-if-touched buy", TakeProfit, Some(100.5), None),
        ("market-to-limit", MarketToLimit, None, None),
    ];
    for (case, order_type, trigger, limit) in rows {
        let plain = req(case, order_type, 1, trigger, limit);
        // the same order submitted at the bar it is released on
        let mut c = book();
        c.submit(&plain);
        c.on_bar(&at(2_000, NEXT));
        let then = c.fills.lock().unwrap().clone();
        assert_eq!(then.len(), 1, "{case}: {then:?}");
        for release_at in [1_500, 2_000] {
            let release = Some(OrderRelease { at: release_at, price: None });
            let mut c = book();
            c.submit(&OrderRequest { release, ..plain.clone() });
            drain(&mut c);
            c.on_bar(&at(1_000, FIRST));
            assert!(drain(&mut c).is_empty(), "{case} @ {release_at}: held, nothing happens");
            assert!(c.fills.lock().unwrap().is_empty(), "{case} @ {release_at}: held");
            let resting = &c.pending[0].1;
            assert_eq!(resting.release, release, "{case} @ {release_at}: the release rests on");
            assert_eq!(
                (resting.kind, resting.price, resting.limit),
                {
                    let o = vike_model::OrderResting::from_request(&plain);
                    (o.kind, o.price, o.limit)
                },
                "{case} @ {release_at}: its terms untouched"
            );
            c.on_bar(&at(2_000, NEXT));
            assert_eq!(*c.fills.lock().unwrap(), then, "{case} @ {release_at}: released at 2_000");
            assert!(c.pending.is_empty(), "{case} @ {release_at}");
        }
    }
}

/// A held order is modified in place (its release stays) and cancelled like any other, and a
/// cancelled one never fills.
#[test]
fn a_held_order_keeps_its_release_through_a_modify_and_cancels_like_any_other() {
    let r = OrderRequest {
        release: Some(OrderRelease { at: 2_000, price: None }),
        ..req("held", OrderType::Limit, 1, None, Some(99.0))
    };
    let mut c = book();
    c.submit(&r);
    c.modify(&r, Some(2.0), Some(101.5));
    c.on_bar(&at(1_000, FIRST));
    assert!(c.fills.lock().unwrap().is_empty(), "held through the modify");
    assert_eq!(c.pending[0].1.release, Some(OrderRelease { at: 2_000, price: None }));
    c.on_bar(&at(2_000, NEXT));
    let fills = c.fills.lock().unwrap().clone();
    assert_eq!(
        (fills.len(), fills[0].timestamp, fills[0].size, fills[0].is_maker),
        (1, 2_000, 2.0, true)
    );

    let mut c = book();
    c.submit(&r);
    drain(&mut c);
    c.cancel("held");
    let events = drain(&mut c);
    assert!(
        matches!(events.as_slice(), [Event::OrderCanceled(x)] if x.reason.as_str() == "user"),
        "{events:?}"
    );
    c.on_bar(&at(2_000, NEXT));
    assert!(drain(&mut c).is_empty() && c.fills.lock().unwrap().is_empty());
}

/// The combo book fills on fresh marks and holds nothing back, so a combo with a release is refused
/// in words instead of filling before it.
#[test]
fn a_combo_with_a_release_is_refused() {
    let combo = OrderRequest {
        symbol: String::new(),
        combo_legs: vec![
            OrderComboLeg { symbol: "NEAR".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "FAR".into(), ratio: -1, limit: None },
        ],
        release: Some(OrderRelease { at: 2_000, price: None }),
        ..req("combo", OrderType::Limit, 1, None, Some(1.0))
    };
    let mut c = book();
    c.submit(&combo);
    let r = refusal(&mut c, "combo");
    assert!(r.reason.contains("a combo order with a release"), "{}", r.reason);
}

fn condition(symbol: &str, level: f64, side: OrderReleasePriceSide) -> Option<OrderRelease> {
    let price = OrderReleasePrice { symbol: symbol.into(), level, side, venue: None };
    Some(OrderRelease { at: 1_000, price: Some(price) })
}

/// A release with a price condition holds until its time AND its condition hold, the condition read
/// at this book's own symbol's bar OPEN (the price as the bar arrives, never a later high or low):
/// a bar whose range crosses the level but whose open does not keeps it held, and the first bar
/// that opens beyond the level releases it, which fills it exactly as that order submitted then. A
/// bar of another symbol tells this book nothing, so it keeps the order held too.
#[test]
fn a_price_held_release_waits_for_its_own_symbols_open_beyond_the_level() {
    #[rustfmt::skip]
    let rows = [
        ("buy, above 105", 1, OrderReleasePriceSide::Above, [100.0, 101.0, 99.0, 100.0], [104.0, 106.0, 103.0, 105.0], [105.5, 106.0, 105.0, 105.5]),
        ("sell, below 95", -1, OrderReleasePriceSide::Below, [100.0, 101.0, 99.0, 100.0], [96.0, 97.0, 94.0, 95.0], [94.5, 95.0, 94.0, 94.5]),
    ];
    for (case, side, at_side, quiet, crosses, opens_beyond) in rows {
        let level = if side > 0 { 105.0 } else { 95.0 };
        let plain = req(case, OrderType::Market, side, None, None);
        let then = {
            let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
            c.submit(&plain);
            c.on_bar(&at(3_000, opens_beyond));
            c.fills.lock().unwrap().clone()
        };
        assert_eq!(then.len(), 1, "{case}: {then:?}");
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        c.submit(&OrderRequest { release: condition("BTCUSDT", level, at_side), ..plain.clone() });
        drain(&mut c);
        c.on_bar(&at(1_000, quiet)); // the time holds, the price does not
        c.on_bar(&at(2_000, crosses)); // the range crosses the level, the open does not
        let mut elsewhere = at(2_500, opens_beyond);
        elsewhere.symbol = Some("ETHUSDT".into()); // another symbol's bar tells this book nothing
        c.on_bar(&elsewhere);
        assert!(c.fills.lock().unwrap().is_empty() && drain(&mut c).is_empty(), "{case}: held");
        c.on_bar(&at(3_000, opens_beyond));
        assert_eq!(*c.fills.lock().unwrap(), then, "{case}: released at 3_000's open");
    }
}

/// This book reads its own symbol's price only, so a release whose condition names another symbol
/// could never be judged here: it is refused at the submit in words, and nothing rests.
#[test]
fn a_release_conditioned_on_another_symbol_is_refused_at_submit() {
    let mut c = book();
    c.submit(&OrderRequest {
        release: condition("ETHUSDT", 3_000.0, OrderReleasePriceSide::Above),
        ..req("cond", OrderType::Market, 1, None, None)
    });
    let r = refusal(&mut c, "cond");
    assert_eq!(r.cause, None);
    assert!(
        r.reason.contains("reads ETHUSDT, and this paper book reads only BTCUSDT's price"),
        "{}",
        r.reason
    );
}

/// Fold every lifecycle event into `fsm` (one order per book here, so all of them name it): each
/// must be a legal transition.
fn fold(fsm: &mut OrderRecord, events: &[Event]) {
    for e in events.iter().filter(|e| !matches!(e, Event::Fill(_))) {
        fsm.apply(e).unwrap();
    }
}

/// A post-only order with a release meets the market at its RELEASE, not at its submit: it rests
/// through the submit whatever the last bar offered, and the bar that releases it judges it against
/// what the market offered as that bar arrived (the reference a submit at that moment meets). Not
/// crossing then, it rests and fills later as a maker; crossing then, it ends in `OrderCanceled`
/// naming the cross (it was accepted, so a cancel is the FSM's edge), and nothing fills.
#[test]
fn a_post_only_order_with_a_release_is_judged_at_its_release() {
    // (case, buy limit, the bar before the release, the release bar, the fill)
    #[rustfmt::skip]
    let rows = [
        ("crosses at the submit, not at the release: rests, then fills", 101.0, [104.0, 106.0, 104.0, 105.0], [105.0, 106.0, 100.0, 102.0], Some(101.0)),
        ("not at the submit, crosses at the release: canceled", 99.0, [99.0, 99.5, 98.0, 98.0], [98.0, 99.0, 97.0, 98.5], None),
    ];
    for (case, limit, before, release_bar, fill) in rows {
        let mut c = ExecutionClientPaper::new("sim", "BTCUSDT", 0.0, 0.0, 0.0);
        c.on_bar(&bar(1_000, 100.0, 100.0, 100.0, 100.0)); // the market at the submit: 100
        let r = OrderRequest {
            time_in_force: TimeInForce::PostOnly,
            release: Some(OrderRelease { at: 3_000, price: None }),
            ..req("po", OrderType::Limit, 1, None, Some(limit))
        };
        let mut fsm = OrderRecord::new(r.clone());
        c.submit(&r);
        let events = drain(&mut c);
        assert!(
            matches!(events.as_slice(), [Event::OrderSubmitted(_), Event::OrderAccepted(_)]),
            "{case}: not judged at the submit: {events:?}"
        );
        fold(&mut fsm, &events);
        c.on_bar(&at(2_000, before));
        assert!(drain(&mut c).is_empty(), "{case}: held, not judged yet");
        c.on_bar(&at(3_000, release_bar));
        let events = drain(&mut c);
        fold(&mut fsm, &events);
        let fills = c.fills.lock().unwrap().clone();
        match fill {
            Some(px) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_eq!(
                    (fills[0].timestamp, fills[0].price, fills[0].is_maker),
                    (3_000, px, true),
                    "{case}"
                );
                assert_eq!(fsm.status, OrderStatus::Filled, "{case}");
            }
            None => {
                assert!(fills.is_empty(), "{case}: nothing fills: {fills:?}");
                let [Event::OrderCanceled(x)] = events.as_slice() else {
                    panic!("{case}: one cancel, got {events:?}");
                };
                assert!(
                    x.reason.contains("would take liquidity (the ask is 98)"),
                    "{case}: {}",
                    x.reason
                );
                assert_eq!(fsm.status, OrderStatus::Canceled, "{case}");
                assert!(c.pending.is_empty() && c.post_only_at_release.is_empty(), "{case}");
            }
        }
    }
}

/// A `day` order with a release takes its day from the release, so it never expires before it is
/// released: held across a day boundary it rests on through its release day and expires on the
/// first bar of the day after. Without the release, the same order takes the day of the first bar
/// it sees and expires at the boundary, as before.
#[test]
fn a_day_order_with_a_release_takes_its_day_from_the_release() {
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    let quiet = |ts| bar(ts, 100.0, 100.5, 99.5, 100.0); // never reaches the limit at 90
    let expired = |c: &mut ExecutionClientPaper| {
        drain(c).iter().any(|e| matches!(e, Event::OrderExpired(x) if x.order_client_id == "day"))
    };
    let day_order = |release| OrderRequest {
        time_in_force: TimeInForce::Day,
        release,
        ..req("day", OrderType::Limit, 1, None, Some(90.0))
    };

    let mut c = book();
    c.submit(&day_order(Some(OrderRelease { at: 2 * DAY + 2 * HOUR, price: None })));
    c.on_bar(&quiet(DAY + HOUR)); // day 1: held
    c.on_bar(&quiet(2 * DAY + HOUR)); // day 2, before the release: held, not expired
    assert!(!expired(&mut c) && c.pending.len() == 1, "never expired before its release");
    c.on_bar(&quiet(2 * DAY + 3 * HOUR)); // released, rests
    assert!(!expired(&mut c) && c.pending.len() == 1, "its release day");
    c.on_bar(&quiet(3 * DAY + HOUR));
    assert!(expired(&mut c) && c.pending.is_empty(), "the day after its release ends it");

    let mut c = book();
    c.submit(&day_order(None));
    c.on_bar(&quiet(DAY + HOUR));
    c.on_bar(&quiet(2 * DAY + HOUR));
    assert!(expired(&mut c) && c.pending.is_empty(), "without a release: the first bar's day");
}
