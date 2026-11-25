//! The delayed-data rule (E07, `crate::execution::delayed_order`): the pure verdict, the mark table, and the real exec loop over
//! the `IbkrTransportFake` (a refused order reaches no transport; the orders the rule lets through do).

use std::time::{Duration, Instant};

use vike_bridge_core::execution_drive::drain;

use vike_execution::{ExecutionClient, event_channel};
use vike_model::events::Event;

use super::*;
use crate::testing::{IbkrTransportFake, ScriptedInbound};

fn order(
    symbol: &str,
    kind: OrderType,
    price: Option<f64>,
    delay: Option<StreamDelay>,
) -> OrderRequest {
    OrderRequest {
        order_client_id: format!("order-client-id-{symbol}"),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: kind,
        price,
        data_delay: delay,
        ..Default::default()
    }
}

fn market(symbol: &str, delay: Option<StreamDelay>) -> OrderRequest {
    order(symbol, OrderType::Market, None, delay)
}

fn limit(symbol: &str, price: f64, delay: Option<StreamDelay>) -> OrderRequest {
    order(symbol, OrderType::Limit, Some(price), delay)
}

/// A mark source that must not be asked: the verdicts that need no price never look one up.
fn no_lookup(_: &str) -> Option<f64> {
    panic!("this verdict needs no mark")
}

fn mark(mid: f64) -> impl FnOnce(&str) -> Option<f64> {
    move |_| Some(mid)
}

/// Note a delayed mark for `symbol` straight into the table, `age` old.
fn set_mark(symbol: &str, mid: f64, age: Duration) {
    let at = Instant::now().checked_sub(age).expect("an Instant that far back");
    MARKS.lock().unwrap().insert(symbol.to_string(), (at, mid));
}

/// **A MARKET order that opens is refused on a stream that is not real-time**, in words that say why and what to send,
/// with no mark asked for: delayed (about 15 minutes old) and frozen (the last value) alike.
#[test]
fn a_market_order_on_delayed_or_frozen_data_is_refused_in_words() {
    let why =
        refusal(&market("AAPL.SMART.USD", Some(StreamDelay::DELAYED)), no_lookup).expect("refused");
    for needle in
        ["AAPL.SMART.USD", "MARKET", "delayed", "15 minutes", "LIMIT", "5 percent", "reduce-only"]
    {
        assert!(why.contains(needle), "`{needle}` missing from: {why}");
    }
    let why =
        refusal(&market("AAPL.SMART.USD", Some(StreamDelay::Frozen)), no_lookup).expect("refused");
    // a frozen stream states no age: the last value the venue holds has none
    assert_eq!(
        why,
        "ibkr: a MARKET order on AAPL.SMART.USD is refused: its price is frozen data, so a market \
         order would trade at a price nobody here can see. Send a LIMIT order priced within 5 \
         percent of the frozen price (an order that only closes a position, reduce-only, is never \
         refused for this)"
    );
    let stated = StreamDelay::Delayed { lag_ms: Some(900_000) };
    assert!(refusal(&market("AAPL.SMART.USD", Some(stated)), no_lookup).is_some());
}

/// **Nothing else is judged**: no label, a real-time label, an order that only closes, a combo, and an order carrying no
/// price at all (nothing to judge) pass whatever they are, and the mark is never asked for.
#[test]
fn an_order_without_a_non_real_time_label_or_that_only_closes_is_never_refused() {
    let wild = |delay| limit("AAPL.SMART.USD", 9_999.0, delay);
    assert_eq!(refusal(&market("AAPL.SMART.USD", None), no_lookup), None, "no label");
    assert_eq!(refusal(&wild(None), no_lookup), None);
    assert_eq!(refusal(&market("AAPL.SMART.USD", Some(StreamDelay::RealTime)), no_lookup), None);
    assert_eq!(refusal(&wild(Some(StreamDelay::RealTime)), no_lookup), None);

    let closing =
        OrderRequest { reduce_only: true, ..market("AAPL.SMART.USD", Some(StreamDelay::DELAYED)) };
    assert_eq!(refusal(&closing, no_lookup), None, "a flatten is never held up");
    let closing = OrderRequest { reduce_only: true, ..wild(Some(StreamDelay::DELAYED)) };
    assert_eq!(refusal(&closing, no_lookup), None, "nor a protective exit priced far by design");

    let unpriced = order(
        "AAPL.SMART.USD",
        OrderType::Unrecognized("moc".into()),
        None,
        Some(StreamDelay::DELAYED),
    );
    assert_eq!(refusal(&unpriced, no_lookup), None, "a kind with no price and not a market order");
}

