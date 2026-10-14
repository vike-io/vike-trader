//! The broker's venue table — the ONE function in this process that constructs a venue
//! market-data client ([`build_client`]), and the declared sharing column ([`sharing_for`]).
//!
//! ⚠ **The client is REMOTE** (decision 0164, `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`):
//! this process opens no venue socket, so every arm builds a
//! `vike_market_stream::market::RemoteDataClient` over the process's one
//! `vike_gatewayhub_client::client::GatewayLink`, and the gatewayhub builds the venue's own client
//! (`crates/vike-gatewayhub/src/data/venues.rs`'s `build_client`, where the arms and the
//! `venue_setting` rows they read live now). What stayed here is what only a CONSUMER can decide:
//! the sharing column ([`EXPLICIT_SHARING`]) and the feature arming, because the broker still decides
//! how many clients a venue gets and which holder a key belongs to (`crate::feeds`' module doc).
//!
//! The client arms moved here from `crate::md::venues`, where each sat behind its `live-<v>`
//! feature. They are gated on `venue-<v>` now, and every `live-<v>` implies its `venue-<v>`
//! (`crates/vike-datahub/Cargo.toml`), because since the broker BOTH planes take their client from
//! this table: a PLANE feature (`live-`, `record-`) says what the process SERVES, a VENUE feature
//! says what it ARMS. Md's own refusal — a venue this build does not serve live — stays md's, in
//! `crates/vike-datahub/src/md/venues.rs`'s `market_builder`, with its message byte-identical.
//!
//! # The shape: one `#[cfg]` arm AND one `#[cfg(not)]` arm per venue
//!
//! The md table's shape, kept. A venue this table knows but this build did not arm is refused
//! naming the feature to rebuild with; a venue it does not know at all is refused naming the table.
//! Two different messages, because "unknown venue" sends somebody hunting for a typo when the answer
//! is a rebuild. The paired arms also mean the `match` never collapses to a single binding arm on a
//! build that arms no venue.
//!
//! # A constructor opens nothing
//!
//! `RemoteDataClient::new` never dials on the caller's thread (its reader thread does), so a
//! gatewayhub that is down when the broker builds a client is a link that is down, not a failed
//! build: the first `subscribe_*` answers a TRANSIENT `Err(Subscribe)`, which the recorder retries on
//! its next pass (`vike_recorder::session::SubscriptionSet` keeps that apart from the permanent
//! `Unsupported`), and the process neither exits nor crash-loops. The only `Err` a linked arm can
//! return is the OS refusing the reader thread.
use std::sync::Arc;

use vike_data::{DataClient, LiveDataSink};
use vike_gatewayhub_client::client::GatewayLink;
// The remote client, named only by the arms that build it: a build arming no venue carries no
// unused import (and no unused `remote_client` below).
#[cfg(any(
    feature = "venue-binance",
    feature = "venue-bybit",
    feature = "venue-okx",
    feature = "venue-aster",
    feature = "venue-hyperliquid",
    feature = "venue-polymarket",
    feature = "venue-deribit",
))]
use vike_gatewayhub_client::opener::GatewayOpener;
#[cfg(any(
    feature = "venue-binance",
    feature = "venue-bybit",
    feature = "venue-okx",
    feature = "venue-aster",
    feature = "venue-hyperliquid",
    feature = "venue-polymarket",
    feature = "venue-deribit",
))]
use vike_market_stream::market::RemoteDataClient;

use super::{ClientBuilder, Sharing};

