//! **`RestLaneBridge`: the real `RestLane`** — the venue half of every non-stream verb, over the bridges.
//!
//! It owns no policy of its own. Each verb is answered by the module that holds its venue calls
//! (`kline_sources`, `ticks` over the tick venue's spec, `credentialed_klines` over each credentialed
//! venue's spec, `catalog`, `options`, `family`, `market_rules`), and this file is the dispatch, the build facts, and the
//! one place the pieces are named together: the bridges' specs are built in [`RestLaneBridge::new`] and, for
//! the credentialed venues, in [`credentialed_specs`]. The server (`crate::server`)
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
//! `RestLaneBridge::new(hyperliquid_hip3, venue_settings)` takes what a lane needs as parameters: the one
//! flag, and the boot's one read of every venue's rows, whose address rows (decision 0265) the
//! kline and funding registries, the catalog and the options hub are built on (the library reads no
//! settings: only binaries read the credential store or the environment), and
//! [`RestLaneBridge::with_credential_store`] takes the store HANDLE and the same read (the credentialed
//! lanes' addresses). The `with_*` seams replace one piece
//! with a scripted one for a suite; production never calls them.
//!
//! # Budget
//!
//! `budget_counters` answers whatever snapshot the root wired ([`RestLaneBridge::with_budget_snapshot`]);
//! unwired it is empty. Wave 1's budget is shadow (counted and disclosed, nothing refused), so no
//! fetch here waits on it and `FetchContext::set_state(Queued)` is never called.

use std::collections::BTreeMap;
use std::sync::Arc;

use vike_bridge_core::credentialed_klines::CredentialedKlinesSpec;
use vike_bridge_core::ticks::TicksSpec;
use vike_data::source::{
    FundingRateSource, KlineSource, MarketRulesSource, OptionTradeSource, SourceError,
};
use vike_gatewayhub_client::history::TICK_VENUE;
use vike_gatewayhub_client::proto::{
    BudgetCounter, FetchRows, FetchWhat, GATEWAY_TICKS_MAX_SPAN_MS, OptionsCall, OptionsFeedSpec,
    OptionsReply,
};
use vike_gatewayhub_client::resolve::FAMILY_VENUE;
use vike_model::CredentialPresence;
use vike_secrets::venue_setting::VenueSettings;

use super::catalog::CatalogProviders;
use super::credentialed_klines::CredentialedKlinesLane;
use super::family::FamilyHub;
use super::kline_sources::{
    fetch_funding, fetch_klines, funding_sources, funding_venues, kline_sources, kline_venues,
    option_trade_source,
};
use super::market_rules::{fetch_market_rules, market_rules_source};
use super::options::OptionsHub;
use super::ticks::fetch_ticks;
use super::{
    CatalogTraits, FeedOptionsHandle, FeedOptionsSink, FetchContext, RestLane, RestRosters,
};
use crate::registry::{RosterLane, roster};
use crate::session::ConnectionMemos;

/// The budget counters the status report carries; wired by the composition root.
pub type BudgetCounterSource = Arc<dyn Fn() -> Vec<BudgetCounter> + Send + Sync>;

/// **Every credentialed kline venue's spec, built over `store`** — the handle the boot resolved (a
/// location and a scope, no value) — each dialling its venue's `venue_url` rows out of `venue_settings`
/// (the daemon's one read; a venue with no entry has no rows, so every lookup is refused, naming the
/// row). In the registry's order (`crate::registry::ROSTERS`' credentialed klines row). The one place the
/// bridges' specs are named: [`RestLaneBridge::with_credential_store`] arms them, and the root's startup lines
/// are theirs.
///
/// cTrader's grant is found through the strategy lane's account read (`crate::data::accounts`'
/// `AccountSourceStore`, narrowed to `NoLiveKeys`), and its presence word asks the same narrowed handle
/// (`feed_store`): the store handle is narrowed HERE, once, for both.
///
/// `fx_clock` is the FX login gate's clock the IG and cTrader lanes log in on (`IgConfig::fx_clock`,
/// `ConnectionConfig::fx_clock`): no login of theirs while the FX market is closed at its time. `None` is no gate.
pub(crate) fn credentialed_specs(
    store: &vike_secrets::CredentialStore,
    venue_settings: &BTreeMap<String, VenueSettings>,
    fx_clock: Option<&Arc<dyn vike_model::Clock + Send + Sync>>,
) -> Vec<Box<dyn CredentialedKlinesSpec>> {
    use crate::data::accounts::{AccountSourceStore, feed_store};

    let urls = |venue: &str| crate::data::venues::venue_urls(venue_settings, venue);
    let accounts = AccountSourceStore::source(store.clone());
    vec![
        Box::new(vike_oanda::credentialed_klines::CredentialedKlinesSpecOanda::new(
            store.clone(),
            &urls(vike_oanda::credentialed_klines::VENUE),
        )),
        Box::new(
            vike_ig::credentialed_klines::CredentialedKlinesSpecIg::new(
                store.clone(),
                &urls(vike_ig::credentialed_klines::VENUE),
            )
            .with_fx_clock(fx_clock.cloned()),
        ),
        Box::new(
            vike_ctrader::credentialed_klines::CredentialedKlinesSpecCtrader::new(
                feed_store(store),
                Arc::new(move |venue: &str| accounts.directory(venue)),
                urls(vike_ctrader::credentialed_klines::VENUE),
            )
            .with_fx_clock(fx_clock.cloned()),
        ),
    ]
}

