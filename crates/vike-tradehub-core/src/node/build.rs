//! The runtime assembly: `assemble_runtime`, its injected-preflight twin, and the preflight it enforces.

use std::collections::HashSet;

use crate::preflight::{CheckStatus, PreflightReport};

#[cfg(doc)]
use super::WiredMarket;
use super::accounts::{
    AccountExtras, adopt_stated_currency_in_restored_engines, armed_live_venues,
    name_the_default_account_where_a_lane_twin_runs, refuse_mounts_without_an_engine,
    refuse_unarmed_live_venues, refuse_unarmed_mount_accounts, runnable_accounts,
    runnable_accounts_of,
};
use super::hot::HotAccounts;
use super::ledger::{ledger_ccy_key, report_mixed_ledger_currencies};
use super::mounts::{declared_legs_for, in_engine_order};
use super::{AssembledRuntime, RuntimeConfig, RuntimeError};

/// Build the live node: one engine per [`super::wired_markets`] row ([`WiredMarket`]s, derived
/// from `vike_model`'s `VENUE_WIRING` per docs/decisions/0098), mounted in TABLE order and held in
/// [`WiredMarket::engine_rank`] order, the `recon_clients` list, `spawn_core_multi`, and the
/// live-event forwarder (the "dead-man" relay lane; a spawn failure is
/// `RuntimeError::ForwarderSpawn`).
///
/// STARTUP PREFLIGHT: [`crate::startup::run_startup_preflight`] runs FIRST, before any venue is
/// mounted, so every caller gets the same "before the first live order" gate from one site. No
/// network with no credentials — **nor with a full store whose accounts are all `paper`** (why
/// `cfg.policy` is threaded in); with armed credentials, a bounded DNS round plus one clock read and
/// one signed balance read per armed venue.
///
/// # Accounts that do not run
///
/// An account runs at the tier its active row states or not at all (decision 0156), so a wired
/// market whose accounts all fail to run contributes NO engine — never a paper one. Each such
/// account is listed in [`AssembledRuntime::not_running`] with its reason, handed to the core
/// (`vike_core::CoreConfig::not_running`, so an order addressed to it is refused by name), and a
/// strategy mount on one refuses the node ([`RuntimeError::Mount`]). A node in which NO account runs
/// is refused too ([`RuntimeError::NoAccountRuns`]): the core needs a primary engine.
///
/// # Panics
///
/// DEBUG builds only: on two rows naming one venue (default account mounted twice) or sharing an
/// [`WiredMarket::engine_rank`].
pub fn assemble_runtime(cfg: RuntimeConfig) -> Result<AssembledRuntime, RuntimeError> {
    // The disk leg watches the journal dir the caller ALREADY resolved into `CoreConfig` (the
    // `config.journal_dir` row) — re-resolving would be a competing authority. No journal ⇒ no
    // disk row.
    let dirs: Vec<(String, std::path::PathBuf)> = cfg
        .core_config
        .journal
        .as_ref()
        .map(|j| vec![("journal".to_string(), j.dir.clone())])
        .unwrap_or_default();
    // ⚠ THE ACCOUNT TABLE reaches the preflight here — the same `cfg.policy` every `make_engine`
    // gets. Without it the preflight went by credential PRESENCE and authenticated against
    // accounts held at `paper` (`crate::startup`'s module doc).
    // THE ARMING TABLE, read ONCE for the whole assembly: the preflight's venue lists, the lock
    // set, the account fan-out and every mount refusal read this one snapshot.
    let table = arming_table_of(&cfg)?;
    let report = crate::startup::run_startup_preflight(
        &cfg.backend,
        &table,
        &cfg.vars,
        &dirs,
        Some(&cfg.policy),
    );
    assemble_after_preflight(cfg, &table, &report)
}

