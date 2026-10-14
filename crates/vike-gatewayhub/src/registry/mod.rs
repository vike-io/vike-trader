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
//! one whose practice token the history lane spends. The tradehub's single-session and credentialed
//! trading venues are in no roster: this daemon opens no session on them in wave 1, and
//! `crates/vike-ops/tests/venues/venue_subset_tables_gate.rs` asks every new venue whether it belongs
//! in [`ROSTERS`] at all.

/// One lane of the daemon a venue can be served on.
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
}

impl Lane {
    /// Every lane, in table order.
    pub const ALL: [Lane; 8] = [
        Lane::Stream,
        Lane::Klines,
        Lane::Funding,
        Lane::Ticks,
        Lane::Catalog,
        Lane::CredentialedKlines,
        Lane::Options,
        Lane::Family,
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

/// Every lane's roster, one row per [`Lane`] (a test holds the table complete).
pub const ROSTERS: [Roster; 8] = [
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

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