/// The real REST lane. See the module doc.
pub struct RestLaneBridge {
    klines: Vec<Box<dyn KlineSource>>,
    funding: Vec<Box<dyn FundingRateSource>>,
    /// The tick lane's venue spec (dukascopy's hour walk), bounded at the wire's span.
    ticks: Box<dyn TicksSpec>,
    /// The options surface's public trade log (deribit's), `Fetch { OptionTrades }`.
    option_trades: Box<dyn OptionTradeSource>,
    catalog: CatalogProviders,
    /// The credentialed kline lanes, in the registry's order (`crate::registry::ROSTERS`' credentialed
    /// klines row).
    credentialed: Vec<CredentialedKlinesLane>,
    options: OptionsHub,
    families: FamilyHub,
    /// The prediction-market rules texts (polymarket's Gamma), `Fetch { ClosedMarketRules | SeriesMarketRules |
    /// MarketRulesOfToken }`.
    market_rules: Box<dyn MarketRulesSource>,
    budget: BudgetCounterSource,
}

impl RestLaneBridge {
    /// The production lane, with no credential store (so no credentialed lane): the kline and funding
    /// registries, the dukascopy `.bi5` fetch, the eight public catalog providers (`hyperliquid_hip3`
    /// is the process's resolved `flags.hyperliquid_hip3`), the persistent mainnet Deribit provider
    /// and the polymarket Gamma directory. `venue_settings` is the boot's one read of every venue's
    /// rows; the sources are BUILT from their venues' address rows here, once.
    pub fn new(hyperliquid_hip3: bool, venue_settings: &BTreeMap<String, VenueSettings>) -> Self {
        Self {
            klines: kline_sources(venue_settings),
            funding: funding_sources(venue_settings),
            ticks: Box::new(vike_dukascopy::ticks::TicksSpecDukascopy::from_urls(
                &crate::data::venues::venue_urls(venue_settings, "dukascopy"),
                GATEWAY_TICKS_MAX_SPAN_MS,
                vike_dukascopy::feed::process_bus(),
            )),
            option_trades: option_trade_source(venue_settings),
            catalog: CatalogProviders::real(hyperliquid_hip3, venue_settings),
            credentialed: Vec::new(),
            // Deribit's option prices are mainnet's whatever any account's tier: its `live` rows.
            options: OptionsHub::new(Box::new(vike_deribit::options::OptionsSpecDeribit::mainnet(
                &crate::data::venues::venue_urls(venue_settings, "deribit"),
            ))),
            families: FamilyHub::new(Box::new(
                vike_polymarket::catalog::rolling_family::FamilySpecPolymarket::gamma(
                    crate::data::venues::venue_urls(venue_settings, "polymarket"),
                ),
            )),
            market_rules: market_rules_source(venue_settings),
            budget: Arc::new(Vec::<BudgetCounter>::new),
        }
    }

    /// Arm the credentialed OANDA, IG and cTrader lanes over `store` — the handle the boot resolved (a
    /// location and a scope, no value). Arming is storing the key on the practice account (OANDA), the
    /// three fields on the demo account (IG) or the OAuth grant on the demo account (cTrader): this only
    /// says WHERE to look, per connection, when a credentialed fetch arrives. Each lane dials its
    /// venue's `venue_url` rows out of `venue_settings` (the daemon's one read; a venue with no entry
    /// has no rows, so every lookup is refused, naming the row). The specs are [`credentialed_specs`]', on the FX
    /// login gate's `fx_clock` (`None` is no gate).
    #[must_use]
    pub fn with_credential_store(
        mut self,
        store: vike_secrets::CredentialStore,
        venue_settings: &BTreeMap<String, VenueSettings>,
        fx_clock: Option<Arc<dyn vike_model::Clock + Send + Sync>>,
    ) -> Self {
        for spec in credentialed_specs(&store, venue_settings, fx_clock.as_ref()) {
            self = self.with_credentialed(spec);
        }
        self
    }

