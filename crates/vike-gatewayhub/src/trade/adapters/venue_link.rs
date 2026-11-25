//! `venue_link` — what the status verb says about an account's path to its venue (`TradeLane::venue_link`): ONE
//! translator, [`venue_link_wire`], from the reading every bridge answers in
//! ([`vike_bridge_core::venue_link::VenueLinkState`]) into the wire's [`VenueLinkWire`].
//!
//! Every path reported here heals itself while the lease stays up (the shared pump reopens a private stream, the
//! Dukascopy client restarts its sidecar, OANDA's lane reopens its transactions stream, the cTrader actor opens a new
//! account session on the socket that stayed up, the fxcm, ig and ibkr bridges log in again), so the daemon rebuilds
//! nothing: a report only READS the bridge's cell, on every status request, so `TradeStatusReport::venue_link` can tell
//! `trade health` that the engine it calls RUNNING has no path right now, why, and what that costs the orders in the
//! bridge's own `effect` words (an order may be waiting to go out, so a blanket "refused" would have an operator place
//! it twice). It changes no order's fate and keeps no copy. A reading the bridge cannot vouch for
//! ([`VenueLinkState::Unknown`]: nothing heard yet, a stale word) is no report: the account reads not known, never up.
//!
//! Every bridge answers in that one reading itself, in the ONE cell of the account
//! (`vike_bridge_core::venue_link::venue_link`, by the route key): a path that is several paths (the pump's streams, ig's
//! order session and trade stream, cTrader's per-account sessions) is a named sub-path of it and the cell reads the
//! worst, so a report only reads that cell ([`cell`]): the words, the counters and the freshness stay the bridge's.

use std::sync::Arc;

use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkState};
use vike_gatewayhub_client::proto::VenueLinkWire;
use vike_model::feed_status::DataClientConnectionState;

use crate::trade::lane::VenueLinkReport;

/// THE translator: one reading in the wire's words, none of it a value. Its verdict is the reading's state by the one
/// order-path mapping (`VenueLinkState::connection_state`): not known is no report, `Connected` a working path, and a
/// reopen (`Connecting`) or a failure (`Error`) a path that is down, with the facts (`up`, `failed_restarts`) the tradehub
/// reads the same state back from.
fn venue_link_wire(state: VenueLinkState) -> Option<VenueLinkWire> {
    match (state.connection_state(), state) {
        (DataClientConnectionState::Unknown, _) => None,
        (_, VenueLinkState::Down(VenueLinkDown { why, effect, failed_restarts, retry_in })) => {
            Some(VenueLinkWire {
                up: false,
                why,
                failed_restarts,
                retry_in_ms: u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX),
                effect: effect.to_string(),
            })
        }
        _ => Some(VenueLinkWire::working()),
    }
}

/// **The report of the account's cell**, read on every status request: a cell no writer has spoken in yet is not known
/// (ruled U-6, as for every venue), so it is no report.
pub(crate) fn cell(link: VenueLink) -> VenueLinkReport {
    Arc::new(move || venue_link_wire(link.state()))
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
