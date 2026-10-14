//! What every start in one process shares ([`AdapterEnv`]) and what a start returns ([`EngineAndRecon`]).

use std::collections::{HashMap, HashSet};

#[cfg(doc)]
use super::{make_engine, make_engine_for_account};
use crate::{AdapterPolicy, VenueRow};

/// [`make_engine`]'s return: the engine and, when a live bridge built one, its reconcile client.
/// Named for clippy's `type_complexity`.
pub type EngineAndRecon = (
    vike_exec::ExecutionEngine<Box<dyn vike_exec::ExecutionClient + Send>>,
    Option<Box<dyn vike_exec::recon::ReconClient>>,
);

/// What every start in one process shares. A start's own facts (venue, symbol, account, legs) are
/// the `make_engine*` arguments.
///
/// Built with [`AdapterEnv::new`]; set the public fields a caller needs. It holds a `&mut` to the live
/// set and has no `Drop` impl, so a caller reads that set again once it stops using the env.
pub struct AdapterEnv<'a> {
    /// The venue registry — `vike_tradehub::registry::REGISTRY` in production.
    pub registry: &'static [VenueRow],
    /// The resolved credential/settings variables. Absent credentials ARE the live gate.
    pub vars: &'a HashMap<String, String>,
    /// The unscoped exec-event lane; [`make_engine_for_account`] scopes it per account.
    pub live_events: &'a vike_exec::EventSender,
    /// The route keys of every start that armed LIVE, written by the start.
    pub live_venues: &'a mut HashSet<String>,
    /// The reconcile gate's verdict. NOT inferred from `recon_trigger.is_some()`: only a venue that takes the
    /// recon trigger gets one, and every other venue still reconciles on the interval.
    pub recon_enabled: bool,
    /// The recon-trigger sender, CLONED into each start that takes it.
    pub recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    /// The symbol-properties recorder, CLONED into each start.
    pub properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    /// The operator's `[risk]` budget, merged onto each venue's limits.
    pub risk_profile: Option<&'a vike_model::ProfileRisk>,
    /// The settings this start applies. `None` reads PAPER.
    pub policy: Option<&'a AdapterPolicy>,
}

impl<'a> AdapterEnv<'a> {
    /// The four fields no start can do without; the rest start OFF (`recon_enabled: false`, the
    /// others `None`).
    pub fn new(
        registry: &'static [VenueRow],
        vars: &'a HashMap<String, String>,
        live_events: &'a vike_exec::EventSender,
        live_venues: &'a mut HashSet<String>,
    ) -> Self {
        AdapterEnv {
            registry,
            vars,
            live_events,
            live_venues,
            recon_enabled: false,
            recon_trigger: None,
            properties_rec: None,
            risk_profile: None,
            policy: None,
        }
    }
}
