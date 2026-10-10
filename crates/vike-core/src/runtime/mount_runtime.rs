//! Runtime strategy mount / unmount (`Command::MountStrategy` / `UnmountStrategy`) and the mount-gate recompute.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// RUNTIME strategy MOUNT (split-plane B5) — the [`Command::MountStrategy`] arm's body. Adds
    /// one mount to the live slot vector WITHOUT a restart, resolving the spec through the
    /// composition root's injected [`CoreConfig::strategy_factory`].
    ///
    /// COLD PATH: runs only when a mount command arrives (an occasional operator verb, the
    /// `UpdateParams` argument). Nothing on the per-event fold changes: the fold keeps reading the
    /// same `any_mount_*` booleans, which [`Self::recompute_mount_gates`] re-derives HERE, off the
    /// event path.
    ///
    /// Every failure is a REFUSAL note in recent-events, never a panic — the runtime twin of
    /// [`assemble_core`]'s duplicate-id PANIC, which deliberately stays: at spawn a duplicate is a
    /// configuration fault caught before a single order exists, while a live core must keep
    /// trading through a bad mount request.
    pub(crate) fn mount_strategy_runtime(&mut self, spec: vike_exec::MountSpec) {
        let mid = crate::strategy_state::mount_id_with(
            spec.controller_id.as_deref(),
            &spec.venue,
            &spec.symbol,
            &spec.interval,
        );
        // (1) Duplicate check over LIVE slots only: a tombstoned (unmounted) slot keeps its id as
        // the label of its residual attribution ledger, but is no longer an identity holder — a
        // re-mount of the same id is legitimate and lands on a FRESH slot.
        if (0..self.mounts.len()).any(|i| self.mounts[i].is_some() && self.mount_ids[i] == mid) {
            self.note(format!(
                "MOUNT REFUSED: duplicate strategy-mount id `{mid}` — a mount with this identity \
                 is already live (give the new mount a distinct controller_id)"
            ));
            return;
        }
        // (2) The venue — AND THE ACCOUNT — must name an engine this core already runs: a mount
        // cannot conjure one, and orders from an engine-less venue would fall through
        // `engine_idx_for_route_key`'s primary fallback and be booked against the wrong venue's
        // account.
        //
        // ⚠ THE SAME FUNCTION `mount_engine_idx` calls at spawn, and that is the point rather than
        // tidiness. This arm and that one used to be hand-written twins, and they had drifted: the
        // spawn door refused the ACCOUNT miss and fell through to engine zero on the VENUE miss,
        // while this one refused both. One call means the RULE and the WORDING cannot differ by
        // door again. What still differs is the SHAPE of the answer, which is requirement-shaped: a
        // live daemon must stay up through a bad mount command (a note, and no slot created), while
        // a spawn-time miss is a configuration fault caught before a single order exists (a panic).
        let mounted = mounted_route_keys(&self.engine, &self.extra_engines);
        if let Err(reason) = mount_engine_resolution(&mounted, &spec.venue, spec.account.as_ref()) {
            self.note(format!("MOUNT REFUSED: `{mid}` {reason}"));
            return;
        }
        // (3) Resolve through the injected factory — composition-root/user code (a registry
        // build, a Rhai compile), guarded like every other strategy-code call site in this file.
        let Some(factory) = self.config.strategy_factory.as_mut() else {
            self.note(format!(
                "MOUNT REFUSED: `{mid}` — no strategy factory is configured on this core \
                 (runtime mounts are disabled on this binary)"
            ));
            return;
        };
        let strategy = match catch_unwind(AssertUnwindSafe(|| factory(&spec))) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                self.note(format!("MOUNT REFUSED: `{mid}` — {e}"));
                return;
            }
            Err(payload) => {
                self.note(format!(
                    "MOUNT REFUSED: `{mid}` — strategy factory panicked: {}",
                    panic_text(payload)
                ));
                return;
            }
        };
        let mut mount = StrategyMount {
            account: spec.account.clone(),
            venue: spec.venue.clone(),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            strategy,
            symbols: Vec::new(),
            underlying_symbol: None,
            controller_id: spec.controller_id.clone(),
        };
        // (4) Durable-state load — the same sidecar read + panic guard `assemble_core` applies at
        // spawn, so a strategy unmounted earlier (or in a prior session) resumes its state.
        if let Some(dir) = self.config.state_dir.as_ref() {
            let sidecar = crate::strategy_state::sidecar_path(dir, &mid);
            if let Some(v) = crate::strategy_state::read_json(&sidecar)
                && let Err(payload) =
                    catch_unwind(AssertUnwindSafe(|| mount.strategy.load_state(&v)))
            {
                tracing::warn!(
                    target: "vike_core::strategy_state",
                    mount_id = %mid,
                    reason = %panic_text(payload),
                    "strategy load_state panicked — starting fresh"
                );
            }
        }
        // (5) Arm applied-fill capture the way spawn would have: the primary engine collects
        // whenever ANY mount exists (`spawn_core_multi`'s rule), an extra engine when a mount
        // trades its venue (`assemble_core`'s rule; a runtime mount declares no cross-venue legs,
        // so venue equality is the whole test). NEVER disarmed on unmount — a sibling mount may
        // depend on it, and an in-flight order's fill must still be capturable.
        self.engine.collect_applied_fills = true;
        for (_, e) in self.extra_engines.iter_mut() {
            if e.venue == mount.venue {
                e.collect_applied_fills = true;
            }
        }
        // (6) Grow EVERY parallel per-mount vector together (same indices — `assemble_core`'s
        // layout law). APPEND-ONLY: an existing slot's index is an attribution key (`coid_mount`
        // values, `strategy_tags`' `{idx}|` prefix), so nothing may ever shift.
        let state =
            if self.config.readiness_gate { MountState::Pending } else { MountState::Ready };
        self.mount_ids.push(mid.clone());
        self.mount_states.push(state);
        self.mount_budget.push(self.config.mount_budgets.get(&mid).copied());
        self.mount_vs.push((mount.venue.clone(), mount.symbol.clone()));
        // …and the ENGINE this mount trades on, resolved through the SAME function `assemble_core`
        // uses, so a runtime mount and a spawn mount cannot disagree about which book they are on.
        // Step (2) above has already refused the miss, so the panic arm is unreachable from here.
        self.mount_engine.push(mount_engine_idx(&self.engine, &self.extra_engines, &mount, &mid));
        self.mount_latched.push(false);
        self.mount_attr.push(MountAttribution::default());
        self.mount_symbols.push(Vec::new());
        self.mount_schedule.push(self.config.mount_schedules.remove(&mid).unwrap_or_default());
        self.mounts.push(Some(mount));
        // (6b) Ownership the order-ownership file restored for THIS mount id and no slot claimed at
        // assemble (decision 0113): a resurrected runtime mount, or a profile mount brought back as
        // a runtime one, takes back its resting orders and its ledger on the NEW slot index.
        let idx = self.mounts.len() - 1;
        if let Some(p) = self.pending_owners.remove(&mid) {
            let held = p.coids.len();
            for coid in p.coids {
                self.coid_mount.insert(coid, idx);
            }
            if let Some(ledger) = p.ledger {
                self.mount_attr[idx] = ledger;
            }
            self.note(format!(
                "ORDER OWNERS BOUND: mount `{mid}` took back {held} resting order(s) from before \
                 the restart"
            ));
        }
        self.recompute_mount_gates();
        // (7) Record the mount in the TOPOLOGY sidecar (B5 residual closed) — daemon-level STATE,
        // deliberately NOT a journal write: the `journaled` match in `dispatch` still excludes
        // this command, because the mount-less replay core (no `strategy_factory`) could never
        // re-fold it and the replay determinism fence must stay over exactly what re-folds. The
        // sidecar rides the same `state_dir` gate and the same atomic-rename shape as the
        // strategy-state sidecar this arm loads in step (4); the composition root replays it at
        // startup through this very command path — [`crate::mount_topology`]'s module doc is the
        // seam's authority. SUCCESS-ONLY and best-effort: a refusal above records nothing, and a
        // write failure warns without unwinding the mount that already landed.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Err(e) = crate::mount_topology::upsert(
                dir,
                crate::mount_topology::MountRecord::stamped(
                    spec.clone(),
                    self.config.clock.now_ms(),
                ),
            )
        {
            tracing::warn!(
                target: "vike_core::mount_topology",
                mount_id = %mid,
                error = %e,
                "failed to record runtime mount in the topology sidecar (best-effort) — the \
                 mount is live but will NOT resurrect on restart"
            );
        }
        self.note(format!(
            "MOUNTED strategy `{mid}` ({}/{} @ {})",
            spec.venue, spec.symbol, spec.interval
        ));
    }

    /// RUNTIME strategy UNMOUNT (split-plane B5) — the [`Command::UnmountStrategy`] arm's body.
    ///
    /// ⚠ **THE RESTING-ORDER DECISION** (the spec's named trading-safety question): unmount
    /// CANCELS the mount's live orders BEFORE removal — the safe default. After removal nothing
    /// routes `on_fill`/`on_order_event` to this strategy any more, so an order left resting would
    /// be an UNMANAGED book: a maker's stale quote sitting at the venue until an operator notices,
    /// which is exactly the silent-vanish class the emitter-split contract exists to prevent.
    /// Scope: EXACTLY the mount's attributed coids (`coid_mount` — the same attribution
    /// [`Self::latch_mount`] cancels by), so a sibling mount on the same symbol is untouched;
    /// terminal/unknown coids are no-ops in the engine's cancel path. It CANCELS; it does NOT
    /// flatten — the standing stop-policy law ([`CoreConfig::cancel_orders_on_shutdown`]'s doc):
    /// the mount's attributed position stays open and visible in the residual attribution row, and
    /// closing it is an operator decision (`Flatten`), never a side effect of unmounting. The
    /// cancels are replay-neutral exactly like `latch_mount`'s (the venue's authoritative
    /// `OrderCanceled` journals as its own `Ingest::Event`).
    pub(crate) fn unmount_strategy_runtime(&mut self, controller_id: &str) {
        // The identity law verbatim: ids are STORED sanitized (`mount_id_with`), so the requested
        // id is sanitized the same way — `maker-a` finds the mount keyed `maker_a`. An
        // empty/whitespace id can name nothing.
        if controller_id.trim().is_empty() {
            self.note("UNMOUNT REFUSED: empty mount id".to_string());
            return;
        }
        let want = crate::strategy_state::mount_id_with(Some(controller_id), "", "", "");
        let Some(i) =
            (0..self.mounts.len()).find(|&i| self.mounts[i].is_some() && self.mount_ids[i] == want)
        else {
            self.note(format!("UNMOUNT REFUSED: no live strategy mount with id `{want}`"));
            return;
        };
        let now = self.engine.now_ms;
        // (1) Cancel this mount's attributed live orders (the decision documented above).
        let coids: Vec<String> =
            self.coid_mount.iter().filter(|&(_, &m)| m == i).map(|(c, _)| c.clone()).collect();
        let canceled = coids.len();
        if !coids.is_empty() {
            // Unclassified on purpose (never `Routine`): once the mount is removed nothing re-runs
            // this, so a cancel a venue held back would leave the UNMANAGED book this decision
            // exists to prevent. Not labeled `RiskOff` either — an unmount is not an emergency; it
            // takes the flatten-safe default, which is what "must not be shed" already means.
            self.apply_intent(OrderIntent::CancelBatch(coids), now);
        }
        // (2) Durable-state save — the single-slot body of [`Self::save_all_strategy_state`]
        // (same panic guard, same atomic write), so the strategy's state survives to a later
        // re-mount or restart.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Some(mount) = self.mounts[i].as_ref()
        {
            match catch_unwind(AssertUnwindSafe(|| mount.strategy.save_state())) {
                Ok(Some(v)) => {
                    let sidecar = crate::strategy_state::sidecar_path(dir, &want);
                    if let Err(e) = crate::strategy_state::write_json_atomic(&sidecar, &v) {
                        tracing::warn!(
                            target: "vike_core::strategy_state",
                            mount_id = %want,
                            error = %e,
                            "failed to save strategy state sidecar on unmount (best-effort)"
                        );
                    }
                }
                Ok(None) => {}
                Err(payload) => {
                    tracing::warn!(
                        target: "vike_core::strategy_state",
                        mount_id = %want,
                        reason = %panic_text(payload),
                        "strategy save_state panicked on unmount (best-effort)"
                    );
                }
            }
        }
        // (3) Retire the slot's tag-registry entries (strategy-local names keyed `{i}|…`): the
        // orders they point at were just canceled, and nothing else ever prunes the map.
        let prefix = format!("{i}|");
        self.strategy_tags.retain(|k, _| !k.starts_with(&prefix));
        // (4) Tombstone: the slot goes `None` PERMANENTLY and its index is never reused —
        // `coid_mount` values and the journal's `StrategySubmit` provenance already point at this
        // index/id, and reusing it would hand a FUTURE mount the dead strategy's in-flight fills.
        // Every runtime iterator already skips a `None` slot (the take/replace-dance defensive
        // filters); `mount_ids[i]`/`mount_attr[i]` deliberately STAY, so a straggler fill of a
        // just-canceled order still folds into the dead mount's ledger (the published view's
        // residual row absorbs it — `mount_views` skips the tombstone). Per-slot FEATURE state is
        // cleared so [`Self::recompute_mount_gates`] sees exactly what `assemble_core`'s formulas
        // would; `Ready` keeps `maintain_mount_readiness`'s Pending short-circuit true.
        self.mounts[i] = None;
        self.mount_states[i] = MountState::Ready;
        self.mount_budget[i] = None;
        self.mount_symbols[i].clear();
        self.mount_schedule[i] = LiveSchedule::default();
        self.mount_latched[i] = false;
        self.recompute_mount_gates();
        // (4b) …and in the ORDER-OWNERSHIP file (decision 0113): an explicit unmount forgets the
        // mount's ownership and its ledger, so a later boot binds nothing of it to a new mount of
        // the same id. The in-memory entries stay, like `mount_attr[i]`, for the stragglers.
        self.pending_owners.remove(&want);
        self.record_owner(crate::order_owners::OwnerRecord::Unmount { mount_id: want.clone() });
        // (5) Forget the mount in the TOPOLOGY sidecar (B5 residual closed) — the atomic-rewrite
        // twin of the mount arm's step (7), and the ONE place a runtime mount is forgotten: a
        // clean shutdown keeps records deliberately (a runtime mount survives a restart the way
        // it survives a crash — "stop the daemon" is not "unmount"), so only this explicit verb
        // removes one. Removing an id that was never recorded (a spawn-time profile mount, or a
        // mount from before `state_dir` was armed) is a quiet no-op inside `remove`. Same
        // not-a-journal-write seam as the mount arm: [`crate::mount_topology`]'s module doc.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Err(e) = crate::mount_topology::remove(dir, &want)
        {
            tracing::warn!(
                target: "vike_core::mount_topology",
                mount_id = %want,
                error = %e,
                "failed to remove the unmounted strategy from the topology sidecar \
                 (best-effort) — a restart may resurrect a mount the operator removed"
            );
        }
        self.note(format!("UNMOUNTED strategy `{want}` — canceled {canceled} attributed order(s)"));
    }

    /// Re-derive the four `any_mount_*` inert-when-empty gates after a runtime mount/unmount —
    /// the SAME formulas [`assemble_core`] evaluates at spawn, kept verbatim so the two sites can
    /// never disagree about when a feature is armed — and then, LAST, the tick lane's table
    /// ([`Self::rebuild_tick_audience`]), which [`assemble_core`] also builds last: it is built
    /// from the audience rule, which reads `mounts`, `mount_symbols` and `any_mount_multi`. COLD
    /// PATH: called only from the two command arms above; the per-event fold keeps reading one bool
    /// per gate, exactly as before.
    ///
    /// ⚠ The ONE place a runtime change to `mounts` or `mount_symbols` reaches the tick table: a
    /// new writer of either must end here, or the tick lane disagrees with the rule about the slots
    /// it changed (a new mount silently deaf to its ticks).
    ///
    /// ⚠ It also bumps [`CoreThread::mount_epoch`], first, which is what tells the published
    /// mount-row cache that the LIST changed (a row added, removed or renamed — nothing in a row's
    /// numbers shows that). A new writer of `mounts` has to end here for that reason too.
    fn recompute_mount_gates(&mut self) {
        self.mount_epoch = self.mount_epoch.wrapping_add(1);
        self.any_mount_budget = self.mount_budget.iter().flatten().any(|b| b.is_active());
        self.any_mount_multi = self.mount_symbols.iter().any(|v| !v.is_empty());
        self.any_mount_ref = self.mounts.iter().enumerate().any(|(i, m)| {
            m.as_ref().is_some_and(|m| {
                self.mount_symbols[i]
                    .iter()
                    .any(|l| l.venue.as_deref().is_some_and(|v| v != m.venue))
            })
        });
        self.any_mount_schedule = self.mount_schedule.iter().any(|s| !s.is_empty());
        // After `any_mount_multi`, which the rule the table is built from reads.
        self.rebuild_tick_audience();
    }
}