/// [`assemble_runtime`] over an ALREADY-RUN preflight report, so "a preflight failure never stops a
/// mount" is testable with a hard-failing report and no network.
///
/// The report is **ENFORCED** per venue:
/// [`PreflightReport::venue_disposition`](crate::preflight::PreflightReport::venue_disposition)
/// is `Stopped` on a hard FAIL, and [`enforce_preflight`] withholds that venue's credentials and
/// stops its `demo`/`live` accounts (`ArmingBlock::PreflightFailed`).
///
/// ⚠ **Why enforcing is safe (it was advisory):** a transient timeout must not stop a live account.
/// `crate::preflight::CredentialGap` splits "answered and refused" (`Fail`, stops) from "never
/// heard back" (`Warn`, never stops), bounded by `crate::startup::CREDENTIAL_PROBE_TIMEOUT`; and
/// refused keys are the same fact as ABSENT keys, which already stop an account.
///
/// ⚠ A GLOBAL fail (`go() == false` — no disk, no internet) does NOT stop the mount: a refusing
/// daemon crash-loops under `Restart=on-failure`, a worse failure. Logged at `error`; it proceeds.
///
/// # Panics
///
/// As [`assemble_runtime`]'s `# Panics`.
pub fn assemble_runtime_with_preflight(
    cfg: RuntimeConfig,
    preflight: &PreflightReport,
) -> Result<AssembledRuntime, RuntimeError> {
    let table = arming_table_of(&cfg)?;
    assemble_after_preflight(cfg, &table, preflight)
}

/// The one arming snapshot of an assembly ([`crate::VenueBackend::arming_table`]); a backend that
/// cannot answer refuses the assembly rather than guessing which account is armed.
fn arming_table_of(cfg: &RuntimeConfig) -> Result<crate::ArmingTable, RuntimeError> {
    cfg.backend.arming_table().map_err(RuntimeError::Backend)
}

/// The body both entry points share once they hold the table and a preflight report: log it,
/// ENFORCE it onto the policy, assemble.
fn assemble_after_preflight(
    mut cfg: RuntimeConfig,
    table: &crate::ArmingTable,
    preflight: &PreflightReport,
) -> Result<AssembledRuntime, RuntimeError> {
    log_preflight(preflight);
    enforce_preflight(preflight, &mut cfg.policy);
    assemble_runtime_inner(cfg, table)
}

/// Apply the report's per-venue dispositions: one `withhold_venue_credentials` per stopped venue on
/// the account directory the arms will read, and the venue in `AdapterPolicy::preflight_failed`, so its
/// `demo`/`live` accounts do NOT RUN this session with that reason (decision 0156 — never a paper
/// engine in their place). Every stop logs at `error` with the count: a silent stop is the failure
/// this lane exists to surface.
///
/// ⚠ ALREADY-absent credentials log `withheld == 0`: the preflight changed none and must not claim to.
fn enforce_preflight(report: &PreflightReport, policy: &mut crate::AdapterPolicy) {
    for venue in report.degraded_venues() {
        let (kept, withheld) = crate::startup::withhold_venue_credentials(&policy.accounts, &venue);
        policy.accounts = kept;
        tracing::error!(
            "startup preflight STOPPED {venue}'s demo and live accounts for this session \
             ({withheld} credential field(s) withheld) — a per-venue FAIL is confirmed evidence (the \
             venue answered and refused, or its clock is measurably out of band), and refused \
             keys are the same fact as absent ones; fix the cause in that venue's row above and \
             restart, or `vike-cli config set flags.preflight_skip true` to mount without any \
             preflight at all"
        );
        policy.preflight_failed.insert(venue);
    }
}

