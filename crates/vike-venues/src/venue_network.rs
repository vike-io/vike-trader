//! `VenueNetworkRow`: the two per-venue NETWORK facts a hub needs and no bridge may be linked for. Which network a
//! venue's PRICE FEED dials, against the account it prices ([`PriceFeedNetwork`]), and whether the venue's
//! instrument list differs per network ([`CatalogListNetwork`]).
//!
//! **Every roster venue is a NAMED row** (`crates/vike-venues/CLAUDE.md`'s "Per-venue capability maps (the
//! playbook)") citing where each answer was read. The price-feed column is read by the tradehub's dead-man fold
//! (`crates/vike-tradehub/src/venue_arming/deadman.rs`'s `price_source_tiers`, through
//! [`PriceFeedNetwork::price_source_tier`]), by the client crate's roster of the feeds that follow the tier
//! (`crates/vike-gatewayhub-client/src/history.rs`'s `network_venues`) and by the gatewayhub's network-taking stream
//! (`crates/vike-gatewayhub/src/data/venues.rs`), where three hand spellings of it used to live (B3, 2026-11-22); the
//! catalog column is each bridge's `SymbolPropertiesListSpec::list_key`. An unknown venue string answers the
//! defaults (`Mainnet`, `OneForAllNetworks`), which no roster venue rides (completeness-tested).

use vike_model::AccountTier;

/// Which network a venue's price feed dials, against the account it prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriceFeedNetwork {
    /// Trusted on every account: the core judges no account's network against the feed. For the crypto venues the
    /// feed is mainnet over a `demo` account too ("demo exec over mainnet prices"), an owner-accepted shape.
    Mainnet,
    /// The feed dials the network of the account tier it was wired for (mainnet when a `live` account is armed), so
    /// only an account beside one of another tier is priced off another network.
    FollowsTier,
    /// The feed is the public mainnet whatever the account, while exec signs on the test network: every real account
    /// but a `live` one is priced off another network.
    LiveOnly,
    /// The ONE credentialed stream of the venue is opened as its default `demo` account: a `live` account is priced off
    /// the demo broker's quotes.
    DemoBroker,
}

impl PriceFeedNetwork {
    /// The account tier whose network the feed dials (`Live` = mainnet, `Demo` = testnet), for the core's price-source
    /// check (`vike_core::PriceOnlyConfig::price_source`); `None` is a trusted feed. `followed` is the tier a
    /// [`PriceFeedNetwork::FollowsTier`] feed was wired on, `None` when no such feed runs.
    #[must_use]
    pub const fn price_source_tier(self, followed: Option<AccountTier>) -> Option<AccountTier> {
        match self {
            PriceFeedNetwork::Mainnet => None,
            PriceFeedNetwork::FollowsTier => followed,
            PriceFeedNetwork::LiveOnly => Some(AccountTier::Live),
            PriceFeedNetwork::DemoBroker => Some(AccountTier::Demo),
        }
    }
}

/// Whether a venue's instrument list is kept per network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogListNetwork {
    /// The test network lists its own instruments, kept under a list key of its own: an account's symbol is judged
    /// against the list of the network it trades on.
    PerNetwork,
    /// One list (or one per-account or per-key lookup) answers every network.
    OneForAllNetworks,
}

/// One venue's network facts, each with where it was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueNetworkRow {
    /// Which network the price feed dials.
    pub price_feed: PriceFeedNetwork,
    /// Why, citing the code (a `path`'s `symbol`, never a line number).
    pub price_feed_why: &'static str,
    /// Whether the instrument list is kept per network.
    pub catalog_list: CatalogListNetwork,
    /// Why, citing the code.
    pub catalog_list_why: &'static str,
}

/// The price-feed `why` of every venue whose feed is trusted on every account.
const MAINNET_FEED: &str = "a public mainnet feed trusted on every account, so the tradehub's dead-man fold \
     (`crates/vike-tradehub/src/venue_arming/deadman.rs`'s `price_source_tiers`) states no tier for it: the core \
     judges no account's network against its feed";

/// The catalog `why` of every venue whose `SymbolPropertiesListSpec` keeps the default `list_key`.
const ONE_LIST: &str = "its `SymbolPropertiesListSpec` keeps the default `list_key` \
     (`crates/vike-bridge-core/src/symbol_properties.rs`'s `SymbolPropertiesListSpec`), the venue's own key on every \
     network";

