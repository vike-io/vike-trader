//! `venues` — the ONLY venue-aware code in the market-data plane: what it SERVES live
//! ([`supported`], the client table's keyless venues) and market data's client builder over the process's feed
//! broker ([`market_builder`]).
//!
//! Everything else under `crate::market_data` is venue-free and testable with no network, which is what lets
//! the §8 item 15 hub suite run on the DEFAULT build over a scripted `DataClient` double.
//!
//! ⚠ **The client ARMS are not here any more.** They moved to
//! `crates/vike-datahub/src/feeds/venues.rs`'s `build_client`, the one function in this process
//! that constructs a venue client, because both planes now take their client from the broker that
//! owns it. What stayed is market data's REFUSAL: a venue this plane does not serve is refused here by
//! name, before the broker is ever asked.
//!
//! # ⚠ The start-narrow set, and the partition it landed on — and what joined it
//!
//! `crates/vike-panels/src/dom.rs`'s `DomVenue` set — binance, bybit, okx, aster, hyperliquid — plus
//! polymarket, whose keyless `feeds` half is the one a market-data plane wants. Read off
//! `crates/vike-venues/src/venue_caps.rs`, that made the two book lanes a strict PARTITION on day
//! one: the five CEX declare `book: false, depth: true` and polymarket declares `book: true,
//! depth: false`. So no venue served both, and every other combination is a `LaneUnsupported`
//! derived from `vike_data::require_live_verb` — §7.5's gate has teeth from the first commit.
//!
//! ⚠ **Deribit is the first venue here that serves BOTH, and the partition is history.** Its row
//! declares `book: true, depth: true` (the DOM lane is the top 200 levels of the same `book.*` fold
//! its lossless lane publishes whole), so `StreamLabel::Book` and `StreamLabel::Depth` both resolve for it,
//! and each opens its OWN socket on the same channel. What stayed true is the gate: every lane a
//! venue's row does not declare is still a `LaneUnsupported` from `require_live_verb`, never from a
//! list written here.

use std::sync::Arc;
use vike_data::DataClientBuilder;

use vike_data::{DataClient, DataClientSink};
use vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES;

use super::hub::{MarketDataAttach, MarketDataAttachGate};
use crate::feeds::DataClientBrokerSharing;
use crate::feeds::venues::sharing_for;

/// The credentialed venues the `config.datahub_md_attach_venues` row opens to this plane, and the words
/// of the row that name none. A venue with no `Shared` row cannot be attached to at all (a client per
/// holder is a login per holder: decision 0193), so it is one of those words. No row opens nothing.
pub fn credentialed_from_row(row: Option<&str>) -> (Vec<&'static str>, Vec<String>) {
    let (mut venues, mut bad) = (Vec::new(), Vec::new());
    for word in row.unwrap_or("").split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match CREDENTIALED_VENUES.iter().find(|v| **v == word) {
            Some(v) if sharing_for(v) == DataClientBrokerSharing::Shared => {
                if !venues.contains(v) {
                    venues.push(*v);
                }
            }
            _ => bad.push(word.to_string()),
        }
    }
    (venues, bad)
}

/// The hub's question over the process's ONE broker: a credentialed venue's key is the operator's to
/// open and a window's only to join (decision 0193, verdict 8).
pub fn attach_gate(broker: Arc<crate::feeds::FeedBroker>) -> MarketDataAttachGate {
    Box::new(move |venue, symbol, lane| {
        if !CREDENTIALED_VENUES.contains(&venue) {
            MarketDataAttach::Open
        } else if broker.operator_holds(venue, symbol, lane) {
            MarketDataAttach::Held
        } else {
            MarketDataAttach::NotHeld
        }
    })
}

/// MarketData's client builder over the process's ONE broker, or say WHY not.
///
/// A venue this plane does not SERVE is refused here by name — the broker may build it for the
/// recorder, market data must not serve it. `credentialed` is what the operator's row opened
/// ([`credentialed_from_row`]). `Err` names the supported set. A silent skip is the failure this shape
/// exists to prevent: a hub that quietly served nothing for a venue would look healthy and deliver
/// nothing, which is indistinguishable from a quiet market.
///
/// A served venue gets a `crate::feeds::DataClientBrokerHandle` — an ordinary `DataClient` — so `MarketDataHub` is
/// untouched. The REAL client behind it is built by the broker on the handle's first subscribe.
pub fn market_builder(
    broker: Arc<crate::feeds::FeedBroker>,
    credentialed: Vec<&'static str>,
) -> DataClientBuilder {
    // The closure's types are spelled: `Box::new` is generic, so an unannotated closure would infer
    // `Result<Box<DataClientBrokerHandle>, _>`, which does not coerce to the builder's signature.
    Box::new(
        move |venue: &str,
              sink: Arc<dyn DataClientSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            if !supported().contains(&venue) && !credentialed.contains(&venue) {
                return Err(format!(
                    "market data: venue `{venue}` has no live feed in this build. Supported: [{}]",
                    supported().join(", ")
                ));
            }
            broker.attach_sink(crate::feeds::DataClientBrokerHolder::MarketData, sink);
            Ok(Box::new(broker.handle(crate::feeds::DataClientBrokerHolder::MarketData, venue)))
        },
    )
}