/// Log the report: one line per check (PASS `info`, WARN `warn`, FAIL `error`), plus a summary for a
/// global no-go or a degraded venue. Returns nothing the mount branches on.
///
/// ⚠ [`CheckStatus::NotApplicable`] logs at `info`, NOT `warn`: it is a DECLARED property ("no
/// server clock, because …"); as a warning on every healthy mount it taught operators to skim past
/// the clock rows.
pub(super) fn log_preflight(report: &PreflightReport) {
    if report.skipped {
        tracing::warn!(
            "startup preflight SKIPPED by the flags.preflight_skip row — no clock, credential or \
             network check ran before this mount"
        );
        return;
    }
    for (check, line) in report.checks.iter().zip(report.lines()) {
        match check.status {
            CheckStatus::NotApplicable | CheckStatus::Pass => tracing::info!("preflight: {line}"),
            CheckStatus::Warn => tracing::warn!("preflight: {line}"),
            CheckStatus::Fail => tracing::error!("preflight: {line}"),
        }
    }
    if !report.go() {
        tracing::error!(
            "startup preflight is a NO-GO (a process-wide check hard-failed) — mounting anyway, \
             but expect venue connections to fail"
        );
    }
    let degraded = report.degraded_venues();
    if !degraded.is_empty() {
        tracing::error!(
            "startup preflight FAILED for {degraded:?} — those venues' demo and live accounts do \
             NOT RUN this session (see the per-venue line each one logs next). A FAIL is confirmed \
             evidence: a probe that merely did not answer WARNs and changes nothing"
        );
    }
}

