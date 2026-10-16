//! **The LIVE mount** — [`live_runtime`] and [`live_runtime_with`], the arm `run` takes when
//! `flags.tradehub_live` is on: the wired-market `assemble_runtime` core over the resolved mount set,
//! the venue BACKEND it is assembled over ([`BackendCtor`]: the gatewayhub in production), the
//! per-venue feed wiring, and the reconcile driver.
//!
//! Cut VERBATIM out of `crates/vike-tradehub/src/tradehub_cli.rs`, whose `//!` block stays the
//! lifecycle contract and whose `use` block this child shares (`use super::*`): the two are one
//! composition root split for size, not two modules with a boundary between them.
//!
//! ⚠ **This root claims no live-account lock any more** (wave 2, W11; decision 0164). The B11
//! sentinel `LIVE-<route key>.lock` — one live process per venue ACCOUNT — is claimed by the
//! gatewayhub, which owns every venue client now: `RemoteBackend::start_adapter` opens the trade
//! lease (the gatewayhub claims the account's lock inside that open) BEFORE it asks for the
//! adapter, and a lease another client holds refuses this whole mount here
//! ([`BackendCtor::lease_refusals`]), as the lock's `AddrInUse` always did. The armed set is still
//! computed here: it decides the reconcile gate.
//!
//! A new parameter goes at the END of either signature: the existing ones keep their places, and
//! `state_dir` is still the directory the `venue_mounted` journal is written in.

use super::*;

/// **How the live mount obtains its venue backend** — the seam production fills with the gatewayhub
/// ([`RemoteCtor`]) and the splice tests with the in-process registry (`LocalCtor`, test-only).
///
/// Called TWICE per mount, with the policy as it stands at each point: once for the feed-plan loop
/// (the full-credential rows: a credentialed-data feed keeps the keys exec loses, so the network it
/// dials is judged on them) and once for the mount itself, after the `data_only` withhold. A remote
/// ctor asks the gatewayhub ONCE and answers both from the same snapshot.
pub(super) trait BackendCtor {
    /// A backend over `policy` (the `account` rows and credentials as they stand) and `vars` (the
    /// deployment map with the resolved flags folded in); `data_only` is the set of venues whose exec
    /// credentials the mount withholds (empty for the plan loop).
    fn backend(
        &self,
        vars: &HashMap<String, String>,
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
        _not_running: &[vike_tradehub_core::NotRunning],
        _datahub: Option<crate::feeds::DatahubProbe>,
    ) -> Option<crate::late_arm::LateArmPlan> {
        None
    }

    /// A plan that waits for NOTHING and only carries `why` to the ready banner (a price feed refused by a CAP: no
    /// restart mends it, so none is ordered, yet the process must say it has no prices). `None` for a backend
    /// with no gatewayhub link to name.
    fn notice_plan(&self, _why: String) -> Option<crate::late_arm::LateArmPlan> {
        None
    }

    /// Whether this backend's events arrive over a replaying ring (the gatewayhub's per-account event
    /// ring, replayed to a resuming lease). `true` makes the start override a reconcile policy that rewrites
    /// positions with `quarantine` ([`reconcile_config::quarantine_when_replaying`]). `false` for an in-process backend.
    fn replays_events(&self) -> bool {
        false
    }

    /// The core is up: its published snapshot, for whatever re-teaches resting orders after a gatewayhub
    /// restart. Called once, after assembly. Nothing to do for an in-process backend.
    fn core_up(&self, _snapshot: Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>>) {}
}

/// **The gatewayhub as the live mount's backend** (production): the first call connects (the bounded
/// wait, only when an account needs a venue), every later call is a view of that one connection.
pub(super) struct RemoteCtor {
    dial: GatewayDial,
    client_id: String,
    connected: std::sync::OnceLock<RemoteBackend>,
    mounted: std::sync::Mutex<Option<Arc<RemoteBackend>>>,
    /// What the boot restore tells the leases (resume cursors, the coids to prefetch wire ids for).
    boot: crate::remote_backend::restore::BootHints,
    /// The core's snapshot, late-bound; shared with every backend view.
    cell: crate::remote_backend::reteach::SnapshotCell,
}

impl RemoteCtor {
    /// A ctor over `dial`; `client_id` names this process's leases.
    pub(super) fn new(dial: GatewayDial, client_id: String) -> Self {
        RemoteCtor {
            dial,
            client_id,
            connected: std::sync::OnceLock::new(),
            mounted: std::sync::Mutex::new(None),
            boot: crate::remote_backend::restore::BootHints::none(),
            cell: crate::remote_backend::reteach::SnapshotCell::default(),
        }
    }

    /// This ctor handing the boot restore's hints to every lease it opens.
    #[must_use]
    pub(super) fn with_boot(mut self, boot: crate::remote_backend::restore::BootHints) -> Self {
        self.boot = boot;
        self
    }
}

impl BackendCtor for RemoteCtor {
    fn backend(
        &self,
        _vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        data_only: &std::collections::HashSet<String>,
    ) -> Arc<dyn vike_tradehub_core::VenueBackend> {
        let connected = self.connected.get_or_init(|| {
            RemoteBackend::connect(
                self.dial.link.clone(),
                policy.accounts.clone(),
                self.client_id.clone(),
                self.dial.connect_wait,
            )
            .with_boot(self.boot.clone())
            .with_snapshot_cell(Arc::clone(&self.cell))
            .with_project_state_dir(vike_boot::halt::declared_project_state_dir())
        });
        let view = Arc::new(connected.view(policy.accounts.clone(), data_only.clone()));
        *self.mounted.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(Arc::clone(&view));
        view
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
        not_running: &[vike_tradehub_core::NotRunning],
        datahub: Option<crate::feeds::DatahubProbe>,
    ) -> Option<crate::late_arm::LateArmPlan> {
        let waiting: Vec<String> = not_running
            .iter()
            .filter(|n| n.block == vike_config::ArmingBlock::GatewayUnavailable)
            .map(vike_tradehub_core::NotRunning::route_key)
            .collect();
        crate::late_arm::LateArmPlan::of(self.dial.link.clone(), waiting, datahub)
    }

