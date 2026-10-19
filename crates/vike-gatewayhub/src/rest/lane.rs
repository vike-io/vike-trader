//! **`VenueRest`: the real `RestLane`** — the venue half of every non-stream verb, over the bridges.
//!
//! It owns no policy of its own. Each verb is answered by the module that holds its venue calls
//! (`kline_sources`, `ticks`, `oanda`, `catalog`, `options`, `family`), and this file is the dispatch,
//! the build facts, and the one place the pieces are named together. The server (`crate::server`)
//! refuses a lane or a venue `facts` does not serve BEFORE a call reaches here, so a method here
//! answers only what it was built to serve; each still refuses defensively what it was not, because a
//! lane must not depend on the server having looked.
//!
//! # `facts` are derived from what the lane DISPATCHES
//!
//! The kline, funding and catalog venues are read off the registries the lane will call, in their
//! order, so the advertisement (`Welcome.features`) and the dispatch cannot disagree; the tests pin
//! the registries equal to `crate::registry`'s typed table and to the client's rosters. The
//! credentialed lane is advertised only when the lane was built with a credential store: without one
//! there is no way to look for the token, and a venue that needs a credential answers
//! `CredentialPresence::NotChecked`, which is not `Absent` (nothing was read).
//!
//! # Construction is the composition root's, and so are the settings
//!
//! `VenueRest::new(hyperliquid_hip3)` takes the one setting a lane needs as a parameter (the library
//! reads no settings: only binaries read the credential store or the environment), and
//! [`VenueRest::with_credential_store`] takes the store HANDLE. The `with_*` seams replace one piece
//! with a scripted one for a suite; production never calls them.
//!
//! # Budget
//!
//! `budget_counters` answers whatever snapshot the root wired ([`VenueRest::with_budget_snapshot`]);
//! unwired it is empty. Wave 1's budget is shadow (counted and disclosed, nothing refused), so no
//! fetch here waits on it and `FetchCtx::set_state(Queued)` is never called.

use std::sync::Arc;

use vike_data::source::{FundingRateSource, KlineSource};
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::history::TICK_VENUE;
use vike_gatewayhub_client::proto::{
    BudgetCounter, FetchFailure, FetchRows, FetchWhat, OptionsCall, OptionsFeedSpec, OptionsReply,
};
use vike_gatewayhub_client::resolve::FAMILY_VENUE;

use super::catalog::CatalogProviders;
use super::family::FamilyHub;
use super::kline_sources::{
    FUNDING_SOURCES, KLINE_SOURCES, fetch_funding, fetch_klines, funding_venues, kline_venues,
};
use super::oanda::{OandaLane, VENUE as OANDA_VENUE};
use super::options::OptionsHub;
use super::ticks::{QuoteFetch, dukascopy_quotes, fetch_quotes};
use super::{CatalogTraits, FetchCtx, OptionsFeedHandle, OptionsFeedSink, RestFacts, RestLane};
use crate::session::ConnectionScope;

/// The budget counters the status report carries; wired by the composition root.
pub type BudgetSnapshot = Arc<dyn Fn() -> Vec<BudgetCounter> + Send + Sync>;

/// The real REST lane. See the module doc.
pub struct VenueRest {
    klines: &'static [&'static dyn KlineSource],
    funding: &'static [&'static dyn FundingRateSource],
    ticks: QuoteFetch,
    catalog: CatalogProviders,
    oanda: Option<OandaLane>,
    options: OptionsHub,
    families: FamilyHub,
    budget: BudgetSnapshot,
}

impl VenueRest {
    /// The production lane, with no credential store (so no credentialed lane): the kline and funding
    /// registries, the dukascopy `.bi5` fetch, the eight public catalog providers (`hyperliquid_hip3`
    /// is the process's resolved `flags.hyperliquid_hip3`), the persistent mainnet Deribit provider
    /// and the polymarket Gamma directory.
    pub fn new(hyperliquid_hip3: bool) -> Self {
        Self {
            klines: KLINE_SOURCES,
            funding: FUNDING_SOURCES,
            ticks: dukascopy_quotes(),
            catalog: CatalogProviders::real(hyperliquid_hip3),
            oanda: None,
            options: OptionsHub::mainnet(),
            families: FamilyHub::gamma(),
            budget: Arc::new(Vec::<BudgetCounter>::new),
        }
    }

    /// Arm the credentialed OANDA lane over `store` — the handle the boot resolved (a location and a
    /// scope, no value). Arming is storing the key on the practice account: this only says WHERE to
    /// look, per connection, when a credentialed fetch arrives.
    #[must_use]
    pub fn with_credential_store(mut self, store: vike_secrets::CredentialStore) -> Self {
        self.oanda = Some(OandaLane::new(store));
        self
    }

    /// Arm the credentialed lane with an explicit [`OandaLane`] — a suite's scripted source.
    #[must_use]
    pub fn with_oanda(mut self, lane: OandaLane) -> Self {
        self.oanda = Some(lane);
        self
    }

    /// Wire the budget counters the status report carries.
    #[must_use]
    pub fn with_budget_snapshot(mut self, snapshot: BudgetSnapshot) -> Self {
        self.budget = snapshot;
        self
    }

