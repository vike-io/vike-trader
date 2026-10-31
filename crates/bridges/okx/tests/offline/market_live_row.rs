//! A MARKET order's `live` row (offline, over rows CAPTURED on the OKX demo, 2026-10-28:
//! `tests/fixtures/orders_channel/`, the account id redacted by the capture sanitizer).
//!
//! The bridge's comments used to say the `orders` channel pushes no `live` row for an order that never
//! rests, so a market order's fill could be the first thing the stream carries for it. The venue says
//! otherwise: a market order is pushed as `state: live` and then as `state: filled`, both inside one
//! millisecond (`b97025411okb`: cTime `1793156052362`, the two frames read 131 ms later at the same
//! millisecond); eight market orders on 2026-10-28 and 2026-10-31 all read `live` first, 132 to 145 ms
//! after their cTime. What that row means for the core, pinned here:
//!
//! * it maps to exactly ONE bare `OrderAccepted` carrying the venue order id, never a fill and never a
//!   second event: it does not read as an order that rests (the fill row behind it ends the order);
//! * the submit fence does NOT hold it (an accept is not a held kind) and DOES hold the fill behind it, so
//!   the lane reads the stream's accept, then the loop's own `OrderSubmitted`/`OrderAccepted`, then the
//!   fill and its wrap. The engine's side of the same order (`A D D A A`, ends `Filled` once) is
//!   `crates/vike-exec/tests/engine/ordinary_accept_replay.rs`'s
//!   `a_streams_bare_accept_then_the_loops_own_answer_then_the_held_fill_ends_filled_once`.

use std::assert_matches;

use serde_json::Value;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_exec::Ingest;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_okx::event_mapper::map_okx_perp;

const LIVE: &str = include_str!("../fixtures/orders_channel/market_buy_live.json");
const FILLED: &str = include_str!("../fixtures/orders_channel/market_buy_filled.json");
const SELL_LIVE: &str =
    include_str!("../fixtures/orders_channel/market_reduce_only_sell_live.json");
const COID: &str = "b97025411okb";
const ORD_ID: &str = "4048464773758496769";

fn events_of(capture: &str) -> Vec<Event> {
    let frame: Value = serde_json::from_str(capture).expect("the capture is JSON");
    // ctVal 0.01: BTC-USDT-SWAP's contract is 0.01 BTC
    map_okx_perp(&frame, "okx", "BTC-USDT-SWAP", 0.01)
}

#[test]
fn a_market_orders_live_row_is_one_bare_accept_and_nothing_else() {
    for (capture, coid, ord_id) in
        [(LIVE, COID, ORD_ID), (SELL_LIVE, "b97025411oks", "4048465298751139841")]
    {
        let frame: Value = serde_json::from_str(capture).expect("the capture is JSON");
        let row = &frame["data"][0];
        assert_eq!(row["ordType"], "market", "the capture is a MARKET order");
        assert_eq!(row["state"], "live", "…and the venue pushed it as `live`");
        assert_eq!(row["fillSz"], "0", "…with nothing filled on that row");
        let evs = events_of(capture);
        assert_matches!(
            evs.as_slice(),
            [Event::OrderAccepted(a)]
                if a.client_order_id == coid && a.venue_order_id.as_deref() == Some(ord_id),
            "{evs:?}"
        );
    }
}

#[test]
fn the_filled_row_behind_it_is_the_fill_and_its_wrap_of_the_same_order() {
    let evs = events_of(FILLED);
    assert_matches!(
        evs.as_slice(),
        [Event::Fill(f), Event::OrderFilled(w)]
            if f.client_order_id == COID && w.client_order_id == COID
                && f.trade_id == "4550131582" && (f.last_qty - 0.0001).abs() < 1e-12,
        "{evs:?}"
    );
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "OrderSubmitted",
            Event::OrderAccepted(_) => "OrderAccepted",
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            _ => "other",
        })
        .collect()
}

/// The venue pushes the `live` row and the `filled` row before the REST answer is read: the lane has the
/// stream's accept at once, the fill waits, and the loop's own pair precedes the fill when it goes.
#[test]
fn the_fence_passes_the_live_row_and_holds_the_fill_behind_it() {
    let (events, mut ingest) = vike_exec::event_channel(16);
    let mut on_lane = move || {
        let mut out = Vec::new();
        while let Ok(Ingest::Event(e)) = ingest.try_recv() {
            out.push(e);
        }
        out
    };
    let fence = SubmitFence::new(events);

    fence.submit(COID, || {
        for event in events_of(LIVE).into_iter().chain(events_of(FILLED)) {
            assert!(fence.deliver(event));
        }
        let early = on_lane();
        assert_matches!(early.as_slice(), [Event::OrderAccepted(_)], "only the live row got out");
        assert_eq!(fence.parked(COID), 2, "the fill and its wrap wait for the order's own answer");
        vec![
            Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.to_string(), ts: 0 }),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: COID.to_string(),
                venue_order_id: Some(ORD_ID.into()),
                ts: 0,
            }),
        ]
    });

    assert_eq!(kinds(&on_lane()), ["OrderSubmitted", "OrderAccepted", "Fill", "OrderFilled"]);
}
