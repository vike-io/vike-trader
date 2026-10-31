//! The delayed-data rule (E07, `crate::delayed_order`): the pure verdict, the mark table, and the real exec loop over
//! the `FakeTransport` (a refused order reaches no transport; the orders the rule lets through do).

use std::time::{Duration, Instant};

use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;

use super::*;
use crate::testing::{FakeTransport, ScriptedInbound};

fn order(
    symbol: &str,
    kind: OrderType,
    price: Option<f64>,
    delay: Option<DataDelay>,
) -> OrderRequest {
    OrderRequest {
        client_order_id: format!("coid-{symbol}"),
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

fn market(symbol: &str, delay: Option<DataDelay>) -> OrderRequest {
    order(symbol, OrderType::Market, None, delay)
}

fn limit(symbol: &str, price: f64, delay: Option<DataDelay>) -> OrderRequest {
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
        refusal(&market("AAPL.SMART.USD", Some(DataDelay::DELAYED)), no_lookup).expect("refused");
    for needle in
        ["AAPL.SMART.USD", "MARKET", "delayed", "15 minutes", "LIMIT", "5 percent", "reduce-only"]
    {
        assert!(why.contains(needle), "`{needle}` missing from: {why}");
    }
    let why =
        refusal(&market("AAPL.SMART.USD", Some(DataDelay::Frozen)), no_lookup).expect("refused");
    assert!(why.contains("frozen") && !why.contains("15 minutes"), "{why}");
    let stated = DataDelay::Delayed { lag_ms: Some(900_000) };
    assert!(refusal(&market("AAPL.SMART.USD", Some(stated)), no_lookup).is_some());
}

/// **Nothing else is judged**: no label, a real-time label, an order that only closes, a combo, and an order carrying no
/// price at all (nothing to judge) pass whatever they are, and the mark is never asked for.
#[test]
fn an_order_without_a_non_real_time_label_or_that_only_closes_is_never_refused() {
    let wild = |delay| limit("AAPL.SMART.USD", 9_999.0, delay);
    assert_eq!(refusal(&market("AAPL.SMART.USD", None), no_lookup), None, "no label");
    assert_eq!(refusal(&wild(None), no_lookup), None);
    assert_eq!(refusal(&market("AAPL.SMART.USD", Some(DataDelay::RealTime)), no_lookup), None);
    assert_eq!(refusal(&wild(Some(DataDelay::RealTime)), no_lookup), None);

    let closing =
        OrderRequest { reduce_only: true, ..market("AAPL.SMART.USD", Some(DataDelay::DELAYED)) };
    assert_eq!(refusal(&closing, no_lookup), None, "a flatten is never held up");
    let closing = OrderRequest { reduce_only: true, ..wild(Some(DataDelay::DELAYED)) };
    assert_eq!(refusal(&closing, no_lookup), None, "nor a protective exit priced far by design");

    let unpriced = order(
        "AAPL.SMART.USD",
        OrderType::Unrecognized("moc".into()),
        None,
        Some(DataDelay::DELAYED),
    );
    assert_eq!(refusal(&unpriced, no_lookup), None, "a kind with no price and not a market order");
}

/// **The band is 5 percent of the delayed mark, both ways, on the price and on a trigger.**
#[test]
fn a_price_outside_five_percent_of_the_delayed_mark_is_refused_both_ways() {
    let delayed = Some(DataDelay::DELAYED);
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

/// **Fail-open on the band only**: with no usable mark (none noted, zero, negative, NaN) a priced order is not judged, and
/// the mark is asked for by the order's own symbol.
#[test]
fn the_band_is_not_judged_without_a_usable_mark() {
    let o = limit("MSFT.SMART.USD", 9_999.0, Some(DataDelay::DELAYED));
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
    let delayed = Some(DataDelay::DELAYED);
    note_quote("N1.SMART.USD", delayed, 99.0, 101.0);
    assert_eq!(recent_mark("N1.SMART.USD"), Some(100.0));
    note_quote("N1.SMART.USD", Some(DataDelay::Frozen), 109.0, 111.0);
    assert_eq!(recent_mark("N1.SMART.USD"), Some(110.0), "the latest replaces the last");

    for (symbol, delay, bid, ask) in [
        ("N2.SMART.USD", Some(DataDelay::RealTime), 99.0, 101.0),
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

fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize, timeout: Duration) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => out.push(ev),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

fn placed(fake: &FakeTransport, n: usize, timeout: Duration) -> usize {
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

/// **Through the real exec loop** (the `FakeTransport` records what reaches the transport): a delayed MARKET order is
/// answered with `OrderSubmitted` then a terminal `OrderRejected` carrying the words, and NO order is placed; a LIMIT inside
/// the band is placed; a LIMIT outside it is refused; an order that only closes and an order with no label are placed.
#[test]
fn the_exec_loop_refuses_a_delayed_market_order_and_places_the_orders_the_rule_lets_through() {
    vike_log::test_init();
    const SYM: &str = "E07.SMART.USD";
    set_mark(SYM, 100.0, Duration::ZERO);
    let (events, mut rx) = event_channel(64);
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    let probe = fake.clone();
    let mut handle = crate::run_exec_for_test(events, Box::new(fake));
    let delayed = Some(DataDelay::DELAYED);

    handle.submit(&OrderRequest { client_order_id: "m-delayed".into(), ..market(SYM, delayed) });
    let evs = drain(&mut rx, 2, Duration::from_secs(2));
    assert_eq!(evs.len(), 2, "Submitted then Rejected: {evs:?}");
    assert!(matches!(evs[0], Event::OrderSubmitted(_)), "{evs:?}");
    let Event::OrderRejected(ref r) = evs[1] else { panic!("expected a rejection: {evs:?}") };
    assert_eq!(r.client_order_id, "m-delayed");
    let reason = r.reason.to_string();
    assert!(reason.contains("MARKET") && reason.contains(SYM), "{reason}");
    assert_eq!(
        placed(&probe, 1, Duration::from_millis(300)),
        0,
        "the refused order reached the transport"
    );

    handle
        .submit(&OrderRequest { client_order_id: "l-inside".into(), ..limit(SYM, 101.0, delayed) });
    assert_eq!(placed(&probe, 1, Duration::from_secs(2)), 1, "a limit inside the band is placed");

    handle.submit(&OrderRequest {
        client_order_id: "l-outside".into(),
        ..limit(SYM, 120.0, delayed)
    });
    let evs = drain(&mut rx, 4, Duration::from_secs(2));
    let rejected: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some((r.client_order_id.clone(), r.reason.to_string())),
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
        client_order_id: "m-closing".into(),
        reduce_only: true,
        ..market(SYM, delayed)
    };
    handle.submit(&closing);
    assert_eq!(placed(&probe, 2, Duration::from_secs(2)), 2, "an order that only closes is placed");
    handle.submit(&OrderRequest { client_order_id: "m-unlabelled".into(), ..market(SYM, None) });
    assert_eq!(placed(&probe, 3, Duration::from_secs(2)), 3, "no label, no rule: placed as before");
    handle.detach();
}
