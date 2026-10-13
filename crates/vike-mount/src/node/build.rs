//! The node assembly: `build_node`, its injected-preflight twin, and the preflight it enforces.

use std::collections::HashSet;

use crate::preflight::{CheckStatus, PreflightReport};

#[cfg(doc)]
use super::WiredMarket;
use super::accounts::{
    AccountExtras, armed_live_venues, mount_accounts, mount_accounts_of,
    refuse_mounts_without_an_engine, refuse_unarmed_live_venues, refuse_unarmed_mount_accounts,
};
use super::mounts::in_engine_order;
use super::{Node, NodeConfig, NodeError};

/// Build the live node: one engine per [`super::mounted_markets`] row ([`WiredMarket`]s, derived
/// from `vike_model`'s `VENUE_WIRING` per docs/decisions/0098), mounted in TABLE order and held in
/// [`WiredMarket::engine_rank`] order, the `recon_clients` list, `spawn_core_multi`, and the
/// live-event forwarder (the "dead-man" relay lane; a spawn failure is
/// `NodeError::ForwarderSpawn`).
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
/// account is listed in [`Node::not_running`] with its reason, handed to the core
/// (`vike_core::CoreConfig::not_running`, so an order addressed to it is refused by name), and a
/// strategy mount on one refuses the node ([`NodeError::Mount`]). A node in which NO account runs
/// is refused too ([`NodeError::NoAccountRuns`]): the core needs a primary engine.
///
/// # Panics
///
/// DEBUG builds only: on two rows naming one venue (default account mounted twice) or sharing an
/// [`WiredMarket::engine_rank`].
pub fn build_node(cfg: NodeConfig) -> Result<Node, NodeError> {
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
    let report =
        crate::startup::run_startup_preflight(cfg.registry, &cfg.vars, &dirs, Some(&cfg.policy));
    build_node_with_preflight(cfg, &report)
}

/// [`build_node`] over an ALREADY-RUN preflight report, so "a preflight failure never stops a
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
/// As [`build_node`]'s `# Panics`.
pub fn build_node_with_preflight(
    mut cfg: NodeConfig,
    preflight: &PreflightReport,
) -> Result<Node, NodeError> {
    log_preflight(preflight);
    enforce_preflight(preflight, &mut cfg.policy);
    build_node_inner(cfg)
}

