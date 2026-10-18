use super::*;
use compact_str::CompactString;
use vike_model::events::*;

/// The pre-change `describe_event` body, VERBATIM. This is the oracle: it is what the GUI /
/// `vike-cli trade` event pane / live smokes were reading before the ring went lazy, so the
/// lazy renderer is only correct if it reproduces this character for character.
fn describe_event_reference(ev: &Event) -> String {
    match ev {
        Event::Fill(e) => {
            format!("FillEvent {} {}@{}", e.client_order_id, e.last_qty, e.last_px)
        }
        Event::OrderSubmitted(e) => format!("OrderSubmitted {}", e.client_order_id),
        Event::OrderAccepted(e) => format!("OrderAccepted {}", e.client_order_id),
        Event::OrderRejected(e) => {
            format!("OrderRejected {} ({})", e.client_order_id, e.reason)
        }
        Event::OrderDenied(e) => format!("OrderDenied {} ({})", e.client_order_id, e.reason),
        Event::OrderTriggered(e) => format!("OrderTriggered {}", e.client_order_id),
        Event::OrderPartiallyFilled(e) => {
            format!("OrderPartiallyFilled {}", e.client_order_id)
        }
        Event::OrderFilled(e) => format!("OrderFilled {}", e.client_order_id),
        Event::OrderCanceled(e) => format!("OrderCanceled {}", e.client_order_id),
        Event::OrderExpired(e) => format!("OrderExpired {}", e.client_order_id),
        Event::OrderLiquidated(e) => format!("OrderLiquidated {}", e.client_order_id),
        Event::OrderModified(e) => {
            format!("OrderModified {} qty={:?} px={:?}", e.client_order_id, e.new_qty, e.new_price)
        }
        Event::OrderCancelRejected(e) => {
            format!("OrderCancelRejected {} ({})", e.client_order_id, e.reason)
        }
        Event::OrderModifyRejected(e) => {
            format!("OrderModifyRejected {} ({})", e.client_order_id, e.reason)
        }
        Event::PositionOpened(e) => format!("PositionOpened {}", e.symbol),
        Event::PositionChanged(e) => format!("PositionChanged {}", e.symbol),
        Event::PositionClosed(e) => format!("PositionClosed {}", e.symbol),
        Event::AccountState(e) => format!("AccountState {}", e.venue),
        Event::Funding(e) => format!("FundingEvent {} {}", e.symbol, e.amount),
        Event::PositionLiquidated(e) => format!("PositionLiquidated {} {}", e.symbol, e.qty),
    }
}