/// The assembly: ONE body shared by [`assemble_runtime_with_preflight`] and [`assemble_runtime`].
fn assemble_runtime_inner(
    cfg: RuntimeConfig,
    table: &crate::ArmingTable,
) -> Result<AssembledRuntime, RuntimeError> {
    // Live clients push fills/cancels/rejects here before the core (and its `EventSender`) exists;
    // the forwarder relays into core ingest after `spawn_core_multi`. In pure PAPER mode nothing
    // holds `live_events`, so the lane closes at the `drop` below and the forwarder exits.
    let (live_events, live_rx) = vike_exec::event_channel(4096);
    // ⚠ The forwarder runs from HERE, not from after the core is spawned: a lease that resumes from the
    // journal's cursor is replayed up to a ring of events the moment it opens, while the boot thread is
    // still assembling and waiting on the reply frames that same lease reader completes. Nothing drained
    // the 4096-slot lane before the core existed, so the reader blocked inside its delivery, the boot
    // thread's `AdapterStart` / `WireIds` replies sat unread behind it, the daemon's write timeout killed
    // the lease and the late-arm restart resumed from the same cursor: a restart loop. Until the core is
    // handed over ([`run_live_forwarder`]) the thread keeps the lane drained into a bounded backlog.
    let (core_door, core_door_rx) =
        std::sync::mpsc::channel::<(vike_exec::EventSender, vike_exec::TickSender)>();
    let forwarder_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = forwarder_stop.clone();
        std::thread::Builder::new()
            .name("live-event-forward".into())
            .spawn(move || run_live_forwarder(live_rx, &core_door_rx, &stop))
            .map_err(RuntimeError::ForwarderSpawn)?;
    }
    let mut live_venues: HashSet<String> = HashSet::new();

    // ⚠ BEFORE ANYTHING IS BUILT: a mount naming an account this box will not arm fails the node,
    // by name ([`refuse_unarmed_mount_accounts`]: why this refusal does not degrade).
    let mounts = runnable_accounts(&cfg.core_config);
    refuse_unarmed_mount_accounts(&cfg, table, &mounts)?;

    // The PRE-MOUNT armed set: the [`armed_live_venues`] answer the root claimed its
    // `vike_boot::live_lock` sentinels from, recomputed from the arms' own inputs and compared at the
    // tail ([`refuse_unarmed_live_venues`]: the lock's only guarantee the probe missed no arm).
    let markets = table.wired_markets();
    let armed = armed_live_venues(table, &cfg.policy);

    // SECOND-AND-LATER accounts; empty (assembly byte-identical) with one account per venue.
    let mut account_extras = AccountExtras::default();

    // The reconcile-trigger channel is built HERE, before any exec client: each live venue's resync
    // supervisor spawns synchronously inside `make_engine`, long before the caller's `spawn_recon`.
    // A `Sender` clone goes to every mounted venue, and the mount contract passes it on only to a
    // bridge whose declaration takes it (`crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s
    // `takes_recon_trigger` check); the pair is RETURNED in
    // `AssembledRuntime.recon_trigger` and `spawn_recon` ADOPTS it (`vike_tradehub_core::spawn_recon`'s doc), so every
    // trigger reaches the one driver thread. Gated on `cfg.recon_enabled` (the caller's reconcile-gate
    // verdict); off => `None` everywhere, no channel built.
    //
    // ⚠ The trigger is NOT the global on/off, so every mount's `AdapterEnv::recon_enabled` ALSO
    // carries `cfg.recon_enabled`: a bridge that takes no trigger gets `None` EVEN WITH
    // RECONCILIATION ON (periodic only). Inferring the gate from `recon_trigger.is_some()` would
    // silently stop reconciling them (`AdapterEnv::recon_enabled`'s doc).
    // wake signal: one `()` per bridge reconnect resync (backoff-limited); the driver drains the
    // whole queue per wake (`spawn_recon`), so a burst is one pass, and the payload is zero-size.
    let recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)> =
        cfg.recon_enabled.then(std::sync::mpsc::channel);
    let recon_trigger_tx = recon_trigger.as_ref().map(|(tx, _)| tx.clone());

    // Mount in TABLE order (the order `armed_live_venues` claimed locks in). Per-venue facts live on
    // `vike_model`'s `VENUE_WIRING` rows [`wired_markets`] reads: this crate names no venue.
    //
    // Debug-only: no venue or rank twice (`vike-model`'s table tests refuse both for the shipped
    // table; `in_engine_order` stays total either way). Held by
    // `crates/vike-venues/src/venue_wiring_tests.rs`'s `the_wiring_table_is_roster_complete`
    // and `the_core_holds_the_default_engines_in_rank_order`.
    debug_assert!(
        markets.iter().enumerate().all(|(i, a)| {
            markets[..i].iter().all(|b| b.venue != a.venue && b.engine_rank != a.engine_rank)
        }),
        "the mounted markets name a venue or an engine_rank twice: {:?}",
        markets
    );
    let mut mounted = Vec::with_capacity(markets.len());
    for m in &markets {
        let trigger = recon_trigger_tx.clone();
        // `None`: this venue's DEFAULT account does not run (decision 0156) — no engine at its
        // rank, and never a paper one in its place.
        if let Some(engine_and_recon) = runnable_accounts_of(
            (m.venue, m.symbol),
            &cfg,
            table,
            &live_events,
            &mut live_venues,
            trigger,
            &mut account_extras,
        )? {
            mounted.push((*m, engine_and_recon));
        }
    }
    // Reconcile handles, one per credentialed venue with a `ReconClient` (built in its bridge mount,
    // docs/decisions/0096), in ENGINE order; `None` (a `paper` account) dropped. The caller's
    // `recon_driver` consumes them after `feeds` exists (gated on `cfg.recon_enabled`).
    // ⚠ **Every leg built from `mounted` is that venue's DEFAULT account** (route key = venue id),
    // so `sole_account_of` gives `route_key: None` and the stamped payload is byte-identical to the
    // pre-accounts one. Extra accounts are appended at the end, carrying BOTH facts.
    //
    // ⚠ One string cannot be both the health-probe key and the routing key (keying extras by route
    // key alone WAS a bug): a leg carries both, `vike_tradehub_core::ReconLeg`'s doc argues both directions.
    let mut engines = Vec::new();
    let mut recon_clients: Vec<crate::ReconLeg> = Vec::new();
    // ...held in ENGINE order (the defaults that run), then the SECOND-AND-LATER accounts' engines
    // and handles (no-ops with one account per venue). The first is the primary.
    for (m, (engine, recon)) in in_engine_order(mounted) {
        engines.push((cfg.seed_cash, engine));
        if let Some(r) = recon {
            recon_clients.push(crate::ReconLeg::sole_account_of(m.venue, r));
        }
    }
    engines.extend(account_extras.engines);
    recon_clients.extend(account_extras.recon);
    let not_running = account_extras.not_running;
    // ⚠ AFTER THE BUILD, the half the projection could not predict: a strategy mount whose account
    // the probe armed and whose mount did not come up (a failed connect, a declined key) has no
    // engine, and the node is refused by name rather than `spawn_core_multi` panicking on it.
    {
        let keys: Vec<&str> = engines.iter().map(|(_, e)| e.route_key.as_str()).collect();
        refuse_mounts_without_an_engine(&mounts, &keys, &not_running)?;
    }
    // THE PRIMARY: the first engine in that order. A node in which NO account runs has none, and
    // the core cannot be built without one — refused, naming every account and why.
    let mut engines = engines.into_iter();
    let Some((_, primary)) = engines.next() else {
        return Err(RuntimeError::NoAccountRuns { not_running });
    };
    let extra: Vec<_> = engines.collect();
    // What a LATER hot start needs to know about the engines this core is spawned with (decision 0169):
    // which venues hold an engine, and under which route keys.
    let engine_keys: Vec<(String, String)> = std::iter::once(&primary)
        .chain(extra.iter().map(|(_, e)| e))
        .map(|e| (e.venue.clone(), e.route_key.clone()))
        .collect();
    // The currency bucket each engine's money is in, read while the engines are still in hand: a node
    // that mixes account currencies says so (its cross-engine totals add them as one unit).
    let ledger_ccys: Vec<(String, String)> = std::iter::once(&primary)
        .chain(extra.iter().map(|(_, e)| e))
        .map(|e| (e.route_key.clone(), ledger_ccy_key(e)))
        .collect();
    report_mixed_ledger_currencies(&ledger_ccys);
    // EVERY market of the wiring table, not only the ones this core holds an engine for (slice S3): a
    // venue the node mounted nothing for gets its first account's engine appended later, on the market
    // its default engine would have mounted. The static table, not this build's `markets`: whether the
    // venue's adapter exists is the FRESH arming table's word at the start, not the boot's.
    let wired: Vec<(String, String, Vec<String>)> = super::wired_markets()
        .iter()
        .map(|m| {
            (
                m.venue.to_string(),
                m.symbol.to_string(),
                declared_legs_for(&cfg.core_config, m.venue, m.symbol),
            )
        })
        .collect();
    // The venues whose startup preflight ran: every venue with an account armed at a venue-client tier. A
    // hot start on any other venue runs the preflight for that venue first (`HotAccounts::start_account`).
    // Per (venue, TIER): the preflight reads that tier's credentials and clock, so a venue that booted at
    // `demo` has not had its `live` side looked at. The same rows `armed_live_venues` reads.
    let preflighted: HashSet<(String, vike_model::AccountTier)> = markets
        .iter()
        .flat_map(|m| crate::venue_account_arming(table, m.venue, Some(&cfg.policy)))
        .filter_map(|row| {
            row.effective
                .filter(|t| *t != vike_model::AccountTier::Paper)
                .map(|t| (row.venue.to_string(), t))
        })
        .collect();
    // ⚠ …then EVERY account of a venue with several ENGINES NAMES ITSELF: `sole_account_of`'s
    // `route_key: None` is correct only for a one-engine venue. This keeps `None` unambiguous, so
    // `CoreThread::reconcile_reports` can REFUSE a `None` it cannot attribute (Class E).
    //
    // ⚠ **Hand in the ENGINES, not the legs (the 2026-09-14 correction).** The refusal counts
    // ENGINES (`CoreThread::engines_of_venue`), and [`runnable_accounts_of`] pushes
    // `AccountExtras::engines` always but `AccountExtras::recon` only on `Some`: a second armed
    // account whose recon client was `None` (failed ctrader/ibkr connect, ig/oanda/deribit handshake)
    // gave two engines and ONE leg, nothing was stamped, and the fold refused the surviving default
    // account's pass every interval for the process's life. Only here are both halves in hand.
    // No-op without an `[accounts]` table (`ReconLeg::name_accounts_of_shared_venues` argues it).
    //
    // ⚠ Spelled from `primary` + `extra` in `spawn_core_multi`'s order: that IS the core's engine
    // set, the pair `assemble_core` computes `CoreThread::multi_account` from. Borrow ends first.
    let engine_venues: Vec<&str> = std::iter::once(primary.venue.as_str())
        .chain(extra.iter().map(|(_, e)| e.venue.as_str()))
        .collect();
    crate::ReconLeg::name_accounts_of_shared_venues(&mut recon_clients, &engine_venues);
    // Drop our sender: the forwarder's `recv` ends when the last live client is torn down.
    drop(live_events);

    // THE ACCOUNTS THAT DO NOT RUN reach the core with their reasons: an order addressed to one is
    // refused by name (`vike_core::account_ambiguity`'s `unheld_account_refusal`), never routed to
    // another book and never filled by a simulator nobody chose (decision 0156).
    let mut core_config = cfg.core_config;
    core_config.not_running = not_running.iter().map(crate::NotRunning::view).collect();
    name_the_default_account_where_a_lane_twin_runs(
        &mut core_config,
        &engine_keys,
        &cfg.policy.accounts,
    );
    // A journaled engine whose label was the constructor's guess is restored under the currency its engine was
    // just told (`adopt_stated_currency_in_restored_engines`: the core would skip its restore otherwise).
    let held: Vec<&_> = std::iter::once(&primary).chain(extra.iter().map(|(_, e)| e)).collect();
    adopt_stated_currency_in_restored_engines(&mut core_config.restored_engines, &held);
    let core = vike_core::spawn_core_multi(primary, extra, core_config);
    // Forwarder: lane → core ingest; detached, exits when the lane closes. The core is handed over
    // now (a closed door is a forwarder that has already ended: nothing to relay).
    //
    // TEARDOWN SAFETY: `core.shutdown_and_join()` stops draining ingest, THEN joins the exec threads
    // → the live pumps, which `blocking_send` into `live_rx`. A forwarder still sending into the
    // undrained ingest lets `live_rx` fill, a pump blocks, and its join hangs forever (0xC…409 on
    // exit). So the caller raises `forwarder_stop` BEFORE core shutdown (while ingest still drains):
    // the forwarder then DRAINS-and-drops, every pump's send returns. Late fills at exit are dropped.
    let _ = core_door.send((core.event_sender(), core.tick_sender()));
    if !live_venues.is_empty() {
        tracing::warn!(
            "LIVE execution active (credential-gated demo) for: {live_venues:?} — real orders \
             will be placed on those venues' demo accounts"
        );
    }
    // THE LOCK BACKSTOP: a venue armed outside the locked set refuses the node. Runs on EVERY build
    // (a no-op on paper).
    refuse_unarmed_live_venues(&armed, &live_venues)?;

    let hot = HotAccounts {
        core_events: core.event_sender(),
        core_ticks: core.tick_sender(),
        forwarder_stop: forwarder_stop.clone(),
        seed_cash: cfg.seed_cash,
        recon_enabled: cfg.recon_enabled,
        recon_refused: false,
        recon_trigger: recon_trigger_tx,
        properties_rec: cfg.properties_rec.clone(),
        risk_profile: cfg.risk_profile.clone(),
        policy: cfg.policy.clone(),
        mounts,
        wired,
        engines: engine_keys,
        ledger_ccys,
        stopped: HashSet::new(),
        vars: cfg.vars.clone(),
        preflighted,
        recon_hot: None,
        hot_driver: None,
        first_balance_notice: None,
    };

    Ok(AssembledRuntime {
        handle: core,
        recon_clients,
        recon_trigger,
        live_venues,
        forwarder_stop,
        not_running,
        hot,
    })
}