/// Apply the report's per-venue dispositions: one `withhold_venue_credentials` per stopped venue on
/// the account directory the arms will read, and the venue in `MountPolicy::preflight_failed`, so its
/// `demo`/`live` accounts do NOT RUN this session with that reason (decision 0156 — never a paper
/// engine in their place). Every stop logs at `error` with the count: a silent stop is the failure
/// this lane exists to surface.
///
/// ⚠ ALREADY-absent credentials log `withheld == 0`: the preflight changed none and must not claim to.
fn enforce_preflight(report: &PreflightReport, policy: &mut crate::MountPolicy) {
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
fn log_preflight(report: &PreflightReport) {
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

/// The assembly: ONE body shared by [`build_node_with_preflight`] and [`build_node`].
fn build_node_inner(cfg: NodeConfig) -> Result<Node, NodeError> {
    // Live clients push fills/cancels/rejects here before the core (and its `EventSender`) exists;
    // the forwarder relays into core ingest after `spawn_core_multi`. In pure PAPER mode nothing
    // holds `live_events`, so the lane closes at the `drop` below and the forwarder exits.
    let (live_events, live_rx) = vike_exec::event_channel(4096);
    let mut live_venues: HashSet<String> = HashSet::new();

    // ⚠ BEFORE ANYTHING IS BUILT: a mount naming an account this box will not arm fails the node,
    // by name ([`refuse_unarmed_mount_accounts`]: why this refusal does not degrade).
    let mounts = mount_accounts(&cfg.core_config);
    refuse_unarmed_mount_accounts(&cfg, &mounts)?;

    // The PRE-MOUNT armed set: the [`armed_live_venues`] answer the root claimed its
    // `vike_boot::live_lock` sentinels from, recomputed from the arms' own inputs and compared at the
    // tail ([`refuse_unarmed_live_venues`]: the lock's only guarantee the probe missed no arm).
    let markets = super::mounted_markets(cfg.registry);
    let armed = armed_live_venues(cfg.registry, &cfg.vars, &cfg.policy);

    // SECOND-AND-LATER accounts; empty (assembly byte-identical) with one account per venue.
    let mut account_extras = AccountExtras::default();

    // The reconcile-trigger channel is built HERE, before any exec client: each live venue's resync
    // supervisor spawns synchronously inside `make_engine`, long before the caller's `spawn_recon`.
    // A `Sender` clone goes to every mounted venue, and the mount contract passes it on only to a
    // bridge whose declaration takes it (`crates/vike-mount/src/contract.rs`'s
    // `takes_recon_trigger` check); the pair is RETURNED in
    // `Node.recon_trigger` and `spawn_recon` ADOPTS it (`vike_mount::spawn_recon`'s doc), so every
    // trigger reaches the one driver thread. Gated on `cfg.recon_enabled` (the caller's reconcile-gate
    // verdict); off => `None` everywhere, no channel built.
    //
    // ⚠ The trigger is NOT the global on/off, so every mount's `MountEnv::recon_enabled` ALSO
    // carries `cfg.recon_enabled`: a bridge that takes no trigger gets `None` EVEN WITH
    // RECONCILIATION ON (periodic only). Inferring the gate from `recon_trigger.is_some()` would
    // silently stop reconciling them (`MountEnv::recon_enabled`'s doc).
    // wake signal: one `()` per bridge reconnect resync (backoff-limited); the driver drains the
    // whole queue per wake (`spawn_recon`), so a burst is one pass, and the payload is zero-size.
    let recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)> =
        cfg.recon_enabled.then(std::sync::mpsc::channel);
    let recon_trigger_tx = recon_trigger.as_ref().map(|(tx, _)| tx.clone());

    // Mount in TABLE order (the order `armed_live_venues` claimed locks in). Per-venue facts live on
    // `vike_model`'s `VENUE_WIRING` rows [`mounted_markets`] reads: this crate names no venue.
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
        if let Some(engine_and_recon) = mount_accounts_of(
            (m.venue, m.symbol),
            &cfg,
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
    // key alone WAS a bug): a leg carries both, `vike_mount::ReconLeg`'s doc argues both directions.
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
        return Err(NodeError::NoAccountRuns { not_running });
    };
    let extra: Vec<_> = engines.collect();
    // ⚠ …then EVERY account of a venue with several ENGINES NAMES ITSELF: `sole_account_of`'s
    // `route_key: None` is correct only for a one-engine venue. This keeps `None` unambiguous, so
    // `CoreThread::reconcile_reports` can REFUSE a `None` it cannot attribute (Class E).
    //
    // ⚠ **Hand in the ENGINES, not the legs (the 2026-09-14 correction).** The refusal counts
    // ENGINES (`CoreThread::engines_of_venue`), and [`mount_accounts_of`] pushes
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
    let core = vike_core::spawn_core_multi(primary, extra, core_config);
    // Forwarder: lane → core ingest; detached, exits when the lane closes.
    //
    // TEARDOWN SAFETY: `core.shutdown_and_join()` stops draining ingest, THEN joins the exec threads
    // → the live pumps, which `blocking_send` into `live_rx`. A forwarder still sending into the
    // undrained ingest lets `live_rx` fill, a pump blocks, and its join hangs forever (0xC…409 on
    // exit). So the caller raises `forwarder_stop` BEFORE core shutdown (while ingest still drains):
    // the forwarder then DRAINS-and-drops, every pump's send returns. Late fills at exit are dropped.
    let forwarder_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let core_events = core.event_sender();
        let stop = forwarder_stop.clone();
        let mut live_rx = live_rx;
        std::thread::Builder::new()
            .name("live-event-forward".into())
            .spawn(move || {
                while let Some(ing) = live_rx.blocking_recv() {
                    if let vike_exec::Ingest::Event(e) = ing {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            continue; // teardown: keep draining, stop forwarding (see above)
                        }
                        if core_events.blocking_send(e).is_err() {
                            break; // core gone — stop relaying
                        }
                    }
                }
            })
            .map_err(NodeError::ForwarderSpawn)?;
    }
    if !live_venues.is_empty() {
        tracing::warn!(
            "LIVE execution active (credential-gated demo) for: {live_venues:?} — real orders \
             will be placed on those venues' demo accounts"
        );
    }
    // THE LOCK BACKSTOP: a venue armed outside the locked set refuses the node. Runs on EVERY build
    // (a no-op on paper).
    refuse_unarmed_live_venues(&armed, &live_venues)?;

    Ok(Node {
        handle: core,
        recon_clients,
        recon_trigger,
        live_venues,
        forwarder_stop,
        not_running,
    })
}