/// **The band is 5 percent of the delayed mark, both ways, on the price and on a trigger.**
#[test]
fn a_price_outside_five_percent_of_the_delayed_mark_is_refused_both_ways() {
    let delayed = Some(StreamDelay::DELAYED);
    let verdict = |o: OrderRequest| refusal(&o, mark(100.0));
    assert_eq!(verdict(limit("X", 100.0, delayed)), None);
    assert_eq!(verdict(limit("X", 104.9, delayed)), None, "inside, above");
    assert_eq!(verdict(limit("X", 95.1, delayed)), None, "inside, below");
    assert!(verdict(limit("X", 105.5, delayed)).is_some(), "outside, above");
    assert!(verdict(limit("X", 94.5, delayed)).is_some(), "outside, below");
    assert!(
        verdict(limit("X", f64::NAN, delayed)).is_some(),
        "a price that is no number is outside"
    );

    let stop = |trigger: f64| OrderRequest {
        trigger_price: Some(trigger),
        ..order("X", OrderType::Stop, None, delayed)
    };
    assert_eq!(verdict(stop(103.0)), None);
    assert!(verdict(stop(120.0)).is_some(), "a trigger is a price too");
    let stop_limit = OrderRequest { trigger_price: Some(101.0), ..limit("X", 130.0, delayed) };
    assert!(verdict(stop_limit).is_some(), "either of the two prices outside refuses");

    let off = verdict(limit("X", 110.0, delayed)).expect("refused");
    for needle in ["110", "100", "10.0 percent", "5 percent", "delayed", "15 minutes"] {
        assert!(off.contains(needle), "`{needle}` missing from: {off}");
    }
}

/// **A stop-limit is judged on BOTH its prices** (0140's 2026-10-27 rule: "a limit or trigger price"): inside the band
/// on both it passes; its limit or its trigger outside refuses it, naming the price; one that only closes passes; and,
/// with no price to judge, a delayed stop-limit is never taken for a market order.
#[test]
fn a_stop_limit_is_judged_on_its_limit_and_its_trigger() {
    let delayed = Some(StreamDelay::DELAYED);
    let stop_limit = |limit: f64, trigger: f64| OrderRequest {
        trigger_price: Some(trigger),
        ..order("X", OrderType::StopLimit, Some(limit), delayed)
    };
    let verdict = |o: OrderRequest| refusal(&o, mark(100.0));
    assert_eq!(verdict(stop_limit(103.5, 103.0)), None, "both inside");
    let off = verdict(stop_limit(110.0, 103.0)).expect("the limit outside refuses");
    assert!(off.contains("110") && off.contains("5 percent"), "{off}");
    let off = verdict(stop_limit(103.0, 94.0)).expect("the trigger outside refuses");
    assert!(off.contains("94"), "{off}");
    let closing = OrderRequest { reduce_only: true, ..stop_limit(130.0, 120.0) };
    assert_eq!(verdict(closing), None, "a protective stop-limit is never held up");
    let unpriced = order("X", OrderType::StopLimit, None, delayed);
    assert_eq!(refusal(&unpriced, no_lookup), None, "not a market order, and nothing to judge");
}

/// **A discretionary limit is judged at its worst price too** (L + d for a buy, L - d for a sell, which it may execute
/// at): L inside and L ± d inside passes, L ± d outside refuses it naming that price; an opening market-to-limit at the
/// auction (`AUC`) is a market order.
#[test]
fn a_discretionary_limit_is_judged_at_its_limit_and_at_its_worst_price() {
    let delayed = Some(StreamDelay::DELAYED);
    let discretionary = |side: i32, l: f64, d: f64| OrderRequest {
        side,
        attributes: Some(vike_model::OrderAttributes {
            discretionary_amount: Some(d),
            ..Default::default()
        }),
        ..limit("X", l, delayed)
    };
    let verdict = |o: OrderRequest| refusal(&o, mark(100.0));
    assert_eq!(verdict(discretionary(1, 101.0, 2.0)), None, "103 is inside");
    let off = verdict(discretionary(1, 101.0, 5.0)).expect("a buy's 106 is outside");
    assert!(off.contains("106") && off.contains("5 percent"), "{off}");
    let off = verdict(discretionary(-1, 99.0, 5.0)).expect("a sell's 94 is outside");
    assert!(off.contains("94"), "{off}");
    let auction = OrderRequest {
        time_in_force: vike_model::TimeInForce::AtOpen,
        ..order("X", OrderType::MarketToLimit, None, delayed)
    };
    assert!(refusal(&auction, no_lookup).expect("a market order").contains("MARKET-TO-LIMIT"));
}

