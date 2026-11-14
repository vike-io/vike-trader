//! **The cpapi LIVE order hold** — stage A of
//! `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`: a `(cpapi, live)` mount
//! binds the real account, serves its data and reconciles it, and its exec client REFUSES every
//! order, because the Client Portal stream has no observed live fill path
//! (`crates/bridges/vike-ibkr/src/transport/cpapi/decode.rs`'s `decode_websocket_frame` decodes only
//! `sor`), so a real order's fill could go unseen and a position build up nobody sees.
//!
//! The hold is the ONE shared order hold, `vike_bridge_core::execution_client::orders_held::ExecutionClientOrdersHeld`,
//! wrapped around the connected client by [`held`] in this venue's words ([`CPAPI_LIVE_HELD_REASON`]). Each submit
//! gets ONE `error!` and a synthesized terminal `OrderRejected` and never reaches the gateway (no order may silently
//! vanish; a batch is the trait's default fan-out onto that `submit`, held by `tests/held_and_reconcile.rs`); a modify,
//! and a trigger amend (`modify_trigger`), gets a non-terminal `OrderModifyRejected` in the hold's words; every
//! cancel door, `confirm` and the lifecycle are delegated, so a hold never traps an operator in an order. The
//! gatewayhub holds the same arm in front of this one (its fold of `ArmingHeldBelowLive::OrdersHeld`); this is the bridge
//! doing what its `resolve` said, for a caller that holds the client without that fold.
//!
//! **What lifts it** is code backed by evidence, never a setting: stage B, the change that adds the
//! cpapi fill decoder written from frames CAPTURED by the owner-attended live smoke, with its
//! offline fixture. Until then [`super::VenueAdapterIbkr`] answers
//! `ArmingHeldBelowLive::OrdersHeld` for every cpapi LIVE arm.

use vike_bridge_core::execution_client::orders_held::ExecutionClientOrdersHeld;
use vike_execution::{EventSender, ExecutionClient};

use crate::reconcile::cpapi::VENUE;

/// The reason a held order is rejected with: what happened, why, and that nothing was sent.
pub(super) const CPAPI_LIVE_HELD_REASON: &str = "ibkr: cpapi LIVE orders are held — the Client \
     Portal stream has no observed live fill path, so a real order's fill could go unseen; \
     nothing was sent (decision 0140, stage A)";

/// The connected cpapi LIVE client `inner`, for the starting `account` (a label, never a credential), with every
/// order held; `events` is the lane its refusals are said on.
pub(super) fn held(
    inner: Box<dyn ExecutionClient + Send>,
    events: EventSender,
    account: String,
) -> ExecutionClientOrdersHeld {
    ExecutionClientOrdersHeld::new(inner, events, VENUE, account, CPAPI_LIVE_HELD_REASON)
}
