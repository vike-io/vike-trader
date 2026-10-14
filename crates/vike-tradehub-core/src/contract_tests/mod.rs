//! The venue-mount contract fold's tests: planted venues and start helpers, one child per concern.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, BookIdentity, DeclaredGridSource, ExecOutcome,
    HeldBelowLive, IdentityReport, LiveExec, PaperCause, ProcessExclusive, Resolution,
    VenueAdapter, VenueDeclaration,
};
use vike_bridge_core::venue_adapter_fixture::{
    PLANTED_DECLARATION, PlantedAdapter, planted_exclusive,
};
use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use crate::VenueRow;
use crate::testutil::{EmptyRecon, NoopClient};

/// Every `held_by_another` text a planted exclusive row rendered, so a test can see the refusal the
/// fold logged — and the holder it named.
static HELD_BY_ANOTHER: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn grid(tick: f64) -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: tick,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 1_000_000.0,
        min_notional: 1.0,
        ..Default::default()
    }
}

/// A planted venue: its resolution, whether an armed start goes live, and counters.
struct Planted {
    venue: &'static str,
    resolution: Resolution,
    live: bool,
    addresses_accounts: bool,
    exclusive: bool,
    /// The exclusive resource's name (REQUIRED on an exclusive row). The claim set is
    /// process-global, keyed on the resource and shared by every test in this binary, so each
    /// exclusive row names its OWN resource and probes by it; two share one only where a test
    /// says so.
    resource: Option<&'static str>,
    takes_trigger: bool,
    resolves: AtomicUsize,
    mounts: AtomicUsize,
    saw_trigger: AtomicBool,
}

impl Planted {
    const fn new(venue: &'static str, resolution: Resolution, live: bool) -> Self {
        Planted {
            venue,
            resolution,
            live,
            addresses_accounts: true,
            exclusive: false,
            resource: None,
            takes_trigger: false,
            resolves: AtomicUsize::new(0),
            mounts: AtomicUsize::new(0),
            saw_trigger: AtomicBool::new(false),
        }
    }
}

impl VenueAdapter for Planted {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: self.addresses_accounts,
            process_exclusive: self.exclusive.then(|| ProcessExclusive {
                resource: self.resource.expect("an exclusive planted row names its own resource"),
                held_by_another: |label, holder| {
                    let text = format!("{label} yields to {holder}");
                    HELD_BY_ANOTHER.lock().expect("the planted log").push(text.clone());
                    text
                },
                already_claimed: |label| format!("{label}: already claimed"),
            }),
            takes_recon_trigger: self.takes_trigger,
            grid_source: DeclaredGridSource::InHand,
            ..PLANTED_DECLARATION
        }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        self.resolution
    }
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        self.saw_trigger.store(req.recon_trigger.is_some(), Ordering::SeqCst);
        match self.resolve(&req.inputs) {
            Resolution::Armed { tier, .. } if self.live => AdapterOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client: Box::new(NoopClient),
                    bound_tier: tier,
                    grid: Some(grid(0.5)),
                    contract_size: Some(2.0),
                    margin_mode: Some(vike_model::MarginMode::Isolated),
                    leg_grids: vec![("ETHUSDT".to_string(), grid(0.01))],
                }),
                recon: None,
                identity: None,
            },
            _ => AdapterOutcome::paper(),
        }
    }
}

/// A policy whose `account` table holds ONE active row: `venue`'s DEFAULT account at `tier`.
fn policy(venue: &str, tier: AccountTier) -> crate::AdapterPolicy {
    crate::AdapterPolicy::default().with_account(venue, &AccountLabel::Default, tier)
}

/// A risk budget wide enough that no planted start is refused for want of one (also used by
/// `crate::exclusive`'s `exclusive_fold_tests`).
pub(crate) fn budget() -> vike_model::ProfileRisk {
    vike_model::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

/// `crate::make_engine` with no variables, reconcile off, no trigger and no properties recorder,
/// so a call site spells only what it varies. The caller owns the channel and live set: several
/// tests share them across starts and assert on the set afterwards.
fn start_one(
    registry: &'static [VenueRow],
    venue: &str,
    symbol: &str,
    tx: &vike_exec::EventSender,
    live: &mut HashSet<String>,
    risk: Option<&vike_model::ProfileRisk>,
    policy: Option<&crate::AdapterPolicy>,
) -> Result<crate::EngineAndRecon, crate::EngineError> {
    let vars = HashMap::new();
    let mut env = crate::AdapterEnv::new(registry, &vars, tx, live);
    env.risk_profile = risk;
    env.policy = policy;
    crate::make_engine(&mut env, venue, symbol)
}

/// `crate::make_engine_with_legs` as `outcome_fold` starts: [`start_one`]'s shape plus `legs`,
/// under [`budget`].
fn budgeted_start_with_legs(
    registry: &'static [VenueRow],
    venue: &str,
    symbol: &str,
    legs: &[String],
    tx: &vike_exec::EventSender,
    live: &mut HashSet<String>,
    policy: Option<&crate::AdapterPolicy>,
) -> Result<crate::EngineAndRecon, crate::EngineError> {
    let (vars, risk) = (HashMap::new(), budget());
    let mut env = crate::AdapterEnv::new(registry, &vars, tx, live);
    env.risk_profile = Some(&risk);
    env.policy = policy;
    crate::make_engine_with_legs(&mut env, venue, symbol, legs)
}

/// The account a start reported as NOT RUNNING (decision 0156), or a panic naming what came back
/// instead — an engine (a paper one standing in included) or another refusal.
fn not_running(result: Result<crate::EngineAndRecon, crate::EngineError>) -> crate::NotRunning {
    match result {
        Err(crate::EngineError::NotRunning(nr)) => nr,
        Err(other) => panic!("expected NOT RUNNING, got the refusal: {other}"),
        Ok((engine, _)) => panic!("expected NOT RUNNING, got an engine at {}", engine.tier),
    }
}

#[cfg(test)]
mod arming;
#[cfg(test)]
mod budget_and_recon;
#[cfg(test)]
mod demo_tier;
#[cfg(test)]
mod exclusive;
#[cfg(test)]
mod orders_held;
#[cfg(test)]
mod outcome_fold;
#[cfg(test)]
mod process_facts;
