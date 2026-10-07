use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    BookIdentity, DeclaredGridSource, ExecOutcome, HeldBelowLive, IdentityReport, LiveExec,
    MountInputs, MountOutcome, MountRequest, PaperCause, ProcessExclusive, Resolution, Tier,
    VenueDeclaration, VenueMount,
};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, PlantedMount, planted_exclusive};
use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::AccountLabel;

use crate::VenueRow;

struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

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

/// A planted venue: its resolution, whether an armed mount goes live, and counters.
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

impl VenueMount for Planted {
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
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        self.resolution
    }
    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        self.saw_trigger.store(req.recon_trigger.is_some(), Ordering::SeqCst);
        match self.resolve(&req.inputs) {
            Resolution::Armed { tier, .. } if self.live => MountOutcome {
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
            _ => MountOutcome::paper(),
        }
    }
}

fn policy(venue: &str, mode: VenueMode) -> crate::MountPolicy {
    crate::MountPolicy { venues: VenuePolicy::default().declare(venue, mode), ..Default::default() }
}

/// A risk budget wide enough that no planted mount is refused for want of one (also used by
/// `crate::exclusive`'s `exclusive_fold_tests`).
pub(crate) fn budget() -> vike_exec::ProfileRisk {
    vike_exec::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

/// `crate::make_engine` with no variables, reconcile off, no trigger and no properties recorder,
/// so a call site spells only what it varies. The caller owns the channel and live set: several
/// tests share them across mounts and assert on the set afterwards.
fn mount_one(
    registry: &'static [VenueRow],
    venue: &str,
    symbol: &str,
    tx: &vike_exec::EventSender,
    live: &mut HashSet<String>,
    risk: Option<&vike_exec::ProfileRisk>,
    policy: Option<&crate::MountPolicy>,
) -> Result<crate::EngineAndRecon, crate::MountError> {
    let vars = HashMap::new();
    let mut env = crate::MountEnv::new(registry, &vars, tx, live);
    env.risk_profile = risk;
    env.policy = policy;
    crate::make_engine(&mut env, venue, symbol)
}

/// `crate::make_engine_with_legs` as `outcome_fold` mounts: [`mount_one`]'s shape plus `legs`,
/// under [`budget`].
fn budgeted_mount_with_legs(
    registry: &'static [VenueRow],
    venue: &str,
    symbol: &str,
    legs: &[String],
    tx: &vike_exec::EventSender,
    live: &mut HashSet<String>,
    policy: Option<&crate::MountPolicy>,
) -> Result<crate::EngineAndRecon, crate::MountError> {
    let (vars, risk) = (HashMap::new(), budget());
    let mut env = crate::MountEnv::new(registry, &vars, tx, live);
    env.risk_profile = Some(&risk);
    env.policy = policy;
    crate::make_engine_with_legs(&mut env, venue, symbol, legs)
}

#[cfg(test)]
mod arming;
#[cfg(test)]
mod budget_and_recon;
#[cfg(test)]
mod demo_ceiling;
#[cfg(test)]
mod exclusive;
#[cfg(test)]
mod outcome_fold;
#[cfg(test)]
mod process_facts;
