//! The broker's venue table — the ONE function in this process that constructs a venue
//! market-data client ([`build_client`]), and the declared sharing column ([`sharing_for`]).
//!
//! ⚠ **The client is REMOTE** (decision 0164, `docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md`):
//! this process opens no venue socket, so every arm builds a
//! `vike_market_stream::market::DataClientStream` over the process's one
//! `vike_gatewayhub_client::client::GatewayLink`, and the gatewayhub builds the venue's own client
//! (`crates/vike-gatewayhub/src/data/venues.rs`'s `build_client`, where the arms and the
//! `venue_setting` rows they read live now). What stayed here is what only a CONSUMER can decide:
//! the sharing column ([`EXPLICIT_SHARING`]), because the broker still decides how many clients a venue
//! gets and which holder a key belongs to (`crate::feeds`' module doc).
//!
//! The client arms moved here from `crate::market_data::venues`, because since the broker BOTH planes take their
//! client from this table. MarketData's own refusal — a venue that plane does not serve — stays market data's, in
//! `crates/vike-datahub/src/market_data/venues.rs`'s `market_builder`.
//!
//! # A constructor opens nothing
//!
//! `DataClientStream::new` never dials on the caller's thread (its reader thread does), so a
//! gatewayhub that is down when the broker builds a client is a link that is down, not a failed
//! build: the first `subscribe_*` answers a TRANSIENT `Err(Subscribe)`, which the recorder retries on
//! its next pass (`vike_recorder::session::SubscriptionSet` keeps that apart from the permanent
//! `Unsupported`), and the process neither exits nor crash-loops. The only `Err` a venue's arm can
//! return is the OS refusing the reader thread.
use std::sync::Arc;
use vike_data::DataClientBuilder;

use vike_data::{DataClient, DataClientSink};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES;
use vike_gatewayhub_client::history::{FEED_TICK_VENUES, KEYLESS_STREAM_VENUES, network_venues};
use vike_gatewayhub_client::opener::StreamOpenerGateway;
use vike_market_stream::market::DataClientStream;

use super::DataClientBrokerSharing;

/// Build one venue's market-data client, or say WHY not.
///
/// `label` is the broker's name for the client ([`client_label`]): a venue, or a venue and the network
/// it is opened on (`hyperliquid@testnet`, the strategy lane's alone). The client is a
/// `DataClientStream` for the venue over `link`, delivering into `sink`: one stream socket to the
/// gatewayhub, one venue client there. The `|| {}` repaint nudge and the `venue_setting` rows
/// (Polymarket's socket batching, each CEX feed's mark-stream row, decision 0095) are the gatewayhub's
/// now, applied when it builds the venue's client.
///
/// ⚠ The credentialed venues ([`CREDENTIALED_VENUES`]) are built here too and ask the gatewayhub for a
/// CREDENTIALED stream (the datahub's control key); this process reads no credential for them. The
/// market-data hub never causes one to be built (`super::FeedBroker`'s rule nine), except for a key the
/// operator's own resident row names, and the recorder asks only for a venue [`EXPLICIT_SHARING`] lists
/// (decision 0193): one with no row stays the strategy lane's alone.
pub fn build_client(
    label: &str,
    sink: Arc<dyn DataClientSink>,
    link: &GatewayLink,
) -> Result<Box<dyn DataClient + Send>, String> {
    let (venue, network) = split_label(label);
    if let Some(network) = network {
        return network_client(link, venue, network, label, sink);
    }
    match venue {
        // The CREDENTIALED venues (`CREDENTIALED_VENUES`): the strategy lane's, and the recorder's for the
        // ones `EXPLICIT_SHARING` lists.
        other if CREDENTIALED_VENUES.contains(&other) => credentialed_client(link, other, sink),
        other if CLIENT_VENUES.contains(&other) => remote_client(link, other, sink),
        other => Err(format!(
            "feed broker: venue `{other}` has no client in this process's venue table. Table: [{}]",
            CLIENT_VENUES.join(", ")
        )),
    }
}

