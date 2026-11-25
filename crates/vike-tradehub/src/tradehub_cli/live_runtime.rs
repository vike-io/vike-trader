//! **The LIVE mount** — [`live_runtime`] and [`live_runtime_with`], the arm `run` takes when
//! `flags.tradehub_live` is on: the wired-market `assemble_runtime` core over the resolved mount set,
//! the venue BACKEND it is assembled over ([`VenueBackendFactory`]: the gatewayhub in production), the
//! per-venue feed wiring, and the reconcile driver.
//!
//! Cut VERBATIM out of `crates/vike-tradehub/src/tradehub_cli.rs`, whose `//!` block stays the
//! lifecycle contract and whose `use` block this child shares (`use super::*`): the two are one
//! composition root split for size, not two modules with a boundary between them.
//!
//! ⚠ **This root claims no live-account lock any more** (wave 2, W11; decision 0164). The B11
//! sentinel `LIVE-<route key>.lock` — one live process per venue ACCOUNT — is claimed by the
//! gatewayhub, which owns every venue client now: `VenueBackendGateway::start_adapter` opens the trade
//! lease (the gatewayhub claims the account's lock inside that open) BEFORE it asks for the
//! adapter, and a lease another client holds refuses this whole mount here
//! ([`VenueBackendFactory::lease_refusals`]), as the lock's `AddrInUse` always did. The armed set is still
//! computed here: it decides the reconcile gate.
//!
//! A new parameter goes at the END of either signature: the existing ones keep their places, and
//! `state_dir` is still the directory the `venue_mounted` journal is written in.

use vike_model::accounts::account_keys::AccountLabel;

use super::*;
use crate::server::symbol_admission::SymbolSources;

use crate::account_follow::{AccountFollowBackend, AccountFollowParts, AccountFollowRows};

/// **How the live mount obtains its venue backend** — the seam production fills with the gatewayhub
/// ([`VenueBackendFactoryGateway`]) and the splice tests with the in-process registry (`LocalCtor`, test-only).
///
/// Called TWICE per mount, with the policy as it stands at each point: once for the feed-plan loop
/// (the full-credential rows: a credentialed-data feed keeps the keys exec loses, so the network it
/// dials is judged on them) and once for the mount itself, after the `data_only` withhold. A remote
/// ctor asks the gatewayhub ONCE and answers both from the same snapshot.
pub(super) trait VenueBackendFactory {
    /// A backend over `policy` (the `account` rows and credentials as they stand); `data_only` is the
    /// set of venues whose exec credentials the mount withholds (empty for the plan loop).
    fn backend(
        &self,
        policy: &vike_tradehub_core::AdapterPolicy,
        data_only: &std::collections::HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend>;

    /// The leases another trade client holds, after the mount assembled. Any entry refuses the whole
    /// live mount (the old lock's behaviour). Empty for an in-process backend.
    fn lease_refusals(&self) -> Vec<String> {
        Vec::new()
    }

    /// Who waits for the gatewayhub, given the accounts the mount does not run: a plan for the late
    /// arm when some account is NOT RUNNING with `gatewayhub unavailable`, or when the price feeds wait
    /// for the datahub (`datahub` is its probe then, `feed_gate.rs`).
    fn late_arm(
        &self,
        _not_running: &[vike_tradehub_core::ArmingNotRunning],
        _datahub: Option<crate::feeds::DatahubProbe>,
    ) -> Option<crate::late_arm::LateArmPlan> {
        None
    }

    /// A plan that waits for NOTHING and only carries `why` to the ready banner (a price feed refused by a CAP: no
    /// restart mends it, so none is ordered, yet the process must say it has no prices). `None` for a backend
    /// with no gatewayhub link to name.
    fn notice_plan(&self, _why: crate::feeds::FeedsDown) -> Option<crate::late_arm::LateArmPlan> {
        None
    }

    /// Whether this backend's events arrive over a replaying ring (the gatewayhub's per-account event
    /// ring, replayed to a resuming lease). `true` makes the start override a reconcile policy that rewrites
    /// positions with `quarantine` ([`reconcile_config::quarantine_when_replaying`]). `false` for an in-process backend.
    fn replays_events(&self) -> bool {
        false
    }

    /// A maker of FRESH backends for a hot account start (decision 0169): each call asks the gatewayhub for
    /// its arming table again, over the given rows. `None` for a backend that cannot follow the account
    /// rows (an in-process one): its accounts keep the restart path.
    fn fresh_backend(&self) -> Option<AccountFollowBackend> {
        None
    }

    /// The settings-database watch and the account reader the account watcher follows the rows with
    /// (decision 0169): handed over once, at the end of the mount. `None` where nothing follows the rows
    /// (an in-process backend, a store that could not be watched).
    fn take_rows(&self) -> Option<AccountFollowRows> {
        None
    }

    /// The core is up: its published snapshot, for whatever re-teaches resting orders after a gatewayhub
    /// restart. Called once, after assembly. Nothing to do for an in-process backend.
    fn core_up(&self, _snapshot: Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>) {}

    /// Where the control door and the boot re-admission look up the facts of a symbol an engine was
    /// not started on (decision 0173): the open trade leases. `None` for a backend with no lease to
    /// ask (an in-process one), whose accounts then trade the symbol they were started on.
    fn symbol_facts(
        &self,
    ) -> Option<Arc<crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry>> {
        None
    }

    /// Where the start reports its steps (`None` = nowhere: every ctor but the production one).
    fn start_progress(&self) -> Option<crate::server::starting::StartProgress> {
        None
    }

    /// The process's stop flag, which a wait inside the start (the boot's wait for the venue balances, decision
    /// 0246) ends on (`None` = watches nothing: every ctor but the production one).
    fn stop_flag(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        None
    }
}

/// **The gatewayhub as the live mount's backend** (production): the first call connects (the bounded
/// wait, only when an account needs a venue), every later call is a view of that one connection.
pub(super) struct VenueBackendFactoryGateway {
    dial: GatewayDial,
    client_id: String,
    connected: std::sync::OnceLock<Arc<VenueBackendGateway>>,
    mounted: std::sync::Mutex<Option<Arc<VenueBackendGateway>>>,
    /// What the boot restore tells the leases (resume cursors, the order client ids to prefetch wire ids for).
    boot: crate::venue_backend::gateway::restore::BootHints,
    /// The core's snapshot, late-bound; shared with every backend view.
    cell: crate::venue_backend::gateway::reteach::SnapshotCell,
    /// The rows the account watcher follows ([`VenueBackendFactory::take_rows`]).
    follow_rows: std::sync::Mutex<Option<AccountFollowRows>>,
}

impl VenueBackendFactoryGateway {
    /// A ctor over `dial`; `client_id` names this process's leases.
    pub(super) fn new(dial: GatewayDial, client_id: String) -> Self {
        VenueBackendFactoryGateway {
            dial,
            client_id,
            connected: std::sync::OnceLock::new(),
            mounted: std::sync::Mutex::new(None),
            boot: crate::venue_backend::gateway::restore::BootHints::none(),
            cell: crate::venue_backend::gateway::reteach::SnapshotCell::default(),
            follow_rows: std::sync::Mutex::new(None),
        }
    }

    /// This ctor handing the account watcher its rows (`None` = nothing follows them).
    #[must_use]
    pub(super) fn with_follow_rows(self, rows: Option<AccountFollowRows>) -> Self {
        *self.follow_rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = rows;
        self
    }

    /// This ctor handing the boot restore's hints to every lease it opens.
    #[must_use]
    pub(super) fn with_boot(
        mut self,
        boot: crate::venue_backend::gateway::restore::BootHints,
    ) -> Self {
        self.boot = boot;
        self
    }
}

impl VenueBackendFactory for VenueBackendFactoryGateway {
    fn backend(
        &self,
        policy: &vike_tradehub_core::AdapterPolicy,
        data_only: &std::collections::HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend> {
        let connected = self.connected.get_or_init(|| {
            if let Some(progress) = &self.dial.progress {
                progress.phase("connecting to the gatewayhub");
            }
            let backend = VenueBackendGateway::connect(
                self.dial.link.clone(),
                policy.accounts.clone(),
                self.client_id.clone(),
                self.dial.connect_wait,
            )
            .with_boot(self.boot.clone())
            .with_snapshot_cell(Arc::clone(&self.cell))
            .with_project_state_dir(vike_boot::halt::declared_project_state_dir())
            .with_start_progress(self.dial.progress.clone());
            if let Some(progress) = &self.dial.progress {
                progress.phase("starting the accounts");
            }
            Arc::new(backend)
        });
        let view = Arc::new(connected.view(policy.accounts.clone(), data_only.clone()));
        *self.mounted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&view));
        view
    }

    fn fresh_backend(&self) -> Option<AccountFollowBackend> {
        let connected = Arc::clone(self.connected.get()?);
        Some(Box::new(move |accounts, data_only| {
            Arc::new(connected.refreshed(accounts, data_only))
                as Arc<dyn vike_tradehub_core::VenueBackend>
        }))
    }

    fn take_rows(&self) -> Option<AccountFollowRows> {
        self.follow_rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    fn lease_refusals(&self) -> Vec<String> {
        self.mounted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or_else(Vec::new, |backend| backend.lease_refusals())
    }

    fn late_arm(
        &self,
        not_running: &[vike_tradehub_core::ArmingNotRunning],
        datahub: Option<crate::feeds::DatahubProbe>,
    ) -> Option<crate::late_arm::LateArmPlan> {
        // Decision 0169 item 9: when the account watcher follows the rows (`follow_rows` is still here: the
        // mount hands it over at its end), an account NOT RUNNING for want of the gatewayhub is started by
        // that watcher, on a backoff, when the gatewayhub answers: no restart is ordered for it. Only the
        // price feeds (and the accounts of a venue whose feeds wait) keep the late-arm restart, and a mount
        // that cannot follow the rows keeps it for the accounts too.
        let followed =
            self.follow_rows.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some();
        let waiting: Vec<String> = if followed {
            Vec::new()
        } else {
            not_running
                .iter()
                .filter(|n| n.block == vike_model::ArmingBlock::GatewayUnavailable)
                .map(vike_tradehub_core::ArmingNotRunning::route_key)
                .collect()
        };
        crate::late_arm::LateArmPlan::of(self.dial.link.clone(), waiting, datahub)
    }

    fn notice_plan(&self, why: crate::feeds::FeedsDown) -> Option<crate::late_arm::LateArmPlan> {
        Some(crate::late_arm::LateArmPlan::notice(self.dial.link.clone(), why))
    }

    fn replays_events(&self) -> bool {
        true
    }

    fn core_up(&self, snapshot: Arc<arc_swap::ArcSwap<vike_execution::CoreSnapshot>>) {
        // Filled once, after assembly; the worker reads it only when a gatewayhub restart happens.
        let _ = self.cell.set(snapshot);
    }

    fn symbol_facts(
        &self,
    ) -> Option<Arc<crate::venue_backend::gateway::trade_lease_registry::TradeLeaseRegistry>> {
        self.connected.get().map(|backend| backend.symbol_facts())
    }

    fn start_progress(&self) -> Option<crate::server::starting::StartProgress> {
        self.dial.progress.clone()
    }

    fn stop_flag(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        self.dial.stop.clone()
    }
}

/// **Why the LIVE start ended without a node**, and whether a restart would meet the same answer:
/// `operator_needed` is a refusal only an operator mends (a refused configuration, a full cap, a missing key), which
/// exits 78 so the supervisor parks the tradehub (`super::refused`); anything else exits 1 and is restarted.
#[derive(Debug)]
pub(super) struct LiveStartFailed {
    pub(super) why: String,
    pub(super) operator_needed: bool,
}

impl LiveStartFailed {
    /// A refusal only an operator mends.
    pub(super) fn refused(why: impl Into<String>) -> Self {
        LiveStartFailed { why: why.into(), operator_needed: true }
    }

