use super::*;
use compact_str::CompactString;
use vike_model::events::*;

/// The pre-change `describe_event` body, VERBATIM. This is the oracle: it is what the GUI /
/// `vike-cli trade` event pane / live smokes were reading before the ring went lazy, so the
/// lazy renderer is only correct if it reproduces this character for character.
fn describe_event_reference(event: &Event) -> String {
    match event {
        Event::Fill(e) => {
            format!("FillEvent {} {}@{}", e.order_client_id, e.last_qty, e.last_price)
        }
        Event::OrderSubmitted(e) => format!("OrderSubmitted {}", e.order_client_id),
        Event::OrderAccepted(e) => format!("OrderAccepted {}", e.order_client_id),
        Event::OrderRejected(e) => {
            format!("OrderRejected {} ({})", e.order_client_id, e.reason)
        }
        Event::OrderDenied(e) => format!("OrderDenied {} ({})", e.order_client_id, e.reason),
        Event::OrderTriggered(e) => format!("OrderTriggered {}", e.order_client_id),
        Event::OrderPartiallyFilled(e) => {
            format!("OrderPartiallyFilled {}", e.order_client_id)
        }
        Event::OrderFilled(e) => format!("OrderFilled {}", e.order_client_id),
        Event::OrderCanceled(e) => format!("OrderCanceled {}", e.order_client_id),
        Event::OrderExpired(e) => format!("OrderExpired {}", e.order_client_id),
        Event::OrderLiquidated(e) => format!("OrderLiquidated {}", e.order_client_id),
        Event::OrderModified(e) => {
            format!("OrderModified {} qty={:?} px={:?}", e.order_client_id, e.new_qty, e.new_price)
        }
        Event::OrderCancelRejected(e) => {
            format!("OrderCancelRejected {} ({})", e.order_client_id, e.reason)
        }
        Event::OrderModifyRejected(e) => {
            format!("OrderModifyRejected {} ({})", e.order_client_id, e.reason)
        }
        Event::PositionOpened(e) => format!("PositionOpened {}", e.symbol),
        Event::PositionChanged(e) => format!("PositionChanged {}", e.symbol),
        Event::PositionClosed(e) => format!("PositionClosed {}", e.symbol),
        Event::AccountState(e) => format!("AccountState {}", e.venue),
        Event::Funding(e) => format!("FundingEvent {} {}", e.symbol, e.amount),
        Event::PositionLiquidated(e) => format!("PositionLiquidated {} {}", e.symbol, e.qty),
        // The one line that changed (B1.22): it now names the event by its wire tag, as every other does.
        Event::FeeAdjustment(e) => {
            format!("FeeAdjustmentEvent {} {}", e.symbol, e.extra_commission)
        }
    }
}