/// The one place a remote client is made: `venue`'s stream session at the gatewayhub `link` names.
fn remote_client(
    link: &GatewayLink,
    venue: &str,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    DataClientStream::new(StreamOpenerGateway::new(link.clone()), venue, sink)
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
/// gatewayhub knows the venue (`hyperliquid`).
mod as_venue {
    use std::io;
    use std::net::TcpStream;

    use vike_market_stream::opener::{StreamConnection, StreamLimits, StreamOpener, StreamTimings};

    pub(super) struct StreamOpenerVenue<O> {
        pub(super) inner: O,
        pub(super) venue: String,
    }

    impl<O: StreamOpener> StreamOpener for StreamOpenerVenue<O> {
        fn addr(&self) -> &str {
            self.inner.addr()
        }
        fn peer(&self) -> &'static str {
            self.inner.peer()
        }
        fn limits(&self) -> StreamLimits {
            self.inner.limits()
        }
        fn timings(&self) -> StreamTimings {
            self.inner.timings()
        }
        fn failure_text(&self, e: &std::io::Error) -> String {
            self.inner.failure_text(e)
        }
        fn open(&self, _label: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<StreamConnection> {
            self.inner.open(&self.venue, observe)
        }
    }
}

/// A client for `venue` on a non-main `network`: a stream session that NAMES the network on `DataOpen`
/// (`StreamOpenerGateway::strategy`), so the gatewayhub builds the venue's client for that network. Only
/// Hyperliquid takes one; any other venue is refused rather than ignored (a silently ignored network
/// would put a test-network strategy on a mainnet feed). `label` is the broker's name for it.
fn network_client(
    link: &GatewayLink,
    venue: &str,
    network: &str,
    label: &str,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    if !network_venues().contains(&venue) {
        return Err(format!(
            "feed broker: venue `{venue}` takes no network (asked for `{network}`); only \
             {} has a feed that follows the account tier",
            network_venues().join(", ")
        ));
    }
    let opener = as_venue::StreamOpenerVenue {
        inner: StreamOpenerGateway::strategy(link.clone(), Some(network.to_string()), false, false),
        venue: venue.to_string(),
    };
    DataClientStream::new(opener, label, sink)
        .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
        .map_err(|e| {
            format!(
                "feed broker: venue `{venue}` on `{network}`: the gatewayhub client could not \
                 start its reader thread: {e}"
            )
        })
}

/// A client for the credentialed `venue`: a CREDENTIALED stream session at the gatewayhub (`Write`
/// scope, `cs_venue=`), delivering into `sink`.
fn credentialed_client(
    link: &GatewayLink,
    venue: &str,
    sink: Arc<dyn DataClientSink>,
) -> Result<Box<dyn DataClient + Send>, String> {
    DataClientStream::new(
        StreamOpenerGateway::strategy(link.clone(), None, true, false),
        venue,
        sink,
    )
    .map(|client| Box::new(client) as Box<dyn DataClient + Send>)
    .map_err(|e| {
        format!(
            "feed broker: venue `{venue}`: the gatewayhub client could not start its reader \
                 thread: {e}"
        )
    })
}

/// The venues the STRATEGY LANE serves - the `strategy_venue=` advertisement's source of truth: the keyless
/// stream roster ([`KEYLESS_STREAM_VENUES`]), then the CREDENTIALED venues ([`CREDENTIALED_VENUES`]), with or
/// without a sharing row: whether the gatewayhub can open one (its `cs_venue=` entry, a usable account) is its
/// answer at the open, which the lane relays.
pub fn strategy_venues() -> Vec<&'static str> {
    let mut venues = KEYLESS_STREAM_VENUES.to_vec();
    venues.extend(CREDENTIALED_VENUES);
    venues
}

