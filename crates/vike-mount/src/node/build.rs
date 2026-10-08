//! The node assembly: `build_node`, its injected-preflight twin, and the preflight it enforces.

use std::collections::{HashMap, HashSet};

use crate::preflight::{CheckStatus, PREFLIGHT_SKIP_ENV, PreflightReport};

#[cfg(doc)]
use super::WiredMarket;
use super::accounts::{
    AccountExtras, armed_live_venues, mount_accounts, mount_accounts_of,
    refuse_unarmed_live_venues, refuse_unarmed_mount_accounts,
};
use super::mounts::in_engine_order;
use super::{Node, NodeConfig, NodeError};

/// Build the live node: one engine per [`NodeConfig::markets`] row ([`WiredMarket`]s, from the
/// composition root per docs/decisions/0098), mounted in TABLE order and held in
/// [`WiredMarket::engine_rank`] order, the `recon_clients` list, `spawn_core_multi`, and the
/// live-event forwarder (the "dead-man" relay lane; a spawn failure is
/// `NodeError::ForwarderSpawn`).
///
/// STARTUP PREFLIGHT: [`crate::startup::run_startup_preflight`] runs FIRST, before any venue is
/// mounted, so every caller gets the same "before the first live order" gate from one site. No
/// network with no credentials — **nor with a full store under an all-`paper` ceiling** (why
/// `cfg.policy` is threaded in); with armed credentials, a bounded DNS round plus one clock read and
/// one signed balance read per armed venue.
///
/// # Panics
///
/// On an EMPTY [`NodeConfig::markets`] (the core needs a primary engine; as
/// [`crate::build_live_multi_strategy_core`] on empty `mounts`). DEBUG builds only: on two rows
/// naming one venue (default account mounted twice) or sharing an [`WiredMarket::engine_rank`].
pub fn build_node(cfg: NodeConfig) -> Result<Node, NodeError> {
    // The disk leg watches the journal dir the caller ALREADY resolved into `CoreConfig` (profile
    // `[sinks.journal]` or `VIKE_JOURNAL_DIR`; only the caller knows which won) — re-resolving would
    // be a competing authority. No journal ⇒ no disk row.
    let dirs: Vec<(String, std::path::PathBuf)> = cfg
        .core_config
        .journal
        .as_ref()
        .map(|j| vec![("journal".to_string(), j.dir.clone())])
        .unwrap_or_default();
    // ⚠ THE ARMING CEILING reaches the preflight here — the same `cfg.policy` every `make_engine`
    // gets. Without it the preflight went by credential PRESENCE and authenticated against venues
    // capped to `paper` (`crate::startup`'s module doc).
    let report =
        crate::startup::run_startup_preflight(cfg.registry, &cfg.vars, &dirs, Some(&cfg.policy));
    build_node_with_preflight(cfg, &report)
}

/// [`build_node`] over an ALREADY-RUN preflight report, so "a preflight failure never stops a
/// mount" is testable with a hard-failing report and no network.
///
/// The report is **ENFORCED** per venue:
/// [`PreflightReport::venue_disposition`](crate::preflight::PreflightReport::venue_disposition)
/// is `Paper` on a hard FAIL, and [`enforce_preflight`] withholds that venue's credentials.
///
/// ⚠ **Why enforcing is safe (it was advisory):** a transient timeout must not turn live into paper.
/// `crate::preflight::CredentialGap` splits "answered and refused" (`Fail`, demotes) from "never
/// heard back" (`Warn`, never demotes), bounded by `crate::startup::CREDENTIAL_PROBE_TIMEOUT`; and
/// refused keys are the same fact as ABSENT keys, which already mount paper.
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
    enforce_preflight(preflight, &mut cfg.vars);
    build_node_inner(cfg)
}

