//! The conversions between the wire's mirror types and the `vike-exec` / `vike-model` originals they
//! stand for.
//!
//! Every one is a total, EXHAUSTIVE `match` with no `_` arm, so a variant added to the original
//! (`CancelIntent`, `FeeSchedule`, `Event`) fails to COMPILE here until the wire decides what to call
//! it. That is the whole reason these types are mirrors rather than the originals: the originals have
//! no serde, or live in a crate this one must keep off its wire.
//!
//! The daemon (`vike-gatewayhub`) uses the same conversions in the other direction, which is why they
//! are `From` impls on the wire types and not private helpers.

use vike_exec::{CancelIntent, GatewayCursor};
use vike_model::FeeSchedule;
use vike_model::events::Event;

use crate::proto::{WireCancelIntent, WireCursor, WireFeeSchedule};

impl From<CancelIntent> for WireCancelIntent {
    fn from(intent: CancelIntent) -> Self {
        match intent {
            CancelIntent::Unspecified => WireCancelIntent::Unspecified,
            CancelIntent::Routine => WireCancelIntent::Routine,
            CancelIntent::RiskOff => WireCancelIntent::RiskOff,
        }
    }
}

impl From<WireCancelIntent> for CancelIntent {
    fn from(intent: WireCancelIntent) -> Self {
        match intent {
            WireCancelIntent::Unspecified => CancelIntent::Unspecified,
            WireCancelIntent::Routine => CancelIntent::Routine,
            WireCancelIntent::RiskOff => CancelIntent::RiskOff,
        }
    }
}

impl From<FeeSchedule> for WireFeeSchedule {
    fn from(fee: FeeSchedule) -> Self {
        match fee {
            FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
                WireFeeSchedule::PercentMakerTaker { maker_bps, taker_bps }
            }
            FeeSchedule::PerShareWithFloor { per_share, min, max_pct } => {
                WireFeeSchedule::PerShareWithFloor { per_share, min, max_pct }
            }
            FeeSchedule::PercentOfUnderlying { bps, premium_cap_pct } => {
                WireFeeSchedule::PercentOfUnderlying { bps, premium_cap_pct }
            }
            FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share } => {
                WireFeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share }
            }
            FeeSchedule::Free => WireFeeSchedule::Free,
        }
    }
}

impl From<WireFeeSchedule> for FeeSchedule {
    fn from(fee: WireFeeSchedule) -> Self {
        match fee {
            WireFeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
                FeeSchedule::PercentMakerTaker { maker_bps, taker_bps }
            }
            WireFeeSchedule::PerShareWithFloor { per_share, min, max_pct } => {
                FeeSchedule::PerShareWithFloor { per_share, min, max_pct }
            }
            WireFeeSchedule::PercentOfUnderlying { bps, premium_cap_pct } => {
                FeeSchedule::PercentOfUnderlying { bps, premium_cap_pct }
            }
            WireFeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share } => {
                FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share }
            }
            WireFeeSchedule::Free => FeeSchedule::Free,
        }
    }
}

impl From<GatewayCursor> for WireCursor {
    fn from(c: GatewayCursor) -> Self {
        WireCursor { gw_epoch: c.gw_epoch, seq: c.seq, ts_ms: c.ts_ms }
    }
}

impl From<WireCursor> for GatewayCursor {
    fn from(c: WireCursor) -> Self {
        GatewayCursor { gw_epoch: c.gw_epoch, seq: c.seq, ts_ms: c.ts_ms }
    }
}

