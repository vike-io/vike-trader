//! **`RefreshedTrade` — the production lane: a [`BridgeTrade`] whose settings are re-read at each
//! tradehub-driven step** (wave 2, W9c).
//!
//! In the tradehub the venue half read the `account` directory, the `venue_setting` rows, the deployment
//! map and the flags ONCE, at the tradehub's boot, and an operator applied a credential rotation, a tier
//! edit or a `venue_setting` change by restarting the tradehub. The venue half is the gatewayhub's now, and
//! the gatewayhub outlives every tradehub: read once at the daemon's start, those edits would take
//! effect only on a GATEWAYHUB restart, and a rotated key would be ignored in silence (the daemon would go
//! on signing with the old set). So this lane takes a [`TradeSource`], a closure that reads all of it
//! afresh, and builds the next [`BridgeTrade`] generation from it at the four verbs a tradehub restart
//! drives: the arming table it opens with, the preflight's plan, the tier check of a `TradeOpen` and the
//! `AdapterStart`. A tradehub restart therefore picks up every edit, exactly as before; a running adapter
//! is untouched (its client already holds the credentials it started with).
//!
//! * One VERB is one generation: a call reads a single consistent snapshot, never a mix. A PREFLIGHT is
//!   not one verb: it is a `Plan` question and then one `Clock`/`Credential`/`Network` question per read,
//!   each its own connection, and the later ones run on the CURRENT generation, which any other verb
//!   (an `ArmingQuery`, another `TradeOpen`'s tier check, an `AdapterStart`) may have replaced since the
//!   plan (review 15, finding 2). A serial tradehub never meets that. When it happens, the credential
//!   probes are rebuilt from the newer snapshot (`BridgeTrade::credential_leg_of`), so a question never
//!   fails with "no probe was planned" for a venue that has one, and a venue that LEFT the newer snapshot
//!   (its key withdrawn, its account deactivated since the plan) answers `Rejected`, which the tradehub
//!   reads as a refusal of that venue. The mix is between two reads of the same store, never between a
//!   probe and a stranger's credentials.
//! * What must outlive a generation (the books venues confirmed at a start and not yet recorded, the
//!   once-per-process not-running report) is shared by [`BridgeTrade::successor`].
//! * `record_identity`, `venues` and `resolves_by_coid` read the CURRENT generation (the one the start
//!   that precedes them refreshed): they are bookkeeping on a start already made.
//! * A source that cannot read something FAILS CLOSED where the BEFORE tree refused to boot: the flags
//!   fall to the guarded defaults, an unreadable `venue_setting` table refuses the start, an unreadable
//!   `account` table runs no account, and the credential store logs and reads empty (the live gate): see
//!   the source in `crate::gatewayhub_cli`.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use vike_config::ArmingBlock;
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::proto::{
    AdapterStart, ArmingTable, PreflightAnswer, PreflightStep, WireSymbolFacts,
};
use vike_model::AccountTier;

use super::bridge_trade::BridgeTrade;
use super::registry::VenueRow;
use super::snapshot::TradeSnapshot;
use crate::trade::lane::{AdapterRefusal, StartedAdapter, TradeLane, TradeTarget};

/// What a refresh reads: the deployment map and the `account` / `venue_setting` snapshot, as one value.
pub type TradeSource = Box<dyn Fn() -> (HashMap<String, String>, TradeSnapshot) + Send + Sync>;

/// See the module doc.
pub struct RefreshedTrade {
    source: TradeSource,
    current: Mutex<Arc<BridgeTrade>>,
}

impl RefreshedTrade {
    /// A lane over `registry` whose first generation is `source`'s first read.
    #[must_use]
    pub fn new(registry: &'static [VenueRow], source: TradeSource) -> Self {
        let (vars, snapshot) = source();
        let first = Arc::new(BridgeTrade::with_snapshot(registry, vars, snapshot));
        RefreshedTrade { source, current: Mutex::new(first) }
    }

    /// This lane answering the reconcile health gate from `health` (carried by every generation).
    #[must_use]
    pub fn with_health(self, health: Arc<dyn crate::health::VenueHealth>) -> Self {
        let first = self.current.into_inner().unwrap_or_else(PoisonError::into_inner);
        let first = Arc::try_unwrap(first)
            .unwrap_or_else(|_| unreachable!("the lane was just built: nobody else holds it"))
            .with_health(health);
        RefreshedTrade { source: self.source, current: Mutex::new(Arc::new(first)) }
    }

    /// This lane with a FRESH read of the account rows behind the supervised link's predicate
    /// ([`BridgeTrade::with_account_rows`]; carried by every generation).
    #[must_use]
    pub fn with_account_rows(self, rows: super::bridge_trade::AccountRows) -> Self {
        let first = self.current.into_inner().unwrap_or_else(PoisonError::into_inner);
        let first = Arc::try_unwrap(first)
            .unwrap_or_else(|_| unreachable!("the lane was just built: nobody else holds it"))
            .with_account_rows(rows);
        RefreshedTrade { source: self.source, current: Mutex::new(Arc::new(first)) }
    }

