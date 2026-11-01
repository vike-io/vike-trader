//! **The venue-catalog providers**: the eight public providers of the datahub's `real_catalog_table`,
//! MOVED here (decision 0164: the datahub keeps the bounds only the SERVER can have — the listing
//! cap, the TTL memo, the per-venue buckets — and asks this daemon for the universe), and the
//! `CatalogDescribe` / `Fetch { CatalogList | CatalogSearch }` answers built from them.
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` is the verdict
//! that governs the MEMBERSHIP, and it is the load-bearing bound here as it was there:
//!
//! # ⚠ Only a provider that reads a PUBLIC endpoint may be in this table
//!
//! A catalog request is a Read-scope request, and no rate limit bounds spending an identity. Three
//! roster venues (alpaca, oanda, ctrader) can be listed only by authenticating as the operator, so
//! they are excluded by CONSTRUCTION: [`CatalogProviders::new`] refuses a venue
//! `vike_catalog::catalog_availability` does not call public, where a request door could be
//! reordered past, and a table that cannot HOLD the venue has no ordering to get wrong. A refused
//! entry is reported by [`CatalogProviders::refused_entries`] so a mount can log what it declined.
//!
//! # What the daemon sends, and what it does not
//!
//! * **The provider's FULL answer.** `CATALOG_MAX_INSTRUMENTS` is the datahub's cap and stays there
//!   (the daemon's reply is paged, so a big listing is not a frame problem): the lane truncates
//!   nothing, and the datahub applies its cap exactly where `list_via` did.
//! * **The provider's words.** A `CatalogError`'s inner text crosses unchanged as
//!   `FetchFailure::Fetch`, so the datahub's own `CatalogError` wrapper prints what the in-process
//!   path printed. A failure is `Err`, never an empty list: an empty list is a legitimate venue fact
//!   the picker would cache.
//! * **A provider per call.** The factories build a fresh provider for every describe, list and
//!   search, because the datahub built one per call (`HyperliquidCatalog` "holds state and has a
//!   `new()`"): a provider kept across calls could remember what the in-process one forgot.
//!
//! # Cancellation: what stops and what cannot
//!
//! A provider's `list_instruments` is one call with its pager inside (polymarket walks up to 40
//! pages, okx one request per option family) and no probe, so a cancel is seen BEFORE the call only.

use vike_catalog::{CatalogProvider, SearchFilter, catalog_availability};
use vike_gatewayhub_client::proto::{FetchFailure, WireCatalogMode, WireTab};
use vike_model::Instrument;

use super::{CatalogTraits, FetchCtx, cancelled};

/// Builds a fresh provider for one call. `Send + Sync` because the lane is shared by every
/// connection.
pub type ProviderFactory = Box<dyn Fn() -> Box<dyn CatalogProvider> + Send + Sync>;

/// One `(venue, factory)` entry.
fn entry<P: CatalogProvider + 'static>(
    venue: &str,
    make: impl Fn() -> P + Send + Sync + 'static,
) -> (String, ProviderFactory) {
    (venue.to_string(), Box::new(move || Box::new(make()) as Box<dyn CatalogProvider>))
}

/// The venue → provider table, in declared order (the order is observable: it is what a refusal
/// prints and what the client's `CATALOG_VENUES` mirrors).
pub struct CatalogProviders {
    entries: Vec<(String, ProviderFactory)>,
}

impl CatalogProviders {
    /// A table from explicit `(venue, factory)` entries — the test seam.
    ///
    /// ⚠ **It REFUSES a venue that is not publicly enumerable**, silently dropping it rather than
    /// panicking, and that is 0062's decision 3 enforced where it cannot be reordered past: a
    /// credentialed venue (alpaca/oanda/ctrader) cannot be in this table even by mistake, so no
    /// request door needs to remember to check. Each dropped entry earns a `tracing::error!`.
    ///
    /// Production code goes through [`CatalogProviders::real`] instead, so a fake can never be
    /// mounted by accident: this constructor takes closures, and the only closures naming real
    /// providers live there.
    pub fn new(entries: Vec<(String, ProviderFactory)>) -> Self {
        let (kept, refused): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|(v, _)| catalog_availability(v).is_public());
        for (venue, _) in &refused {
            tracing::error!(
                venue = %venue,
                "vike-gatewayhub catalog: REFUSING a provider entry — `vike_catalog::\
                 catalog_availability` does not classify this venue as publicly enumerable, and a \
                 server will not spend the operator's venue credentials on a client's request \
                 (docs/decisions/0062, decision 3). The venue will answer NeedsCredentials or \
                 NotServed; nothing was mounted for it."
            );
        }
        Self { entries: kept }
    }

    /// Which of `entries` [`CatalogProviders::new`] would drop — for a mount's own log line. Empty
    /// for [`CatalogProviders::real`], whose entries are all public by construction.
    pub fn refused_entries(entries: &[(String, ProviderFactory)]) -> Vec<String> {
        entries
            .iter()
            .filter(|(v, _)| !catalog_availability(v).is_public())
            .map(|(v, _)| v.clone())
            .collect()
    }

    /// The PRODUCTION table: **eight** venue catalogs, each reading a PUBLIC endpoint or no endpoint
    /// at all.
    ///
    /// ⚠ **Six of the eight read a keyless endpoint**: binance, bybit, okx, aster, hyperliquid and
    /// polymarket each declare `pub mod catalog;` UNGATED, so a feeds build compiles these providers.
    ///
    /// ⚠ **dukascopy and fxcm are the CHEAPEST entries in the table.** Both providers are pure folds
    /// over a `const` table — `crates/bridges/dukascopy/src/catalog.rs`'s `FX_TABLE` (49 rows) and
    /// `crates/bridges/fxcm/src/catalog.rs`'s `FX_TABLE` (41) — so a listing costs **zero venue
    /// requests, zero credential reads and no network**, which is what
    /// `vike_catalog::catalog_availability` records for both. fxcm's compiles no C++ either: its
    /// `build.rs` returns immediately without `CARGO_FEATURE_FXCM`, which no path from this crate
    /// enables.
    ///
    /// ⚠ **Their SPELLING differs from the six and will not compile if copied.** Both bridges declare
    /// `mod catalog;` PRIVATE and re-export at the crate root (`vike_dukascopy::DukascopyCatalog`,
    /// `vike_fxcm::FxcmCatalog`), where the other six go through `pub mod catalog;`. The crate-root
    /// path is the canonical public one for this pair; widening their modules to match would be a
    /// second name for one symbol, which the root `CLAUDE.md`'s no-shim rule is about.
    ///
    /// ⚠ **Six of the fourteen roster venues are absent, in three classes**, and only one of them is
    /// a scope question (0062's consequences enumerate all three):
    ///
    /// * **alpaca / oanda / ctrader** — credentialed, refused by decision 3. Admitting one is a
    ///   reopener, and [`CatalogProviders::new`] would drop it anyway.
    /// * **ig / ibkr** — no bulk list at any price. Nothing to add.
    /// * **deribit** — publicly enumerable, and the ONE venue the DESKTOP still links and routes
    ///   `Direct` (`vike_catalog::catalog_source_for`). An entry for it would be a dependency nothing
    ///   asks for, so it is left out deliberately rather than pending — a separate finding that this
    ///   move does not touch.
    ///
    /// `hyperliquid_hip3` is the process's resolved `flags.hyperliquid_hip3` — the flag the mount
    /// reads (decision 0095) — so the picker's universe and the mount's symbology agree about HIP-3
    /// builder-dex markets. The wire's `CatalogList` carries no such field, so it is a parameter of
    /// the table the composition root builds.
    ///
    /// A new venue's entry is deliberately NOT generated by `just new-venue`: an entry in this table
    /// is what makes a SERVER fetch that venue's universe on an untrusted Read client's request, and
    /// it is only correct once two facts hold that a generator cannot see —
    /// `vike_catalog::catalog_availability` classifies the venue as `PublicBulk` (the scaffolded row
    /// there is the fail-closed `NoBulkList`, and [`CatalogProviders::new`] would DROP a non-public
    /// entry with a `tracing::error!`: a row added blind is dead weight that reads as coverage), and
    /// the manifest names the bridge under `data`. Add the entry in the PR that classifies the venue,
    /// with the request COST measured and written into the availability row
    /// (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` decision 3
    /// is what a wrong entry breaks). The marker that tells the scaffold so is the `new-venue` note on
    /// this function; it moved here from the datahub's `real_catalog_table` when the providers did
    /// (the datahub's table now follows the client's `CATALOG_VENUES` and names no venue).
    // vike:new-venue:note do NOT add an `entry("{venue}", || …)` here yet. An entry in THIS table is what makes the daemon fetch that venue's universe on an untrusted Read client's request, and it is only correct once three facts hold that a generator cannot see: `vike_catalog::catalog_availability` classifies `{venue}` as `PublicBulk` (the scaffolded row there is the fail-closed `NoBulkList`, and `CatalogProviders::new` would DROP a non-public entry with a `tracing::error!` — a row added blind is dead weight that reads as coverage), this crate's manifest names that bridge under `data`, and the client crate's `CATALOG_VENUES` lists the venue in the same position (the datahub's catalog table folds that roster, and the roster test holds the two equal). Add the entry in the PR that classifies the venue, with the request COST measured and written into the availability row — `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` decision 3 is what a wrong entry breaks.
    pub fn real(hyperliquid_hip3: bool) -> Self {
        Self::new(vec![
            entry("binance", || vike_binance::catalog::BinanceCatalog),
            entry("bybit", || vike_bybit::catalog::BybitCatalog),
            entry("okx", || vike_okx::catalog::OkxCatalog),
            entry("aster", || vike_aster::catalog::AsterCatalog),
            // ⚠ The one entry here that is NOT a unit struct — `HyperliquidCatalog` holds state and
            // has a `new()`. Its four siblings are unit structs and polymarket's is too, which is
            // exactly why the `data` feature's lanes exist: nothing else compiles this line.
            entry("hyperliquid", move || {
                vike_hyperliquid::feeds::catalog::HyperliquidCatalog::new()
                    .with_hip3(hyperliquid_hip3)
            }),
            entry("polymarket", || vike_polymarket::catalog::PolymarketCatalog),
            // ⚠ The two BUNDLED-STATIC providers, and note the CRATE-ROOT path: these two bridges
            // declare `mod catalog;` private and re-export the type, so `vike_dukascopy::catalog::…`
            // does not resolve. See this function's doc.
            entry("dukascopy", || vike_dukascopy::DukascopyCatalog),
            entry("fxcm", || vike_fxcm::FxcmCatalog),
        ])
    }

    /// The venues this table can list, in declared order — the `catalog` lane's advertisement and the
    /// refusal's "served" set.
    pub fn venues(&self) -> Vec<String> {
        self.entries.iter().map(|(venue, _)| venue.clone()).collect()
    }

    /// A fresh provider for `venue`, or the refusal naming the served set. A venue the table does
    /// not hold is also every non-public venue: [`CatalogProviders::new`] never admitted it.
    fn provider(&self, venue: &str) -> Result<Box<dyn CatalogProvider>, String> {
        match self.entries.iter().find(|(v, _)| v == venue) {
            Some((_, make)) => Ok(make()),
            None => Err(format!(
                "gatewayhub: `{venue}` has no catalog provider on this daemon (served: [{}]); \
                 nothing was asked of any venue",
                self.venues().join(", ")
            )),
        }
    }

    /// `CatalogDescribe`: the provider's `asset_classes()` and `mode()`.
    pub fn describe(&self, venue: &str) -> Result<CatalogTraits, String> {
        let provider = self.provider(venue)?;
        Ok(CatalogTraits {
            asset_classes: provider.asset_classes().to_vec(),
            mode: WireCatalogMode::from_mode(provider.mode()),
        })
    }

    /// `Fetch { CatalogList }`: the provider's whole universe, uncapped.
    pub fn list(&self, venue: &str, ctx: &FetchCtx) -> Result<Vec<Instrument>, FetchFailure> {
        let provider = self.provider(venue).map_err(FetchFailure::Refused)?;
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        provider.list_instruments().map_err(|e| FetchFailure::Fetch(e.0))
    }

    /// `Fetch { CatalogSearch }`: a QUERY-BACKED provider's live search under the picker's filter.
    pub fn search(
        &self,
        venue: &str,
        query: &str,
        tab: Option<WireTab>,
        filter_venue: Option<String>,
        ctx: &FetchCtx,
    ) -> Result<Vec<Instrument>, FetchFailure> {
        let provider = self.provider(venue).map_err(FetchFailure::Refused)?;
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        let filter = SearchFilter { tab: tab.map(WireTab::tab), venue: filter_venue };
        provider.search_remote(query, &filter).map_err(|e| FetchFailure::Fetch(e.0))
    }
}

impl std::fmt::Debug for CatalogProviders {
    /// Venue names only — the factories have nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogProviders").field("venues", &self.venues()).finish()
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
