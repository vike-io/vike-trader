//! The core thread's drain loop (`CoreThread::run`), its per-message guard (`handle`) and `panic_text`.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    pub(crate) fn run(mut self, mut rx: mpsc::Receiver<Ingest>) {
        // Arm the boundary timer wheel (audit co6) BEFORE the loop, off the hot path. Gated on the
        // watchdog being enabled so that when it is OFF (default) NO clock read happens here — the
        // wheel stays empty, the boundary advance is skipped by a single `is_empty` check, and the
        // fold path (and, critically, the per-message clock-read COUNT that replay/harness clocks
        // depend on for deterministic `now_ms`) is byte-identical to a wheel-free runtime.
        if self.config.submit_ack_timeout.is_some()
            || self.config.deadman.is_some()
            || self.config.link_deadman.is_some()
            || self.config.gtd_sweep.is_some()
            || self.config.portfolio_snapshot_interval.is_some()
            || self.config.inflight_confirm.is_some()
        {
            self.arm_boundary_timers(self.config.clock.now_ms());
        }
        let mut last_pub =
            Instant::now().checked_sub(self.config.snapshot_interval).unwrap_or_else(Instant::now); // publish ASAP
        'outer: loop {
            // Never block with dirty state unpublished — this also covers the batch-max
            // boundary case (a burst of exactly batch_max messages then silence would
            // otherwise strand the final state until the next message).
            let first = match rx.try_recv() {
                Ok(msg) => msg,
                Err(mpsc::error::TryRecvError::Empty) => {
                    // leftover on_fill / on_order_event deliveries beyond the per-message round cap
                    // must not strand on a quiet feed — drain (guarded, like handle()) before parking
                    if !self.engine.applied_fills.is_empty()
                        || !self.engine.order_events.is_empty()
                        || self
                            .extra_engines
                            .iter()
                            .any(|(_, e)| !e.applied_fills.is_empty() || !e.order_events.is_empty())
                    {
                        if let Err(payload) =
                            catch_unwind(AssertUnwindSafe(|| self.dispatch_applied_fills()))
                        {
                            self.enter_safe_state(panic_text(payload));
                            self.drain_poisoned(); // audit C4 (safeguard)
                        }
                        self.drain_delivered();
                    }
                    if self.dirty {
                        self.publish_guarded();
                        last_pub = Instant::now();
                    }
                    match rx.blocking_recv() {
                        Some(msg) => msg,
                        None => break, // every sender dropped -> clean exit
                    }
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            };
            if self.handle(first).is_break() {
                break 'outer;
            }
            // opportunistic bounded drain (keeps per-wakeup latency bounded)
            for _ in 1..self.config.batch_max {
                match rx.try_recv() {
                    Ok(msg) => {
                        if self.handle(msg).is_break() {
                            break 'outer;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => break 'outer,
                }
            }
            // EQUITY SAMPLER (portfolio-observer PR-3): arm/disarm the cold equity-sample timer at
            // this SAME drain-loop boundary, before the wheel advances below — so an interval armed
            // this pass (a position that just opened in the batch just folded) is visible to
            // `drive_due_timers` immediately. `equity_sample` gates the whole block to a single
            // `Option::is_some()` read when disabled (the default): nothing is armed, `self.timers`
            // is never touched, and this boundary stays byte-identical to a sampler-free runtime.
            if self.config.equity_sample.is_some() {
                let now = self.config.clock.now_ms();
                self.maintain_equity_timer(now);
            }
            // PERIODIC STATE-SAVE (portfolio-observer PR-4 T3): arm/disarm the cold state-save
            // timer at this SAME drain-loop boundary, mirroring the equity sampler immediately
            // above — a mount that just got added this pass is visible to `drive_due_timers`
            // immediately. `state_save` gates the whole block to a single `Option::is_some()`
            // read when disabled (the default): nothing is armed, `self.timers` is never touched
            // by this feature, and this boundary stays byte-identical to a save-timer-free
            // runtime. (`state_dir` is checked inside `maintain_state_save_timer` itself, not
            // here, so both gates stay independently readable at their own call sites.)
            if self.config.state_save.is_some() {
                let now = self.config.clock.now_ms();
                self.maintain_state_save_timer(now);
            }
            // READINESS GATE (portfolio-observer PR-4 T5): probe still-Pending mounts at this SAME
            // drain-loop boundary — no timer-wheel entry needed (unlike the two blocks above, this
            // has no cadence of its own: it just re-checks the board every boundary pass while
            // anything is still Pending, and `maintain_mount_readiness` itself no-ops in O(1) the
            // moment nothing is). `readiness_gate` gates the whole block to a single `bool` read
            // when off (the default): no clock read, no scan, byte-identical to a gate-free runtime.
            if self.config.readiness_gate {
                let now = self.config.clock.now_ms();
                self.maintain_mount_readiness(now);
            }
            // WALL-CLOCK SCHEDULE (steal/core-live-scheduler): checked at this SAME drain-loop
            // boundary, the readiness-gate pattern — NO timer-wheel entry. Driving off the CLOCK
            // VALUE here (rather than a wheel deadline computed as `now + tick`) is load-bearing:
            // an injected/deterministic clock (replay, tests) may not advance between boundary
            // passes, and a wheel deadline computed from a frozen clock is unreachable until the
            // clock moves — which starves the poll to at most one check per clock movement (and the
            // establish-then-fire latch law consumes the first). The boundary waker (cadence fed by
            // `schedule_poll`) guarantees an idle core reaches this check on cadence; a rule fires
            // at the first boundary pass AT or AFTER its wall-clock instant. `any_mount_schedule`
            // gates the whole block to a single bool read when off (the default): no clock read, no
            // scan, byte-identical to a schedule-free runtime.
            if self.any_mount_schedule {
                let now = self.config.clock.now_ms();
                self.drive_schedule(now);
            }
            // DEADLINE TIMER WHEEL (audit co6): advance ONCE here, at the drain-loop boundary —
            // after the whole batch folded, NEVER per message (the p99<10µs fold above is
            // untouched). The `is_empty` gate makes this a single field read when no timer is armed
            // (the default: watchdog off ⇒ empty wheel), so the wheel-free path stays byte-identical.
            // When armed, this is where the stuck-order sweep (and, since PR-3, the equity
            // sampler, and since PR-4, the periodic strategy-state save) fires — driven by the
            // injected clock at the coalesced boundary, not by the `Ingest::Watchdog` message
            // (which is now only a waker; see its dispatch arm). The OS waker thread guarantees
            // the core wakes at least once per tick, so an idle core still reaches this boundary
            // on cadence.
            if !self.timers.is_empty() {
                let now = self.config.clock.now_ms();
                self.drive_due_timers(now);
            }
            // interval-coalesced publish while busy (idle publish happens above)
            if self.dirty && last_pub.elapsed() >= self.config.snapshot_interval {
                self.publish_guarded();
                last_pub = Instant::now();
            }
        }
        // Symmetric teardown + final snapshot so the GUI sees terminal state. Guarded:
        // a panicking client detach must still leave a published fault, not a dead cell.
        if catch_unwind(AssertUnwindSafe(|| {
            self.drain_market(); // fold any tick stranded behind Shutdown in the queue
            self.dispatch_applied_fills(); // pending on_fill deliveries while the client is attached
            // OPT-IN SHUTDOWN POLICY, and it must run HERE — this is the last instant at which the
            // core still owns live engines AND the client is still attached, so it is the only
            // point a cancel can still reach the venue. Everything after this guard is teardown:
            // phase one below raises every client's stop flag and phase two `shutdown()`s them,
            // and the same sweep fired into a client whose flag is already up would go nowhere.
            // OFF by default ⇒ one bool read, and the teardown stays byte-identical to the
            // version that had no such field.
            if self.config.cancel_orders_on_shutdown {
                self.cancel_resting_on_shutdown();
            }
            // TWO PHASES, and the split is the whole cost model — the exec-plane twin of
            // `crates/vike-recorder/src/runtime.rs`'s `stop_all`, whose doc argues the same finding
            // one plane over.
            //
            // `ExecutionEngine::shutdown` detaches the venue client, and a venue's detach JOINS its
            // user-data pump — a thread that learns it should stop on its next stop-flag poll
            // (`crates/bridges/binance/src/family/listenkey.rs`'s `POLL` is 1s, and the recv loop
            // in `vike_bridge_core::user_data::run_user_data_forever_with_idle` re-checks the flag only on that
            // cadence). The one-loop version of this teardown — detach-and-join, one engine at a
            // time — therefore cost ONE wind-down PER ENGINE, serially, on the shutdown path of
            // every shipped binary: a twelve-venue mount paid about twelve of them while a
            // service-managed stop was already counting against its unit's `TimeoutStopSec=`.
            //
            // PHASE 1 — raise every flag, join nothing. This must complete for EVERY engine before
            // the first `shutdown()` below, or the parallelism it exists for is given back one
            // venue at a time. `begin_shutdown` is a default no-op on any client that has not
            // wired it, so this loop is free where it buys nothing.
            self.engine.begin_shutdown();
            for (_, e) in self.extra_engines.iter_mut() {
                e.begin_shutdown();
            }
            // PHASE 2 — the exact detaches that shipped before, now joining threads that have all
            // been winding down since phase one. Nothing else about this teardown moves: no engine
            // state is touched by phase one, the order below is unchanged, and a single-engine
            // mount behaves as it always did.
            //
            // ⚠ It does NOT race the venue events phase one stops sooner. The fold loop has already
            // broken by the time we get here, and nothing in this teardown drains the venue ingest
            // lane — `drain_market` above drains MARKET data and `pump_client` below polls
            // IN-PROCESS clients only. A venue event arriving during the old code's serial join
            // window was folded by nobody either.
            self.engine.shutdown();
            for (_, e) in self.extra_engines.iter_mut() {
                e.shutdown();
            }
            self.pump_client();
            self.dispatch_applied_fills(); // fills the final pump surfaced
            self.drain_delivered();
        }))
        .is_err()
        {
            self.engine.trading_state = TradingState::Halted;
            if self.fault.is_none() {
                self.fault = Some("panic during teardown".to_string());
            }
            self.drain_poisoned(); // audit C4 (safeguard)
        }
        // Final Snap on exit — Shutdown, senders-dropped, and disconnect all funnel here. This
        // always-present tail Snap is what a clean restart restores from (replay is latest-Snap-
        // wins). `engine.shutdown()` above only detaches the client (OMS state is unchanged), so
        // this captures the fully-folded terminal state. No-op when journaling is off, so the
        // is_some guard keeps the teardown byte-identical to today when disabled.
        if self.journal.is_some() {
            let now = self.config.clock.now_ms();
            self.write_snap(now);
        }
        // Portfolio-observer PR-4 T2/T3: best-effort save of each live mount's durable-state
        // sidecar on a clean shutdown — the SAME per-mount loop the periodic
        // [`TimerKind::StateSave`] fire uses (T3), factored into `save_all_strategy_state` so the
        // two call sites can't drift apart (DRY; see that method's doc for the full guard
        // rationale — catch_unwind per mount, indexed so a panicked mount can't shift
        // `mount_ids` alignment, write failures logged and swallowed). `None` `state_dir`
        // (default) skips this entirely — byte-identical teardown to today.
        if self.config.state_dir.is_some() {
            self.save_all_strategy_state();
        }
        self.publish_guarded();
    }

    /// Dispatch one message under the panic policy. Hooks run inside the guard too — no
    /// user-supplied code on this thread can unwind past run() unsurfaced.
    fn handle(&mut self, msg: Ingest) -> std::ops::ControlFlow<()> {
        if let Ingest::Command(Command::Shutdown) = msg {
            return std::ops::ControlFlow::Break(()); // teardown runs in run()
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if let Some(hook) = self.config.on_dequeued.as_mut() {
                hook(&msg);
            }
            self.dispatch(msg);
            // deliver any fills this message's fold accepted (venue lane, client pumps)
            self.dispatch_applied_fills();
        }));
        match outcome {
            Ok(()) => {}
            Err(payload) => {
                self.enter_safe_state(panic_text(payload));
                self.drain_poisoned(); // audit C4: don't lose an event the panic interrupted mid-fold
            }
        }
        self.drain_delivered();
        std::ops::ControlFlow::Continue(())
    }
}

pub(crate) fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic (non-string payload)".to_string()
    }
}
