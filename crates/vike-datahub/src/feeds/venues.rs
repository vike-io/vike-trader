//! The broker's venue table — the ONE function in this process that constructs a venue
//! market-data client ([`build_client`]), and the declared sharing column ([`sharing_for`]).
//!
//! The client arms moved here from `crate::md::venues`, where each sat behind its `live-<v>`
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

use vike_data::{DataClient, LiveDataSink};

use super::{ClientBuilder, Sharing};

/// Build one venue's market-data client, or say WHY not.
///
/// ⚠ Every constructor is the one `crate::md::venues` called before the broker existed, verbatim —
/// plus, since decision 0095, the venue's own `venue_setting` rows out of `venue_settings`
/// (Polymarket's socket batching, each CEX feed's mark-stream row). The `|| {}` is the GUI repaint
/// nudge — a headless caller passes an empty closure, exactly as the recorder does.
// `sink` is consumed by every venue arm and `venue_settings` by every arm but deribit's (its marks
// stream chain-wide through the options feed, so it has no `venue_setting` row to read), so ONE
// condition — the six venues that read it — covers both unused-parameter cases: a build that links
// none of those six (no venue at all, the `light-consumers` feature suite; or `venue-deribit`
// alone, which uses `sink` only) leaves `venue_settings` unread. ⚠ There was a second
// `#[cfg_attr(…, allow(unused_variables))]` here once, and it OVERLAPPED the first the moment
// every venue feature was off: clippy's `duplicated_attributes` refuses two
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

        // Deribit reads NO `venue_setting` row (its marks stream chain-wide through the options
        // feed, not a per-bars companion socket), so unlike every arm above it hands the feed
        // nothing but the sink.
        #[cfg(feature = "venue-deribit")]
        "deribit" => Ok(Box::new(vike_deribit::market_feed::Feeds::new(sink, || {}))),
        #[cfg(not(feature = "venue-deribit"))]
        "deribit" => Err(not_linked("deribit")),

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

/// The venues [`build_client`] has an arm for, whatever this build's features — DERIVED from
/// [`EXPLICIT_SHARING`], the one table a client venue is declared in, so a venue cannot be built
/// here and left unclassified (or classified and unbuilt) by forgetting one of two lists.
pub const CLIENT_VENUES: [&str; EXPLICIT_SHARING.len()] = {
    let mut out = [""; EXPLICIT_SHARING.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = EXPLICIT_SHARING[i].0;
        i += 1;
    }
    out
};

