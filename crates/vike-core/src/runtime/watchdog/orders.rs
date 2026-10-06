//! Order sweeps: stuck-order ladder, in-flight confirms, GTD expiry, safe state, shutdown cancel.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Stuck-order watchdog (audit C3), hardened into a confirm-grace LADDER with an in-flight-confirm
    /// guard: the LAST-RESORT backstop for an order stuck pre-ack (`Initialized`/`Submitted`) — an
    /// adapter that accepted `submit` but whose thread wedged before emitting any terminal (audit A1).
    /// It must NEVER race a slow-but-real venue ack into a phantom reject: if the venue actually
    /// accepted/filled the order, a synthesized `OrderRejected` would strand a real position (a later
    /// real `OrderAccepted`/`OrderFilled` is then an illegal transition from `Rejected`, dropped — now
    /// counted as `stranded_terminal_drops` in the exec fold, but the position is still stranded). The
    /// ladder:
    ///
    ///   * STAGE 1 — soft-warn AND actively `confirm` once at `created + submit_ack_timeout`: the
    ///     order is flagged un-acked and the venue is PRODDED into a status re-query (its authoritative
    ///     terminal folds back on the normal event lane — a slow WS ack OR the adapter's own REST
    ///     re-query, `vike_bridge_core::resolve_ambiguous_submit`). We do NOT terminalize here, and we
    ///     record WHEN the confirm was issued (`confirm_issued_ms`).
    ///   * STAGE 2 — hard-reject, gated by the IN-FLIGHT-CONFIRM GUARD. The plain reject deadline is
    ///     `created + submit_ack_timeout + grace`; but a reject candidate that had a stage-1 confirm
    ///     issued and is STILL pre-ack (nothing authoritative folded — a real ack/fill would have moved
    ///     it out of `{Initialized, Submitted}`) has its confirm presumed IN FLIGHT, so the reject is
    ///     DEFERRED one additional grace window, to `created + submit_ack_timeout + 2·grace`. Only once
    ///     THAT extended deadline passes, still pre-ack, is the last-resort `OrderRejected` synthesized.
    ///     This is what stops a slow confirm (Bybit's realtime→history re-query ≈ 2×5s, which can
    ///     exceed a single 5s grace) from being clobbered — see
    ///     [`CoreConfig::submit_ack_confirm_grace`] for the hard lower bound. An order that somehow
    ///     reaches the reject deadline WITHOUT a prior confirm (a busy core skipped ticks, or a clock
    ///     jump) is not rejected on the spot either: it is confirmed now and deferred, so the invariant
    ///     "no backstop reject without at least one active confirm + its grace window" holds.
    ///
    /// The core fold thread issues NO network call (the hard constraint): `confirm_order` is
    /// fire-and-forget onto the adapter's own thread, and the sweep only WAITS for the terminal to
    /// arrive on the event lane. A no-op when the timeout is `None` (default) or nothing is stuck.
    /// `now_ms` was stamped at the top of `dispatch`, so the sweep is deterministic; reconcile-seeded
    /// orders (`created_ms == None`) are never swept. `confirm_issued_ms` is self-cleaning (pruned to
    /// only still-pre-ack coids at the end of every sweep), so it stays bounded.
    pub(crate) fn sweep_stuck_orders(&mut self) {
        let Some(timeout) = self.config.submit_ack_timeout else {
            // watchdog disabled — drop any stale ladder state (a config can be flipped off at runtime)
            if !self.confirm_issued_ms.is_empty() {
                self.confirm_issued_ms.clear();
            }
            return;
        };
        let ack_ms = timeout.as_millis() as i64;
        let grace_ms = self.config.submit_ack_confirm_grace.as_millis() as i64;
        let reject_ms = ack_ms.saturating_add(grace_ms);
        // The in-flight-confirm guard's EXTENDED deadline: one additional grace window past the plain
        // reject deadline, so an issued-but-unresolved confirm gets up to 2·grace total to land.
        let extended_reject_ms = reject_ms.saturating_add(grace_ms);
        let now = self.engine.now_ms;
        // one pass over the registry, classifying each still-pre-ack order by how long it has waited
        let mut in_window: Vec<String> = Vec::new(); // stage 1: un-acked, still within the grace
        let mut reject_candidates: Vec<(String, i64)> = Vec::new(); // stage 2: (coid, age), grace elapsed
        for (coid, mo) in self.engine.registry.iter() {
            if !matches!(
                mo.status,
                vike_exec::OrderStatus::Initialized | vike_exec::OrderStatus::Submitted
            ) {
                continue;
            }
            let Some(created) = mo.created_ms else {
                continue; // reconcile-seeded — never swept
            };
            let age = now.saturating_sub(created);
            if age > reject_ms {
                reject_candidates.push((coid.clone(), age));
            } else if age > ack_ms {
                in_window.push(coid.clone());
            }
        }
        // STAGE 1 — soft-warn AND actively confirm each order exactly once as it enters the grace
        // window (audit ex1 residual). `confirm_order` is fire-and-forget + a no-op for clients with
        // no re-query (the `ExecutionClient::confirm` default). Record the issue time so stage 2's
        // guard knows a confirm is in flight.
        let mut issued_confirm = false;
        for coid in &in_window {
            if !self.confirm_issued_ms.contains_key(coid) {
                tracing::warn!(
                    target: "vike_core::watchdog",
                    coid = %coid,
                    grace_ms = grace_ms as u64,
                    "order un-acked past submit_ack_timeout; issuing active confirm, awaiting venue terminal before backstop reject"
                );
                self.engine.confirm_order(coid);
                self.confirm_issued_ms.insert(coid.clone(), now);
                issued_confirm = true;
            }
        }
        // STAGE 2 — last-resort terminal, gated by the in-flight-confirm guard.
        let mut to_reject: Vec<String> = Vec::new();
        for (coid, age) in &reject_candidates {
            if self.confirm_issued_ms.contains_key(coid) {
                // A confirm was issued and the order is STILL pre-ack (loop filter above) ⇒ nothing
                // authoritative folded ⇒ the confirm is presumed in flight. Defer until the EXTENDED
                // deadline gives it a full 2·grace to land; only then terminalize.
                if *age > extended_reject_ms {
                    to_reject.push(coid.clone());
                }
                // else: defer this sweep — keep the entry, wait for the confirm's terminal or the deadline
            } else {
                // Reached the reject deadline WITHOUT ever passing through stage 1 (the sweep never
                // caught this order in-window — a busy core skipped ticks, or `now` jumped). Never
                // reject without first prodding the venue: issue the active confirm NOW, record it, and
                // defer. The next sweep applies the extended-deadline guard above.
                tracing::warn!(
                    target: "vike_core::watchdog",
                    coid = %coid,
                    grace_ms = grace_ms as u64,
                    "stuck order reached the reject deadline unconfirmed; issuing active confirm and deferring the backstop reject"
                );
                self.engine.confirm_order(coid);
                self.confirm_issued_ms.insert(coid.clone(), now);
                issued_confirm = true;
            }
        }
        // Drain any events an in-process client synthesized from the confirm(s) (real venue clients
        // emit over the ingest lane instead; this is a no-op for them).
        if issued_confirm {
            self.pump_client();
        }
        // STAGE 2 execution — synthesize the last-resort terminal for orders past the extended deadline.
        for coid in to_reject {
            tracing::warn!(target: "vike_core::watchdog", coid = %coid, "terminalizing stuck order (no venue ack or REST confirm within the extended confirm-grace)");
            self.bus.publish(
                Event::OrderRejected(OrderRejected {
                    client_order_id: coid,
                    reason: "watchdog: no venue ack within confirm-grace".to_string().into(),
                    ts: now,
                }),
                &mut self.engine,
            );
            self.dirty = true;
        }
        // Self-cleaning prune: keep only entries whose order is STILL pre-ack. Orders that progressed
        // (acked/filled), terminalized (incl. the rejects just published), or vanished from the
        // registry drop out — bounding the map exactly like the old rebuilt set did.
        if !self.confirm_issued_ms.is_empty() {
            let pre_ack: std::collections::HashSet<String> = self
                .engine
                .registry
                .iter()
                .filter(|(_, mo)| {
                    matches!(
                        mo.status,
                        vike_exec::OrderStatus::Initialized | vike_exec::OrderStatus::Submitted
                    )
                })
                .map(|(coid, _)| coid.clone())
                .collect();
            self.confirm_issued_ms.retain(|coid, _| pre_ack.contains(coid));
        }
    }

    /// FAST IN-FLIGHT CONFIRM (recon path-to-superset, F1-A) — the REJECT-FREE early rung of the
    /// stuck-order ladder, grafted from Nautilus's ~2s `check_inflight_orders`. Re-queries an order
    /// stuck SUBMITTED-but-unacked (`Initialized`/`Submitted`, `created_ms == Some`) on a fast
    /// cadence (~2s), catching a wedged adapter in seconds instead of waiting the conservative
    /// `submit_ack_timeout` (at LEAST 30s) for [`Self::sweep_stuck_orders`]'s first confirm. Opt-in
    /// via [`CoreConfig::inflight_confirm`]; `None` (default) never arms the timer, so this method is
    /// never called and the runtime is byte-identical.
    ///
    /// SAFETY — this rung NEVER synthesizes a terminal and NEVER fights the reject ladder. Three
    /// guarantees, all load-bearing (the single biggest correctness risk for this feature is a fast
    /// confirm becoming a fast reject, or confirming an order the ladder is mid-deferral on):
    ///   * it calls ONLY [`vike_exec::ExecutionEngine::confirm_order`] — `is_live`-guarded,
    ///     fire-and-forget onto the adapter's OWN thread, publishes NOTHING (the venue's
    ///     authoritative terminal folds back on the normal ingest lane). It can strand nothing.
    ///   * it is BAND-LIMITED to `age in [inflight_confirm, submit_ack_timeout)` — the window the
    ///     reject-capable late rung ([`Self::sweep_stuck_orders`], gated on `submit_ack_timeout`)
    ///     ignores. The instant an order crosses `submit_ack_timeout` this rung STOPS touching it and
    ///     the late rung owns it, so the two never act on the same order at the same age and this rung
    ///     never touches the ladder's `confirm_issued_ms` bookkeeping. When `submit_ack_timeout` is
    ///     `None` (watchdog off) the band is `[inflight_confirm, +inf)` — this becomes the ONLY
    ///     stuck-order signal, still reject-free (without a configured reject timeout we must never
    ///     invent a terminal).
    ///   * its OWN dedup map (`inflight_confirm_last_ms`, SEPARATE from the ladder's
    ///     `confirm_issued_ms`) re-confirms a still-stuck order at most once per `inflight_confirm`
    ///     window, so a persistently-stuck order is not re-confirmed every tick.
    ///
    /// Covers every engine (primary + extras), mirroring [`Self::sweep_gtd_expiry`] — the confirm is
    /// reject-free, so extending it to secondary venues is purely additive (they gain the fast
    /// confirm; for a primary-engine order the hand-off past the band is to `sweep_stuck_orders`,
    /// which today is primary-engine-only). The core fold thread issues NO network call
    /// (`confirm_order` is fire-and-forget); `pump_client()` once if any confirm was issued drains an
    /// in-process client's synthesized events (a no-op for real venue clients, which emit over the
    /// ingest lane). The dedup map is self-cleaning — pruned each sweep to still-pre-ack coids — so it
    /// stays bounded. `now` was stamped at the drain-loop boundary by [`Self::drive_due_timers`];
    /// reconcile-seeded orders (`created_ms == None`) are never swept.
    pub(crate) fn sweep_inflight_confirms(&mut self, now: i64) {
        let Some(inflight) = self.config.inflight_confirm else {
            // feature disabled — drop any stale dedup state (a config could be flipped off at runtime)
            if !self.inflight_confirm_last_ms.is_empty() {
                self.inflight_confirm_last_ms.clear();
            }
            return;
        };
        let inflight_ms = inflight.as_millis() as i64;
        // Upper band bound: hand off to the reject ladder AT `submit_ack_timeout`. `None` (watchdog
        // off) ⇒ no upper bound, so this rung is the only stuck-order signal (still reject-free).
        let ack_ms = self.config.submit_ack_timeout.map(|t| t.as_millis() as i64);
        // READ pass: classify each still-pre-ack, in-band order across every engine as (idx, coid).
        // Only immutable borrows of `self` here (the dedup map read included), so the borrow checker
        // is satisfied; the mutable `confirm_order` calls happen in the act pass below.
        let mut to_confirm: Vec<(usize, String)> = Vec::new();
        for idx in 0..=self.extra_engines.len() {
            for (coid, mo) in self.eng(idx).registry.iter() {
                if !matches!(
                    mo.status,
                    vike_exec::OrderStatus::Initialized | vike_exec::OrderStatus::Submitted
                ) {
                    continue;
                }
                let Some(created) = mo.created_ms else {
                    continue; // reconcile-seeded — never swept
                };
                let age = now.saturating_sub(created);
                // BAND: [inflight_confirm, submit_ack_timeout). Below the lower bound the order is
                // not yet "stuck"; at/above the upper bound the reject ladder owns it (hand-off).
                if age < inflight_ms {
                    continue;
                }
                if let Some(ack) = ack_ms
                    && age >= ack
                {
                    continue; // handed off to sweep_stuck_orders — never double-handled
                }
                // Re-confirm-gap dedup: first sight (absent), or the `inflight_confirm` gap elapsed.
                let due = match self.inflight_confirm_last_ms.get(coid) {
                    Some(last) => now.saturating_sub(*last) >= inflight_ms,
                    None => true,
                };
                if due {
                    to_confirm.push((idx, coid.clone()));
                }
            }
        }
        // ACT pass: issue the reject-free re-query, record the issue time. NEVER pushes a terminal.
        let mut issued = false;
        for (idx, coid) in &to_confirm {
            self.eng_mut(*idx).confirm_order(coid);
            self.inflight_confirm_last_ms.insert(coid.clone(), now);
            issued = true;
        }
        // Drain any events an in-process client synthesized from the confirm(s); real venue clients
        // emit over the ingest lane instead, so this is a no-op for them.
        if issued {
            self.pump_client();
        }
        // Self-cleaning prune (mirrors `confirm_issued_ms`/`gtd_canceled`): keep only coids STILL
        // pre-ack in some registry — an order that acked/filled/terminalized or vanished drops out,
        // bounding the map. Re-read after `pump_client` so an order a confirm just resolved is pruned.
        if !self.inflight_confirm_last_ms.is_empty() {
            let mut pre_ack: std::collections::HashSet<String> = std::collections::HashSet::new();
            for idx in 0..=self.extra_engines.len() {
                for (coid, mo) in self.eng(idx).registry.iter() {
                    if matches!(
                        mo.status,
                        vike_exec::OrderStatus::Initialized | vike_exec::OrderStatus::Submitted
                    ) {
                        pre_ack.insert(coid.clone());
                    }
                }
            }
            self.inflight_confirm_last_ms.retain(|coid, _| pre_ack.contains(coid));
        }
    }

    /// MANAGED GTD/DAY EXPIRY (core-ergonomics): cancel every resting order whose time-in-force
    /// deadline has passed, on the existing [`DeadlineTimerWheel`] boundary cadence — NO new
    /// thread, NO per-message work. Opt-in via [`CoreConfig::gtd_sweep`]; `None` (default) never
    /// arms the timer, so this method is never called and the runtime is byte-identical.
    ///
    /// The expiry itself is NOT new state: [`vike_model::OrderRequest`] already carries
    /// `time_in_force` + `gtd_expiry` (epoch ms), and venues that support GTD/DAY natively expire
    /// the order themselves. This sweep is the MANAGED twin for everything else — a venue with no
    /// GTD support, or an emulated/paper client — so the same order terms mean the same thing
    /// everywhere. An order is swept when ALL of:
    ///   * [`vike_model::tif_expired`] returns true — the ONE expiry law (dedup A4) shared with the
    ///     backtest paper book: `Gtd` past its inclusive `gtd_expiry` deadline (`t <= now`), OR
    ///     `Day` once `now` crosses the UTC-day boundary from the order's session anchor (here the
    ///     order's creation wall-clock, `created_ms`; a reconcile-seeded order with no creation ts
    ///     is never Day-expired). **`Day` support is new here** — before dedup A4 this sweep
    ///     enforced `Gtd` only and silently ignored `Day`, so a live `Day` order rested forever
    ///     while its backtest twin expired; the two now agree on the expiry DECISION.
    ///   * its status is still live (not `Filled`/`Canceled`/`Rejected`/`Denied`/`Expired`/
    ///     `Liquidated`),
    ///   * it was not already swept this episode (`gtd_canceled`).
    ///
    /// **TERMINAL VOCABULARY — this sweep terminalizes via CANCEL (`OrderCanceled`), NOT
    /// `OrderExpired`, and that split from the paper book is deliberate, not an oversight.** The
    /// paper book OWNS its resting order, so it mints `OrderExpired` directly (it IS the expiry
    /// authority). This live sweep does not: the order rests at a REAL venue that lacks native GTD,
    /// so the only correct way to retire it is to ASK the venue to cancel it and let the venue's
    /// authoritative terminal fold back — which for a cancel is `OrderCanceled` (the emitter-split
    /// contract: the core never mints a terminal the venue owns). Publishing `OrderExpired` locally
    /// here would desync local state from the venue (local terminal, venue order still live) and
    /// risk a later real fill arriving as an illegal transition from `Expired` → dropped → stranded
    /// position — the exact hazard the stuck-order backstop is built around. `OrderExpired` IS a
    /// legal FSM edge (from `Accepted`/`Triggered`/`PartiallyFilled`), but it is reserved for a
    /// venue's OWN native expiry, so it stays off this managed-cancel path.
    ///
    /// The cancel is issued engine-directly (`cancel_order` + `pump_client`), exactly like the
    /// safe-state working-order sweep above it — no order is SUBMITTED here, so the single
    /// order-write site (`apply_intent`) is not bypassed. It is FIRE-ONCE
    /// per order: the coid is recorded in `gtd_canceled`, so a venue that takes several sweeps to
    /// report the terminal is not spammed with duplicate cancels. That set is self-cleaning
    /// (pruned each sweep to coids still present-and-live in a registry), so it stays bounded.
    ///
    /// ROUTING: the sweep already knows which engine each coid was found in, so the cancel is
    /// dispatched to THAT engine index directly rather than re-derived from `coid_venue` (which is
    /// populated only by `apply_intent`'s own Submit/Bracket arms — an extra engine's order that
    /// was seeded another way, e.g. by reconcile, would otherwise be cancelled against the PRIMARY
    /// engine, cancel nothing, and never be retried because it was already marked fire-once).
    ///
    /// **REPLAY: journaled and SUPPORTED (emulator PR-5).** Every expiry this pass decides is
    /// appended write-ahead as [`vike_journal::JournalRecord::GtdExpire`] before the cancel it
    /// causes — the last member of the runtime-internal residual class to get a write-ahead site,
    /// after conditional-order FIRE (`submit_fired`) and margin-call auto-liquidation
    /// (`sweep_margin_call_engine`).
    ///
    /// It is journaled DIFFERENTLY from those two, on purpose. They RELEASE an order, so their
    /// record must be re-applied through `apply_intent` on replay. This sweep releases nothing: it
    /// calls `ExecutionEngine::cancel_order`, which "publishes NOTHING — the venue stream emits the
    /// authoritative `OrderCanceled` that advances the FSM". That `OrderCanceled` is journaled as
    /// its own `Ingest::Event` and replays independently, so the sweep's contribution to
    /// `state_hash` is exactly NIL and `replay.rs` deliberately does NOT re-apply this record
    /// (re-issuing the cancel would be a no-op at best and a double-cancel against a non-inert
    /// client at worst). The record's job is to make the local decision AUDITABLE and to remove
    /// the need to re-decide it on replay — which is what lets `gtd_sweep` drop out of the
    /// `journal_waker_records` refusal gate. The `now` it reads is still wall clock; nothing about
    /// replay re-reads it.
    pub(crate) fn sweep_gtd_expiry(&mut self, now: i64) {
        // (engine index, coid) — the index is carried through so the cancel routes to the engine
        // the order actually lives in (see the ROUTING note above).
        let mut expired: Vec<(usize, String)> = Vec::new();
        let mut live: std::collections::HashSet<String> = std::collections::HashSet::new();
        for idx in 0..=self.extra_engines.len() {
            for (coid, mo) in self.eng(idx).registry.iter() {
                if is_terminal_status(mo.status) {
                    continue;
                }
                live.insert(coid.clone());
                // The ONE expiry law (dedup A4): `vike_model::tif_expired` decides Gtd (inclusive
                // deadline) AND Day (UTC-day boundary) identically for the backtest paper book and
                // this live sweep. The `Day` session anchor is the order's creation wall-clock
                // (`created_ms`); a reconcile-seeded order with no creation ts falls back to `now`,
                // which the law reads as same-day ⇒ never Day-expired — deliberately: we do not
                // synthesize an expiry for an order whose session start we cannot place. `Gtd` is
                // independent of the anchor.
                let day_anchor = mo.created_ms.unwrap_or(now);
                let is_expired = vike_model::tif_expired(
                    mo.request.time_in_force,
                    mo.request.gtd_expiry,
                    day_anchor,
                    now,
                );
                if is_expired && !self.gtd_canceled.contains(coid) {
                    expired.push((idx, coid.clone()));
                }
            }
        }
        for (idx, coid) in expired {
            tracing::info!(
                target: "vike_core::watchdog",
                coid = %coid,
                now_ms = now,
                "GTD expiry reached — cancelling the resting order"
            );
            // WRITE-AHEAD (emulator PR-5): journal the expiry DECISION before the cancel it
            // causes. Unlike the conditional-FIRE / margin-call siblings this is a MARKER, not a
            // replayable command — `cancel_order` publishes nothing locally, so the sweep's
            // contribution to `state_hash` is nil and replay must NOT re-issue it (see
            // `JournalRecord::GtdExpire`). Only an ACTUAL expiry writes (fire-once per coid), so a
            // sweep tick that finds nothing appends nothing; off-fold and gated on journaling
            // being on, so the no-journal path stays byte-identical.
            if let Some(j) = self.journal.as_mut() {
                j.append_gtd_expire(now, &coid, idx).expect("journal append");
                self.journaled_since_snap += 1;
            }
            self.gtd_canceled.insert(coid.clone());
            // Route by the engine the order was FOUND in. `coid_venue` only knows coids this
            // runtime minted/submitted itself, so re-deriving from it would misroute anything an
            // extra engine acquired another way.
            //
            // ⚠ DELIBERATELY UNCLASSIFIED (`CancelIntent::Unspecified`, never `Routine`), and this
            // is the tempting misclassification: an expiry sweep LOOKS like housekeeping. It is
            // FIRE-ONCE — `gtd_canceled` is inserted just above, so a cancel a venue held back to
            // protect its own budget would never be re-offered and the expired order would rest at
            // the venue forever. "Routine" means "safe to shed and re-offer"; this is neither.
            self.eng_mut(idx).cancel_order(&coid);
            self.pump_client();
            self.dirty = true;
        }
        // Self-cleaning prune (mirrors `confirm_issued_ms`): keep only coids that are still live in
        // some registry — an order that terminalized (the cancel landed) or vanished drops out.
        if !self.gtd_canceled.is_empty() {
            self.gtd_canceled.retain(|coid| live.contains(coid));
        }
    }

    pub(crate) fn enter_safe_state(&mut self, why: String) {
        // Fault-transition visibility (NOT per-message — this fires once per panic). The `fault`
        // field below stays the source of truth for the GUI; this only surfaces it to the log sink.
        tracing::error!(target: "vike_core::core", reason = %why, "entering safe state (trading halted)");
        self.engine.trading_state = TradingState::Halted;
        if self.fault.is_none() {
            self.fault = Some(why);
        }
        // best-effort working-order sweep; itself guarded (a poisoned client must not
        // take the loop down with it)
        let coids: Vec<String> = self
            .engine
            .registry
            .iter()
            .filter(|(_, mo)| !is_terminal_status(mo.status))
            .map(|(coid, _)| coid.clone())
            .collect();
        for (_, e) in self.extra_engines.iter_mut() {
            e.trading_state = TradingState::Halted;
        }
        // RISK-OFF, declared: the core has faulted and is pulling its whole book. A venue metering
        // its cancels must not hold ANY of these back — there is no later sweep to retry them.
        let sweep = catch_unwind(AssertUnwindSafe(|| {
            for coid in &coids {
                self.engine.cancel_order_with_intent(coid, CancelIntent::RiskOff);
            }
            for ei in 0..self.extra_engines.len() {
                let coids: Vec<String> = self.extra_engines[ei]
                    .1
                    .registry
                    .iter()
                    .filter(|(_, mo)| !is_terminal_status(mo.status))
                    .map(|(coid, _)| coid.clone())
                    .collect();
                for coid in &coids {
                    self.extra_engines[ei].1.cancel_order_with_intent(coid, CancelIntent::RiskOff);
                }
            }
            self.pump_client();
        }));
        if sweep.is_err() {
            self.fault = Some(format!(
                "{} (cancel-all sweep also panicked)",
                self.fault.take().unwrap_or_default()
            ));
        }
        self.dirty = true;
    }

    /// THE OPT-IN SHUTDOWN SWEEP ([`crate::CoreConfig::cancel_orders_on_shutdown`]): cancel every
    /// resting order on the way out, so a stopped daemon leaves nothing live at the venue.
    ///
    /// Called from the teardown block in `CoreThread::run` and **only** from there, at the one
    /// instant that works: after the final drain (so an order that filled a moment ago is already
    /// terminal and is not pointlessly cancelled) and BEFORE `engine.shutdown()` detaches the
    /// client. It routes through the single order-write path
    /// ([`vike_exec::OrderIntent::MassCancel`] via `apply_intent`) rather than walking the registry
    /// itself, for the reason [`CoreThread::sweep_deadman`] does the same: mint/route/gate, the
    /// armed conditional books and the held bracket exits are all that path's job, and a second
    /// hand-rolled walk would drift from it. `MassCancel` is ungated by `TradingState`, so this
    /// works from a `Halted` core too — which is the state a crashed strategy leaves behind.
    ///
    /// BEST-EFFORT, and deliberately so. `cancel_batch` is fire-and-forget over each venue's
    /// `ExecActor` channel; the ACKs would arrive long after we are gone. What IS guaranteed is
    /// DELIVERY: the cancels are queued to the actor before the detach, and the actor's own
    /// teardown drains its channel in order before it returns. Everything here sits inside the
    /// caller's `vike_ops::shutdown::run_with_deadline` budget, which abandons whatever is still in
    /// flight and lets the process exit — a wedged venue delays the stop, it cannot prevent it.
    ///
    /// It cancels; it does NOT flatten. See the config field for why.
    pub(crate) fn cancel_resting_on_shutdown(&mut self) {
        // Count first, over the SAME predicate `enter_safe_state`'s sweep filters on ("this order
        // will not respond to a cancel"). An empty book returns without touching a client, so the
        // flag costs one registry walk and nothing else when there is nothing to do.
        let resting: usize = (0..=self.extra_engines.len())
            .map(|idx| {
                self.eng(idx).registry.values().filter(|mo| !is_terminal_status(mo.status)).count()
            })
            .sum();
        if resting == 0 {
            tracing::info!(
                target: "vike_core::core",
                "shutdown: cancel_orders_on_shutdown is ON and nothing was resting to cancel"
            );
            return;
        }
        // LOUD on purpose: this is a policy the operator opted into, it destroys live venue state,
        // and it is the last thing this process does. A stop that silently cancelled a book would
        // be indistinguishable from the venue pulling it.
        tracing::warn!(
            target: "vike_core::core",
            resting,
            "shutdown: cancelling every resting order (cancel_orders_on_shutdown). \
             POSITIONS ARE NOT CLOSED — this leaves no ORDERS, not a flat book."
        );
        let now = self.config.clock.now_ms();
        // RISK-OFF, declared: this is the LAST thing the process does, so a cancel a venue held
        // back would never be re-offered — the order would simply survive the stop.
        self.apply_intent_with_cancel_intent(
            vike_exec::OrderIntent::MassCancel { venue: None, symbol: None, account: None },
            now,
            CancelIntent::RiskOff,
        );
        self.note(format!("shutdown: cancelled {resting} resting order(s); positions untouched"));
        self.dirty = true;
    }
}
