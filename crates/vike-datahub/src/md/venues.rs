//! `venues` — the ONLY venue-aware code in the market-data plane, one `#[cfg]` arm per venue.
//!
//! Everything else under `crate::md` is venue-free and testable with no network, which is what lets
//! the §8 item 15 hub suite run on the DEFAULT build over a scripted `DataClient` double.
//!
//! # The shape, and where it comes from
//!
//! `crates/vike-recorder/src/venues/mod.rs`'s `build_feed`/`supported` — including its two tests: an
//! UNKNOWN venue and a COMPILED-OUT one get DIFFERENT, actionable messages, because "unknown venue"
//! sends somebody hunting for a typo when the answer is a rebuild.
//!
//! ⚠ **It does NOT reuse `build_feed`.** That returns a `Box<dyn VenueFeed>` carrying family
//! resolution and a `Subscription` profile type this plane does not want, it supports only two
//! venues against the six here, and it is **not `Send`** — which is precisely why
//! `crates/vike-datahub/src/datahub_cli.rs` gives the recorder the main thread. The reuse is the
//! naming convention and the `#[cfg(not(feature))]`-arm-per-venue shape, not the code.
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
//! `crates/vike-model/src/venue_caps.rs`, that makes the two book lanes a strict PARTITION on day
//! one: the five CEX declare `book: false, depth: true` and polymarket declares `book: true,
//! depth: false`. So no venue serves both, and every other combination is a `LaneUnsupported`
//! derived from `vike_data::require_live_verb` — §7.5's gate has teeth from the first commit.

use std::sync::Arc;

use vike_data::live::{DataClient, LiveDataSink};

use super::hub::MarketClientBuilder;

/// Build one venue's market-data client, or say WHY not.
///
/// `Err` when this build links no client for that venue — the message names the Cargo feature to
/// rebuild with, or the supported set for a venue that is not in this plane at all. A silent skip is
/// the failure this shape exists to prevent: a hub that quietly served nothing for a venue would
/// look healthy and deliver nothing, which is indistinguishable from a quiet market.
#[cfg_attr(
    not(any(
        feature = "live-binance",
        feature = "live-bybit",
        feature = "live-okx",
        feature = "live-aster",
        feature = "live-hyperliquid",
        feature = "live-polymarket",
    )),
    allow(unused_variables)
)]
pub fn build_market_client(
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    match venue {
        // ⚠ Every arm is ONE line, and it is the same constructor
        // `crates/vike-recorder/src/venues/binance.rs`'s `BinanceFeed::symbols` calls. The `|| {}`
        // is the GUI repaint nudge — a headless caller passes an empty closure, exactly as the
        // recorder does.
        #[cfg(feature = "live-binance")]
        "binance" => Ok(Box::new(vike_binance::market_feed::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "live-binance"))]
        "binance" => Err(missing_feature("binance", "live-binance")),

        #[cfg(feature = "live-bybit")]
        "bybit" => Ok(Box::new(vike_bybit::market_feed::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "live-bybit"))]
        "bybit" => Err(missing_feature("bybit", "live-bybit")),

        #[cfg(feature = "live-okx")]
        "okx" => Ok(Box::new(vike_okx::market_feed::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "live-okx"))]
        "okx" => Err(missing_feature("okx", "live-okx")),

        #[cfg(feature = "live-aster")]
        "aster" => Ok(Box::new(vike_aster::market_feed::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "live-aster"))]
        "aster" => Err(missing_feature("aster", "live-aster")),

        #[cfg(feature = "live-hyperliquid")]
        "hyperliquid" => Ok(Box::new(vike_hyperliquid::market_feed::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "live-hyperliquid"))]
        "hyperliquid" => Err(missing_feature("hyperliquid", "live-hyperliquid")),

        #[cfg(feature = "live-polymarket")]
        "polymarket" => Ok(Box::new(vike_polymarket::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "live-polymarket"))]
        "polymarket" => Err(missing_feature("polymarket", "live-polymarket")),

        other => Err(format!(
            "market data: venue `{other}` has no live feed in this build. Supported: [{}]",
            supported().join(", ")
        )),
    }
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

/// The REAL venue table — the one function in this plane that names a bridge crate, and therefore
/// the only thing `live-feeds` adds to the dependency tree.
///
/// Everything else under `crate::md` is feature-free (see that module's doc and
/// `crate::backfill::BackfillTable`'s precedent), which is what keeps `MdHub` out of
/// `crate::server::serve_authed`'s signature as a cfg'd type — the shape §8 item 4 proposed and that
/// cannot work.
pub fn real_market_venue_table() -> MarketClientBuilder {
    Box::new(build_market_client)
}

#[cfg(not(all(
    feature = "live-binance",
    feature = "live-bybit",
    feature = "live-okx",
    feature = "live-aster",
    feature = "live-hyperliquid",
    feature = "live-polymarket",
)))]
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

    /// `build_market_client` returns a `Box<dyn DataClient + Send>`, which has no `Debug`.
    fn err_of(r: Result<Box<dyn DataClient + Send>, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    /// An UNKNOWN venue is an error naming what IS supported — never a silently absent feed.
    #[test]
    fn an_unsupported_venue_names_what_is_supported() {
        let err = err_of(build_market_client("kalshi", Arc::new(NoopSink)));
        assert!(err.contains("kalshi"), "{err}");
        assert!(err.contains("Supported"), "{err}");
    }

    /// A venue that EXISTS but was compiled out gets a DIFFERENT, actionable message: the feature to
    /// rebuild with, rather than "unknown venue", which would send somebody hunting for a typo.
    #[cfg(not(feature = "live-binance"))]
    #[test]
    fn a_compiled_out_venue_names_its_feature() {
        let err = err_of(build_market_client("binance", Arc::new(NoopSink)));
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
        assert!(build_market_client("binance", Arc::new(NoopSink)).is_ok());
        assert!(build_market_client("polymarket", Arc::new(NoopSink)).is_ok());
        assert!(supported().contains(&"binance") && supported().contains(&"polymarket"));
    }
}
