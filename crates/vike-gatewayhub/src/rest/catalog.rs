//! **The venue-catalog providers**: the providers of the datahub's `real_catalog_table`,
//! MOVED here (decision 0164: the datahub keeps the bounds only the SERVER can have — the listing
//! cap, the TTL memo, the per-venue buckets — and asks this daemon for the universe), and the
//! `CatalogDescribe` / `Fetch { CatalogList | CatalogSearch }` answers built from them.
//!
//! `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` is the verdict
//! that governs the MEMBERSHIP, and it is the load-bearing bound here as it was there:
//!
//! # ⚠ Only a provider that reads a PUBLIC endpoint, or a session this daemon ALREADY holds, may be in this table
//!
//! A catalog request is a Read-scope request, and no rate limit bounds spending an identity. A
//! roster venue that can be listed only by authenticating as the operator (alpaca) is
//! excluded by CONSTRUCTION: [`CatalogProviders::new`] refuses a venue
//! `vike_catalog::catalog_availability` does not call served (`is_served`), where a request door
//! could be reordered past, and a table that cannot HOLD the venue has no ordering to get wrong. A
//! refused entry is reported by [`CatalogProviders::refused_entries`] so a mount can log what it
//! declined. A non-public member is a `HeldSession` venue (0062's decision 3 as amended
//! 2026-11-13; fxcm, whose provider reads the Offers table of its reconcile session, and dukascopy,
//! whose sidecar's login pushes the account's list): its provider
//! reads a session a running account already holds, or the list its login pushed
//! (`vike_catalog::held_list`), and never logs in; with no such session its `no_session` answers and
//! [`CatalogProviders::list`] sends a `SourceError::Refused` having asked no venue, which the
//! datahub folds as no change.
//!
//! # What the daemon sends, and what it does not
//!
//! * **The provider's FULL answer.** `CATALOG_MAX_INSTRUMENTS` is the datahub's cap and stays there
//!   (the daemon's reply is paged, so a big listing is not a frame problem): the lane truncates
//!   nothing, and the datahub applies its cap exactly where `list_via` did.
//! * **The provider's words.** A `CatalogError`'s inner text crosses unchanged as
//!   `SourceError::Fetch`, so the datahub's own `CatalogError` wrapper prints what the in-process
//!   path printed. A failure is `Err`, never an empty list: an empty list is a legitimate venue fact
//!   the picker would cache.
//! * **A provider per call.** The factories build a fresh provider for every describe, list and
//!   search, because the datahub built one per call (`CatalogProviderHyperliquid` "holds state and has a
//!   `new()`"): a provider kept across calls could remember what the in-process one forgot.
//!
//! # Cancellation: what stops and what cannot
//!
//! A provider's `list_instruments` is one call with its pager inside (polymarket walks up to 40
//! pages, okx one request per option family) and no probe, so a cancel is seen BEFORE the call only.

use std::collections::BTreeMap;

use vike_catalog::{CatalogProvider, CatalogSearchFilter, PickerTab, catalog_availability};
use vike_data::source::SourceError;
use vike_model::Instrument;
use vike_secrets::venue_setting::VenueSettings;
use vike_venues::venue_urls::UrlRefusal;

use crate::data::venues::venue_urls;

use super::{CatalogTraits, FetchContext, cancelled};

/// Builds a fresh provider for one call. `Send + Sync` because the lane is shared by every
/// connection.
pub type CatalogProviderFactory = Box<dyn Fn() -> Box<dyn CatalogProvider> + Send + Sync>;

/// One `(venue, factory)` entry.
fn entry<P: CatalogProvider + 'static>(
    venue: &str,
    make: impl Fn() -> P + Send + Sync + 'static,
) -> (String, CatalogProviderFactory) {
    (venue.to_string(), Box::new(move || Box::new(make()) as Box<dyn CatalogProvider>))
}

/// One `(venue, factory)` entry whose provider was built ONCE on the venue's address rows
/// (`crate::data::venues::venue_urls`, decision 0265). A refused build stays in the table in its
/// place, as a provider that refuses every list with the reason (logged once here): a row and a
/// restart cure it, so the venue is not advertised away.
fn entry_on<P: CatalogProvider + Clone + Send + Sync + 'static>(
    venue: &'static str,
    built: Result<P, UrlRefusal>,
) -> (String, CatalogProviderFactory) {
    match built {
        Ok(provider) => entry(venue, move || provider.clone()),
        Err(why) => {
            let why = format!("{venue} catalog is NOT RUNNING: {why}");
            tracing::error!("vike-gatewayhub: {why}");
            entry(venue, move || Unaddressed { venue, why: why.clone() })
        }
    }
}