fn fill() -> FillEvent {
    FillEvent {
        trade_id: "t1".into(), // a source literal — `TradeId: From<&'static str>`
        order_client_id: "deadbeef12".to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.5,
        last_price: 100.25,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Maker,
        timestamp: 7,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

/// One instance of EVERY `Event` variant. A new variant makes the exhaustive `match` in
/// `EventNote::capture` fail to compile, and this list is where its line gets pinned.
fn every_variant() -> Vec<Event> {
    let order_client_id = || "deadbeef12".to_string();
    vec![
        Event::Fill(fill()),
        Event::OrderSubmitted(OrderSubmitted { order_client_id: order_client_id(), timestamp: 1 }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id(),
            venue_order_id: Some(CompactString::new("v1")),
            timestamp: 1,
        }),
        Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id(),
            reason: CompactString::new("insufficient balance"),
            timestamp: 1,
            refusal_id: None,
        }),
        Event::OrderDenied(OrderDenied {
            order_client_id: order_client_id(),
            reason: CompactString::new("RiskGate: max_order_qty"),
            timestamp: 1,
            refusal_id: None,
        }),
        Event::OrderTriggered(OrderTriggered { order_client_id: order_client_id(), timestamp: 1 }),
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id: order_client_id(),
            fill: fill(),
            timestamp: 1,
        }),
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id(),
            fill: fill(),
            timestamp: 1,
        }),
        // NON-EMPTY reason on purpose: `OrderCanceled` carries one but the line has never
        // shown it, and an empty fixture would let a wrongly-added `({reason})` render as
        // `"OrderCanceled c ()"` -> caught, but a CORRECT-looking empty tail could also hide a
        // missing one. A real reason makes both directions loud.
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id(),
            reason: CompactString::new("user requested"),
            timestamp: 1,
            post_only_would_cross: false,
            refusal_id: None,
        }),
        Event::OrderExpired(OrderExpired { order_client_id: order_client_id(), timestamp: 1 }),
        Event::OrderLiquidated(OrderLiquidated {
            order_client_id: order_client_id(),
            liquidation_price: 9.0,
            timestamp: 1,
        }),
        // both Option arms of the `{:?}` rendering
        Event::OrderModified(OrderModified {
            order_client_id: order_client_id(),
            venue_order_id: None,
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
            timestamp: 1,
        }),
        Event::OrderModified(OrderModified {
            order_client_id: order_client_id(),
            venue_order_id: None,
            new_qty: None,
            new_price: Some(101.5),
            new_trigger_price: None,
            timestamp: 1,
        }),
        Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: order_client_id(),
            reason: CompactString::new("network error: timed out"),
            timestamp: 1,
            refusal_id: None,
        }),
        Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order_client_id(),
            reason: CompactString::new("modify rejected"),
            timestamp: 1,
            refusal_id: None,
        }),
        Event::PositionOpened(PositionOpened {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 1.0,
            average_price: 100.0,
            timestamp: 1,
            mark_price: None,
        }),
        Event::PositionChanged(PositionChanged {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            average_price: 100.0,
            realized_pnl: 0.0,
            timestamp: 1,
            mark_price: None,
        }),
        Event::PositionClosed(PositionClosed {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            realized_pnl: 5.0,
            timestamp: 1,
        }),
        Event::AccountState(AccountState {
            venue: "sim".into(),
            balances: vec![("USDT".to_string(), 10.0)],
            timestamp: 1,
            route_key: None,
        }),
        Event::Funding(FundingEvent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            funding_rate: 0.0001,
            amount: -0.25,
            mark_price: None,
            timestamp: 1,
            route_key: None,
            currency: None,
        }),
        Event::PositionLiquidated(PositionLiquidated {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            position_side: PositionSide::Both,
            qty: 3.0,
            liquidation_price: 50.0,
            fee: 0.1,
            timestamp: 1,
            trade_id: CompactString::new("l1"),
            route_key: None,
            fee_currency: None,
        }),
        Event::FeeAdjustment(vike_model::events::FeeAdjustmentEvent {
            trade_id: "t1".into(),
            order_client_id: order_client_id(),
            venue: "ibkr".into(),
            symbol: "AAPL.SMART.USD".into(),
            last_price: 190.0,
            extra_commission: 1.25,
            commission_asset: "USD".into(),
            timestamp: 1,
        }),
    ]
}

/// THE contract: capture-then-render is byte-identical to the eager `format!` it replaced,
/// for every event variant (and both `Option` arms of the `OrderModified` line).
#[test]
fn render_matches_the_reference_format() {
    for event in every_variant() {
        let want = describe_event_reference(&event);
        let got = RecentNote::Event(EventNote::capture(&event)).render();
        assert_eq!(got, want, "lazy render drifted from the reference for {event:?}");
        // Every line opens with the event's wire tag (`FillEvent` and `OrderModified` are spelled
        // in `Display`, the rest come from `Event::wire_tag` at the capture).
        let tag = format!("{} ", event.wire_tag());
        assert!(got.starts_with(&tag), "{got:?} does not open with {tag:?}");
    }
}

/// The salvaged-event line keeps its prefix, byte-identically to the old nested `format!`.
#[test]
fn lost_note_keeps_the_panic_prefix() {
    for event in every_variant() {
        let want = format!("LOST(panic mid-fold) {}", describe_event_reference(&event));
        assert_eq!(RecentNote::Lost(EventNote::capture(&event)).render(), want);
    }
}

/// A runtime-authored note is passed through verbatim.
#[test]
fn text_notes_round_trip_verbatim() {
    let s = "DRIFT position sim/BTCUSDT[BOTH]: local 1 vs venue 0";
    assert_eq!(RecentNote::from(s).render(), s);
    assert_eq!(RecentNote::from(s.to_string()).render(), s);
}

/// The allocation claim the module doc makes: a live-shaped order client id fits `CompactString`'s inline
/// budget, so capturing a fill's line never touches the heap for the id.
#[test]
fn a_live_shaped_order_client_id_stays_inline() {
    // `<8-hex session><seq>` — the `OrderClientIdGenerator` wire form, at an absurd seq.
    let order_client_id = CompactString::new("deadbeef18446744073709551615");
    assert!(
        order_client_id.len() > 24,
        "precondition: this one is deliberately over the inline budget"
    );
    assert!(order_client_id.is_heap_allocated(), "…and therefore heap");
    // A realistic one is not.
    let real = CompactString::new("deadbeef1234567");
    assert!(
        !real.is_heap_allocated(),
        "a live order client id must stay inline (no fold-path malloc)"
    );
}