    /// The assembly's refusal: a mount on an account that does not run for its own cause, a missing risk budget, or
    /// no account at all that runs (the assembly says so only when none waits for its venue: one that does, an IB
    /// Gateway that is down, starts the node with no engine, owner 2026-11-15) are an operator's; a probe
    /// under-count, the OS and the backend are not (they keep today's restart).
    pub(super) fn assembly(e: &vike_tradehub_core::RuntimeError) -> Self {
        use vike_tradehub_core::RuntimeError;
        let operator_needed = match e {
            RuntimeError::Mount(_)
            | RuntimeError::RiskBudget(_)
            | RuntimeError::NoAccountRuns { .. } => true,
            RuntimeError::ForwarderSpawn(_)
            | RuntimeError::UnarmedLiveVenues { .. }
            | RuntimeError::Backend(_) => false,
        };
        LiveStartFailed { why: format!("build_live_multi_strategy_core: {e}"), operator_needed }
    }
}

/// A failure a restart may mend: one with no class of its own.
impl From<String> for LiveStartFailed {
    fn from(why: String) -> Self {
        LiveStartFailed { why, operator_needed: false }
    }
}

/// A feed's refusal the start did not survive: a CAP needs an operator (`crate::feed_gate::failed_start_code`, decided
/// on the class), anything else a restart may mend.
impl From<crate::feeds::FeedRefusal> for LiveStartFailed {
    fn from(refusal: crate::feeds::FeedRefusal) -> Self {
        let operator_needed = crate::feed_gate::failed_start_code(refusal.class).is_some();
        LiveStartFailed { why: refusal.text, operator_needed }
    }
}

/// What a LIVE mount hands back: the core, its teardown handles, `assemble_runtime`'s own arming record,
/// the late-arm plan (`Some` when an account waits for the gatewayhub) and every account the node does
/// NOT run with why (decision 0156: `vike_tradehub_core::RuntimeAssembly::not_running`, published
/// with every frame).
pub(super) type RuntimeLive = (
    CoreHandle,
    StrategyLanesLiveTeardown,
    std::collections::HashSet<String>,
    Option<crate::late_arm::LateArmPlan>,
    Vec<vike_execution::ArmingNotRunningView>,
);

/// The LIVE mount (`flags.tradehub_live`): stand up the wired-market [`vike_tradehub_core::assemble_runtime`] core
/// with the resolved strategy mounts folded in (split-plane I10: one for the historical
/// single-mount profile, N for a `[[mounts]]` one), then wire each DISTINCT mounted venue's live
/// market feed onto it. Returns the live
/// [`CoreHandle`] plus the [`StrategyLanesLiveTeardown`] handles. HARD-errors (never a silent paper fallback) on any
/// venue/symbol the daemon has not wired for live — the daemon's own allow-list (safety gates #4/#5).
/// The PER-ACCOUNT credential gate (#2, the gatewayhub's arming answer) and each venue's own network
/// gate (#3, decision 0095: the account's own tier for binance/bybit/okx/hyperliquid, decision 0119)
/// still apply INSIDE `assemble_runtime`, so an account with no credentials is NOT RUNNING with its
/// `ArmingBlock`, never a paper stand-in (decision 0156).
///
/// `risk_profile` is the active `run` row's [`vike_config::ProfileRun`] `[risk]` table (ProfileRun
/// wiring — closing the live gap): threaded straight into [`RuntimeConfig::risk_profile`], which
/// `assemble_runtime` applies uniformly to every wired venue's `RiskLimits` via
/// `vike_tradehub_core::make_engine`. `None` (no active `run` row) leaves every venue's `RiskLimits` as
/// `make_engine` builds them, and a live-intent venue then REFUSES the mount. This is the SAME
/// budget `resolve_paper_risk_limits` already arms on the PAPER mount above, reaching the LIVE one.
/// `make_engine` judges it under the grid source of each mounted account's TIER (decision 0156
/// item 7: a run profile states none), so a profile's venue-owned instrument fields reach a `paper`
/// account and the mount of every `demo`/`live` one is REFUSED, by name
/// (`vike_tradehub_core::EngineError::VenueOwnedRiskFields`).
///
/// `policy` is this MACHINE's `policy` settings, resolved once by [`resolve_settings`] at the
/// top of [`main`] and projected here onto [`vike_tradehub_core::AdapterPolicy`] — the subset a venue mount
/// applies (settings-unification Phase 6c). A DIFFERENT authority from `risk_profile`: per-machine
/// and admin-owned, with no env and no CLI layer at all.
///
/// ⚠ It does NOT arm anything: since decision 0119 an account trades at its own `account.tier`
/// while `account.active = 1`, so the `account` table snapshot ([`live_runtime_with`]'s `accounts`)
/// decides: an account runs at its own tier or NOT AT ALL (decision 0156), and a box whose every
/// active row is `paper` trades only the simulator, whatever the credential store holds.
/// `market_slippage` is a binding field, carried to the gatewayhub on the `AdapterStart` and consumed
/// by the hyperliquid bridge's start as `AdapterRequest::market_slippage` (this daemon's primary live
/// venue, and the only roster venue with no native market order, so its every market intent and every
/// tripped stop-MARKET is priced at that band); for that one, no row leaves the bridge on
/// hyperliquid's own compiled-in literal, byte-identically.
///
/// `flags` is the resolved [`vike_config::Flags`] — this arm consumes `reconcile`,
/// `tradehub_record` and `oco_cancel_sibling_on_dead_exit`, each a row with no environment layer
/// (decision 0111). They arrive as a parameter rather than being read here so that ONE
/// load decides them for the whole process.
///
/// `mounts` is the resolved mount SET (split-plane I10: N strategies / N venues, one row for the
/// historical single-mount profile) — each entry carries its strategy (the A-S maker by default,
/// or whatever that profile row's `[strategy]` table named) and its strategy-free `spec`
/// ([`vike_tradehub_core::build_live_multi_strategy_core`] mounts on it, and this function's FEED
/// wiring reads it: `spec.symbol`/`spec.interval` name what to subscribe, `spec.tick_size` sizes a
/// book, and the Polymarket arm's `TickBarSynthesizer` window is `spec.interval_ms`).
///
/// The FIRST entry is the PRIMARY mount: its `spec` supplies the per-venue account seed
/// (`RuntimeConfig::seed_cash` / `CoreConfig::seed_cash`), exactly as the single-mount daemon always
/// did. Each DISTINCT venue's feed arm is wired exactly once, however many mounts share the venue
/// (subscriptions dedup per series inside the arm); the teardown handle carries one [`StrategyLanesLive`]
/// entry per venue.
///
/// ⚠ The `allow` matches [`live_runtime_with`]'s, which has carried one since it was written, and for
/// the same reason: every parameter here is a FACT THIS FUNCTION MAY NOT RESOLVE ITSELF — the
/// policy, the flags, the sentinel directory, the origin claim, the WAL map — and bundling them
/// into a struct to satisfy a count would hide the one property
/// `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs` follows through this signature by INDEX.
#[expect(clippy::too_many_arguments)]
pub(super) fn live_runtime(
    mounts: Vec<MountResolved>,
    risk_profile: Option<vike_model::ProfileRisk>,
    profile: Option<vike_config::ProfileRun>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    state_dir: &std::path::Path,
    // `config.instance_origin` — see [`live_runtime_with`]. LAST, and deliberately not beside
    // `flags`: `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs` pins `state_dir`'s POSITION in
    // this signature (it is the live-account sentinel's directory, and the gate follows it by
    // index), so a new parameter goes after it or the pin measures the wrong argument.
    instance_origin: Option<vike_model::OrderClientIdOrigin>,
    // The WAL's directory rung (`config.journal_dir`, `config.journal_snapshot_every`) — see
    // `crate::profile_rows::JournalRung`. A PARAMETER for the same reason `instance_origin` is one: this function resolves no settings of its own, and
    // `main` resolves it ONCE so this core's `journal` and the paper mount's name the same
    // directory (the materializer's tail-follow was the second reader
    // until the `materialize` feature was deleted on 2026-09-22). AFTER `state_dir`, like every
    // other addition — see the note above.
    journal_rung: &crate::profile_rows::JournalRung,
    // What the journal restores into this core (`vike_core::replay::restore_for_boot`, decision
    // 0127), computed ONCE by `run` from the same WAL the paper arm would get. LAST, like every
    // addition since the live-lock gate pinned `state_dir`'s position.
    boot_restore: vike_core::replay::RestoreReport,
    // The trading-mount batch's `config.*` rows (`MountConfigRows`), resolved ONCE by `run`: this
    // function reads no setting of its own. LAST, for the same reason as `boot_restore`.
    rows: MountConfigRows,
    // Where the gatewayhub is and how long to wait for it at start (`config.gatewayhub_addr`,
    // `config.gatewayhub_connect_wait_ms`), resolved ONCE by `run`. LAST, for the same reason as
    // `rows`.
    dial: GatewayDial,
    // This install's past order client id sessions (`vike.db`'s `order_client_id_session`), read ONCE by `run`:
    // see [`live_runtime_with`]. LAST, for the same reason as `dial`.
    order_client_id_past_sessions: Vec<(String, u64)>,
    // Where the session this core mints under is recorded before it mints ([`order_client_id_session_recorder`]).
    order_client_id_session_recorder: Option<vike_core::OrderClientIdSessionRecorder>,
) -> Result<RuntimeLive, LiveStartFailed> {
    // ⚠ THE `account` TABLE IS READ HERE, in the composition root, and nowhere below it. Since
    // decision 0119 it is what ARMS a venue — each account trades at its own `tier` while `active`
    // — and since decision 0156 item 6 it owns each account's credentials too: this root keeps their
    // key NAMES ([`read_account_directory`]), the gatewayhub hands every bridge the fields of the ONE
    // account row it starts (dukascopy's legal entity is that row,
    // `crates/bridges/dukascopy/src/adapter.rs`), and the arming projection must describe
    // exactly what the mount will do. Until 2026-09-15
    // `vike-tradehub-core` opened the store itself, from a library file, at a directory taken from a
    // process global: the class `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
    // `CREDENTIAL_STORE_PIN` ratchets down, and invisible to it because neither account reader
    // was one of `CREDENTIAL_STORE_READERS`' keyed names. Both are keyed now, and this root —
    // already pinned — performs the read.
    //
    // ONE snapshot, handed to [`live_runtime_with`] as a PARAMETER (the seam tests plant their own
    // rows through it) and carried on the policy object both seams receive, so no caller can hand
    // the projection and the mount different tables. Errors are carried VERBATIM rather than
    // swallowed: a store that exists and will not open refuses a labelled account by NAMING the
    // failure, where a swallowed error used to be re-reported as a bad `account` row.
    // ⚠ The account watcher's commit counter is opened BEFORE this read (decision 0169): a commit made while
    // the node assembles then shows at the watcher's first poll instead of falling between the two.
    let follow_rows = match settings::credential_store().watch() {
        Ok(Some(watch)) => {
            Some(AccountFollowRows { watch, read: Box::new(read_account_directory) })
        }
        Ok(None) => None,
        Err(e) => {
            tracing::error!(
                error = %e,
                "the settings database cannot be watched, so the account rows are NOT followed: an \
                 account that becomes active starts with a restart (decision 0169)"
            );
            None
        }
    };
    let mut accounts = read_account_directory();
    let feeds = production_feed_ctors(dial.stop.clone());
    let mounted = distinct_mount_venues(&mounts);
    // A box that can run nothing yet WAITS here, its door answering `starting` with why, never refusing its start into
    // a supervisor restart every few seconds (owner 2026-11-14): for the hubs when every account it runs needs them
    // (`super::hub_wait`), and for the FX open when every one is an FX account and the market is closed (no FX login is
    // made then). One loop, so a wait for the hubs that ends inside the FX close still waits for the open, and the rows
    // are read again after the FX wait.
    loop {
        accounts = super::hub_wait::wait_for_the_hubs(accounts, &mounted, &dial, &feeds)?;
        let Some(open) =
            crate::account_follow::nothing_runs_before_the_fx_open(&accounts, vike_model::now_ms())
        else {
            break;
        };
        if !wait_for_the_fx_open(open, &dial) {
            return Err("stopped while waiting for the FX market to open".to_string().into());
        }
        accounts = read_account_directory();
    }
    // What the boot restore tells the gatewayhub leases: each account's resume cursor and the order client ids the
    // restore will ask the wire-id prefetch about (the journal's orders and the ownership file's).
    let boot_hints = crate::venue_backend::gateway::restore::BootHints::from_restore(
        &boot_restore,
        strategy_state_dir().map(|d| d.join(vike_core::order_owners::ORDER_OWNERS_FILE)).as_deref(),
        vike_model::now_ms(),
    );
    live_runtime_with(
        mounts,
        risk_profile,
        profile,
        policy,
        flags,
        state_dir,
        instance_origin,
        journal_rung,
        &feeds,
        accounts,
        boot_restore,
        rows,
        &VenueBackendFactoryGateway::new(dial, client_id())
            .with_boot(boot_hints)
            .with_follow_rows(follow_rows),
        PRICE_ONLY_SWEEP,
        order_client_id_past_sessions,
        order_client_id_session_recorder,
    )
}

/// **Wait for the FX week's open** (`open`, epoch ms) plus [`crate::late_arm::FIRST_POLL`] (the gatewayhub's clock may be
/// a moment behind this one), said once and on the door's `starting` line. `false` when the process stop flag rose first.
fn wait_for_the_fx_open(open: i64, dial: &GatewayDial) -> bool {
    let words = vike_venues::session::fx_login_closed_words(open);
    tracing::warn!(
        "ARMING: every account this tradehub runs is an FX account and {words}: no account can run before \
         the open, so the start WAITS for it here, once (no FX login is made meanwhile, and the process \
         does not exit into a restart loop)"
    );
    if let Some(progress) = &dial.progress {
        progress.phase(&format!("waiting for the FX open ({words})"));
    }
    let margin = i64::try_from(crate::late_arm::FIRST_POLL.as_millis()).unwrap_or(i64::MAX);
    let until = open.saturating_add(margin);
    loop {
        if dial.stop.as_ref().is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            return false;
        }
        let left = until.saturating_sub(vike_model::now_ms());
        if left <= 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(u64::try_from(left.min(1_000)).unwrap_or(1_000)));
    }
}

/// **The `account` table now**, with every account's key NAMES and no credential VALUE: the boot's one
/// reader of the rows, called by [`live_runtime`] at start and by the account watcher at every change
/// of the rows (decision 0169), so the two cannot disagree about what a row is. Errors are carried
/// VERBATIM inside the directory rather than swallowed: a store that exists and will not open refuses
/// a labelled account by NAMING the failure.
pub(super) fn read_account_directory() -> vike_secrets::account_directory::AccountDirectory {
    let directory = vike_secrets::account_directory::AccountDirectory::read(
        settings::credential_store().accounts(),
        settings::credential_store().account_keys(),
        // An EMPTY book, as the gatewayhub's own rows reader passes (`gatewayhub_cli/wire.rs`'s
        // `account_rows`): this daemon holds no venue credential VALUE (decision 0146's second
        // reopener). The gatewayhub, which owns every venue connection, reads an account's fields
        // itself and ANSWERS whether the account arms (`ArmingBlock::NoCredentials`,
        // `LiveCredentialsAbsent`); what this root keeps of the credentials is their key NAMES,
        // which `account_keys` above already carries ([`vike_tradehub_core`]'s
        // `holds_credentials` reads them for the level of the `ARMING:` line, and the `data_only`
        // disclosure below counts them).
        Ok::<_, String>(vike_secrets::account_credentials::CredentialRegistry::new()),
    );
    // The price tap marks a derived lane twin's own symbol, which is the rows' (`note_lane_twins`): learnt at every
    // read, before an engine started from these rows can take an order.
    crate::feeds::price_tap::note_lane_twins(&directory);
    directory
}

/// The production feed constructor: the dial to the datahub's strategy lane (wave 2 of `vike-gatewayhub`).
/// The address is `config.datahub_addr` (the datahub's own loopback default when the row is unset: a
/// tradehub and its datahub share a box), and the key is the tradehub's strategy-lane key
/// (`VIKE_DATAHUB_TRADE_KEY`) out of the hub-key store `hub_server::resolve_daemon_hub_key_store` reads.
/// An absent key is not an error HERE: every dial then refuses by name (absent credentials are the gate),
/// and a store that exists and will not read says so at `error` and is the same refusal.
pub(super) fn production_feed_ctors(
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> StrategyLaneFactoriesProduction {
    let addr = settings::boot_config()
        .datahub_addr
        .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string());
    // The key is read AGAIN at every dial and at every probe (the late arm polls the probe): a start with no key
    // is held with the reason, and a key the operator mints afterwards must be found by the next poll.
    // `stop` is the process's stop flag: the wait before an arm asks again ends when it is raised.
    StrategyLaneFactoriesProduction::with_key_source(addr, datahub_trade_key).with_stop(stop)
}

/// The tradehub's strategy-lane key out of the hub-key store, read NOW. A store that EXISTS and cannot be read is
/// an ERROR (said once per process, not once per poll), never "no key": the answer is still `None`.
fn datahub_trade_key() -> Option<Vec<u8>> {
    static UNREADABLE_SAID: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    match crate::hub_server::resolve_daemon_hub_key_store(settings::credential_store()) {
        Ok(resolved) => resolved
            .secrets
            .into_map()
            .remove(vike_hub_protocol::auth::DATAHUB_TRADE_KEY_ENV)
            .map(String::into_bytes),
        Err(e) => {
            if !UNREADABLE_SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::error!(
                    error = %e,
                    "the hub-key store is PRESENT but UNREADABLE — the datahub strategy-lane key cannot be
                     read, so no price feed can be dialled. This is NOT an absent key: fix the store's
                     permissions and restart"
                );
            }
            None
        }
    }
}

/// **The STORED funding series a restarted paper engine catches up from** (`vike_core::CoreConfig::funding_history`,
/// decision 0262's amendment of 2026-11-08): one dial of the datahub's strategy lane with this daemon's lane key per
/// restored funding clock (`vike_datahub_client::strat::funding_history`, the `StrategyFundingHistory` read). The address and
/// the key are read when a catch-up asks, never at construction, as the feed's dial reads them; no key, or a datahub
/// without `funding_history`, is the `Err` the core warns with and books nothing for.
pub(super) fn datahub_funding_history() -> vike_core::FundingHistory {
    Box::new(|venue: &str, symbol: &str, after: i64, before: i64| {
        let addr = settings::boot_config()
            .datahub_addr
            .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string());
        let key = datahub_trade_key().ok_or_else(|| {
            format!(
                "datahub {addr}: this daemon holds no strategy-lane key ({}), so the stored funding cannot be read",
                vike_venues::credential_keys::datahub_trade_key()
            )
        })?;
        let keys = vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_trade(key);
        vike_datahub_client::strat::funding_history(&addr, &keys, venue, symbol, after, before)
    })
}

/// **The EXECUTION STREAM's publisher** ([`crate::execution_stream`]), started once the core runs, on either arm: to
/// `config.datahub_addr` (the datahub's loopback default when unset) under this daemon's datahub trade key, read again
/// at every dial as the feeds read it. `None`, said once, without a journal (nothing to stream) or when its thread
/// cannot start (then the journal keeps every execution, unsent).
pub(super) fn start_execution_stream(
    handle: &CoreHandle,
    journal_dir: Option<&Path>,
    tradehub: &str,
    settings_dir: Option<std::path::PathBuf>,
    stop: Arc<AtomicBool>,
) -> Option<crate::execution_stream::ExecutionStreamPublisher> {
    let (Some(dir), Some(watch)) = (journal_dir, handle.journal_durable_watch()) else {
        tracing::info!(
            "execution stream: this node keeps no journal (`config.journal_dir` is unset), so no execution is sent to \
             the datahub"
        );
        return None;
    };
    let source = crate::execution_stream::ExecutionStreamSource {
        journal_dir: dir.to_path_buf(),
        watch,
        datahub_addr: settings::boot_config()
            .datahub_addr
            .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string()),
        tradehub: tradehub.to_string(),
        keys: Arc::new(|| {
            datahub_trade_key().map(|key| {
                vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_trade(key)
            })
        }),
        settings_dir,
        timings: crate::execution_stream::ExecutionStreamTimings::default(),
    };
    match crate::execution_stream::ExecutionStreamPublisher::spawn(source, stop) {
        Ok(publisher) => Some(publisher),
        Err(e) => {
            tracing::error!(
                error = %e,
                "execution stream: its thread could not start, so nothing is sent to the datahub; the journal keeps \
                 every execution"
            );
            None
        }
    }
}

