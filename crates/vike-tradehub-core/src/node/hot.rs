//! **Starting ONE account in the RUNNING node** (decision 0169, slices S1 and S3): the account whose
//! `active` flag was just set, at the `demo` or the `live` tier, on any venue of the wiring table.
//!
//! [`assemble_runtime`](super::assemble_runtime) builds every engine once, at start, and used to hand
//! the finished set to a core that could not take another. This is the same construction for one
//! more account, run later: [`make_engine_for_account`] (the code startup runs per account, so a hot
//! start and a boot start cannot disagree about what an account is), then
//! [`vike_exec::EventSender::add_account_engine`] to APPEND the engine to the core, then the
//! account's reconcile leg to the reconcile driver ([`LegChange`]).
//!
//! # The order is the safety argument
//!
//! 1. **The account's own event lane is held back until the core has its engine.** The engine is built
//!    over a NEW lane whose forwarder thread buffers (the boot forwarder's own pre-core backlog); the
//!    hand-over message is queued on the core's ingest FIRST, and only then is the lane's door opened.
//!    The core's ingest is one FIFO, so the first event of the new account folds after its engine
//!    exists: nothing is mis-attributed, nothing is dropped as unattributable.
//! 2. **The venue's first account is re-stamped before the second engine joins.** A reconcile leg
//!    carrying no route key means "this venue's sole account", and the core REFUSES such a pass on a
//!    venue it runs two engines of. Naming the leg first routes identically while there is one engine.
//! 3. **A failure is that one account NOT RUNNING** ([`HotStart::NotRunning`]): a lease another client
//!    holds, a gatewayhub that does not answer, a key the venue declines, a refused risk budget. Never
//!    a panic, never a refused node, never another account touched.
//!
//! # The `live` tier and a venue the node mounted nothing for (slice S3)
//!
//! A `live` account crosses the gates the boot crosses, in the order the boot crosses them, and a
//! refusal at any of them is THAT account NOT RUNNING, never the node and never another account:
//!
//! * `flags.tradehub_live`: this module is reached only through a live mount, which exists only with the
//!   flag on (a restart-class setting); the account watcher also refuses a `live` start with the flag
//!   off (`ArmingBlock::LiveSwitchOff`), so the gate is spelled where the start is decided.
//!   `flags.poly_exec` is the gatewayhub's own word in the FRESH arming table (`ExecFlagUnset`).
//! * the risk budget: [`make_engine_for_account`] refuses a venue-client account with no `[risk]` budget
//!   (`EngineError::MissingRiskBudget`), the boot's `RuntimeError::RiskBudget`, here one account.
//! * the preflight: a venue the boot never preflighted (no account of it was armed then) is preflighted
//!   now, for that venue alone ([`super::hot_preflight`]); a confirmed FAIL is `PreflightFailed` for
//!   this attempt (and the next attempt asks again, so a fixed cause starts the account with no restart).
//! * the HALT sentinel and the core's risk state: the new engine starts in the core's trading state (the
//!   core's `AccountChange::AddEngine`), and every submit crosses the gatewayhub's HALT gate.
//! * the live lock and the lease: claimed by the gatewayhub inside the trade-lease open
//!   (`start_adapter`); a lock or lease another client holds is that account's `AdapterFailed`. (At BOOT it
//!   still refuses the whole mount: owner question O2 of the plan.)
//! * the armed set: recomputed from the fresh table at every start ([`armed_live_venues`]) and required to
//!   name the account before anything is built; the boot's lock backstop ([`refuse_unarmed_live_venues`])
//!   compares it with what the start armed afterwards.
//!
//! A venue the node holds NO engine for is started like any other: its engine is APPENDED at the end of the
//! core's list, outside [`super::WiredMarket::engine_rank`] order. The rank is a boot-time convention for
//! which engine is the primary and where the defaults sit; nothing the core does after `spawn_core_multi`
//! reads an engine's position except as its attribution index, and an index never moves
//! (`the_default_engine_of_an_unmounted_venue_is_appended_outside_rank_order`).
//!
//! # A `paper` account (slice S4)
//!
//! A `paper` account starts the way the boot starts one: the simulator engine over the same construction
//! ([`make_engine_for_account`]'s `paper` branch), appended to the core. It reaches no venue: no arming table
//! is asked for, no armed set, preflight or lease applies, and it has no reconcile leg. Whether a venue's
//! price feed was wired is the caller's guard, as for every tier.
//!
//! # Reconciliation started in the running node (slice S4)
//!
//! Reconciliation is decided once, at the boot, from the accounts armed THEN. A node that booted with it off
//! only because nothing was armed (the gate's `NothingArmedLive`, not the operator's `flags.reconcile_off`) is
//! given a [`HotRecon`] by the composition root: the first `demo` or `live` account that starts with a
//! reconcile client then STARTS THE DRIVER (`spawn_recon_on`, over the core's ingest lane, the same
//! driver and the same config the boot would have built), and the next ones join it as legs. Without a
//! [`HotRecon`] a `live` account in such a node keeps the restart path ([`HotStart::RestartRequired`]), and a
//! `demo` one starts with no reconcile leg and says so.
//!
//! # The balance window (what judges an order right after a start)
//!
//! The core appends the engine with `seed_cash` for its equity, in `Delta` mode, and only the reconcile
//! driver's first pass over the new leg puts the venue's balance there. The boot has the same window and
//! closes it by construction (reconciliation seeds first, before anything trades); a start in the running
//! node does not, and the pass can be a whole pass of every other account's blocking fetches away (the
//! 2026-10-18 okx drill: a 4x order denied `insufficient-margin` on a seed of 1000 against a balance of
//! 4999.6; a POOR account's seed overstates it, and then the gate admits what the venue refuses). So a start
//! that hands the driver a leg first announces [`HotAccounts::with_first_balance_notice`]'s flag, `false`
//! until the leg's first answered balance is enqueued (`ReconLeg::awaited`), and the control edge refuses the
//! account's OPENING orders while it is. A start with no leg (a `paper` account, reconciliation off) announces
//! nothing: no balance will ever land there, so nothing waits.
//!
//! # What it does NOT do
//!
//! The engine list only grows.
//!
//! # Stopping (slice S2)
//!
//! [`HotAccounts::stop_account`] is the other direction, in two phases: the reconcile leg is retired and
//! the core is told to stop the account (cancel its own orders, tombstone the engine, keep its client
//! attached so the cancels are answered); then, once they are, [`HotAccounts::detach_account`] begins the
//! client's detach and the session and lease close. The account is NOT removed from the core and nothing
//! is flattened: an open position stays at the venue and in the published block. A later
//! [`HotAccounts::start_account`] of the same account appends a NEW engine under the same route key.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender};

