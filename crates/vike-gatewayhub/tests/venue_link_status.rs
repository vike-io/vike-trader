#![cfg(feature = "trade")]
//! **A venue path the bridge heals by itself is STATED by the status verb** (`TradeLane::venue_link`,
//! `TradeStatusReport::venue_link`), over a real loopback gatewayhub and the client crate's real `TradeSession`.
//!
//! What it pins: a lane that reports a path makes the status of a RUNNING adapter say up, then down with the
//! bridge's own words, then up again, each read live; a lane that reports none (every other venue, the trait's
//! default) leaves `venue_link` empty; and the report changes nothing else (the adapter stays up and no lease is
//! ended: this is a reading, not the IBKR link supervision of `tests/ibkr_reconnect.rs`).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::gateway::TradeKit;
use vike_gatewayhub::testkit::trade::ScriptedTrade;
use vike_gatewayhub::trade::lane::VenueLinkReport;
use vike_gatewayhub_client::proto::{AdapterReply, WireVenueLink};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/trade_rig.rs"]
mod trade_rig;

use trade_rig::{open, rig_with, start_args, wait_until};

const WAIT: Duration = Duration::from_secs(8);

fn down(why: &str) -> WireVenueLink {
    WireVenueLink { up: false, why: why.to_string(), failed_restarts: 1, retry_in_ms: 10_000 }
}

#[test]
fn a_reported_venue_path_is_stated_live_and_the_adapter_stays_up() {
    let now = Arc::new(Mutex::new(WireVenueLink::working()));
    let read = Arc::clone(&now);
    let report: VenueLinkReport =
        Arc::new(move || read.lock().unwrap_or_else(PoisonError::into_inner).clone());
    let lane = ScriptedTrade::new(&["binance"]).with_venue_link("binance", report);
    let rig = rig_with(lane, ServerConfig::PRODUCTION, TradeKit::default());
    let (session, _core) = open(&rig, "tradehub-1");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Live { .. }
    ));

    let status = session.status().expect("status");
    assert_eq!(status.venue_link, Some(WireVenueLink::working()), "up is stated, not left blank");

    *now.lock().unwrap_or_else(PoisonError::into_inner) = down("JForex session disconnected");
    let status = session.status().expect("status");
    assert_eq!(status.venue_link, Some(down("JForex session disconnected")));
    assert!(status.adapter_up, "a reading changes nothing: the adapter is still up");

    *now.lock().unwrap_or_else(PoisonError::into_inner) = WireVenueLink::working();
    assert!(
        wait_until(WAIT, || {
            session.status().is_ok_and(|s| s.venue_link == Some(WireVenueLink::working()))
        }),
        "the report follows the bridge back up"
    );
}

#[test]
fn a_venue_that_reports_no_path_leaves_the_field_empty() {
    let rig =
        rig_with(ScriptedTrade::new(&["binance"]), ServerConfig::PRODUCTION, TradeKit::default());
    let (session, _core) = open(&rig, "tradehub-1");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Live { .. }
    ));
    let status = session.status().expect("status");
    assert_eq!(
        status.venue_link, None,
        "no report is not 'up': a reader asks no more of this lease"
    );
    assert!(status.adapter_up);
}