/// Build one venue's market-data client, or say WHY not.
///
/// The client is a `RemoteDataClient` for `venue` over `link`, delivering into `sink`: one stream
/// socket to the gatewayhub, one venue client there. The `|| {}` repaint nudge and the
/// `venue_setting` rows (Polymarket's socket batching, each CEX feed's mark-stream row, decision
/// 0095) are the gatewayhub's now, applied when it builds the venue's client.
// `sink` and `link` are consumed by every venue arm, so ONE condition — the seven venues — covers
// both unused-parameter cases: a build that arms none of them (no venue at all, the
// `light-consumers` feature suite) leaves both unread. ⚠ There was a second
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
        feature = "venue-deribit",
    )),
    allow(unused_variables, clippy::allow_attributes)
)]
pub fn build_client(
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
    link: &GatewayLink,
) -> Result<Box<dyn DataClient + Send>, String> {
    match venue {
        #[cfg(feature = "venue-binance")]
        "binance" => remote_client(link, "binance", sink),
        #[cfg(not(feature = "venue-binance"))]
        "binance" => Err(not_linked("binance")),

        #[cfg(feature = "venue-bybit")]
        "bybit" => remote_client(link, "bybit", sink),
        #[cfg(not(feature = "venue-bybit"))]
        "bybit" => Err(not_linked("bybit")),

        #[cfg(feature = "venue-okx")]
        "okx" => remote_client(link, "okx", sink),
        #[cfg(not(feature = "venue-okx"))]
        "okx" => Err(not_linked("okx")),

        #[cfg(feature = "venue-aster")]
        "aster" => remote_client(link, "aster", sink),
        #[cfg(not(feature = "venue-aster"))]
        "aster" => Err(not_linked("aster")),

        #[cfg(feature = "venue-hyperliquid")]
        "hyperliquid" => remote_client(link, "hyperliquid", sink),
        #[cfg(not(feature = "venue-hyperliquid"))]
        "hyperliquid" => Err(not_linked("hyperliquid")),

        #[cfg(feature = "venue-polymarket")]
        "polymarket" => remote_client(link, "polymarket", sink),
        #[cfg(not(feature = "venue-polymarket"))]
        "polymarket" => Err(not_linked("polymarket")),

        #[cfg(feature = "venue-deribit")]
        "deribit" => remote_client(link, "deribit", sink),
        #[cfg(not(feature = "venue-deribit"))]
        "deribit" => Err(not_linked("deribit")),

        other => Err(format!(
            "feed broker: venue `{other}` has no client in this process's venue table. Table: [{}]",
            CLIENT_VENUES.join(", ")
        )),
    }
}

/// The one place a remote client is made: `venue`'s stream session at the gatewayhub `link` names.
///
/// Every arm passes its OWN slug here as a literal, so an arm that built another venue's session
/// would be a copy-paste the structural pin cannot see and
/// `every_arm_asks_the_gatewayhub_for_its_own_venue` can.
#[cfg(any(
    feature = "venue-binance",
    feature = "venue-bybit",
    feature = "venue-okx",
    feature = "venue-aster",
    feature = "venue-hyperliquid",
    feature = "venue-polymarket",
    feature = "venue-deribit",
))]
fn remote_client(
    link: &GatewayLink,
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    RemoteDataClient::new(GatewayOpener::new(link.clone()), venue, sink)
        .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
        .map_err(|e| {
            format!(
                "feed broker: venue `{venue}`: the gatewayhub client could not start its reader \
                 thread: {e}"
            )
        })
}