    /// Arm one credentialed kline lane over `spec` — a venue's production spec, or a suite's scripted
    /// source behind it. A lane of the same venue is replaced, and the lanes keep the registry's order.
    #[must_use]
    pub fn with_credentialed(mut self, spec: Box<dyn CredentialedKlinesSpec>) -> Self {
        let lane = CredentialedKlinesLane::new(spec);
        self.credentialed.retain(|armed| armed.venue() != lane.venue());
        self.credentialed.push(lane);
        let order = roster(RosterLane::CredentialedKlines);
        self.credentialed.sort_by_key(|armed| {
            order.iter().position(|venue| *venue == armed.venue()).unwrap_or(order.len())
        });
        self
    }

    /// The credentialed kline lane of `venue`, if armed.
    fn credentialed_lane(&self, venue: &str) -> Option<&CredentialedKlinesLane> {
        self.credentialed.iter().find(|lane| lane.venue() == venue)
    }

    /// Wire the budget counters the status report carries.
    #[must_use]
    pub fn with_budget_snapshot(mut self, snapshot: BudgetCounterSource) -> Self {
        self.budget = snapshot;
        self
    }

    /// Replace the kline registry (a suite's scripted sources).
    #[must_use]
    pub fn with_klines(mut self, sources: Vec<Box<dyn KlineSource>>) -> Self {
        self.klines = sources;
        self
    }

    /// Replace the funding registry.
    #[must_use]
    pub fn with_funding(mut self, sources: Vec<Box<dyn FundingRateSource>>) -> Self {
        self.funding = sources;
        self
    }

    /// Replace the tick lane's spec (a suite's: dukascopy's hour walk over a scripted venue call).
    #[must_use]
    pub fn with_ticks(mut self, spec: Box<dyn TicksSpec>) -> Self {
        self.ticks = spec;
        self
    }

    /// Replace the option-trades source (a suite's scripted one).
    #[must_use]
    pub fn with_option_trades(mut self, source: Box<dyn OptionTradeSource>) -> Self {
        self.option_trades = source;
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

    /// Replace the market-rules source (a suite's scripted one).
    #[must_use]
    pub fn with_market_rules(mut self, source: Box<dyn MarketRulesSource>) -> Self {
        self.market_rules = source;
        self
    }
}

impl std::fmt::Debug for RestLaneBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VenueRest").field("facts", &self.facts()).finish_non_exhaustive()
    }
}

/// The refusal for a request this lane was not built to serve: the venue was NOT asked.
fn refused(text: String) -> SourceError {
    SourceError::Refused(format!("gatewayhub: {text}; nothing was asked of any venue").into())
}

/// **The operator's lookup of ONE instrument on `venue`** (`FetchCredentialed { CatalogSearch }`, the datahub's
/// `OperatorInstruments`): the venue's own lookup over a session this daemon already holds, its chosen row FIRST
/// and the forms it passed over after it. Only ig answers one (`vike_ig::operator_row`): every other venue either
/// lists its instruments or has its facts stated by the operator.
fn operator_lookup(venue: &str, symbol: &str) -> Result<Vec<vike_model::Instrument>, SourceError> {
    if venue == vike_ig::credentialed_klines::VENUE {
        return vike_ig::operator_row::operator_lookup(symbol)
            .map_err(|why| SourceError::Fetch(why.into()));
    }
    Err(refused(format!(
        "`{venue}` answers no instrument lookup on this daemon (only ig does); state the instrument's facts instead"
    )))
}

impl RestLane for RestLaneBridge {
    fn facts(&self) -> RestRosters {
        RestRosters {
            klines: kline_venues(&self.klines),
            funding: funding_venues(&self.funding),
            ticks: vec![TICK_VENUE.to_string()],
            catalog: self.catalog.venues(),
            // The armed lanes, kept in the registry's order (`crate::registry::ROSTERS`' credentialed
            // klines row): oanda, ig, ctrader.
            credentialed: self.credentialed.iter().map(|lane| lane.venue().to_string()).collect(),
            options: true,
            family: vec![FAMILY_VENUE.to_string()],
            presence: true,
            market_rules: vec![self.market_rules.venue().to_string()],
        }
    }