    fn notice_plan(&self, why: String) -> Option<crate::late_arm::LateArmPlan> {
        Some(crate::late_arm::LateArmPlan::notice(self.dial.link.clone(), why))
    }

    fn replays_events(&self) -> bool {
        true
    }

    fn core_up(&self, snapshot: Arc<arc_swap::ArcSwap<vike_exec::CoreSnapshot>>) {
        // Filled once, after assembly; the worker reads it only when a gatewayhub restart happens.
        let _ = self.cell.set(snapshot);
    }
}

/// What a LIVE mount hands back: the core, its teardown handles, `assemble_runtime`'s own arming record,
/// the late-arm plan (`Some` when an account waits for the gatewayhub) and every account the node does
/// NOT run with why (decision 0156: `vike_tradehub_core::AssembledRuntime::not_running`, published
/// with every frame).
pub(super) type LiveRuntime = (
    CoreHandle,
    LiveTeardown,
    std::collections::HashSet<String>,
    Option<crate::late_arm::LateArmPlan>,
    Vec<vike_exec::NotRunningAccount>,
);

/// The LIVE mount (`flags.tradehub_live`): stand up the wired-market [`vike_tradehub_core::assemble_runtime`] core
/// with the resolved strategy mounts folded in (split-plane I10: one for the historical
/// single-mount profile, N for a `[[mounts]]` one), then wire each DISTINCT mounted venue's live
/// market feed onto it. Returns the live
/// [`CoreHandle`] plus the [`LiveTeardown`] handles. HARD-errors (never a silent paper fallback) on any
/// venue/symbol the daemon has not wired for live — the daemon's own allow-list (safety gates #4/#5).
/// The PER-VENUE credential gate (#2) and each venue's own network gate (#3, decision 0095: the
/// account's own tier for binance/bybit/okx/hyperliquid, decision 0119) still apply INSIDE
/// `assemble_runtime`, so a credential-less venue runs paper even here (no real orders).
///
/// `risk_profile` is the active `run` row's [`vike_config::RunProfile`] `[risk]` table (RunProfile
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
/// decides, and a box with no active non-paper row mounts an ALL-PAPER daemon regardless of the
/// credential store — see this module's doc. `market_slippage` is a binding field, consumed by the
/// hyperliquid bridge's mount as `AdapterRequest::market_slippage` (this daemon's primary live venue,
/// and the only roster venue with no native market order, so its every market intent and every
/// tripped stop-MARKET is priced at that band); for that one, no file leaves the mount on
/// hyperliquid's own compiled-in literal, byte-identically.
///
/// `flags` is the resolved [`vike_config::Flags`] — this arm consumes `reconcile`,
/// `tradehub_record` and `oco_cancel_sibling_on_dead_exit`, each still overridden by its own
/// variable inside the loader. They arrive as a parameter rather than being read here so that ONE
/// load decides them for the whole process.
///
/// `mounts` is the resolved mount SET (split-plane I10: N strategies / N venues, one row for the
/// historical single-mount profile) — each entry carries its strategy (the A-S maker by default,
/// or whatever that profile row's `[strategy]` table named), its A-S lowering `cfg` (this
/// function's FEED wiring reads it: `cfg.token_id`/`cfg.interval` name what to subscribe, and the
/// Polymarket arm's `TickBarSynthesizer` window is `cfg.interval_ms`) and its strategy-free `spec`
/// projection ([`vike_tradehub_core::build_live_multi_strategy_core`] mounts on those). Per entry the two
/// agree by construction — the caller derives each `spec` from its `cfg`.
///
/// The FIRST entry is the PRIMARY mount: its `cfg` supplies the per-venue account seed
/// (`RuntimeConfig::seed_cash` / `CoreConfig::seed_cash`), exactly as the single-mount daemon always
/// did. Each DISTINCT venue's feed arm is wired exactly once, however many mounts share the venue
/// (subscriptions dedup per series inside the arm); the teardown handle carries one [`LiveFeeds`]
/// entry per venue.
///
/// ⚠ The `allow` matches [`live_runtime_with`]'s, which has carried one since it was written, and for
/// the same reason: every parameter here is a FACT THIS FUNCTION MAY NOT RESOLVE ITSELF — the
/// policy, the flags, the sentinel directory, the origin claim, the WAL map — and bundling them
/// into a struct to satisfy a count would hide the one property
/// `crates/vike-tradehub/tests/live_lock_claim_order_gate.rs` follows through this signature by INDEX.
#[expect(clippy::too_many_arguments)]
pub(super) fn live_runtime(
    mounts: Vec<ResolvedAdapter>,
    risk_profile: Option<vike_model::ProfileRisk>,
    profile: Option<vike_config::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    state_dir: &std::path::Path,
    // `config.instance_origin` — see [`live_runtime_with`]. LAST, and deliberately not beside
    // `flags`: `crates/vike-tradehub/tests/live_lock_claim_order_gate.rs` pins `state_dir`'s POSITION in
    // this signature (it is the live-account sentinel's directory, and the gate follows it by
    // index), so a new parameter goes after it or the pin measures the wrong argument.
    instance_origin: Option<vike_model::InstanceOrigin>,
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
    boot_restore: vike_core::replay::BootRestore,
    // The trading-mount batch's `config.*` rows (`MountRows`), resolved ONCE by `run`: this
    // function reads no setting of its own. LAST, for the same reason as `boot_restore`.
    rows: MountRows,
    // Where the gatewayhub is and how long to wait for it at start (`config.gatewayhub_addr`,
    // `config.gatewayhub_connect_wait_ms`), resolved ONCE by `run`. LAST, for the same reason as
    // `rows`.
    dial: GatewayDial,
) -> Result<LiveRuntime, String> {
    // The DEPLOYMENT map — the names the workspace reads by name (flags, attribution codes, tool
    // paths) — the daemon binary owns this I/O (the light client crates must not). An account's
    // credentials are NOT read out of it: they ride the `account` snapshot below, by field
    // (decision 0156 item 6), and absent ones keep that account PAPER even with the gate on
    // (safety gate #2). See [`workspace_credentials`].
    let vars = workspace_credentials();
    // ⚠ THE `account` TABLE IS READ HERE, in the composition root, and nowhere below it. Since
    // decision 0119 it is what ARMS a venue — each account trades at its own `tier` while `active`
    // — and since decision 0156 item 6 it carries each account's credentials too: the mount hands
    // every bridge the fields of the ONE account row it mounts (dukascopy's legal entity is that
    // row, `crates/bridges/dukascopy/src/adapter.rs`), and the arming projection must describe
    // exactly what the mount will do. Until 2026-09-15
    // `vike-tradehub-core` opened the store itself, from a library file, at a directory taken from a
    // process global: the class `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
    // `CREDENTIAL_STORE_PIN` ratchets down, and invisible to it because neither account reader
    // was one of `CREDENTIAL_STORE_READERS`' keyed names. Both are keyed now, and this root —
    // already pinned, already the box's one credential reader — performs the read.
    //
    // ONE snapshot, handed to [`live_runtime_with`] as a PARAMETER (the seam tests plant their own
    // rows through it) and carried on the policy object both seams receive, so no caller can hand
    // the projection and the mount different tables. Errors are carried VERBATIM rather than
    // swallowed: a store that exists and will not open refuses a labelled account by NAMING the
    // failure, where a swallowed error used to be re-reported as a bad `account` row.
    let accounts = vike_secrets::account_directory::AccountDirectory::read(
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
        Ok::<_, String>(vike_secrets::account_credentials::CredentialBook::new()),
    );
    // What the boot restore tells the gatewayhub leases: each account's resume cursor and the coids the
    // restore will ask the wire-id prefetch about (the journal's orders and the ownership file's).
    let boot_hints = crate::remote_backend::restore::BootHints::from_restore(
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
        vars,
        state_dir,
        instance_origin,
        journal_rung,
        &production_feed_ctors(),
        accounts,
        boot_restore,
        rows,
        &RemoteCtor::new(dial, client_id()).with_boot(boot_hints),
    )
}

