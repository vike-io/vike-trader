//! Spawning the single-writer core thread: `spawn_core` and the cross-venue `spawn_core_multi`.

use super::*;

/// Spawn the single-writer core thread owning `engine`. The pinned contract in one place.
pub fn spawn_core<C: ExecutionClient + Send + 'static>(
    engine: Engine<C>,
    config: CoreConfig,
) -> CoreHandle {
    spawn_core_multi(engine, Vec::new(), config)
}

/// The route key of the PLACEHOLDER a core with no account holds at index 0 ([`spawn_core_without_accounts`]).
/// It ends in [`super::RETIRED_ROUTE_KEY_SUFFIX`] because the placeholder IS a stopped engine: a journaled copy of
/// it is skipped at INFO by the boot restore and names no re-admission target. No account carries it (a label is
/// `[A-Z0-9]+`), so no lookup by an account's route key ever reaches it.
pub const NO_ACCOUNT_ROUTE_KEY: &str = "no-account#stopped";

/// **Spawn a core in which NO account runs yet** (owner 2026-11-15, "1.a": a box whose every account failed at its
/// adapter start must not exit). Index 0 holds a PLACEHOLDER: an engine on no venue, under [`NO_ACCOUNT_ROUTE_KEY`],
/// with no capital (seed 0), STOPPED from birth (`assemble_core` tombstones it), so the core is in the state a
/// core whose primary account was stopped is in (decision 0169 item 7): an order that reaches it is refused
/// terminally (`ACCOUNT_NOT_RUNNING`), the published totals and the drawdown latch follow the first LIVE engine,
/// and every account joins by `AccountChange::AddEngine` (appended at index 1 on), its mounts by
/// `AccountChange::AddMounts`. `client` is the placeholder's: the core never sends it an order, so a no-op client
/// (`crate::replay::ExecutionClientJournalReplay`) serves; it fixes `C`, the type of the engines that join.
///
/// Nothing on the fold changes: the placeholder is a tombstone every per-event path already skips.
pub fn spawn_core_without_accounts<C: ExecutionClient + Send + 'static>(
    client: C,
    mut config: CoreConfig,
) -> CoreHandle {
    let mut placeholder = Engine::new(
        vike_execution::Account::new(1.0, "", None, BalanceMode::Delta),
        vike_execution::RiskGate::new(vike_model::RiskLimits::new()),
        client,
        "",
        "",
    );
    placeholder.route_key = NO_ACCOUNT_ROUTE_KEY.to_string();
    config.seed_cash = 0.0; // the primary's seed: the placeholder is no capital
    spawn_core_multi(placeholder, Vec::new(), config)
}

