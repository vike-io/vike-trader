//! `venue_routing` — the CLIENT half of the node's routing gate, shared by every surface that writes to
//! a node, and the only defence against the one direction the node cannot cover: a node that predates
//! [`crate::proto::FEATURE_VENUE_ROUTING`] accepts a command naming ANY venue and applies it to its
//! PRIMARY engine — `Ack`, an order in the snapshot, no error — so an operator picking a venue sees
//! success and the order is signed on a different exchange. Against such a node a client sends only a
//! venue the node has been seen to publish ([`may_send_to_backend`]).
//!
//! PURE: the caller holds the two facts (the venues the node publishes an engine for, off its frame,
//! and [`crate::TradehubControlHandle::routes_by_venue`]) and says the refusal in its own channel:
//! the desktop latches it on its status strip (`vike-app-core`'s `send_to_backend`); `vike-cli trade` and
//! `vike-mcp` refuse with its reason when no frame shows the venue (`crates/vike-client/src/order_gate.rs`'s
//! `judge`, rule 4).

use crate::wire::CommandWire;

/// What a client should do with a command it is about to send, given WHICH VENUE that command
/// names and what the connected node has said about itself — the answer to
/// [`venue_routing_verdict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendVenueRoutingVerdict {
    /// Send it. Either this node publishes an engine for that venue, or it advertises
    /// [`crate::proto::FEATURE_VENUE_ROUTING`] and will answer a `Response::Error`
    /// naming its own roster — both of which are honest outcomes.
    Send,
    /// Do NOT send it; show the operator this reason instead. The node would accept the frame and
    /// apply it to a book the operator did not name.
    Refuse(String),
}

/// **May this client send a command addressed to `venue`?** — the client half of the routing fix,
/// and the half that covers the direction the server half cannot.
///
/// # The direction this exists for
///
/// A node that advertises `FEATURE_VENUE_ROUTING` refuses a venue it runs no engine for, so a
/// client talking to one can offer the operator anything and let the node answer. A node that does
/// NOT advertise it takes `vike_core`'s historical `route_of(..).unwrap_or(0)`: it accepts the
/// command, applies it to its PRIMARY engine, answers `Ack`, and shows the order in the snapshot.
/// The operator picks a venue, sees no error, and the order is signed on a different exchange.
/// Nothing on the wire distinguishes that from success, which is why the check has to happen before
/// the frame is written and why it cannot be a check on the reply.
///
/// # The rules, and which way each one leans
///
/// * a BLANK venue is refused — an empty string is not a venue, and the node's routing would fall
///   to engine 0 on every build, advertised or not (this is the same hole the desktop dispatcher's
///   `OrderEntryDispatchRejectReason::NoRoutableMarket` closes for the snapshot's own empty placeholder, restated
///   here because this function is reachable from paths that never pass through that planner);
/// * a venue the node PUBLISHES an engine for is sent — that is positive evidence, and it is the
///   only evidence available against an old node;
/// * otherwise, if the node advertises the capability, it is sent — the node can say no, and it
///   knows more than this client does: `node_venues` comes from a pushed snapshot that lags a
///   runtime `MountStrategy`, so a client that refused here would block a venue the node had since
///   acquired. **This is the one rule that leans toward sending**, and it leans there because the
///   worst case is a clean refusal from the node rather than a misroute;
/// * otherwise it is REFUSED locally, naming both the venue asked for and what the node publishes.
///
/// ⚠ `node_venues` EMPTY plus no capability is the most conservative case and is refused: an
/// observer that has not yet received a frame knows nothing about the node, and "I know nothing"
/// must never read as "anything goes" on a path that signs orders.
pub fn venue_routing_verdict(
    venue: &str,
    node_venues: &[String],
    node_routes_by_venue: bool,
) -> BackendVenueRoutingVerdict {
    let venue = venue.trim();
    if venue.is_empty() {
        return BackendVenueRoutingVerdict::Refuse(
            "this command names no venue, so the backend would apply it to whichever book it \
             happens to run first"
                .to_string(),
        );
    }
    if node_venues.iter().any(|v| v == venue) {
        return BackendVenueRoutingVerdict::Send;
    }
    if node_routes_by_venue {
        return BackendVenueRoutingVerdict::Send;
    }
    let known = if node_venues.is_empty() {
        "it has published none yet".to_string()
    } else {
        format!("it publishes: {}", node_venues.join(", "))
    };
    BackendVenueRoutingVerdict::Refuse(format!(
        "this backend does not check which venue a command names — it would apply an order for \
         `{venue}` to its PRIMARY book without saying so, and {known}. Upgrade the backend, or \
         trade a venue it publishes"
    ))
}

/// **The ROUTING gate a write passes before it reaches a node**: `Ok` to send, and on `Err` the
/// refusal's reason, for the caller to say (the desktop logs it and latches it on the status strip).
///
/// `node_venues` is the node's engine roster as the caller's frame shows it, one entry per published
/// block (EMPTY before the first frame, which [`venue_routing_verdict`] reads as "I know nothing").
///
/// A command that addresses no venue ([`CommandWire::addressed_venue`] answers `None` — a cancel by
/// order client id, the account-wide kill switch, the UNSCOPED panic button) is always sendable: those
/// name no book, so there is nothing to get wrong, and a panic button with a prerequisite is not one.
pub fn may_send_to_backend(
    cmd: &CommandWire,
    node_venues: &[String],
    node_routes_by_venue: bool,
) -> Result<(), String> {
    let Some(venue) = cmd.addressed_venue() else {
        return Ok(());
    };
    match venue_routing_verdict(venue, node_venues, node_routes_by_venue) {
        BackendVenueRoutingVerdict::Send => Ok(()),
        BackendVenueRoutingVerdict::Refuse(why) => Err(why),
    }
}

#[path = "venue_routing_tests.rs"]
#[cfg(test)]
mod venue_routing_tests;