/// A tick-pump client for `venue`: the concrete [`DataClientStream`] (whose inherent `subscribe_ticks`
/// is the pump verb) over `link`, delivering into `sink`. One per strategy-lane SESSION, never shared
/// and never routed through the broker: a pump's frames are one tradehub's, sized to that tradehub's
/// grid, so `sink` is the session's own wire sink.
///
/// `Err` names why not, the way [`build_client`]'s refusals do: a venue with no pump.
pub fn build_ticks_client(
    venue: &str,
    sink: Arc<dyn DataClientSink>,
    link: &GatewayLink,
) -> Result<DataClientStream, String> {
    if !FEED_TICK_VENUES.contains(&venue) {
        return Err(format!(
            "strategy lane: venue `{venue}` has no tick pump. The pump is the CEX feed of [{}]",
            FEED_TICK_VENUES.join(", ")
        ));
    }
    // A pump session: the gatewayhub must advertise `gateway_feed_tick` (the client checks it locally and says so),
    // so a daemon that cannot host the pump fails the open by name instead of serving a session whose
    // `Ticks` it would refuse.
    DataClientStream::new(
        StreamOpenerGateway::strategy(link.clone(), None, false, true),
        venue,
        sink,
    )
    .map_err(|e| {
        format!(
            "strategy lane: venue `{venue}`: the gatewayhub client could not start its reader \
                 thread: {e}"
        )
    })
}

/// The tick-pump table [`crate::strat::StrategyLane`] takes in production: [`build_ticks_client`] over the
/// process's one gatewayhub `link`.
pub fn real_ticks_table(link: &GatewayLink) -> crate::strat::StrategyLaneTicksBuilder {
    let link = link.clone();
    Box::new(move |venue, sink| build_ticks_client(venue, sink, &link))
}

/// The table [`super::FeedBroker::new`] takes in production — every client it builds dials the
/// gatewayhub `link` names (decision 0164), and `link` is cloned into the table (the hub keys are
/// shared, not copied).
pub fn real_client_table(link: &GatewayLink) -> DataClientBuilder {
    let link = link.clone();
    Box::new(move |venue, sink| build_client(venue, sink, &link))
}

/// The venues [`build_client`] builds a client for by name — DERIVED from
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