/// THE client-venue table: one row per venue [`build_client`] has an arm for, carrying its sharing.
/// ⚠ A row here is not enough on its own — `a_client_venue_has_every_site_it_needs` names each of
/// the other places a venue lives in this crate (the arm and its twin, the two features, the
/// `md::venues::supported` row) and fails with the full list for the one a new row forgot.
///
/// MEASURED from each bridge's `DataClient::unsubscribe` (2026-09-28): the five CEX stop and join
/// exactly one thread per subscription; polymarket releases a SEAT and resubscribes the shard's
/// other tokens, which would punch a gap into a co-tenant holder's tape.
///
/// ⚠ **Deribit is `PerHolder` for a DIFFERENT reason, and not a measured one.** Its `unsubscribe`
/// is the CEX shape (`FeedRegistry::stop_join`, one thread per subscription), so that column would
/// say `Shared` — but it declares a LOSSLESS book lane (`live_data.book`), and
/// `no_shared_venue_serves_a_lossless_book_lane` forbids sharing one: a recorder joining a live
/// book mid-chain would get deltas with no anchor. Nothing records deribit today, so the cost of
/// the safe direction is nil; the day something does, that test is where the exception is argued.
pub const EXPLICIT_SHARING: [(&str, Sharing); 7] = [
    ("binance", Sharing::Shared),
    ("bybit", Sharing::Shared),
    ("okx", Sharing::Shared),
    ("aster", Sharing::Shared),
    ("hyperliquid", Sharing::Shared),
    ("polymarket", Sharing::PerHolder),
    ("deribit", Sharing::PerHolder),
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
    feature = "venue-deribit",
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
// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` skips wholesale, whose ceiling exists so that set
// does not grow without an argument — and a 60-line module has no argument for leaving its file.
#[cfg(test)]
mod tests {
    use super::*;

    use vike_data::NoopSink;

    /// `build_client`'s own source text, from its signature to its closing brace — what the two
    /// structural pins below read, because with a build's `venue-<v>` features off an arm is not
    /// even compiled and nothing else can see it.
    fn build_client_source() -> &'static str {
        let src = include_str!("venues.rs");
        let body = &src[src.find("pub fn build_client(").expect("build_client")..];
        &body[..body.find("\n}\n").expect("build_client's closing brace")]
    }

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

    /// Deribit shares nothing, and for the one reason a measurement cannot give: its `unsubscribe`
    /// is the CEX shape, but it declares a lossless book lane, which [`EXPLICIT_SHARING`]'s doc and
    /// `no_shared_venue_serves_a_lossless_book_lane` say a shared client may never serve.
    #[test]
    fn deribit_is_per_holder_because_it_serves_a_lossless_book() {
        assert_eq!(sharing_for("deribit"), crate::feeds::Sharing::PerHolder);
        assert!(vike_model::caps_for("deribit").live_data.book);
    }

    /// Every client venue is a real roster slug and is listed once. [`CLIENT_VENUES`] is DERIVED
    /// from [`EXPLICIT_SHARING`], so "every client venue has a sharing row" holds by construction —
    /// what that leaves is a typo'd slug (which would build a client no roster venue can ask for)
    /// and a duplicate row (which `sharing_for` would answer from the first and never the second).
    #[test]
    fn every_client_venue_is_a_roster_slug_and_listed_once() {
        for (i, v) in CLIENT_VENUES.iter().enumerate() {
            assert!(vike_model::VENUES.contains(v), "`{v}` is not in vike_model::VENUES");
            assert!(!CLIENT_VENUES[..i].contains(v), "`{v}` has two rows in EXPLICIT_SHARING");
        }
    }

    /// **The next venue is one row here and then THIS test, not a scavenger hunt.** Every site in
    /// this crate a client venue lives at, named for each [`CLIENT_VENUES`] row and reported TOGETHER
    /// — the arm and its `#[cfg(not)]` twin in [`build_client`], the `venue-<v>` and `live-<v>`
    /// features in `crates/vike-datahub/Cargo.toml`, and the `#[cfg(feature = "live-<v>")]` row of
    /// `crates/vike-datahub/src/md/venues.rs`'s `supported` — so a venue added to the table alone
    /// fails once with the whole list instead of once per site over five rebuilds.
    ///
    /// ⚠ It reads this crate's OWN files (compile-time `include_str!`, mirror-safe) and nothing
    /// outside it, so it is deliberately blind to the sites that live elsewhere — the multicall's
    /// forwarding (`crates/vike/Cargo.toml`, held by `crates/vike-ops/tests/container_deploy/multicall_gate.rs`), the
    /// CI lane and its justfile mirror (`xtask/tests/local_gate_mirrors_ci.rs`), the
    /// affected-set trigger row (`xtask/src/ci/tables/feature_suites.rs`) and the desktop's `LOCAL_FEED_VENUES`.
    /// Those gates (or, for the last, a Trade window with no feed) are where those fail.
    #[test]
    fn a_client_venue_has_every_site_it_needs() {
        let manifest = include_str!("../../Cargo.toml");
        let md = include_str!("../md/venues.rs");
        let build = build_client_source();
        let mut missing = Vec::new();
        for v in CLIENT_VENUES {
            for feature in [format!("venue-{v}"), format!("live-{v}")] {
                let declared = manifest.lines().any(|l| {
                    l.split('#').next().unwrap_or("").starts_with(&format!("{feature} = ["))
                });
                if !declared {
                    missing.push(format!("`{v}`: Cargo.toml declares no `{feature}` feature"));
                }
            }
            for arm in [
                format!("#[cfg(feature = \"venue-{v}\")]\n        \"{v}\" =>"),
                format!(
                    "#[cfg(not(feature = \"venue-{v}\"))]\n        \"{v}\" => Err(not_linked(\"{v}\"))"
                ),
            ] {
                if !build.contains(&arm) {
                    missing.push(format!("`{v}`: build_client has no arm shaped `{arm}`"));
                }
            }
            if !md.contains(&format!("#[cfg(feature = \"live-{v}\")]\n        \"{v}\",")) {
                missing.push(format!("`{v}`: md::venues::supported has no `live-{v}` row"));
            }
        }
        assert!(
            missing.is_empty(),
            "a client venue is missing a site:\n  {}",
            missing.join("\n  ")
        );
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

    /// **Each CEX arm hands its OWN venue's `mark_streams` row to its OWN feed, and the Polymarket
    /// arm hands its `ws_tokens_per_socket` row to the shard sizing** (decision 0095).
    ///
    /// ⚠ A STRUCTURAL pin, and why it cannot be a behavioural one: [`build_client`] returns a
    /// `Box<dyn DataClient>`, the five feeds keep the mark-stream switch in a private field, and
    /// the only way to see it work is to subscribe — which dials the venue. What can be held is the
    /// wiring in the arms: with the build's `venue-<v>` features off the arm is not even compiled,
    /// so this reads the function's own source. It fails for the two defects the wiring can have —
    /// an arm that drops the `.with_mark_streams(…)` call (the row would then be stored, shown by
    /// `config show` and read by nothing on the data server), and an arm that reads ANOTHER venue's
    /// row (a copy-paste of the bybit arm into okx's) — and for a Polymarket arm that stops sizing
    /// its shards from the row. `a_venues_mark_stream_row_is_read_under_that_venue_only` is the
    /// lookup's behavioural half.
    #[test]
    fn every_arm_threads_its_own_venues_settings_into_its_own_feed() {
        let body = build_client_source();
        // The arm of `venue`: from its `#[cfg(feature = …)]` match line to the `#[cfg(not(…))]`
        // twin that follows it.
        let arm = |venue: &str| -> &str {
            let start = body
                .find(&format!("#[cfg(feature = \"venue-{venue}\")]"))
                .unwrap_or_else(|| panic!("no `venue-{venue}` arm in build_client"));
            let rest = &body[start..];
            &rest[..rest.find(&format!("#[cfg(not(feature = \"venue-{venue}\"))]")).expect("twin")]
        };
        for venue in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
            let text = arm(venue);
            assert!(
                text.contains(&format!("vike_{venue}::market_feed::Feeds::new(sink")),
                "the `{venue}` arm must build `{venue}`'s own feed:\n{text}"
            );
            assert!(
                text.contains(&format!(
                    ".with_mark_streams(mark_streams_setting(venue_settings, \"{venue}\"))"
                )),
                "the `{venue}` arm must apply `{venue}`'s own mark_streams row:\n{text}"
            );
        }
        let poly = arm("polymarket");
        assert!(
            poly.contains("vike_polymarket::tokens_per_socket(")
                && poly.contains(".get(\"polymarket\")"),
            "the polymarket arm must size its shards from polymarket's own settings rows:\n{poly}"
        );
        assert!(
            poly.contains(".with_tokens_per_socket(k)"),
            "…and hand the result to the feed:\n{poly}"
        );
        // Deribit reads no `venue_setting` row at all, so its arm builds its feed from the sink
        // alone — and must not carry a CEX arm's `mark_streams` wiring (a copy-paste of one).
        let deribit = arm("deribit");
        assert!(
            deribit.contains("vike_deribit::market_feed::Feeds::new(sink"),
            "the `deribit` arm must build deribit's own feed:\n{deribit}"
        );
        assert!(
            !deribit.contains("mark_streams"),
            "deribit has no mark_streams row — its marks stream chain-wide:\n{deribit}"
        );
    }

    /// The lookup half of the pin above, observed: a venue's `mark_streams` row is read for that
    /// venue and no other, and a venue with no row gets `None` (its charter default).
    #[cfg(any(
        feature = "venue-binance",
        feature = "venue-bybit",
        feature = "venue-okx",
        feature = "venue-aster",
        feature = "venue-hyperliquid",
    ))]
    #[test]
    fn a_venues_mark_stream_row_is_read_under_that_venue_only() {
        use vike_secrets::VenueSettingRow;
        use vike_secrets::venue_setting::VenueSettings;
        let row = |venue: &str, value: &str| VenueSettingRow {
            venue: venue.to_string(),
            tier: None,
            field: "MARK_STREAMS".to_string(),
            value: value.to_string(),
        };
        let rows = [row("bybit", "0"), row("okx", "1")];
        let settings: std::collections::BTreeMap<String, VenueSettings> = ["bybit", "okx"]
            .into_iter()
            .map(|v| (v.to_string(), VenueSettings::from_rows(v, &rows)))
            .collect();
        assert_eq!(mark_streams_setting(&settings, "bybit"), Some("0"));
        assert_eq!(mark_streams_setting(&settings, "okx"), Some("1"));
        assert_eq!(mark_streams_setting(&settings, "binance"), None, "no row: the charter default");
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
