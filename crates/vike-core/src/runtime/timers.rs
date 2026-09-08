//! Drain-loop boundary timers — the [`DeadlineTimerWheel`] arm/advance, the equity sampler, the
//! periodic strategy-state save, and the per-mount readiness gate/view — split out of the runtime
//! fold module (behavior byte-identical; the block moved verbatim). `use super::*` re-exports the
//! parent runtime module's full import set + items, so nothing about resolution changes.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Arm the drain-loop boundary timers (audit co6). Called ONCE at [`Self::run`] startup, off the
    /// hot path. Only the stuck-order watchdog is wired this increment: when `submit_ack_timeout` is
    /// set, a self-rescheduling [`TimerKind::StuckSweep`] is armed at `now + tick` (where `tick =
    /// submit_ack_timeout / 2`, clamped `>= 50ms`, mirroring the OS waker's cadence). When the
    /// watchdog is disabled (default) NOTHING is armed — the wheel stays empty and the boundary
    /// advance is skipped by a single `is_empty` check, so the fold path is byte-identical to today.
    ///
    /// The DEAD-MAN'S SWITCH (trading-hardening) arms its own self-rescheduling
    /// [`TimerKind::DeadManSweep`] here on the SAME pattern — cadence `deadman.timeout / 2` clamped
    /// `>= 50ms` — but ONLY when [`CoreConfig::deadman`] is `Some` (`self.deadman` built). Both
    /// features are independent: either, both, or neither may be armed; when neither is set the wheel
    /// stays empty and byte-identical to today.
    pub(crate) fn arm_boundary_timers(&mut self, now_ms: i64) {
        if let Some(timeout) = self.config.submit_ack_timeout {
            let tick = (timeout / 2).max(Duration::from_millis(50)).as_millis() as i64;
            self.watchdog_tick_ms = tick.max(1);
            self.timers.insert(now_ms.saturating_add(self.watchdog_tick_ms), TimerKind::StuckSweep);
        }
        if let Some(dm) = self.deadman.as_ref() {
            // cadence = half the staleness threshold, clamped `>= 50ms` (mirrors the watchdog + the
            // OS waker), so the switch evaluates freshness at least twice per timeout window.
            let tick = (dm.threshold_ms() / 2).clamp(50, i64::MAX);
            self.deadman_tick_ms = tick.max(1);
            self.timers
                .insert(now_ms.saturating_add(self.deadman_tick_ms), TimerKind::DeadManSweep);
        }
        // THE CONNECTION-STATE DEAD-MAN (M13) — the same self-rescheduling pattern on its OWN
        // cadence (`grace / 2`, so a link's grace is evaluated at least twice before it expires),
        // armed ONLY when `CoreConfig::link_deadman` is `Some`. Independent of the switch above:
        // either, both or neither may be armed.
        if let Some(ldm) = self.link_deadman.as_ref() {
            let tick = (ldm.grace_ms() / 2).clamp(50, i64::MAX);
            self.link_deadman_tick_ms = tick.max(1);
            self.timers.insert(
                now_ms.saturating_add(self.link_deadman_tick_ms),
                TimerKind::LinkDeadManSweep,
            );
        }
        // MANAGED GTD EXPIRY (core-ergonomics) — same self-rescheduling pattern, armed ONLY when
        // `gtd_sweep` is opted in. The cadence IS the configured interval (unlike the watchdog's
        // half-timeout: this one is not backstopping another timeout, it IS the check rate).
        if let Some(interval) = self.config.gtd_sweep {
            self.gtd_tick_ms = (interval.as_millis() as i64).max(1);
            self.timers.insert(now_ms.saturating_add(self.gtd_tick_ms), TimerKind::GtdSweep);
        }
        // PERIODIC PORTFOLIO SNAPSHOT (core-ergonomics) — armed only when BOTH the interval and a
        // journal to write into are configured (see `CoreConfig::portfolio_snapshot_interval`).
        if let Some(interval) = self.config.portfolio_snapshot_interval
            && self.journal.is_some()
        {
            self.portfolio_snap_tick_ms = (interval.as_millis() as i64).max(1);
            self.timers.insert(
                now_ms.saturating_add(self.portfolio_snap_tick_ms),
                TimerKind::PortfolioSnap,
            );
        }
        // FAST IN-FLIGHT CONFIRM (recon path-to-superset, F1-A) — same self-rescheduling pattern,
        // armed ONLY when `inflight_confirm` is opted in. Like `gtd_sweep`, the cadence IS the
        // configured interval (it is not backstopping another timeout, it IS the check rate).
        if let Some(interval) = self.config.inflight_confirm {
            self.inflight_confirm_tick_ms = (interval.as_millis() as i64).max(1);
            self.timers.insert(
                now_ms.saturating_add(self.inflight_confirm_tick_ms),
                TimerKind::InflightConfirm,
            );
        }
    }

    /// Drive any timers due at `now_ms` — the ONE place the [`DeadlineTimerWheel`] is advanced, at
    /// the drain-loop boundary (once per drain batch), NEVER per message. The caller has already
    /// gated on `!self.timers.is_empty()`, so this runs only when a timer is armed (the watchdog).
    ///
    /// `now_ms` (the injected clock, read once at the boundary) is stamped onto every engine so a
    /// fired sweep sees exactly the timestamp semantics the old per-message `Ingest::Watchdog`
    /// dispatch gave it (which stamped `engine.now_ms` at dispatch top). The next dispatched message
    /// re-stamps `now_ms` regardless, so nothing stale leaks forward. A fired [`TimerKind::StuckSweep`]
    /// runs the unchanged [`Self::sweep_stuck_orders`] and re-arms the next sweep at `now + tick`
    /// (skipping missed ticks rather than bursting catch-up sweeps after a long idle).
    pub(crate) fn drive_due_timers(&mut self, now_ms: i64) {
        self.due_timers.clear();
        self.timers.advance(now_ms, &mut self.due_timers);
        if self.due_timers.is_empty() {
            return;
        }
        self.engine.now_ms = now_ms;
        for (_, e) in self.extra_engines.iter_mut() {
            e.now_ms = now_ms;
        }
        // pop (Copy) so we never borrow the buffer while acting on `self` in the loop body
        while let Some(kind) = self.due_timers.pop() {
            match kind {
                TimerKind::StuckSweep => {
                    self.sweep_stuck_orders();
                    // re-arm the self-rescheduling cadence timer for the next boundary
                    self.timers.insert(
                        now_ms.saturating_add(self.watchdog_tick_ms),
                        TimerKind::StuckSweep,
                    );
                }
                TimerKind::EquitySample => {
                    self.sample_equity(now_ms);
                    // Re-arm only while still open — a fire that closed the LAST position on this
                    // exact tick (sample_equity ran with it still counted, one final honest sample)
                    // must not keep sampling a flat book afterward. `maintain_equity_timer`'s own
                    // disarm branch (checked at every boundary) is the OTHER route to
                    // `equity_timer = None`; this is the self-rescheduling twin for the still-open
                    // case, mirroring `TimerKind::StuckSweep`'s re-arm just above. A day this fires
                    // with `equity_sample` unset would mean a config mutated after spawn (not
                    // possible today — no setter exists) — the `if let` simply skips the re-arm
                    // rather than assuming `Some`, the same defensive style `sweep_stuck_orders`
                    // uses for `submit_ack_timeout`.
                    if let Some(interval) = self.config.equity_sample {
                        if self.any_position_open() {
                            self.equity_timer = Some(self.timers.insert(
                                now_ms.saturating_add(interval.as_millis() as i64),
                                TimerKind::EquitySample,
                            ));
                        } else {
                            self.equity_timer = None;
                        }
                    }
                }
                TimerKind::StateSave => {
                    self.save_all_strategy_state();
                    // Re-arm only while a mount still exists — mirrors `TimerKind::EquitySample`'s
                    // re-arm just above, substituting `has_any_mount()` for `any_position_open()`
                    // (this timer's one deliberate difference: durable strategy state matters flat
                    // or not, so it never gates on open positions). `maintain_state_save_timer`'s
                    // own disarm branch (checked at every boundary) is the OTHER route to
                    // `state_save_timer = None`. The `if let` mirrors the same defensive
                    // not-necessarily-`Some` style as the `EquitySample` arm above.
                    if let Some(interval) = self.config.state_save {
                        if self.config.state_dir.is_some() && self.has_any_mount() {
                            self.state_save_timer = Some(self.timers.insert(
                                now_ms.saturating_add(interval.as_millis() as i64),
                                TimerKind::StateSave,
                            ));
                        } else {
                            self.state_save_timer = None;
                        }
                    }
                }
                TimerKind::DeadManSweep => {
                    // Evaluate the dead-man's switch (trip / re-arm) at this boundary, then
                    // unconditionally re-arm the next sweep — like `StuckSweep`, the switch has no
                    // disarm condition (once armed it watches for the core's whole life). The trip
                    // itself (cancel-all / HALT) lives in `sweep_deadman`.
                    self.sweep_deadman(now_ms);
                    self.timers.insert(
                        now_ms.saturating_add(self.deadman_tick_ms),
                        TimerKind::DeadManSweep,
                    );
                }
                TimerKind::LinkDeadManSweep => {
                    // Evaluate the CONNECTION-state dead-man (M13) at this boundary, then
                    // unconditionally re-arm — like `DeadManSweep`, this switch has no disarm
                    // condition. The trip itself (the venue-scoped cancel / HALT) lives in
                    // `sweep_link_deadman`.
                    self.sweep_link_deadman(now_ms);
                    self.timers.insert(
                        now_ms.saturating_add(self.link_deadman_tick_ms),
                        TimerKind::LinkDeadManSweep,
                    );
                }
                TimerKind::GtdSweep => {
                    // Managed good-till-date expiry (core-ergonomics): cancel every resting order
                    // whose deadline has passed, then unconditionally re-arm — like `StuckSweep`
                    // this sweep has no disarm condition (once armed it runs for the core's life).
                    self.sweep_gtd_expiry(now_ms);
                    self.timers
                        .insert(now_ms.saturating_add(self.gtd_tick_ms), TimerKind::GtdSweep);
                }
                TimerKind::PortfolioSnap => {
                    // Periodic compact equity/positions journal record (core-ergonomics). Re-arms
                    // unconditionally, like the sweeps above; `write_portfolio_snap` itself no-ops
                    // when no journal is open (defensive — the arm condition already requires one).
                    self.write_portfolio_snap(now_ms);
                    self.timers.insert(
                        now_ms.saturating_add(self.portfolio_snap_tick_ms),
                        TimerKind::PortfolioSnap,
                    );
                }
                TimerKind::InflightConfirm => {
                    // FAST in-flight confirm (recon path-to-superset, F1-A): reject-free re-query of
                    // orders stuck pre-ack in the `[inflight_confirm, submit_ack_timeout)` band, then
                    // unconditionally re-arm — like `StuckSweep` this sweep has no disarm condition
                    // (once armed it runs for the core's life).
                    self.sweep_inflight_confirms(now_ms);
                    self.timers.insert(
                        now_ms.saturating_add(self.inflight_confirm_tick_ms),
                        TimerKind::InflightConfirm,
                    );
                }
            }
        }
    }

    /// Append ONE compact portfolio observation to the write-ahead journal (core-ergonomics) —
    /// COLD path only, called from the [`TimerKind::PortfolioSnap`] arm of
    /// [`Self::drive_due_timers`], never the per-message fold. See
    /// [`CoreConfig::portfolio_snapshot_interval`] for WHY the journal is the sink.
    ///
    /// Reads the same equity source the sampler and `CoreSnapshot` use (resolver-priced
    /// [`vike_exec::ExecutionEngine::resolved_equity`] under [`CoreConfig::price_cfg`], at each
    /// engine's own seed — the one-price law) and every OPEN position, in `Account` insertion
    /// order. A no-op when no journal is open (defensive: the arm condition already requires one).
    ///
    /// BEST-EFFORT: a write error is logged and swallowed rather than `expect`ed. Unlike the
    /// write-ahead command appends — where a lost record would break replay — this record is a
    /// replay-neutral observation, so losing one must not take a live trading core down. It also
    /// deliberately does NOT bump `journaled_since_snap`: that counter paces the REPLAY-BASE `Snap`
    /// cadence, which should stay a function of folded commands, not of an observation timer.
    pub(crate) fn write_portfolio_snap(&mut self, now_ms: i64) {
        if self.journal.is_none() {
            return;
        }
        let cfg = self.config.price_cfg;
        let mut venues = Vec::with_capacity(self.extra_engines.len() + 1);
        let mut positions = Vec::new();
        for idx in 0..=self.extra_engines.len() {
            let seed = self.seed_of(idx);
            let eng = self.eng(idx);
            venues.push(crate::journal::PortfolioVenueSample {
                venue: eng.venue.clone(),
                // ⚠ `resolved_equity`, deliberately UNCAPPED by
                // `vike_config::Policy::max_sizing_equity`: this is a RECORD of what the account
                // held, not a decision made against it. A journal that wrote the operator's ceiling
                // instead of the account's equity would read an incident review the number it was
                // configured with rather than the number that existed.
                equity: eng.resolved_equity(seed, &cfg),
                balance: eng.account.balance,
                realized_pnl: eng.account.realized_pnl,
            });
            for ((venue, symbol, side), p) in eng.account.positions.iter() {
                if p.size == 0.0 {
                    continue;
                }
                positions.push(crate::journal::PortfolioPositionSample {
                    venue: venue.to_string(),
                    symbol: symbol.to_string(),
                    position_side: side.to_string(),
                    size: p.size,
                    avg_px: p.avg_px,
                });
            }
        }
        let sample = crate::journal::PortfolioSample { ts: now_ms, venues, positions };
        if let Some(journal) = self.journal.as_mut()
            && let Err(e) = journal.append_portfolio_snap(now_ms, &sample)
        {
            tracing::warn!(
                target: "vike_core::journal",
                error = %e,
                "failed to append periodic portfolio snapshot (best-effort)"
            );
        }
    }

    /// True while ANY engine (primary + extras) holds an open position — the equity sampler's
    /// arm/disarm gate (portfolio-observer PR-3). A CLOSED position leaves a ZERO-SIZE entry in
    /// `account.positions` that is never removed (the map is keyed by `(venue, symbol,
    /// position_side)` for the engine's whole life), so this tests `.size != 0.0`, NEVER
    /// emptiness of the map itself — `positions.is_empty()` would never go true again after the
    /// first fill and the sampler would sample forever.
    pub(crate) fn any_position_open(&self) -> bool {
        self.engine.account.positions.values().any(|p| p.size != 0.0)
            || self
                .extra_engines
                .iter()
                .any(|(_, e)| e.account.positions.values().any(|p| p.size != 0.0))
    }

    /// Arm/disarm the equity-sample timer (portfolio-observer PR-3) at the drain-loop boundary —
    /// the SAME boundary [`Self::drive_due_timers`] advances at, never the per-message fold.
    /// Called only when `config.equity_sample` is `Some` (see the call site in [`Self::run`]).
    /// While ANY position is open ([`Self::any_position_open`]) a self-rescheduling
    /// [`TimerKind::EquitySample`] stays armed at `interval`; the instant the book goes flat the
    /// still-armed timer is cancelled, so a flat account samples nothing until it opens again.
    pub(crate) fn maintain_equity_timer(&mut self, now_ms: i64) {
        let Some(interval) = self.config.equity_sample else { return };
        let open = self.any_position_open();
        match (self.equity_timer, open) {
            (None, true) => {
                self.equity_timer = Some(self.timers.insert(
                    now_ms.saturating_add(interval.as_millis() as i64),
                    TimerKind::EquitySample,
                ));
            }
            (Some(id), false) => {
                self.timers.cancel(id);
                self.equity_timer = None;
            }
            _ => {} // (None, false): stays disarmed; (Some, true): stays armed, nothing to do
        }
    }

    /// True while at least one strategy mount is live — the state-save timer's arm gate
    /// (portfolio-observer PR-4 T3), mirroring [`Self::any_position_open`]'s role for the equity
    /// sampler. A mount slot is `None` only transiently, mid-panic, during the take/replace dance
    /// around a strategy call (see e.g. [`Self::drive_strategy`]) — by the time this runs (the
    /// drain-loop boundary, after every dispatch in the batch has returned its mount), every slot
    /// is back to `Some` in the normal case, so this simply mirrors the same permissive
    /// `slot.is_some()` read the save-on-stop loop already uses.
    fn has_any_mount(&self) -> bool {
        self.mounts.iter().any(|m| m.is_some())
    }

    /// Arm/disarm the state-save timer (portfolio-observer PR-4 T3) at the drain-loop boundary —
    /// the SAME boundary [`Self::drive_due_timers`] advances at, never the per-message fold.
    /// Called only when `config.state_save` is `Some` (see the call site in [`Self::run`]).
    /// Mirrors [`Self::maintain_equity_timer`] almost exactly; the one deliberate difference is
    /// the arm condition: while [`Self::has_any_mount`] is true AND [`CoreConfig::state_dir`] is
    /// also configured (there is nothing to persist to otherwise), a self-rescheduling
    /// [`TimerKind::StateSave`] stays armed at `interval` — NOT additionally gated on any
    /// position being open, unlike the equity sampler: a strategy's durable state (a breaker's
    /// trip count, an A-S accumulator, ...) matters whether the book is flat or not. The instant
    /// every mount is gone (or `state_dir` was never set) the still-armed timer is cancelled.
    pub(crate) fn maintain_state_save_timer(&mut self, now_ms: i64) {
        let Some(interval) = self.config.state_save else { return };
        let armable = self.config.state_dir.is_some() && self.has_any_mount();
        match (self.state_save_timer, armable) {
            (None, true) => {
                self.state_save_timer = Some(self.timers.insert(
                    now_ms.saturating_add(interval.as_millis() as i64),
                    TimerKind::StateSave,
                ));
            }
            (Some(id), false) => {
                self.timers.cancel(id);
                self.state_save_timer = None;
            }
            _ => {} // (None, false): stays disarmed; (Some, true): stays armed, nothing to do
        }
    }

    /// Save every live mount's durable-state sidecar RIGHT NOW (portfolio-observer PR-4 T3):
    /// the SAME per-mount save loop `run()`'s clean-shutdown teardown uses (T2), factored out
    /// here so BOTH the shutdown save and the periodic [`TimerKind::StateSave`] fire share this
    /// one code path (DRY) instead of two copies drifting apart. For each live mount (indexed, so
    /// a slot left `None` by an earlier panicked strategy call can't shift the `mount_ids`
    /// alignment for the mounts after it — see [`Self::mount_ids`]): `Strategy::save_state` runs
    /// under the same `catch_unwind` guard as every other strategy-hook call site in this file
    /// (arbitrary user code, must not be able to unwind past this call, still less kill the
    /// "vt-core" thread mid-fire and skip every mount sequenced after the panicking one); `Some`
    /// writes the sidecar via `write_json_atomic`, `None` skips that mount, and a write/panic
    /// error is logged and swallowed — best-effort, must never panic or block the caller (the
    /// shutdown teardown relies on that same guarantee). Gated on `config.state_dir` being
    /// `Some` — the caller in `run()`'s teardown re-checks this itself first so it can also skip
    /// calling this at all when disabled, and [`Self::maintain_state_save_timer`]'s own arm
    /// condition already guarantees `state_dir` is `Some` by the time the timer ever fires — but
    /// this method re-checks anyway so it is safe to call unconditionally, matching the
    /// defensive style [`Self::sweep_stuck_orders`] uses for `submit_ack_timeout`. Takes no
    /// timestamp (review cleanup, T6): the sidecar format carries no save timestamp, so there was
    /// nothing for one to feed — an earlier `_now_ms` parameter was dropped as functionally
    /// inert, and with it the teardown call site's clock read that existed solely to supply it.
    pub(crate) fn save_all_strategy_state(&mut self) {
        let Some(dir) = self.config.state_dir.as_ref() else { return };
        for (i, slot) in self.mounts.iter().enumerate() {
            let Some(mount) = slot else { continue };
            let mount_id = &self.mount_ids[i];
            let v = match catch_unwind(AssertUnwindSafe(|| mount.strategy.save_state())) {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(payload) => {
                    tracing::warn!(
                        target: "vike_core::strategy_state",
                        mount_id = %mount_id,
                        reason = %panic_text(payload),
                        "strategy save_state panicked (best-effort, continuing)"
                    );
                    continue;
                }
            };
            let sidecar = crate::strategy_state::sidecar_path(dir, mount_id);
            if let Err(e) = crate::strategy_state::write_json_atomic(&sidecar, &v) {
                tracing::warn!(
                    target: "vike_core::strategy_state",
                    mount_id = %mount_id,
                    error = %e,
                    "failed to save strategy state sidecar (best-effort)"
                );
            }
        }
    }

    /// Readiness-gate boundary probe (portfolio-observer PR-4 T5): while [`CoreConfig::readiness_gate`]
    /// is on (see the call site in [`Self::run`]), check every still-[`MountState::Pending`] mount's
    /// (venue, symbol) against its ENGINE'S OWN `PriceBoard` and flip it to [`MountState::Ready`] the
    /// moment EITHER side resolves (long OR short — either quote/mark/trade/bar-close side pricing is
    /// enough to trust the symbol, mirroring the sampler's own "priced" definition). Unlike the
    /// equity-sample/state-save timers this has no cadence of its own and is not timer-wheel-driven:
    /// it just re-walks the still-Pending subset on every boundary pass, using [`CoreConfig::price_cfg`]
    /// — the SAME resolver knobs [`Self::sample_equity`] and `CoreSnapshot::build` already price
    /// through, so "priced" means one consistent thing everywhere. Nothing ever flips a mount back to
    /// `Pending`, so the leading `contains(&Pending)` check short-circuits to O(1) the moment every
    /// mount has traded at least once — a long-running gated core does not keep re-resolving prices
    /// for mounts that are already trading. A mount slot that is transiently `None` (mid strategy-hook dispatch) is
    /// skipped rather than panicking — this boundary never runs from inside that window in practice
    /// (see [`Self::has_any_mount`]'s doc for why), so this is defensive, not a normal-path filter.
    pub(crate) fn maintain_mount_readiness(&mut self, now_ms: i64) {
        if !self.mount_states.contains(&MountState::Pending) {
            return;
        }
        let cfg = self.config.price_cfg;
        for i in 0..self.mounts.len() {
            if self.mount_states[i] != MountState::Pending {
                continue;
            }
            let Some(m) = self.mounts[i].as_ref() else { continue };
            // The MOUNT's own engine, like every other per-mount read — a readiness verdict about
            // account `ALT`'s mount is a question about `ALT`'s price board. The two answers agree
            // today (`CoreThread::mirror_venue_price` puts every venue price on every engine of the
            // exchange), and this is spelled the structural way so that stays a property of the
            // mirror rather than something this lane depends on silently.
            let eidx = self.mount_eng(i);
            let pb = &self.eng(eidx).price_board;
            let ready =
                !matches!(pb.resolve(&m.venue, &m.symbol, true, now_ms, &cfg), Resolution::Missing)
                    || !matches!(
                        pb.resolve(&m.venue, &m.symbol, false, now_ms, &cfg),
                        Resolution::Missing
                    );
            if ready {
                self.mount_states[i] = MountState::Ready;
            }
        }
    }

    /// Build one [`MountView`] per live mount slot (portfolio-observer PR-4 T5) — the snapshot's
    /// per-mount readiness view, read by [`Self::publish`] — PLUS the trailing
    /// [`crate::snapshot::MountRowKind::Residual`] row (gap E) whenever at least one mount exists.
    /// Cold path only. A transiently-taken slot (mid strategy-hook dispatch) is skipped rather than
    /// panicking, for the same reason [`Self::maintain_mount_readiness`]'s doc gives (publish never
    /// runs from inside that window in practice).
    pub(crate) fn mount_views(&self) -> Vec<MountView> {
        let mut views: Vec<MountView> = (0..self.mounts.len())
            .filter_map(|i| {
                let m = self.mounts[i].as_ref()?;
                // steal/core-per-mount-budget: surface the resolver-priced per-mount attribution +
                // budget on the read-only view (off the fold, at the coalesced publish cadence). A
                // mount that never filled reports all-zero valuation and its `budget`/`latched`
                // straight from the runtime state.
                let (position, realized_pnl, unrealized_pnl, notional) = self.mount_valuation(i);
                Some(MountView {
                    kind: crate::snapshot::MountRowKind::Mount,
                    venue: m.venue.clone(),
                    symbol: m.symbol.clone(),
                    interval: m.interval.clone(),
                    ready: self.mount_states[i] == MountState::Ready,
                    position,
                    realized_pnl,
                    unrealized_pnl,
                    notional,
                    budget: self.mount_budget[i],
                    latched: self.mount_latched[i],
                    // The live-params READ (this function's own cold-path contract, above): ask the
                    // strategy what it is CURRENTLY tuned to, so a re-tune is visible instead of the
                    // boot config being replayed forever. `Strategy::params` takes `&self` like
                    // `save_state`, so no take/replace dance is needed and a transiently-taken slot
                    // was already skipped by the `as_ref()?` above. `None` for a strategy that
                    // publishes none — the trait default, and the same set of mounts
                    // `Command::UpdateParams` can address.
                    //
                    // ⚠ WHY THIS IS FREE TO THE LATENCY GATE, stated structurally so the next
                    // reader need not re-run an A/B to find out. This is a per-mount struct copy at
                    // the publish cadence — and publish fires PER EVENT when the core goes idle
                    // (`crates/vike-core/CLAUDE.md`), so "per publish" is not automatically cheap
                    // and the question is a fair one. The answer is that the three GATED harnesses
                    // never reach this line at all: `crates/vike-core/tests/runtime_latency.rs`'s
                    // `run_core_hop` builds its `CoreConfig` with `..CoreConfig::default()` and
                    // never sets `strategy`, so `baseline`/`journal`/`journal-snap` all run with
                    // `strategy: None` — `self.mounts` is EMPTY, this closure is invoked zero times,
                    // and no ceiling in that file can see this work. The mounted variants that DO
                    // pay it (`book-hop-*-mounted`) carry no ceilings, deliberately.
                    //
                    // That argument holds regardless of box load, which is why it is written here
                    // rather than replaced by a measurement: an A/B on a busy the CI box lane could not
                    // settle it (measured 2026-09-07 at load 11→59 on 32 cores, where `origin/main`
                    // itself failed the `journal-snap` max gate and sibling variants ranked in
                    // opposite directions). If a future implementor's `params()` stops being a plain
                    // struct copy, this fill needs a change gate — see `Strategy::params`' doc.
                    params: m.strategy.params(),
                })
            })
            .collect();
        // Multi-mount durability (gap E): close the ledger with the RESIDUAL row, so
        // `Σ (mount rows) + residual == account realized (net of fees)` is a checkable invariant
        // instead of a silent discrepancy. Only when a mount exists — a mount-free core publishes an
        // empty `mounts` vec exactly as before.
        if !views.is_empty() {
            let residual = self.mount_residual_view(&views);
            views.push(residual);
        }
        views
    }

    /// The account-vs-mounts RESIDUAL row (gap E) — see [`crate::snapshot::MountRowKind::Residual`]
    /// for what it means and why it exists.
    ///
    /// `account realized NET of fees`, summed over the primary engine + every extra engine (a
    /// cross-venue core folds one `Account` per venue), MINUS the sum of the mount rows already
    /// built — which are themselves net (`mount_valuation` returns `realized_pnl - fees_paid`). Both
    /// sides therefore use ONE definition of "realized", so the invariant is exact rather than
    /// approximately right. Funding is deliberately NOT in either side: it is booked separately from
    /// fill PnL and belongs to no mount by construction.
    ///
    /// Cold path only (the coalesced publish, alongside the rest of `mount_views`) — O(engines +
    /// mounts) of pure arithmetic, never the per-message fold.
    fn mount_residual_view(&self, mounts: &[MountView]) -> MountView {
        let mut account_net = 0.0;
        for idx in 0..=self.extra_engines.len() {
            let a = &self.eng(idx).account;
            account_net += a.realized_pnl - a.fees_paid;
        }
        let attributed: f64 = mounts.iter().map(|m| m.realized_pnl).sum();
        MountView {
            kind: crate::snapshot::MountRowKind::Residual,
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            ready: true,
            position: 0.0,
            realized_pnl: account_net - attributed,
            unrealized_pnl: 0.0,
            notional: 0.0,
            budget: None,
            latched: false,
            // No strategy behind this row to ask — it is account-wide, not a mount.
            params: None,
        }
    }

    /// Fire one equity-sample batch (portfolio-observer PR-3): COLD path only, called from the
    /// [`TimerKind::EquitySample`] arm in [`Self::drive_due_timers`], never the per-message fold.
    /// Resolves each engine's equity through the SAME `resolve_equity` PR-2's `CoreSnapshot`
    /// build uses (`config.price_cfg`), notes EVERY open position's priced/missing state on that
    /// engine's OWN `PriceBoard` (the warn-once wiring `resolve_equity`'s doc comment deferred to
    /// this sampler — `Missing` arms the per-venue warn-once set, `Priced` clears it so a symbol
    /// that recovers doesn't sit stuck in the missing set forever), and pushes one row per engine
    /// (primary then extras, registration order) plus a `"TOTAL"` cross-venue row (the SAME
    /// `py_sum` laws as `CoreSnapshot::equity_total`) into the reusable `equity_rows` buffer —
    /// cleared and refilled every fire so steady-state sampling allocates nothing beyond the
    /// per-position `note()` key scratch below. Calls `config.on_equity_sample` with the finished
    /// slice when a sink is set.
    pub(crate) fn sample_equity(&mut self, now_ms: i64) {
        let cfg = self.config.price_cfg;
        self.equity_rows.clear();
        for idx in 0..=self.extra_engines.len() {
            let seed = self.seed_of(idx);
            // ⚠ `resolve_equity`, so this row is UNCAPPED by
            // `vike_config::Policy::max_sizing_equity` — an OBSERVATION of the account, like the
            // portfolio snap above it and the `CoreSnapshot` display below. The GUI's equity curve
            // and the `on_equity_sample` callback must show what the wallet holds; the ceiling is a
            // bound on what may be spent against it, not a restatement of it.
            let re = self.eng(idx).resolve_equity(seed, &cfg);
            // Missing/recovery bookkeeping (deferred from PR-2's `resolve_equity` doc comment,
            // completed here): `note()` needs (venue, symbol), which `ResolvedPosition` does not
            // carry — re-derive it from `account.positions`' keys, in the SAME insertion order
            // `resolve_equity` walked them. Collected into an owned Vec BEFORE the `&mut`
            // `note()` calls below so the immutable `eng(idx)` read and the mutable
            // `eng_mut(idx)` write never overlap. EVERY open position is `note()`d exactly once
            // per sample, Missing or Priced — the dummy `px: 0.0`/`ts: now_ms` on the Priced arm
            // are harmless since `note()`'s `Resolution::Priced { .. }` match arm ignores every
            // field but the variant itself; the real `source` is threaded through for
            // cleanliness even though `note()` doesn't read it.
            let keys: Vec<(String, String)> = self
                .eng(idx)
                .account
                .positions
                .keys()
                .map(|(venue, symbol, _side)| (venue.to_string(), symbol.to_string()))
                .collect();
            for (rp, (venue, symbol)) in re.per_position.iter().zip(keys.iter()) {
                let res = match rp.mark_source {
                    Some(source) => Resolution::Priced { px: 0.0, source, ts: now_ms },
                    None => Resolution::Missing,
                };
                self.eng_mut(idx).price_board.note(venue, symbol, &res);
            }
            let venue = self.eng(idx).venue.clone();
            let realized = self.eng(idx).account.realized_pnl;
            self.equity_rows.push(EquitySample {
                ts: now_ms,
                venue,
                equity: re.equity,
                realized,
                unrealized: re.unrealized_total,
                missing_prices: re.missing,
            });
        }
        let total = EquitySample {
            ts: now_ms,
            venue: "TOTAL".to_string(),
            equity: vike_model::py_sum(self.equity_rows.iter().map(|r| r.equity)),
            realized: vike_model::py_sum(self.equity_rows.iter().map(|r| r.realized)),
            unrealized: vike_model::py_sum(self.equity_rows.iter().map(|r| r.unrealized)),
            missing_prices: self.equity_rows.iter().map(|r| r.missing_prices).sum(),
        };
        self.equity_rows.push(total);
        if let Some(cb) = self.config.on_equity_sample.as_mut() {
            cb(&self.equity_rows);
        }
    }
}
