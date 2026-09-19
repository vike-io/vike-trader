//! The venue contribution seam. Two population modes: `Enumerable` venues hand over a full list
//! (cached + searched locally); `QueryBacked` venues (IBKR-scale) are searched live per query.
//! `CatalogSource` is the orthogonal client-side axis (`Direct` venues vs `ServerBacked` ones, the
//! latter listed by whichever datahub the caller's backend advertises).

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

/// Where the CLIENT gets catalog data, decided per venue by [`crate::catalog_source_for`].
/// Orthogonal to [`CatalogMode`].
///
/// ⚠ **This doc said "`ServerBacked` (the CI box) is Phase 2; the type exists now so the design is
/// stable" until 2026-09-16, and both halves of that have moved on.** The variant is live —
/// `crates/vike-app-core/src/catalog_wire.rs`'s `fetch_venue_catalog` sends the verb and
/// `crates/vike-datahub/src/catalog.rs`'s `real_catalog_table` answers it — and the server is not
/// "the CI box" but whichever datahub the caller's active backend advertises, which on a developer box
/// is a loopback port.
/// `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` is the
/// scope decision behind it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CatalogSource {
    /// The caller LINKS this venue's `CatalogProvider` and fetches it itself. No server is needed
    /// and none should be asked.
    Direct,
    /// The caller does not link it, and it is publicly enumerable, so a datahub carrying the
    /// provider can list it. ⚠ This says only that the route EXISTS: whether a PARTICULAR server
    /// will is answered by the handshake (`vike_datahub_client::FEATURE_VENUE_CATALOG`) and refined
    /// by the response (`CatalogOutcome::NotArmed`, `CatalogRefusal::NotServed`).
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
                contract_type: None,
                settle_asset: None,
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