/// The catalog `why` of a venue whose instruments are one account's own list.
const ACCOUNT_LIST: &str = "account-listed (`crates/vike-bridge-core/src/symbol_properties.rs`'s \
     `SymbolPropertiesAccountSpec`): the asking account's own list, read with its own credentials";

/// The catalog `why` of a venue looked up one key at a time.
const KEYED_LOOKUP: &str = "looked up one key at a time (`crates/vike-bridge-core/src/symbol_properties.rs`'s \
     `SymbolPropertiesKeyedSpec`) on the account's own session: no list to keep per network";

/// The catalog `why` of a venue whose facts are quoted on its own session.
const SESSION_QUOTED: &str = "its facts are quoted on the account's own session (the bridge's `quoted_facts`), \
     with no instrument list to keep per network";

const fn row(
    price_feed: PriceFeedNetwork,
    price_feed_why: &'static str,
    catalog_list: CatalogListNetwork,
    catalog_list_why: &'static str,
) -> VenueNetworkRow {
    VenueNetworkRow { price_feed, price_feed_why, catalog_list, catalog_list_why }
}

/// The network row of `venue`.
#[must_use]
pub fn network_for(venue: &str) -> VenueNetworkRow {
    use CatalogListNetwork::{OneForAllNetworks, PerNetwork};
    use PriceFeedNetwork::{DemoBroker, FollowsTier, LiveOnly, Mainnet};
    match venue {
        "binance" => row(
            Mainnet,
            MAINNET_FEED,
            PerNetwork,
            "`crates/bridges/binance/src/symbol_properties.rs`'s `SymbolPropertiesListSpecBinance` keys the testnet's \
             list as `BINANCE_DEMO_LIST` (and its COIN-M book as `BINANCE_DEMO_COIN_M_LIST`)",
        ),
        "okx" => row(
            Mainnet,
            MAINNET_FEED,
            PerNetwork,
            "`crates/bridges/okx/src/symbol_properties.rs`'s `SymbolPropertiesListSpecOkx` keys the simulated-trading \
             list as `OKX_DEMO_LIST`",
        ),
        "hyperliquid" => row(
            FollowsTier,
            "the one feed wired on the tier it follows: the tradehub's dead-man fold reads the tier its feed was wired \
             on (`hl_feed_tier`, `crates/vike-tradehub/src/venue_arming/deadman.rs`), and the gatewayhub's \
             network-taking stream dials it (`crates/vike-gatewayhub/src/data/venues.rs`)",
            PerNetwork,
            "`crates/bridges/hyperliquid/src/symbol_properties.rs`'s `SymbolPropertiesListSpecHyperliquid` keys the \
             testnet's list as `HYPERLIQUID_TESTNET_LIST`",
        ),
        "deribit" => row(
            LiveOnly,
            "the public MAINNET host always, while exec signs on the testnet, so every real account but a `live` \
             one is priced off another network (the dead-man fold, \
             `crates/vike-tradehub/src/venue_arming/deadman.rs`, judges it so)",
            OneForAllNetworks,
            ONE_LIST,
        ),
        "ctrader" => row(
            DemoBroker,
            "the gatewayhub opens the venue's one credentialed stream as its default `demo` account \
             (`crates/vike-secrets/src/account_directory.rs`'s `data_feed_credentials`), so a `live` account is \
             priced off the demo broker's quotes",
            OneForAllNetworks,
            ACCOUNT_LIST,
        ),
        "ig" => row(
            DemoBroker,
            "the one credentialed stream is the default `demo` account's, as for ctrader, so a `live` account is \
             priced off the demo broker's quotes",
            OneForAllNetworks,
            KEYED_LOOKUP,
        ),
        "aster" | "bybit" | "polymarket" | "alpaca" => {
            row(Mainnet, MAINNET_FEED, OneForAllNetworks, ONE_LIST)
        }
        "oanda" => row(Mainnet, MAINNET_FEED, OneForAllNetworks, ACCOUNT_LIST),
        "ibkr" => row(Mainnet, MAINNET_FEED, OneForAllNetworks, KEYED_LOOKUP),
        "fxcm" | "dukascopy" => row(Mainnet, MAINNET_FEED, OneForAllNetworks, SESSION_QUOTED),
        // vike:new-venue:row "{venue}" => row(Mainnet, MAINNET_FEED, OneForAllNetworks, ONE_LIST), // TODO(new-venue: {venue}): read its feed network and its list key from the bridge
        _ => row(Mainnet, "not a roster venue", OneForAllNetworks, "not a roster venue"),
    }
}

#[path = "venue_network_tests.rs"]
#[cfg(test)]
mod venue_network_tests;