use vike_config::ArmingBlock;
use vike_config::account_arming::account_tier;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;

use super::accounts::{
    RuntimeAccount, account_symbols_for, armed_live_venues, refuse_unarmed_live_venues,
};
use super::build::{log_preflight, run_live_forwarder};
use super::hot_preflight::{OneVenue, one_venue_table};
use crate::recon_manager::spawn_recon_on;
use crate::{
    AdapterEnv, AdapterPolicy, ArmingTable, EngineError, LegChange, NotRunning, ReconConfig,
    ReconDriver, ReconLeg, VenueBackend, account_route_key, make_engine_for_account,
    symbol_for_account,
};

/// What a running node needs to START the reconcile driver itself, for a node that booted with reconciliation
/// off only because no account was armed then (the reconcile gate's `NothingArmedLive`; the operator's own
/// `flags.reconcile_off` is never given one). The composition root builds it from the settings the boot read, so
/// the driver started here is the driver the boot would have built.
pub struct HotRecon {
    make_config: Box<dyn Fn() -> ReconConfig + Send>,
}

impl HotRecon {
    /// A maker of the driver's config, called once, when the first account that has a reconcile client starts.
    pub fn new(make_config: impl Fn() -> ReconConfig + Send + 'static) -> Self {
        HotRecon { make_config: Box::new(make_config) }
    }
}

/// What starting one account in the running node came to.
#[derive(Debug)]
pub enum HotStart {
    /// The engine is in the core, its lane is open and its reconcile leg (when reconcile is on) is
    /// with the driver.
    Started {
        /// The account's routing key.
        route_key: String,
        /// The tier it runs at.
        tier: AccountTier,
    },
    /// The account does not run, with its reason. THIS account only: nothing else was touched.
    NotRunning(NotRunning),
    /// This build does not start that account without a restart (the scope guard), in words.
    RestartRequired(String),
    /// The account already has an engine in this process: nothing to do.
    AlreadyRunning,
    /// The core has exited: the process is going down.
    CoreGone,
}

