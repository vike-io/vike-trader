//! What every mount in one process shares ([`MountEnv`]) and what a mount returns ([`EngineAndRecon`]).

use std::collections::{HashMap, HashSet};

#[cfg(doc)]
use super::{make_engine, make_engine_for_account};
use crate::{MountPolicy, VenueRow};

/// [`make_engine`]'s return: the engine and, when a live bridge built one, its reconcile client.
/// Named for clippy's `type_complexity`.
pub type EngineAndRecon = (
    vike_exec::ExecutionEngine<Box<dyn vike_exec::ExecutionClient + Send>>,
    Option<Box<dyn vike_exec::recon::ReconClient>>,
);

/// What every mount in one process shares. A mount's own facts (venue, symbol, account, legs) are
/// the `make_engine*` arguments.
///
/// Built with [`MountEnv::new`]; set the public fields a caller needs. It holds a `&mut` to the live
/// set and has no `Drop` impl, so a caller reads that set again once it stops using the env.
pub struct MountEnv<'a> {
    /// The venue registry — `vike_tradehub::registry::REGISTRY` in production.
    pub registry: &'static [VenueRow],
    /// The resolved credential/settings variables. Absent credentials ARE the live gate.
    pub vars: &'a HashMap<String, String>,
    /// The unscoped exec-event lane; [`make_engine_for_account`] scopes it per account.
    pub live_events: &'a vike_exec::EventSender,
    /// The route keys of every mount that armed LIVE, written by the mount.
    pub live_venues: &'a mut HashSet<String>,
    /// The reconcile gate's verdict. NOT inferred from `recon_trigger.is_some()`: only a row with a
    /// reconnect poke gets a trigger, and every other venue still reconciles on the interval.
    pub recon_enabled: bool,
    /// The reconnect-poke sender, CLONED into each mount that takes it.
    pub recon_trigger: Option<std::sync::mpsc::Sender<()>>,
    /// The symbol-properties recorder, CLONED into each mount.
    pub properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
    /// The operator's `[risk]` budget, merged onto each venue's limits.
    pub risk_profile: Option<&'a vike_model::ProfileRisk>,
    /// The settings this mount applies. `None` reads PAPER.
    pub policy: Option<&'a MountPolicy>,
}

impl<'a> MountEnv<'a> {
    /// The four fields no mount can do without; the rest start OFF (`recon_enabled: false`, the
    /// others `None`).
    pub fn new(
        registry: &'static [VenueRow],
        vars: &'a HashMap<String, String>,
        live_events: &'a vike_exec::EventSender,
        live_venues: &'a mut HashSet<String>,
    ) -> Self {
        MountEnv {
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