/// **An adjustable stop is judged at the worst of its target's prices too** (the stop and the limit it may become):
/// all inside passes; a target stop or limit outside refuses it naming that price; one that only closes passes.
#[test]
fn an_adjustable_stop_is_judged_at_its_targets_prices() {
    use vike_model::{OrderStopAdjust, OrderStopAdjustTo};
    let delayed = Some(StreamDelay::DELAYED);
    let adjustable = |to| OrderRequest {
        side: -1,
        trigger_price: Some(97.0),
        adjust: Some(OrderStopAdjust { at: 103.0, to }),
        ..order("X", OrderType::Stop, None, delayed)
    };
    let verdict = |o: OrderRequest| refusal(&o, mark(100.0));
    assert_eq!(
        verdict(adjustable(OrderStopAdjustTo::StopLimit { stop: 101.0, limit: 100.5 })),
        None
    );
    let off =
        verdict(adjustable(OrderStopAdjustTo::Stop { stop: 94.0 })).expect("a target stop outside");
    assert!(off.contains("94"), "{off}");
    let off = verdict(adjustable(OrderStopAdjustTo::StopLimit { stop: 101.0, limit: 94.5 }))
        .expect("a target limit outside");
    assert!(off.contains("94.5"), "{off}");
    let closing =
        OrderRequest { reduce_only: true, ..adjustable(OrderStopAdjustTo::Stop { stop: 94.0 }) };
    assert_eq!(verdict(closing), None, "a protective stop is never held up");
}

/// **The wave-2 kinds under the rule**: a market-to-limit and a market order at an auction are market orders (refused
/// with no mark asked for); a market-if-touched is judged on its trigger, a limit-if-touched on both its prices, a
/// limit order at an auction on its limit; one that only closes passes.
#[test]
fn a_market_to_limit_is_a_market_order_and_the_if_touched_kinds_are_judged_on_their_prices() {
    let delayed = Some(StreamDelay::DELAYED);
    let mtl = order("X", OrderType::MarketToLimit, None, delayed);
    let why = refusal(&mtl, no_lookup).expect("a market-to-limit is a market order");
    assert!(why.contains("MARKET-TO-LIMIT") && why.contains("LIMIT order priced"), "{why}");
    assert_eq!(refusal(&OrderRequest { reduce_only: true, ..mtl }, no_lookup), None);
    let moc =
        OrderRequest { time_in_force: vike_model::TimeInForce::AtClose, ..market("X", delayed) };
    assert!(refusal(&moc, no_lookup).expect("market-on-close").contains("a MARKET order"));

    let verdict = |o: OrderRequest| refusal(&o, mark(100.0));
    let if_touched = |price: Option<f64>, trigger: f64| OrderRequest {
        trigger_price: Some(trigger),
        ..order("X", OrderType::TakeProfit, price, delayed)
    };
    assert_eq!(verdict(if_touched(None, 97.0)), None, "a market-if-touched inside the band");
    assert!(
        verdict(if_touched(None, 90.0)).is_some_and(|w| w.contains("90")),
        "its trigger outside"
    );
    assert_eq!(verdict(if_touched(Some(97.5), 97.0)), None, "a limit-if-touched inside on both");
    assert!(
        verdict(if_touched(Some(110.0), 97.0)).is_some_and(|w| w.contains("110")),
        "its limit outside"
    );
    let loo = OrderRequest {
        time_in_force: vike_model::TimeInForce::AtOpen,
        ..limit("X", 120.0, delayed)
    };
    assert!(
        verdict(loo).is_some_and(|w| w.contains("120")),
        "a limit-on-open is judged on its limit"
    );
}