/// **The journal ARCHIVE thread** ([`crate::journal_archive`]), started once the core runs, on either arm, under the
/// policy the four archive rows set (`config.journal_keep_hours`, `config.journal_archive_max_gib`,
/// `config.journal_archive_max_days`, `config.journal_archive_full`; decision 0304). `None` without a journal (nothing
/// to archive) or when its thread cannot start, said at ERROR: the live directory then keeps every segment.
pub(super) fn start_journal_archive(
    handle: &CoreHandle,
    journal_dir: Option<&Path>,
    config: &vike_config::Config,
    stop: Arc<AtomicBool>,
) -> Option<crate::journal_archive::JournalArchiver> {
    use vike_config::config::{
        DEFAULT_JOURNAL_ARCHIVE_MAX_DAYS, DEFAULT_JOURNAL_ARCHIVE_MAX_GIB,
        DEFAULT_JOURNAL_KEEP_HOURS,
    };
    let (Some(dir), Some(watch)) = (journal_dir, handle.journal_durable_watch()) else {
        return None;
    };
    let hours = config.journal_keep_hours.unwrap_or(DEFAULT_JOURNAL_KEEP_HOURS);
    let gib = config.journal_archive_max_gib.unwrap_or(DEFAULT_JOURNAL_ARCHIVE_MAX_GIB);
    let days = config.journal_archive_max_days.unwrap_or(DEFAULT_JOURNAL_ARCHIVE_MAX_DAYS);
    let policy = vike_journal::JournalArchivePolicy {
        keep_ms: i64::from(hours) * vike_model::MS_PER_HOUR,
        max_bytes: u64::from(gib) << 30,
        max_age_ms: i64::from(days) * vike_model::MS_PER_DAY,
        at_limit: config.journal_archive_full.unwrap_or_default(),
    };
    match crate::journal_archive::JournalArchiver::spawn(dir.to_path_buf(), watch, policy, stop) {
        Ok(archiver) => Some(archiver),
        Err(e) => {
            tracing::error!(
                error = %e,
                "journal archive: its thread could not start, so no superseded segment leaves the live journal \
                 directory (nothing is lost; the directory grows)"
            );
            None
        }
    }
}

/// **This install's order client id sessions at boot** (`vike.db`'s `order_client_id_session`): the session the
/// journal restored (`restored`, at the sequence it reached) recorded there first, then every session the table holds,
/// for `vike_core::CoreConfig::order_client_id_past_sessions`. No project: none. A store that cannot be read is said at
/// ERROR and gives none (a late fill on a past session's id then reads foreign), never a refused start.
pub(super) fn order_client_id_sessions_at_boot(
    settings_dir: Option<&Path>,
    restored: Option<&(String, u64)>,
) -> Vec<(String, u64)> {
    use vike_secrets::order_client_id_sessions::raise_order_client_id_session;
    let Some(dir) = settings_dir else { return Vec::new() };
    if let Some((session, sequence)) = restored
        && let Err(e) = raise_order_client_id_session(dir, session, *sequence, vike_model::now_ms())
    {
        tracing::warn!(
            error = %e,
            "the restored order client id session could not be recorded in the settings store"
        );
    }
    recorded_order_client_id_sessions(Some(dir))
}

/// **Where the core records the session it mints under, before it mints** (`vike_core::CoreConfig::
/// order_client_id_session_recorder`): a closure over the settings directory that raises the session's
/// `order_client_id_session` row to its starting sequence plus
/// [`crate::execution_stream::ORDER_CLIENT_ID_SESSION_LEASE`]. The row is then written before the core can mint
/// under the session, and it bounds every id the session mints until the publisher raises it again, so an id minted
/// a moment before a crash is still this install's after its journal is moved aside. No project: none (no install
/// identity to remember). A failed write is said at ERROR, never a refused start.
pub(super) fn order_client_id_session_recorder(
    settings_dir: Option<&Path>,
) -> Option<vike_core::OrderClientIdSessionRecorder> {
    let dir = settings_dir?.to_path_buf();
    Some(Box::new(move |session: &str, sequence: u64| {
        let leased =
            sequence.saturating_add(crate::execution_stream::ORDER_CLIENT_ID_SESSION_LEASE);
        if let Err(e) = vike_secrets::order_client_id_sessions::raise_order_client_id_session(
            &dir,
            session,
            leased,
            vike_model::now_ms(),
        ) {
            tracing::error!(
                session,
                error = %e,
                "the order client id session this core mints under could not be recorded in the settings store: \
                 a late fill on one of its ids may read as foreign if its journal is ever moved aside"
            );
        }
    }))
}

/// **This install's order client id sessions as `vike.db` holds them** (`order_client_id_session`), read BEFORE the
/// boot restore so its replay cores hold them too (`vike_core::replay::restore_for_boot`'s `past_sessions`): a late
/// fill on an id a session older than the readable journal minted regenerates as ours. No project: none. A store that
/// cannot be read is said at ERROR and gives none, never a refused start.
pub(super) fn recorded_order_client_id_sessions(settings_dir: Option<&Path>) -> Vec<(String, u64)> {
    let Some(dir) = settings_dir else { return Vec::new() };
    vike_secrets::order_client_id_sessions::read_order_client_id_sessions(dir).unwrap_or_else(|e| {
        tracing::error!(
            error = %e,
            "this install's order client id sessions cannot be read from the settings store: a late fill on an id \
             of a past session is reported as foreign"
        );
        Vec::new()
    })
}

/// **A paper account's symbol facts** (decision 0285's amendment of 2026-11-16): one dial of the datahub's strategy
/// lane with this daemon's lane key (`vike_datahub_client::strat::symbol_facts`, the `StrategySymbolFacts` read: the
/// store's recorded row, else the venue's public catalog row the datahub reads once), the address and the key read at
/// each read as [`datahub_funding_history`] reads them. No key, or a datahub without `symbol_facts`, is a `Refused`
/// `Err`; a datahub that did not answer is a link word (`vike_datahub_client::strat::symbol_facts`).
fn datahub_symbol_facts(
    venue: &str,
    symbol: &str,
) -> Result<vike_model::SymbolProperties, (vike_utility::errors::FailureOutcome, String)> {
    let addr = settings::boot_config()
        .datahub_addr
        .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string());
    let key = datahub_trade_key().ok_or_else(|| {
        let why = format!(
            "datahub {addr}: this daemon holds no strategy-lane key ({}), so a symbol's facts cannot be read",
            vike_venues::credential_keys::datahub_trade_key()
        );
        (vike_utility::errors::FailureOutcome::Refused, why)
    })?;
    let keys = vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_trade(key);
    vike_datahub_client::strat::symbol_facts(&addr, &keys, venue, symbol)
}

/// **The STORED series a mount's INPUTS read** (decision 0271's live half, [`super::inputs`]): one dial of
/// the datahub's strategy lane with this daemon's lane key per read (`vike_datahub_client::strat::input_bars`,
/// the `StrategyInputBars` read), the address and the key read at each read as [`datahub_funding_history`]
/// reads them. No key, or a datahub without `input_bars`, is the `Err` the core refuses the mount with.
pub(super) fn datahub_input_reader() -> super::inputs::InputReader {
    std::sync::Arc::new(|id: &vike_data::SeriesId, start: i64, end: i64| {
        let addr = settings::boot_config()
            .datahub_addr
            .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string());
        let key = datahub_trade_key().ok_or_else(|| {
            format!(
                "datahub {addr}: this daemon holds no strategy-lane key ({}), so a stored input cannot be read",
                vike_venues::credential_keys::datahub_trade_key()
            )
        })?;
        let keys = vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_trade(key);
        vike_datahub_client::strat::input_bars(
            &addr,
            &keys,
            &id.venue,
            &id.symbol,
            id.interval.as_deref().unwrap_or_default(),
            id.source.as_deref(),
            start,
            end,
        )
    })
}

/// **The forecasts the live strategies published, read back** (book W9, [`super::drift_watch`]): one dial of the
/// datahub's strategy lane with this daemon's lane key per read (`vike_datahub_client::strat::forecasts`, the
/// `StrategyForecasts` read), the address and the key read at each read as [`datahub_input_reader`] reads them.
/// No key, or a datahub without `forecasts`, is the `Err` the drift watch says once.
pub(super) fn datahub_forecast_reader() -> super::drift_watch::ForecastReader {
    std::sync::Arc::new(|venue: &str, symbol: &str, start: i64, end: i64| {
        let addr = settings::boot_config()
            .datahub_addr
            .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDRESS.to_string());
        let key = datahub_trade_key().ok_or_else(|| {
            format!(
                "datahub {addr}: this daemon holds no strategy-lane key ({}), so the published forecasts cannot be read",
                vike_venues::credential_keys::datahub_trade_key()
            )
        })?;
        let keys = vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new()).with_trade(key);
        vike_datahub_client::strat::forecasts(&addr, &keys, venue, symbol, start, end)
    })
}