/// What stopping one account in the running node came to (decision 0169, slice S2).
#[derive(Debug, PartialEq, Eq)]
pub enum HotStop {
    /// The core was told to stop the account: its resting orders are being cancelled, its lease closes
    /// behind them, new orders for it are refused. Its engine stays in the core (an index is an
    /// attribution key), listed NOT RUNNING.
    Stopped,
    /// The account has no running engine here: nothing was sent.
    NotRunning,
    /// The core has exited: the process is going down.
    CoreGone,
}

/// Everything starting one more account needs, taken from the assembly that built the core. Owned by
/// ONE thread (the tradehub's account watcher), so its methods take `&mut self`.
pub struct HotAccounts {
    /// The core's own ingest lane (a strong sender): where the engine is handed over, and the
    /// door the new account's forwarder opens onto.
    pub(super) core_events: vike_exec::EventSender,
    /// The core's tick lane, the other half of that door.
    pub(super) core_ticks: vike_exec::TickSender,
    /// The assembly's teardown flag, shared with the boot forwarder: raised before the core shuts
    /// down, so a hot forwarder drains-and-drops too.
    pub(super) forwarder_stop: Arc<AtomicBool>,
    /// `RuntimeConfig::seed_cash`: every extra engine's equity seed.
    pub(super) seed_cash: f64,
    /// The reconcile gate's verdict; builds nothing here, it rides `AdapterEnv::recon_enabled`.
    pub(super) recon_enabled: bool,
    /// Whether the operator REFUSED reconciliation (`flags.reconcile_off`), as opposed to the gate being off
    /// because no live account was armed at the boot. Set by the composition root
    /// ([`Self::with_reconcile_refused`]); it only decides whether a `live` account may start while
    /// `recon_enabled` is false.
    pub(super) recon_refused: bool,
    /// The reconcile driver's reconnect trigger (`Some` iff reconcile is on): cloned into a start that
    /// takes it, and poked once after a new leg joins so the account's first pass is not 60 s away.
    pub(super) recon_trigger: Option<Sender<()>>,
    /// `RuntimeConfig::properties_rec`.
    pub(super) properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// `RuntimeConfig::risk_profile`: the operator's `[risk]` budget, merged onto the new engine as
    /// onto every boot engine.
    pub(super) risk_profile: Option<vike_model::ProfileRisk>,
    /// The policy the boot assembly ran under AFTER the preflight's enforcement; its `accounts` are
    /// replaced by the rows read at each start.
    pub(super) policy: AdapterPolicy,
    /// The mounts' routing declarations (which labelled account trades which symbol).
    pub(super) mounts: Vec<RuntimeAccount>,
    /// `(venue, wired symbol, declared extra legs)` of EVERY market of the wiring table, whether or not the
    /// core holds an engine for it: the first account of a venue the node mounted nothing for is started on
    /// its default engine's market.
    pub(super) wired: Vec<(String, String, Vec<String>)>,
    /// `(venue, route key)` of every engine the core holds, STOPPED ones included (the core's list only
    /// grows, so this one does too: it is what the venue's engine count is read from).
    pub(super) engines: Vec<(String, String)>,
    /// The route keys whose NEWEST engine was stopped ([`Self::stop_account`]). A key leaves this set when
    /// its account starts again, which appends a new engine under the same key.
    pub(super) stopped: HashSet<String>,
    /// The deployment map the boot assembly was given (`RuntimeConfig::vars`, the folded flags among
    /// it): read only for the preflight skip flag. It may hold deployment secrets (the Telegram
    /// token), as the assembly's own copy does: this one is memory only and is never printed.
    pub(super) vars: std::collections::HashMap<String, String>,
    /// The `(venue, tier)` pairs whose startup preflight has run: the ones with an account armed at the boot,
    /// and every pair a hot start has preflighted since without a confirmed FAIL. The TIER is part of the
    /// key because the preflight reads that tier's credentials and clock: a venue that booted at `demo`
    /// has not had its `live` side looked at.
    pub(super) preflighted: HashSet<(String, AccountTier)>,
    /// How to start the reconcile driver in this node when it booted without one ([`Self::with_hot_reconcile`]).
    /// Taken when the driver starts.
    pub(super) recon_hot: Option<HotRecon>,
    /// The driver this node started itself ([`HotRecon`]); the boot's own is the caller's `recon_legs`.
    pub(super) hot_driver: Option<ReconDriver>,
    /// Where a start with a reconcile leg says its FIRST-BALANCE flag ([`Self::with_first_balance_notice`]):
    /// told the account's route key and the flag its leg raises once the venue's balance reached the core.
    pub(super) first_balance_notice: Option<FirstBalanceNotice>,
}

