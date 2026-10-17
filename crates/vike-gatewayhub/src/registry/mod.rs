//! The data-side VENUE TABLE: which venues each lane of the daemon serves.
//!
//! This is the table decision 0164 folds out of the datahub: the roster of venues per lane, typed and
//! ordered, and NO venue code. The arms live beside the code that needs the bridges, behind the
//! `data` feature: the function that builds a venue's stream client is `crate::data::venues`'s
//! `build_client`, the sources that fetch klines and funding are `crate::rest::kline_sources`'s
//! registries, the catalog providers `crate::rest::catalog`'s. Each real lane's `venues` / `facts` is
//! built FROM this table, or held equal to it by a test that reads both, so the advertisement and
//! the dispatch cannot disagree. Wave 1 folds only the data side: the tradehub's `REGISTRY` and
//! `FEED_ARMS` stay where they are until wave 2.
//!
//! # The rosters are ordered, and the order is observable
//!
//! [`Lane::Klines`], [`Lane::Funding`] and [`Lane::Catalog`] are the orders of the datahub's own
//! tables at the time the venue calls moved (`KLINE_SOURCES`, `FUNDING_SOURCES`, `real_catalog_table`),
//! and the client's rosters (`vike_gatewayhub_client::history::KLINE_VENUES`, `FUNDING_VENUES`,
//! `catalog::CATALOG_VENUES`) are the same lists: the datahub folds the client's, this daemon
//! advertises its own, and a disagreement is a refusal that names venues in a different order. The
//! tests hold the two equal IN ORDER. [`Lane::Stream`] is the order of the datahub's
//! `EXPLICIT_SHARING` table (the seven venues a feed broker can open a client for).
//!
//! # What is deliberately not here
//!
//! No sharing column (the datahub keeps `Shared`/`PerHolder`, a consumer-side rule), no deribit in the
//! catalog roster (a separate finding wave 1 must not touch) and no credentialed venue other than the
//! one whose practice token the history lane spends, and (wave 2, W10) the four credentialed-DATA
//! venues of `Lane::CredentialedStream`. The tradehub's remaining single-session trading venues (ibkr)
//! are in no roster: this daemon opens no session on them, and
//! `crates/vike-ops/tests/venues/venue_subset_tables_gate.rs` asks every new venue whether it belongs
//! in [`ROSTERS`] at all.

/// One lane of the daemon a venue can be served on.
///
/// The last three (`Pump`, `CredentialedStream`, `StreamNetwork`) are the strategy lane's additions
/// (wave 2, W10) and live on the data side of the table: a tick pump hosted at the daemon, a stream the
/// daemon logs in to as the account the strategy trades, and the venue whose stream takes a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    /// A keyless live stream: `md_venue=` in `Welcome.features`, `DataOpen`.
    Stream,
    /// The keyless kline fetch.
    Klines,
    /// The funding-rate fetch.
    Funding,
    /// The dukascopy tick range (the `.bi5` archive fetch).
    Ticks,
    /// The venue catalog: `CatalogDescribe` and the catalog lanes of `Fetch`.
    Catalog,
    /// The credentialed kline fetch: `cd_venue=`, the daemon spends a credential it holds.
    CredentialedKlines,
    /// The options surface: REST, the contract index and the two feeds.
    Options,
    /// A rotating recorder family (`ResolveFamily`).
    Family,
    /// The CEX tick pump (`SubOp::Ticks`): `gw_pump`.
    Pump,
    /// A credentialed live stream: `cs_venue=`, `Scope::Write`, the daemon spends a credential it holds.
    CredentialedStream,
    /// A stream that takes a network on `DataOpen`: `stream_network`.
    StreamNetwork,
}

impl Lane {
    /// Every lane, in table order.
    pub const ALL: [Lane; 11] = [
        Lane::Stream,
        Lane::Klines,
        Lane::Funding,
        Lane::Ticks,
        Lane::Catalog,
        Lane::CredentialedKlines,
        Lane::Options,
        Lane::Family,
        Lane::Pump,
        Lane::CredentialedStream,
        Lane::StreamNetwork,
    ];