/// [`live_runtime`] with its impurities as PARAMETERS — the `account` snapshot (`accounts`,
/// production: [`live_runtime`]'s one read, key NAMES and no value) and the
/// feed-construction seam (production: [`StrategyLaneFactoriesProduction`], the dial to the datahub's strategy lane). The split exists for the deterministic data-only splice test
/// (`src/tradehub_cli/tests/feed_splice.rs`): a scripted constructor plus fake credentials planted in
/// the account directory is what lets a CI test drive the REAL mount path — plan gate, withhold,
/// `assemble_runtime`, feed wiring — with no store, no network and no weekday market.
///
/// The third return is `assemble_runtime`'s own `live_venues` record — the per-venue EXEC ARMING STATE
/// (`vike_tradehub_core::make_engine` inserts a venue exactly when it constructs a real exec client). It is
/// what the seam test asserts "the data-only declaration kept exec paper" against, and it is also
/// what `main` RENDERS: the ready banner's `mode` and every `MountViewWire::live` are derived from
/// it, so no paper-vs-live report can disagree with the mount that produced it.
///
/// ⚠ [`live_runtime`] used to DISCARD it, on the reasoning that "the arming disclosures inside
/// [`wire_venue_feeds`] already read it". Those disclosures are `tracing` lines; the ready banner is
/// the string `docs/ops/tradehub-the CI box.md` and `deploy/vike-tradehub.service` both call the
/// authority on paper-vs-live, and it was being built from the PROFILE instead. MEASURED on the CI box,
/// one startup: nine venues in this set, one venue in that banner. Do not re-narrow the return.
// ⚠ MORE arguments than clippy's default allows, and the shape is the point rather than an
// oversight: this is the daemon's live composition seam and every parameter is an INJECTED
// IMPURITY — the resolved mount set, the operator budget, the run profile, the machine policy, the
// flags, the state directory the B11 sentinels are claimed in, this
// deployment's origin claim, the feed constructors and the account snapshot. That list is what lets
// `src/tradehub_cli/tests/feed_splice.rs` drive the REAL mount path with no store, no network and no real
// keys; bundling them into a struct would hide exactly the substitutions those tests exist to
// make. `state_dir` was the argument that crossed clippy's threshold, and it cannot be resolved
// here: only the boot walk knows it.
// ⚠ A NEW parameter goes at the END, after `state_dir` — see that parameter's own note: the
// live-account sentinel gate follows it through this signature by INDEX. This comment used to
// carry a COUNT of the arguments, which is why it now does not.
#[expect(clippy::too_many_arguments)]
pub(super) fn live_runtime_with(
    mounts: Vec<MountResolved>,
    risk_profile: Option<vike_model::ProfileRisk>,
    // The SAME run profile `risk_profile` was derived from, carried WHOLE so its `[guards]` and
    // `[sinks]` tables can reach the `CoreConfig` below. Threading it stopped at `[risk]` before:
    // every guard an operator wrote was parsed, validated, announced as ignored — and dropped.
    profile: Option<vike_config::ProfileRun>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    // Where the B11 live-account sentinels are claimed (`<state_dir>/LIVE-<venue>.lock`) — the
    // daemon's own `booted.state_dir`, a PARAMETER because the claim is made here (after the
    // `data_only` withhold, before `assemble_runtime`) and this function resolves no paths of its own.
    state_dir: &std::path::Path,
    // `config.instance_origin` — this deployment's origin claim, stamped into every client order
    // id this core mints so a SECOND instance on the same venue account is recognisable on
    // reconcile rather than anonymous (`vike_model::orders::client_id::origin`). A PARAMETER because this
    // function resolves no settings of its own; `None` (the default) is byte-identical to before
    // the key existed.
    //
    // ⚠ AFTER `state_dir`, not beside `flags`, and that is a constraint rather than a preference:
    // `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs` follows the sentinel directory through
    // this signature BY INDEX, so a parameter inserted ahead of `state_dir` makes that gate pin
    // the wrong argument. Add new ones at the end.
    instance_origin: Option<vike_model::OrderClientIdOrigin>,
    // The WAL's directory rung — see [`live_runtime`].
    journal_rung: &crate::profile_rows::JournalRung,
    make: &dyn StrategyLaneFactories,
    // The `account` table snapshot — which accounts trade, at which tier (decision 0119), and each
    // account's credential key NAMES (a seam test plants fields by value, decision 0156 item 6) — read
    // ONCE by [`live_runtime`] (production) or planted by a seam test, and carried on `AdapterPolicy::accounts`. LAST,
    // like every addition since the live-lock gate pinned `state_dir`'s position.
    accounts: vike_secrets::account_directory::AccountDirectory,
    // The boot restore `run` computed (see [`live_runtime`]); applied to the core config below, after
    // the run profile's `[guards]`. LAST, for the same reason as `accounts`.
    boot_restore: vike_core::replay::RestoreReport,
    // The trading-mount batch's `config.*` rows — see [`live_runtime`]. LAST, for the same reason.
    rows: MountConfigRows,
    // The venue BACKEND the mount is assembled over (production: [`VenueBackendFactoryGateway`], the gatewayhub;
    // the splice tests: an in-process one). LAST, for the same reason as `rows`.
    ctor: &dyn VenueBackendFactory,
    // The core's risk-clock cadence (`price_only_config`): `PRICE_ONLY_SWEEP` from [`live_runtime`], always;
    // a splice test that waits on a sweep passes a short one. LAST, for the same reason as `ctor`.
    price_only_sweep: Duration,
    // This install's past order client id sessions (`vike_core::CoreConfig::order_client_id_past_sessions`), so a
    // late fill on an id one of them minted is reported as ours; a seam test hands in none. LAST, for the same reason.
    order_client_id_past_sessions: Vec<(String, u64)>,
    // The core's `order_client_id_session_recorder` ([`order_client_id_session_recorder`]); a seam test hands in none.
    order_client_id_session_recorder: Option<vike_core::OrderClientIdSessionRecorder>,
) -> Result<RuntimeLive, LiveStartFailed> {
    // The profile's `[inputs]` remap (decision 0271), daemon-wide: every lowered row carries the same table.
    let input_remap = mounts.first().map(|m| m.row.inputs.clone()).unwrap_or_default();
    // The reconcile FAMILY's rows — cadences, lookbacks, the policy name, the balance tolerances,
    // AND the two sibling FLAGS (`reconcile_generate_missing`, `reconcile_balance`) — read into the
    // one value `reconcile_config::build_reconcile_config` takes below, from the same resolved `flags`
    // the gate reads and the boot's `config` (`daemon_reconcile_settings`). Rows only (decision 0111):
    // no `VIKE_RECONCILE_*` variable reaches it, and an unset policy is `quarantine`
    // (`reconcile_config::parse_policy`).
    //
    // ⚠ The MASTER GATE itself is decided further down, not here, and the move is deliberate: since
    // S2 the default is ON for a mount that arms a live venue account, and that fact is not known
    // until the policy has been through the `data_only` withhold below and `vike_tradehub_core::armed_live_venues`
    // has been asked. Deciding it up here would either read a pre-withhold policy (arming reconcile for
    // a venue this daemon deliberately leaves on paper) or force a second probe call that could
    // disagree with the one the lock claim uses.
    let reconcile_settings = daemon_reconcile_settings(flags, &settings::boot_config());

    // THE MOUNT'S POLICY, built ONCE and BEFORE the plan loop, because the loop asks the venue
    // registry which NETWORK each venue's accounts mount on (`venue_feed_plan` →
    // `venue_arming::feed_tier`/`cex_mainnet_enabled`) and that answer has to come from the very
    // value the mount is built from — a bridge's `resolve` that reads the `account` table or a
    // `venue_setting` row must see the same snapshot in both places, or the feed's network is a
    // second opinion again. It is built ahead of the `data_only` withhold pass and that pass takes
    // the declared venues' credentials OUT of it (each account's key names ride on its
    // `accounts`, decision 0156 item 6), after the plans resolved; it is moved into the
    // `RuntimeConfig` below, exactly as before.
    let mut adapter_policy = vike_tradehub_core::AdapterPolicy {
        // The ONE `account` table snapshot [`live_runtime`] read (see the read's own comment there),
        // carried on the policy object both seams receive.
        accounts,
        // `config.paper_slippage` — what a `paper`-tier account's simulated exchange slips fills
        // by. `None` (no row) keeps the mount's compiled default, the book it always built.
        paper_slippage: rows.paper_slippage,
        // `config.paper_slippage_table` — the slippage table those books are priced
        // by instead, when the row is set (never beside `config.paper_slippage`). `None` keeps the flat figure.
        paper_slippage_table: rows.paper_slippage_table.clone(),
        // `config.preflight_disk_warn_mib` / `_fail_mib` — the disk floors the startup preflight
        // judges the journal directory by; no rows is the compiled pair.
        preflight_disk: rows.preflight_disk,
        // The venues whose engines FAIL CLOSED on symbols (decision 0174 item 7): exactly the ones the
        // control door admits symbols for. Set HERE, in the literal both seams are built from, and not
        // inside the `data_only` loop below, which a box with no declared venue never enters.
        fail_closed_symbol_venues: crate::server::symbol_admission::admitting_venues(),
        // `flags.preflight_skip` — the startup preflight is skipped at the boot and at every hot start (the
        // core reads it off this policy). A flag, not a `policy` row, so `From<&Policy>` below cannot fill it.
        preflight_skip: flags.preflight_skip,
        ..vike_tradehub_core::AdapterPolicy::from(policy)
    };

    // Safety gates #4/#5 — the daemon's venue+symbol ALLOW-LIST, resolved per MOUNT and BEFORE
    // the policy is moved into the RuntimeConfig (split-plane I10: every mount must pass its own venue's
    // gate, and the venue set collects ONE plan per DISTINCT venue, in mount order — the plan is
    // venue+environment-derived, so mounts sharing a venue share its plan). Any unwired venue is a
    // HARD ERROR (never a silent paper fallback). The dispatch itself lives in `venue_feed_plan`
    // (`feeds.rs`), where `FEED_ARMS` lists its arms. Safety gate #3 (feed side,
    // decision 0095) is now INSIDE that dispatch: `venue_feed_plan` takes the mount policy and asks
    // each venue's registry row which network its accounts dial.
    // ⚠ The plan loop reads its OWN arming snapshot, built from the policy as it stands HERE (before the
    // `data_only` withhold below takes credentials out of it): the feed keeps the keys exec loses, so
    // the network it dials is judged on the full-credential rows. The mount's snapshot is built after
    // the withhold.
    //
    // ⚠ With the gatewayhub as the backend this is where the bounded wait for it happens (only when
    // an account needs a venue, `crate::venue_backend::gateway::needs_gateway`): the table the gatewayhub
    // computes from its own read of the account rows and credentials is fetched ONCE, here, and every later
    // read — the mount's own table included — is a view of that one snapshot.
    let plan_table = ctor
        .backend(&adapter_policy, &std::collections::HashSet::new())
        .arming_table()
        .map_err(|e| format!("refusing the live mount: {e}"))?;
    let mut venue_plans: Vec<(String, StrategyLanePlan)> = Vec::new();
    for m in &mounts {
        let plan = venue_feed_plan(&m.spec, &plan_table, &adapter_policy)
            .map_err(LiveStartFailed::refused)?;
        if !venue_plans.iter().any(|(v, _)| v == &m.spec.venue) {
            venue_plans.push((m.spec.venue.clone(), plan));
        }
    }
    // The price-only feeds' venues (wired below, once the core exists): every `Admits` venue with a feed
    // dial that no mount names. Hyperliquid's network is decided HERE, from the same rows and the same
    // function as its mounted arm (`feed_tier`: mainnet when a `live` account is armed), because the
    // policy moves into the runtime config before the feeds are wired.
    let admitting = crate::server::symbol_admission::admitting_venues();
    let hl_unmounted = admitting.iter().any(|a| a == "hyperliquid")
        && !venue_plans.iter().any(|(v, _)| v == "hyperliquid");
    let hl_price_network =
        hl_unmounted.then(|| crate::feeds::hl_price_network(&plan_table, &adapter_policy));
    // …and the tier that network is (the core compares it with each account's own; `price_only_config`).
    let hl_price_tier = hl_unmounted
        .then(|| crate::venue_arming::tier::feed_tier("hyperliquid", &plan_table, &adapter_policy));
    // The DATA-PLANE-ONLY declarations (the `data_only` profile key, validated to the
    // credentialed-data venue set at load): withhold each declared venue's credentials from the
    // account directory `assemble_runtime`'s exec arms will read (the policy's `accounts`: a credential
    // belongs to its account, decision 0156 item 6) — AFTER the plan loop above, which is the
    // ordering the seam rests on (the gatewayhub's credentialed stream logs in from its own read, so the
    // FEED keeps the credentials exec loses; see
    // [`crate::venue_arming::withhold_data_only_credentials`]). An undeclared mount leaves the
    // policy untouched, byte-identically. The startup line is the declaration's own disclosure —
    // the arming banner inside `wire_venue_feeds` then announces the resulting paper exec per
    // venue ([`data_only_arming`]).
    let data_only: std::collections::HashSet<String> = mounts
        .iter()
        .filter(|m| m.row.data_only_effective())
        .map(|m| m.spec.venue.clone())
        .collect();
    for venue in &data_only {
        let (accounts, withheld) =
            crate::venue_arming::withhold_data_only_credentials(&adapter_policy.accounts, venue);
        // This daemon's book is empty (it holds no venue value), so the disclosure counts the key
        // NAMES the venue's accounts own; the larger of the two is the count of a directory that
        // does hold a book (the seam tests').
        let keys_withheld =
            withheld.max(crate::venue_arming::held_key_names(&adapter_policy.accounts, venue));
        adapter_policy.accounts = accounts;
        tracing::warn!(
            venue = %venue,
            keys_withheld,
            "DATA-ONLY mount declared for {venue} (`data_only = true` in the profile): its \
             credentials are WITHHELD from the exec mount, so exec stays on the PAPER book by \
             declaration while the credentialed market feed authenticates from the same store"
        );
    }
    // A credentialed-data venue whose DEFAULT demo account holds no usable key set is refused HERE, before a
    // core, a restore or a lease exists (the gatewayhub's credentialed stream would refuse it after all of
    // them, and the arming table already says so). `plan_table` is the full-credential snapshot, so a
    // `data_only` venue's feed credentials are judged as the feed will see them: those venues are skipped
    // (their feed account is a `paper` row the arming table cannot judge) and left to the gatewayhub.
    for (venue, plan) in &venue_plans {
        if let Some(why) =
            crate::feed_gate::credentialed_feed_refusal(venue, plan, &plan_table, &data_only)
        {
            return Err(LiveStartFailed::refused(why));
        }
    }
    // THE PRICE FEEDS' GATE (W12b, `crate::feed_gate`): the prices come from the datahub's strategy lane, so
    // a datahub that cannot be dialled (or a missing strategy-lane key) means no engine of a mounted venue
    // can run on a price. Asked HERE, before the core exists, because the core cannot add or drop an engine
    // later: the `demo`/`live` accounts of the mounted venues are held NOT RUNNING below, `paper` accounts
    // run without prices, the feeds are left unwired and the late arm waits for the datahub. BEFORE wave 2
    // the feeds were in-process and this start did not depend on the datahub at all.
    let mounted_venues: Vec<String> = venue_plans.iter().map(|(v, _)| v.clone()).collect();
    let datahub = make.datahub_probe(&mounted_venues);
    let feed_down: Option<crate::feeds::FeedsDown> =
        datahub.as_ref().and_then(crate::feeds::DatahubProbe::check);
    // THE MOUNT'S BACKEND AND ITS ARMING SNAPSHOT, read ONCE here: after the `data_only` withhold (so a
    // declared venue's exec keys are already gone and its rows say they do not run, which is what
    // this daemon actually does), over the final `adapter_policy`. The armed
    // set below, the journal rows and the `assemble_runtime` this moves into all read this one
    // table.
    //
    // ⚠ NO LIVE-ACCOUNT LOCK IS CLAIMED HERE ANY MORE (split-plane B11, the Danger-2 tripwire: one
    // live process per venue ACCOUNT). The claim moved to the process that owns the venue clients:
    // the gatewayhub claims `LIVE-<route key>.lock` when it answers the account's `TradeOpen`, which
    // `VenueBackendGateway::start_adapter` sends before it asks for the adapter, so a second trading
    // process is refused before a venue client exists. A lease another client holds refuses this
    // whole mount just after assembly ([`VenueBackendFactory::lease_refusals`]) with no market data wired,
    // as the lock's `AddrInUse` always did. What stays here is the armed SET, because the SAME
    // list decides whether this mount reconciles:
    //
    // ⚠ The probe reads the SETTINGS, not this profile's mounts, and that is correct rather than an
    // omission: a second account ARMS on its own ACTIVE `account` row plus its own
    // credentials, and `make_engine_accounts` mounts it whether or not a `[[mounts]]` row names it.
    let backend = ctor.backend(&adapter_policy, &data_only);
    let backend = match &feed_down {
        Some(why) => {
            let held: std::collections::BTreeSet<String> =
                venue_plans.iter().map(|(v, _)| v.clone()).collect();
            tracing::error!(
                reason = %why,
                venues = %held.iter().map(String::as_str).collect::<Vec<_>>().join(", "),
                "{}: {}: every `demo`/`live` account of the mounted venues is NOT RUNNING this session \
                 (the table carries the word `{}`; the cause is the datahub's strategy lane, not the \
                 gatewayhub, and `vike-backend venues` may still show them as would-arm), no feed is wired, \
                 and `paper` accounts run without prices on those venues. The late arm restarts this \
                 tradehub armed once the datahub (and, for an account that waits for it, the gatewayhub) \
                 answers: {why}",
                crate::feed_gate::PRICE_FEEDS_UNAVAILABLE,
                crate::feeds::short_cause(why),
                vike_model::ArmingBlock::FeedsUnavailable.as_str(),
            );
            crate::feed_gate::VenueBackendFeedGate::wrap(
                backend,
                held,
                format!("the price feeds are unavailable: {why}"),
            )
        }
        None => backend,
    };
    let table = backend.arming_table().map_err(|e| format!("refusing the live mount: {e}"))?;
    let armed_live = vike_tradehub_core::armed_live_venues(&table, &adapter_policy);

    // THE RECONCILE MASTER GATE (S2) — ON by DEFAULT for a mount that arms a live venue account,
    // paired with `quarantine` so it folds nothing. One decision — in `vike-ops`, and called
    // identically by `vike-app`'s `App::new`, until the desktop lost its local core and the gate
    // moved here (2026-09-23) — so the GUI and the daemon could not differ about what a restart does.
    //
    // ⚠ THE CONSEQUENCE, stated where it happens: from here a live mount issues AUTHENTICATED READ
    // calls against every armed account at startup and then every `config.reconcile_interval_ms`
    // (default 60 s), whether or not anybody asked. That is the price of not trading against a
    // belief, and it is bounded (a handful of REST reads per venue per minute, inside every wired
    // venue's published budget) — but aster has no testnet account in the store (its testnet
    // exists and is routed; only a `live` aster account is configured), so on a box that arms aster
    // they are MAINNET reads. `reconcile_gate`'s own doc carries the argument and the three ways to refuse.
    let reconcile_gate =
        reconcile_config::reconcile_gate(flags.reconcile, flags.reconcile_off, armed_live.len());
    // Disclosed at WARN whichever way it went, through the ONE shared emitter: "reconcile is on and
    // nobody asked" and "reconcile is off on a live box" are both things an operator must be able to
    // find in a journal without knowing which flag to grep for, and `vike-app` had to say it the same
    // way while it reconciled. See `reconcile_config::log_reconcile_gate`.
    reconcile_config::log_reconcile_gate(reconcile_gate, armed_live.len());
    let reconcile_enabled = reconcile_gate.enabled();
    // ...and the one CROSS-mount consistency gate, likewise before any core spawns: two polymarket
    // mounts on ONE token must agree on the interval, because they will share one `TickSink`
    // whose bar synth runs at exactly one window (see `StrategyLanesLive::Polymarket`).
    check_polymarket_token_intervals(&mounts).map_err(LiveStartFailed::refused)?;
    // ...and its ctrader twin (split-plane I9): every ctrader mount shares one data socket and one
    // bar SYNTHESIZER window, so rows disagreeing on the interval are refused before any core
    // spawns — see `check_ctrader_intervals`.
    check_ctrader_intervals(&mounts.iter().map(|m| &m.spec).collect::<Vec<_>>())
        .map_err(LiveStartFailed::refused)?;

    // Opt-in PIT-`SymbolProperties` recorder (`flags.record_properties`). ⚠ ALWAYS `None` since
    // the daemon stopped writing the store (#2093, 2026-09-22, docs/decisions/0084):
    // `PropertiesRecorder` opens the concrete DataFusion store, which only a `record-feeds`/
    // `materialize` build carried (both enabled the DataFusion tree, now `vike-store`, and #2093
    // deleted both); the recorder that survives runs inside the datahub, which opens its own
    // store. So nothing is constructed here, and `flags.record_properties` is inert in this binary
    // whatever sets it: nothing here reads it.
    //
    // ⚠ This comment described a construction HERE, through the recorder's map half over a
    // credential map, until 2026-09-28 — a call this function has not made since #2093. Decision
    // 0111 deleted that map half and the recorder's environment door with it.
    let properties_rec: Option<Arc<vike_data::PropertiesRecorder>> = None;

    // The live core config carries the SAFETY KNOBS a live daemon must be capped with (NOT the paper
    // mount's minimal `CoreConfig::default()`). Field names/values mirrored vike-app's fat-build
    // CoreConfig (the desktop builds none since #1610): a 30s submit-ack backstop, a 25%
    // equity-drawdown liquidate-only latch, and the margin-call watchdog (inert at 1× until
    // leverage is raised). `reconcile_enabled` is the
    // S2 reconcile gate resolved above (a PAPER mount ⇒ `false`, byte-identical to the pre-reconcile
    // daemon: `assemble_runtime` builds no reconnect-trigger channel). `strategy`/`extra_mounts` are
    // left unset here — `build_live_multi_strategy_core` folds the resolved mounts into them
    // before `assemble_runtime` consumes the config.
    //
    // The PRIMARY (first) mount's `seed_cash` supplies the per-venue account seed, exactly as the
    // single-mount daemon always passed its one mount's — `assemble_runtime` applies `RuntimeConfig::
    // seed_cash` uniformly to every extra venue engine and `core_config.seed_cash` to the primary,
    // and a per-ROW seed would need a per-venue seed table `assemble_runtime` does not take (stated
    // rather than silently summed: `validate_for_live`'s seed refusal runs per row either way).
    let primary_seed = mounts[0].spec.seed_cash;
    // THE ARMING ROWS, computed HERE and nowhere else in this function. The window is narrow and
    // both edges are load-bearing: AFTER `withhold_data_only_credentials` above (so a `data_only`
    // venue's exec keys are already gone from `adapter_policy`, and the rows say paper for it, which
    // is what this daemon actually does) and BEFORE `adapter_policy` moves into `RuntimeConfig`
    // below (after which it does not exist to read).
    //
    // This is why `vike_tradehub_core::journal_venue_mounts` takes ROWS rather than the policy. A signature
    // taking the policy would let the call sit anywhere downstream and be wrong by a comment; taking
    // rows makes the caller pick this moment, and there is only one.
    let arming = vike_tradehub_core::venue_arming(&table, &adapter_policy);
    // What the control door needs to know about the account-wide exposure ceiling, read BEFORE the policy
    // moves into the `RuntimeConfig`: the policy row and the account rows (the engine takes the tighter of
    // the two, so either one being set means the account has a ceiling).
    let account_cap_policy = adapter_policy.max_account_exposure.is_some();
    let account_rows = adapter_policy.accounts.clone();
    // ...and bound NOW, before the engines are built: an ibkr account's start asks the registry whether its
    // account has a ceiling, because the ceiling is the opt-in for converting a quote currency (the gatewayhub
    // reads the account's currency, a dial of a scarce budget, only for an account that will convert).
    // The ENGINES THE BOOT BUILT are judged by the rows the boot built them from. An account started later in
    // the running node is judged by `TradeLeaseRegistry::engine_starting` (`account_follow`), from the rows ITS engine
    // was built from: a snapshot of the boot's rows would disagree with it both ways.
    if let Some(facts) = ctor.symbol_facts() {
        let account_rows = account_rows.clone();
        facts.bind_exposure_cap(move |route_key| {
            let venue = vike_model::accounts::account_keys::venue_of_route_key(route_key);
            account_cap_policy
                || vike_model::accounts::account_keys::label_of_route_key(venue, route_key)
                    .is_some_and(|label| {
                        crate::venue_backend::gateway::trade_lease_registry::rows_arm_a_cap(
                            false,
                            &account_rows,
                            venue,
                            &label,
                        )
                    })
        });
    }

    // THE DEAD-MAN'S ABSENT-KEY WARNING, once, beside the construction it describes (the
    // `deadman:` field of the literal below). Here and not in `deadman_config_from_policy`, because
    // this is the composition root where `policy` is known to be a LIVE mount's — the fold is pure
    // and the gate-off `start_paper` arm never calls it, so a warning inside the fold would be a
    // warning nobody could count. Fires for an ABSENT key only: `deadman_timeout_ms = 0` is the
    // operator's recorded decision and gets no line. `warn_deadman_absent`'s doc argues the shape.
    warn_deadman_absent(policy);

    // THE PER-MOUNT LOSS BUDGET, said once when it is armed (the `mount_budget_default:` field of the
    // literal below). Silent for an absent row, which is every box that has not written the key.
    // …and the LEVERAGE cap the same way (decision 0249's amendment of 2026-11-23).
    let budget_lines = [mount_budget_arming_line(policy), mount_leverage_arming_line(policy)];
    for line in budget_lines.into_iter().flatten() {
        tracing::info!("{line}");
    }

    // THE LINK DEAD-MAN's venue set and its per-venue report (M13). The set is the DISTINCT venues
    // this daemon mounts, in mount order — `venue_plans` above already collected exactly that, and
    // it is the right set rather than a convenient one: a feed thread exists only for a mounted
    // venue, so a venue outside it can never disclose a `FeedStatus` and arming it would watch a
    // link nothing reports on. The report is emitted here, at INFO, one line per venue, because
    // "which venues does my automatic stop actually cover" is a startup question and the answer is
    // a join of a policy key, a per-venue table and this mount's own venue list — none of which an
    // operator can compute from the file alone.
    // ⚠ The PLAN travels with the venue, not just its name: whether the switch can fire here is a
    // question about the LANES this daemon subscribed, and `adapter_link_disclosure` is the reading
    // of the feed wiring that answers it. Without it the mount announced an armed switch on every
    // CEX venue while nothing could reach the latch for any of them.
    let mut link_venues: Vec<(String, DeadManLinkDisclosure)> =
        venue_plans.iter().map(|(v, plan)| (v.clone(), adapter_link_disclosure(plan))).collect();
    // …and the PRICE-ONLY venues (the door admits symbols for them, no mount names them): the same
    // pump or datahub link carries their prices once a symbol is admitted, so the switch hears them
    // too — and the core acts on them only while they hold an admitted symbol or a live order
    // (`CoreConfig::price_only` below), so an idle box is not halted over a pump nobody uses.
    let price_only_set = crate::feeds::price_only_venues(&admitting, &mounted_venues);
    link_venues.extend(price_only_set.iter().map(|v| (v.clone(), price_only_link_disclosure(v))));
    for line in link_deadman_arming_report(policy, &link_venues) {
        tracing::info!("{line}");
    }

    // What this node keeps fresh for the inputs the core opens (decision 0271's live half), and the opener
    // that fills it, the paper arm's too (`super::inputs::datahub_inputs`): the bar keeper and the lane
    // refresher after the core is up read it.
    let (input_keep, input_opener) = super::inputs::datahub_inputs(input_remap);

    let mut runtime_config = RuntimeConfig {
        // THE VENUE BACKEND over the registry this daemon names the bridges in (docs/decisions/0096,
        // amended 2026-09-29): `assemble_runtime` names no bridge, and is handed the backend that
        // answered the table above.
        backend,
        properties_rec,
        seed_cash: primary_seed,
        reconcile_enabled,
        risk_profile,
        // The machine's policy, projected onto what a venue mount applies. No `policy` rows ⇒
        // `Policy::default()` ⇒ `AdapterPolicy::default()` ⇒ every venue keeps its compiled-in
        // literal, byte-identical to this daemon before Phase 6c.
        //
        // ⚠ The SAME value the live-account lock claims above were computed from — one projection,
        // so the set that was locked and the set the arms resolve cannot disagree by construction.
        policy: adapter_policy,
        core_config: vike_core::CoreConfig {
            seed_cash: primary_seed,
            // The origin claim this daemon's order client ids carry — the parameter above,
            // straight through. Consumed at a FRESH order client id session only (a restart resumes its
            // persisted one verbatim): `vike_core::CoreConfig::instance_origin` argues why.
            instance_origin,
            // ⚠ These three are the daemon's DEFAULTS, not its answer: `profile`'s `[guards]` table
            // overwrites whichever of them it names, via `apply_guards_and_sinks` below. Before that
            // call existed they were the only values that could ever apply, and an operator's
            // configured guards were parsed, warned about and discarded.
            submit_ack_timeout: Some(vike_model::DEFAULT_SUBMIT_ACK_TIMEOUT),
            max_drawdown: Some(0.25),
            margin_call: Some(vike_model::MarginCallConfig::default()),
            // THE DEAD-MAN SWITCH, from the `policy` settings and nothing else (M4). OPT-IN: armed only
            // when the operator wrote `deadman_timeout_ms = <n>`; an absent key and an explicit
            // `0` both leave this `None`, and `warn_deadman_absent` above is what tells the two
            // apart. (This comment read "Armed by default — sixty seconds of ingest silence
            // cancels the book" for one morning; the key's doc records the reversal.)
            // `deadman_config_from_policy` is the whole fold and argues each half;
            // `crates/vike-config/tests/policy_is_consumed.rs` names THIS line. ⚠ Not one of the
            // three `apply_guards_and_sinks` overwrites below: a run profile has no `[guards]`
            // field for it, deliberately — a profile may not lower a policy ceiling, and
            // `the_run_profile_cannot_touch_the_deadman` pins that.
            // ⚠ When written, armed by the live GATE, not by any venue arming: this literal is
            // built before `assemble_runtime` decides per venue, so an all-paper or `data_only` run
            // under the gate carries the switch too (the key's doc states the shape and the open
            // decision).
            deadman: deadman_config_from_policy(policy),
            // THE CONNECTION-STATE DEAD-MAN (M13) — ARMED BY DEFAULT, unlike the switch above, and
            // the asymmetry is the whole point of the re-ruling: this one observes what the BRIDGE
            // reports about the link, so a market that merely closed cannot trip it, so it may
            // have an armed default. `link_deadman_config_from_policy` folds three things — the
            // policy grace (absent ⇒ armed at `DEFAULT_LINK_DEADMAN_GRACE_MS`, `0` ⇒ off), the
            // per-venue table `vike_venues::link_deadman_default`, and the venues this mount
            // actually has — and returns `None` when the grace is off OR no mounted venue is
            // armed, so an FX-only daemon builds nothing. The per-venue INFO lines above say which
            // it was. `crates/vike-config/tests/policy_is_consumed.rs` names THIS line.
            // ⚠ Not reachable from a run profile's `[guards]`, deliberately, exactly like the
            // sibling: a profile may not lower a policy ceiling.
            link_deadman: link_deadman_config_from_policy(policy, &link_venues),
            // THE PER-MOUNT LOSS BUDGET (decision 0249) — from the `policy.max_mount_loss` and
            // `policy.mount_loss_action` rows and nothing else. ABSENT ⇒ `None` ⇒ the core assembles
            // every mount with no budget and arms no sweep: byte-identical to this daemon before the
            // rows. Present ⇒ EVERY mount, the ones assembled here and the ones mounted at run time,
            // is held to it, judged on the 30-second risk clock and on every closed bar (cold; the
            // fold is untouched). A `policy` row is never hot-applied, so it is read here, at start.
            // ⚠ Not reachable from a run profile's `[guards]`, deliberately: a profile may not lower
            // a policy ceiling. `crates/vike-config/tests/policy_is_consumed.rs` names THIS line.
            mount_budget_default: mount_budget_default_from_policy(policy),
            // …and a KEYED budget for each boot mount whose own `mount.max_leverage` column states a
            // cap, the lower of it and `policy.max_mount_leverage` (the column may only narrow the
            // row), keyed by the id the core keys the mount by. No column ⇒ no key ⇒ the default.
            mount_budgets: mount_budgets_from_columns(
                policy,
                mounts.iter().map(|m| {
                    let id = vike_core::strategy_state::mount_id_with(
                        m.spec.controller_id.as_deref(),
                        &m.spec.venue,
                        &m.spec.symbol,
                        &m.spec.interval,
                    );
                    (id, m.row.max_leverage)
                }),
            ),
            // THE RISK CLOCK, AND THE PRICE-ONLY VENUES (docs/ops/price-only-venue-safety-plan.md): the
            // core sweeps the margin call and the drawdown latch off the clock for EVERY engine (owner
            // ruling 2026-10-25: a mount on 4-hour bars is not margin-checked once per 4 hours), so it is
            // ALWAYS `Some`, with an empty venue set when every admitting venue is mounted. No mount
            // drives a price-only venue's engine, so the clock is the only thing that ever runs those
            // two for it, and the core gates the link dead-man above on its holdings.
            price_only: Some(price_only_config(&price_only_set, hl_price_tier, price_only_sweep)),
            // Honor the same opt-in write-ahead journal the paper mount does (off by default):
            // on exactly when the `config.journal_dir` row is set, in that directory, tuned by the
            // active run row's `[sinks.journal]` when it carries one. The same
            // function over the same two inputs as `run`'s one resolution, which the paper core
            // takes — see `crate::profile_rows::journal_config_for`.
            journal: crate::profile_rows::journal_config_for(profile.as_ref(), journal_rung),
            // OPT-IN OCO SIBLING-CANCEL ON A DEAD EXIT (off by default) —
            // `flags.oco_cancel_sibling_on_dead_exit`. When a released bracket exit terminates
            // UNFILLED, the default KEEPS the surviving OCO sibling so the position retains
            // whatever protection it still has; on, the sibling is canceled and the book is left
            // flat. Off ⇒ byte-identical to this daemon before the flag reached it.
            //
            // ⚠ vike-app was this flag's ONLY reader in the tree, so the behaviour existed in the
            // GUI and was UNREACHABLE on the server that runs unattended — the one deployment where
            // "leave the book flat when protection dies" is most likely to be what an operator
            // wants. Both mounts read it now (the paper arm via `StrategyCorePaperConfig`), so the rehearsal
            // and the live run cannot disagree about it either.
            oco_cancel_sibling_on_dead_exit: flags.oco_cancel_sibling_on_dead_exit,
            // OPT-IN SHUTDOWN POLICY (off by default) — `flags.cancel_orders_on_shutdown`. OFF (the default, and the behaviour that has always
            // shipped) leaves every resting order LIVE AT THE VENUE when the daemon stops, with
            // nothing left running to manage it; ON cancels the book during teardown, inside the
            // `[daemon] shutdown_deadline_ms` budget. It never flattens — positions survive either
            // way.
            //
            // ⚠ Reachable on every stop that TEARS DOWN: an interactive `shutdown`/`quit`/Ctrl-D,
            // AND — since 2026-08-07 — SIGTERM, whose handler (installed first thing in `run`)
            // raises the same stop flag. Under `deploy/vike-tradehub.service` stdin is `/dev/null`,
            // so `systemctl stop` is the stop that matters, and it now reaches this sweep inside
            // `TimeoutStopSec=`; a binary older than that date killed the process before any of it
            // ran — see `docs/ops/kill-switches.md` §D, which says so to the operator.
            cancel_orders_on_shutdown: flags.cancel_orders_on_shutdown,
            // Runtime strategy mount/unmount (split-plane B5): resolve a wire `MountStrategy`'s
            // `[strategy]`-vocabulary spec through the daemon's own profile machinery
            // (`crate::mount_factory` — same refusals as a profile load). Absent this,
            // the core refuses every runtime mount. Grafted through the I10 rebase: the
            // relocated multi-mount RuntimeConfig must answer MountStrategy exactly as B5's did.
            strategy_factory: Some(crate::mount_factory::strategy_factory(
                crate::mount_factory::project_strategies_dir(),
                true,
            )),
            // PAPER FUNDING from the venue's public rate (the funding-rate lane): a paper engine is
            // charged by the simulator's one formula at each settlement its venue rolls past; a demo or
            // live engine is never charged from the rate (its venue's own settlement events are its
            // funding). The law is `ContractValue`'s; `true` arms the lane (absent, funding is OFF).
            charge_paper_funding: true,
            // ...and a restarted paper engine catches up the settlements it missed while the node was down from
            // the datahub's stored funding series, read on the strategy lane (`datahub_funding_history`).
            funding_history: Some(datahub_funding_history()),
            // A mount's INPUTS (decision 0271's live half): each series its strategy declares, read from the
            // datahub's store at mount and kept fresh by this node afterwards (`super::inputs`).
            input_opener: Some(input_opener),
            // The KEPT rows a restart's closed trades read a market's class from (`CoreConfig::session_rows`), read
            // from the datahub's lists at spawn: the tail is booked before any mount comes up.
            session_rows: super::settings::session_rows(),
            // A COMBO's legs, which the core writes into its engine with no `AdmitSymbol`, are committed to the
            // account's gatewayhub like an admitted symbol, so its reconcile client reads them (and a restarted
            // gatewayhub is told them again): `TradeLeaseRegistry::tell_combo_legs`, on a thread of its own.
            on_combo_legs: ctor.symbol_facts().map(|facts| facts.combo_leg_teller()),
            // Durable strategy state + runtime-mount topology (B5 residual closed): arms the
            // per-mount `<mount_id>.json` sidecars AND the `runtime_mounts.json` topology record
            // the resurrect below replays. `None` (no project) disarms
            // both — byte-identical to this daemon before the residual closed.
            state_dir: strategy_state_dir(),
            // Order ownership across a restart (decision 0113): `order_owners.jsonl` beside the
            // sidecars above, so an order left resting over a restart still reaches the mount that
            // placed it, and that mount's ledger is still its position. `None` (no state root)
            // records nothing, as before.
            order_owners: strategy_state_dir()
                .map(|d| vike_core::order_owners::OrderOwnerLog::in_state_dir(&d)),
            // The operator's refusal of ORDER RESTORE (`flags.reconcile_restore_off`, default
            // false): `true` leaves the previous session's resting orders out of the engine
            // registry and the tag registry while ownership and fills restore as before. Read from
            // the SAME resolved `flags` the reconcile gate above reads, so one boot decides both;
            // it is NOT a `ReconcileSettings` knob (it configures the core, not the pass). The paper
            // arm (`start_paper`) runs no reconcile pass and leaves the field at its default.
            restore_orders_off: flags.reconcile_restore_off,
            // The venue gate of that restore follows the reconcile VERDICT resolved above. A box
            // that refused reconcile (`flags.reconcile_off`) mounts no driver, so no pass ever
            // adopts a restored order: with the declared table's gate the core would mute the
            // strategy's tagged resubmit for the whole refusal window "until the first pass" and
            // then orphan the order anyway. Every other verdict keeps the table itself (the CI box
            // builds exactly the gate it always did). `restore_venue_gate` argues why this is not
            // `restore_orders_off`.
            restore_venue_gate: reconcile_config::restore_venue_gate(reconcile_gate),
            // The read-back a pass asks for (`config.reconcile_lookback_ms`, resolved as the passes resolve it) sizes the
            // trade-id window each engine's journal `Snapshot` keeps (`vike_execution::Engine::size_fill_window`),
            // whatever the reconcile verdict: a pass turned on at the next boot reads the same journal.
            reconcile_lookback_ms: Some(reconcile_config::lookback_ms(
                reconcile_settings.lookback_ms,
            )),
            // A wired market whose account is at tier `paper` runs a paper book in this process's
            // memory, which a restart empties: its
            // restored orders are reported gone at boot, since no reconcile pass ever judges them
            // (decision 0127). The core reads which engines are paper from `Engine::mode`,
            // which the mount sets per venue.
            restored_paper_orders_gone: true,
            // THE FAST IN-FLIGHT CONFIRM (`config.inflight_confirm_ms`, off by default): re-queries an
            // order stuck submitted-but-unacked on this cadence, reject-free — it asks the venue and
            // the venue's own answer folds back, so it NEVER synthesizes a terminal — in the band
            // `[inflight_confirm, submit_ack_timeout)` the reject ladder above deliberately ignores.
            // `None` (no row, or `0`) arms no timer and is byte-identical to this daemon before the
            // row: the first root wiring of the field. It costs extra reject-free REST reads per
            // stuck order: a cold-path sweep at the drain-loop boundary whose only action is a
            // fire-and-forget re-query onto the venue adapter's own thread, never the per-message
            // fold. ⚠ At or above the effective `submit_ack_timeout` it can never act —
            // `inflight_confirm_dead_band` warns below.
            inflight_confirm: rows.inflight_confirm,
            // THE PERIODIC STRATEGY-STATE SAVE (`config.strategy_state_save_ms`, off by default):
            // every stateful mount's durable state (a breaker's trip count, an A-S accumulator) is
            // re-saved to its sidecar under `state_dir` above on this cadence, instead of only at a
            // clean shutdown — so a crash no longer loses it. The core arms a timer on its existing
            // wheel at the drain-loop boundary; `None` (no row, or `0`) arms nothing and is
            // byte-identical to this daemon before the row. Each save is the shutdown save's atomic
            // fsynced write, on the core thread: cold path, never the per-message fold.
            state_save: rows.state_save,
            ..vike_core::CoreConfig::default()
        },
    };

    // ⚠ THE `[guards]`/`[sinks]` WIRING. Applied HERE — after the daemon's own defaults, before
    // `assemble_runtime` — because the profile is the auditable authority and the literals above are the
    // fallback. Five guards (`submit_ack_timeout_ms`, `submit_ack_confirm_grace_ms`,
    // `max_drawdown`, `conditionals_on_ticks`, `margin_call`) plus `sinks.equity_sample_ms` map
    // 1:1 onto `CoreConfig` fields that already existed; the converters already returned the right
    // types. Nothing called them. From #816 (which wired `[risk]`) until now, every one of those
    // keys was parsed, VALIDATED, reported as ignored by a `warn!` and then dropped — a settings
    // key that nothing reads, which this workspace deleted `Policy::max_total_exposure` for. The
    // sixth guard, `initial_trading_state`, is a field of no `CoreConfig` (the state lives on the
    // engine): it is SENT to the spawned core below, once `assemble_runtime` has returned.
    //
    // ⚠ The return's `confirm_grace` is the stage-1/stage-2 pairing evaluated
    // on the FINAL numbers — this daemon's own default above (`DEFAULT_SUBMIT_ACK_TIMEOUT`), or
    // whatever the profile put in its place, against the grace the profile did or did not name. A
    // profile that RAISES the timeout and says nothing about the grace walks the untouched grace
    // under HARD LOWER BOUND (a) in silence, and nothing below this line could ever notice: an
    // undersized grace is not an error, it is a narrower margin, and it surfaces as a phantom
    // OrderRejected against an order the venue actually holds. It WARNS rather than refusing (see
    // `vike_tradehub_core::ConfirmGraceHazard`, which argues that against
    // `docs/decisions/0013-degrade-vs-refuse.md`), and the sentence is rendered by the type so no
    // caller can drift from the arithmetic — and this daemon is the only one that applies a run
    // profile's guards (it named "this daemon and `vike-app`" until 2026-09-28).
    if let Some(p) = &profile {
        let vike_tradehub_core::GuardsReport { confirm_grace } =
            vike_tradehub_core::apply_guards_and_sinks(p, &mut runtime_config.core_config);
        if let Some(hazard) = confirm_grace {
            tracing::warn!("{hazard}");
        }
    }
    // The fast in-flight confirm against the timeout the profile may just have changed: read AFTER
    // `apply_guards_and_sinks`, because the effective `submit_ack_timeout` is the daemon's default
    // or the profile's `[guards]` value, and only here is it known.
    if let Some(why) = inflight_confirm_dead_band(
        runtime_config.core_config.inflight_confirm,
        runtime_config.core_config.submit_ack_timeout,
    ) {
        tracing::warn!("{why}");
    }
    // The trading state each engine the journal restores comes back in — read HERE, because
    // `apply_to` below consumes the restore, and needed after the core is up: a profile's
    // `guards.initial_trading_state` is never sent over a STRICTER state the journal restored
    // (`vike_tradehub_core::send_initial_trading_state`).
    let restored_trading_states =
        vike_tradehub_core::restored_trading_states(boot_restore.restored_engines());
    // The symbols those engines trade beyond their own (every admitted one, held or flat), for the
    // boot re-admission below (`apply_to` consumes the restore).
    let readmit_targets = crate::readmit::targets(boot_restore.restored_engines());
    // …and the legs of the combos they accepted, which the gatewayhub is told below (the engines restore them).
    let combo_legs = crate::readmit::combo_legs(boot_restore.restored_engines());
    // …and the default engines among them that hold a position or an order in their OWN symbol, which that pass
    // never lists and nothing but a strategy mount prices (decision 0197).
    let own_holds = crate::readmit::own_hold_targets(boot_restore.restored_engines());
    // What the journaled engines HOLD (a position, a resting order), read for the same reason: an engine the
    // boot does not rebuild leaves it at the venue with nothing watching (`flag_unmanaged_exposure`).
    let restored_holds = crate::readmit::restored_exposure(boot_restore.restored_engines());
    // THE BOOT RESTORE (decision 0127): the order client id session, the books, each engine's account state
    // and the resting-order candidates the journal gives back — through the one function the paper
    // arm uses too, and after the `[guards]` above, which it does not touch (no journaled limit is
    // ever applied).
    boot_restore.apply_to(&mut runtime_config.core_config);
    runtime_config.core_config.order_client_id_past_sessions = order_client_id_past_sessions;
    runtime_config.core_config.order_client_id_session_recorder = order_client_id_session_recorder;

    // Fold the resolved strategies into `core_config.strategy` + `extra_mounts`, then build the
    // wired-market runtime. Per-venue exec is credential-gated inside `assemble_runtime` (absent creds ⇒ that
    // venue is paper), and `spawn_core_multi` wires applied-fill capture on each mount's own venue
    // engine. `reconcile_clients`/`reconcile_trigger` are captured (the reference mount needs them).
    //
    // ⚠ `build_live_multi_strategy_core` with ONE mount IS `build_live_strategy_core` (first mount
    // → `strategy`, an empty rest → `extra_mounts`), so the single-mount daemon builds the same
    // node it always did. `configs` is cloned out first — the feed wiring below still needs each
    // mount's spec after the specs move into the core.
    let configs: Vec<MountPlacement> = mounts.iter().map(|m| m.spec.clone()).collect();
    // …and the bar series of each mount's declared LEGS (its strategy's `symbols`, at the mount's interval):
    // the bar keeper opens them on the venue's dial below, so a two-leg strategy hears both legs' closes.
    let leg_bars: Vec<vike_execution::SeriesKey> = mounts
        .iter()
        .flat_map(|m| {
            m.spec.legs.iter().map(|leg| {
                let venue = leg.venue.clone().unwrap_or_else(|| m.spec.venue.clone());
                (venue, leg.symbol.clone(), m.spec.interval.clone())
            })
        })
        .collect();
    // …and so is the ACCOUNT each mount trades (a mount naming none trades the default account): the
    // feed arms' exec badge looks that account's route key up in `live_venues` (decision 0172).
    let mount_accounts: Vec<(String, AccountLabel)> = mounts
        .iter()
        .map(|m| (m.spec.venue.clone(), m.spec.account.clone().unwrap_or(AccountLabel::Default)))
        .collect();
    // A `.rs` file's sandboxed mount is re-armed at its ACCOUNT's own fee rate, as the in-process
    // maker was: it opened on the venue's static fee floor (`crate::config::SandboxOpen::maker_round_trip_fee`),
    // and its handle's re-arm (`RemoteHandle::fee_rearm`) rebuilds it in a fresh child at the rate the
    // account's engine resolved, at that mount's index. A `.rhai` mount is not re-armed. A `.rs` mount still
    // BUILDING is parked by the core (`CoreConfig::mounts_awaiting_strategy`), and its re-arm only keeps the
    // rate, for the re-arm right after its child opens (`SandboxBuild::fee_capture`, `super::awaited_builds`).
    runtime_config.core_config.mounts_awaiting_strategy =
        super::awaited_builds::awaiting_ids(&mounts);
    let mut specs = Vec::with_capacity(mounts.len());
    let mut rearms: Vec<Option<vike_tradehub_core::FeeRearm>> = Vec::with_capacity(mounts.len());
    for m in mounts {
        rearms.push(match &m.building {
            Some(build) => Some(build.fee_capture()),
            None => {
                m.remote.as_ref().map(vike_tradehub_core::remote_strategy::RemoteHandle::fee_rearm)
            }
        });
        specs.push(vike_tradehub_core::MountBuild { strategy: m.strategy, spec: m.spec });
    }
    let vike_tradehub_core::RuntimeAssembly {
        handle,
        reconcile_clients,
        reconcile_trigger,
        live_venues,
        forwarder_stop,
        not_running,
        hot,
    } = vike_tradehub_core::build_live_multi_strategy_core_armed(specs, rearms, runtime_config)
        .map_err(|e| LiveStartFailed::assembly(&e))?
        .runtime;
    // The core is up: whatever re-teaches resting orders after a gatewayhub restart reads its snapshot.
    ctor.core_up(handle.snapshot_cell());
    // A mount INPUT in a lane (a user's CSV import), which nothing streams, is read again from the datahub on
    // a cold timer and handed to the core as a bar seed under its key (decision 0271's live half). The thread
    // ends with the core (a refused seed) or the keep; with no lane input it reads nothing.
    super::inputs::spawn_datahub_lane_refresher(&input_keep, &handle);
    // The accounts that do not run, as the wire lists them: and a journaled engine that holds a position or a
    // resting order but was not rebuilt (no row derives it now) is said at ERROR and listed with what it holds.
    let mut not_running_views: Vec<vike_execution::ArmingNotRunningView> =
        not_running.iter().map(vike_tradehub_core::ArmingNotRunning::view).collect();
    crate::readmit::flag_unmanaged_exposure(
        &restored_holds,
        &hot.running_route_keys(),
        &mut not_running_views,
    );

    // A LEASE ANOTHER TRADE CLIENT HOLDS refuses the whole live mount — the old lock's `AddrInUse`,
    // kept (two live processes on one venue account is the accident the lock exists to refuse). It
    // is read HERE, with the core up and nothing wired to it yet: no feed, no strategy tick, no
    // order has been sent, so tearing the core down is the same as not having started it. The
    // forwarder is quiesced first (the order the teardown always keeps: a live pump can otherwise
    // wedge the core join).
    let refusals = ctor.lease_refusals();
    if !refusals.is_empty() {
        crate::feed_gate::tear_down_failed_start(handle, Vec::new(), &forwarder_stop);
        return Err(format!("refusing LIVE mount: {}", refusals.join("; ")).into());
    }

    // ...and RECORD what each venue was ASKED to be and what it BECAME. The `venue_mounted`
    // channel's writer, called HERE and not from `main` for exactly the reason the table is read
    // here: `arming` was computed over the POST-withhold policy. A `data_only` venue's exec
    // credentials are gone from it by now, so journalling from `main` — where `live_venues` is
    // conveniently in scope — would predict against the PRE-withhold policy and record a venue this daemon
    // deliberately keeps on paper as one that was refused for a credential reason.
    //
    // Placed AFTER the node is built because the whole value of the record is the pairing: the
    // prediction (`vike_tradehub_core::venue_arming`, which the arming screen renders) against the OUTCOME
    // (`live_venues`). Before this line the second half does not exist.
    //
    // Failures WARN and continue. A journal that cannot be written must never stop a daemon that
    // has already mounted its venues — the ledger is evidence, not a gate.
    for r in vike_tradehub_core::journal_venue_mounts(
        Some(state_dir),
        &arming,
        &live_venues,
        env!("CARGO_PKG_VERSION"),
        vike_model::now_ms(),
    ) {
        if let Err(e) = r {
            tracing::warn!(error = %e, "venue mount record not journalled");
        }
    }

    // THE PROFILE'S STARTING TRADING STATE (`guards.initial_trading_state`): sent to the spawned core
    // as `Command::SetTradingState` — AFTER the core spawned, BEFORE a resurrected mount or a
    // feed can reach it (the ingest lane is FIFO and this is the lossless sender, so it folds
    // first). `active` — the default — sends nothing, and a stricter state the journal restored
    // is never loosened (`vike_tradehub_core::send_initial_trading_state` decides and says which). The
    // paper arm (`start_paper`) makes the same call from the same function.
    if let Some(p) = &profile {
        vike_tradehub_core::send_initial_trading_state(p, &restored_trading_states, |c| {
            handle.send_command(c)
        });
    }

    // Resurrect RUNTIME strategy mounts (B5 residual closed): replay the topology sidecar through
    // the SAME lossless command lane a wire MountStrategy is lowered into — AFTER the core
    // spawned, BEFORE `wire_venue_feeds` below arms a single feed. The ingest lane is FIFO, so
    // every resurrected mount (and its `<mount_id>.json` state load, part of the mount arm) folds
    // ahead of the first market message — `resurrect_runtime_mounts`'s ordering contract. Nothing
    // here can fail the mount: a stale record skips with a warn, a corrupt file reads empty.
    if let Some(dir) = strategy_state_dir() {
        let outcome =
            crate::mount_factory::resurrect_runtime_mounts(&dir, |c| handle.send_command(c));
        if outcome.sent + outcome.skipped > 0 {
            tracing::info!(
                sent = outcome.sent,
                skipped = outcome.skipped,
                "runtime-mount resurrect replayed the topology sidecar"
            );
        }
    }

    // Carry the reconcile ingredients across the feed build ONLY when the S2 gate said yes — the
    // `spawn_reconcile` mount runs AFTER the feeds so a feed-subscribe error returns BEFORE any
    // `th-core-recon` thread is spawned (no orphaned driver on the error path). OFF (a paper mount): the
    // handles are moved into the tuple and DROPPED right here — matching the pre-reconcile daemon's
    // `..`-destructure drop timing exactly (`reconcile_trigger` is already `None`, since `assemble_runtime` gates
    // the channel on the same flag) — so no ingredients survive to mount a driver. This is the ONLY
    // unconditional new binding, and its OFF-path effect is a byte-identical early drop.
    let reconcile_ingredients = reconcile_enabled.then_some((reconcile_clients, reconcile_trigger));

    // ⚠ The daemon no longer records its own feed (0084: the store has ONE writer plane, and the
    // recorder that survives runs inside the datahub with its watchdog, silence detection and
    // record profiles — none of which the removed `record-feeds` tee had). `post_feeds` stays as
    // the teardown slot so the sequential TAIL below keeps its shape; it is always `None` now.
    let post_feeds: StrategyLanesPost = None;

    // Identity: nothing is teed off the venue's base sink any more.
    let wrap = |base: Arc<dyn DataClientSink>| -> Arc<dyn DataClientSink> { base };

    // Wire the venue's live market feed onto the core's ingest lanes.
    // Wire each mounted venue's live market feed onto the core's ingest lanes — ONE feed arm per
    // distinct venue, however many mounts share it (subscriptions dedup per series INSIDE the
    // arm). The arms live in `wire_venue_feeds`, directly above `live_runtime`, where
    // `cex_feed_wiring_pin.rs` scans them.
    let mut feeds: Vec<StrategyLanesLive> = Vec::with_capacity(venue_plans.len());
    // ...none at all when the datahub's strategy lane is away (`feed_down` above: the accounts that needed
    // a price are NOT RUNNING and the late arm waits). A feed that REFUSES here — the gatewayhub's
    // credentialed-stream refusal, an interval a bridge cannot serve, or a datahub that went away between
    // the probe and now — ends the start, as it always did, but it ends it the way the lease refusal above
    // does: the feeds already wired are released and the core is told to stop (journal flushed), instead of
    // the process exiting under a live core thread.
    //
    // The start decides on the refusal's CLASS (`crate::feeds::FeedRefusal`, a `match` in `crate::feed_gate`), never on
    // its words.
    //
    // ⚠ A NOT NOW refusal (the datahub's gatewayhub link is away while the datahub itself passed the probe: a
    // link that is down, an ack that never came) is survived where it honestly can be (`survivable_refusal`: no
    // `demo`/`live` account of the venue RUNS, so nothing is traded on a missing price): that venue's feed is
    // left unwired (the partial one was shut down in order when the arm dropped it), its `paper` accounts run
    // without prices, and the late arm waits and restarts the tradehub armed. A venue with a running live account
    // cannot be held once the core is up, and ends the start as before.
    //
    // ⚠ A CAP refusal (`FeedRefusalClass::Cap`: a pump, session or stream cap that is full) is the third class: it persists until
    // an OPERATOR raises the cap or drops a subscription, so no restart can mend it and none is ordered. Where a
    // transient would be survived (no `demo`/`live` account of the venue runs) the venue stays UP and priceless
    // (`feeds_capped`: its feed is unwired, `paper` and every other venue run, the banner names the cap, ONE `error`
    // line says what to change, and the late arm is not told to wait for it). A venue with a running live account
    // cannot be held after the core exists; that start ends with `failed_start_code`'s status, which the unit does not
    // restart on (a plain failure exit would loop the whole process every `RestartSec=`).
    let mut feeds_held_back: Vec<(String, String)> = Vec::new();
    let mut feeds_capped: Vec<(String, String)> = Vec::new();
    let mut watchers: HashMap<String, crate::feeds::StrategyLaneCexWatcher> = HashMap::new();
    // The network Hyperliquid's price feed was wired for (a mount's or the price-only one): the account
    // follower refuses to hot-start a `live` account beside a test-network feed (`hyperliquid_feed_refusal`).
    let mut hl_feed_network: Option<vike_model::VenueNetwork> = None;
    for (venue, plan) in venue_plans.iter().filter(|_| feed_down.is_none()) {
        let venue_configs: Vec<&MountPlacement> =
            configs.iter().filter(|c| &c.venue == venue).collect();
        let venue_accounts: Vec<AccountLabel> = mount_accounts
            .iter()
            .filter(|(mounted, _)| mounted == venue)
            .map(|(_, account)| account.clone())
            .collect();
        match wire_venue_feeds(
            plan,
            &venue_configs,
            &handle,
            &live_venues,
            &arming,
            &venue_accounts,
            &data_only,
            &wrap,
            make,
        ) {
            Ok(wired) => {
                if let Some(watcher) = wired.price_watcher() {
                    watchers.insert(venue.clone(), watcher);
                }
                if let StrategyLanePlan::Hyperliquid(network) = plan {
                    hl_feed_network = Some(*network);
                }
                feeds.push(wired);
            }
            Err(refusal) if crate::feed_gate::survivable_refusal(&refusal, venue, &live_venues) => {
                tracing::error!(
                    venue = %venue,
                    reason = %refusal,
                    "{}: {venue}'s price feed was refused as TRANSIENT (the datahub answered the probe, so \
                     its own link to the gatewayhub, or a budget, is the cause) and no `demo`/`live` \
                     account of {venue} runs: the feed is left unwired, `paper` accounts on {venue} run \
                     without prices, and the late arm restarts this tradehub armed once the price path \
                     answers",
                    crate::feed_gate::PRICE_FEEDS_UNAVAILABLE
                );
                feeds_held_back.push((venue.clone(), refusal.text));
            }
            Err(refusal) if crate::feed_gate::survivable_cap(&refusal, venue, &live_venues) => {
                tracing::error!(
                    venue = %venue,
                    reason = %refusal,
                    "{} (CAP): {venue}'s price feed was refused because {}, and no `demo`/`live` account of \
                     {venue} runs: the feed is left unwired, `paper` accounts on {venue} (and every other \
                     venue) keep running, and NO restart is ordered, because a restart meets the same cap. An \
                     operator must raise the cap named in the refusal or drop a subscription that holds a \
                     place, then restart this tradehub",
                    crate::feed_gate::PRICE_FEEDS_UNAVAILABLE,
                    crate::feeds::FEEDS_CAPPED_MARK
                );
                feeds_capped.push((venue.clone(), refusal.text));
            }
            Err(refusal) => {
                crate::feed_gate::tear_down_failed_start(handle, feeds, &forwarder_stop);
                return Err(refusal.into());
            }
        }
    }
    // THE PRICE-ONLY FEEDS of the admitting venues no mount names. The control door prices a symbol it
    // admits through its venue's watcher, and the watchers above exist only for MOUNTED venues, so a box that
    // mounts bybit alone refused every foreign binance, okx, aster, hyperliquid or deribit symbol (`this node runs no
    // price feed for binance`). The core routes a venue's quote, trade and book to that venue's engine without
    // a mount, so a dial with nothing subscribed is all the door needs. Each venue is asked of the datahub
    // first (`datahub_probe`: is a strategy lane advertised for it?), so a datahub that serves no lane for a
    // venue costs no dial that would be refused for ever. A refusal is said and costs only that venue's foreign
    // symbols (the door fails closed); it never ends the start. The link dead-man hears these venues too
    // (`price_only_set` above), acting only while one holds an admitted symbol or a live order.
    if feed_down.is_none() {
        let price_only = crate::feeds::wire_price_only_feeds(
            &admitting,
            &mounted_venues,
            hl_price_network,
            &handle,
            &wrap,
            make,
        );
        watchers.extend(price_only.watchers);
        if price_only.hl_network.is_some() {
            hl_feed_network = price_only.hl_network;
        }
        feeds.extend(price_only.feeds);
    }
    // THE PRICE FEEDS OF ADMITTED SYMBOLS AND THE BOOT RE-ADMISSION (decisions 0173 and 0174).
    //
    // The control door subscribes a newly admitted symbol's tick pump on its venue's feed dial, so
    // each CEX feed hands the lease registry a watcher (`TradeLeaseRegistry::bind_prices`); a venue with no
    // watcher answers a price ask with the reason. Then the journal's restored positions and orders
    // in symbols the engines were not started on are re-admitted with fresh facts, and their
    // prices subscribed. It runs AFTER the feeds are up (the subscription needs the dial) and BEFORE
    // the reconcile driver is spawned below: the command lane is FIFO, so every admission folds
    // ahead of the first `ReconcileReports`. A symbol the facts source cannot answer is said at
    // `error` and left as the restore put it (the journal's own admission, until an order on it
    // admits it again).
    let symbol_facts = ctor.symbol_facts();
    if let Some(facts) = &symbol_facts {
        facts.bind_prices(watchers);
        if let Some(links) = make.price_links() {
            facts.bind_price_links(links);
        }
        facts.spawn_idle_reaper();
        // The lane twins THIS boot built an engine for: their own symbol is on no mount, so the pass below
        // (which prices what the journal admitted) never opens its lane (`warm_twins`). Read from the
        // boot's account rows; the exposure-cap closure bound above took its own clone of them.
        let twin_warm = crate::readmit::twin_targets(&account_rows, &live_venues);
        // The restored own-symbol holds of the engines that came up (a lease to price over): their lanes are
        // opened below like the twins', and the lane keeper after the boot keeps and releases them.
        let own_warm: Vec<crate::readmit::ReadmitTarget> =
            own_holds.into_iter().filter(|t| live_venues.contains(&t.route_key)).collect();
        // The conversion rate of a held foreign-quoted position (decision 0183, phase 1): re-read about once a
        // minute and re-admitted with no order to carry it, because the engine calls a rate stale at 120 s and
        // then denies opening orders. A symbol the account holds no position in is left to the order path, which
        // re-reads the rate before the order is judged. Nothing happens on a box that admits no such symbol.
        {
            // How old the engine's rate for a currency is, as the core publishes it: what lets the timer name a
            // held position whose rate nothing is refreshing (`TradeLeaseRegistry::unrefreshed_held`). Read off the
            // NEWEST block of the key: the core keeps a stopped account's engine, tombstone first.
            let rows_cell = handle.snapshot_cell();
            facts.bind_rate_rows(move |route_key, currency| {
                crate::readmit::engine_rate_stamp(
                    &rows_cell.load().portfolio.venues,
                    route_key,
                    currency,
                )
            });
            let cell = handle.snapshot_cell();
            let sink = handle.command_sink();
            facts.spawn_rate_refresher(
                crate::venue_backend::gateway::trade_lease_registry::RATE_REFRESH_EVERY,
                move |route_key, symbol| {
                    crate::readmit::engine_holds_position(
                        &cell.load().portfolio.venues,
                        route_key,
                        symbol,
                    )
                },
                move |c| {
                    let _ = sink.send_blocking(c);
                },
            );
            // THE RATE OF AN ENGINE'S OWN MOUNTED SYMBOL (decision 0183, the mounted-symbol amendment): a ledger
            // in the account's money over a mount quoted in another one (a EUR account on `EURUSD`) books the mount
            // through a rate, declared at assembly with none. This timer rates it a tick after its start and keeps
            // it rated about every `MOUNT_RATE_EVERY`, position or not (a strategy mounted on it never meets the
            // door). A box with no such account notes no mount, and the thread finds nothing due.
            let sink = handle.command_sink();
            facts.spawn_mount_rater(
                crate::venue_backend::gateway::trade_lease_registry::MOUNT_RATE_TICK,
                move |c| {
                    let _ = sink.send_blocking(c);
                },
            );
            // THE CONTRACT SIZE OF A PAPER IG ACCOUNT'S OWN EPIC (the owner's ruling of 2026-11-16): read over ig's
            // demo login and stated to the paper engine, whose book refuses the epic until then
            // (`TradeLeaseRegistry::state_paper_units`). A box with no such account states nothing.
            let cell = handle.snapshot_cell();
            let sink = handle.command_sink();
            facts.spawn_paper_unit_stater(
                crate::venue_backend::gateway::trade_lease_registry::MOUNT_RATE_TICK,
                move || cell.load().portfolio.venues.clone(),
                move |c| {
                    let _ = sink.send_blocking(c);
                },
            );
        }
        // THE PRICE LANE OF AN ENGINE'S OWN SYMBOL (decision 0197): a default `demo`/`live` engine that holds a
        // position or a live order in the symbol it was started on has that symbol's lane kept open, and handed
        // back to the idle reaper when it is flat, read off the same published snapshot as the rate timer's
        // `held`. A symbol a strategy mount of THIS BOOT prices already is left to the mount: the gatewayhub allows
        // one tick pump per symbol per session. A runtime `mount` (or one resurrected from the topology sidecar)
        // adds a slot on an existing engine and starts no feed of its own, so the same keeper holds ITS engine's
        // tick lane for as long as the mount is published (decision 0245: no mount without ticks).
        {
            let mounted_at_boot: std::collections::HashSet<(String, String)> =
                configs.iter().map(|c| (c.venue.clone(), c.symbol.clone())).collect();
            facts.bind_mounted(move |venue, symbol| {
                mounted_at_boot.contains(&(venue.to_string(), symbol.to_string()))
            });
            // A PAPER account holds no trade lease, so a symbol it is asked to trade (a polymarket paper account's
            // token) is admitted on the facts the datahub states for it (decision 0285's amendment of 2026-11-16).
            let accounts = handle.snapshot_cell();
            facts.bind_keyless_facts(move |route_key, venue, symbol| {
                let paper = accounts.load().portfolio.venues.iter().any(|block| {
                    block.route_key == route_key
                        && block.tier == Some(vike_model::AccountTier::Paper)
                });
                paper.then(|| datahub_symbol_facts(venue, symbol))
            });
            // A mount on a LABELLED account (boot or runtime) is priced by the tap's mark, which only a lane twin or
            // a held lane switched on until now (decision 0245): the keeper tells the registry which
            // `(venue, symbol)` pairs those are, each tick, and it switches the tap to exactly them. Only a `Weak`
            // is captured, so the registry still ends when its owner lets go.
            let cell = handle.snapshot_cell();
            let marks = Arc::downgrade(facts);
            facts.spawn_lane_keeper(
                crate::venue_backend::gateway::trade_lease_registry::LANE_KEEP_EVERY,
                move || {
                    let snapshot = cell.load();
                    // A paper account follows its venue's market clock: nothing is held for it while that market is
                    // closed, and this tick's wall clock says when it opens (`readmit::paper_waits_for_open`).
                    let now_ms = vike_model::now_ms();
                    if let Some(facts) = marks.upgrade() {
                        facts.sync_mount_marks(crate::readmit::mount_mark_pairs(
                            &snapshot.mounts,
                            &snapshot.portfolio.venues,
                        ));
                        // A paper account of a venue priced through its demo login (dukascopy, fxcm, ibkr: the
                        // owner's ruling of 2026-11-16): its quotes fold into its book's bars, or it says why not.
                        facts.sync_paper_quotes(
                            &crate::readmit::paper_quote_engines(&snapshot.portfolio.venues),
                            now_ms,
                        );
                    }
                    let mut wanted = crate::readmit::own_symbol_holds(
                        &snapshot.portfolio.venues,
                        &snapshot.orders,
                        now_ms,
                    );
                    // A live mount's tick lane, and the lane a HELD order's release reads another venue's price on
                    // (decision 0267's 2.13): `HOLD_GRACE` gives it back once the order leaves the HELD list.
                    for target in crate::readmit::mount_lane_holds(
                        &snapshot.mounts,
                        &snapshot.portfolio.venues,
                    )
                    .into_iter()
                    .chain(crate::readmit::release_price_holds(
                        &snapshot.held_exits,
                        &snapshot.portfolio.venues,
                    )) {
                        if !wanted.contains(&target) {
                            wanted.push(target);
                        }
                    }
                    wanted
                },
            );
            // …and its BAR lane: a series nobody subscribed was never heard, so the keeper opens every published
            // mount's series that the boot did not (once each; the feed's teardown releases it).
            facts.bind_boot_bars(
                configs
                    .iter()
                    .map(|c| (c.venue.clone(), c.symbol.clone(), c.interval.clone()))
                    .collect(),
            );
            // …and so does every venue series a mount's INPUT reads (decision 0271's live half): its closes
            // reach the core through the same sink, filed into the inputs alone (`vike_core`'s `input_only`).
            let cell = handle.snapshot_cell();
            let keep = std::sync::Arc::clone(&input_keep);
            facts.spawn_bar_keeper(
                crate::venue_backend::gateway::trade_lease_registry::LANE_KEEP_EVERY,
                move || {
                    let mut wanted = crate::readmit::mount_bar_series(&cell.load().mounts);
                    for series in leg_bars.iter().cloned().chain(keep.venue_series()) {
                        if !wanted.contains(&series) {
                            wanted.push(series);
                        }
                    }
                    wanted
                },
            );
            // THE PRICE PUMP OF AN ADMITTED PAIR on a venue whose lane is one permanent poll (fxcm, A39): given
            // back once every account that admitted it has held nothing in it for `ADMITTED_IDLE_GRACE`, and
            // opened again when one holds something. A tick with no published engine reaps nothing.
            let cell = handle.snapshot_cell();
            facts.spawn_admitted_reaper(
                crate::venue_backend::gateway::trade_lease_registry::LANE_KEEP_EVERY,
                move || {
                    let snapshot = cell.load();
                    (!snapshot.portfolio.venues.is_empty()).then(|| {
                        crate::readmit::symbol_holdings(
                            &snapshot.portfolio.venues,
                            &snapshot.orders,
                        )
                    })
                },
            );
        }
        // (The exposure ceiling's answer was bound before the engines were built, above.) The boot window: every gatewayhub round trip below waits at most the boot bound
        // (`config.gatewayhub_connect_wait_ms`), and a lease that misses it is not asked again, so one
        // silent gatewayhub cannot hold `ready`. A symbol whose answer missed is NOT admitted (the
        // journal's grid and its restored position/orders stay as the restore put them).
        let boot_window = facts.begin_boot();
        // Each symbol is a round trip, so the pass reports `readmitting <venue> <k>/<n>` to the start's
        // `starting` stream and ends on its own budget (`READMIT_BUDGET`): what it did not reach is
        // `missed`, and the retry thread below admits it.
        let start_progress = ctor.start_progress();
        let report = |text: &str| {
            if let Some(progress) = &start_progress {
                progress.phase(text);
            }
        };
        let pace = crate::readmit::ReadmitPace::new(crate::readmit::READMIT_BUDGET, &report);
        let (done, missed) =
            crate::readmit::readmit_and_price(&readmit_targets, facts.as_ref(), &pace, |c| {
                handle.send_command(c);
            });
        // The restored combo legs, inside the same window and before the reconcile driver: the commit alone.
        for (leg, live_combos) in &combo_legs {
            facts.tell_combo_legs(&leg.route_key, live_combos, std::slice::from_ref(&leg.symbol));
        }
        drop(boot_window);
        // AFTER the pass and its window, so the warm-up's lease round trips share neither the window's bound
        // nor its `missed` set with the restored symbols. Not waited for (`warm_twins`): a lane that never
        // opens holds nothing.
        drop(crate::readmit::warm_twins(&twin_warm, &pace, {
            let facts = Arc::clone(facts);
            move |t: &crate::readmit::ReadmitTarget| {
                facts.warm_price(&t.route_key, &t.venue, &t.symbol)
            }
        }));
        // …and the same for what the restored engines hold in their OWN symbol (decision 0197): not waited for
        // either, and the lane keeper started above keeps or releases the lane from here on.
        drop(crate::readmit::warm_twins(&own_warm, &pace, {
            let facts = Arc::clone(facts);
            move |t: &crate::readmit::ReadmitTarget| {
                facts.hold_price(&t.route_key, &t.venue, &t.symbol)
            }
        }));
        // Move the phase off the last `pricing ...` step, as the heartbeat repeats the current one.
        if let Some(progress) = &start_progress {
            if !readmit_targets.is_empty() {
                progress.phase("re-admitted the restored symbols");
            } else if !twin_warm.is_empty() || !own_warm.is_empty() {
                progress.phase("opening the price lanes of what the engines hold");
            }
        }
        // A symbol the boot missed (a slow gatewayhub, a commit that timed out) is asked again after the
        // boot window, at the runtime bound, so a transient miss heals without a restart; until it does,
        // the restored orders on it are HELD and the line below says so.
        if !missed.is_empty() {
            crate::readmit::say_missed(&missed);
            let sink = handle.command_sink();
            if let Err(e) = crate::readmit::spawn_retry(
                missed,
                facts,
                Arc::clone(&forwarder_stop),
                &crate::readmit::RETRY_PAUSES,
                move |c| {
                    let _ = sink.send_blocking(c);
                },
            ) {
                tracing::error!(error = %e, "boot: the re-admission retry could not start; the missed symbols are admitted by an order on them or by a restart");
            }
        }
        if !readmit_targets.is_empty() {
            tracing::info!(
                wanted = readmit_targets.len(),
                sent = done.sent,
                failed = done.failed.len(),
                "boot: re-admitted the symbols the restored engines hold beyond their own"
            );
        }
    }
    // Who waits for the gatewayhub (`NOT RUNNING` with `gatewayhub unavailable`) or for the datahub (the price feeds
    // above, held before the core or refused as a transient after it): the late arm's plan. Decided after the feeds
    // because a transient refusal at wiring time adds the datahub to the wait. It adds the GATEWAYHUB only when a
    // `demo`/`live` account of a held-back venue exists: that account arms through the gatewayhub once its feed is
    // back, so a restart that finds the gatewayhub away would arm nothing. A box with no such account (every
    // account `paper`) may have no gatewayhub key at all, so its own link to the gatewayhub can never answer;
    // waiting on it would leave the late arm unable to fire for ever, on exactly the box the survivable refusal
    // serves most often.
    let feeds_why: Option<crate::feeds::FeedsDown> =
        feed_down.clone().or_else(|| feeds_held_back.first().map(|(_, why)| why.clone().into()));
    let mut late_arm =
        ctor.late_arm(&not_running, (feeds_why.is_some()).then(|| datahub.clone()).flatten());
    if let Some(plan) = late_arm.as_mut() {
        plan.feeds_why.clone_from(&feeds_why);
        let held: Vec<&str> = feeds_held_back.iter().map(|(venue, _)| venue.as_str()).collect();
        plan.gateway |= crate::feed_gate::arms_through_gateway(&table, &held);
        // The feed-driven restarts are counted in the project's state directory and budgeted
        // (`late_arm::FEED_RESTARTS_PER_WINDOW`): a cause the late arm's own checks cannot see would
        // otherwise restart the whole process every ~95 s.
        plan.restarts = crate::late_arm::LateArmRestartLedger::in_dir(state_dir);
    }
    // The CAP class carries its cause to the banner and orders nothing: on a plan that exists for other reasons
    // (a gatewayhub wait, a transient feed wait) it only leads the cause; with no plan at all a notice-only plan
    // (`LateArmPlan::notice`, no thread) carries it.
    if let Some((venue, refusal)) = feeds_capped.first() {
        let capped_why = crate::feeds::FeedsDown::cap(format!("{venue}: {refusal}"));
        match late_arm.as_mut() {
            Some(plan) => plan.feeds_why = Some(capped_why),
            None => late_arm = ctor.notice_plan(capped_why),
        }
    }
    let late_arm = late_arm.map(|plan| plan.within_budget(vike_model::now_ms()));

    // Reconcile driver mount — the reference was `vike-app`'s `App::new` reconcile_driver
    // block (gone with the desktop's local core). Placed AFTER the feeds are up so a
    // feed-subscribe error above returns before this ever spawns a thread. `reconcile_ingredients` is
    // `Some` only when `reconcile_enabled` (the OFF path already dropped the handles above) ⇒ this
    // whole mount is byte-identically absent on a PAPER mount and whenever the operator refused it.
    // `spawn_reconcile` owns its own `th-core-recon` thread and respects the single-writer rule (it
    // only blocking-fetches REST reports and enqueues `Command::ReconcileReports` for the fold thread —
    // see that module's doc); it ADOPTS the pre-built `reconcile_trigger` pair so a venue's recon trigger
    // reaches this same driver.
    let reconcile_driver = match reconcile_ingredients {
        None => None,
        // Enabled but every venue is paper (no creds ⇒ no `ReconcileClient`): inert, no thread — mirrors
        // the `reconcile_clients.is_empty()` arm vike-app had. The (empty) clients + `reconcile_trigger`
        // drop here.
        Some((clients, _trigger)) if clients.is_empty() => {
            // ⚠ This arm is REACHABLE now in a way it was not before S2: the armed-live probe is
            // INTENT-based (`vike_tradehub_core::armed_live_venues`' own doc says so), so a venue whose
            // synchronous connect fails or whose factory declines a present-but-bad key probes live,
            // turns the default gate on, and then hands back no client. Saying so plainly beats a
            // silent no-op, because "reconcile is on" and "reconcile reached a venue" are different
            // facts and an operator reading the gate's own disclosure above has only the first.
            tracing::warn!(
                "reconcile is ON but no venue produced a ReconClient (every armed venue fell back \
                 to paper); the driver is inert this session and NOTHING is being reconciled"
            );
            None
        }
        Some((clients, reconcile_trigger)) => {
            // The per-venue feed-status health map, from the feeds THIS mount actually built
            // ([`StrategyLanesLive::reconcile_feed_statuses`]). `assemble_runtime` builds no market feeds of its own
            // (they stay with the caller — see `crates/vike-tradehub-core/src/node.rs`'s module doc), and
            // since wave 2 those feeds are dials to the datahub: the bridge's feed-status handle lives in the
            // gatewayhub, so the map is EMPTY and every reconciled venue reads `Healthy` and is never
            // health-blocked here (`crates/vike-tradehub/CLAUDE.md`'s health-gate bullet).
            //
            // ⚠ The map was UNCONDITIONALLY EMPTY before the CEX arm, and empty is still the right
            // answer for hyperliquid/polymarket — the gate can only ever SUPPRESS a pass, and a
            // wrongly-suppressed pass can stay suppressed, so a row is added per venue on evidence,
            // never by sweeping up whatever handles are in scope.
            let feed_statuses = reconcile_feed_statuses_of(&feeds);
            let mut recon_cfg =
                reconcile_config::build_reconcile_config(&reconcile_settings, feed_statuses);
            // A policy that rewrites positions cannot ride a ring that replays (the W3c double-count
            // finding): overridden with `quarantine` (said at `error`), not refused - a refusal turns a row
            // written while the daemon runs into a restart loop with live orders resting.
            recon_cfg.policy = reconcile_config::quarantine_when_replaying(
                reconcile_enabled,
                ctor.replays_events(),
                recon_cfg.policy,
            );
            tracing::warn!(
                // LEGS, not venues: one row per venue ACCOUNT since the per-account producer
                // landed, and equal to the venue count on every box with no labelled account.
                accounts = clients.len(),
                policy = ?recon_cfg.policy.default,
                "mounting the reconciliation engine (quarantine-first unless config.reconcile_policy \
                 says otherwise); a restarted live daemon re-adopts open venue orders/positions instead of \
                 a blind fresh core. See the reconcile-gate line above for WHY it is on"
            );
            // THE ENGINE TAKES THE VENUE'S BALANCE BEFORE THE DOOR OPENS (decision 0246; COLD, the boot
            // only): `first_balance.rs` mounts the driver and waits, bounded, for every leg's first
            // balance, so the door below opens on engines that already trade on the venue's figure (an
            // account the venue did not answer stays refused for OPENING orders, by name, with the reason).
            let (progress, stop) = (ctor.start_progress(), ctor.stop_flag());
            Some(first_balance::mount_reconcile_driver(
                &handle,
                clients,
                recon_cfg,
                reconcile_trigger,
                symbol_facts.as_deref(),
                &first_balance::BootWait {
                    progress: progress.as_ref(),
                    stop: stop.as_deref(),
                    bound: vike_tradehub_core::FIRST_BALANCE_BOUND,
                },
            ))
        }
    };

    // THE ACCOUNT WATCHER's parts (decision 0169): what starts one more account in this core, the rows to
    // follow, and the facts a start needs that only this mount knows. `None` where nothing follows the
    // rows (an in-process backend, a store that could not be watched): those accounts keep the restart path.
    // A node that booted with reconciliation off ONLY because no account was armed then (not because the operator
    // refused it) can start the driver itself with the first account that has a reconcile client: it is given
    // the maker of the very config the boot would have built.
    let hot = if reconcile_gate == reconcile_config::ReconcileGate::NothingArmedLive {
        let replays = ctor.replays_events();
        let feed_statuses = reconcile_feed_statuses_of(&feeds);
        hot.with_hot_reconcile(vike_tradehub_core::HotAccountReconcile::new(move || {
            let mut config = reconcile_config::build_reconcile_config(
                &reconcile_settings,
                feed_statuses.clone(),
            );
            config.policy =
                reconcile_config::quarantine_when_replaying(true, replays, config.policy);
            config
        }))
    } else {
        hot
    };
    // An account started in the running node is judged on its seed cash until its first reconcile pass lands the
    // venue's balance (decision 0169): each such start tells the control door, which holds the account's opening
    // orders on the flag its leg raises. The boot's engines announce nothing (reconciliation seeds them first).
    let hot = match &symbol_facts {
        Some(facts) => {
            let facts = Arc::clone(facts);
            hot.with_first_balance_notice(move |route_key, synced| {
                facts.engine_awaiting_balance(route_key, synced);
            })
        }
        None => hot,
    };
    let follow = ctor.take_rows().map(|follow_rows| AccountFollowParts {
        hot: hot.with_reconcile_refused(
            reconcile_gate == reconcile_config::ReconcileGate::RefusedByOperator,
        ),
        rows: follow_rows,
        fresh_backend: ctor.fresh_backend(),
        data_only: data_only.clone(),
        // A venue whose price feed is not wired (the datahub was away at start, or refused it) keeps the
        // restart path: an account started on it would trade without prices.
        held_venues: if feed_down.is_some() {
            venue_plans.iter().map(|(v, _)| v.clone()).collect()
        } else {
            feeds_held_back.iter().chain(feeds_capped.iter()).map(|(v, _)| v.clone()).collect()
        },
        hl_feed_network,
        reconcile_legs: reconcile_driver
            .as_ref()
            .map(vike_tradehub_core::ReconcileDriver::leg_sender),
        not_running: not_running_views.clone(),
        live_switch: flags.tradehub_live,
        facts: symbol_facts.clone(),
        policy_cap: account_cap_policy,
    });

    Ok((
        handle,
        StrategyLanesLiveTeardown {
            feeds,
            forwarder_stop,
            post_feeds,
            reconcile_driver,
            symbol_sources: symbol_facts.map(SymbolSources::of).unwrap_or_default(),
            follow,
        },
        live_venues,
        late_arm,
        not_running_views,
    ))
}

