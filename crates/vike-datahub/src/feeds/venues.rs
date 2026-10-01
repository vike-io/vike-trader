//! The broker's venue table — the ONE function in this process that constructs a venue
//! market-data client ([`build_client`]), and the declared sharing column ([`sharing_for`]).
//!
//! The six client arms moved here from `crate::md::venues`, where each sat behind its `live-<v>`
//! feature. They are gated on `venue-<v>` now, and every `live-<v>` implies its `venue-<v>`
//! (`crates/vike-datahub/Cargo.toml`), because since the broker BOTH planes take their client from
//! this table: a PLANE feature (`live-`, `record-`) says what the process SERVES, a VENUE feature
//! says what it LINKS. Md's own refusal — a venue this build does not serve live — stays md's, in
//! `crates/vike-datahub/src/md/venues.rs`'s `market_builder`, with its message byte-identical.
//!
//! # The shape: one `#[cfg]` arm AND one `#[cfg(not)]` arm per venue
//!
//! The md table's shape, kept. A venue this table knows but this build did not link is refused
//! naming the feature to rebuild with; a venue it does not know at all is refused naming the table.
//! Two different messages, because "unknown venue" sends somebody hunting for a typo when the answer
//! is a rebuild. The paired arms also mean the `match` never collapses to a single binding arm on a
//! build that links no venue.
use std::sync::Arc;

use vike_data::live::{DataClient, LiveDataSink};

use super::{ClientBuilder, Sharing};

/// Build one venue's market-data client, or say WHY not.
///
/// ⚠ Every constructor is the one `crate::md::venues` called before the broker existed, verbatim —
/// plus, since decision 0095, the venue's own `venue_setting` rows out of `venue_settings`
/// (Polymarket's socket batching, each CEX feed's mark-stream row). The `|| {}` is the GUI repaint
/// nudge — a headless caller passes an empty closure, exactly as the recorder does.
// `sink` and `venue_settings` are consumed by whichever venue arm this build compiled — every arm
// that builds a client reads both — so ONE condition covers both unused-parameter cases: a build
// that links no venue at all (the `light-consumers` feature suite) uses neither. ⚠ There was a
// second `#[cfg_attr(…, allow(unused_variables))]` here once, and it OVERLAPPED the first the
// moment every venue feature was off: clippy's `duplicated_attributes` refuses two
// `allow(unused_variables)` resolving onto one item under `-D warnings`.
#[cfg_attr(
    not(any(
        feature = "venue-binance",
        feature = "venue-bybit",
        feature = "venue-okx",
        feature = "venue-aster",
        feature = "venue-hyperliquid",
        feature = "venue-polymarket",
    )),
    allow(unused_variables)
)]
pub fn build_client(
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
    venue_settings: &std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<Box<dyn DataClient + Send>, String> {
    match venue {
        #[cfg(feature = "venue-binance")]
        "binance" => Ok(Box::new(
            vike_binance::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "binance")),
        )),
        #[cfg(not(feature = "venue-binance"))]
        "binance" => Err(not_linked("binance")),

        #[cfg(feature = "venue-bybit")]
        "bybit" => Ok(Box::new(
            vike_bybit::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "bybit")),
        )),
        #[cfg(not(feature = "venue-bybit"))]
        "bybit" => Err(not_linked("bybit")),

        #[cfg(feature = "venue-okx")]
        "okx" => Ok(Box::new(
            vike_okx::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "okx")),
        )),
        #[cfg(not(feature = "venue-okx"))]
        "okx" => Err(not_linked("okx")),

        #[cfg(feature = "venue-aster")]
        "aster" => Ok(Box::new(
            vike_aster::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "aster")),
        )),
        #[cfg(not(feature = "venue-aster"))]
        "aster" => Err(not_linked("aster")),

        #[cfg(feature = "venue-hyperliquid")]
        "hyperliquid" => Ok(Box::new(
            vike_hyperliquid::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "hyperliquid")),
        )),
        #[cfg(not(feature = "venue-hyperliquid"))]
        "hyperliquid" => Err(not_linked("hyperliquid")),

        #[cfg(feature = "venue-polymarket")]
        "polymarket" => {
            // K — `venue.polymarket.ws_tokens_per_socket` (decision 0095), or the bridge's default.
            let k = vike_polymarket::tokens_per_socket(|field| {
                venue_settings
                    .get("polymarket")
                    .and_then(|s| s.get(vike_secrets::venue_setting::SettingTier::Any, field))
                    .map(str::to_string)
            });
            Ok(Box::new(vike_polymarket::Feeds::new(sink, || {}).with_tokens_per_socket(k)))
        }
        #[cfg(not(feature = "venue-polymarket"))]
        "polymarket" => Err(not_linked("polymarket")),

        other => Err(format!(
            "feed broker: venue `{other}` has no client in this process's venue table. Table: [{}]",
            CLIENT_VENUES.join(", ")
        )),
    }
}

/// `venue`'s stored `mark_streams` row (decision 0095), or `None` — the feed then keeps its charter
/// default (`vike_bridge_core::mark_streams_from`). The same three lines as the trading daemon's
/// twin in `crates/vike-tradehub/src/feeds.rs`; gated to the venues whose arms call it, so a build
/// linking none of them carries no unused function.
#[cfg(any(
    feature = "venue-binance",
    feature = "venue-bybit",
    feature = "venue-okx",
    feature = "venue-aster",
    feature = "venue-hyperliquid",
))]
fn mark_streams_setting<'a>(
    venue_settings: &'a std::collections::BTreeMap<
        String,
        vike_secrets::venue_setting::VenueSettings,
    >,
    venue: &str,
) -> Option<&'a str> {
    venue_settings
        .get(venue)
        .and_then(|s| s.get(vike_secrets::venue_setting::SettingTier::Any, "mark_streams"))
}