    /// The lane's name, for a log line or a test message. Exhaustive: a new lane fails to compile
    /// here until it has one, and [`ROSTERS`] is held to carry a row for every entry of
    /// [`Lane::ALL`].
    pub const fn name(self) -> &'static str {
        match self {
            Lane::Stream => "stream",
            Lane::Klines => "klines",
            Lane::Funding => "funding",
            Lane::Ticks => "ticks",
            Lane::Catalog => "catalog",
            Lane::CredentialedKlines => "credentialed_klines",
            Lane::Options => "options",
            Lane::Family => "family",
            Lane::Pump => "pump",
            Lane::CredentialedStream => "credentialed_stream",
            Lane::StreamNetwork => "stream_network",
        }
    }
}

/// One lane and the venues it serves, in order.
#[derive(Debug, Clone, Copy)]
pub struct Roster {
    /// The lane.
    pub lane: Lane,
    /// Its venues, each a `vike_model::VENUES` slug, in the lane's observable order.
    pub venues: &'static [&'static str],
}

// vike:new-venue:note decide, for EVERY lane in `ROSTERS`, whether `{venue}` belongs in it, and add no row blind: a roster row is the daemon's claim that it serves the venue on that lane, so the lane's own arm must exist first (a stream client in `crates/vike-gatewayhub/src/data/venues.rs`'s `build_client`, a kline or funding source, a catalog provider, a pump arm in `crates/vike-gatewayhub/src/data/pump.rs`'s `real_pump_table`, a credentialed arm in `crates/vike-gatewayhub/src/data/credentialed.rs`'s `build_credentialed`), and the client crate's rosters must list it in the same position: crates/vike-gatewayhub/src/registry/mod.rs's `ROSTERS`
/// Every lane's roster, one row per [`Lane`] (a test holds the table complete).
pub const ROSTERS: [Roster; 11] = [
    Roster {
        lane: Lane::Stream,
        venues: &["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket", "deribit"],
    },
    Roster {
        lane: Lane::Klines,
        venues: &["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"],
    },
    Roster { lane: Lane::Funding, venues: &["binance", "hyperliquid"] },
    Roster { lane: Lane::Ticks, venues: &["dukascopy"] },
    Roster {
        lane: Lane::Catalog,
        venues: &[
            "binance",
            "bybit",
            "okx",
            "aster",
            "hyperliquid",
            "polymarket",
            "dukascopy",
            "fxcm",
        ],
    },
    Roster { lane: Lane::CredentialedKlines, venues: &["oanda"] },
    Roster { lane: Lane::Options, venues: &["deribit"] },
    Roster { lane: Lane::Family, venues: &["polymarket"] },
    Roster { lane: Lane::Pump, venues: &["binance", "bybit", "okx", "aster"] },
    Roster { lane: Lane::CredentialedStream, venues: &["alpaca", "ctrader", "ig", "oanda"] },
    Roster { lane: Lane::StreamNetwork, venues: &["hyperliquid"] },
];

/// The venues `lane` serves, in order.
pub fn roster(lane: Lane) -> &'static [&'static str] {
    ROSTERS.iter().find(|r| r.lane == lane).map_or(&[], |r| r.venues)
}

/// The lanes `venue` is served on, in table order.
pub fn lanes_of(venue: &str) -> Vec<Lane> {
    ROSTERS.iter().filter(|r| r.venues.contains(&venue)).map(|r| r.lane).collect()
}

/// A lane's roster as owned strings: the shape `crate::data::DataLane::venues` and
/// `crate::rest::RestFacts` take.
pub fn owned(lane: Lane) -> Vec<String> {
    roster(lane).iter().map(|v| (*v).to_string()).collect()
}

/// The trade-side venue table (feature `trade`): the rows `BridgeTrade` is started over.
#[cfg(feature = "trade")]
pub mod trade;

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
