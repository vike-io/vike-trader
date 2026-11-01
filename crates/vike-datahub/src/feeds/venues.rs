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
// The remote client: named by the arms that build it AND by `build_ticks_client`, which every build
// compiles (the strategy lane's pump table), so both imports are unconditional.
use vike_gatewayhub_client::opener::GatewayOpener;
use vike_market_stream::market::RemoteDataClient;

use super::{ClientBuilder, Sharing};

/// Build one venue's market-data client, or say WHY not.
///
/// `label` is the broker's name for the client ([`client_label`]): a venue, or a venue and the network
/// it is opened on (`hyperliquid@testnet`, the strategy lane's alone). The client is a
/// `RemoteDataClient` for the venue over `link`, delivering into `sink`: one stream socket to the
/// gatewayhub, one venue client there. The `|| {}` repaint nudge and the `venue_setting` rows
/// (Polymarket's socket batching, each CEX feed's mark-stream row, decision 0095) are the gatewayhub's
/// now, applied when it builds the venue's client.
///
/// ⚠ The credentialed venues ([`CREDENTIALED_VENUES`]) are built here too and ask the gatewayhub for a
/// CREDENTIALED stream (the datahub's control key); this process reads no credential for them. The
/// market-data hub never asks this table for one (md's own roster refuses them), and the recorder asks
/// only for a venue [`EXPLICIT_SHARING`] lists (OANDA so far, decision 0193): the others stay the
/// strategy lane's alone.
pub fn build_client(
    label: &str,
    sink: Arc<dyn LiveDataSink>,
    link: &GatewayLink,
) -> Result<Box<dyn DataClient + Send>, String> {
    let (venue, network) = split_label(label);
    if let Some(network) = network {
        return network_client(link, venue, network, label, sink);
    }
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

        // The CREDENTIALED venues (`CREDENTIALED_VENUES`): the strategy lane's, and the recorder's for the
        // ones `EXPLICIT_SHARING` lists.
        other if CREDENTIALED_VENUES.contains(&other) => credentialed_client(link, other, sink),

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

/// The character a broker name carries a non-main network after: `hyperliquid@testnet`.
const NETWORK_SEP: char = '@';

/// The name the broker (and the strategy lane's sink) know a venue client by: the venue, and its
/// network when it is not the venue's main one. The broker keys its clients by this string, so a
/// Hyperliquid test-network client is never the main-network one, never shared with market data or the
/// recorder (it is not in [`EXPLICIT_SHARING`], so it is `PerHolder`), and its frames are routed by
/// their own name ([`split_label`] is the way back).
pub fn client_label(venue: &str, network: Option<&str>) -> String {
    match network {
        Some(network) => format!("{venue}{NETWORK_SEP}{network}"),
        None => venue.to_string(),
    }
}

/// [`client_label`] backwards: the venue and the network a broker name stands for.
pub fn split_label(label: &str) -> (&str, Option<&str>) {
    match label.split_once(NETWORK_SEP) {
        Some((venue, network)) => (venue, Some(network)),
        None => (label, None),
    }
}

/// The opener that opens its venue's session under another name than the one the engine delivers
/// under: the engine labels every sink call with the broker name (`hyperliquid@testnet`), the
/// gatewayhub knows the venue (`hyperliquid`). Compiled where the one venue that takes a network is.
#[cfg(feature = "venue-hyperliquid")]
mod as_venue {
    use std::io;
    use std::net::TcpStream;

    use vike_market_stream::opener::{DataStream, Opener, StreamLimits, StreamTimings};

    pub(super) struct AsVenue<O> {
        pub(super) inner: O,
        pub(super) venue: String,
    }

    impl<O: Opener> Opener for AsVenue<O> {
        fn addr(&self) -> &str {
            self.inner.addr()
        }
        fn limits(&self) -> StreamLimits {
            self.inner.limits()
        }
        fn timings(&self) -> StreamTimings {
            self.inner.timings()
        }
        fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
            self.inner.failure_text(e)
        }
        fn open(&self, _label: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<DataStream> {
            self.inner.open(&self.venue, observe)
        }
    }
}

/// The one venue whose feed follows the account tier, so the one that takes a network.
pub const NETWORK_VENUE: &str = "hyperliquid";

/// A client for `venue` on a non-main `network`: a stream session that NAMES the network on `DataOpen`
/// (`GatewayOpener::strategy`), so the gatewayhub builds the venue's client for that network. Only
/// Hyperliquid takes one; any other venue is refused rather than ignored (a silently ignored network
/// would put a test-network strategy on a mainnet feed). `label` is the broker's name for it.
fn network_client(
    link: &GatewayLink,
    venue: &str,
    network: &str,
    label: &str,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    if venue != NETWORK_VENUE {
        return Err(format!(
            "feed broker: venue `{venue}` takes no network (asked for `{network}`); only \
             {NETWORK_VENUE} has a feed that follows the account tier"
        ));
    }
    #[cfg(feature = "venue-hyperliquid")]
    {
        let opener = as_venue::AsVenue {
            inner: GatewayOpener::strategy(link.clone(), Some(network.to_string()), false, false),
            venue: venue.to_string(),
        };
        RemoteDataClient::new(opener, label, sink)
            .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
            .map_err(|e| {
                format!(
                    "feed broker: venue `{venue}` on `{network}`: the gatewayhub client could not \
                     start its reader thread: {e}"
                )
            })
    }
    #[cfg(not(feature = "venue-hyperliquid"))]
    {
        let _ = (link, label, sink);
        Err(not_linked(venue))
    }
}

// vike:new-venue:note do NOT add `{venue}` to `CREDENTIALED_VENUES` by copying a neighbour: the list is the datahub's copy of the gatewayhub's `Lane::CredentialedStream` roster (`crates/vike-gatewayhub/src/registry/mod.rs`'s `ROSTERS`), so the venue's credentialed stream (an arm of `crates/vike-gatewayhub/src/data/credentialed.rs`'s `build_credentialed`) must exist first and the two lists must stay equal and in the same order: crates/vike-datahub/src/feeds/venues.rs's `CREDENTIALED_VENUES`
/// The venues whose live stream the gatewayhub logs in for, as the account the strategy trades
/// (alpaca, ctrader, ig, oanda), the account whose IB Gateway session prices a stock the engine was not
/// mounted on (ibkr: the any-symbol PRICE stream, lazily dialled, quotes only) or what the sidecar the trade lane
/// already started serves (dukascopy: the any-symbol PRICE stream, which logs in to nothing,
/// `crates/vike-gatewayhub/src/data/dukascopy_quotes.rs`; fxcm's, which logs in to nothing either and polls the
/// Offers table of the account's running reconcile session,
/// `crates/vike-gatewayhub/src/data/fxcm_quotes.rs`): the strategy lane's, never market data's, and the
/// recorder's only for a venue [`EXPLICIT_SHARING`] lists (decision 0193). The
/// datahub asks for the stream under its CONTROL key (`Scope::Write`, `cs_venue=`) and reads no
/// credential, label or tier: the gatewayhub reads the account row itself
/// (`crates/vike-gatewayhub/src/data/accounts.rs`), and a venue with no usable credentials is the
/// gatewayhub's refusal of the open. No `venue-<v>` marker: a client for these links nothing here.
pub const CREDENTIALED_VENUES: [&str; 7] =
    ["alpaca", "ctrader", "ig", "oanda", "ibkr", "dukascopy", "fxcm"];

/// A client for the credentialed `venue`: a CREDENTIALED stream session at the gatewayhub (`Write`
/// scope, `cs_venue=`), delivering into `sink`.
fn credentialed_client(
    link: &GatewayLink,
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    RemoteDataClient::new(GatewayOpener::strategy(link.clone(), None, true, false), venue, sink)
        .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
        .map_err(|e| {
            format!(
                "feed broker: venue `{venue}`: the gatewayhub client could not start its reader \
                 thread: {e}"
            )
        })
}

/// The venues whose feed is a CEX tick PUMP (`spawn_*_market_data`): the only ones the strategy lane's
/// `Ticks` op is asked of. Not a capability the broker has (the broker brokers `DataClient` lanes and
/// the pump is not one, `vike_market_stream::wire::SubOp::Ticks`), so the table lives with the venue
/// table and the lane's `Ticks` handler refuses any other venue by this list.
pub const PUMP_VENUES: [&str; 4] = ["binance", "bybit", "okx", "aster"];

/// The venues the STRATEGY LANE serves in this build - the `strat_venue=` advertisement's source of
/// truth. The `venue-<v>` markers decide, not the `live-<v>` ones: a venue's remote arm compiles under
/// `venue-<v>`, and the lane needs the arm and nothing of the market-data plane (a `record-` build
/// serves the lane's prices too). The four CREDENTIALED venues ([`CREDENTIALED_VENUES`]) are served
/// whatever the build's features: their arm links nothing, and whether the gatewayhub can open them
/// (its `cs_venue=` entry, a usable account) is its answer at the open, which the lane relays.
pub fn strat_venues() -> Vec<&'static str> {
    let mut venues = vec![
        #[cfg(feature = "venue-binance")]
        "binance",
        #[cfg(feature = "venue-bybit")]
        "bybit",
        #[cfg(feature = "venue-okx")]
        "okx",
        #[cfg(feature = "venue-aster")]
        "aster",
        #[cfg(feature = "venue-hyperliquid")]
        "hyperliquid",
        #[cfg(feature = "venue-polymarket")]
        "polymarket",
        #[cfg(feature = "venue-deribit")]
        "deribit",
    ];
    venues.extend(CREDENTIALED_VENUES);
    venues
}