/// What [`HotAccounts::with_first_balance_notice`] holds: the account's route key and the flag. A factored type
/// so the `Option` above does not trip `clippy::type_complexity` (the idiom of `crate::HealthProbe`).
type FirstBalanceNotice = Box<dyn Fn(&str, &Arc<AtomicBool>) + Send>;

impl HotAccounts {
    /// **Say, before an account's engine joins the core, the flag that tells when its balance has landed.**
    /// An engine the node appends judges orders on `seed_cash` (the wrong figure in BOTH directions) until
    /// the reconcile driver's first pass puts the venue's balance on the core's lane, and that pass can be
    /// a whole pass of every other account's fetches away. `notice(route_key, flag)` is called for a start
    /// whose engine gets a reconcile leg, BEFORE the engine is handed to the core, and `flag` stays `false`
    /// until the leg's first balance answer is enqueued (`ReconLeg::awaited`): the control door refuses
    /// the account's opening orders on it. Never called for the boot's engines (reconciliation seeds them
    /// first by design), for a `paper` account, or for a start with no reconcile leg: nothing will ever land
    /// for those, so nothing waits.
    #[must_use]
    pub fn with_first_balance_notice(
        mut self,
        notice: impl Fn(&str, &Arc<AtomicBool>) + Send + 'static,
    ) -> Self {
        self.first_balance_notice = Some(Box::new(notice));
        self
    }

    /// Let this node start the reconcile driver when the first account that has a reconcile client starts, for a
    /// node that booted with reconciliation off because nothing was armed (not because the operator refused it).
    #[must_use]
    pub fn with_hot_reconcile(mut self, hot: HotRecon) -> Self {
        self.recon_hot = Some(hot);
        self
    }

    /// Whether this node started the reconcile driver itself.
    #[must_use]
    pub fn reconcile_started_hot(&self) -> bool {
        self.hot_driver.is_some()
    }

    /// Stop the driver this node started, if any: no new pass begins and the thread ends once a fetch in flight
    /// returns. The boot's driver is the caller's to shut down; this is the teardown of the other one.
    pub fn begin_shutdown_hot_reconcile(&self) {
        if let Some(driver) = &self.hot_driver {
            driver.begin_shutdown();
        }
    }

    /// The leg mailbox a start or a stop writes to: the boot driver's (`given`), else the one this node started.
    fn legs_of(&self, given: Option<&Sender<LegChange>>) -> Option<Sender<LegChange>> {
        given.cloned().or_else(|| self.hot_driver.as_ref().map(ReconDriver::leg_sender))
    }

    /// Say whether the operator refused reconciliation (`flags.reconcile_off`). Without it, a `live` account
    /// is not started while reconciliation is off (it would trade with no reconcile leg until a restart,
    /// which the boot would have turned on): [`HotStart::RestartRequired`].
    #[must_use]
    pub fn with_reconcile_refused(mut self, refused: bool) -> Self {
        self.recon_refused = refused;
        self
    }

    /// Whether the core holds a RUNNING engine for `route_key`.
    #[must_use]
    pub fn runs(&self, route_key: &str) -> bool {
        !self.stopped.contains(route_key) && self.engines.iter().any(|(_, k)| k == route_key)
    }

    /// Whether the core holds any engine for `venue`, a stopped one included.
    #[must_use]
    pub fn mounts_venue(&self, venue: &str) -> bool {
        self.engines.iter().any(|(v, _)| v == venue)
    }

