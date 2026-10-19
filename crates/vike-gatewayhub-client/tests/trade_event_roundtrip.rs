#![cfg(feature = "trade")]
//! **R9 - event fidelity: all 20 `Event` variants cross the trade hop BIT-EXACT, in order, once.**
//!
//! `TradeDown::Event` carries one venue event with its ring cursor. This suite generates every variant
//! of `vike_model::events::Event` with the float classes that bite (`-0.0`, subnormals, the largest
//! normals), pushes them through the REAL client against the fake peer, and compares what reached the
//! core's lane with what the daemon sent. A float is compared by its serialized text, which for a finite
//! `f64` is injective (shortest round trip) - `-0.0` and `0.0` differ, so a `==` that forgives them
//! cannot hide a flipped sign - and by `PartialEq` besides. The cursor and the route key ride along.
//!
//! A non-finite float serializes to `null` and cannot cross; the daemon drops such a row rather than
//! send it (the `proto` module doc), so the strategies generate finite floats only.
//!
//! If a counterexample appears, commit proptest's failure-persistence seed BESIDE this file as
//! crates/vike-gatewayhub-client/tests/trade_event_roundtrip.proptest-regressions (written without
//! backticks: the file does not exist until a counterexample does) - not under a `proptest-regressions/`
//! directory, which proptest never creates for an integration test.

use std::time::Duration;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestCaseError;
use vike_exec::Ingest;
use vike_gatewayhub_client::proto::{TradeDown, read_frame_raw, write_frame};
use vike_model::events::{
    AccountState, Event, FillEvent, FundingEvent, LiquiditySide, OrderAccepted,
    OrderCancelRejected, OrderCanceled, OrderDenied, OrderExpired, OrderFilled, OrderLiquidated,
    OrderModified, OrderModifyRejected, OrderPartiallyFilled, OrderRejected, OrderSubmitted,
    OrderTriggered, PositionChanged, PositionClosed, PositionLiquidated, PositionOpened,
    PositionSide, TradeId,
};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{TradePeer, TradePeerConfig, open};

fn finite() -> impl Strategy<Value = f64> {
    proptest::num::f64::POSITIVE
        | proptest::num::f64::NEGATIVE
        | proptest::num::f64::NORMAL
        | proptest::num::f64::SUBNORMAL
        | proptest::num::f64::ZERO
}

fn opt_f64() -> impl Strategy<Value = Option<f64>> {
    proptest::option::of(finite())
}

fn text() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_-]{1,12}"
}

/// A lifecycle event's two fields: the client order id and the stamp.
fn lifecycle() -> impl Strategy<Value = (String, i64)> {
    (text(), any::<i64>())
}

fn side() -> impl Strategy<Value = PositionSide> {
    prop_oneof![Just(PositionSide::Both), Just(PositionSide::Long), Just(PositionSide::Short)]
}

fn liquidity() -> impl Strategy<Value = LiquiditySide> {
    prop_oneof![
        Just(LiquiditySide::Maker),
        Just(LiquiditySide::Taker),
        Just(LiquiditySide::Unknown)
    ]
}

fn fill() -> impl Strategy<Value = FillEvent> {
    (
        (text(), text(), text(), text()),
        (any::<i32>(), finite(), finite(), finite()),
        (text(), liquidity(), any::<i64>(), opt_f64(), side()),
    )
        .prop_map(
            |(
                (trade_id, coid, venue, symbol),
                (side_i, last_qty, last_px, commission),
                (asset, liquidity_side, ts, mark_price, position_side),
            )| FillEvent {
                trade_id: TradeId::new(&trade_id).expect("the regex generates a non-empty id"),
                client_order_id: coid,
                venue: venue.as_str().into(),
                symbol: symbol.as_str().into(),
                side: side_i,
                last_qty,
                last_px,
                commission,
                commission_asset: asset.as_str().into(),
                liquidity_side,
                ts,
                mark_price,
                position_side,
            },
        )
}