/// **A pegged order is refused on a stream that is not real-time, every anchor, whether it opens or closes** (the owner's
/// ruling of 2026-11-09), in words and with no mark asked for: the venue keeps it at a live quote nobody here can see, so
/// the closing-order exemption does not reach it. On real-time data (or with no label) it is judged by nothing here.
#[test]
fn a_pegged_order_on_delayed_data_is_refused_whatever_its_anchor() {
    use vike_model::{OrderPeg, OrderPegAnchor};
    for anchor in OrderPegAnchor::ALL {
        for delay in [Some(StreamDelay::DELAYED), Some(StreamDelay::Frozen)] {
            let pegged = OrderRequest {
                peg: Some(OrderPeg { anchor, offset: 0.0 }),
                ..order("AAPL.SMART.USD", OrderType::Pegged, None, delay)
            };
            let closing = OrderRequest { reduce_only: true, ..pegged.clone() };
            for (what, o) in [("opening", &pegged), ("reduce-only", &closing)] {
                let why = refusal(o, no_lookup).expect("refused");
                for needle in [
                    "a PEGGED order on AAPL.SMART.USD",
                    "live quote",
                    "whether it opens or closes",
                    "LIMIT",
                ] {
                    assert!(
                        why.contains(needle),
                        "{anchor:?} {delay:?} {what}: `{needle}` missing from: {why}"
                    );
                }
            }
            for live in [None, Some(StreamDelay::RealTime)] {
                assert_eq!(
                    refusal(&OrderRequest { data_delay: live, ..pegged.clone() }, no_lookup),
                    None
                );
            }
        }
    }
}

/// **Fail-open on the band only**: with no usable mark (none noted, zero, negative, NaN) a priced order is not judged, and
/// the mark is asked for by the order's own symbol.
#[test]
fn the_band_is_not_judged_without_a_usable_mark() {
    let o = limit("MSFT.SMART.USD", 9_999.0, Some(StreamDelay::DELAYED));
    assert_eq!(refusal(&o, |_| None), None);
    for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        assert_eq!(refusal(&o, mark(bad)), None, "{bad}");
    }
    let asked = std::cell::RefCell::new(String::new());
    let _ = refusal(&o, |symbol| {
        *asked.borrow_mut() = symbol.to_string();
        Some(100.0)
    });
    assert_eq!(*asked.borrow(), "MSFT.SMART.USD");
}

/// The table answers a mark noted less than [`MARK_FRESH`] ago and nothing older or unknown.
#[test]
fn the_table_serves_a_recent_mark_only() {
    set_mark("T1.SMART.USD", 12.5, Duration::from_secs(1));
    set_mark("T2.SMART.USD", 12.5, MARK_FRESH + Duration::from_secs(1));
    assert_eq!(recent_mark("T1.SMART.USD"), Some(12.5));
    assert_eq!(recent_mark("T2.SMART.USD"), None, "older than the bound");
    assert_eq!(recent_mark("T3.SMART.USD"), None, "never noted");
}

/// The quote pump notes the MID of a two-sided quote of a stream that is not real-time, and nothing else.
#[cfg(any(feature = "ibkr-socket", feature = "ibkr-cpapi"))]
#[test]
fn only_a_two_sided_quote_of_a_non_real_time_stream_is_noted() {
    let delayed = Some(StreamDelay::DELAYED);
    note_quote("N1.SMART.USD", delayed, 99.0, 101.0);
    assert_eq!(recent_mark("N1.SMART.USD"), Some(100.0));
    note_quote("N1.SMART.USD", Some(StreamDelay::Frozen), 109.0, 111.0);
    assert_eq!(recent_mark("N1.SMART.USD"), Some(110.0), "the latest replaces the last");

    for (symbol, delay, bid, ask) in [
        ("N2.SMART.USD", Some(StreamDelay::RealTime), 99.0, 101.0),
        ("N3.SMART.USD", None, 99.0, 101.0),
        ("N4.SMART.USD", delayed, 0.0, 101.0),
        ("N5.SMART.USD", delayed, 99.0, 0.0),
        ("N6.SMART.USD", delayed, 101.0, 99.0),
        ("N7.SMART.USD", delayed, f64::NAN, 101.0),
    ] {
        note_quote(symbol, delay, bid, ask);
        assert_eq!(recent_mark(symbol), None, "{symbol} {delay:?} {bid}/{ask}");
    }
}