/// A tick-pump client for `venue`: the concrete [`RemoteDataClient`] (whose inherent `subscribe_ticks`
/// is the pump verb) over `link`, delivering into `sink`. One per strategy-lane SESSION, never shared
/// and never routed through the broker: a pump's frames are one tradehub's, sized to that tradehub's
/// grid, so `sink` is the session's own wire sink.
///
/// `Err` names why not, the way [`build_client`]'s refusals do: a venue with no pump, a venue this
/// build does not arm.
pub fn build_ticks_client(
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
    link: &GatewayLink,
) -> Result<RemoteDataClient, String> {
    if !PUMP_VENUES.contains(&venue) {
        return Err(format!(
            "strategy lane: venue `{venue}` has no tick pump. The pump is the CEX feed of [{}]",
            PUMP_VENUES.join(", ")
        ));
    }
    if !strat_venues().contains(&venue) {
        return Err(format!(
            "strategy lane: venue `{venue}` is not linked into this build - rebuild with \
             `--features venue-{venue}` (or a plane feature that implies it)"
        ));
    }
    // A pump session: the gatewayhub must advertise `gw_pump` (the client checks it locally and says so),
    // so a daemon that cannot host the pump fails the open by name instead of serving a session whose
    // `Ticks` it would refuse.
    RemoteDataClient::new(GatewayOpener::strategy(link.clone(), None, false, true), venue, sink)
        .map_err(|e| {
            format!(
                "strategy lane: venue `{venue}`: the gatewayhub client could not start its reader \
                 thread: {e}"
            )
        })
}