fn fill() -> FillEvent {
    FillEvent {
        trade_id: "t1".into(), // a source literal — `TradeId: From<&'static str>`
        client_order_id: "deadbeef12".to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.5,
        last_px: 100.25,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Maker,
        ts: 7,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// One instance of EVERY `Event` variant. A new variant makes the exhaustive `match` in
/// `EventNote::capture` fail to compile, and this list is where its line gets pinned.
fn every_variant() -> Vec<Event> {
    let coid = || "deadbeef12".to_string();
    vec![
        Event::Fill(fill()),
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid(), ts: 1 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid(),
            venue_order_id: Some(CompactString::new("v1")),
            ts: 1,
        }),
        Event::OrderRejected(OrderRejected {
            client_order_id: coid(),
            reason: CompactString::new("insufficient balance"),
            ts: 1,
        }),
        Event::OrderDenied(OrderDenied {
            client_order_id: coid(),
            reason: CompactString::new("RiskGate: max_order_qty"),
            ts: 1,
        }),
        Event::OrderTriggered(OrderTriggered { client_order_id: coid(), ts: 1 }),
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid(),
            fill: fill(),
            ts: 1,
        }),
        Event::OrderFilled(OrderFilled { client_order_id: coid(), fill: fill(), ts: 1 }),
        // NON-EMPTY reason on purpose: `OrderCanceled` carries one but the line has never
        // shown it, and an empty fixture would let a wrongly-added `({reason})` render as
        // `"OrderCanceled c ()"` -> caught, but a CORRECT-looking empty tail could also hide a
        // missing one. A real reason makes both directions loud.
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid(),
            reason: CompactString::new("user requested"),
            ts: 1,
        }),
        Event::OrderExpired(OrderExpired { client_order_id: coid(), ts: 1 }),
        Event::OrderLiquidated(OrderLiquidated { client_order_id: coid(), liq_price: 9.0, ts: 1 }),
        // both Option arms of the `{:?}` rendering
        Event::OrderModified(OrderModified {
            client_order_id: coid(),
            venue_order_id: None,
            new_qty: Some(2.0),
            new_price: None,
            ts: 1,
        }),
        Event::OrderModified(OrderModified {
            client_order_id: coid(),
            venue_order_id: None,
            new_qty: None,
            new_price: Some(101.5),
            ts: 1,
        }),
        Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: coid(),
            reason: CompactString::new("network error: timed out"),
            ts: 1,
        }),
        Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: coid(),
            reason: CompactString::new("modify rejected"),
            ts: 1,
        }),
        Event::PositionOpened(PositionOpened {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 1.0,
            avg_px: 100.0,
            ts: 1,
            mark_price: None,
        }),
        Event::PositionChanged(PositionChanged {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            avg_px: 100.0,
            realized_pnl: 0.0,
            ts: 1,
            mark_price: None,
        }),
        Event::PositionClosed(PositionClosed {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            realized_pnl: 5.0,
            ts: 1,
        }),
        Event::AccountState(AccountState {
            venue: "sim".into(),
            balances: vec![("USDT".to_string(), 10.0)],
            ts: 1,
            route_key: None,
        }),
        Event::Funding(FundingEvent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: -0.25,
            mark_price: None,
            ts: 1,
            route_key: None,
        }),
        Event::PositionLiquidated(PositionLiquidated {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 3.0,
            liq_price: 50.0,
            fee: 0.1,
            ts: 1,
            trade_id: CompactString::new("l1"),
            route_key: None,
        }),
    ]
}

/// THE contract: capture-then-render is byte-identical to the eager `format!` it replaced,
/// for every event variant (and both `Option` arms of the `OrderModified` line).
#[test]
fn render_matches_the_reference_format() {
    for ev in every_variant() {
        let want = describe_event_reference(&ev);
        let got = RecentNote::Event(EventNote::capture(&ev)).render();
        assert_eq!(got, want, "lazy render drifted from the reference for {ev:?}");
    }
}

/// The salvaged-event line keeps its prefix, byte-identically to the old nested `format!`.
#[test]
fn lost_note_keeps_the_panic_prefix() {
    for ev in every_variant() {
        let want = format!("LOST(panic mid-fold) {}", describe_event_reference(&ev));
        assert_eq!(RecentNote::Lost(EventNote::capture(&ev)).render(), want);
    }
}

/// A runtime-authored note is passed through verbatim.
#[test]
fn text_notes_round_trip_verbatim() {
    let s = "DRIFT position sim/BTCUSDT[BOTH]: local 1 vs venue 0";
    assert_eq!(RecentNote::from(s).render(), s);
    assert_eq!(RecentNote::from(s.to_string()).render(), s);
}

/// The allocation claim the module doc makes: a live-shaped coid fits `CompactString`'s inline
/// budget, so capturing a fill's line never touches the heap for the id.
#[test]
fn a_live_shaped_coid_stays_inline() {
    // `<8-hex session><seq>` — the `ClientOrderIdGenerator` wire form, at an absurd seq.
    let coid = CompactString::new("deadbeef18446744073709551615");
    assert!(coid.len() > 24, "precondition: this one is deliberately over the inline budget");
    assert!(coid.is_heap_allocated(), "…and therefore heap");
    // A realistic one is not.
    let real = CompactString::new("deadbeef1234567");
    assert!(!real.is_heap_allocated(), "a live coid must stay inline (no fold-path malloc)");
}