fn placed(fake: &IbkrTransportFake, n: usize, timeout: Duration) -> usize {
    let handle = fake.place_contracts();
    let deadline = Instant::now() + timeout;
    loop {
        let got = handle.lock().unwrap().len();
        if got >= n || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **Through the real exec loop** (the `IbkrTransportFake` records what reaches the transport): a delayed MARKET order is
/// answered with `OrderSubmitted` then a terminal `OrderRejected` carrying the words, and NO order is placed; a LIMIT inside
/// the band is placed; a LIMIT outside it is refused; an order that only closes and an order with no label are placed.
#[test]
fn the_execution_loop_refuses_a_delayed_market_order_and_places_the_orders_the_rule_lets_through() {
    vike_log::test_init();
    const SYM: &str = "E07";
    set_mark(SYM, 100.0, Duration::ZERO);
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let probe = fake.clone();
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let delayed = Some(StreamDelay::DELAYED);

    handle.submit(&OrderRequest { order_client_id: "m-delayed".into(), ..market(SYM, delayed) });
    let evs = drain(|| rx.try_recv().ok(), 2, Duration::from_secs(2));
    assert_eq!(evs.len(), 2, "Submitted then Rejected: {evs:?}");
    assert!(matches!(evs[0], Event::OrderSubmitted(_)), "{evs:?}");
    let Event::OrderRejected(ref r) = evs[1] else { panic!("expected a rejection: {evs:?}") };
    assert_eq!(r.order_client_id, "m-delayed");
    let reason = r.reason.to_string();
    assert!(reason.contains("MARKET") && reason.contains(SYM), "{reason}");
    assert_eq!(
        placed(&probe, 1, Duration::from_millis(300)),
        0,
        "the refused order reached the transport"
    );

    handle
        .submit(&OrderRequest { order_client_id: "l-inside".into(), ..limit(SYM, 101.0, delayed) });
    assert_eq!(placed(&probe, 1, Duration::from_secs(2)), 1, "a limit inside the band is placed");

    handle.submit(&OrderRequest {
        order_client_id: "l-outside".into(),
        ..limit(SYM, 120.0, delayed)
    });
    let evs = drain(|| rx.try_recv().ok(), 4, Duration::from_secs(2));
    let rejected: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some((r.order_client_id.clone(), r.reason.to_string())),
            _ => None,
        })
        .collect();
    assert_eq!(rejected.len(), 1, "only the limit outside the band is rejected: {evs:?}");
    assert_eq!(rejected[0].0, "l-outside");
    assert!(
        rejected[0].1.contains("5 percent") && rejected[0].1.contains("120"),
        "{}",
        rejected[0].1
    );
    assert_eq!(
        placed(&probe, 2, Duration::from_millis(300)),
        1,
        "the refused limit was not placed"
    );

    let closing = OrderRequest {
        order_client_id: "m-closing".into(),
        reduce_only: true,
        ..market(SYM, delayed)
    };
    handle.submit(&closing);
    assert_eq!(placed(&probe, 2, Duration::from_secs(2)), 2, "an order that only closes is placed");
    handle.submit(&OrderRequest { order_client_id: "m-unlabelled".into(), ..market(SYM, None) });
    assert_eq!(placed(&probe, 3, Duration::from_secs(2)), 3, "no label, no rule: placed as before");
    handle.detach();
}

/// **Through the real exec loop, a delayed STOP-LIMIT**: inside the band on both prices it reaches the transport as
/// TWS's `STP LMT` with its limit and its trigger; its trigger outside the band is refused and never placed.
#[test]
fn the_execution_loop_places_a_delayed_stop_limit_inside_the_band_and_refuses_one_outside() {
    vike_log::test_init();
    const SYM: &str = "E07SL";
    set_mark(SYM, 100.0, Duration::ZERO);
    let (events, mut rx) = event_channel(64);
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let specs = fake.place_specs();
    let probe = fake.clone();
    let mut handle = crate::run_execution_for_test(events, Box::new(fake));
    let delayed = Some(StreamDelay::DELAYED);
    let stop_limit = |order_client_id: &str, limit: f64, trigger: f64| OrderRequest {
        order_client_id: order_client_id.into(),
        trigger_price: Some(trigger),
        ..order(SYM, OrderType::StopLimit, Some(limit), delayed)
    };

    handle.submit(&stop_limit("sl-inside", 102.5, 102.0));
    assert_eq!(placed(&probe, 1, Duration::from_secs(2)), 1, "inside the band: placed");
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(spec.order_type, "STP LMT");
    assert_eq!((spec.lmt_price, spec.aux_price), (Some(102.5), Some(102.0)));

    handle.submit(&stop_limit("sl-outside", 102.5, 120.0));
    let evs = drain(|| rx.try_recv().ok(), 4, Duration::from_secs(2));
    assert!(
        evs.iter().any(|e| matches!(e, Event::OrderRejected(r)
            if r.order_client_id == "sl-outside" && r.reason.contains("120"))),
        "the trigger outside the band is refused: {evs:?}"
    );
    assert_eq!(placed(&probe, 2, Duration::from_millis(300)), 1, "the refused one was not placed");
    handle.detach();
}
