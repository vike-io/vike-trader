//! `venues` — the ONLY venue-aware code in this crate, one module per recordable venue, each behind
//! its own Cargo feature.
//!
//! Everything else here ([`config`](crate::config), [`membership`](crate::membership),
//! [`session`](crate::session), [`runtime`](crate::runtime)) is venue-free and tested with no
//! network. A venue module's job is to implement [`VenueFeed`](crate::runtime::VenueFeed) by
//! ASSEMBLING what the bridge crate already owns — resolving a family to live symbols and handing
//! over the bridge's own `DataClient` — never by reimplementing venue logic here.
//!
//! Features rather than plain deps because a bridge is heavy: enabling `polymarket` pulls
//! k256/keccak/tungstenite into the recorder binary, and a build recording only static-symbol venues
//! should not link an EIP-712 signer. [`build_feed`] is what a feature-absent build answers with —
//! a startup ERROR naming the missing feature, never a silent no-record.

use std::sync::Arc;

use vike_data::live::LiveDataSink;

use crate::config::Subscription;
use crate::runtime::VenueFeed;

#[cfg(feature = "binance")]
pub mod binance;
#[cfg(feature = "polymarket")]
pub mod polymarket;

/// Build the [`VenueFeed`] one profile subscription names.
///
/// `Err` when this build has no support for that venue — the message names the Cargo feature to
/// rebuild with. A recorder that silently skipped an unsupported venue would look healthy while
/// recording nothing, which is the failure this crate's config validation already exists to prevent
/// at the profile level; the same rule applies to the build.
// `sink` is consumed by whichever venue arm this build compiled; a build with NO venue feature has
// no such arm, and the signature stays the same either way.
#[cfg_attr(not(any(feature = "polymarket", feature = "binance")), allow(unused_variables))]
pub fn build_feed(
    sub: &Subscription,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn VenueFeed>, String> {
    match sub.venue.as_str() {
        #[cfg(feature = "polymarket")]
        polymarket::VENUE => {
            let feed = match &sub.family {
                Some(family) => polymarket::PolymarketFeed::family(family, sink)?,
                None => polymarket::PolymarketFeed::symbols(&sub.symbols, sink),
            };
            Ok(Box::new(feed))
        }
        #[cfg(not(feature = "polymarket"))]
        "polymarket" => Err(missing_feature("polymarket", "polymarket")),
        #[cfg(feature = "binance")]
        binance::VENUE => {
            let feed = match &sub.family {
                Some(family) => binance::BinanceFeed::family(family, sink),
                None => binance::BinanceFeed::symbols(&sub.symbols, sink),
            };
            Ok(Box::new(feed))
        }
        #[cfg(not(feature = "binance"))]
        "binance" => Err(missing_feature("binance", "binance")),
        other => Err(format!(
            "recorder: venue `{other}` has no feed in this build. Supported: [{}]",
            supported().join(", ")
        )),
    }
}

/// The venues this build can actually record.
pub fn supported() -> Vec<&'static str> {
    vec![
        #[cfg(feature = "binance")]
        binance::VENUE,
        #[cfg(feature = "polymarket")]
        polymarket::VENUE,
    ]
}

#[cfg(not(all(feature = "polymarket", feature = "binance")))]
fn missing_feature(venue: &str, feature: &str) -> String {
    format!(
        "recorder: venue `{venue}` is not compiled into this build — rebuild with \
         `--features {feature}`. (It is a Cargo feature because the bridge is heavy: it pulls the \
         venue's signing and transport tree into this binary.)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Backfill;
    use vike_data::NoopSink;

    /// `build_feed` returns a `Box<dyn VenueFeed>`, which has no `Debug`, so `unwrap_err` cannot be
    /// used on it.
    fn err_of(r: Result<Box<dyn VenueFeed>, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    fn sub(venue: &str) -> Subscription {
        Subscription {
            venue: venue.into(),
            family: Some("btc-updown-5m".into()),
            symbols: Vec::new(),
            backfill: Backfill::Off,
        }
    }

    /// An unknown venue must FAIL at startup, not be skipped. A recorder that skipped it would
    /// report itself healthy while recording nothing for that venue — the exact silent-nothing
    /// failure `RecorderProfile::from_toml`'s validation exists to prevent one layer up.
    #[test]
    fn an_unsupported_venue_is_a_startup_error_naming_what_is_supported() {
        let err = err_of(build_feed(&sub("kalshi"), Arc::new(NoopSink)));
        assert!(err.contains("kalshi"), "{err}");
        assert!(err.contains("Supported"), "{err}");
    }

    /// A venue that EXISTS but was compiled out gets a different, actionable message: the feature to
    /// rebuild with, rather than "unknown venue", which would send someone hunting for a typo.
    #[cfg(not(feature = "polymarket"))]
    #[test]
    fn a_compiled_out_venue_names_the_feature() {
        let err = err_of(build_feed(&sub("polymarket"), Arc::new(NoopSink)));
        assert!(err.contains("--features polymarket"), "{err}");
    }

    #[cfg(feature = "polymarket")]
    #[test]
    fn a_supported_venue_builds_its_feed() {
        let feed = build_feed(&sub("polymarket"), Arc::new(NoopSink)).unwrap();
        assert_eq!(feed.venue(), "polymarket");
        assert_eq!(feed.family(), Some("btc-updown-5m"));
        // CONTAINS, not equals: `supported()` grows with each venue feature, and this build may have
        // several on. An equality assertion here made adding the second venue fail a test about the
        // first one.
        assert!(supported().contains(&"polymarket"), "{:?}", supported());
    }

    /// The binance arm — the STATIC-family venue, built through the same dispatch.
    #[cfg(feature = "binance")]
    #[test]
    fn the_binance_arm_builds_a_static_family_feed() {
        let mut s = sub("binance");
        s.family = Some("*USDT.P".into());
        let feed = build_feed(&s, Arc::new(NoopSink)).unwrap();
        assert_eq!(feed.venue(), "binance");
        assert_eq!(feed.family(), Some("*USDT.P"));
        assert!(supported().contains(&"binance"), "{:?}", supported());
    }

    /// A profile naming BOTH venues is the real multi-venue case, and the one whose `#[cfg]` shape
    /// differs from either single-venue build.
    #[cfg(all(feature = "binance", feature = "polymarket"))]
    #[test]
    fn both_venues_dispatch_in_one_build() {
        assert_eq!(supported(), vec!["binance", "polymarket"]);
        assert_eq!(
            build_feed(&sub("polymarket"), Arc::new(NoopSink)).unwrap().venue(),
            "polymarket"
        );
        let mut b = sub("binance");
        b.family = Some("*USDT".into());
        assert_eq!(build_feed(&b, Arc::new(NoopSink)).unwrap().venue(), "binance");
    }
}