/// The client order id an event RESOLVES, if it resolves one: the venue spoke about the order itself
/// (it rests, it filled, it was refused, it ended), so a submit the client held as UNKNOWN is UNKNOWN no
/// more. **EXHAUSTIVE over `Event`**: a new variant must be classified here (a `_ =>` would let an order
/// stay "unknown" for ever after its terminal event, or clear it on a mere advisory).
///
/// Four events name a coid and RESOLVE NOTHING, and each is `None` on purpose: `OrderCancelRejected` and
/// `OrderModifyRejected` are advisories about a COMMAND ("unknown order" is exactly what a venue says to a
/// cancel of a submit it never saw), `OrderSubmitted` is the bridge's own "handed to the venue" mark, not
/// the venue's verdict, and an `OrderAccepted` that names NO venue order is the resolver's OPTIMISTIC answer
/// to a lookup that failed (`vike_bridge_core::resolve_ambiguous_submit`'s `Err` arm): the fold refuses it
/// from `Initialized` (`ManagedOrder::apply_over_lost_prefix` opens only for an accept that names one), so
/// the core still holds the order without its prefix and the next fill or cancel needs the client to state it
/// (W2G-4). Clearing the mark on any of them made the order vanish from `unknown_orders()`, and from the
/// tradehub's re-ask of `ResolveOrders`, while it was neither terminal nor seen (W19-D2).
pub fn resolved_coid(event: &Event) -> Option<&str> {
    match event {
        Event::Fill(e) => Some(&e.client_order_id),
        Event::OrderAccepted(e) if e.venue_order_id.is_some() => Some(&e.client_order_id),
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderDenied(e) => Some(&e.client_order_id),
        Event::OrderTriggered(e) => Some(&e.client_order_id),
        Event::OrderPartiallyFilled(e) => Some(&e.client_order_id),
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderLiquidated(e) => Some(&e.client_order_id),
        Event::OrderModified(e) => Some(&e.client_order_id),
        Event::OrderAccepted(_)
        | Event::OrderSubmitted(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::Funding(_)
        | Event::PositionLiquidated(_) => None,
    }
}

/// The client order id an event ENDS, if it ends one: the order is terminal in the core once it folds
/// (W2G-6). **EXHAUSTIVE over `Event`**, like [`resolved_coid`]. `OrderPartiallyFilled` is not here (the
/// order still rests), nor is an advisory.
pub(crate) fn ended_coid(event: &Event) -> Option<&str> {
    match event {
        Event::OrderFilled(e) => Some(&e.client_order_id),
        Event::OrderRejected(e) => Some(&e.client_order_id),
        Event::OrderDenied(e) => Some(&e.client_order_id),
        Event::OrderCanceled(e) => Some(&e.client_order_id),
        Event::OrderExpired(e) => Some(&e.client_order_id),
        Event::OrderLiquidated(e) => Some(&e.client_order_id),
        Event::Fill(_)
        | Event::OrderSubmitted(_)
        | Event::OrderAccepted(_)
        | Event::OrderTriggered(_)
        | Event::OrderPartiallyFilled(_)
        | Event::OrderModified(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::Funding(_)
        | Event::PositionLiquidated(_) => None,
    }
}

/// The fill a FILLED or PARTIALLY-FILLED wrap embeds, for the coid it names (W2G-2). The dual-publish
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
/// `vike_bridge_core::exec_actor::cancel_event(coid, CancelOutcome::Canceled)` makes, a terminal
/// `OrderCanceled` with no venue time and no reason (W2G-1). A venue's own cancel carries its time; the two
/// that do not (polymarket's `CANCELLATION`, ibkr's cancelled status) are told apart by the caller, which asks
/// whether it SENT a cancel for the coid.
pub(crate) fn synthetic_cancel_coid(event: &Event) -> Option<&str> {
    if let Event::OrderCanceled(e) = event
        && e.ts == 0
        && e.reason.is_empty()
    {
        Some(&e.client_order_id)
    } else {
        None
    }
}

/// The timestamp of an event that proves the venue holds (or held) the order PAST acceptance, for the
/// events the transition table takes only from `Accepted` or later (W2F-D1); `None` for every other event.
/// **EXHAUSTIVE over `Event`**, like [`resolved_coid`].
///
/// This is the question "does the core need the order's `OrderSubmitted` and `OrderAccepted` before it can
/// apply this?". `OrderAccepted` itself is not here (a venue-named one opens the fold's lost-prefix door),
/// and neither are `OrderRejected` and `OrderDenied` (the table takes them from `Initialized`, and an
/// implied accept would make them illegal).
pub(crate) fn implied_prefix_ts(event: &Event) -> Option<i64> {
    match event {
        Event::Fill(e) => Some(e.ts),
        Event::OrderTriggered(e) => Some(e.ts),
        Event::OrderPartiallyFilled(e) => Some(e.ts),
        Event::OrderFilled(e) => Some(e.ts),
        Event::OrderCanceled(e) => Some(e.ts),
        Event::OrderExpired(e) => Some(e.ts),
        Event::OrderLiquidated(e) => Some(e.ts),
        Event::OrderModified(e) => Some(e.ts),
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
        | Event::PositionLiquidated(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cancel_intent_mirror_is_a_bijection() {
        for intent in [CancelIntent::Unspecified, CancelIntent::Routine, CancelIntent::RiskOff] {
            assert_eq!(CancelIntent::from(WireCancelIntent::from(intent)), intent);
        }
        assert_eq!(WireCancelIntent::default(), WireCancelIntent::Unspecified);
        assert_eq!(CancelIntent::default(), CancelIntent::from(WireCancelIntent::default()));
    }

    #[test]
    fn the_fee_schedule_mirror_is_a_bijection_bit_for_bit() {
        let all = [
            FeeSchedule::PercentMakerTaker { maker_bps: 1.0 / 3.0, taker_bps: 2.5 },
            FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.5 },
            FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 12.5 },
            FeeSchedule::ProbabilityScaled {
                taker_rate: 0.07,
                maker_rate: 0.0,
                maker_rebate_share: 0.2,
            },
            FeeSchedule::Free,
        ];
        for fee in all {
            let wire = WireFeeSchedule::from(fee);
            let json = serde_json::to_string(&wire).expect("encodes");
            let back: WireFeeSchedule = serde_json::from_str(&json).expect("decodes");
            assert_eq!(FeeSchedule::from(back), fee, "{json}");
        }
    }

    #[test]
    fn the_cursor_mirror_keeps_all_three_numbers() {
        let c = GatewayCursor { gw_epoch: u64::MAX - 3, seq: 17, ts_ms: -5 };
        assert_eq!(GatewayCursor::from(WireCursor::from(c)), c);
    }
}