/// The tick-pump table [`crate::strat::StratHub`] takes in production: [`build_ticks_client`] over the
/// process's one gatewayhub `link`.
pub fn real_ticks_table(link: &GatewayLink) -> crate::strat::TicksBuilder {
    let link = link.clone();
    Box::new(move |venue, sink| build_ticks_client(venue, sink, &link))
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
/// **OANDA is the first credentialed row** (2026-10-20, decision 0193): its `unsubscribe` is
/// `FeedRegistry::stop_join` of one id, and each `subscribe_quotes` spawns exactly one registry thread
/// holding exactly one `/pricing/stream`, so releasing a key ends that key's stream and no other
/// (`crates/bridges/oanda/src/market_feed_tests.rs`'s
/// `releasing_one_quote_key_ends_exactly_that_pricing_stream_and_no_other` holds it). The REST
/// first-price lane is one thread for every pair and an unsubscribe never touches it. The other six
/// credentialed venues stay unlisted, hence `PerHolder`, until each is read the same way in its own
/// slice (cTrader's `UnsubscribeSpots` has no per-symbol count yet; dukascopy and fxcm MUST be
/// `Shared`, their bus has one listener).
///
/// ⚠ **Deribit is `PerHolder` for a DIFFERENT reason, and not a measured one.** Its `unsubscribe`
/// is the CEX shape (`FeedRegistry::stop_join`, one thread per subscription), so that column would
/// say `Shared` — but it declares a LOSSLESS book lane (`live_data.book`), and
/// `no_shared_venue_serves_a_lossless_book_lane` forbids sharing one: a recorder joining a live
/// book mid-chain would get deltas with no anchor. Nothing records deribit today, so the cost of
/// the safe direction is nil; the day something does, that test is where the exception is argued.
pub const EXPLICIT_SHARING: [(&str, Sharing); 8] = [
    ("binance", Sharing::Shared),
    ("bybit", Sharing::Shared),
    ("okx", Sharing::Shared),
    ("aster", Sharing::Shared),
    ("hyperliquid", Sharing::Shared),
    ("polymarket", Sharing::PerHolder),
    ("deribit", Sharing::PerHolder),
    ("oanda", Sharing::Shared),
];

/// ⚠ The fallback is `PerHolder` — two clients, the behaviour before the broker — because it is the
/// direction that cannot corrupt a tape. A venue earns `Shared` only by a row above, argued from its
/// own `unsubscribe`.
pub fn sharing_for(venue: &str) -> Sharing {
    EXPLICIT_SHARING.iter().find(|(name, _)| *name == venue).map_or(Sharing::PerHolder, |(_, s)| *s)
}

/// Whether `venue`'s QUOTE stream gives its first price to a NEW stream only: the venue's on-connect
/// price snapshot and the bridge's first-price read both belong to the start of the stream's thread, so
/// a subscriber that joins an OPEN stream waits for the venue's next price, which on a quiet pair is long.
/// OANDA is the one such venue the broker shares (decision 0193): a strategy that joins a quote key only
/// the recorder holds, and that has gone quiet, is given a fresh stream
/// (`FeedBroker::acquire`'s rule eight).
pub fn first_price_comes_with_a_new_stream(venue: &str) -> bool {
    venue == "oanda"
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

    /// **OANDA is the one credentialed venue that shares** (decision 0193), on the reading of its
    /// `unsubscribe` that `crates/bridges/oanda/src/market_feed_tests.rs`'s
    /// `releasing_one_quote_key_ends_exactly_that_pricing_stream_and_no_other` pins; the other six keep
    /// the safe fallback until their own slice. It declares no lossless book lane, which
    /// `no_shared_venue_serves_a_lossless_book_lane` checks for every shared row.
    #[test]
    fn oanda_is_the_one_credentialed_venue_that_shares() {
        assert_eq!(sharing_for("oanda"), crate::feeds::Sharing::Shared);
        for v in CREDENTIALED_VENUES.into_iter().filter(|v| *v != "oanda") {
            assert_eq!(
                sharing_for(v),
                crate::feeds::Sharing::PerHolder,
                "`{v}` shares only on a reading of its own `unsubscribe`, written in its own slice"
            );
        }
    }

    /// The credentialed limits table (`vike_gatewayhub_client::credentialed_limits::ROWS`) names exactly
    /// the credentialed venues this table builds, in this order: a venue added to one list fails here
    /// until the other has decided its row.
    #[test]
    fn the_stream_limits_table_names_the_credentialed_venues_of_this_table() {
        let rows: Vec<&str> = vike_gatewayhub_client::credentialed_limits::ROWS
            .iter()
            .map(|(venue, _)| *venue)
            .collect();
        assert_eq!(rows, CREDENTIALED_VENUES);
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
        // A credentialed venue links nothing here (`CREDENTIALED_VENUES`: no `venue-<v>` marker, one
        // shared arm, no market-data row), so it has none of these sites.
        for v in CLIENT_VENUES.into_iter().filter(|v| !CREDENTIALED_VENUES.contains(v)) {
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
        // The keyless venues only: a credentialed one asks for a `cs_venue=` session this scripted lane
        // does not serve (`strat_gateway.rs` drives those through a lane that does).
        let keyless: Vec<&str> =
            CLIENT_VENUES.into_iter().filter(|v| !CREDENTIALED_VENUES.contains(v)).collect();
        let data = ScriptedData::new(&keyless);
        let gateway = LoopbackGateway::start(data.clone(), ScriptedRest::new());
        let link = gateway.link();
        let mut clients = Vec::new();
        let mut built = Vec::new();
        for v in keyless {
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

    /// **A test-network client is never shared, a credentialed venue is the market-data plane's never,
    /// and the recorder's only where the table lists it.** The broker names a network client
    /// `venue@network`, which is no row of [`EXPLICIT_SHARING`], so it falls to the safe `PerHolder`
    /// (never the market-data plane's or the recorder's client). The credentialed venues are in no
    /// market-data roster (`supported`) until the operator enables one (decision 0193 verdict 8, a later
    /// slice), every one is the strategy lane's, and only the ones with a sharing row are in the client
    /// table (`CLIENT_VENUES`) the recorder reaches.
    #[test]
    fn a_network_client_is_never_shared_and_a_credentialed_venue_is_the_recorders_only_on_a_row() {
        assert_eq!(
            sharing_for(&client_label("hyperliquid", Some("testnet"))),
            crate::feeds::Sharing::PerHolder
        );
        assert_eq!(sharing_for("hyperliquid"), crate::feeds::Sharing::Shared);
        for v in CREDENTIALED_VENUES {
            assert_eq!(
                CLIENT_VENUES.contains(&v),
                v == "oanda",
                "`{v}`: a credentialed venue is a client venue exactly when it has a sharing row"
            );
            assert!(!crate::md::venues::supported().contains(&v), "`{v}` must not be served by md");
            assert!(strat_venues().contains(&v), "`{v}` is the strategy lane's");
        }
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
