//! The data-side VENUE TABLE: which venues each lane of the daemon serves.
//!
//! This is the table decision 0164 folds out of the datahub: the roster of venues per lane, typed and
//! ordered, and NO venue code. The arms live beside the code that needs the bridges, behind the
//! `data` feature: the function that builds a venue's stream client is `crate::data::venues`'s
//! `build_client`, the sources that fetch klines and funding are `crate::rest::kline_sources`'s
//! registries, the catalog providers `crate::rest::catalog`'s. Each real lane's `venues` / `facts` is
//! built FROM this table, or held equal to it by a test that reads both, so the advertisement and
//! the dispatch cannot disagree. The trade side's table is `registry::trade`'s `REGISTRY` (feature `trade`; it
//! moved here from the tradehub in wave 2); the tradehub keeps `FEED_ARMS`
//! (`crates/vike-tradehub/src/feeds.rs`), whose arms dial the datahub's strategy lane and name no bridge.
//!
//! # The rosters are ordered, and the order is observable
//!
//! [`RosterLane::Klines`], [`RosterLane::Funding`] and [`RosterLane::Catalog`] are the orders of the datahub's own
//! tables at the time the venue calls moved (`KLINE_SOURCES`, `FUNDING_SOURCES`, `real_catalog_table`),
//! and their rows ARE the client's rosters (`vike_gatewayhub_client::history::KLINE_VENUES`,
//! `FUNDING_VENUES`, `catalog::CATALOG_VENUES`; the single-venue [`RosterLane::Ticks`] and [`RosterLane::Family`]
//! name the client's `TICK_VENUE` and `FAMILY_VENUE`): the datahub folds the client's list and this
//! daemon advertises the same one, so the two cannot name venues in a different order. The strategy
//! lane's [`RosterLane::Pump`], [`RosterLane::CredentialedStream`] and [`RosterLane::StreamNetwork`] are the client's
//! `history::FEED_TICK_VENUES`, `credentialed_limits::CREDENTIALED_VENUES` (the names of its `ROWS`) and
//! `history::NETWORK_VENUE`, which the datahub's strategy lane refuses and opens by. A venue joins
//! those lanes in the client crate. [`RosterLane::Stream`] is the order of the datahub's
//! `EXPLICIT_SHARING` table's keyless venues (the seven a feed broker opens a client for over a plain
//! `DataOpen`); its eighth row, `oanda` (decision 0193), is a credentialed stream and is on
//! [`RosterLane::CredentialedStream`], where this daemon's session guard keeps its one live session.
//!
//! # What is deliberately not here
//!
//! No sharing column (the datahub keeps `Shared`/`PerHolder`, a consumer-side rule), no deribit in the
//! catalog roster (left out on purpose: `crates/vike-gatewayhub/src/rest/catalog.rs`'s `CatalogProviders`)
//! and no credentialed venue other than the three whose credential the history lane spends
//! (`RosterLane::CredentialedKlines`), and (wave 2, W10) the four credentialed-DATA
//! venues of `RosterLane::CredentialedStream`. ibkr joins that lane as the any-symbol PRICE stream only
//! (decision 0174): a lazily dialled quote stream over the gateway session's one data connection, in
//! the `ibkr` build alone (`CredentialedStreamLane::venues` drops it otherwise); dukascopy joins it the same
//! way: it logs in to nothing, the stream rides the trade lane's sidecar (`vike_dukascopy::feed`);
//! and so does fxcm, in the `fxcm` build alone (`CredentialedStreamLane::venues` drops it otherwise): it logs in to
//! nothing either, the stream polls the Offers table of the account's running reconcile session
//! (`vike_fxcm::feed`). None of them has a keyless stream or a history lane, and [`ROSTERS`]' new-venue
//! note asks every new venue whether it belongs in each lane.

/// One lane of the daemon a venue can be served on.
///
/// The last three (`Pump`, `CredentialedStream`, `StreamNetwork`) are the strategy lane's additions
/// (wave 2, W10) and live on the data side of the table: a tick pump hosted at the daemon, a stream the
/// daemon logs in to as the account the strategy trades, and the venue whose stream takes a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RosterLane {
    /// A keyless live stream: `market_data_venue=` in `Welcome.features`, `DataOpen`.
    Stream,
    /// The keyless kline fetch.
    Klines,
    /// The funding-rate fetch.
    Funding,
    /// The dukascopy tick range (the `.bi5` archive fetch).
    Ticks,
    /// The venue catalog: `CatalogDescribe` and the catalog lanes of `Fetch`.
    Catalog,
    /// The credentialed kline fetch: `credentialed_data_venue=`, the daemon spends a credential it holds.
    CredentialedKlines,
    /// The options surface: REST, the contract index and the two feeds.
    Options,
    /// A rotating recorder family (`ResolveFamily`).
    Family,
    /// The CEX tick pump (`SubscriptionOperation::Ticks`): `gateway_feed_tick`.
    Pump,
    /// A credentialed live stream: `cs_venue=`, `Scope::Write`, the daemon spends a credential it holds.
    CredentialedStream,
    /// A stream that takes a network on `DataOpen`: `stream_network`.
    StreamNetwork,
}