/// The table [`super::FeedBroker::new`] takes in production — every client it builds dials the
/// gatewayhub `link` names (decision 0164), and `link` is cloned into the table (the hub keys are
/// shared, not copied).
pub fn real_client_table(link: &GatewayLink) -> ClientBuilder {
    let link = link.clone();
    Box::new(move |venue, sink| build_client(venue, sink, &link))
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
// is neither, and a 60-line module has no argument for leaving its file.
#[cfg(test)]
mod tests {
    use super::*;

    use vike_data::NoopSink;
    use vike_hub_proto::auth::HubKeys;

    /// A link to nothing: a port nobody listens on and no key. A client over it is a link that is
    /// down, which is all a test that only BUILDS clients needs.
    fn unreachable_link() -> GatewayLink {
        GatewayLink::new("127.0.0.1:1", HubKeys::new(Vec::new(), Vec::new()))
    }

    /// `build_client`'s own source text, from its signature to its closing brace — what the
    /// structural pin below reads, because with a build's `venue-<v>` features off an arm is not
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
                    !vike_venues::caps_for(v).live_data.book,
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
        assert!(vike_venues::caps_for("deribit").live_data.book);
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
    /// all-features `datahub-all` job, now that md's own `a_supported_venue_builds_a_client` reaches a broker
    /// handle rather than a real client.
    #[test]
    fn every_client_venue_builds_or_names_its_feature() {
        let link = unreachable_link();
        for v in CLIENT_VENUES {
            if let Err(e) = build_client(v, Arc::new(NoopSink), &link) {
                assert!(e.contains(&format!("--features venue-{v}")), "{e}");
                assert!(!e.contains("Table:"), "a compiled-out venue is not an unknown one: {e}");
            }
        }
    }

    /// **Each arm asks the gatewayhub for its OWN venue** (decision 0095's pin, re-aimed: the
    /// `mark_streams` and `ws_tokens_per_socket` rows are the gatewayhub's to thread now, and
    /// `crates/vike-gatewayhub/src/data/venues_tests.rs`'s
    /// `every_arm_threads_its_own_venues_settings_into_its_own_feed` holds them there).
    ///
    /// What is left to get wrong HERE is a copy-paste: an arm that names another venue's slug
    /// (`"okx" => remote_client(link, "bybit", sink)`) builds a client that compiles, passes
    /// `every_client_venue_builds_or_names_its_feature` and subscribes the wrong venue's
    /// symbols. Unlike the old bridge arms, a remote client can be watched: this subscribes through
    /// every built client against a loopback gatewayhub and reads back which venue the daemon was
    /// asked to open, in the order the clients were built.
    ///
    /// Both ways: the venues that BUILD are exactly the ones whose `venue-<v>` feature is on, so a
    /// build that cannot reach an arm (a stale `#[cfg]`) fails here too. On a build arming no venue
    /// this asserts nothing about an arm; `datahub-all` is where it has teeth.
    #[test]
    fn every_arm_asks_the_gatewayhub_for_its_own_venue() {
        use vike_gatewayhub::testkit::data::ScriptedData;
        use vike_gatewayhub::testkit::gateway::LoopbackGateway;
        use vike_gatewayhub::testkit::rest::ScriptedRest;

        let armed: Vec<&str> = [
            ("binance", cfg!(feature = "venue-binance")),
            ("bybit", cfg!(feature = "venue-bybit")),
            ("okx", cfg!(feature = "venue-okx")),
            ("aster", cfg!(feature = "venue-aster")),
            ("hyperliquid", cfg!(feature = "venue-hyperliquid")),
            ("polymarket", cfg!(feature = "venue-polymarket")),
            ("deribit", cfg!(feature = "venue-deribit")),
        ]
        .into_iter()
        .filter_map(|(venue, on)| on.then_some(venue))
        .collect();

        // The gateway is declared BEFORE the clients, so they drop first (the testkit's rule).
        let data = ScriptedData::new(&CLIENT_VENUES);
        let gateway = LoopbackGateway::start(data.clone(), ScriptedRest::new());
        let link = gateway.link();
        let mut clients = Vec::new();
        let mut built = Vec::new();
        for v in CLIENT_VENUES {
            if let Ok(mut client) = build_client(v, Arc::new(NoopSink), &link) {
                client.subscribe_trades("PROBE").unwrap_or_else(|e| panic!("`{v}`: {e}"));
                built.push(v);
                clients.push(client);
            }
        }
        assert_eq!(built, armed, "the venues that build are the venues whose feature is on");
        let asked: Vec<String> = data.sessions().iter().map(|s| s.venue.clone()).collect();
        assert_eq!(asked, built, "each arm must open a session for its OWN venue at the daemon");
    }

    /// An UNKNOWN venue is refused naming the table — never a feature that does not exist.
    #[test]
    fn an_unknown_venue_names_the_table_not_a_feature() {
        let err = match build_client("kalshi", Arc::new(NoopSink), &unreachable_link()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(err.contains("kalshi") && err.contains("Table:"), "{err}");
        assert!(!err.contains("--features"), "{err}");
    }
}
