//! `ctrader_link` — what the status verb says about the exec socket's account session (`TradeLane::venue_link`).
//!
//! The socket stays up when the venue ends the account's session, and the actor heals it by itself, so the lease
//! stays up too: this module only READS the actor's word off the grant's cell (`SharedGrant::exec_path_down`) and
//! states it in the wire's words, so `TradeStatusReport::venue_link` can tell the tradehub's doctor that the engine
//! it calls RUNNING is not authorized, why, and what that costs the orders in the bridge's own words: an order may be
//! waiting to go out, so a blanket "refused" would have an operator place it twice (`crates/bridges/ctrader/CLAUDE.md`'s
//! "A lapsed account session"). It changes no order's fate.

use std::sync::Arc;

use vike_ctrader::conn::grant::SharedGrant;
use vike_gatewayhub_client::proto::WireVenueLink;

use crate::trade::lane::VenueLinkReport;

/// The report of the account whose grant `cell` holds: read on every status request.
pub(crate) fn report_of(cell: Arc<SharedGrant>) -> VenueLinkReport {
    Arc::new(move || match cell.exec_path_down() {
        None => WireVenueLink::working(),
        Some(down) => WireVenueLink {
            up: false,
            why: down.why,
            failed_restarts: down.failed_attempts,
            retry_in_ms: down.retry_in_ms,
            effect: down.effect.to_string(),
        },
    })
}

#[path = "ctrader_link_tests.rs"]
#[cfg(test)]
mod ctrader_link_tests;