    fn fetch(
        &self,
        scope: &ConnectionMemos,
        what: &FetchWhat,
        credentialed: bool,
        ctx: &FetchContext,
    ) -> Result<FetchRows, SourceError> {
        if credentialed {
            // The credentialed verb serves two lanes: klines of the venue whose credential this daemon holds, and
            // the operator's lookup of ONE instrument over a session it already holds. Anything else is refused
            // here exactly as the server refuses it.
            if let FetchWhat::CatalogSearch { venue, query, .. } = what {
                if ctx.is_cancelled() {
                    return Err(super::cancelled("before the venue was asked"));
                }
                return operator_lookup(venue, query).map(FetchRows::Instruments);
            }
            let FetchWhat::Klines { venue, symbol, interval, start_ms, end_ms } = what else {
                return Err(refused(format!(
                    "FetchCredentialed serves klines and the operator's instrument lookup only; a {} \
                     fetch is not credentialed",
                    what.lane()
                )));
            };
            if let Some(lane) = self.credentialed_lane(venue) {
                return lane
                    .fetch(scope, symbol, interval, *start_ms, *end_ms, ctx)
                    .map(FetchRows::Bars);
            }
            return Err(refused(format!("`{venue}` has no credentialed kline lane")));
        }
        match what {
            FetchWhat::Klines { venue, symbol, interval, start_ms, end_ms } => {
                fetch_klines(&self.klines, venue, symbol, interval, *start_ms, *end_ms, ctx)
                    .map(FetchRows::Bars)
            }
            FetchWhat::Funding { venue, symbol, start_ms, end_ms } => {
                fetch_funding(&self.funding, venue, symbol, *start_ms, *end_ms, ctx)
                    .map(FetchRows::Funding)
            }
            FetchWhat::Ticks { venue, symbol, start_ms, end_ms } => {
                if venue != TICK_VENUE {
                    return Err(refused(format!("`{venue}` has no tick lane")));
                }
                fetch_ticks(self.ticks.as_ref(), symbol, *start_ms, *end_ms, ctx)
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
            FetchWhat::OptionTrades { currency, start_ms, end_ms } => {
                // One venue call that pages inside, like a kline fetch: the only stop is before it.
                if ctx.is_cancelled() {
                    return Err(super::cancelled("before the venue was asked"));
                }
                self.option_trades.fetch(currency, *start_ms, *end_ms).map(FetchRows::OptionTrades)
            }
            FetchWhat::ClosedMarketRules { venue, start_ms, end_ms } => {
                fetch_market_rules(self.market_rules.as_ref(), venue, ctx, |source| {
                    source.closed_ending(*start_ms, *end_ms)
                })
                .map(FetchRows::Texts)
            }
            FetchWhat::MarketRulesOfToken { venue, token } => {
                fetch_market_rules(self.market_rules.as_ref(), venue, ctx, |source| {
                    source.of_token(token)
                })
                .map(FetchRows::Texts)
            }
            FetchWhat::SeriesMarketRules { venue, series, start_ms, end_ms } => {
                fetch_market_rules(self.market_rules.as_ref(), venue, ctx, |source| {
                    source.closed_of_series(series, *start_ms, *end_ms)
                })
                .map(FetchRows::Texts)
            }
        }
    }

    fn catalog_describe(
        &self,
        _scope: &ConnectionMemos,
        venue: &str,
    ) -> Result<CatalogTraits, String> {
        self.catalog.describe(venue)
    }

    fn options(
        &self,
        _scope: &ConnectionMemos,
        call: &OptionsCall,
    ) -> Result<OptionsReply, String> {
        self.options.call(call)
    }

    fn open_options_feed(
        &self,
        _scope: &ConnectionMemos,
        spec: &OptionsFeedSpec,
        sink: Arc<dyn FeedOptionsSink>,
    ) -> Result<Box<dyn FeedOptionsHandle>, String> {
        self.options.open_feed(spec, sink)
    }

    fn resolve_family(
        &self,
        scope: &ConnectionMemos,
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
        match self.credentialed_lane(venue) {
            Some(lane) => lane.presence(),
            // A venue that needs a credential, with this lane built without a way to look, was not read.
            None if roster(RosterLane::CredentialedKlines).contains(&venue) => {
                CredentialPresence::NotChecked
            }
            // A keyless lane, or a venue no lane serves: no credential is needed for it.
            None => CredentialPresence::NotNeeded,
        }
    }

    fn budget_counters(&self) -> Vec<BudgetCounter> {
        (self.budget)()
    }
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod lane_tests;