/// Cross-venue entry: ONE core thread driving the primary engine plus one engine per
/// extra venue — each `(seed_cash, engine)` keeps its own Account/RiskGate (the
/// CrossVenueDriver firewall, now structural in the runtime). Venue-tagged events and
/// `OrderIntent::Submit` route by venue; order-lifecycle replies route via the order client id map.
/// Use `C = Box<dyn ExecutionClient + Send>` for heterogeneous venue adapters.
/// Strategy mounts remain primary-venue in this slice (named next increment).
pub fn spawn_core_multi<C: ExecutionClient + Send + 'static>(
    mut engine: Engine<C>,
    mut extra_engines: Vec<(f64, Engine<C>)>,
    mut config: CoreConfig,
) -> CoreHandle {
    // Pre-warm the Symbol intern table off the hot path: the first interning builds the process
    // table (ustr's arena cost ~3.2 ms here; Symbol's is a lock and an empty map). Doing it here,
    // synchronously before the core thread spawns and before any venue feed connects, keeps that
    // cost off the first live fill/quote.
    vike_model::events::prewarm_interner();

    // applied-fill capture (Strategy::on_fill delivery) only when a strategy is mounted,
    // so a GUI-only engine never grows the buffer
    engine.collect_applied_fills = config.strategy.is_some() || !config.extra_mounts.is_empty();
    engine.equity_seed = config.seed_cash;
    // The ONE resolver-config source (`CoreConfig::price_config`), imposed on every engine's own
    // `price_config` so the exec-internal decision sites (the pre-trade gate) read the SAME knobs
    // the snapshot/sampler/watchdog pass explicitly. Like `equity_seed`, config-not-state:
    // re-imposed here on restore too.
    engine.price_config = config.price_config;
    // Same config-not-state discipline for the account mark-slot ownership windows (streamed AND
    // reconcile): the law lives in `Account::set_mark_from`, so both knobs have to reach every
    // Account the core folds.
    engine.account.set_mark_staleness_ms(config.mark_staleness_ms);
    engine.account.set_reconcile_staleness_ms(config.reconcile_mark_staleness_ms);
    // ...and the trade-id window each `Snapshot` keeps, from the node's reconcile read-back (`None`: each engine's own).
    if let Some(lookback_ms) = config.reconcile_lookback_ms {
        engine.size_fill_window(lookback_ms);
    }
    for (seed, e) in extra_engines.iter_mut() {
        e.equity_seed = *seed;
        e.price_config = config.price_config;
        e.account.set_mark_staleness_ms(config.mark_staleness_ms);
        e.account.set_reconcile_staleness_ms(config.reconcile_mark_staleness_ms);
        if let Some(lookback_ms) = config.reconcile_lookback_ms {
            e.size_fill_window(lookback_ms);
        }
    }
    let watchdog_timeout = config.submit_ack_timeout; // copy out before `config` moves into CoreThread
    let deadman_timeout = config.deadman.as_ref().map(|c| c.timeout); // ditto — waker cadence below
    // The LINK dead-man's grace joins the SAME waker fold below: an idle core (an armed venue whose
    // link died is, by construction, a core receiving nothing from it) must still reach the
    // drain-loop boundary on cadence, or the sweep that trips the switch never runs.
    let link_deadman_grace = config.link_deadman.as_ref().map(|c| c.grace);
    // …and so does the price-only sweep's interval (an idle core must still reach the boundary on cadence).
    let price_only_interval = config.price_only.as_ref().map(|c| c.sweep_interval);
    // Core-ergonomics: the managed-GTD sweep and the periodic portfolio snapshot ride the SAME
    // boundary waker (no thread of their own) — copied out here for the cadence fold below. The
    // portfolio one is included only when a journal exists, mirroring its arm condition exactly
    // (`arm_boundary_timers`): no journal ⇒ no timer ⇒ nothing to wake for.
    let gtd_interval = config.gtd_sweep;
    let portfolio_snap_interval = config.journal.as_ref().and(config.portfolio_snapshot_interval);
    let inflight_interval = config.inflight_confirm; // recon path-to-superset (F1-A): waker cadence below
    // steal/core-live-scheduler: the wall-clock schedule poll rides the SAME boundary waker (no
    // thread of its own). Its cadence is `schedule_poll` (default 1s), contributed to the waker ONLY
    // when some mount actually has a non-empty schedule (mirroring the journal-gated portfolio snap).
    let schedule_interval = if config.mount_schedules.values().any(|s| !s.is_empty()) {
        Some(config.schedule_poll.unwrap_or(Duration::from_millis(1000)))
    } else {
        None
    };
    let (tx, rx) = mpsc::channel::<Ingest>(config.ingest_capacity);
    let market = Arc::new(MarketConflation {
        state: Mutex::new(MarketConflationState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let rejected = Arc::new(AtomicU64::new(0));

    // The composition root's steps over the journaled books (`CoreConfig::restored_engine_steps`), opened where the
    // client type is known to be 'static, run here over the boot's books against the engines this core holds, and
    // kept for a late join's book (`late_join.rs`): one function at both.
    let restore_steps = super::late_join::open_restored_engine_steps::<C>(
        config.restored_engine_steps.take(),
        &mut config.restore_notes,
    );
    if let Some(run) = restore_steps {
        let engines: Vec<&Engine<C>> =
            std::iter::once(&engine).chain(extra_engines.iter().map(|(_, e)| e)).collect();
        run(&mut config.restored_engines, &engines);
    }
    let mut core = assemble_core(
        engine,
        extra_engines,
        config,
        Arc::clone(&market),
        Arc::clone(&snapshot),
        Arc::clone(&rejected),
    );
    // The one place the client type is known to be 'static: the door `Ingest::Accounts` is opened with.
    core.engine_door = Some(super::account_change::engine_of_parcel::<C>);
    core.restore_steps = restore_steps;
    // A held shared close wakes its own core at its grace's end (`shared_close.rs`); a WEAK sender, so it never
    // keeps the lane open (the waker below's linchpin).
    core.self_wake = Some(tx.downgrade());
    let journal_durable_watch = core.journal.as_ref().map(vike_journal::Journal::durable_watch);
    // The session is recorded BEFORE the core thread can mint an id under it (a schedule fires as soon as it runs).
    if let Some(record) = core.config.order_client_id_session_recorder.take() {
        let (session, sequence) = core.order_client_id_gen.state();
        record(&session, sequence);
    }
    let join = std::thread::Builder::new()
        .name("th-core".into())
        .spawn(move || {
            // Opt-in HFT pinning (VIKE_PIN_CORES=core:N): keep the single-writer hot hop on a fixed
            // core. No-op unless the env names the `core` role — the default desktop path.
            vike_execution::affinity::pin_current_thread(
                vike_execution::affinity::Role::Core,
                "th-core",
            );
            core.run(rx)
        })
        .expect("spawn th-core");

    // audit C3 + dead-man's switch: opt-in boundary WAKER thread. A WEAK sender is the linchpin —
    // it does NOT keep the ingest channel open, so the core's "every sender dropped -> break" clean
    // exit still fires; the timer self-exits when `upgrade()` returns None. A full queue (try_send
    // Err) just means the core is busy — the next tick retries. `Ingest::Watchdog` is a NO-OP
    // dispatch now (a pure waker: the stuck-order sweep AND the dead-man sweep both run at the
    // drain-loop boundary off the `TimerWheel`); the waker only guarantees an idle core
    // reaches that boundary on cadence. The tick is the SMALLEST half-timeout among the enabled
    // features (stuck-order watchdog and/or dead-man) so BOTH sweeps fire on time. Never spawned
    // when both are disabled (the default), so zero idle cost.
    let waker_tick = [
        watchdog_timeout,
        deadman_timeout,
        link_deadman_grace,
        price_only_interval,
        gtd_interval,
        portfolio_snap_interval,
        inflight_interval,
        schedule_interval,
    ]
    .into_iter()
    .flatten()
    .map(|t| (t / 2).max(Duration::from_millis(50)))
    .min();
    if let Some(tick) = waker_tick {
        let weak = tx.downgrade();
        std::thread::Builder::new()
            .name("th-core-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(tick);
                    match weak.upgrade() {
                        Some(tx) => {
                            // Full = a wake is already queued; Closed = shutting down.
                            let _ = tx.try_send(Ingest::Watchdog);
                        }
                        None => break,
                    }
                }
            })
            .expect("spawn th-core-watchdog");
    }

    CoreHandle { ingest: tx, market, snapshot, rejected, join, journal_durable_watch }
}