/// The table [`super::FeedBroker::new`] takes in production — carrying every venue's
/// `venue_setting` rows, which the clients read (decision 0095).
pub fn real_client_table(
    venue_settings: std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> ClientBuilder {
    Box::new(move |venue, sink| build_client(venue, sink, &venue_settings))
}

/// The venues [`build_client`] has an arm for, whatever this build's features.
pub const CLIENT_VENUES: [&str; 6] =
    ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"];

/// MEASURED from each bridge's `DataClient::unsubscribe` (2026-09-28): the five CEX stop and join
/// exactly one thread per subscription; polymarket releases a SEAT and resubscribes the shard's
/// other tokens, which would punch a gap into a co-tenant holder's tape.
pub const EXPLICIT_SHARING: [(&str, Sharing); 6] = [
    ("binance", Sharing::Shared),
    ("bybit", Sharing::Shared),
    ("okx", Sharing::Shared),
    ("aster", Sharing::Shared),
    ("hyperliquid", Sharing::Shared),
    ("polymarket", Sharing::PerHolder),
];

/// ⚠ The fallback is `PerHolder` — two clients, the behaviour before the broker — because it is the
/// direction that cannot corrupt a tape. A venue earns `Shared` only by a row above, argued from its
/// own `unsubscribe`.
pub fn sharing_for(venue: &str) -> Sharing {
    EXPLICIT_SHARING.iter().find(|(name, _)| *name == venue).map_or(Sharing::PerHolder, |(_, s)| *s)
}

#[cfg(not(all(
    feature = "venue-binance",
    feature = "venue-bybit",
    feature = "venue-okx",
    feature = "venue-aster",
    feature = "venue-hyperliquid",
    feature = "venue-polymarket",
)))]
fn not_linked(venue: &str) -> String {
    format!(
        "feed broker: venue `{venue}` is not linked into this build — rebuild with \
         `--features venue-{venue}` (or a plane feature that implies it, e.g. `live-{venue}`)"
    )
}

// ⚠ INLINE, deliberately, where the plan drew a `venues_tests.rs` beside this file: the code-layout
// rule moves a test block out only for a file >= 400 lines carrying >= 150 test lines, and this one
// is neither. A new `#[cfg(test)] mod NAME;` file would also widen the set
// `crates/vike-ops/tests/compile_time_path_gate.rs` skips wholesale, whose ceiling exists so that set
// does not grow without an argument — and a 60-line module has no argument for leaving its file.
#[cfg(test)]
mod tests {
    use super::*;

    use vike_data::NoopSink;

    /// D4, as a check on the table rather than a sentence: a venue that serves a LOSSLESS book
    /// lane is never Shared, so the recorder can never join a live book mid-chain without its
    /// anchor.
    #[test]
    fn no_shared_venue_serves_a_lossless_book_lane() {
        for v in vike_model::VENUES {
            if sharing_for(v) == crate::feeds::Sharing::Shared {
                assert!(
                    !vike_model::caps_for(v).live_data.book,
                    "`{v}` is Shared but declares a lossless book lane — a holder joining it live \
                     gets deltas with no anchor. Make it PerHolder, or argue the exception in \
                     decision record 0092."
                );
            }
        }
    }

    /// The measured column: polymarket's `unsubscribe` resubscribes a shard's co-tenants.
    #[test]
    fn polymarket_is_per_holder_and_the_five_cex_share() {
        assert_eq!(sharing_for("polymarket"), crate::feeds::Sharing::PerHolder);
        for v in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
            assert_eq!(sharing_for(v), crate::feeds::Sharing::Shared, "{v}");
        }
    }

    /// Every venue this table can build has an EXPLICIT sharing row — the fallback is for venues
    /// with no client here, and must never be how a client venue got classified.
    #[test]
    fn every_client_venue_is_classified_explicitly() {
        for v in CLIENT_VENUES {
            assert!(
                EXPLICIT_SHARING.iter().any(|(name, _)| *name == v),
                "`{v}` has no sharing row"
            );
        }
    }

    /// The both-ways check between [`CLIENT_VENUES`] and [`build_client`]'s arms: every listed
    /// venue either BUILDS (its `venue-<v>` feature is on) or is refused naming that feature —
    /// never with the unknown-venue message, which would mean the list names a venue the table has
    /// no arm for. It is what keeps the constructors themselves compiled AND called in the
    /// `live-feeds` lane, now that md's own `a_supported_venue_builds_a_client` reaches a broker
    /// handle rather than a real client.
    #[test]
    fn every_client_venue_builds_or_names_its_feature() {
        for v in CLIENT_VENUES {
            if let Err(e) = build_client(v, Arc::new(NoopSink), &std::collections::BTreeMap::new())
            {
                assert!(e.contains(&format!("--features venue-{v}")), "{e}");
                assert!(!e.contains("Table:"), "a compiled-out venue is not an unknown one: {e}");
            }
        }
    }

    /// An UNKNOWN venue is refused naming the table — never a feature that does not exist.
    #[test]
    fn an_unknown_venue_names_the_table_not_a_feature() {
        let err =
            match build_client("kalshi", Arc::new(NoopSink), &std::collections::BTreeMap::new()) {
                Ok(_) => panic!("expected an error"),
                Err(e) => e,
            };
        assert!(err.contains("kalshi") && err.contains("Table:"), "{err}");
        assert!(!err.contains("--features"), "{err}");
    }
}
