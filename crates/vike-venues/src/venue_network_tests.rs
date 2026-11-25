//! The network table.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_network_matrix_is_pinned_verbatim`] | a column moved, or a venue added to the roster with no row |
//! | [`the_price_feed_column_is_price_source_tiers`] | the tradehub's dead-man fold, which reads this column, answering other than its old arms did |

use super::*;
use vike_model::VENUES;

use CatalogListNetwork::{OneForAllNetworks, PerNetwork};
use PriceFeedNetwork::{DemoBroker, FollowsTier, LiveOnly, Mainnet};

/// The whole table, verbatim.
#[test]
fn the_network_matrix_is_pinned_verbatim() {
    #[rustfmt::skip]
    const MATRIX: &[(&str, PriceFeedNetwork, CatalogListNetwork)] = &[
        ("binance",     Mainnet,     PerNetwork),
        ("bybit",       Mainnet,     OneForAllNetworks),
        ("okx",         Mainnet,     PerNetwork),
        ("deribit",     LiveOnly,    OneForAllNetworks),
        ("oanda",       Mainnet,     OneForAllNetworks),
        ("ig",          DemoBroker,  OneForAllNetworks),
        ("fxcm",        Mainnet,     OneForAllNetworks),
        ("dukascopy",   Mainnet,     OneForAllNetworks),
        ("polymarket",  Mainnet,     OneForAllNetworks),
        ("ibkr",        Mainnet,     OneForAllNetworks),
        ("ctrader",     DemoBroker,  OneForAllNetworks),
        ("alpaca",      Mainnet,     OneForAllNetworks),
        ("aster",       Mainnet,     OneForAllNetworks),
        ("hyperliquid", FollowsTier, PerNetwork),
        // vike:new-venue:row ("{venue}", Mainnet, OneForAllNetworks), // TODO(new-venue: {venue}): move with the row above
    ];
    assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
    let fallback = network_for("no-such-venue");
    for &(venue, price_feed, catalog_list) in MATRIX {
        assert!(VENUES.contains(&venue), "{venue} is not a roster venue");
        let row = network_for(venue);
        assert_eq!((row.price_feed, row.catalog_list), (price_feed, catalog_list), "{venue}");
        assert_ne!(row, fallback, "{venue} rides the unknown-venue fallback");
        assert!(row.price_feed_why.len() > 30 && row.catalog_list_why.len() > 30, "{venue}");
    }
}

/// The price-feed column answers exactly what `price_source_tiers` (the tradehub's dead-man fold, which reads it)
/// answered from its own arms before B3: deribit `Live`, ctrader and ig `Demo`, hyperliquid the tier its feed was wired
/// on, every other venue absent.
#[test]
fn the_price_feed_column_is_price_source_tiers() {
    for followed in [None, Some(AccountTier::Live), Some(AccountTier::Demo)] {
        for venue in VENUES {
            let want = match *venue {
                "deribit" => Some(AccountTier::Live),
                "ctrader" | "ig" => Some(AccountTier::Demo),
                "hyperliquid" => followed,
                _ => None,
            };
            assert_eq!(network_for(venue).price_feed.price_source_tier(followed), want, "{venue}");
        }
    }
}
