//! `venues` — the ONLY venue-aware code in the market-data plane: what this build SERVES live
//! ([`supported`], one `#[cfg]` row per venue) and md's client builder over the process's feed
//! broker ([`market_builder`]).
//!
//! Everything else under `crate::md` is venue-free and testable with no network, which is what lets
//! the §8 item 15 hub suite run on the DEFAULT build over a scripted `DataClient` double.
//!
//! ⚠ **The client ARMS are not here any more.** They moved to
//! `crates/vike-datahub/src/feeds/venues.rs`'s `build_client`, the one function in this process
//! that constructs a venue client, because both planes now take their client from the broker that
//! owns it. What stayed is md's REFUSAL: a venue this build does not serve live is refused here by
//! name, with the two messages the arms used to give, before the broker is ever asked.
//!
//! # The shape, and where it comes from
//!
//! The recording table's (`crates/vike-datahub/src/recording.rs`'s
//! `build_recording_feed`/`supported`, `vike-recorder`'s `venues` module until decision 0092) —
//! including its two tests: an UNKNOWN venue and a COMPILED-OUT one get DIFFERENT, actionable
//! messages, because "unknown venue" sends somebody hunting for a typo when the answer is a rebuild.
//!
//! ⚠ **It does NOT reuse `build_recording_feed`.** That returns an `AssembledFeed` carrying family
//! resolution and a `Subscription` profile type this plane does not want, and it supports only two
//! venues against the six here. The reuse is the naming convention and the
//! `#[cfg(not(feature))]`-arm-per-venue shape, not the code — and, since 0092, the broker both sit
//! on: neither table constructs a client.
//!
//! # Why `live-` prefixes the feature names
//!
//! `crates/vike-datahub/Cargo.toml`'s own argument for `record-polymarket`, verbatim: on
//! `vike-recorder`, `polymarket` can only mean the feed; *"here it would sit beside
//! `vike-polymarket`'s EXEC plane — the k256/keccak signer this daemon must never link — and a
//! feature called `polymarket` on a process that also serves a store write is a name that invites
//! exactly that mistake. The prefix says which plane."* This daemon now has TWO venue-linking
//! families, so the prefix does double duty: it also says which PLANE inside this process.
//!
//! # ⚠ The start-narrow set, and the partition it lands on
//!
//! `crates/vike-panels/src/dom.rs`'s `DomVenue` set — binance, bybit, okx, aster, hyperliquid — plus
//! polymarket, whose keyless `feeds` half is the one a market-data plane wants. Read off
//! `crates/vike-model/src/venues/venue_caps.rs`, that makes the two book lanes a strict PARTITION on day
//! one: the five CEX declare `book: false, depth: true` and polymarket declares `book: true,
//! depth: false`. So no venue serves both, and every other combination is a `LaneUnsupported`
//! derived from `vike_data::require_live_verb` — §7.5's gate has teeth from the first commit.

use std::sync::Arc;

use vike_data::{DataClient, LiveDataSink};

use super::hub::MarketClientBuilder;

/// Md's client builder over the process's ONE broker, or say WHY not.
///
/// A venue this build does not SERVE live is refused here by name — the broker may link it for the
/// recorder, md must not serve it. `Err` names the Cargo feature to rebuild with, or the supported
/// set for a venue that is not in this plane at all; both texts are byte-identical to the ones the
/// per-venue arms gave before they moved to `crates/vike-datahub/src/feeds/venues.rs`'s
/// `build_client`. A silent skip is the failure this shape exists to prevent: a hub that quietly
/// served nothing for a venue would look healthy and deliver nothing, which is indistinguishable
/// from a quiet market.
///
/// A served venue gets a `crate::feeds::FeedHandle` — an ordinary `DataClient` — so `MdHub` is
/// untouched. The REAL client behind it is built by the broker on the handle's first subscribe.
pub fn market_builder(broker: Arc<crate::feeds::FeedBroker>) -> MarketClientBuilder {
    // The closure's types are spelled: `Box::new` is generic, so an unannotated closure would infer
    // `Result<Box<FeedHandle>, _>`, which does not coerce to the builder's signature.
    Box::new(
        move |venue: &str,
              sink: Arc<dyn LiveDataSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            if !supported().contains(&venue) {
                return Err(if crate::feeds::venues::CLIENT_VENUES.contains(&venue) {
                    missing_feature(venue, &format!("live-{venue}"))
                } else {
                    format!(
                        "market data: venue `{venue}` has no live feed in this build. Supported: [{}]",
                        supported().join(", ")
                    )
                });
            }
            broker.attach_sink(crate::feeds::Holder::Md, sink);
            Ok(Box::new(broker.handle(crate::feeds::Holder::Md, venue)))
        },
    )
}

