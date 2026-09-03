//! The venue contribution seam. Two population modes: `Enumerable` venues hand over a full list
//! (cached + searched locally); `QueryBacked` venues (IBKR-scale) are searched live per query.
//! `CatalogSource` is the orthogonal client-side axis (`Direct` venues vs the CI box `ServerBacked`).

use crate::{AssetClass, Instrument, Tab};
use std::fmt;

/// How a venue's universe is obtained (decided by API shape + size, NOT asset class).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatalogMode {
    /// Bulk list endpoint + cacheable universe → fetch once, search locally.
    Enumerable,
    /// No bulk endpoint / too big (IBKR) → search the venue's server per query, never cached.
    QueryBacked,
}

/// Where the CLIENT gets catalog data. `ServerBacked` (the CI box) is Phase 2; the type exists now so
/// the design is stable. Orthogonal to `CatalogMode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatalogSource {
    Direct,
    ServerBacked,
}

/// Picker filter. `tab: None` = the "All" tab; `venue: None` = every venue.
#[derive(Clone, Default, Debug)]
pub struct SearchFilter {
    pub tab: Option<Tab>,
    pub venue: Option<String>,
}

/// A catalog fetch/parse failure. Never carries secrets.
#[derive(Debug, Clone)]
pub struct CatalogError(pub String);

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "catalog error: {}", self.0)
    }
}
impl std::error::Error for CatalogError {}

/// One venue's contribution to the catalog.
pub trait CatalogProvider: Send + Sync {
    fn venue(&self) -> &str;
    fn asset_classes(&self) -> &[AssetClass];
    fn mode(&self) -> CatalogMode;

    /// Enumerable venues: the full universe, fetched off-UI-thread. QueryBacked returns empty.
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        Ok(Vec::new())
    }

    /// QueryBacked venues: live per-query search (debounced, off-UI-thread). Enumerable defaults.
    fn search_remote(
        &self,
        _query: &str,
        _filter: &SearchFilter,
    ) -> Result<Vec<Instrument>, CatalogError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetClass, Instrument, Tab};

    struct FakeEnumerable;
    impl CatalogProvider for FakeEnumerable {
        fn venue(&self) -> &str {
            "fake"
        }
        fn asset_classes(&self) -> &[AssetClass] {
            &[AssetClass::CryptoSpot]
        }
        fn mode(&self) -> CatalogMode {
            CatalogMode::Enumerable
        }
        fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
            Ok(vec![Instrument {
                venue: "fake".into(),
                raw_symbol: "BTCUSDT".into(),
                asset_class: AssetClass::CryptoSpot,
                base: "BTC".into(),
                quote: "USDT".into(),
                description: String::new(),
                properties: Default::default(),
            }])
        }
    }

    #[test]
    fn defaulted_search_remote_is_empty_for_enumerable() {
        let p = FakeEnumerable;
        assert_eq!(p.mode(), CatalogMode::Enumerable);
        assert_eq!(p.list_instruments().unwrap().len(), 1);
        assert!(p.search_remote("btc", &SearchFilter::default()).unwrap().is_empty());
    }

    #[test]
    fn filter_defaults_to_all() {
        let f = SearchFilter::default();
        assert!(f.tab.is_none() && f.venue.is_none());
        let _ = Tab::Crypto; // Tab in scope
    }
}