/// One of every `Event` variant (the `match` in `every_variant_is_generated` fails to COMPILE when a
/// variant is added and not listed).
fn event() -> impl Strategy<Value = Event> {
    prop_oneof![
        fill().prop_map(Event::Fill),
        lifecycle()
            .prop_map(|(c, ts)| Event::OrderSubmitted(OrderSubmitted { client_order_id: c, ts })),
        (text(), proptest::option::of(text()), any::<i64>()).prop_map(|(c, v, ts)| {
            Event::OrderAccepted(OrderAccepted {
                client_order_id: c,
                venue_order_id: v.map(|v| v.as_str().into()),
                ts,
            })
        }),
        (text(), text(), any::<i64>()).prop_map(|(c, r, ts)| Event::OrderRejected(OrderRejected {
            client_order_id: c,
            reason: r.as_str().into(),
            ts
        })),
        (text(), text(), any::<i64>()).prop_map(|(c, r, ts)| Event::OrderDenied(OrderDenied {
            client_order_id: c,
            reason: r.as_str().into(),
            ts
        })),
        lifecycle()
            .prop_map(|(c, ts)| Event::OrderTriggered(OrderTriggered { client_order_id: c, ts })),
        (text(), fill(), any::<i64>()).prop_map(|(c, f, ts)| {
            Event::OrderPartiallyFilled(OrderPartiallyFilled { client_order_id: c, fill: f, ts })
        }),
        (text(), fill(), any::<i64>()).prop_map(|(c, f, ts)| {
            Event::OrderFilled(OrderFilled { client_order_id: c, fill: f, ts })
        }),
        (text(), text(), any::<i64>()).prop_map(|(c, r, ts)| Event::OrderCanceled(OrderCanceled {
            client_order_id: c,
            reason: r.as_str().into(),
            ts
        })),
        lifecycle()
            .prop_map(|(c, ts)| Event::OrderExpired(OrderExpired { client_order_id: c, ts })),
        (text(), finite(), any::<i64>()).prop_map(|(c, liq_price, ts)| {
            Event::OrderLiquidated(OrderLiquidated { client_order_id: c, liq_price, ts })
        }),
        (text(), proptest::option::of(text()), opt_f64(), opt_f64(), any::<i64>()).prop_map(
            |(c, v, new_qty, new_price, ts)| {
                Event::OrderModified(OrderModified {
                    client_order_id: c,
                    venue_order_id: v.map(|v| v.as_str().into()),
                    new_qty,
                    new_price,
                    ts,
                })
            }
        ),
        (text(), text(), side(), finite(), finite(), any::<i64>(), opt_f64()).prop_map(
            |(venue, symbol, position_side, qty, avg_px, ts, mark_price)| {
                Event::PositionOpened(PositionOpened {
                    venue: venue.as_str().into(),
                    symbol: symbol.as_str().into(),
                    position_side,
                    qty,
                    avg_px,
                    ts,
                    mark_price,
                })
            }
        ),
        (text(), text(), side(), (finite(), finite(), finite()), any::<i64>(), opt_f64()).prop_map(
            |(venue, symbol, position_side, (qty, avg_px, realized_pnl), ts, mark_price)| {
                Event::PositionChanged(PositionChanged {
                    venue: venue.as_str().into(),
                    symbol: symbol.as_str().into(),
                    position_side,
                    qty,
                    avg_px,
                    realized_pnl,
                    ts,
                    mark_price,
                })
            }
        ),
        (text(), text(), side(), finite(), any::<i64>()).prop_map(
            |(venue, symbol, position_side, realized_pnl, ts)| {
                Event::PositionClosed(PositionClosed {
                    venue: venue.as_str().into(),
                    symbol: symbol.as_str().into(),
                    position_side,
                    realized_pnl,
                    ts,
                })
            }
        ),
        (
            text(),
            proptest::collection::vec((text(), finite()), 0..4),
            any::<i64>(),
            proptest::option::of(text())
        )
            .prop_map(|(venue, balances, ts, route_key)| {
                Event::AccountState(AccountState {
                    venue: venue.as_str().into(),
                    balances,
                    ts,
                    route_key: route_key.map(|k| k.as_str().into()),
                })
            }),
        (
            text(),
            text(),
            side(),
            (finite(), finite()),
            opt_f64(),
            any::<i64>(),
            proptest::option::of(text())
        )
            .prop_map(
                |(
                    venue,
                    symbol,
                    position_side,
                    (funding_rate, amount),
                    mark_price,
                    ts,
                    route_key,
                )| {
                    Event::Funding(FundingEvent {
                        venue: venue.as_str().into(),
                        symbol: symbol.as_str().into(),
                        position_side,
                        funding_rate,
                        amount,
                        mark_price,
                        ts,
                        route_key: route_key.map(|k| k.as_str().into()),
                    })
                }
            ),
        (
            text(),
            text(),
            side(),
            (finite(), finite(), finite()),
            any::<i64>(),
            text(),
            proptest::option::of(text())
        )
            .prop_map(
                |(venue, symbol, position_side, (qty, liq_price, fee), ts, trade_id, route_key)| {
                    Event::PositionLiquidated(PositionLiquidated {
                        venue: venue.as_str().into(),
                        symbol: symbol.as_str().into(),
                        position_side,
                        qty,
                        liq_price,
                        fee,
                        ts,
                        trade_id: trade_id.as_str().into(),
                        route_key: route_key.map(|k| k.as_str().into()),
                    })
                }
            ),
        (text(), text(), any::<i64>()).prop_map(|(c, r, ts)| {
            Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: c,
                reason: r.as_str().into(),
                ts,
            })
        }),
        (text(), text(), any::<i64>()).prop_map(|(c, r, ts)| {
            Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: c,
                reason: r.as_str().into(),
                ts,
            })
        }),
    ]
}