    /// This lane answering `TradeLane::symbol_facts` from `facts` (carried by every generation).
    #[must_use]
    pub fn with_symbol_facts(self, facts: Arc<super::symbol_facts::SymbolFacts>) -> Self {
        let first = self.current.into_inner().unwrap_or_else(PoisonError::into_inner);
        let first = Arc::try_unwrap(first)
            .unwrap_or_else(|_| unreachable!("the lane was just built: nobody else holds it"))
            .with_symbol_facts(facts);
        RefreshedTrade { source: self.source, current: Mutex::new(Arc::new(first)) }
    }

    /// The generation the last refresh built.
    fn current(&self) -> Arc<BridgeTrade> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Read the source again and make its answer the current generation.
    fn refreshed(&self) -> Arc<BridgeTrade> {
        let (vars, snapshot) = (self.source)();
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let next = Arc::new(current.successor(vars, snapshot));
        *current = Arc::clone(&next);
        next
    }
}

impl TradeLane for RefreshedTrade {
    fn venues(&self) -> Vec<String> {
        self.current().venues()
    }

    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        self.refreshed().account_tier(venue, account)
    }

    fn arming_table(&self) -> ArmingTable {
        self.refreshed().arming_table()
    }

    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer {
        // Only the steps that BEGIN a preflight read the source: the plan (and a batch) builds the credential
        // probes the later per-venue steps answer from. A clock, credential or network step runs on the
        // CURRENT generation, which is the plan's only while no other verb refreshed in between (a
        // serial tradehub: always); see the module doc for what a concurrent refresh does to it.
        match step {
            PreflightStep::Plan | PreflightStep::Batch => self.refreshed().preflight(venues, step),
            PreflightStep::Clock | PreflightStep::Credential | PreflightStep::Network => {
                self.current().preflight(venues, step)
            }
        }
    }

    fn start_adapter(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal> {
        self.refreshed().start_adapter(target, start, events, trigger)
    }

    fn record_identity(
        &self,
        target: &TradeTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), String> {
        self.current().record_identity(target, bound_tier, recon)
    }

    fn resolves_by_coid(&self, venue: &str) -> bool {
        self.current().resolves_by_coid(venue)
    }

    fn recon_degraded(&self, venue: &str) -> Option<String> {
        self.current().recon_degraded(venue)
    }

    /// Public data, no credential: the current generation answers (its listing cache is shared by all).
    fn symbol_facts(&self, target: &TradeTarget, symbol: &str) -> Result<WireSymbolFacts, String> {
        self.current().symbol_facts(target, symbol)
    }

    /// Asked once per SUCCESSFUL start, so the current generation is the one that start refreshed.
    fn leverage_cap(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> Option<Result<f64, String>> {
        self.current().leverage_cap(target, start)
    }

    /// Called once per SUCCESSFUL start, so the current generation is the one that start refreshed.
    fn link_watch(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> Option<Arc<dyn crate::trade::lane::LinkWatch>> {
        self.current().link_watch(target, start)
    }
}

#[cfg(test)]
impl RefreshedTrade {
    /// The deployment map of the current generation (the tests' window onto what a start would read).
    #[cfg(feature = "data")]
    pub(crate) fn current_vars(&self) -> HashMap<String, String> {
        self.current().vars_for_test()
    }

    /// How many venue-confirmed books wait for `record_identity`.
    pub(crate) fn current_confirmed_books(&self) -> usize {
        self.current().confirmed_books_for_test()
    }

    /// The venue settings of the current generation.
    #[cfg(feature = "data")]
    pub(crate) fn current_venue_settings(
        &self,
    ) -> std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings> {
        self.current().venue_settings_for_test()
    }
}

#[cfg(test)]
mod delegation_tests {
    /// The method names declared in the body of `block` (a trait or impl) that starts at `header`: the
    /// lines `    fn name(` at one indent, which is how both files spell them.
    fn methods(source: &str, header: &str) -> Vec<String> {
        let body = source.split(header).nth(1).expect("the block exists");
        let body = body.split("\n}\n").next().expect("the block ends");
        body.lines()
            .filter_map(|line| line.strip_prefix("    fn "))
            .filter_map(|rest| rest.split(['(', '<']).next())
            .map(str::to_string)
            .collect()
    }

    /// **`RefreshedTrade` delegates EVERY `TradeLane` verb.** A verb with a DEFAULT body (`link_watch`,
    /// `recon_degraded`) compiles without being delegated and then silently answers the default for the
    /// production lane: the review of W9b's wrapper trap. Adding a verb to the trait without delegating it
    /// here fails this test.
    #[test]
    fn the_wrapper_delegates_every_verb_of_the_trait() {
        let trait_methods =
            methods(include_str!("../lane.rs"), "pub trait TradeLane: Send + Sync {");
        let wrapper_methods =
            methods(include_str!("refreshed.rs"), "impl TradeLane for RefreshedTrade {");
        assert!(trait_methods.len() >= 9, "the scan found the trait's verbs: {trait_methods:?}");
        for verb in &trait_methods {
            assert!(
                wrapper_methods.contains(verb),
                "`RefreshedTrade` does not delegate `TradeLane::{verb}`: the production lane would \
                 answer the trait's default (if it has one) instead of `BridgeTrade`'s"
            );
        }
    }
}

#[path = "refreshed_tests.rs"]
#[cfg(test)]
mod refreshed_tests;
