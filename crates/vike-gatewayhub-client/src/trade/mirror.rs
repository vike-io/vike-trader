//! What the lease reads off the `vike-model` vocabulary it carries: which `Event` resolves, ends or wraps
//! an order (each table EXHAUSTIVE over `Event`, with no `_` arm, so a new variant fails to COMPILE here
//! until the lease decides what it means), and the `AdoptRestored` teaching of the restored orders.
//!
//! The wire carries the model's own types where they are exact twins (`EventRingCursor`,
//! `OrderCancelIntent`, `FeeSchedule`), so nothing here converts them; their variants are pinned by the
//! fixture gate instead (`crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `WIRE_ENUMS`). The
//! daemon (`vike-gatewayhub`) uses the public helpers below in the other direction.

use vike_execution::RestoredOrder;
use vike_model::events::Event;

use crate::proto::TradeUp;

/// The order client id an event RESOLVES, if it resolves one: the venue spoke about the order itself
/// (it rests, it filled, it was refused, it ended), so a submit the client held as UNKNOWN is UNKNOWN no
/// more. **EXHAUSTIVE over `Event`**: a new variant must be classified here (a `_ =>` would let an order
/// stay "unknown" for ever after its terminal event, or clear it on a mere advisory).
///
/// Four events name an order client id and RESOLVE NOTHING, and each is `None` on purpose: `OrderCancelRejected` and
/// `OrderModifyRejected` are advisories about a COMMAND ("unknown order" is exactly what a venue says to a
/// cancel of a submit it never saw), `OrderSubmitted` is the bridge's own "handed to the venue" mark, not
/// the venue's verdict, and an `OrderAccepted` that names NO venue order is an OPTIMISTIC answer to a lookup
/// that failed (`crates/bridges/ig/src/execution/mod.rs`'s `unresolved_confirm`): the fold refuses it
/// from `Initialized` (`OrderRecord::apply_over_lost_prefix` opens only for an accept that names one), so
/// the core still holds the order without its prefix and the next fill or cancel needs the client to state it
/// (W2G-4). Clearing the mark on any of them made the order vanish from `unknown_orders()`, and from the
/// tradehub's re-ask of `ResolveOrders`, while it was neither terminal nor seen (W19-D2).
pub fn resolved_order_client_id(event: &Event) -> Option<&str> {
    match event {
        Event::Fill(e) => Some(&e.order_client_id),
        Event::OrderAccepted(e) if e.venue_order_id.is_some() => Some(&e.order_client_id),
        Event::OrderRejected(e) => Some(&e.order_client_id),
        Event::OrderDenied(e) => Some(&e.order_client_id),
        Event::OrderTriggered(e) => Some(&e.order_client_id),
        Event::OrderPartiallyFilled(e) => Some(&e.order_client_id),
        Event::OrderFilled(e) => Some(&e.order_client_id),
        Event::OrderCanceled(e) => Some(&e.order_client_id),
        Event::OrderExpired(e) => Some(&e.order_client_id),
        Event::OrderLiquidated(e) => Some(&e.order_client_id),
        Event::OrderModified(e) => Some(&e.order_client_id),
        Event::OrderAccepted(_)
        | Event::OrderSubmitted(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::Funding(_)
        | Event::PositionLiquidated(_)
        // a fee restatement says nothing new about the order's state
        | Event::FeeAdjustment(_) => None,
    }
}

/// The order client id an event ENDS for this lease (W2G-6): [`Event::ended_order`], and a liquidation
/// too, because an order forced closed has nothing left to resolve (`Shared::mark_unknown` skips it).
pub(crate) fn ended_order_client_id(event: &Event) -> Option<&str> {
    if let Event::OrderLiquidated(e) = event {
        return Some(&e.order_client_id);
    }
    event.ended_order()
}

/// The fill a FILLED or PARTIALLY-FILLED wrap embeds, for the order client id it names (W2G-2). The dual-publish
/// contract sends the same `FillEvent` as a bare `Fill` too; a lookup states the wrap alone.
pub(crate) fn wrapped_fill(event: &Event) -> Option<&vike_model::events::FillEvent> {
    if let Event::OrderFilled(e) = event {
        Some(&e.fill)
    } else if let Event::OrderPartiallyFilled(e) = event {
        Some(&e.fill)
    } else {
        None
    }
}

/// Whether `event` is the bridge's SYNTHETIC answer to a cancel: exactly what
/// `vike_bridge_core::exec_actor::cancel_event(order client id, ExecutionClientActorCancelOutcome::Canceled)` makes, a terminal
/// `OrderCanceled` with no venue time and no reason (W2G-1). A venue's own cancel carries its time; the two
/// that do not (polymarket's `CANCELLATION`, ibkr's cancelled status) are told apart by the caller, which asks
/// whether it SENT a cancel for the order client id.
pub(crate) fn synthetic_cancel_order_client_id(event: &Event) -> Option<&str> {
    if let Event::OrderCanceled(e) = event
        && e.timestamp == 0
        && e.reason.is_empty()
    {
        Some(&e.order_client_id)
    } else {
        None
    }
}

/// The timestamp of an event that proves the venue holds (or held) the order PAST acceptance, for the
/// events the transition table takes only from `Accepted` or later (W2F-D1); `None` for every other event.
/// **EXHAUSTIVE over `Event`**, like [`resolved_order_client_id`].
///
/// This is the question "does the core need the order's `OrderSubmitted` and `OrderAccepted` before it can
/// apply this?". `OrderAccepted` itself is not here (a venue-named one opens the fold's lost-prefix door),
/// and neither are `OrderRejected` and `OrderDenied` (the table takes them from `Initialized`, and an
/// implied accept would make them illegal).
pub(crate) fn implied_prefix_timestamp(event: &Event) -> Option<i64> {
    match event {
        Event::Fill(e) => Some(e.timestamp),
        Event::OrderTriggered(e) => Some(e.timestamp),
        Event::OrderPartiallyFilled(e) => Some(e.timestamp),
        Event::OrderFilled(e) => Some(e.timestamp),
        Event::OrderCanceled(e) => Some(e.timestamp),
        Event::OrderExpired(e) => Some(e.timestamp),
        Event::OrderLiquidated(e) => Some(e.timestamp),
        Event::OrderModified(e) => Some(e.timestamp),
        Event::OrderSubmitted(_)
        | Event::OrderAccepted(_)
        | Event::OrderRejected(_)
        | Event::OrderDenied(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::Funding(_)
        | Event::PositionLiquidated(_)
        // not a statement about the order's lifecycle: the engine books it by trade id
        | Event::FeeAdjustment(_) => None,
    }
}

/// The `AdoptRestored` frame that teaches `orders` (its `command_sequence` is the link's to stamp): the pairs, and the venue
/// order id of each order that has one, keyed by its order client id.
pub(crate) fn adopt_restored_frame(orders: &[RestoredOrder]) -> TradeUp {
    TradeUp::AdoptRestored {
        command_sequence: 0,
        pairs: orders.iter().map(|o| (o.order_client_id.clone(), o.symbol.clone())).collect(),
        venue_ids: orders
            .iter()
            .filter_map(|o| Some((o.order_client_id.clone(), o.venue_order_id.clone()?)))
            .collect(),
    }
}

/// The orders an `AdoptRestored` frame teaches, as the daemon hands them to its client: each pair with the venue
/// order id `venue_ids` names for its order client id (the LAST one, a later teaching being the fresher), `None` for an order client id
/// it does not name (an older client's frame names none). A venue id for an order client id no pair names teaches nothing.
pub fn restored_orders(
    pairs: Vec<(String, String)>,
    venue_ids: Vec<(String, String)>,
) -> Vec<RestoredOrder> {
    let ids: std::collections::HashMap<String, String> = venue_ids.into_iter().collect();
    pairs
        .into_iter()
        .map(|(order_client_id, symbol)| {
            let venue_order_id = ids.get(&order_client_id).cloned();
            RestoredOrder { order_client_id, symbol, venue_order_id }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The teaching crosses the wire with and without venue ids: an order the node holds an id for gets it back,
    /// one it holds none for stays `None`, and a frame with no id at all keeps the bytes it had before the key.
    #[test]
    fn a_restored_order_round_trips_with_and_without_its_venue_id() {
        let with = RestoredOrder {
            venue_order_id: Some("v-1".into()),
            ..RestoredOrder::new("c1", "EURUSD")
        };
        let without = RestoredOrder::new("c2", "GBPUSD");
        let orders = vec![with, without.clone()];
        let json = serde_json::to_string(&adopt_restored_frame(&orders)).expect("encodes");
        let TradeUp::AdoptRestored { pairs, venue_ids, .. } =
            serde_json::from_str(&json).expect("decodes")
        else {
            panic!("{json}")
        };
        assert_eq!(restored_orders(pairs, venue_ids), orders, "{json}");

        let plain = serde_json::to_string(&adopt_restored_frame(std::slice::from_ref(&without)))
            .expect("encodes");
        assert_eq!(
            plain, r#"{"AdoptRestored":{"command_sequence":0,"pairs":[["c2","GBPUSD"]]}}"#,
            "no key, same bytes"
        );
        // An older client's frame (no key) decodes, and teaches no venue id.
        let TradeUp::AdoptRestored { pairs, venue_ids, .. } =
            serde_json::from_str(&plain).expect("decodes")
        else {
            panic!("{plain}")
        };
        assert_eq!(restored_orders(pairs, venue_ids), vec![without]);
    }

    /// A merged frame can name an order client id twice: the later id wins, and an id for an order client id no pair names is dropped.
    #[test]
    fn the_last_venue_id_of_an_order_client_id_wins_and_a_stray_one_teaches_nothing() {
        let pairs = vec![("c1".to_string(), "BTC".to_string())];
        let ids = vec![
            ("c1".into(), "old".into()),
            ("stray".into(), "x".into()),
            ("c1".into(), "new".into()),
        ];
        let taught = restored_orders(pairs, ids);
        assert_eq!(
            taught,
            vec![RestoredOrder {
                venue_order_id: Some("new".into()),
                ..RestoredOrder::new("c1", "BTC")
            }]
        );
    }
}
