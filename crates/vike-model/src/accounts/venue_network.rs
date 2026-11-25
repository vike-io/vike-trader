//! [`VenueNetwork`] — which of a venue's two networks an account reaches, MAINNET or TESTNET, and
//! the ONE mapping from an [`AccountTier`] to it ([`VenueNetwork::for_tier`]). Its words and their
//! roster are `vike_utility::word_enum!`'s (`word`, `from_word`, `WORDS`).
//!
//! Shared because two sides must not disagree about it and one of them may not link the other: the
//! hyperliquid bridge signs and dials on the network, and the tradehub's feed plan picks the network
//! its price feed follows while linking no bridge (decision 0164). What a network MEANS on a venue
//! (its hosts, its signing identity) stays in that venue's bridge, as functions over this type.
//! `crates/vike-tradehub/tests/daemon/feed_network_table.rs` pins the feed plan's choice over every
//! account tier.

use crate::AccountTier;

vike_utility::word_enum! {
    /// A venue's main network or its test network. Its word is the network as an operator says it
    /// (`mainnet` / `testnet`), for a line or a refusal that names whose universe something was
    /// judged against, and the word the strategy and data wires carry.
    ///
    /// ⚠ **A wire network word is read EXACTLY**, through [`VenueNetwork::from_word`]: `TESTNET` is
    /// refused, so a reader fails closed on a word it does not know. The case-insensitive
    /// [`VenueNetwork::parse`] is for a word an operator types, never for a wire field.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum VenueNetwork {
        Mainnet => "mainnet",
        Testnet => "testnet",
    }
}

impl VenueNetwork {
    /// The network an account's tier reaches (decision 0095, item 3 of 0119): `live` means
    /// MAINNET, every tier below it testnet. [`AccountTier::Paper`], which never reaches a bridge,
    /// reads as `demo` does (decision 0129 item 5).
    #[must_use]
    pub fn for_tier(tier: AccountTier) -> Self {
        match tier {
            AccountTier::Live => VenueNetwork::Mainnet,
            AccountTier::Demo | AccountTier::Paper => VenueNetwork::Testnet,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `live` is mainnet, and `paper` answers exactly as `demo`: never mainnet.
    #[test]
    fn only_live_reaches_mainnet() {
        assert_eq!(VenueNetwork::for_tier(AccountTier::Live), VenueNetwork::Mainnet);
        assert_eq!(VenueNetwork::for_tier(AccountTier::Demo), VenueNetwork::Testnet);
        assert_eq!(VenueNetwork::for_tier(AccountTier::Paper), VenueNetwork::Testnet);
        assert_eq!(
            (VenueNetwork::Mainnet.word(), VenueNetwork::Testnet.word()),
            ("mainnet", "testnet")
        );
    }
}