/// The production feed constructor: the dial to the datahub's strategy lane (wave 2 of `vike-gatewayhub`).
/// The address is `config.datahub_addr` (the datahub's own loopback default when the row is unset: a
/// tradehub and its datahub share a box), and the key is the tradehub's strategy-lane key
/// (`VIKE_DATAHUB_TRADE_KEY`) out of the hub-key store `hub_server::resolve_daemon_hub_key_store` reads.
/// An absent key is not an error HERE: every dial then refuses by name (absent credentials are the gate),
/// and a store that exists and will not read says so at `error` and is the same refusal.
fn production_feed_ctors() -> ProdFeedCtors {
    let addr = settings::boot_config()
        .datahub_addr
        .unwrap_or_else(|| vike_config::DEFAULT_DATAHUB_ADDR.to_string());
    // The key is read AGAIN at every dial and at every probe (the late arm polls the probe): a start with no key
    // is held with the reason, and a key the operator mints afterwards must be found by the next poll.
    ProdFeedCtors::with_key_source(addr, datahub_trade_key)
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
            .remove(vike_hub_proto::auth::DATAHUB_TRADE_KEY_ENV)
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

/// [`live_runtime`] with its impurities as PARAMETERS — the deployment map (production:
/// [`workspace_credentials`], the store the binary owns), the `account` snapshot with every
/// account's credentials by field (`accounts`, production: [`live_runtime`]'s one read) and the
/// feed-construction seam (production: [`ProdFeedCtors`], the dial to the datahub's strategy lane). The split exists for the deterministic data-only splice test
/// (`src/tradehub_cli/tests/feed_splice.rs`): a scripted constructor plus fake credentials planted in
/// the account directory is what lets a CI test drive the REAL mount path — plan gate, withhold,
/// `assemble_runtime`, feed wiring — with no store, no network and no weekday market.
///
/// The third return is `assemble_runtime`'s own `live_venues` record — the per-venue EXEC ARMING STATE
/// (`vike_tradehub_core::make_engine` inserts a venue exactly when it constructs a real exec client). It is
/// what the seam test asserts "the data-only declaration kept exec paper" against, and it is also
/// what `main` RENDERS: the ready banner's `mode` and every `WireMountRow::live` are derived from
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
// flags, the deployment map, the state directory the B11 sentinels are claimed in, this
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
    mounts: Vec<ResolvedAdapter>,
    risk_profile: Option<vike_model::ProfileRisk>,
    // The SAME run profile `risk_profile` was derived from, carried WHOLE so its `[guards]` and
    // `[sinks]` tables can reach the `CoreConfig` below. Threading it stopped at `[risk]` before:
    // every guard an operator wrote was parsed, validated, announced as ignored — and dropped.
    profile: Option<vike_config::RunProfile>,
    policy: &vike_config::Policy,
    flags: vike_config::Flags,
    mut vars: HashMap<String, String>,
    // Where the B11 live-account sentinels are claimed (`<state_dir>/LIVE-<venue>.lock`) — the
    // daemon's own `booted.state_dir`, a PARAMETER because the claim is made here (after the
    // `data_only` withhold, before `assemble_runtime`) and this function resolves no paths of its own.
    state_dir: &std::path::Path,
    // `config.instance_origin` — this deployment's origin claim, stamped into every client order
    // id this core mints so a SECOND instance on the same venue account is recognisable on
    // reconcile rather than anonymous (`vike_model::instance_origin`). A PARAMETER because this
    // function resolves no settings of its own; `None` (the default) is byte-identical to before
    // the key existed.
    //
    // ⚠ AFTER `state_dir`, not beside `flags`, and that is a constraint rather than a preference:
    // `crates/vike-tradehub/tests/live_lock_claim_order_gate.rs` follows the sentinel directory through
    // this signature BY INDEX, so a parameter inserted ahead of `state_dir` makes that gate pin
    // the wrong argument. Add new ones at the end.
    instance_origin: Option<vike_model::InstanceOrigin>,
    // The WAL's directory rung — see [`live_runtime`].
    journal_rung: &crate::profile_rows::JournalRung,
    make: &dyn FeedCtors,
    // The `account` table snapshot — which accounts trade, at which tier (decision 0119), and each
    // account's credentials by field (decision 0156 item 6) — read ONCE by [`live_runtime`]
    // (production) or planted by a seam test, and carried on `AdapterPolicy::accounts`. LAST,
    // like every addition since the live-lock gate pinned `state_dir`'s position.
    accounts: vike_secrets::account_directory::AccountDirectory,
    // The boot restore `run` computed (see [`live_runtime`]); applied to the core config below, after
    // the run profile's `[guards]`. LAST, for the same reason as `accounts`.
    boot_restore: vike_core::replay::BootRestore,
    // The trading-mount batch's `config.*` rows — see [`live_runtime`]. LAST, for the same reason.
    rows: MountRows,
    // The venue BACKEND the mount is assembled over (production: [`RemoteCtor`], the gatewayhub;
    // the splice tests: an in-process one). LAST, for the same reason as `rows`.
    ctor: &dyn BackendCtor,
) -> Result<LiveRuntime, String> {
    // The reconcile FAMILY's rows — cadences, lookbacks, the policy name, the balance tolerances,
    // AND the two sibling FLAGS (`reconcile_generate_missing`, `reconcile_balance`) — read into the
    // one value `reconcile_config::build_recon_config` takes below, from the same resolved `flags`
    // the gate reads and the boot's `config` (`daemon_recon_settings`). Rows only (decision 0111):
    // no `VIKE_RECONCILE_*` variable reaches it, and an unset policy is `quarantine`
    // (`reconcile_config::parse_policy`).
    //
    // ⚠ The MASTER GATE itself is decided further down, not here, and the move is deliberate: since
    // S2 the default is ON for a mount that arms a live venue account, and that fact is not known
    // until the policy has been through the `data_only` withhold below and `vike_tradehub_core::armed_live_venues`
    // has been asked. Deciding it up here would either read a pre-withhold policy (arming reconcile for
    // a venue this daemon deliberately leaves on paper) or force a second probe call that could
    // disagree with the one the lock claim uses.
    let recon_settings = daemon_recon_settings(flags, &settings::boot_config());

    // ⚠ **FIRST, before anything reads `vars`.** The resolved flags have to be in the map before
    // `venue_feed_plan`, before `armed_live_venues` and a long way before `assemble_runtime`, because
    // every one of those asks the map a question a `flags` setting is now allowed to answer.
    // Folding later would give the earlier readers a different map from the later ones, which is
    // the "one flag, two places, disagreeing" failure `vike_config::flags`' module doc names.
    //
    // The `data_only` withhold below does not touch this map: it takes a declared venue's
    // credentials out of the ACCOUNT directory (a credential belongs to its account, decision 0156
    // item 6), so no folded key can be counted in that disclosure's `keys_withheld`.
    fold_flags_into_vars(flags, &mut vars);

    // THE MOUNT'S POLICY, built ONCE and BEFORE the plan loop, because the loop asks the venue
    // registry which NETWORK each venue's accounts mount on (`venue_feed_plan` →
    // `venue_arming::feed_tier`/`cex_mainnet_enabled`) and that answer has to come from the very
    // value the mount is built from — a bridge's `resolve` that reads the `account` table or a
    // `venue_setting` row must see the same snapshot in both places, or the feed's network is a
    // second opinion again. It is built ahead of the `data_only` withhold pass and that pass takes
    // the declared venues' credentials OUT of it (each account's credentials ride on its
    // `accounts`, decision 0156 item 6), after the plans resolved theirs; it is moved into the
    // `RuntimeConfig` below, exactly as before.
    let mut adapter_policy = vike_tradehub_core::AdapterPolicy {
        // The ONE `account` table snapshot [`live_runtime`] read (see the read's own comment there),
        // carried on the policy object both seams receive.
        accounts,
        // `config.paper_slippage` — what a `paper`-tier account's simulated exchange slips fills
        // by. `None` (no row) keeps the mount's compiled default, the book it always built.
        paper_slippage: rows.paper_slippage,
        // `config.preflight_disk_warn_mib` / `_fail_mib` — the disk floors the startup preflight
        // judges the journal directory by; no rows is the compiled pair.
        preflight_disk: rows.preflight_disk,
        ..vike_tradehub_core::AdapterPolicy::from(policy)
    };

    // Safety gates #4/#5 — the daemon's venue+symbol ALLOW-LIST, resolved per MOUNT and BEFORE
    // `vars` is moved into the RuntimeConfig (split-plane I10: every mount must pass its own venue's
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
    // an account needs a venue, `crate::remote_backend::needs_gateway`): the table the daemon
    // computes from its own account rows and credentials is fetched ONCE, here, and every later
    // read — the mount's own table included — is a view of that one snapshot.
    let plan_table = ctor
        .backend(&vars, &adapter_policy, &std::collections::HashSet::new())
        .arming_table()
        .map_err(|e| format!("refusing the live mount: {e}"))?;
    let mut venue_plans: Vec<(String, VenuePlan)> = Vec::new();
    for m in &mounts {
        let plan = venue_feed_plan(&m.cfg, &plan_table, &adapter_policy)?;
        if !venue_plans.iter().any(|(v, _)| v == &m.cfg.venue) {
            venue_plans.push((m.cfg.venue.clone(), plan));
        }
    }
    // The DATA-PLANE-ONLY declarations (the `data_only` profile key, validated to the
    // credentialed-data venue set at load): withhold each declared venue's credentials from the
    // account directory `assemble_runtime`'s exec arms will read (the policy's `accounts`: a credential
    // belongs to its account, decision 0156 item 6) — AFTER the plan loop above, which is the
    // ordering the seam rests on (each credentialed-data plan CARRIES its resolved config, so the
    // FEED keeps the credentials exec loses; see
    // [`crate::venue_arming::withhold_data_only_credentials`]). An undeclared mount leaves the
    // policy untouched, byte-identically. The startup line is the declaration's own disclosure —
    // the arming banner inside `wire_venue_feeds` then announces the resulting paper exec per
    // venue ([`data_only_arming`]).
    let data_only: std::collections::HashSet<String> = mounts
        .iter()
        .filter(|m| m.row.data_only_effective())
        .map(|m| m.cfg.venue.clone())
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
            return Err(why);
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
    let feed_down: Option<String> = datahub.as_ref().and_then(crate::feeds::DatahubProbe::check);
    // THE MOUNT'S BACKEND AND ITS ARMING SNAPSHOT, read ONCE here: after the `data_only` withhold (so a
    // declared venue's exec keys are already gone and its rows say they do not run, which is what
    // this daemon actually does), over the folded `vars` and the final `adapter_policy`. The armed
    // set below, the journal rows and the `assemble_runtime` this moves into all read this one
    // table.
    //
    // ⚠ NO LIVE-ACCOUNT LOCK IS CLAIMED HERE ANY MORE (split-plane B11, the Danger-2 tripwire: one
    // live process per venue ACCOUNT). The claim moved to the process that owns the venue clients:
    // the gatewayhub claims `LIVE-<route key>.lock` when it answers the account's `TradeOpen`, which
    // `RemoteBackend::start_adapter` sends before it asks for the adapter, so a second trading
    // process is refused before a venue client exists. A lease another client holds refuses this
    // whole mount just after assembly ([`BackendCtor::lease_refusals`]) with no market data wired,
    // as the lock's `AddrInUse` always did. What stays here is the armed SET, because the SAME
    // list decides whether this mount reconciles:
    //
    // ⚠ The probe reads the SETTINGS, not this profile's mounts, and that is correct rather than an
    // omission: a second account ARMS on its own ACTIVE `account` row plus its own
    // credentials, and `make_engine_accounts` mounts it whether or not a `[[mounts]]` row names it.
    let backend = ctor.backend(&vars, &adapter_policy, &data_only);
    let backend = match &feed_down {
        Some(why) => {
            let held: std::collections::BTreeSet<String> =
                venue_plans.iter().map(|(v, _)| v.clone()).collect();
            tracing::error!(
                reason = %why,
                venues = %held.iter().map(String::as_str).collect::<Vec<_>>().join(", "),
                "{}: {}: every `demo`/`live` account of the mounted venues is NOT RUNNING this session \
                 (the table carries the word `{}`; the cause is the datahub's strategy lane, not the \
                 gatewayhub, and `vike-cli venues` may still show them as would-arm), no feed is wired, \
                 and `paper` accounts run without prices on those venues. The late arm restarts this \
                 tradehub armed once the datahub (and, for an account that waits for it, the gatewayhub) \
                 answers: {why}",
                crate::feed_gate::PRICE_FEEDS_UNAVAILABLE,
                crate::feeds::short_cause(why),
                vike_config::ArmingBlock::FeedsUnavailable.as_str(),
            );
            crate::feed_gate::FeedGatedBackend::wrap(
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
    let recon_gate =
        reconcile_config::reconcile_gate(flags.reconcile, flags.reconcile_off, armed_live.len());
    // Disclosed at WARN whichever way it went, through the ONE shared emitter: "reconcile is on and
    // nobody asked" and "reconcile is off on a live box" are both things an operator must be able to
    // find in a journal without knowing which flag to grep for, and `vike-app` had to say it the same
    // way while it reconciled. See `reconcile_config::log_reconcile_gate`.
    reconcile_config::log_reconcile_gate(recon_gate, armed_live.len());
    let recon_enabled = recon_gate.enabled();
    // ...and the one CROSS-mount consistency gate, likewise before any core spawns: two polymarket
    // mounts on ONE token must agree on the interval, because they will share one `MakerSink`
    // whose bar synth runs at exactly one window (see `LiveFeeds::Polymarket`).
    check_poly_token_intervals(&mounts)?;
    // ...and its ctrader twin (split-plane I9): every ctrader mount shares one data socket and one
    // bar SYNTHESIZER window, so rows disagreeing on the interval are refused before any core
    // spawns — see `check_ctrader_intervals`.
    check_ctrader_intervals(&mounts.iter().map(|m| &m.cfg).collect::<Vec<_>>())?;

    // Opt-in PIT-`SymbolProperties` recorder (`flags.record_properties`). ⚠ ALWAYS `None` since
    // the daemon stopped writing the store (#2093, 2026-09-22, docs/decisions/0084):
    // `PropertiesRecorder` opens the concrete DataFusion store, which only a `record-feeds`/
    // `materialize` build carried (both enabled the DataFusion tree, now `vike-store`, and #2093
    // deleted both); the recorder that survives runs inside the datahub, which opens its own
    // store. So nothing is constructed here, and `flags.record_properties` is inert in this binary
    // whatever sets it — the flag fold still writes it into `vars`, and no code downstream reads
    // that key.
    //
    // ⚠ This comment described a construction HERE, through the recorder's map half over the
    // folded `vars`, until 2026-09-28 — a call this function has not made since #2093. Decision
    // 0111 deleted that map half and the recorder's environment door with it: a root that
    // constructs one again calls `vike_store::open_properties_recorder(root, flags.record_properties)`.
    let properties_rec: Option<Arc<vike_data::PropertiesRecorder>> = None;

    // The live core config carries the SAFETY KNOBS a live daemon must be capped with (NOT the paper
    // mount's minimal `CoreConfig::default()`). Field names/values mirrored vike-app's fat-build
    // CoreConfig (the desktop builds none since #1610): a 30s submit-ack backstop, a 25%
    // equity-drawdown liquidate-only latch, and the margin-call watchdog (inert at 1× until
    // leverage is raised). `recon_enabled` is the
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
    let primary_seed = mounts[0].cfg.seed_cash;
    // THE ARMING ROWS, computed HERE and nowhere else in this function. The window is narrow and
    // both edges are load-bearing: AFTER `withhold_data_only_credentials` above (so a `data_only`
    // venue's exec keys are already gone from `adapter_policy`, and the rows say paper for it, which
    // is what this daemon actually does) and BEFORE `vars`/`adapter_policy` move into `RuntimeConfig`
    // below (after which neither exists to read).
    //
    // This is why `vike_tradehub_core::journal_venue_mounts` takes ROWS rather than the map. A signature
    // taking `vars` would let the call sit anywhere downstream and be wrong by a comment; taking
    // rows makes the caller pick this moment, and there is only one.
    let arming = vike_tradehub_core::venue_arming(&table, &adapter_policy);

    // THE DEAD-MAN'S ABSENT-KEY WARNING, once, beside the construction it describes (the
    // `deadman:` field of the literal below). Here and not in `deadman_config_from_policy`, because
    // this is the composition root where `policy` is known to be a LIVE mount's — the fold is pure
    // and the gate-off `start_paper` arm never calls it, so a warning inside the fold would be a
    // warning nobody could count. Fires for an ABSENT key only: `deadman_timeout_ms = 0` is the
    // operator's recorded decision and gets no line. `warn_deadman_absent`'s doc argues the shape.
    warn_deadman_absent(policy);

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
    let link_venues: Vec<(String, AdapterLinkDisclosure)> =
        venue_plans.iter().map(|(v, plan)| (v.clone(), adapter_link_disclosure(plan))).collect();
    for line in link_deadman_arming_report(policy, &link_venues) {
        tracing::info!("{line}");
    }

    let mut runtime_cfg = RuntimeConfig {
        // THE VENUE BACKEND over the registry this daemon names the bridges in (docs/decisions/0096,
        // amended 2026-09-29): `assemble_runtime` names no bridge, and is handed the backend that
        // answered the table above.
        backend,
        vars,
        properties_rec,
        seed_cash: primary_seed,
        recon_enabled,
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
            // The origin claim this daemon's client order ids carry — the parameter above,
            // straight through. Consumed at a FRESH coid session only (a restart resumes its
            // persisted one verbatim): `vike_core::CoreConfig::instance_origin` argues why.
            instance_origin,
            // ⚠ These three are the daemon's DEFAULTS, not its answer: `profile`'s `[guards]` table
            // overwrites whichever of them it names, via `apply_guards_and_sinks` below. Before that
            // call existed they were the only values that could ever apply, and an operator's
            // configured guards were parsed, warned about and discarded.
            submit_ack_timeout: Some(Duration::from_secs(30)),
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
            // Honor the same opt-in write-ahead journal the paper mount does (off by default):
            // on exactly when the `config.journal_dir` row is set, in that directory, tuned by the
            // active run row's `[sinks.journal]` when it carries one. The same
            // function over the same two inputs as `run`'s one resolution, which the paper core
            // and the wire tearsheet take — see `crate::profile_rows::journal_config_for`.
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
            // wants. Both mounts read it now (the paper arm via `PaperCoreOpts`), so the rehearsal
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
            strategy_factory: Some(crate::mount_factory::strategy_factory()),
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
            // it is NOT a `ReconSettings` knob (it configures the core, not the pass). The paper
            // arm (`start_paper`) runs no reconcile pass and leaves the field at its default.
            restore_orders_off: flags.reconcile_restore_off,
            // The venue gate of that restore follows the reconcile VERDICT resolved above. A box
            // that refused reconcile (`flags.reconcile_off`) mounts no driver, so no pass ever
            // adopts a restored order: with the declared table's gate the core would mute the
            // strategy's tagged resubmit for the whole refusal window "until the first pass" and
            // then orphan the order anyway. Every other verdict keeps the table itself (the CI box
            // builds exactly the gate it always did). `restore_venue_gate` argues why this is not
            // `restore_orders_off`.
            restore_venue_gate: reconcile_config::restore_venue_gate(recon_gate),
            // A wired market whose venue stays on PAPER (no credentials, `data_only`, a refused
            // tier) runs a paper book in this process's memory, which a restart empties: its
            // restored orders are reported gone at boot, since no reconcile pass ever judges them
            // (decision 0127). The core reads which engines are paper from `ExecutionEngine::mode`,
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
    // on the FINAL numbers — this daemon's own `submit_ack_timeout: Some(30s)` literal above, or
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
            vike_tradehub_core::apply_guards_and_sinks(p, &mut runtime_cfg.core_config);
        if let Some(hazard) = confirm_grace {
            tracing::warn!("{hazard}");
        }
    }
    // The fast in-flight confirm against the timeout the profile may just have changed: read AFTER
    // `apply_guards_and_sinks`, because the effective `submit_ack_timeout` is the daemon's 30 s
    // literal or the profile's `[guards]` value, and only here is it known.
    if let Some(why) = inflight_confirm_dead_band(
        runtime_cfg.core_config.inflight_confirm,
        runtime_cfg.core_config.submit_ack_timeout,
    ) {
        tracing::warn!("{why}");
    }
    // The trading state each engine the journal restores comes back in — read HERE, because
    // `apply_to` below consumes the restore, and needed after the core is up: a profile's
    // `guards.initial_trading_state` is never sent over a STRICTER state the journal restored
    // (`vike_tradehub_core::send_initial_trading_state`).
    let restored_trading_states: Vec<vike_model::TradingState> =
        boot_restore.restored_engines().iter().map(|snap| snap.trading_state).collect();
    // THE BOOT RESTORE (decision 0127): the coid session, the books, each engine's account state
    // and the resting-order candidates the journal gives back — through the one function the paper
    // arm uses too, and after the `[guards]` above, which it does not touch (no journaled limit is
    // ever applied).
    boot_restore.apply_to(&mut runtime_cfg.core_config);

    // Fold the resolved strategies into `core_config.strategy` + `extra_mounts`, then build the
    // wired-market runtime. Per-venue exec is credential-gated inside `assemble_runtime` (absent creds ⇒ that
    // venue is paper), and `spawn_core_multi` wires applied-fill capture on each mount's own venue
    // engine. `recon_clients`/`recon_trigger` are captured (the reference mount needs them).
    //
    // ⚠ `build_live_multi_strategy_core` with ONE mount IS `build_live_strategy_core` (first mount
    // → `strategy`, an empty rest → `extra_mounts`), so the single-mount daemon builds the same
    // node it always did. `cfgs` is cloned out first — the feed wiring below still needs each
    // mount's A-S lowering after the specs move into the core.
    let cfgs: Vec<MakerConfig> = mounts.iter().map(|m| m.cfg.clone()).collect();
    let vike_tradehub_core::AssembledRuntime {
        handle,
        recon_clients,
        recon_trigger,
        live_venues,
        forwarder_stop,
        not_running,
    } = vike_tradehub_core::build_live_multi_strategy_core(
        mounts
            .into_iter()
            .map(|m| vike_tradehub_core::StrategyMountSpec { strategy: m.strategy, spec: m.spec })
            .collect(),
        runtime_cfg,
    )
    .map_err(|e| format!("build_live_multi_strategy_core: {e}"))?
    .runtime;
    // The core is up: whatever re-teaches resting orders after a gatewayhub restart reads its snapshot.
    ctor.core_up(handle.snapshot_cell());

    // A LEASE ANOTHER TRADE CLIENT HOLDS refuses the whole live mount — the old lock's `AddrInUse`,
    // kept (two live processes on one venue account is the accident the lock exists to refuse). It
    // is read HERE, with the core up and nothing wired to it yet: no feed, no strategy tick, no
    // order has been sent, so tearing the core down is the same as not having started it. The
    // forwarder is quiesced first (the order the teardown always keeps: a live pump can otherwise
    // wedge the core join).
    let refusals = ctor.lease_refusals();
    if !refusals.is_empty() {
        crate::feed_gate::tear_down_failed_start(handle, Vec::new(), &forwarder_stop);
        return Err(format!("refusing LIVE mount: {}", refusals.join("; ")));
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
    // `spawn_recon` mount runs AFTER the feeds so a feed-subscribe error returns BEFORE any
    // `vt-core-recon` thread is spawned (no orphaned driver on the error path). OFF (a paper mount): the
    // handles are moved into the tuple and DROPPED right here — matching the pre-reconcile daemon's
    // `..`-destructure drop timing exactly (`recon_trigger` is already `None`, since `assemble_runtime` gates
    // the channel on the same flag) — so no ingredients survive to mount a driver. This is the ONLY
    // unconditional new binding, and its OFF-path effect is a byte-identical early drop.
    let recon_ingredients = recon_enabled.then_some((recon_clients, recon_trigger));

    // ⚠ The daemon no longer records its own feed (0084: the store has ONE writer plane, and the
    // recorder that survives runs inside the datahub with its watchdog, silence detection and
    // record profiles — none of which the removed `record-feeds` tee had). `post_feeds` stays as
    // the teardown slot so the sequential TAIL below keeps its shape; it is always `None` now.
    let post_feeds: PostFeeds = None;

    // Identity: nothing is teed off the venue's base sink any more.
    let wrap = |base: Arc<dyn LiveDataSink>| -> Arc<dyn LiveDataSink> { base };

    // Wire the venue's live market feed onto the core's ingest lanes.
    // Wire each mounted venue's live market feed onto the core's ingest lanes — ONE feed arm per
    // distinct venue, however many mounts share it (subscriptions dedup per series INSIDE the
    // arm). The arms live in `wire_venue_feeds`, directly above `live_runtime`, where
    // `cex_feed_wiring_pin.rs` scans them.
    let mut feeds: Vec<LiveFeeds> = Vec::with_capacity(venue_plans.len());
    // ...none at all when the datahub's strategy lane is away (`feed_down` above: the accounts that needed
    // a price are NOT RUNNING and the late arm waits). A feed that REFUSES here — the gatewayhub's
    // credentialed-stream refusal, an interval a bridge cannot serve, or a datahub that went away between
    // the probe and now — ends the start, as it always did, but it ends it the way the lease refusal above
    // does: the feeds already wired are released and the core is told to stop (journal flushed), instead of
    // the process exiting under a live core thread.
    //
    // ⚠ A TRANSIENT refusal (the datahub's gatewayhub link is away while the datahub itself passed the probe: a
    // link that is down, an ack that never came) is survived where it honestly can be (`survivable_refusal`: no
    // `demo`/`live` account of the venue RUNS, so nothing is traded on a missing price): that venue's feed is
    // left unwired (the partial one was shut down in order when the arm dropped it), its `paper` accounts run
    // without prices, and the late arm waits and restarts the tradehub armed. A venue with a running live account
    // cannot be held once the core is up, and ends the start as before.
    //
    // ⚠ A CAP refusal (`CAP_MARK`: a pump, session or stream cap that is full) is the third class: it persists until
    // an OPERATOR raises the cap or drops a subscription, so no restart can mend it and none is ordered. Where a
    // transient would be survived (no `demo`/`live` account of the venue runs) the venue stays UP and priceless
    // (`feeds_capped`: its feed is unwired, `paper` and every other venue run, the banner names the cap, ONE `error`
    // line says what to change, and the late arm is not told to wait for it). A venue with a running live account
    // cannot be held after the core exists; that start ends with `failed_start_exit`'s status, which the unit does not
    // restart on (a plain failure exit would loop the whole process every `RestartSec=`).
    let mut feeds_held_back: Vec<(String, String)> = Vec::new();
    let mut feeds_capped: Vec<(String, String)> = Vec::new();
    for (venue, plan) in venue_plans.iter().filter(|_| feed_down.is_none()) {
        let venue_cfgs: Vec<&MakerConfig> = cfgs.iter().filter(|c| &c.venue == venue).collect();
        match wire_venue_feeds(
            plan,
            &venue_cfgs,
            &handle,
            &live_venues,
            &arming,
            &data_only,
            &wrap,
            make,
        ) {
            Ok(wired) => feeds.push(wired),
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
                feeds_held_back.push((venue.clone(), refusal));
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
                feeds_capped.push((venue.clone(), refusal));
            }
            Err(refusal) => {
                crate::feed_gate::tear_down_failed_start(handle, feeds, &forwarder_stop);
                return Err(refusal);
            }
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
    let feeds_why: Option<String> =
        feed_down.clone().or_else(|| feeds_held_back.first().map(|(_, why)| why.clone()));
    let mut late_arm =
        ctor.late_arm(&not_running, (feeds_why.is_some()).then(|| datahub.clone()).flatten());
    if let Some(plan) = late_arm.as_mut() {
        plan.feeds_why.clone_from(&feeds_why);
        let held: Vec<&str> = feeds_held_back.iter().map(|(venue, _)| venue.as_str()).collect();
        plan.gateway |= crate::feed_gate::arms_through_gateway(&table, &held);
        // The feed-driven restarts are counted in the project's state directory and budgeted
        // (`late_arm::FEED_RESTARTS_PER_WINDOW`): a cause the late arm's own checks cannot see would
        // otherwise restart the whole process every ~95 s.
        plan.restarts = crate::late_arm::RestartLedger::in_dir(state_dir);
    }
    // The CAP class carries its cause to the banner and orders nothing: on a plan that exists for other reasons
    // (a gatewayhub wait, a transient feed wait) it only leads the cause; with no plan at all a notice-only plan
    // (`LateArmPlan::notice`, no thread) carries it.
    if let Some((venue, refusal)) = feeds_capped.first() {
        let capped_why = format!("{}: {venue}: {refusal}", crate::feeds::FEEDS_CAPPED_MARK);
        match late_arm.as_mut() {
            Some(plan) => plan.feeds_why = Some(capped_why),
            None => late_arm = ctor.notice_plan(capped_why),
        }
    }
    let late_arm = late_arm.map(|plan| plan.within_budget(vike_model::now_ms()));

    // Reconcile driver mount — the reference was `vike-app`'s `App::new` recon_driver
    // block (gone with the desktop's local core). Placed AFTER the feeds are up so a
    // feed-subscribe error above returns before this ever spawns a thread. `recon_ingredients` is
    // `Some` only when `recon_enabled` (the OFF path already dropped the handles above) ⇒ this
    // whole mount is byte-identically absent on a PAPER mount and whenever the operator refused it.
    // `spawn_recon` owns its own `vt-core-recon` thread and respects the single-writer rule (it
    // only blocking-fetches REST reports and enqueues `Command::ReconcileReports` for the fold thread —
    // see that module's doc); it ADOPTS the pre-built `recon_trigger` pair so a venue's recon trigger
    // reaches this same driver.
    let recon_driver = match recon_ingredients {
        None => None,
        // Enabled but every venue is paper (no creds ⇒ no `ReconClient`): inert, no thread — mirrors
        // the `recon_clients.is_empty()` arm vike-app had. The (empty) clients + `recon_trigger`
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
        Some((clients, recon_trigger)) => {
            // The per-venue feed-status health map, from the feeds THIS mount actually built
            // ([`LiveFeeds::recon_feed_statuses`]). `assemble_runtime` builds no market feeds of its own
            // (they stay with the caller — see `crates/vike-tradehub-core/src/node.rs`'s module doc), so the mounted venue's
            // own `Feeds::status` handle is the only one that exists, and it gates ONLY that venue:
            // every other reconciled venue is absent from the map, reads `Healthy`, and is never
            // health-blocked (`crates/vike-tradehub/CLAUDE.md`'s health-gate bullet).
            //
            // ⚠ The map was UNCONDITIONALLY EMPTY before the CEX arm, and empty is still the right
            // answer for hyperliquid/polymarket — the gate can only ever SUPPRESS a pass, and a
            // wrongly-suppressed pass can stay suppressed, so a row is added per venue on evidence,
            // never by sweeping up whatever handles are in scope.
            let feed_statuses = recon_feed_statuses_of(&feeds);
            let mut recon_cfg =
                reconcile_config::build_recon_config(&recon_settings, feed_statuses);
            // A policy that rewrites positions cannot ride a ring that replays (the W3c double-count
            // finding): overridden with `quarantine` (said at `error`), not refused - a refusal turns a row
            // written while the daemon runs into a restart loop with live orders resting.
            recon_cfg.policy = reconcile_config::quarantine_when_replaying(
                recon_enabled,
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
            Some(vike_tradehub_core::spawn_recon(&handle, clients, recon_cfg, recon_trigger))
        }
    };

    Ok((
        handle,
        LiveTeardown { feeds, forwarder_stop, post_feeds, recon_driver },
        live_venues,
        late_arm,
        not_running.iter().map(vike_tradehub_core::NotRunning::view).collect(),
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