/// A FX + CFD broker's list as this daemon holds it (`vike_catalog::held_list`).
fn held(venue: &'static str) -> vike_catalog::held_list::CatalogProviderHeldList {
    vike_catalog::held_list::CatalogProviderHeldList {
        venue,
        classes: &[vike_model::AssetClass::Fx, vike_model::AssetClass::Cfd],
    }
}

/// The provider in the place of one whose address rows refused it ([`entry_on`]).
struct Unaddressed {
    venue: &'static str,
    why: String,
}

impl CatalogProvider for Unaddressed {
    fn venue(&self) -> &str {
        self.venue
    }
    fn asset_classes(&self) -> &[vike_model::AssetClass] {
        &[]
    }
    fn mode(&self) -> vike_catalog::CatalogMode {
        vike_catalog::CatalogMode::Enumerable
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, vike_catalog::CatalogError> {
        Err(vike_catalog::CatalogError(self.why.clone()))
    }
}

/// The venue → provider table, in declared order (the order is observable: it is what a refusal
/// prints and what the client's `CATALOG_VENUES` mirrors).
pub struct CatalogProviders {
    entries: Vec<(String, CatalogProviderFactory)>,
}

impl CatalogProviders {
    /// A table from explicit `(venue, factory)` entries — the test seam.
    ///
    /// ⚠ **It REFUSES a venue that is not served** (`vike_catalog::CatalogAvailability::is_served`:
    /// public, or listed from a held session), silently dropping it rather than
    /// panicking, and that is 0062's decision 3 enforced where it cannot be reordered past: a
    /// credentialed venue (alpaca) cannot be in this table even by mistake, so no
    /// request door needs to remember to check. Each dropped entry earns a `tracing::error!`.
    ///
    /// Production code goes through [`CatalogProviders::real`] instead, so a fake can never be
    /// mounted by accident: this constructor takes closures, and the only closures naming real
    /// providers live there.
    pub fn new(entries: Vec<(String, CatalogProviderFactory)>) -> Self {
        let (kept, refused): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|(v, _)| catalog_availability(v).is_served());
        for (venue, _) in &refused {
            tracing::error!(
                venue = %venue,
                "vike-gatewayhub catalog: REFUSING a provider entry — `vike_catalog::\
                 catalog_availability` does not classify this venue as publicly enumerable or \
                 listed from a held session, and a server will not spend the operator's venue \
                 credentials on a client's request (docs/decisions/0062, decision 3). The venue \
                 will answer NeedsCredentials or NotServed; nothing was mounted for it."
            );
        }
        Self { entries: kept }
    }

    /// Which of `entries` [`CatalogProviders::new`] would drop — for a mount's own log line. Empty
    /// for [`CatalogProviders::real`], whose entries are all served by construction.
    pub fn refused_entries(entries: &[(String, CatalogProviderFactory)]) -> Vec<String> {
        entries
            .iter()
            .filter(|(v, _)| !catalog_availability(v).is_served())
            .map(|(v, _)| v.clone())
            .collect()
    }