impl RosterLane {
    /// Every lane, in table order.
    pub const ALL: [RosterLane; 11] = [
        RosterLane::Stream,
        RosterLane::Klines,
        RosterLane::Funding,
        RosterLane::Ticks,
        RosterLane::Catalog,
        RosterLane::CredentialedKlines,
        RosterLane::Options,
        RosterLane::Family,
        RosterLane::Pump,
        RosterLane::CredentialedStream,
        RosterLane::StreamNetwork,
    ];

    /// The lane's name, for a log line or a test message. Exhaustive: a new lane fails to compile
    /// here until it has one, and [`ROSTERS`] is held to carry a row for every entry of
    /// [`RosterLane::ALL`].
    pub const fn name(self) -> &'static str {
        match self {
            RosterLane::Stream => "stream",
            RosterLane::Klines => "klines",
            RosterLane::Funding => "funding",
            RosterLane::Ticks => "ticks",
            RosterLane::Catalog => "catalog",
            RosterLane::CredentialedKlines => "credentialed_klines",
            RosterLane::Options => "options",
            RosterLane::Family => "family",
            RosterLane::Pump => "pump",
            RosterLane::CredentialedStream => "credentialed_stream",
            RosterLane::StreamNetwork => "stream_network",
        }
    }
}

/// One lane and the venues it serves, in order.
#[derive(Debug, Clone, Copy)]
pub struct Roster {
    /// The lane.
    pub lane: RosterLane,
    /// Its venues, each a `vike_model::VENUES` slug, in the lane's observable order.
    pub venues: &'static [&'static str],
}

// vike:new-venue:note decide, for EVERY lane in `ROSTERS`, whether `{venue}` belongs in it, and add no row blind: a roster row is the daemon's claim that it serves the venue on that lane, so the lane's own arm must exist first (a stream client in `crates/vike-gatewayhub/src/data/venues.rs`'s `build_client`, a kline or funding source, a catalog provider, a pump arm in `crates/vike-gatewayhub/src/data/feed_tick.rs`'s `feed_tick_table`, a credentialed arm in `crates/vike-gatewayhub/src/data/credentialed.rs`'s `build_credentialed`), and the klines, funding, ticks, catalog, family, pump, credentialed-stream and stream-network rows ARE the client crate's rosters (the credentialed one is the names of `crates/vike-gatewayhub-client/src/credentialed_limits.rs`'s `ROWS`), so a venue joins those lanes there: crates/vike-gatewayhub/src/registry/mod.rs's `ROSTERS`
/// Every lane's roster, one row per [`RosterLane`] (a test holds the table complete).
pub const ROSTERS: [Roster; 11] = [
    Roster {
        lane: RosterLane::Stream,
        venues: &["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit"],
    },
    Roster { lane: RosterLane::Klines, venues: &vike_gatewayhub_client::history::KLINE_VENUES },
    Roster { lane: RosterLane::Funding, venues: &vike_gatewayhub_client::history::FUNDING_VENUES },
    Roster { lane: RosterLane::Ticks, venues: &[vike_gatewayhub_client::history::TICK_VENUE] },
    Roster { lane: RosterLane::Catalog, venues: &vike_gatewayhub_client::catalog::CATALOG_VENUES },
    Roster { lane: RosterLane::CredentialedKlines, venues: &["oanda", "ig", "ctrader"] },
    Roster { lane: RosterLane::Options, venues: &["deribit"] },
    Roster { lane: RosterLane::Family, venues: &[vike_gatewayhub_client::resolve::FAMILY_VENUE] },
    Roster { lane: RosterLane::Pump, venues: &vike_gatewayhub_client::history::FEED_TICK_VENUES },
    Roster {
        lane: RosterLane::CredentialedStream,
        venues: &vike_gatewayhub_client::credentialed_limits::CREDENTIALED_VENUES,
    },
    Roster {
        lane: RosterLane::StreamNetwork,
        venues: &[vike_gatewayhub_client::history::NETWORK_VENUE],
    },
];

/// The venues `lane` serves, in order.
pub fn roster(lane: RosterLane) -> &'static [&'static str] {
    ROSTERS.iter().find(|r| r.lane == lane).map_or(&[], |r| r.venues)
}

/// The lanes `venue` is served on, in table order.
pub fn lanes_of(venue: &str) -> Vec<RosterLane> {
    ROSTERS.iter().filter(|r| r.venues.contains(&venue)).map(|r| r.lane).collect()
}

/// A lane's roster as owned strings: the shape `crate::data::DataLane::venues` and
/// `crate::rest::RestRosters` take.
pub fn owned(lane: RosterLane) -> Vec<String> {
    roster(lane).iter().map(|v| (*v).to_string()).collect()
}

/// The trade-side venue table (feature `trade`): the rows `TradeLaneBridge` is started over.
#[cfg(feature = "trade")]
pub mod trade;

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
