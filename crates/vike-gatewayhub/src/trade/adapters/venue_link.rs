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
//! Every bridge answers in that one reading itself, so a report only reads the bridge's own cell ([`report_of`]): the
//! words, the counters and the freshness stay the bridge's. The two readers below name the two shared cells, the
//! user-data pump's ([`pump`]) and the one a bridge with a session of its own writes ([`cell`]).

use std::sync::Arc;

use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkState};
use vike_bridge_core::websocket_session::user::stream_link::VenueLinkStream;
use vike_gatewayhub_client::proto::VenueLinkWire;

use crate::trade::lane::VenueLinkReport;

/// THE translator: one reading in the wire's words, none of it a value. Not known is no report.
fn venue_link_wire(state: VenueLinkState) -> Option<VenueLinkWire> {
    match state {
        VenueLinkState::Unknown => None,
        VenueLinkState::Up => Some(VenueLinkWire::working()),
        VenueLinkState::Down(VenueLinkDown { why, effect, failed_restarts, retry_in }) => {
            Some(VenueLinkWire {
                up: false,
                why,
                failed_restarts,
                retry_in_ms: u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX),
                effect: effect.to_string(),
            })
        }
    }
}

/// The report of a path whose bridge answers `state`: read on every status request.
pub(crate) fn report_of(
    state: impl Fn() -> VenueLinkState + Send + Sync + 'static,
) -> VenueLinkReport {
    Arc::new(move || venue_link_wire(state()))
}

/// **The private stream of a venue on the shared user-data pump**, by the route key its pumps write, in the pump's own
/// reading: an account with no pump yet has said nothing, so it is not known (ruled U-6, as for every venue).
pub(crate) fn pump(link: VenueLinkStream) -> VenueLinkReport {
    report_of(move || link.venue_link_state())
}

/// **The cell a bridge with a session of its own writes** (`vike_bridge_core::venue_link::venue_link`, by the route
/// key): OANDA's transactions stream, fxcm's ForexConnect session and ibkr's exec connection, each in its bridge's words.
pub(crate) fn cell(link: VenueLink) -> VenueLinkReport {
    report_of(move || link.state())
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