/// The venues this build can actually serve — the `md_venue=` advertisement's source of truth, so a
/// client learns at the HANDSHAKE which venues it may name.
pub fn supported() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "live-binance")]
        "binance",
        #[cfg(feature = "live-bybit")]
        "bybit",
        #[cfg(feature = "live-okx")]
        "okx",
        #[cfg(feature = "live-aster")]
        "aster",
        #[cfg(feature = "live-hyperliquid")]
        "hyperliquid",
        #[cfg(feature = "live-polymarket")]
        "polymarket",
    ]
}

// ⚠ No `#[cfg(not(all(...)))]` on this any more: `market_builder` reaches it on every build, for
// any known venue this build does not serve live.
fn missing_feature(venue: &str, feature: &str) -> String {
    format!(
        "market data: venue `{venue}` is not compiled into this build — rebuild with \
         `--features {feature}`. (It is a Cargo feature because the bridge is heavy: it pulls the \
         venue's transport tree into this binary. The `live-` prefix names the PLANE — this is the \
         keyless market-data half, never the order signer.)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_data::NoopSink;

    /// Md's builder over a broker on the real table — the production wiring, minus the process.
    fn build(venue: &str) -> Result<Box<dyn DataClient + Send>, String> {
        let b = crate::feeds::FeedBroker::new(
            crate::feeds::venues::real_client_table(std::collections::BTreeMap::new()),
            crate::feeds::venues::sharing_for,
        );
        market_builder(b)(venue, Arc::new(NoopSink))
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

    /// A venue that EXISTS but was compiled out gets a DIFFERENT, actionable message: the feature to
    /// rebuild with, rather than "unknown venue", which would send somebody hunting for a typo.
    #[cfg(not(feature = "live-binance"))]
    #[test]
    fn a_compiled_out_venue_names_its_feature() {
        let err = err_of(build("binance"));
        assert!(err.contains("--features live-binance"), "{err}");
        assert!(!err.contains("Supported"), "a compiled-out venue is not an unknown one: {err}");
    }

    /// The DEFAULT build serves NOTHING, and that is a real configuration rather than an oversight —
    /// it is what a `live-feeds`-less build is, and it must refuse by name rather than serve an
    /// empty plane.
    #[cfg(not(any(
        feature = "live-binance",
        feature = "live-bybit",
        feature = "live-okx",
        feature = "live-aster",
        feature = "live-hyperliquid",
        feature = "live-polymarket",
    )))]
    #[test]
    fn a_default_build_serves_no_venue() {
        assert!(supported().is_empty(), "{:?}", supported());
    }

    /// Every venue this build DOES claim must be a real `vike_model::VENUES` slug — otherwise the
    /// `md_venue=` advertisement names something no client could ever ask for, and `acquire`'s
    /// first refusal (`UnknownVenue`) would fire on a venue this build says it serves.
    #[test]
    fn every_supported_venue_is_a_roster_slug() {
        for v in supported() {
            assert!(vike_model::VENUES.contains(&v), "`{v}` is not in vike_model::VENUES");
        }
    }

    /// The compiled set is exactly the one this build's features name — the both-ways check that
    /// stops a venue arm and its `supported()` row drifting apart.
    #[cfg(all(feature = "live-binance", feature = "live-polymarket"))]
    #[test]
    fn a_supported_venue_builds_a_client() {
        assert!(build("binance").is_ok());
        assert!(build("polymarket").is_ok());
        assert!(supported().contains(&"binance") && supported().contains(&"polymarket"));
    }
}
