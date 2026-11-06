//! `oanda_link` — what the status verb says about an account's transactions stream (`TradeLane::venue_link`).
//!
//! The bridge reopens that stream by itself and the lease stays up through its loss, so the daemon rebuilds nothing:
//! this module only READS the lane's own word (`vike_oanda::StreamLink`) and states it in the wire's, so
//! `TradeStatusReport::venue_link` can tell the tradehub's doctor that the engine it calls RUNNING has no stream of
//! resting fills and venue-side cancels right now, and why. It changes no order's fate: orders go out on REST whatever
//! this says, which is why the report carries the bridge's own `effect` words.

use std::sync::Arc;

use vike_gatewayhub_client::proto::WireVenueLink;
use vike_oanda::{StreamDown, StreamLink};

use crate::trade::lane::VenueLinkReport;

/// The report of the stream whose lane states itself on `link`: read on every status request.
pub(crate) fn report_of(link: StreamLink) -> VenueLinkReport {
    Arc::new(move || link.down().as_ref().map_or_else(WireVenueLink::working, link_of))
}

/// One outage in the wire's words, none of it a value.
fn link_of(down: &StreamDown) -> WireVenueLink {
    WireVenueLink {
        up: false,
        why: down.why.clone(),
        failed_restarts: down.failed_opens,
        retry_in_ms: u64::try_from(down.retry_in.as_millis()).unwrap_or(u64::MAX),
        effect: down.effect.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that has reported nothing is up; the words of an outage reach the wire whole; the report reads the cell
    /// as it is at each ask, so the return reads up.
    #[test]
    fn the_report_states_the_lanes_own_word_at_each_ask() {
        let link = StreamLink::default();
        let report = report_of(link.clone());
        assert_eq!(report(), WireVenueLink::working());
        link.set(Some(StreamDown {
            why: "HTTP 401 on stream open".to_string(),
            effect: "orders still go out",
            failed_opens: 4,
            retry_in: std::time::Duration::from_secs(30),
        }));
        assert_eq!(
            report(),
            WireVenueLink {
                up: false,
                why: "HTTP 401 on stream open".to_string(),
                failed_restarts: 4,
                retry_in_ms: 30_000,
                effect: "orders still go out".to_string(),
            }
        );
        link.set(None);
        assert_eq!(report(), WireVenueLink::working());
    }
}