/// The most events the forwarder keeps for a core that does not exist yet. Sized to the gatewayhub's
/// per-account replay ring (`GW_TRADE_RING_EVENTS`, 20 000) times a dozen accounts: a resume cannot
/// replay more than a ring per lease.
///
/// **Memory at the cap** (`tests` pin the arithmetic against the real sizes): each held event is an
/// `Ingest` inline in the `VecDeque` (`size_of::<Ingest>()`, 168 bytes by `vike_exec`'s size guard) plus,
/// for a gateway event, a `Box<GatewayEvent>` (an `Event` and a cursor, about 200 bytes) and the event's
/// strings: 250 000 x about 370 bytes is roughly 90 MB, 120 to 170 MB with the strings and the queue's last
/// doubling. It is freed when the backlog has been relayed (the queue is consumed by value).
///
/// **Beyond the cap the forwarder SHEDS, it does not stall** (W13c): it keeps draining the lane and drops
/// the venue events (`Ingest::Event`, `Ingest::GatewayEvent`) it has no room for, loudly, and an account that
/// lost one relays everything after it WITHOUT its ring cursor (a plain event), so the journal's resume
/// cursor for that account stays where the last kept event left it: a restart resumes from before the gap
/// (or is told the ring is gone and reconciles), and the startup reconcile pass that follows this boot
/// reads what the venue holds now. Stalling instead (the lane's producers blocking) parks the lease
/// reader, which is also the thread that completes the boot thread's `AdapterStart` / `WireIds` replies:
/// the boot thread times out, the lease is detached, and the late-arm restart resumes from the same
/// cursor, a restart loop. A link's own status (`ExecLink`, `StreamStatus`) is tiny and always kept.
pub(super) const PRECORE_BACKLOG_CAP: usize = 250_000;
/// How often the forwarder looks for the core while it has none (a std channel cannot be waited on
/// together with the lane; the wait is a boot, not a steady state).
const PRECORE_POLL: std::time::Duration = std::time::Duration::from_millis(2);