// vike:new-venue:note do NOT give `{venue}` a row of `EXPLICIT_SHARING` by copying a neighbour: a row arms the venue's whole path (one client the strategy lane, the recorder and market data share), so it is written from a reading of the venue bridge's `unsubscribe` and pinned by a bridge test its comment names, and a credentialed venue's stream is its row of `crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s `ROWS`, which must exist first: crates/vike-datahub/src/feeds/venues.rs's `EXPLICIT_SHARING`
/// THE client-venue table: one row per venue [`build_client`] builds by name, carrying its sharing. A row
/// is all this crate asks of a venue: the arm follows from it. Each plane's list of keyless venues is the
/// client crate's roster ([`KEYLESS_STREAM_VENUES`]), and `every_arm_asks_the_gatewayhub_for_its_own_venue`
/// builds every one of them, so a roster venue with no row here fails a test.
///
/// MEASURED from each bridge's `DataClient::unsubscribe` (2026-09-28): the five CEX stop and join
/// exactly one thread per subscription; polymarket releases a SEAT and resubscribes the shard's
/// other tokens, which would punch a gap into a co-tenant holder's tape.
///
/// **A credentialed row arms the venue's whole path** (decision 0193): the recorder follows the strategy
/// lane's quote keys on it (`crate::recording::strategy_followed`) and all three holders share its one client.
/// So it is written from a reading of the bridge's `unsubscribe`, pinned by a bridge test the row's comment
/// names, and it needs a row of `vike_gatewayhub_client::credentialed_limits::ROWS` beside it
/// (`a_credentialed_venue_shares_only_on_a_row_that_brings_its_ceiling`). A credentialed venue with no row
/// stays `PerHolder`, the strategy lane's alone.
///
/// **OANDA's reading** (2026-10-20): its `unsubscribe` is `DataClientRegistry::stop_join` of one id, and each
/// `subscribe_quotes` spawns exactly one registry thread holding exactly one `/pricing/stream`, so releasing
/// a key ends that key's stream and no other (`crates/bridges/oanda/src/feed/feed_tests.rs`'s
/// `releasing_one_quote_key_ends_exactly_that_pricing_stream_and_no_other` holds it). The REST first-price
/// lane is one thread for every pair and an unsubscribe never touches it.
///
/// **IG's reading** (2026-11-06): its `unsubscribe` is `DataClientRegistry::stop_join` of one lane thread, and the lane's
/// dropped stream sends one `delete` of its own subscription on the client's one Lightstreamer socket
/// (`crates/bridges/ig/src/feed/link.rs`'s `LightstreamerLink::detach`). So releasing a key ends that key and no other, the quote and
/// candle keys of one epic included, and it costs no login and no socket
/// (`crates/bridges/ig/tests/offline/quote_feed.rs`'s
/// `releasing_one_key_of_an_epic_deletes_only_that_subscription_and_opens_nothing` holds it).
///
/// **IBKR's reading** (2026-11-06): its stream is the shared `DataClientBus` over
/// `vike_ibkr::feed::price::DataClientBusSpecIbkr`, one data client for every
/// symbol, and its `unsubscribe` stops one symbol's stream and nothing else: one pump and its one `cancelMarketData` on the
/// socket backend (`crates/bridges/vike-ibkr/src/feed/socket/wire_tests.rs`'s
/// `releasing_one_quote_key_cancels_exactly_its_market_data_request_and_no_other`, the real `ibapi` client against a
/// loopback gateway, another key of the same contract included), one stream on the Client Portal, whose `umd` goes only
/// with the last stream of a conId (`crates/bridges/vike-ibkr/src/feed/cpapi/pump_tests.rs`'s
/// `remove_reports_the_conid_with_its_last_stream`). A second holder here is a hold on the broker's one client and
/// reaches the bridge not at all, so it opens no second data client under the gateway's `data_client_id`.
///
/// **cTrader's reading** (2026-11-06): one client is one multiplexed socket with one login, so a key is a frame
/// and no holder adds a session. Its `unsubscribe` is the engine's holder count and the hold's `unwant`
/// (`crates/bridges/ctrader/src/feed/mod.rs`'s `DataClientBusSpecHoldCtrader`): every key of a symbol stands on the
/// symbol's one spot subscription, the last key out writes `UnsubscribeSpots` and a release in between writes
/// nothing (`crates/bridges/ctrader/tests/offline/data_holds.rs`'s
/// `releasing_a_quote_key_keeps_the_spot_stream_of_a_bar_key_of_the_same_symbol` holds it).
///
/// ⚠ **Deribit is `PerHolder` for a DIFFERENT reason, and not a measured one.** Its `unsubscribe`
/// is the CEX shape (`DataClientRegistry::stop_join`, one thread per subscription), so that column would
/// say `Shared` — but it declares a LOSSLESS book lane (`live_data.book`), and
/// `no_shared_venue_serves_a_lossless_book_lane` forbids sharing one: a recorder joining a live
/// book mid-chain would get deltas with no anchor. Nothing records deribit today, so the cost of
/// the safe direction is nil; the day something does, that test is where the exception is argued.
pub const EXPLICIT_SHARING: &[(&str, DataClientBrokerSharing)] = &[
    ("binance", DataClientBrokerSharing::Shared),
    ("bybit", DataClientBrokerSharing::Shared),
    ("okx", DataClientBrokerSharing::Shared),
    ("aster", DataClientBrokerSharing::Shared),
    ("hyperliquid", DataClientBrokerSharing::Shared),
    ("polymarket", DataClientBrokerSharing::PerHolder),
    ("deribit", DataClientBrokerSharing::PerHolder),
    ("oanda", DataClientBrokerSharing::Shared),
    // FXCM's reading: `unsubscribe` takes the one pair out of the price desk's wanted set and touches neither the
    // desk's one consumer nor its poller, the stream carries quotes alone (so no other key of the symbol exists), and a
    // second client would be a second consumer, which silences the first: `crates/bridges/fxcm/src/feed_tests.rs`'s
    // `releasing_one_quote_key_stops_exactly_that_pair_and_no_other`.
    ("fxcm", DataClientBrokerSharing::Shared),
    ("ig", DataClientBrokerSharing::Shared),
    ("ibkr", DataClientBrokerSharing::Shared),
    // A necessity as well as a reading: the bus has ONE listener and a second client REPLACES the first. Its
    // `unsubscribe` is `Listening::unwant` of that one symbol: one `unsubscribe` line, no other symbol, the
    // listener kept, and a pair the rate reader holds stays subscribed
    // (`crates/bridges/dukascopy/src/feed_tests.rs`'s `releasing_one_want_stops_that_symbol_and_nothing_else`).
    ("dukascopy", DataClientBrokerSharing::Shared),
    // Releasing a key disturbs no other key of the symbol: "cTrader's reading" above, and `data_holds.rs`'s
    // `releasing_a_quote_key_keeps_the_spot_stream_of_a_bar_key_of_the_same_symbol` pins it.
    ("ctrader", DataClientBrokerSharing::Shared),
];