/// The startup WARNING for a fast in-flight confirm that can never act, or `None` when it can.
///
/// The core's fast rung acts on an order stuck submitted-but-unacked only while its age is in
/// `[inflight_confirm, submit_ack_timeout)` — at the timeout the reject-capable ladder owns the
/// order. So a cadence at or above the effective timeout leaves that band EMPTY: the row is
/// accepted (the loader cannot see a run profile's timeout), and the daemon says so here instead of
/// confirming a setting that does nothing. `None` for no row, and for no timeout at all (the ladder
/// off): the band is then open-ended and the rung is the only stuck-order signal.
pub(super) fn inflight_confirm_dead_band(
    inflight_confirm: Option<Duration>,
    submit_ack_timeout: Option<Duration>,
) -> Option<String> {
    let (inflight, timeout) = (inflight_confirm?, submit_ack_timeout?);
    (inflight >= timeout).then(|| {
        format!(
            "config.inflight_confirm_ms is {} ms, at or above the effective submit_ack_timeout of \
             {} ms: the fast in-flight confirm acts only on orders younger than that timeout, so \
             it will NEVER act. Lower the row below the timeout, raise the run profile's \
             [guards] submit_ack_timeout_ms, or `vike-cli config set config.inflight_confirm_ms 0` \
             to switch the rung off",
            inflight.as_millis(),
            timeout.as_millis()
        )
    })
}
