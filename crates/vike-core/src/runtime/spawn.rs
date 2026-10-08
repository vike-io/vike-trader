//! Spawning the single-writer core thread: `spawn_core` and the cross-venue `spawn_core_multi`.

use super::*;

/// Spawn the single-writer core thread owning `engine`. The pinned contract in one place.
pub fn spawn_core<C: ExecutionClient + Send + 'static>(
    engine: ExecutionEngine<C>,
    config: CoreConfig,
) -> CoreHandle {
    spawn_core_multi(engine, Vec::new(), config)
}

/// Cross-venue entry: ONE core thread driving the primary engine plus one engine per
/// extra venue — each `(seed_cash, engine)` keeps its own Account/RiskGate (the
/// CrossVenueDriver firewall, now structural in the runtime). Venue-tagged events and
/// `OrderIntent::Submit` route by venue; order-lifecycle replies route via the coid map.
/// Use `C = Box<dyn ExecutionClient + Send>` for heterogeneous venue adapters.
/// Strategy mounts remain primary-venue in this slice (named next increment).
pub fn spawn_core_multi<C: ExecutionClient + Send + 'static>(
    mut engine: ExecutionEngine<C>,
    mut extra_engines: Vec<(f64, ExecutionEngine<C>)>,
    config: CoreConfig,
) -> CoreHandle {
    // Pre-warm the ustr intern table off the hot path: the first Ustr construction builds the
    // global table (~3.2 ms one-time). Doing it here, synchronously before the core thread spawns
    // and before any venue feed connects, keeps that cost off the first live fill/quote.
    vike_model::events::prewarm_interner();

    // applied-fill capture (Strategy::on_fill delivery) only when a strategy is mounted,
    // so a GUI-only engine never grows the buffer
    engine.collect_applied_fills = config.strategy.is_some() || !config.extra_mounts.is_empty();
    engine.equity_seed = config.seed_cash;
    // The ONE resolver-config source (`CoreConfig::price_cfg`), imposed on every engine's own
    // `price_cfg` so the exec-internal decision sites (the pre-trade gate) read the SAME knobs
    // the snapshot/sampler/watchdog pass explicitly. Like `equity_seed`, config-not-state:
    // re-imposed here on restore too.
    engine.price_cfg = config.price_cfg;
    // Same config-not-state discipline for the account mark-slot ownership windows (streamed AND
    // reconcile): the law lives in `Account::set_mark_from`, so both knobs have to reach every
    // Account the core folds.
    engine.account.set_mark_staleness_ms(config.mark_staleness_ms);
    engine.account.set_reconcile_staleness_ms(config.reconcile_mark_staleness_ms);
    for (seed, e) in extra_engines.iter_mut() {
        e.equity_seed = *seed;
        e.price_cfg = config.price_cfg;
        e.account.set_mark_staleness_ms(config.mark_staleness_ms);
        e.account.set_reconcile_staleness_ms(config.reconcile_mark_staleness_ms);
    }
    let watchdog_timeout = config.submit_ack_timeout; // copy out before `config` moves into CoreThread
    let deadman_timeout = config.deadman.as_ref().map(|c| c.timeout); // ditto — waker cadence below
    // The LINK dead-man's grace joins the SAME waker fold below: an idle core (an armed venue whose
    // link died is, by construction, a core receiving nothing from it) must still reach the
    // drain-loop boundary on cadence, or the sweep that trips the switch never runs.
    let link_deadman_grace = config.link_deadman.as_ref().map(|c| c.grace);
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
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    let rejected = Arc::new(AtomicU64::new(0));

    let core = assemble_core(
        engine,
        extra_engines,
        config,
        Arc::clone(&market),
        Arc::clone(&snapshot),
        Arc::clone(&rejected),
    );
    let join = std::thread::Builder::new()
        .name("vt-core".into())
        .spawn(move || {
            // Opt-in HFT pinning (VIKE_PIN_CORES=core:N): keep the single-writer hot hop on a fixed
            // core. No-op unless the env names the `core` role — the default desktop path.
            vike_exec::affinity::pin_current_thread(vike_exec::affinity::Role::Core, "vt-core");
            core.run(rx)
        })
        .expect("spawn vt-core");

    // audit C3 + dead-man's switch: opt-in boundary WAKER thread. A WEAK sender is the linchpin —
    // it does NOT keep the ingest channel open, so the core's "every sender dropped -> break" clean
    // exit still fires; the timer self-exits when `upgrade()` returns None. A full queue (try_send
    // Err) just means the core is busy — the next tick retries. `Ingest::Watchdog` is a NO-OP
    // dispatch now (a pure waker: the stuck-order sweep AND the dead-man sweep both run at the
    // drain-loop boundary off the `DeadlineTimerWheel`); the waker only guarantees an idle core
    // reaches that boundary on cadence. The tick is the SMALLEST half-timeout among the enabled
    // features (stuck-order watchdog and/or dead-man) so BOTH sweeps fire on time. Never spawned
    // when both are disabled (the default), so zero idle cost.
    let waker_tick = [
        watchdog_timeout,
        deadman_timeout,
        link_deadman_grace,
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
            .name("vt-core-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(tick);
                    match weak.upgrade() {
                        Some(tx) => {
                            let _ = tx.try_send(Ingest::Watchdog);
                        }
                        None => break,
                    }
                }
            })
            .expect("spawn vt-core-watchdog");
    }

    CoreHandle { ingest: tx, market, snapshot, rejected, join }
}