/// ⚠ The fallback is `PerHolder` — two clients, the behaviour before the broker — because it is the
/// direction that cannot corrupt a tape. A venue earns `Shared` only by a row above, argued from its
/// own `unsubscribe`.
pub fn sharing_for(venue: &str) -> DataClientBrokerSharing {
    EXPLICIT_SHARING
        .iter()
        .find(|(name, _)| *name == venue)
        .map_or(DataClientBrokerSharing::PerHolder, |(_, s)| *s)
}

// ⚠ INLINE, deliberately, where the plan drew a `venues_tests.rs` beside this file: the code-layout
// rule moves a test block out only for a file >= 400 lines carrying >= 150 test lines, and this one
// is neither, and a 60-line module has no argument for leaving its file.
#[cfg(test)]
mod tests {
    use super::*;

    use vike_data::NoopSink;
    use vike_hub_protocol::auth::HubKeys;

    /// A link to nothing: a port nobody listens on and no key. A client over it is a link that is
    /// down, which is all a test that only BUILDS clients needs.
    fn unreachable_link() -> GatewayLink {
        GatewayLink::new("127.0.0.1:1", HubKeys::new(Vec::new(), Vec::new()))
    }

    /// D4, as a check on the table rather than a sentence: a venue that serves a LOSSLESS book
    /// lane is never Shared, so the recorder can never join a live book mid-chain without its
    /// anchor.
    #[test]
    fn no_shared_venue_serves_a_lossless_book_lane() {
        for v in vike_model::VENUES {
            if sharing_for(v) == crate::feeds::DataClientBrokerSharing::Shared {
                assert!(
                    !vike_venues::capabilities_for(v).live_data.book,
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
        assert_eq!(sharing_for("polymarket"), crate::feeds::DataClientBrokerSharing::PerHolder);
        for v in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
            assert_eq!(sharing_for(v), crate::feeds::DataClientBrokerSharing::Shared, "{v}");
        }
    }

    /// **A credentialed venue shares only on a row of its own, and that row brings its ceiling** (decision
    /// 0193): `Shared` is written on a reading of the venue's own `unsubscribe`, the ceiling for all holders
    /// together is what keeps sharing from opening the venue's allowance to three holders, and a `PerHolder`
    /// row says nothing the fallback does not. `no_shared_venue_serves_a_lossless_book_lane` checks every
    /// shared row for a lossless book lane. Alpaca is deferred by the owner and has no row.
    #[test]
    fn a_credentialed_venue_shares_only_on_a_row_that_brings_its_ceiling() {
        assert_eq!(sharing_for("oanda"), crate::feeds::DataClientBrokerSharing::Shared);
        assert_eq!(sharing_for("alpaca"), crate::feeds::DataClientBrokerSharing::PerHolder);
        for (v, sharing) in EXPLICIT_SHARING.iter().filter(|(v, _)| CREDENTIALED_VENUES.contains(v))
        {
            assert_eq!(
                *sharing,
                crate::feeds::DataClientBrokerSharing::Shared,
                "`{v}`: no row is how to say PerHolder"
            );
            assert!(
                vike_gatewayhub_client::credentialed_limits::stream_limits_for(v).is_some(),
                "`{v}` shares with no row in `credentialed_limits::ROWS`: nothing counts its holders together"
            );
        }
    }

    /// **IBKR shares on the reading of its stream's `unsubscribe`**, which [`EXPLICIT_SHARING`]'s doc names the bridge
    /// tests of, and it has no lossless book a holder could join mid-chain.
    #[test]
    fn ibkr_shares_on_the_reading_of_its_unsubscribe() {
        assert_eq!(sharing_for("ibkr"), crate::feeds::DataClientBrokerSharing::Shared);
        assert!(!vike_venues::capabilities_for("ibkr").live_data.book);
    }

    /// IBKR's quote keys are followed outside the silence watchdog: an equity market closes, and a delayed stream
    /// records nothing at all, so a silent series is no fault.
    #[cfg(feature = "record")]
    #[test]
    fn ibkr_is_followed_outside_the_silence_watchdog() {
        assert!(crate::recording::strategy_followed().contains(&"ibkr"));
        assert!(crate::recording::session_bounded().contains(&"ibkr"));
    }

    /// **cTrader shares on the reading of its counted `unsubscribe`**, which
    /// `crates/bridges/ctrader/tests/offline/data_holds.rs`'s
    /// `releasing_a_quote_key_keeps_the_spot_stream_of_a_bar_key_of_the_same_symbol` pins. Without this row the
    /// recorder follows nothing there and nothing else says so.
    #[test]
    fn ctrader_shares_on_the_reading_of_its_counted_unsubscribe() {
        assert_eq!(sharing_for("ctrader"), crate::feeds::DataClientBrokerSharing::Shared);
    }

    /// Deribit shares nothing, and for the one reason a measurement cannot give: its `unsubscribe`
    /// is the CEX shape, but it declares a lossless book lane, which [`EXPLICIT_SHARING`]'s doc and
    /// `no_shared_venue_serves_a_lossless_book_lane` say a shared client may never serve.
    #[test]
    fn deribit_is_per_holder_because_it_serves_a_lossless_book() {
        assert_eq!(sharing_for("deribit"), crate::feeds::DataClientBrokerSharing::PerHolder);
        assert!(vike_venues::capabilities_for("deribit").live_data.book);
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

    /// Every listed venue builds, credentialed or not: the list never names a venue the table has no arm
    /// for, which would be the unknown-venue message for a venue the sharing table declares.
    #[test]
    fn every_client_venue_builds() {
        let link = unreachable_link();
        for v in CLIENT_VENUES {
            if let Err(e) = build_client(v, Arc::new(NoopSink), &link) {
                panic!("`{v}`: {e}");
            }
        }
    }

    /// **Each arm asks the gatewayhub for its OWN venue** (decision 0095's pin, re-aimed: the
    /// `mark_streams` and `ws_tokens_per_socket` rows are the gatewayhub's to thread now, and
    /// `crates/vike-gatewayhub/src/data/venues_tests.rs`'s
    /// `every_arm_threads_its_own_venues_settings_into_its_own_feed` holds them there).
    ///
    /// What is left to get wrong HERE is a client that names another venue's slug, which compiles and
    /// subscribes the wrong venue's symbols. Unlike the old bridge arms, a remote client can be watched:
    /// this subscribes through every built client against a loopback gatewayhub and reads back which venue
    /// the daemon was asked to open, in the order the clients were built.
    #[test]
    fn every_arm_asks_the_gatewayhub_for_its_own_venue() {
        use vike_gatewayhub::testkit::data::DataLaneScript;
        use vike_gatewayhub::testkit::gateway::LoopbackGateway;
        use vike_gatewayhub::testkit::rest::RestLaneScript;

        // The gateway is declared BEFORE the clients, so they drop first (the testkit's rule).
        // The keyless venues only: a credentialed one asks for a `cs_venue=` session this scripted lane
        // does not serve (`strategy_gateway.rs` drives those through a lane that does).
        let keyless = KEYLESS_STREAM_VENUES;
        let data = DataLaneScript::new(&keyless);
        let gateway = LoopbackGateway::start(data.clone(), RestLaneScript::new());
        let link = gateway.link();
        let mut clients = Vec::new();
        for v in keyless {
            let mut client = build_client(v, Arc::new(NoopSink), &link)
                .unwrap_or_else(|e| panic!("`{v}` must build: {e}"));
            client.subscribe_trades("PROBE").unwrap_or_else(|e| panic!("`{v}`: {e}"));
            clients.push(client);
        }
        let asked: Vec<String> = data.sessions().iter().map(|s| s.venue.clone()).collect();
        assert_eq!(asked, keyless, "each arm must open a session for its OWN venue at the daemon");
    }

    /// **A test-network client is never shared, a credentialed venue is the market-data plane's never,
    /// and the recorder's only where the table lists it.** The broker names a network client
    /// `venue@network`, which is no row of [`EXPLICIT_SHARING`], so it falls to the safe `PerHolder`
    /// (never the market-data plane's or the recorder's client). The credentialed venues are in no
    /// market-data roster (`supported`) until the operator's row opens one (decision 0193 verdict 8,
    /// `crate::market_data::venues::credentialed_from_row`), every one is the strategy lane's, and only the ones
    /// with a sharing row are in the client table (`CLIENT_VENUES`) the recorder reaches.
    #[test]
    fn a_network_client_is_never_shared_and_a_credentialed_venue_is_the_recorders_only_on_a_row() {
        assert_eq!(
            sharing_for(&client_label("hyperliquid", Some("testnet"))),
            crate::feeds::DataClientBrokerSharing::PerHolder
        );
        assert_eq!(sharing_for("hyperliquid"), crate::feeds::DataClientBrokerSharing::Shared);
        for v in CREDENTIALED_VENUES {
            assert_eq!(
                CLIENT_VENUES.contains(&v),
                sharing_for(v) == crate::feeds::DataClientBrokerSharing::Shared,
                "`{v}`: a credentialed venue is a client venue exactly when it has a sharing row"
            );
            assert!(
                !crate::market_data::venues::supported().contains(&v),
                "`{v}` must not be served by market data"
            );
            assert!(strategy_venues().contains(&v), "`{v}` is the strategy lane's");
        }
    }

    /// An UNKNOWN venue is refused naming the table.
    #[test]
    fn an_unknown_venue_names_the_table() {
        let err = match build_client("kalshi", Arc::new(NoopSink), &unreachable_link()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert!(err.contains("kalshi") && err.contains("Table:"), "{err}");
    }

    /// **A network on a venue whose feed does not follow the account tier is refused, in today's words**:
    /// the venues named are `network_venues()`, the client crate's reading of the `vike_venues` price-feed column.
    #[test]
    fn a_network_on_another_venue_is_refused_naming_the_one_that_takes_it() {
        let err = match build_client("binance@testnet", Arc::new(NoopSink), &unreachable_link()) {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        };
        assert_eq!(
            err,
            "feed broker: venue `binance` takes no network (asked for `testnet`); only \
             hyperliquid has a feed that follows the account tier"
        );
    }
}
