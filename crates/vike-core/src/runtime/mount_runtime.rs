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
    pub(crate) fn mount_strategy_runtime(&mut self, spec: vike_execution::MountSpec) {
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
        // `engine_index_for_route_key`'s primary fallback and be booked against the wrong venue's
        // account.
        //
        // ⚠ THE SAME FUNCTION `resolve_mount_engines` calls at spawn, and that is the point rather
        // than tidiness. This arm and that one used to be hand-written twins, and they had
        // drifted: the spawn door refused the ACCOUNT miss and fell through to engine zero on the
        // VENUE miss, while this one refused both. One call means the RULE and the WORDING cannot
        // differ by door again. The SHAPE is the same too since the spawn door stopped panicking:
        // a live daemon stays up through a bad mount, a note and no slot (spawn: a tombstoned
        // slot, an `error!` and the same note).
        let mounted = mounted_route_keys(&self.engine, &self.extra_engines);
        let engine_index =
            match mount_engine_resolution(&mounted, &spec.venue, spec.account.as_ref()) {
                Ok(i) => i,
                Err(reason) => {
                    self.note(format!("MOUNT REFUSED: `{mid}` {reason}"));
                    return;
                }
            };
        // (3) Resolve through the injected factory — composition-root/user code (a registry
        // build, a Rhai compile), guarded like every other strategy-code call site in this file.
        let Some(factory) = self.config.strategy_factory.as_mut() else {
            self.note(format!(
                "MOUNT REFUSED: `{mid}` — no strategy factory is configured on this core \
                 (runtime mounts are disabled on this binary)"
            ));
            return;
        };
        let (strategy, strategy_file) = match catch_unwind(AssertUnwindSafe(|| factory(&spec))) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                self.note(format!("MOUNT REFUSED: `{mid}` — {e}"));
                return;
            }
            Err(payload) => {
                self.note(format!(
                    "MOUNT REFUSED: `{mid}` — strategy factory panicked: {}",
                    payload_text(&*payload)
                ));
                return;
            }
        };
        let mount = Mount {
            // The code the composition root stamped on the spec (decision 0270); `None` mints `0000`.
            order_client_id_code: spec.order_client_id_code,
            account: spec.account.clone(),
            venue: spec.venue.clone(),
            symbol: spec.symbol.clone(),
            interval: spec.interval.clone(),
            strategy,
            symbols: Vec::new(),
            underlying_symbol: None,
            controller_id: spec.controller_id.clone(),
        };
        if !self.install_mount(&mid, engine_index, mount) {
            return;
        }
        // (6c) The strategy FILE it opened on: journaled once, now, and kept for its `Snapshot` rows; and its `fill`
        // word, which a paper engine fills its bar orders by (`CoreConfig::mount_fills_at_close`), exactly as a boot
        // mount's. A spec no file names drops whatever an earlier mount of this id ran.
        let index = self.mounts.len() - 1;
        let fill_at_close = strategy_file.as_ref().is_some_and(|f| f.fill_at_close);
        match strategy_file {
            Some(f) => self.config.mount_strategy_files.insert(mid.clone(), f.file),
            None => self.config.mount_strategy_files.remove(&mid),
        };
        if fill_at_close {
            self.config.mount_fills_at_close.insert(mid.clone());
        } else {
            self.config.mount_fills_at_close.remove(&mid);
        }
        self.journal_strategy_file(index);
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

    /// **Mount the strategies the boot DEACTIVATED** ([`vike_execution::AccountChange::AddMounts`]): their account's
    /// venue was not connected at the start (owner 2026-11-14, decision 0156's 2026-11-14 amendment), and its engine
    /// has just joined; or their strategy file was still building ([`CoreConfig::mounts_awaiting_strategy`]), and its
    /// strategy has arrived. Each goes through the runtime mount's own checks and steps (the duplicate id, the engine
    /// its account names, its inputs, its legs, its state), and none is written to the topology sidecar: it is the
    /// profile's mount, which the next boot builds again. Its strategy FILE is the one the boot named for its id
    /// ([`CoreConfig::mount_strategy_files`]), journaled now as a boot mount's is at assembly. A refusal is a note, as
    /// a runtime mount's is.
    ///
    /// A mount of an id the assembly PARKED takes only its strategy from the parcel: the parked mount's own fields
    /// (its account among them) are the ones the boot built. One whose account is listed NOT RUNNING waits for that
    /// account (`CoreThread::awaiting_account`) and mounts when its engine joins, never refused for it.
    pub(crate) fn add_mounts_hot(&mut self, parcel: Box<dyn std::any::Any + Send>) {
        let Ok(mounts) = parcel.downcast::<Vec<Mount>>() else {
            self.note(
                "MOUNT REFUSED: the deactivated mounts arrived as a parcel of another type"
                    .to_string(),
            );
            return;
        };
        for mount in *mounts {
            let mid = crate::strategy_state::mount_id_with(
                mount.controller_id.as_deref(),
                &mount.venue,
                &mount.symbol,
                &mount.interval,
            );
            let id_of = |m: &Mount| {
                crate::strategy_state::mount_id_with(
                    m.controller_id.as_deref(),
                    &m.venue,
                    &m.symbol,
                    &m.interval,
                )
            };
            let mount = match self.awaiting_strategy.iter().position(|m| id_of(m) == mid) {
                Some(at) => Mount { strategy: mount.strategy, ..self.awaiting_strategy.remove(at) },
                None => mount,
            };
            if (0..self.mounts.len()).any(|i| self.mounts[i].is_some() && self.mount_ids[i] == mid)
            {
                self.note(format!(
                    "MOUNT REFUSED: duplicate strategy-mount id `{mid}` — a mount with this identity \
                     is already live"
                ));
                continue;
            }
            let mounted = mounted_route_keys(&self.engine, &self.extra_engines);
            let engine_index =
                match mount_engine_resolution(&mounted, &mount.venue, mount.account.as_ref()) {
                    Ok(i) => i,
                    Err(reason) => {
                        let label = mount.account.clone().unwrap_or_default();
                        let key =
                            vike_model::accounts::account_keys::route_key_of(&mount.venue, &label);
                        if self.config.not_running.iter().any(|nr| nr.route_key() == key) {
                            self.note(format!(
                            "MOUNT `{mid}` waits for its account `{key}`, which does not run: it \
                                 mounts when the account starts"
                        ));
                            self.awaiting_account.push(mount);
                        } else {
                            self.note(format!("MOUNT REFUSED: `{mid}` {reason}"));
                        }
                        continue;
                    }
                };
            let said = format!("{}/{} @ {}", mount.venue, mount.symbol, mount.interval);
            if self.install_mount(&mid, engine_index, mount) {
                self.journal_strategy_file(self.mounts.len() - 1);
                self.note(format!(
                    "MOUNTED strategy `{mid}` ({said}): the start deactivated it (its account's venue \
                     was not connected, or its strategy file was still building), and it runs now"
                ));
            }
        }
    }

    /// Steps (3b) to (6b) of a runtime mount, over a `mount` whose strategy exists and whose engine `engine_index`
    /// is resolved: [`Self::mount_strategy_runtime`]'s and [`Self::add_mounts_hot`]'s one body. `false` when it
    /// was refused (a note says why) and nothing was installed.
    fn install_mount(&mut self, mid: &str, engine_index: usize, mut mount: Mount) -> bool {
        // (3b) Its INPUTS, read through the host's opener (decision 0271), the spawn door's rule
        // (`open_mount_inputs`): one it cannot read refuses the mount by name.
        let inputs = match open_mount_inputs(
            self.config.input_opener.as_mut(),
            &*mount.strategy,
            (&mount.venue, &mount.symbol, &mount.interval),
            &mut self.input_keys,
        ) {
            Ok(inputs) => inputs,
            Err(reason) => {
                self.note(format!("MOUNT REFUSED: `{mid}` {reason}"));
                return false;
            }
        };
        // (3c) Its LEGS: what the strategy trades beside its own symbol, declared by the rule a profile
        // mount declares through (`MountLeg::declare_strategy_legs`), so an order naming one is routed
        // there rather than refused as undeclared. Only a leg this mount's engine TRADES is declared: one
        // it does not (a paper core built without a book for it, a live engine that never admitted it)
        // would be booked on another symbol's single-symbol book, so it stays undeclared, its orders
        // refused by name, and the note says so. A declaration that panics declares nothing, as the
        // daemon's resolve treats one. A leg the control door writes another way refuses the mount by
        // name, as it stops the daemon's resolve (the strategy's orders would miss the leg's book).
        let mut legs: Vec<MountLeg> = Vec::new();
        match catch_unwind(AssertUnwindSafe(|| mount.strategy.symbols())) {
            Ok(symbols) => {
                if let Err(reason) =
                    MountLeg::declare_strategy_legs(&mut legs, &mount.venue, &mount.symbol, symbols)
                {
                    self.note(format!("MOUNT REFUSED: `{mid}` {reason}"));
                    return false;
                }
            }
            Err(payload) => self.note(format!(
                "MOUNT `{mid}`: the strategy's symbol declaration panicked ({}): it trades its own \
                 symbol alone",
                payload_text(&*payload)
            )),
        }
        // A leg named by its account books on THAT account's engine (decision 0293), judged by the rule a
        // boot mount's legs are; every other leg on this mount's engine, as before.
        let mounted = mounted_route_keys(&self.engine, &self.extra_engines);
        let own = (mount.venue.as_str(), mount.symbol.as_str(), engine_index);
        let leg_engines = match mount_legs_resolution(&mounted, own, &legs) {
            Ok(engines) => engines,
            Err(reason) => {
                self.note(format!("MOUNT REFUSED: `{mid}` {reason}"));
                return false;
            }
        };
        let (mut traded, mut untraded) = (Vec::new(), Vec::new());
        let mut leg_books: Vec<usize> = Vec::new();
        for (leg, engine) in legs.into_iter().zip(leg_engines) {
            let book = engine.unwrap_or(engine_index);
            if self.engine(book).accepts_symbol(&leg.symbol) {
                traded.push(leg);
                leg_books.push(book);
            } else {
                untraded.push(leg);
            }
        }
        let legs = traded;
        if !untraded.is_empty() {
            let names: Vec<&str> = untraded.iter().map(|leg| leg.symbol.as_str()).collect();
            self.note(format!(
                "MOUNT `{mid}`: it also trades {}, which its engine on {} does not trade: undeclared, \
                 so its orders there are refused",
                names.join(", "),
                mount.venue
            ));
        }
        mount.symbols = legs.clone();
        // (4) Durable-state load — the same sidecar read + panic guard `assemble_core` applies at
        // spawn, so a strategy unmounted earlier (or in a prior session) resumes its state.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Some(v) = crate::strategy_state::read_state(dir, mid)
            && let Err(payload) = catch_unwind(AssertUnwindSafe(|| mount.strategy.load_state(&v)))
        {
            tracing::warn!(
                target: "vike_core::strategy_state",
                mount_id = %mid,
                reason = %payload_text(&*payload),
                "strategy load_state panicked — starting fresh"
            );
        }
        // (5) Arm applied-fill capture the way spawn would have: the primary engine collects
        // whenever ANY mount exists (`spawn_core_multi`'s rule), an extra engine when a mount
        // trades its venue (`assemble_core`'s rule), or books a leg the strategy names by its account
        // on another venue (decision 0293, `leg_books`). NEVER disarmed on unmount — a sibling mount
        // may depend on it, and an in-flight order's fill must still be capturable.
        self.engine.collect_applied_fills = true;
        for (i, (_, e)) in self.extra_engines.iter_mut().enumerate() {
            if e.venue == mount.venue || leg_books.contains(&(i + 1)) {
                e.collect_applied_fills = true;
            }
        }
        // (6) Grow EVERY parallel per-mount vector together (same indices — `assemble_core`'s
        // layout law). APPEND-ONLY: an existing slot's index is an attribution key (`order_client_id_mount`
        // values, `strategy_tags`' `{idx}|` prefix), so nothing may ever shift.
        let state =
            if self.config.readiness_gate { MountState::Pending } else { MountState::Ready };
        self.mount_ids.push(mid.to_string());
        self.mount_states.push(state);
        // …its own keyed budget, else the node-wide default: a runtime mount must not slip under
        // the ceiling a boot mount is held to (decision 0249).
        self.mount_budget
            .push(self.config.mount_budgets.get(mid).copied().or(self.config.mount_budget_default));
        self.mount_vs.push((mount.venue.clone(), mount.symbol.clone()));
        // The label its stream already reported, if any (decision 0140): a mount that arrives after
        // the feed labelled the stream must not read "no label".
        self.mount_delays.push(self.delay_of(&mount.venue, &mount.symbol));
        self.mount_inputs.push(inputs);
        // Its closed-trade ledger, once its own ledger is restored below (6b).
        self.mount_closed.push(None);
        self.mount_closed_clock.push(None);
        self.mount_archive.push(Default::default());
        // …and the ENGINE this mount trades on, resolved through the SAME function `assemble_core`
        // uses, so a runtime mount and a spawn mount cannot disagree about which book they are on.
        // Step (2) above resolved it, so the index is the one `assemble_core` would have computed.
        self.mount_engine.push(engine_index);
        self.mount_latched.push(false);
        self.mount_loss_base.push(0.0);
        self.mount_attribution.push(MountAttribution::default());
        self.mount_symbols.push(legs);
        self.mount_schedule.push(self.config.mount_schedules.remove(mid).unwrap_or_default());
        self.mounts.push(Some(mount));
        // (6b) Ownership the order-ownership file restored for THIS mount id and no slot claimed at
        // assemble (decision 0113): a resurrected runtime mount, or a profile mount brought back as
        // a runtime one, takes back its resting orders and its ledger on the NEW slot index.
        let index = self.mounts.len() - 1;
        // The arms its account re-armed at a late join, bound to it as the boot binds a mounted slot's (`late_join.rs`).
        self.bind_awaiting_arm_owners(mid, index);
        // …and none of the ledger of a book its account's late join refused for its tier (the boot's rule).
        self.forget_tier_refused_join_ledger(mid, index, engine_index);
        if let Some(p) = self.pending_owners.remove(mid) {
            let held = p.order_client_ids.len();
            for order in p.order_client_ids {
                self.order_client_id_mount.insert(order.order_client_id.clone(), index);
                self.restore_owned_order(index, order);
            }
            if let Some(ledger) = p.ledger {
                self.mount_attribution[index] = ledger;
            }
            self.note(format!(
                "ORDER OWNERS BOUND: mount `{mid}` took back {held} resting order(s) from before \
                 the restart"
            ));
        }
        // …resuming the journal's closed-trade ledger for this id, or holding the position that restored ledger
        // holds (`closed_trades.rs`'s "A restart").
        self.restore_closed_trades(index, self.engine.now_ms);
        self.recompute_mount_gates();
        true
    }

    /// RUNTIME strategy UNMOUNT (split-plane B5) — the [`Command::UnmountStrategy`] arm's body.
    ///
    /// ⚠ **THE RESTING-ORDER DECISION** (the spec's named trading-safety question): unmount
    /// CANCELS the mount's live orders BEFORE removal — the safe default. After removal nothing
    /// routes `on_fill`/`on_order_event` to this strategy any more, so an order left resting would
    /// be an UNMANAGED book: a maker's stale quote sitting at the venue until an operator notices,
    /// which is exactly the silent-vanish class the emitter-split contract exists to prevent.
    /// Scope: EXACTLY the mount's attributed order client ids (`order_client_id_mount` — the same attribution
    /// [`Self::latch_mount`] cancels by), so a sibling mount on the same symbol is untouched;
    /// terminal/unknown order client ids are no-ops in the engine's cancel path. It CANCELS; it does NOT
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
        let order_client_ids: Vec<String> = self
            .order_client_id_mount
            .iter()
            .filter(|&(_, &m)| m == i)
            .map(|(c, _)| c.clone())
            .collect();
        let canceled = order_client_ids.len();
        if !order_client_ids.is_empty() {
            // Unclassified on purpose (never `Routine`): once the mount is removed nothing re-runs
            // this, so a cancel a venue held back would leave the UNMANAGED book this decision
            // exists to prevent. Not labeled `RiskOff` either — an unmount is not an emergency; it
            // takes the flatten-safe default, which is what "must not be shed" already means.
            self.apply_intent(OrderIntent::CancelBatch(order_client_ids), now);
        }
        // (2) Durable-state save — the single-slot body of [`Self::save_all_strategy_state`]
        // (same panic guard, same atomic write), so the strategy's state survives to a later
        // re-mount or restart.
        if let Some(dir) = self.config.state_dir.as_ref()
            && let Some(mount) = self.mounts[i].as_ref()
        {
            match catch_unwind(AssertUnwindSafe(|| mount.strategy.save_state())) {
                Ok(Some(v)) => {
                    let sidecar = crate::strategy_state::state_path(dir, &want);
                    if let Err(e) = crate::strategy_state::write_state_atomic(&sidecar, &v) {
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
                        reason = %payload_text(&*payload),
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
        // `order_client_id_mount` values and the journal's `StrategySubmit` provenance already point at this
        // index/id, and reusing it would hand a FUTURE mount the dead strategy's in-flight fills.
        // Every runtime iterator already skips a `None` slot (the take/replace-dance defensive
        // filters); `mount_ids[i]`/`mount_attribution[i]` deliberately STAY, so a straggler fill of a
        // just-canceled order still folds into the dead mount's ledger (the published view's
        // residual row absorbs it — `mount_views` skips the tombstone). Per-slot FEATURE state is
        // cleared so [`Self::recompute_mount_gates`] sees exactly what `assemble_core`'s formulas
        // would; `Ready` keeps `maintain_mount_readiness`'s Pending short-circuit true.
        self.mounts[i] = None;
        self.mount_states[i] = MountState::Ready;
        self.mount_budget[i] = None;
        self.mount_symbols[i].clear();
        self.mount_schedule[i] = MountSchedule::default();
        // Its inputs go; their keys stay in `input_keys`, whose bars the host still sends.
        self.mount_inputs[i] = None;
        // …and its closed trades: a straggler fill reaches no strategy, so none is booked.
        self.mount_closed[i] = None;
        self.mount_closed_clock[i] = None;
        self.mount_archive[i] = Default::default();
        // …and a bar step a shared close held for it (`shared_close.rs`): no strategy is left to step.
        self.shared_closes.retain(|p| p.mount != i);
        self.mount_latched[i] = false;
        self.mount_loss_base[i] = 0.0;
        self.recompute_mount_gates();
        // (4b) …and in the ORDER-OWNERSHIP file (decision 0113): an explicit unmount forgets the
        // mount's ownership and its ledger, so a later boot binds nothing of it to a new mount of
        // the same id. The in-memory entries stay, like `mount_attribution[i]`, for the stragglers.
        if let Some(p) = self.pending_owners.remove(&want) {
            for o in &p.order_client_ids {
                self.restore_unproven.remove(&o.order_client_id);
            }
        }
        // …and the journal's closed trades of that id, so the next `Snapshot` carries none of them.
        self.pending_closed.remove(&want);
        self.restored_orders.retain(|order_client_id, o| {
            let keep = o.mount_id != want;
            if !keep {
                self.restore_unproven.remove(order_client_id);
            }
            keep
        });
        self.record_owner(crate::order_owners::OrderOwnerRecord::Unmount {
            mount_id: want.clone(),
        });
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
        // Parallel to `mounts`, so a slot added or removed re-indexes it.
        self.rebuild_leg_values();
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
