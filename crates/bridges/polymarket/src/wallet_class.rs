//! Pure wallet-class taxonomy for the RTDS activity/trades tape (Wave 5c, producer side).
//!
//! The [`crate::rtds`] activity lane decodes a platform-wide, wallet-attributed trade stream
//! ([`crate::rtds::ActivityTrade`]); this module is the pure classifier that labels a trade's
//! `proxy_wallet` with a [`WalletClass`]. The MAKER-side consumption (skewing quotes toward or away
//! from a class) is a deferred follow-up — nothing here reads the tape or touches a strategy.
//!
//! Design: a hot-loadable [`WalletClassMap`] (wallet → class), built from an owned map so a caller
//! can swap the whole table at runtime (e.g. behind an arc-swap), and a pure [`classify`] that keys a
//! wallet into it — [`WalletClass::Unknown`] when the wallet is absent, never an error. No I/O, no
//! internal-crate deps: the map is populated by whatever offline analysis produces it.

use std::collections::HashMap;

/// A wallet's behavioral class on the trade tape. `Sharp` (edge-carrying takers whose flow is
/// adverse-selection risk), `Whale` (size that moves the book), `Retail` (uninformed flow), and
/// `Unknown` (unclassified / absent from the map — the default for every wallet not named).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletClass {
    Sharp,
    Whale,
    Retail,
    Unknown,
}

/// A hot-loadable wallet → [`WalletClass`] table. Built from an owned map ([`WalletClassMap::new`])
/// so a caller can rebuild it offline and swap the whole table in without mutating in place. Absent
/// wallets classify as [`WalletClass::Unknown`].
#[derive(Debug, Clone, Default)]
pub struct WalletClassMap {
    by_wallet: HashMap<String, WalletClass>,
}

impl WalletClassMap {
    /// Build the table from an owned wallet → class map. The map is taken by value (moved), never
    /// borrowed, so the caller can hand off a freshly-built table and drop its own copy.
    pub fn new(by_wallet: HashMap<String, WalletClass>) -> Self {
        WalletClassMap { by_wallet }
    }

    /// This wallet's class, or [`WalletClass::Unknown`] when it is not in the table. Exact string
    /// match — the caller normalizes wallet casing before building the map if it needs to.
    pub fn get(&self, wallet: &str) -> WalletClass {
        self.by_wallet.get(wallet).copied().unwrap_or(WalletClass::Unknown)
    }

    /// How many wallets are classified.
    pub fn len(&self) -> usize {
        self.by_wallet.len()
    }

    /// Whether the table classifies no wallets (so every [`classify`] returns `Unknown`).
    pub fn is_empty(&self) -> bool {
        self.by_wallet.is_empty()
    }
}

/// Classify one wallet against `map` — [`WalletClass::Unknown`] when the wallet is absent. Pure: a
/// thin free-function facade over [`WalletClassMap::get`] so callers read a verb, not a method.
pub fn classify(wallet: &str, map: &WalletClassMap) -> WalletClass {
    map.get(wallet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_map() -> WalletClassMap {
        let mut m = HashMap::new();
        m.insert("0xSHARP".to_string(), WalletClass::Sharp);
        m.insert("0xWHALE".to_string(), WalletClass::Whale);
        m.insert("0xRETAIL".to_string(), WalletClass::Retail);
        WalletClassMap::new(m)
    }

    #[test]
    fn classify_maps_known_wallets() {
        let map = sample_map();
        assert_eq!(classify("0xSHARP", &map), WalletClass::Sharp);
        assert_eq!(classify("0xWHALE", &map), WalletClass::Whale);
        assert_eq!(classify("0xRETAIL", &map), WalletClass::Retail);
    }

    #[test]
    fn classify_returns_unknown_for_absent_wallets() {
        let map = sample_map();
        assert_eq!(classify("0xMYSTERY", &map), WalletClass::Unknown);
        assert_eq!(classify("", &map), WalletClass::Unknown);
        // exact match: a different casing is a different (absent) wallet
        assert_eq!(classify("0xsharp", &map), WalletClass::Unknown);
    }

    #[test]
    fn an_empty_map_classifies_everything_unknown() {
        let map = WalletClassMap::default();
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
        assert_eq!(classify("0xANY", &map), WalletClass::Unknown);
    }

    #[test]
    fn len_and_is_empty_track_the_table() {
        let map = sample_map();
        assert_eq!(map.len(), 3);
        assert!(!map.is_empty());
    }
}