    /// The route keys of every RUNNING engine the core holds, in the order they joined.
    #[must_use]
    pub fn running_route_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = Vec::new();
        for (_, key) in &self.engines {
            if self.runs(key) && !keys.contains(key) {
                keys.push(key.clone());
            }
        }
        keys
    }

    /// **STOP the account `not_running` names, phase one** (decision 0169, slice S2), listed with the
    /// reason it carries. The caller has already put the account on the published NOT RUNNING list (the
    /// control edge refuses its new orders from then on). What happens, in this order: its reconcile leg
    /// is retired (no further pass is started for it); the core is sent the stop, which cancels ONLY this
    /// account's resting orders and tombstones its engine. Its client stays attached so the venue's
    /// answers to those cancels reach the engine: the caller waits for them (bounded) and then calls
    /// [`Self::detach_account`], which ends the session. No other account is touched, and nothing is
    /// flattened.
    ///
    /// Non-blocking: the sends are queued.
    pub fn stop_account(
        &mut self,
        not_running: vike_exec::NotRunningAccount,
        recon_legs: Option<&Sender<LegChange>>,
    ) -> HotStop {
        let route_key = not_running.route_key.clone();
        if !self.runs(&route_key) {
            return HotStop::NotRunning;
        }
        if let Some(legs_tx) = self.legs_of(recon_legs) {
            let _ = legs_tx.send(LegChange::Retire(route_key.clone()));
        }
        if self.core_events.stop_account_engine(not_running).is_err() {
            return HotStop::CoreGone;
        }
        self.stopped.insert(route_key.clone());
        tracing::warn!(
            route_key = %route_key,
            "ARMING: {route_key} STOPPING in the running tradehub — its resting orders are being \
             cancelled and new orders for it are refused; its lease closes once the cancels are \
             answered; no restart, no other account touched (decision 0169)"
        );
        HotStop::Stopped
    }

    /// **STOP, phase two**: tell the core to begin the detach of the stopped account's client, so its
    /// session and lease close (on the link's own thread; nothing waits here). The caller sends it once the
    /// account's orders are terminal in the core, or a bound has passed. `NotRunning` when the account is
    /// not one this node stopped.
    pub fn detach_account(&mut self, route_key: &str) -> HotStop {
        if !self.stopped.contains(route_key) {
            return HotStop::NotRunning;
        }
        if self.core_events.detach_account_engine(route_key).is_err() {
            return HotStop::CoreGone;
        }
        tracing::warn!(
            route_key,
            "ARMING: {route_key} STOPPED in the running tradehub — its session and lease are closing \
             (decision 0169)"
        );
        HotStop::Stopped
    }

    /// **Whether the startup preflight, run now for `venue` alone, hard-FAILED it** (`true` = the venue's
    /// accounts do not run on this attempt). A venue the boot already preflighted (an account of it was
    /// armed then) is not asked again: its verdict stands, and a boot FAIL is already in the policy the
    /// rows are judged under. A venue that passes, or only WARNs (a probe that did not answer stops
    /// nothing), is remembered as preflighted; a FAIL is not, so a fixed cause starts the account at the
    /// next attempt with no restart.
    fn preflight_stops(
        &mut self,
        backend: &Arc<dyn VenueBackend>,
        table: &crate::ArmingTable,
        policy: &AdapterPolicy,
        venue: &str,
        tier: AccountTier,
    ) -> bool {
        if self.preflighted.contains(&(venue.to_string(), tier)) {
            return false;
        }
        let one: Arc<dyn VenueBackend> =
            Arc::new(OneVenue { inner: Arc::clone(backend), venue: venue.to_string() });
        let report = crate::startup::run_startup_preflight(
            &one,
            &one_venue_table(table, venue),
            &self.vars,
            &[],
            Some(policy),
        );
        log_preflight(&report);
        if report.degraded_venues().iter().any(|v| v == venue) {
            tracing::error!(
                venue,
                "startup preflight STOPPED {venue}'s account for this attempt (a per-venue FAIL is \
                 confirmed evidence: the venue answered and refused, or its clock is measurably out of \
                 band). Fix the cause in that venue's row above: the next change of the account rows \
                 asks again, with no restart"
            );
            return true;
        }
        self.preflighted.insert((venue.to_string(), tier));
        false
    }

    /// **The guards of a hot start, answered WITHOUT touching the network**: `Some(outcome)` when the
    /// account needs no start attempt (it already runs, its row resolves to a refusal, it names a venue
    /// the wiring table has no engine row for, or it is a `paper` account), `None` when
    /// [`Self::start_account`] should go on. The caller asks this first so that no gatewayhub is dialled
    /// for an account that cannot start anyway.
    #[must_use]
    pub fn check(
        &self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> Option<HotStart> {
        self.resolve(accounts, venue, label).err()
    }

    /// The guards, and what a start that passes them needs: the tier, the venue's wired symbol and its
    /// declared legs. `Err` is the outcome that ends the attempt.
    fn resolve(
        &self,
        accounts: &AccountDirectory,
        venue: &str,
        label: &AccountLabel,
    ) -> Result<(AccountTier, String, Vec<String>), HotStart> {
        let route_key = account_route_key(venue, label);
        if self.runs(&route_key) {
            return Err(HotStart::AlreadyRunning);
        }
        let tier = account_tier(Some(accounts), venue, label).map_err(|block| {
            HotStart::NotRunning(NotRunning {
                venue: venue.to_string(),
                account: label.clone(),
                tier: None,
                block,
            })
        })?;
        // A venue the wiring table gives no engine row is one no build can run an account on.
        let Some((_, wired_symbol, legs)) = self.wired.iter().find(|(v, _, _)| v == venue).cloned()
        else {
            return Err(HotStart::NotRunning(NotRunning {
                venue: venue.to_string(),
                account: label.clone(),
                tier: Some(tier),
                block: ArmingBlock::NoLiveArm,
            }));
        };
        // Reconciliation is decided once, at the boot, from the accounts armed THEN. A `live` account started
        // into a node that has none would trade with no reconcile leg until a restart (which turns it on),
        // so it keeps the restart path, unless the operator refused reconciliation outright or the node can
        // start the driver itself ([`HotRecon`]).
        if tier == AccountTier::Live
            && !self.recon_enabled
            && !self.recon_refused
            && self.recon_hot.is_none()
        {
            return Err(HotStart::RestartRequired(format!(
                "{route_key} is a `live` account and this tradehub booted with reconciliation off (no \
                 live account was armed then): it starts with the next restart, which turns \
                 reconciliation on for it (`flags.reconcile_off` would refuse that)"
            )));
        }
        Ok((tier, wired_symbol, legs))
    }

    /// Start the reconcile driver in this node with `leg` as its first leg, adopting `trigger` (the pair whose
    /// sender the account's client already holds). From here the node reconciles as a boot with an armed account
    /// would have, and the next starts join as legs.
    fn start_reconcile_driver(
        &mut self,
        leg: ReconLeg,
        trigger: Option<(Sender<()>, Receiver<()>)>,
    ) {
        let Some(hot) = self.recon_hot.take() else { return };
        let route_key = leg.route_key.clone().unwrap_or_else(|| leg.venue.clone());
        let config = (hot.make_config)();
        self.recon_trigger = trigger.as_ref().map(|(tx, _)| tx.clone());
        self.hot_driver =
            Some(spawn_recon_on(&self.core_events.ingest, vec![leg], config, trigger));
        self.recon_enabled = true;
        tracing::warn!(
            route_key = %route_key,
            "ARMING: reconciliation was OFF at this tradehub's start (no account was armed then) and \
             is ON now: {route_key} is the first account with a reconcile client, so the driver was \
             started in the running tradehub (quarantine-first unless config.reconcile_policy says \
             otherwise); it fetches that venue's own orders and positions with authenticated reads \
             and holds every divergence"
        );
    }

    /// **Start `venue`'s `label` account** over `backend` (a backend whose arming table and account
    /// rows are FRESH, which the caller made for this start) and `accounts` (the rows read now, with
    /// the caller's `data_only` withhold already applied). `recon_legs` is the reconcile driver's leg
    /// mailbox, `None` when reconcile is off.
    ///
    /// Blocking (a lease, an adapter start, one venue handshake): run it on a thread of its own,
    /// never on the core's.
    pub fn start_account(
        &mut self,
        backend: &Arc<dyn VenueBackend>,
        accounts: AccountDirectory,
        venue: &str,
        label: &AccountLabel,
        recon_legs: Option<&Sender<LegChange>>,
    ) -> HotStart {
        let (tier, wired_symbol, legs) = match self.resolve(&accounts, venue, label) {
            Ok(resolved) => resolved,
            Err(early) => return early,
        };
        let route_key = account_route_key(venue, label);
        // The boot driver's mailbox, or the one this node started itself.
        let driver_legs = self.legs_of(recon_legs);
        let recon_legs = driver_legs.as_ref();
        let not_running = |tier, block| NotRunning {
            venue: venue.to_string(),
            account: label.clone(),
            tier,
            block,
        };
        // The rows of THIS start, under the boot's preflight verdict: a venue the startup preflight
        // stopped keeps its credentials withheld for the whole session.
        let mut policy = self.policy.clone();
        policy.accounts = accounts;
        for stopped in policy.preflight_failed.clone() {
            policy.accounts =
                crate::startup::withhold_venue_credentials(&policy.accounts, &stopped).0;
        }
        // A `paper` account reaches no venue (module doc): no arming table is asked for, nothing is armed
        // and nothing is preflighted, so it starts while the gatewayhub is away.
        let paper = tier == AccountTier::Paper;
        let table = if paper {
            ArmingTable::new(BTreeMap::new(), Vec::new())
        } else {
            match backend.arming_table() {
                Ok(table) => table,
                Err(e) => {
                    tracing::error!(venue, account = %label, "{venue}: {e}");
                    return HotStart::NotRunning(not_running(
                        Some(tier),
                        ArmingBlock::GatewayUnavailable,
                    ));
                }
            }
        };
        let armed = if paper { Vec::new() } else { armed_live_venues(&table, &policy) };
        if !paper {
            // WHICH accounts start is the fan-out's own decision (`accounts_to_run`: the row RUNS in the fresh
            // table), so an account the gatewayhub does not arm is NOT RUNNING with the table's word and never
            // reaches a lease open.
            let rows = crate::venue_account_arming(&table, venue, Some(&policy));
            match rows.iter().find(|row| row.label == *label) {
                Some(row) if row.runs() => {}
                row => {
                    let nr = NotRunning {
                        venue: venue.to_string(),
                        account: label.clone(),
                        tier: row.and_then(|r| r.tier),
                        block: row.map_or(ArmingBlock::NoAccountRow, |r| r.block),
                    };
                    crate::report_not_running(&nr, Some(&policy));
                    return HotStart::NotRunning(nr);
                }
            }
            // THE ARMED SET names the account, or nothing is built for it: the set the boot's lock claim and
            // its backstop are read from, recomputed from the fresh rows (the module doc, "armed set").
            if !armed.contains(&route_key) {
                tracing::error!(
                    venue, account = %label, route_key = %route_key,
                    "{route_key}: the armed set recomputed from the rows does not name this account \
                     ({armed:?}), so nothing is built for it: it does NOT RUN"
                );
                return HotStart::NotRunning(not_running(Some(tier), ArmingBlock::AdapterFailed));
            }
            // THE PREFLIGHT of a venue the boot never preflighted, for that venue alone. A confirmed FAIL is
            // this attempt's refusal and is not remembered: the next attempt asks again.
            if self.preflight_stops(backend, &table, &policy, venue, tier) {
                return HotStart::NotRunning(not_running(Some(tier), ArmingBlock::PreflightFailed));
            }
            // The boot's shared-BOOK report, for the pairs this account is in: a report, never a refusal
            // (decision 0013), said once at the start.
            for shared in crate::shared_books_for(&table, &rows)
                .iter()
                .filter(|s| s.first == *label || s.second == *label)
            {
                tracing::warn!(
                    venue,
                    book = %shared.book,
                    "TWO {venue} ACCOUNTS SHARE ONE BOOK: {}. The new account is started anyway — this \
                     is a report, not a refusal.",
                    shared.why()
                );
            }
        }

        // THE NEW ACCOUNT'S OWN EVENT LANE, held back: its forwarder buffers until the door opens,
        // which happens only after the core has been handed the engine (module doc, item 1). A start
        // that returns early drops `core_door`, which ends the forwarder ("a door closed with no core").
        let (live_events, live_rx) = vike_exec::event_channel(4096);
        let (core_door, core_door_rx) =
            std::sync::mpsc::channel::<(vike_exec::EventSender, vike_exec::TickSender)>();
        let stop = Arc::clone(&self.forwarder_stop);
        if let Err(e) = std::thread::Builder::new()
            .name(format!("live-event-forward-{route_key}"))
            .spawn(move || run_live_forwarder(live_rx, &core_door_rx, &stop))
        {
            tracing::error!(venue, account = %label, error = %e, "the account's event forwarder could not start");
            return HotStart::NotRunning(not_running(Some(tier), ArmingBlock::AdapterFailed));
        }

        // A node that booted with reconciliation off for want of an armed account starts the driver itself with
        // the first account that has a reconcile client (module doc): that start is built with reconciliation ON
        // and a trigger channel the new driver adopts.
        let starts_driver = !paper && !self.recon_enabled && self.recon_hot.is_some();
        let own_trigger = starts_driver.then(std::sync::mpsc::channel::<()>);
        let mut live_venues: HashSet<String> = HashSet::new();
        let mut env = AdapterEnv::new(&**backend, &table, &live_events, &mut live_venues);
        env.recon_enabled = self.recon_enabled || starts_driver;
        env.recon_trigger =
            self.recon_trigger.clone().or_else(|| own_trigger.as_ref().map(|(tx, _)| tx.clone()));
        env.properties_rec = self.properties_rec.clone();
        env.risk_profile = self.risk_profile.as_ref();
        env.policy = Some(&policy);
        let account_symbols =
            account_symbols_for(&self.mounts, venue, &wired_symbol, &policy.accounts);
        let started = make_engine_for_account(
            &mut env,
            venue,
            symbol_for_account(&account_symbols, label),
            label,
            &legs,
        );
        let (mut engine, recon) = match started {
            Ok(built) => built,
            Err(EngineError::NotRunning(nr)) => return HotStart::NotRunning(nr),
            // The boot path fails the NODE on these; a running node does not (decision 0156 item 6:
            // a refusal of one account is NOT RUNNING for that account only).
            Err(refusal) => {
                tracing::error!(
                    venue, account = %label,
                    "{route_key}: refused at its hot start, it does NOT RUN: {refusal}"
                );
                return HotStart::NotRunning(not_running(Some(tier), ArmingBlock::AdapterFailed));
            }
        };
        // THE LOCK BACKSTOP, as boot makes it: nothing arms outside the set the table named.
        if let Err(e) = refuse_unarmed_live_venues(&armed, &live_venues) {
            tracing::error!(venue, account = %label, "{route_key}: {e}");
            engine.begin_shutdown();
            engine.shutdown();
            return HotStart::NotRunning(not_running(Some(tier), ArmingBlock::AdapterFailed));
        }
        debug_assert_eq!(engine.route_key, route_key, "the engine carries the account's route key");

        // Module doc, item 2: name the venue's existing leg(s) BEFORE a second engine joins.
        let venue_engines = self.engines.iter().filter(|(v, _)| v == venue).count();
        if venue_engines >= 1
            && let Some(legs_tx) = recon_legs
        {
            let _ = legs_tx.send(LegChange::NameAccountsOf(venue.to_string()));
        }
        // The engine is appended with `seed_cash` for equity, the wrong figure in both directions, until the
        // leg's first pass lands the venue's balance: a start that GIVES the driver a leg says so, BEFORE the
        // engine exists, with the flag that leg raises (module doc, "The balance window").
        let first_balance = (recon.is_some() && (recon_legs.is_some() || starts_driver))
            .then(|| Arc::new(AtomicBool::new(false)));
        if let (Some(flag), Some(notice)) = (&first_balance, &self.first_balance_notice) {
            notice(&route_key, flag);
        }
        // The engine, then the door: one FIFO on the core's ingest.
        if self.core_events.add_account_engine(self.seed_cash, engine).is_err() {
            return HotStart::CoreGone;
        }
        let _ = core_door.send((self.core_events.clone(), self.core_ticks.clone()));
        // The reconcile leg, and one poke so the account's first pass is not an interval away.
        if let Some(recon) = recon {
            let mut leg = ReconLeg::account(venue, route_key.clone(), recon);
            if let Some(flag) = first_balance {
                leg = leg.awaited(flag);
            }
            if let Some(legs_tx) = recon_legs {
                let _ = legs_tx.send(LegChange::Add { leg, venue_is_shared: venue_engines >= 1 });
                if let Some(trigger) = &self.recon_trigger {
                    let _ = trigger.send(());
                }
            } else if starts_driver {
                self.start_reconcile_driver(leg, own_trigger);
            }
        }
        self.engines.push((venue.to_string(), route_key.clone()));
        self.stopped.remove(&route_key);
        tracing::warn!(
            venue, account = %label, route_key = %route_key, tier = tier.as_str(),
            "ARMING: {route_key} STARTED in the running tradehub at tier {tier} — its engine joined the \
             core, no restart, no other account touched (decision 0169)"
        );
        if !self.recon_enabled && !paper {
            // Reconciliation is decided once, at start, from the accounts armed THEN (or refused by
            // `flags.reconcile_off`); this account had no reconcile client to start it with. Said, never silent.
            tracing::error!(
                venue, account = %label, route_key = %route_key,
                "ARMING: {route_key} runs with NO reconcile leg: reconciliation is OFF in this process \
                 (it is decided at start, from the accounts armed then, and no armed account asked for \
                 it, or `flags.reconcile_off` refused it), so what the venue holds is not compared with \
                 the core's for this account until the next restart"
            );
        }
        HotStart::Started { route_key, tier }
    }
}

#[path = "hot_tests.rs"]
#[cfg(test)]
mod hot_tests;