/// The body of the `live-event-forward` thread: drain the live exec lane from the moment it exists,
/// hold what arrives until `core_door` delivers the core's senders, then relay the backlog IN ORDER,
/// exactly once, and everything after it as it comes.
///
/// A door closed with no core (the assembly failed) ends the thread; so does a core that is gone.
/// `stop` is the caller's teardown flag: raised, the thread keeps draining and stops forwarding.
pub(super) fn run_live_forwarder(
    live_rx: tokio::sync::mpsc::Receiver<vike_exec::Ingest>,
    core_door: &std::sync::mpsc::Receiver<(vike_exec::EventSender, vike_exec::TickSender)>,
    stop: &std::sync::atomic::AtomicBool,
) {
    relay_with_cap(live_rx, core_door, stop, PRECORE_BACKLOG_CAP);
}

/// [`run_live_forwarder`] with the backlog bound injected, so the cap behaviour is testable without
/// holding a quarter of a million events.
pub(super) fn relay_with_cap(
    mut live_rx: tokio::sync::mpsc::Receiver<vike_exec::Ingest>,
    core_door: &std::sync::mpsc::Receiver<(vike_exec::EventSender, vike_exec::TickSender)>,
    stop: &std::sync::atomic::AtomicBool,
    cap: usize,
) {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::TryRecvError;
    use tokio::sync::mpsc::error::TryRecvError as LaneEmpty;
    use vike_exec::Ingest;

    let mut backlog = std::collections::VecDeque::new();
    // The accounts (ring route keys) that lost a venue event to the cap, and how many events were shed.
    let mut gapped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut shed: u64 = 0;
    let mut lane_closed = false;
    let (core_events, core_ticks) = loop {
        match core_door.try_recv() {
            Ok(core) => break core,
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        if lane_closed {
            std::thread::sleep(PRECORE_POLL);
            continue;
        }
        match live_rx.try_recv() {
            Ok(ing) => {
                let is_venue_event = matches!(ing, Ingest::Event(_) | Ingest::GatewayEvent(_));
                if backlog.len() >= cap && is_venue_event {
                    if shed == 0 {
                        tracing::error!(
                            cap,
                            "the live exec lane has more pre-core events than the boot backlog holds: \
                             events beyond it are DROPPED (the lane stays drained so the lease reader \
                             can still answer the boot thread), their accounts relay without a resume \
                             cursor, and the startup reconcile pass reads what the venue holds"
                        );
                    }
                    shed += 1;
                    if let Ingest::GatewayEvent(g) = &ing {
                        gapped.insert(g.route_key.to_string());
                    }
                } else {
                    backlog.push_back(ing);
                }
            }
            Err(LaneEmpty::Empty) => std::thread::sleep(PRECORE_POLL),
            Err(LaneEmpty::Disconnected) => lane_closed = true,
        }
    };
    if shed > 0 {
        tracing::error!(
            shed,
            accounts = ?gapped,
            "venue events that arrived before the core were DROPPED (backlog full): those accounts \
             relay without a resume cursor from here on, and the startup reconcile pass is what \
             recovers the state they carried"
        );
    }
    if !backlog.is_empty() {
        tracing::info!(events = backlog.len(), "relaying the events that arrived before the core");
    }
    for ing in backlog {
        if stop.load(Ordering::Relaxed) {
            continue; // teardown: the backlog is dropped like any late event
        }
        if forward_ingest(ing, &core_events, &core_ticks).is_err() {
            return; // core gone — stop relaying
        }
    }
    while let Some(ing) = live_rx.blocking_recv() {
        if stop.load(Ordering::Relaxed) {
            continue; // teardown: keep draining, stop forwarding
        }
        let relayed = match ing {
            // An account that lost an event to the cap: its later events carry no cursor, so the journal's
            // resume cursor for it never jumps over the gap.
            Ingest::GatewayEvent(g) if gapped.contains(g.route_key.as_str()) => {
                core_events.blocking_send(g.event)
            }
            other => forward_ingest(other, &core_events, &core_ticks),
        };
        if relayed.is_err() {
            break; // core gone — stop relaying
        }
    }
}

/// What the `live-event-forward` thread does with ONE message off the live exec lane: the venue
/// events (and the link's own status) go to the core on the lane they belong on; nothing else
/// ever arrives here, and what does is dropped, as before.
///
/// **EXHAUSTIVE on purpose, with no `_` arm.** The thread used to read `if let Ingest::Event(e)`
/// and drop every other variant without a sound, which is exactly how a journal cursor riding an
/// [`vike_exec::Ingest::GatewayEvent`] (journal v18) would have died here: the remote execution
/// client's reader sends its events with their cursors, and the core never sees the cursor if this
/// relay unwraps only `Event`. A variant added to `Ingest` is a compile error on this match until
/// somebody decides where it goes.
pub(super) fn forward_ingest(
    ing: vike_exec::Ingest,
    core_events: &vike_exec::EventSender,
    core_ticks: &vike_exec::TickSender,
) -> Result<(), vike_exec::CoreGone> {
    use vike_exec::Ingest;
    match ing {
        Ingest::Event(e) => core_events.blocking_send(e),
        // The cursor travels WITH the event, to the core's event lane: that is the one door that
        // journals the pair as one record.
        Ingest::GatewayEvent(g) => core_events.blocking_send_gateway(*g),
        // A link's own disclosure ("the trade link is down") reaches the strategies and the link
        // dead-man on the lane every status uses.
        Ingest::StreamStatus(s) => core_ticks.stream_status(*s),
        // The account's trade link went down or came back: the stuck-order watchdog defers while it
        // is down. It rides the event lane (the account-scoped door), and the payload names its
        // account itself.
        Ingest::ExecLink(u) => core_events.exec_link(*u),
        // An execution lane carries events and link status. These are other lanes' traffic (the
        // market data, the control plane, the core's own waker) and have never been relayed here.
        Ingest::Command(_)
        | Ingest::Market
        | Ingest::BarSeed(_)
        | Ingest::BarClose(_)
        | Ingest::Quote(_)
        | Ingest::Trade(_)
        | Ingest::Book(_)
        | Ingest::Watchdog
        | Ingest::Flow(_)
        | Ingest::DataDelay(_)
        // A change to the core's engine set is sent by the account watcher straight to the core's own
        // lane (`HotAccounts`), never through a venue lane.
        | Ingest::Accounts(_) => Ok(()),
    }
}
