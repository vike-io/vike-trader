//! The snapshot publish (coalesced while busy, AND on every idle transition — so per event on a
//! sporadic feed) + the recent-events drains + the opt-in mmap counter mirror — split
//! out of the runtime fold module (behavior byte-identical; the block moved verbatim). `use super::*`
//! re-exports the parent runtime module's full import set + items, so nothing about resolution changes.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Fold the bus delivery log into the bounded recent-events ring (journal feed).
    ///
    /// PER-MESSAGE PATH - `handle()` calls this after every `Ingest`, inside the
    /// `p99 core-hop < 10us` gate. It captures each event's few rendering inputs
    /// ([`crate::recent::EventNote`]) and RENDERS the line right here, on the fold
    /// ([`Self::note_event`]). Perf audit 2026-07-28 finding #2 (#887) had deferred the render to
    /// publish; #896 moved it back, because publish also runs per event on a sporadic feed.
    pub(crate) fn drain_delivered(&mut self) {
        for ev in self.bus.take_delivered() {
            self.note_event(crate::recent::EventNote::capture(&ev));
        }
    }

    /// Salvage any event the bus was folding when a handler panicked (audit C4): surface the lost
    /// terminal in the recent-events ring (+ the fault snapshot) so the loss is visible. Called from
    /// every fold-guarding `catch_unwind` Err arm — a no-op on the happy path (poisoned is empty).
    pub(crate) fn drain_poisoned(&mut self) {
        for ev in self.bus.take_poisoned() {
            self.note_lost(crate::recent::EventNote::capture(&ev));
        }
    }

    /// Guarded publish: neither the snapshot build nor the GUI's repaint hook may kill
    /// the core thread. On a build/hook panic a minimal fault snapshot is stored so the
    /// GUI still sees terminal state (never a stale Active/no-fault cell).
    pub(crate) fn publish_guarded(&mut self) {
        if catch_unwind(AssertUnwindSafe(|| self.publish())).is_err() {
            self.engine.trading_state = TradingState::Halted;
            let mut snap = CoreSnapshot::empty(&self.engine.venue, &self.engine.symbol);
            self.seq += 1;
            snap.seq = self.seq;
            snap.trading_state = TradingState::Halted;
            snap.fault = Some(
                self.fault.clone().unwrap_or_else(|| "panic during snapshot publish".to_string()),
            );
            self.fault = snap.fault.clone();
            self.snapshot.store(Arc::new(snap));
            self.dirty = false;
        }
    }

    fn publish(&mut self) {
        self.seq += 1;
        let mount_views = self.mount_views();
        let recon = self.recon_block();
        // Live-runtime OTO/OCO: surface the pending bracket exits held off the venue so the GUI can
        // show a resting bracket's protective legs (they are NOT in the registry). Insertion order;
        // empty on every non-bracket run — a single `is_empty` fast path on a publish that runs per
        // event on a sporadic feed.
        let held_exits: Vec<crate::snapshot::HeldOrderView> = self
            .held_orders
            .values()
            .map(|r| crate::snapshot::HeldOrderView {
                client_order_id: r.client_order_id.clone(),
                venue: r.venue.clone(),
                symbol: r.symbol.clone(),
                side: r.side,
                qty: r.qty,
                order_type: r.order_type.clone(),
                price: r.price,
                trigger_price: r.trigger_price,
                parent_order_id: r.parent_order_id.clone(),
            })
            .collect();
        // The ONE maintenance-rate source for the GUI liq badge: the operator's watchdog config
        // when armed, else the same default the watchdog would run with. (A per-symbol
        // venue-reported rate would take precedence here once an adapter carries one — none
        // does today; see `vike_model::pool_breached`'s precedence note.)
        let mm_rate = self
            .config
            .margin_call
            .as_ref()
            .map(|c| c.mm_requirement)
            .unwrap_or_else(|| vike_exec::MarginCallConfig::default().mm_requirement);
        let mut snap = CoreSnapshot::build(
            self.seq,
            &self.engine,
            &self.extra_engines,
            self.config.seed_cash,
            self.config.price_cfg,
            mm_rate,
            // The ring already holds RENDERED lines; publish is a refcount bump per entry (see
            // `CoreThread::note_event`). No mutation, no formatting at publish.
            &self.recent,
            &self.bars,
            &mount_views,
            &self.fault,
            self.market.drops.load(Ordering::Relaxed),
            self.rejected.load(Ordering::Relaxed),
            recon,
            &held_exits,
        );
        // Wave 5d: overlay the reconcile-path per-position coin deltas (side map, not derived from
        // the engine) so the greeks tool can fold a Deribit perp/future leg. Cheap clone on the
        // publish path (per event on a sporadic feed); empty (a no-op clone) until a venue reports
        // a coin delta.
        snap.recon_coin_deltas = self.recon_coin_deltas.clone();
        self.snapshot.store(Arc::new(snap));
        self.dirty = false;
        if let Some(repaint) = &self.config.repaint {
            repaint();
        }
        // Opt-in mmap counter mirror (audit co9): copy the key health counters into the external
        // file at THIS publish — the same point that just built the snapshot above, which is NOT
        // rare: coalesced while busy, but also run on every idle transition, i.e. per event on a
        // sporadic feed, inside the gated core hop. `handle()` is byte-identical; the only new
        // cost is here, at the existing publish. No-op (one `is_some` check) when disabled (default).
        // Collect (an immutable borrow) BEFORE taking the `&mut` on the file, so the two borrows
        // don't overlap; then a pure in-memory copy (the file is pre-mapped) — no syscall, no flush.
        if self.counters.is_some() {
            let c = self.collect_counters();
            let now = self.config.clock.now_ms();
            if let Some(cf) = self.counters.as_mut() {
                cf.write(now, &c);
            }
        }
    }

    /// Build the GUI-facing `ReconBlock` from the held-alert store (Task 17): one `ReconAlertView`
    /// per currently-held alert (insertion order — oldest first) plus the last-pass timestamp.
    /// Called at every publish, like `mount_views` — and publish runs per event on a sporadic
    /// feed, so this is inside the gated core hop: O(held alerts), empty on a box with none held.
    fn recon_block(&self) -> crate::snapshot::ReconBlock {
        crate::snapshot::ReconBlock {
            alerts: self
                .recon_alerts
                .iter()
                .map(|(&id, a)| crate::snapshot::ReconAlertView {
                    id,
                    kind: format!("{:?}", a.kind),
                    detail: a.detail.clone(),
                    proposed_event_count: a.proposed_events.len(),
                    // The raising pass's own routing key — the venue itself for a venue with one
                    // account, so this reads as the exchange id on every box with no `[accounts]`
                    // table and names the book on one that has them.
                    account: a.route_key.clone(),
                })
                .collect(),
            last_pass_ts: self.recon_last_pass_ts,
        }
    }

    /// Snapshot the key runtime health counters for the mmap mirror (audit co9), SUMMED across the
    /// primary engine + every extra engine so a cross-venue core reports totals. `conflated_market_
    /// drops` and `rejected_commands` are the runtime-wide atomics; the rest are per-engine folds
    /// (`stranded_terminal_drops`/`dropped_terminal_on_live`/`dropped_unknown_coid`/
    /// `dropped_nonfinite`), each added up over every engine; the per-engine `exec_db`
    /// queue-depth/error-count this sentence once ended on went with the SQLite sink (below).
    /// Reads only — called from `publish` while the mirror is enabled, so per event on a sporadic
    /// feed (never from `handle()` itself). The `exec_db_*` slots
    /// stay 0 (the SQLite audit sink was retired; durable persistence is the vike-data journal now)
    /// but remain in the fixed mmap wire order per `COUNTER_NAMES`'s append-only rule.
    fn collect_counters(&self) -> crate::counters::Counters {
        let mut c = crate::counters::Counters {
            conflated_market_drops: self.market.drops.load(Ordering::Relaxed),
            rejected_commands: self.rejected.load(Ordering::Relaxed),
            ..Default::default()
        };
        for e in std::iter::once(&self.engine).chain(self.extra_engines.iter().map(|(_, e)| e)) {
            c.stranded_terminal_drops += e.stranded_terminal_drops;
            c.dropped_terminal_on_live += e.dropped_terminal_on_live;
            c.dropped_unknown_coid += e.dropped_unknown_coid;
            c.dropped_nonfinite += e.dropped_nonfinite;
        }
        c
    }

    /// Write a full-state `Snap` record (spec §A "snapshot-as-command"): every engine (primary +
    /// extras) via [`ExecutionEngine::snapshot_state`], the coid `(session, seq)` for restart
    /// continuity, and `state_hash` as the determinism fence the T5 replay checks against. Asks the
    /// journal's syncer thread to carry the checkpoint to disk (a Snap is the durability checkpoint
    /// the next replay resumes from) and resets the cadence counter. No-op when journaling is off,
    /// so callers may invoke it unconditionally.
    ///
    /// # ⚠ This runs on the FOLD THREAD — the one the `p99 < 10µs` gate protects
    ///
    /// Both cadence call sites (`dispatch`'s tail and `pump_client`'s [`Self::maybe_cadence_snap`])
    /// fire from inside the single-writer fold, once per `snapshot_every` journaled records — 1024
    /// at the production default. So **nothing in here may block**, and until #932 something did:
    /// this function ended with `CommandJournal::flush()`, the blocking whole-mapping `msync` whose
    /// own doc says it is "not safe to call from the vike-core fold". #929 had moved the journal's
    /// other two blocking `msync`s off this thread and left this one, invisibly — `runtime_latency`
    /// pinned `snapshot_every = u64::MAX`, so no snapshot ever fired inside a measured hop.
    ///
    /// MEASURED on the latency box (cores 28-31, `SCHED_FIFO 50`, 100 000 hops, 64 MiB segment, ~287 B/record
    /// ⇒ ~287 KB dirty per snap), production config vs the same run with `snapshot_every = u64::MAX`:
    ///
    /// ```text
    ///   variant       p99      p99.9     max        hops>100µs
    ///   journal       5.2 µs   38.6 µs    77.7 µs      0        (no snap fires)
    ///   journal-snap  6.1 µs  153.9 µs    14.1 ms    102        (97 snaps + jitter)
    /// ```
    ///
    /// One hop over 100 µs per snapshot, near-exactly (`snaps_expected=97`), and a worst hop of
    /// 10.7–16.7 ms across four reps — ~1 400x the budget. One rep also breached the plan gate
    /// itself (`p99 = 10 059 ns`). `tests/runtime_latency.rs`'s `journal-snap` variant is that
    /// measurement, kept as a gate so the call cannot come back unnoticed.
    ///
    /// The cure is the mechanism #929 already built: post the watermark to the journal's syncer
    /// thread and return (`queue_sync`). The snapshot BUILD below stays here — it is pure CPU,
    /// O(engines + arms + legs + mounts), and runs at snapshot cadence, not per message.
    pub(crate) fn write_snap(&mut self, now_ms: i64) {
        if self.journal.is_none() {
            return;
        }
        let mut engines: Vec<vike_exec::EngineSnapshot> =
            Vec::with_capacity(1 + self.extra_engines.len());
        engines.push(self.engine.snapshot_state());
        for (_, e) in &self.extra_engines {
            engines.push(e.snapshot_state());
        }
        let (coid_session, coid_seq) = self.coid_gen.state();
        let hash = vike_exec::state_hash(&engines);
        // `arm_seq` rides the Snap next to `coid_seq`, mirroring it exactly: both are the id
        // counters a restart must resume, and the Snap is the ONE restore base — a value derived
        // from the surviving record set instead (the pre-v7 shape) is prune-UNSAFE, because
        // `prune_before_latest_snap` deletes whole early segments out from under the count.
        let arm_seq = self.arm_seq;
        // Emulator PR-3: the resting conditional BOOKS ride the Snap too — the only durable home
        // for a trailing arm's CURRENT ratcheted extreme (it moves on non-journaled market data,
        // so neither the write-ahead ARM `Cmd` nor the `ConditionalArmed` seed record can carry
        // it). Captured in fire order (books map insertion order, then each book's). O(arms) at
        // snapshot cadence, zero cost on the per-message fold. Deliberately NOT folded into
        // `hash` (which stays `state_hash(&engines)`): book contents move fenced state only
        // through a FIRE, and every fire has its own write-ahead record — see `crate::replay`.
        let mut conditionals: Vec<vike_journal::SnapConditional> = Vec::new();
        for ((venue, symbol), book) in &self.conditional_books {
            for (arm_id, o) in book.iter() {
                conditionals.push(vike_journal::SnapConditional {
                    arm_id: arm_id.to_string(),
                    terms: vike_journal::ConditionalRecord {
                        venue: venue.clone(),
                        symbol: symbol.clone(),
                        side: o.order.side,
                        qty: o.order.size,
                        price: o.order.price,
                        trail: o.order.trail,
                        extreme: o.order.extreme,
                        trigger_by: o.trigger_by,
                    },
                });
            }
        }
        // Live-runtime OTO/OCO: the resting contingency book rides the Snap too — the ONLY durable
        // home for a HELD exit (a leg the runtime is keeping off the venue until its parent fills;
        // an ACTIVE leg is already in `engines` via the registry). Captured in the book's own
        // insertion order (arm/cancel iteration order); each held leg carries its resolved request
        // so a restart can re-submit it. O(legs) at snapshot cadence, nothing on the per-message
        // fold. NOT folded into `hash` (same fence argument as the conditionals above): the book
        // moves fenced state only through a fill that releases/cancels, and that fill is itself a
        // journaled `Cmd` event replay re-folds through the re-seeded book — see `crate::replay`.
        let contingencies: Vec<vike_journal::SnapContingency> = self
            .contingency
            .snapshot()
            .into_iter()
            .map(|(coid, parent, linked, active)| vike_journal::SnapContingency {
                held_request: self.held_orders.get(&coid).cloned(),
                coid,
                parent,
                linked,
                active,
            })
            .collect();
        // Multi-mount durability (gap D): the per-mount ATTRIBUTION ledgers ride the Snap too — the
        // only durable home they have. Attribution deliberately lives one layer up from `Account`
        // (which is parity-gated and wire-pinned), so nothing in `engines` carries it, and without
        // this a restart resumed every mount ledger at zero while `Account.realized_pnl` kept the
        // same PnL — mount sums silently stopped equalling the account total. One row per mount slot,
        // keyed by `mount_id` so a re-ordered mount list restores onto the right mounts. O(mounts) at
        // snapshot cadence, nothing on the per-message fold; NOT folded into `hash` (the ledger is a
        // read-only view, exactly like the two books above).
        // LIVE slots only (split-plane B5): a runtime-UNMOUNTED slot is a tombstone whose id may
        // be re-mounted onto a fresh slot later in the same session — capturing both would write
        // two rows under one `mount_id`, and the restore's first-match fold would seed the NEW
        // mount with the DEAD strategy's ledger. The tombstone's residual PnL is deliberately not
        // restored anywhere (its mount no longer exists after a restart), exactly like a
        // pre-restart manual ticket's.
        let mount_attr: Vec<vike_journal::SnapMountAttr> = self
            .mount_ids
            .iter()
            .zip(self.mount_attr.iter())
            .zip(self.mounts.iter())
            .filter(|(_, slot)| slot.is_some())
            .map(|((mount_id, a), _)| vike_journal::SnapMountAttr {
                mount_id: mount_id.clone(),
                size: a.size,
                avg_px: a.avg_px,
                realized_pnl: a.realized_pnl,
                fees_paid: a.fees_paid,
            })
            .collect();
        let j = self.journal.as_mut().unwrap();
        j.append_snap(
            now_ms,
            &engines,
            &coid_session,
            coid_seq,
            arm_seq,
            &conditionals,
            &contingencies,
            &mount_attr,
            hash,
        )
        .expect("journal snap");
        // NOT `j.flush()` — see this function's doc. One atomic store, one atomic swap and at most
        // one channel send; the `msync` itself happens on the journal's syncer thread, which this
        // thread never joins and never waits on. The watermark is monotonic, so a syncer that is
        // still busy with an earlier request coalesces this one instead of queueing behind it —
        // the backlog cannot grow and this call can never block, whatever the disk is doing.
        //
        // Durability is unchanged in every case that matters. Per the spec's §A1.1 contract an
        // un-synced tail survives a process crash / OOM-kill / clean reboot (the pages are the OS
        // page cache), a hard power cut is repaired by the post-replay venue reconcile (§A4), and a
        // CLEAN stop still forces the tail: the exit `Snap` this same function writes during
        // teardown is followed by `CoreThread::run` dropping `self` — journal included — inside the
        // vt-core thread, and `CommandJournal::drop` JOINS the syncer after its final whole-segment
        // `msync`. So `CoreHandle::shutdown_and_join` still returns only once the checkpoint is on
        // disk.
        j.queue_sync();
        self.journaled_since_snap = 0;
    }
}