/// The venues this plane serves — with [`credentialed_from_row`]'s, the `market_data_venue=`
/// advertisement's source of truth, so a client learns at the HANDSHAKE which venues it may name. It
/// names no credentialed venue: only the operator's row opens one.
pub fn supported() -> Vec<&'static str> {
    crate::feeds::venues::keyless_venues()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::NoopSink;

    /// MarketData's builder over a broker on the real table — the production wiring, minus the process. The
    /// table's link points at nothing: a handle reaches the broker (and so the gatewayhub) only on
    /// its first subscribe, which none of these tests makes.
    fn build(venue: &str) -> Result<Box<dyn DataClient + Send>, String> {
        let link = vike_gatewayhub_client::client::GatewayLink::new(
            "127.0.0.1:1",
            vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()),
        );
        let b = crate::feeds::FeedBroker::new(
            crate::feeds::venues::real_client_table(&link),
            crate::feeds::venues::sharing_for,
        );
        market_builder(b, Vec::new())(venue, Arc::new(NoopSink))
    }

    /// **The row opens a credentialed venue that has a `Shared` row and nothing else**, and no row opens
    /// nothing. A word that names no such venue comes back to be named in the log.
    #[test]
    fn the_row_opens_only_a_shared_credentialed_venue() {
        assert_eq!(credentialed_from_row(None), (Vec::new(), Vec::new()));
        let (venues, bad) =
            credentialed_from_row(Some(" oanda, ig ,,oanda,alpaca,binance,notavenue"));
        assert_eq!(venues, ["oanda", "ig"], "trimmed, once each, in the row's order");
        assert_eq!(bad, ["alpaca", "binance", "notavenue"], "no sharing row, keyless, unknown");
    }

    /// **The row is what makes a venue buildable**: the same builder refuses it with no row.
    #[test]
    fn a_venue_the_row_opened_gets_a_handle() {
        let link = vike_gatewayhub_client::client::GatewayLink::new(
            "127.0.0.1:1",
            vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()),
        );
        let b = crate::feeds::FeedBroker::new(
            crate::feeds::venues::real_client_table(&link),
            crate::feeds::venues::sharing_for,
        );
        assert!(market_builder(b, vec!["oanda"])("oanda", Arc::new(NoopSink)).is_ok());
    }

    /// `build` returns a `Box<dyn DataClient + Send>`, which has no `Debug`.
    fn err_of(r: Result<Box<dyn DataClient + Send>, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    /// An UNKNOWN venue is an error naming what IS supported — never a silently absent feed.
    #[test]
    fn an_unsupported_venue_names_what_is_supported() {
        let err = err_of(build("kalshi"));
        assert!(err.contains("kalshi"), "{err}");
        assert!(err.contains("Supported"), "{err}");
    }

    /// A CREDENTIALED venue the broker shares (OANDA, decision 0193) is refused until the operator's row
    /// opens it, naming what is served.
    #[test]
    fn a_credentialed_venue_is_refused_until_the_row_opens_it() {
        let err = err_of(build("oanda"));
        assert!(err.contains("oanda") && err.contains("Supported"), "{err}");
    }

    /// Every venue this build DOES claim must be a real `vike_model::VENUES` slug — otherwise the
    /// `market_data_venue=` advertisement names something no client could ever ask for, and `acquire`'s
    /// first refusal (`UnknownVenue`) would fire on a venue this build says it serves.
    #[test]
    fn every_supported_venue_is_a_roster_slug() {
        for v in supported() {
            assert!(vike_model::VENUES.contains(&v), "`{v}` is not in vike_model::VENUES");
        }
    }

    /// Every venue this plane claims builds a handle (the real client is built by the broker on the
    /// first subscribe, so this opens no socket), and the plane claims every keyless client venue.
    #[test]
    fn a_supported_venue_builds_a_client() {
        for v in supported() {
            assert!(build(v).is_ok(), "`{v}`");
        }
        assert!(supported().contains(&"binance") && supported().contains(&"polymarket"));
    }

    /// Deribit's row: the plane advertises it and builds a handle for it.
    #[test]
    fn deribit_is_served() {
        assert!(supported().contains(&"deribit"));
        assert!(build("deribit").is_ok());
    }
}