    /// The PRODUCTION table: **ten** venue catalogs, each reading a PUBLIC endpoint, a session this daemon
    /// already holds (fxcm, dukascopy), or what a running account already handed this daemon (oanda,
    /// ctrader: [`held`]).
    ///
    /// ⚠ **Six of the ten read a keyless endpoint**: binance, bybit, okx, aster, hyperliquid and
    /// polymarket each declare `pub mod catalog;` UNGATED, so a feeds build compiles these providers.
    ///
    /// ⚠ **dukascopy lists what a RUNNING account's login received** (`vike_catalog::held_list`,
    /// decision 0062's decision 3 as amended 2026-11-13): the JForex sidecar states the account's list
    /// once per login and `crates/bridges/dukascopy/src/catalog.rs`'s `hold` pushes it, so a listing
    /// costs **zero venue requests and no login**; until a login has pushed one the entry's
    /// `no_session` answers.
    ///
    /// ⚠ **fxcm lists from a RUNNING account's session** (`vike_fxcm::CatalogProviderFxcm`, decision
    /// 0062's decision 3 as amended 2026-11-13): one Offers-table refresh on the reconcile session the
    /// account already holds, never a login; no running account is its `no_session` refusal. It
    /// compiles no C++: its `build.rs` returns immediately without `CARGO_FEATURE_FXCM`, and the
    /// session is opened at runtime by the trade half.
    ///
    /// ⚠ **Their SPELLING differs from the six and will not compile if copied.** fxcm declares
    /// `mod catalog;` PRIVATE and re-exports its provider at the crate root
    /// (`vike_fxcm::CatalogProviderFxcm`), and dukascopy's entry is the shared held-list provider over
    /// the classes its crate root states (`vike_dukascopy::CATALOG_CLASSES`), where the other six go
    /// through `pub mod catalog;`. Widening their modules to match would be a second name for one
    /// symbol, which the root `CLAUDE.md`'s no-shim rule is about.
    ///
    /// ⚠ **Four of the fourteen roster venues are absent, in three classes**, and only one of them is
    /// a scope question (0062's consequences enumerate all three):
    ///
    /// * **alpaca** — credentialed, refused by decision 3. Admitting it is a reopener, and
    ///   [`CatalogProviders::new`] would drop it anyway (oanda and ctrader were admitted as HELD lists,
    ///   decision 3's 2026-11-13 amendment).
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
    /// `venue_settings` is the boot's one read of every venue's rows: an entry builds its provider on
    /// its venue's addresses (`crate::data::venues::venue_urls`, decision 0265).
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
    pub fn real(hyperliquid_hip3: bool, venue_settings: &BTreeMap<String, VenueSettings>) -> Self {
        let urls = |venue| venue_urls(venue_settings, venue);
        let (hyperliquid, polymarket) = (urls("hyperliquid"), urls("polymarket"));
        Self::new(vec![
            // The four CEX providers hold the hosts their rows name, built once here ([`entry_on`]).
            entry_on(
                "binance",
                vike_binance::catalog::CatalogProviderBinance::from_urls(&urls("binance")),
            ),
            entry_on("bybit", vike_bybit::catalog::CatalogProviderBybit::from_urls(&urls("bybit"))),
            entry_on("okx", vike_okx::catalog::CatalogProviderOkx::from_urls(&urls("okx"))),
            entry_on("aster", vike_aster::catalog::CatalogProviderAster::from_urls(&urls("aster"))),
            // ⚠ `CatalogProviderHyperliquid` holds state and has a `new()` (polymarket's holds its Gamma row),
            // which is exactly why the `data` feature's lanes exist: nothing else compiles this line.
            entry("hyperliquid", move || {
                vike_hyperliquid::catalog::CatalogProviderHyperliquid::new(&hyperliquid)
                    .with_hip3(hyperliquid_hip3)
            }),
            entry("polymarket", move || {
                vike_polymarket::catalog::CatalogProviderPolymarket::new(&polymarket)
            }),
            // ⚠ Note the CRATE-ROOT paths: these two bridges declare `mod catalog;` private. See this
            // function's doc: dukascopy's list is the one its sidecar's login pushed, fxcm's is read
            // from a held session.
            entry("dukascopy", || vike_catalog::held_list::CatalogProviderHeldList {
                venue: "dukascopy",
                classes: vike_dukascopy::CATALOG_CLASSES,
            }),
            entry("fxcm", || vike_fxcm::CatalogProviderFxcm),
            // ⚠ The two HELD lists (decision 0062's decision 3, amended 2026-11-13): what a running account's symbol
            // facts already read in this daemon (`TradeLaneBridge`'s `hold_listing`), answered from the slot with no
            // request; `no_session` until one was read.
            entry("oanda", || held("oanda")),
            entry("ctrader", || held("ctrader")),
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
            mode: provider.mode(),
        })
    }

    /// `Fetch { CatalogList }`: the provider's whole universe, uncapped; `Refused` with the provider's
    /// words when it lists from a session this daemon does not hold now (`no_session`).
    pub fn list(&self, venue: &str, ctx: &FetchContext) -> Result<Vec<Instrument>, SourceError> {
        let provider = self.provider(venue).map_err(SourceError::Refused)?;
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        if let Some(why) = provider.no_session() {
            return Err(SourceError::Refused(why));
        }
        provider.list_instruments().map_err(|e| SourceError::Fetch(e.0))
    }

    /// `Fetch { CatalogSearch }`: a QUERY-BACKED provider's live search under the picker's filter.
    pub fn search(
        &self,
        venue: &str,
        query: &str,
        tab: Option<PickerTab>,
        filter_venue: Option<String>,
        ctx: &FetchContext,
    ) -> Result<Vec<Instrument>, SourceError> {
        let provider = self.provider(venue).map_err(SourceError::Refused)?;
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        let filter = CatalogSearchFilter { tab, venue: filter_venue };
        provider.search_remote(query, &filter).map_err(|e| SourceError::Fetch(e.0))
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