    /// Replace the kline registry (a suite's scripted sources).
    #[must_use]
    pub fn with_klines(mut self, sources: &'static [&'static dyn KlineSource]) -> Self {
        self.klines = sources;
        self
    }

    /// Replace the funding registry.
    #[must_use]
    pub fn with_funding(mut self, sources: &'static [&'static dyn FundingRateSource]) -> Self {
        self.funding = sources;
        self
    }

    /// Replace the tick fetch.
    #[must_use]
    pub fn with_ticks(mut self, fetch: QuoteFetch) -> Self {
        self.ticks = fetch;
        self
    }

    /// Replace the catalog table.
    #[must_use]
    pub fn with_catalog(mut self, catalog: CatalogProviders) -> Self {
        self.catalog = catalog;
        self
    }

    /// Replace the options hub.
    #[must_use]
    pub fn with_options(mut self, options: OptionsHub) -> Self {
        self.options = options;
        self
    }

    /// Replace the family lane.
    #[must_use]
    pub fn with_families(mut self, families: FamilyHub) -> Self {
        self.families = families;
        self
    }

    /// The OANDA lane, if armed (the root's startup read asks it for the store handle).
    pub fn oanda(&self) -> Option<&OandaLane> {
        self.oanda.as_ref()
    }
}

impl std::fmt::Debug for VenueRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VenueRest").field("facts", &self.facts()).finish_non_exhaustive()
    }
}

/// The refusal for a request this lane was not built to serve: the venue was NOT asked.
fn refused(text: String) -> FetchFailure {
    FetchFailure::Refused(format!("gatewayhub: {text}; nothing was asked of any venue"))
}

impl RestLane for VenueRest {
    fn facts(&self) -> RestFacts {
        RestFacts {
            klines: kline_venues(self.klines),
            funding: funding_venues(self.funding),
            ticks: vec![TICK_VENUE.to_string()],
            catalog: self.catalog.venues(),
            credentialed: if self.oanda.is_some() {
                vec![OANDA_VENUE.to_string()]
            } else {
                Vec::new()
            },
            options: true,
            family: vec![FAMILY_VENUE.to_string()],
            presence: true,
        }
    }

    fn fetch(
        &self,
        scope: &ConnectionScope,
        what: &FetchWhat,
        credentialed: bool,
        ctx: &FetchCtx,
    ) -> Result<FetchRows, FetchFailure> {
        if credentialed {
            // The credentialed verb serves one lane: klines of the venue whose credential this daemon
            // holds. Anything else is refused here exactly as the server refuses it.
            let FetchWhat::Klines { venue, symbol, interval, start_ms, end_ms } = what else {
                return Err(refused(format!(
                    "FetchCredentialed serves klines only; a {} fetch is not credentialed",
                    what.lane()
                )));
            };
            let Some(lane) = self.oanda.as_ref().filter(|_| venue == OANDA_VENUE) else {
                return Err(refused(format!("`{venue}` has no credentialed kline lane")));
            };
            return lane
                .fetch(scope, symbol, interval, *start_ms, *end_ms, ctx)
                .map(FetchRows::Bars);
        }
        match what {
            FetchWhat::Klines { venue, symbol, interval, start_ms, end_ms } => {
                fetch_klines(self.klines, venue, symbol, interval, *start_ms, *end_ms, ctx)
                    .map(FetchRows::Bars)
            }
            FetchWhat::Funding { venue, symbol, start_ms, end_ms } => {
                fetch_funding(self.funding, venue, symbol, *start_ms, *end_ms, ctx)
                    .map(FetchRows::Funding)
            }
            FetchWhat::Ticks { venue, symbol, start_ms, end_ms } => {
                if venue != TICK_VENUE {
                    return Err(refused(format!("`{venue}` has no tick lane")));
                }
                fetch_quotes(self.ticks.as_ref(), symbol, *start_ms, *end_ms, ctx)
                    .map(FetchRows::Quotes)
            }
            FetchWhat::CatalogList { venue } => {
                self.catalog.list(venue, ctx).map(FetchRows::Instruments)
            }
            FetchWhat::CatalogSearch { venue, query, tab, filter_venue } => self
                .catalog
                .search(venue, query, *tab, filter_venue.clone(), ctx)
                .map(FetchRows::Instruments),
            FetchWhat::OptionIndex { kind } => {
                self.options.instruments(*kind, ctx).map(FetchRows::Instruments)
            }
        }
    }

    fn catalog_describe(
        &self,
        _scope: &ConnectionScope,
        venue: &str,
    ) -> Result<CatalogTraits, String> {
        self.catalog.describe(venue)
    }

    fn options(
        &self,
        _scope: &ConnectionScope,
        call: &OptionsCall,
    ) -> Result<OptionsReply, String> {
        self.options.call(call)
    }

    fn open_options_feed(
        &self,
        _scope: &ConnectionScope,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn OptionsFeedSink>,
    ) -> Result<Box<dyn OptionsFeedHandle>, String> {
        self.options.open_feed(spec, sink)
    }

    fn resolve_family(
        &self,
        scope: &ConnectionScope,
        venue: &str,
        family: &str,
        now_ms: i64,
    ) -> Result<Vec<String>, String> {
        if venue != FAMILY_VENUE {
            return Err(format!(
                "gatewayhub: `{venue}` has no rolling family lane (served: [{FAMILY_VENUE}]); \
                 nothing was asked of any venue"
            ));
        }
        self.families.resolve(scope, family, now_ms)
    }

    fn credential_presence(&self, venue: &str) -> CredentialPresence {
        if venue != OANDA_VENUE {
            // A keyless lane, or a venue no lane serves: no credential is needed for it.
            return CredentialPresence::NotNeeded;
        }
        match &self.oanda {
            Some(lane) => lane.presence(),
            // The venue needs a credential and this lane was built without a way to look.
            None => CredentialPresence::NotChecked,
        }
    }

    fn budget_counters(&self) -> Vec<BudgetCounter> {
        (self.budget)()
    }
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod lane_tests;