/// The generator names every variant: the day `Event` grows one, this stops compiling until the
/// strategy above is taught it.
#[test]
fn every_variant_is_generated() {
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..4_000 {
        let e = event().new_tree(&mut runner).expect("a value").current();
        let tag = match e {
            Event::Fill(_) => "Fill",
            Event::OrderSubmitted(_) => "OrderSubmitted",
            Event::OrderAccepted(_) => "OrderAccepted",
            Event::OrderRejected(_) => "OrderRejected",
            Event::OrderDenied(_) => "OrderDenied",
            Event::OrderTriggered(_) => "OrderTriggered",
            Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            Event::OrderExpired(_) => "OrderExpired",
            Event::OrderLiquidated(_) => "OrderLiquidated",
            Event::OrderModified(_) => "OrderModified",
            Event::PositionOpened(_) => "PositionOpened",
            Event::PositionChanged(_) => "PositionChanged",
            Event::PositionClosed(_) => "PositionClosed",
            Event::AccountState(_) => "AccountState",
            Event::Funding(_) => "Funding",
            Event::PositionLiquidated(_) => "PositionLiquidated",
            Event::OrderCancelRejected(_) => "OrderCancelRejected",
            Event::OrderModifyRejected(_) => "OrderModifyRejected",
        };
        seen.insert(tag);
    }
    assert_eq!(seen.len(), Event::TABLE.len(), "a variant the strategy never produces: {seen:?}");
    assert_eq!(seen.len(), 20);
}

/// What the CORE'S lane does to an account-wide payload on the way in: the mount's `routed("binance")`
/// sender stamps `route_key` on the three payloads that carry no client order id (`AccountState`,
/// `FundingEvent`, `PositionLiquidated`) unless the key equals the payload's own venue or the producer
/// already named an account (`EventSender::routed`). It is the existing mount behaviour, written out here
/// so the comparison below is against what the core must receive, not against what the daemon sent.
fn as_the_core_receives_it(mut sent: Event) -> Event {
    let key = "binance";
    match &mut sent {
        Event::AccountState(a) if a.route_key.is_none() && a.venue.as_str() != key => {
            a.route_key = Some(key.into());
        }
        Event::Funding(f) if f.route_key.is_none() && f.venue.as_str() != key => {
            f.route_key = Some(key.into());
        }
        Event::PositionLiquidated(p) if p.route_key.is_none() && p.venue.as_str() != key => {
            p.route_key = Some(key.into());
        }
        _ => {}
    }
    sent
}

fn check_same(sent: &Event, got: &Event) -> Result<(), TestCaseError> {
    let (a, b) = (
        serde_json::to_string(sent).expect("encodes"),
        serde_json::to_string(got).expect("encodes"),
    );
    prop_assert_eq!(&a, &b, "the serialized text moved across the hop");
    prop_assert_eq!(sent, got);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// The frame codec alone: an event in a `TradeDown::Event` frame decodes to the same bits.
    #[test]
    fn an_event_frame_decodes_to_the_same_bits(sent in event(), seq in any::<u64>(), epoch in any::<u64>(), ts in any::<i64>()) {
        let frame = TradeDown::Event {
            account: "binance".into(),
            gw_epoch: epoch,
            seq,
            ts_ms: ts,
            replayed: false,
            event: sent.clone(),
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &frame).expect("encodes");
        let body = read_frame_raw(&mut std::io::Cursor::new(wire)).expect("a frame reads back");
        let TradeDown::Event { gw_epoch, seq: got_seq, ts_ms, event: got, .. } =
            serde_json::from_slice(&body).expect("decodes")
        else { panic!("an Event frame decodes to an Event") };
        prop_assert_eq!((gw_epoch, got_seq, ts_ms), (epoch, seq, ts));
        check_same(&sent, &got)?;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// The whole hop: events pushed through the daemon's ring reach the core's lane equal, in order,
    /// once, each with the position the daemon assigned and the account's route key.
    #[test]
    fn a_burst_through_the_ring_reaches_the_core_bit_exact_and_in_order(
        events in proptest::collection::vec(event(), 1..8)
    ) {
        let peer = TradePeer::start(TradePeerConfig::new());
        let (_session, mut lane) = open(&peer, "tradehub-1");
        for e in &events {
            peer.push_event(e.clone());
        }
        let got = lane.collect_until(Duration::from_secs(5), events.len(), |i| {
            matches!(i, Ingest::GatewayEvent(_))
        });
        prop_assert_eq!(got.len(), events.len());
        for (i, (sent, ingest)) in events.iter().zip(&got).enumerate() {
            let Ingest::GatewayEvent(g) = ingest else { panic!("only ring events were collected") };
            prop_assert_eq!(g.cursor.seq, i as u64 + 1);
            prop_assert_eq!(g.cursor.gw_epoch, peer.epoch());
            prop_assert_eq!(g.route_key.as_str(), "binance");
            check_same(&as_the_core_receives_it(sent.clone()), &g.event)?;
        }
    }
}