/// Apply the report's per-venue dispositions to the credential map the arms will read: one
/// `withhold_venue_credentials` per degraded venue, so `make_engine` mounts its paper fallback
/// through the ordinary live gate (no new branch). Every demotion logs at `error` with the count:
/// a silent live→paper switch is the failure this lane exists to surface.
///
/// ⚠ ALREADY-absent keys log `withheld == 0`: the preflight changed nothing and must not claim to.
fn enforce_preflight(report: &PreflightReport, vars: &mut HashMap<String, String>) {
    for venue in report.degraded_venues() {
        let withheld = crate::startup::withhold_venue_credentials(vars, &venue);
        tracing::error!(
            "startup preflight DEGRADED {venue} to PAPER for this session ({withheld} credential \
             key(s) withheld) — a per-venue FAIL is confirmed evidence (the venue answered and \
             refused, or its clock is measurably out of band), and refused keys are the same fact \
             as absent ones; fix the cause in that venue's row above and restart, or set \
             {PREFLIGHT_SKIP_ENV}=1 to mount without any preflight at all"
        );
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
            "startup preflight SKIPPED by {PREFLIGHT_SKIP_ENV}=1 — no clock, credential or network \
             check ran before this mount"
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
            "startup preflight FAILED for {degraded:?} — those venues ARE demoted to paper for \
             this session (see the per-venue line each one logs next). A FAIL is confirmed \
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
    // `vike_ops::live_lock` sentinels from, recomputed from the arms' own inputs and compared at the
    // tail ([`refuse_unarmed_live_venues`]: the lock's only guarantee the probe missed no arm).
    let armed = armed_live_venues(cfg.registry, cfg.markets, &cfg.vars, &cfg.policy);

    // SECOND-AND-LATER accounts; empty (assembly byte-identical) with one account per venue.
    let mut account_extras = AccountExtras::default();

    // The reconcile-trigger channel is built HERE, before any exec client: each live venue's resync
    // supervisor spawns synchronously inside `make_engine`, long before the caller's `spawn_recon`.
    // A `Sender` clone goes to every venue whose row sets `reconnect_poke` and whose bridge declares
    // `takes_recon_trigger` ([`WiredMarket::reconnect_poke`]); the pair is RETURNED in
    // `Node.recon_trigger` and `spawn_recon` ADOPTS it (`vike_core::spawn_recon`'s doc), so every
    // poke reaches the one driver thread. Gated on `cfg.recon_enabled` (the caller's reconcile-gate
    // verdict); off => `None` everywhere, no channel built.
    //
    // ⚠ The trigger is NOT the global on/off, so every mount's `MountEnv::recon_enabled` ALSO
    // carries `cfg.recon_enabled`: rows without `reconnect_poke` get `None` EVEN WITH
    // RECONCILIATION ON (periodic only). Inferring the gate from `recon_trigger.is_some()` would
    // silently stop reconciling them (`MountEnv::recon_enabled`'s doc).
    let recon_trigger: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)> =
        cfg.recon_enabled.then(std::sync::mpsc::channel);
    let recon_trigger_tx = recon_trigger.as_ref().map(|(tx, _)| tx.clone());

    // Mount in TABLE order (the order `armed_live_venues` claimed locks in). Per-venue facts live on
    // the rows in `crates/vike-tradehub/src/wired_markets.rs`: this crate names no venue.
    //
    // Debug-only: no venue or rank twice (`vike-tradehub`'s table tests refuse both for the shipped
    // table; `in_engine_order` stays total either way). Held by `crates/vike-mount/src/node_tests.rs`'s
    // `a_table_naming_a_venue_twice_panics_in_a_debug_build` and
    // `a_table_sharing_an_engine_rank_panics_in_a_debug_build`.
    debug_assert!(
        cfg.markets.iter().enumerate().all(|(i, a)| {
            cfg.markets[..i].iter().all(|b| b.venue != a.venue && b.engine_rank != a.engine_rank)
        }),
        "NodeConfig::markets names a venue or an engine_rank twice: {:?}",
        cfg.markets
    );
    let mut mounted = Vec::with_capacity(cfg.markets.len());
    for m in cfg.markets {
        let poke = if m.reconnect_poke { recon_trigger_tx.clone() } else { None };
        let engine_and_recon = mount_accounts_of(
            (m.venue, m.symbol),
            &cfg,
            &live_events,
            &mut live_venues,
            poke,
            &mut account_extras,
        )?;
        mounted.push((*m, engine_and_recon));
    }
    // ...and hold them in ENGINE order. The first is the primary.
    let mut held = in_engine_order(mounted).into_iter();
    let (primary_market, (primary, primary_recon)) = held.next().expect(
        "NodeConfig::markets is empty: the composition root's table names at least its primary venue",
    );
    // Reconcile handles, one per credentialed venue with a `ReconClient` (built in its bridge mount,
    // docs/decisions/0096), in ENGINE order; `None` (paper / no creds) dropped. The caller's
    // `recon_driver` consumes them after `feeds` exists (gated on `cfg.recon_enabled`).
    // ⚠ **Every leg built here is that venue's DEFAULT account** (route key = venue id), so
    // `sole_account_of` gives `route_key: None` and the stamped payload is byte-identical to the
    // pre-accounts one. Extra accounts are appended at the end, carrying BOTH facts.
    //
    // ⚠ One string cannot be both the health-probe key and the routing key (keying extras by route
    // key alone WAS a bug): a leg carries both, `vike_core::ReconLeg`'s doc argues both directions.
    let mut extra = Vec::new();
    let mut recon_clients: Vec<vike_core::ReconLeg> = Vec::new();
    if let Some(r) = primary_recon {
        recon_clients.push(vike_core::ReconLeg::sole_account_of(primary_market.venue, r));
    }
    for (m, (engine, recon)) in held {
        extra.push((cfg.seed_cash, engine));
        if let Some(r) = recon {
            recon_clients.push(vike_core::ReconLeg::sole_account_of(m.venue, r));
        }
    }
    // …then the SECOND-AND-LATER accounts' engines and handles (no-ops with one account per venue).
    extra.extend(account_extras.engines);
    recon_clients.extend(account_extras.recon);
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
    vike_core::ReconLeg::name_accounts_of_shared_venues(&mut recon_clients, &engine_venues);
    // Drop our sender: the forwarder's `recv` ends when the last live client is torn down.
    drop(live_events);

    let core = vike_core::spawn_core_multi(primary, extra, cfg.core_config);
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

    Ok(Node { handle: core, recon_clients, recon_trigger, live_venues, forwarder_stop })
}
