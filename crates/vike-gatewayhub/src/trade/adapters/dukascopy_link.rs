//! `dukascopy_link` — what the status verb says about the JForex sidecar (`TradeLane::venue_link`).
//!
//! The exec client restarts a dead sidecar by itself (`crates/bridges/dukascopy/CLAUDE.md`'s "The sidecar is
//! restarted by itself"), so the lease stays up through an outage and the daemon rebuilds nothing: this module
//! only READS the client's [`SidecarState`] off the process's price lane (`vike_dukascopy::quotes::process_bus`,
//! the one thing the client and this lane share) and states it in the wire's words, so
//! `TradeStatusReport::venue_link` can tell the tradehub's doctor that the engine it calls RUNNING has no venue
//! path right now, and why. It changes no order's fate: a submit is still refused by the bridge in words.

use std::sync::Arc;

use vike_dukascopy::SidecarState;
use vike_dukascopy::quotes::{QuoteBus, process_bus};
use vike_gatewayhub_client::proto::WireVenueLink;

use crate::trade::lane::VenueLinkReport;

/// The report of the process's one sidecar: read on every status request.
pub(crate) fn process_report() -> VenueLinkReport {
    report_of(process_bus())
}

/// The report of the sidecar whose client owns `bus`.
pub(crate) fn report_of(bus: Arc<QuoteBus>) -> VenueLinkReport {
    Arc::new(move || link_of(&bus.sidecar()))
}

/// One [`SidecarState`] in the wire's words: the sidecar's own `why`, the restarts that failed and the wait
/// before the next, none of it a value.
fn link_of(state: &SidecarState) -> WireVenueLink {
    match state {
        SidecarState::Up => WireVenueLink::working(),
        SidecarState::Down { why, failed_restarts, retry_in } => WireVenueLink {
            up: false,
            why: why.clone(),
            failed_restarts: *failed_restarts,
            retry_in_ms: u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX),
        },
    }
}

#[path = "dukascopy_link_tests.rs"]
#[cfg(test)]
mod dukascopy_link_tests;
