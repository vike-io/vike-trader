//! The snapshot publish (coalesced while busy, AND on every idle transition — so per event on a
//! sporadic feed) + the recent-events drains + the opt-in mmap counter mirror — split
//! out of the runtime fold module (behavior byte-identical; the block moved verbatim). `use super::*`
//! re-exports the parent runtime module's full import set + items, so nothing about resolution changes.

use super::*;

/// The suffix `CoreThread::write_snap` puts on the route key of a STOPPED primary engine's
/// tombstone (decision 0169): `binance` is journaled as `binance#stopped` once a newer engine of
/// the account replaced it. The boot restore skips such a snapshot BY DESIGN (no engine of the
/// next boot carries the key), so that skip is an expected `info`, not a `warn`; `vike-tradehub`'s
/// boot re-admission reads the same constant. No running engine carries it: a label is `[A-Z0-9]+`.
pub const RETIRED_ROUTE_KEY_SUFFIX: &str = "#stopped";

impl<C: ExecutionClient> CoreThread<C> {
    /// Fold the bus delivery log into the bounded recent-events ring (journal feed).
    ///
    /// PER-MESSAGE PATH - `handle()` calls this after every `Ingest`, inside the
    /// `p99 core-hop < 10us` gate. It captures each event's few rendering inputs
    /// ([`crate::recent::EventNote`]) and RENDERS the line right here, on the fold
    /// ([`Self::note_event`]). Perf audit 2026-07-28 finding #2 (#887) had deferred the render to
    /// publish; #896 moved it back, because publish also runs per event on a sporadic feed.
    ///
    /// It is also where a venue's refusal of a cancel reaches the drain's record of it (P1,
    /// `CoreThread::tag_cancels_sent`): one `is_empty()` read per message, and a variant test per
    /// delivered event only while a mount's cancel is on record (the owner's ruling "A" of
    /// 2026-11-12: never in the fold).
    pub(crate) fn drain_delivered(&mut self) {
        let cancels_on_record = !self.tag_cancels_sent.is_empty();
        for event in self.bus.take_delivered() {
            if cancels_on_record && let Event::OrderCancelRejected(r) = &event {
                self.forget_refused_cancel(&r.order_client_id);
            }
            self.note_event(crate::recent::EventNote::capture(&event));
        }
    }

    /// The venue refused the cancel of `order_client_id`: the order still works, so its tag reads BUSY again
    /// (`drain_broker`'s `tag_busy`) and a place under it is refused rather than orphaning it.
    #[cold]
    #[inline(never)]
    fn forget_refused_cancel(&mut self, order_client_id: &str) {
        self.tag_cancels_sent.remove(order_client_id);
    }

    /// Salvage any event the bus was folding when a handler panicked (audit C4): surface the lost
    /// terminal in the recent-events ring (+ the fault snapshot) so the loss is visible. Called from
    /// every fold-guarding `catch_unwind` Err arm — a no-op on the happy path (poisoned is empty).
    pub(crate) fn drain_poisoned(&mut self) {
        for event in self.bus.take_poisoned() {
            self.note_lost(crate::recent::EventNote::capture(&event));
        }
    }

    /// Guarded publish: neither the snapshot build nor the GUI's repaint hook may kill
    /// the core thread. On a build/hook panic a minimal fault snapshot is stored so the
    /// GUI still sees terminal state (never a stale Active/no-fault cell).
    pub(crate) fn publish_guarded(&mut self) {
        if catch_unwind(AssertUnwindSafe(|| self.publish())).is_err() {
            self.engine.trading_state = TradingState::Halted;
            self.node_floor = TradingState::Halted; // see `enter_safe_state`
            let mut snap = CoreSnapshot::empty(&self.engine.venue, &self.engine.symbol);
            self.sequence += 1;
            snap.sequence = self.sequence;
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
        self.sequence += 1;
        let mount_rows = self.published_mount_rows();
        let recon = self.reconcile_block();
        // Live-runtime OTO/OCO: surface the pending bracket exits held off the venue so the GUI can
        // show a resting bracket's protective legs (they are NOT in the registry). Insertion order;
        // empty on every non-bracket run — a single `is_empty` fast path on a publish that runs per
        // event on a sporadic feed.
        let mut held_exits: Vec<vike_execution::OrderHeldView> = self
            .held_orders
            .values()
            .map(|r| vike_execution::OrderHeldView {
                order_client_id: r.order_client_id.clone(),
                venue: r.venue.clone(),
                symbol: r.symbol.clone(),
                side: r.side,
                qty: r.qty,
                order_type: r.order_type.clone(),
                price: r.price,
                trigger_price: r.trigger_price,
                parent_order_id: r.parent_order_id.clone(),
                account: None,
                release: None,
                waiting: None,
                // The exit's terms off its request: `Copy`s of an `Option` each, no allocation.
                attributes: r.attributes,
                trail_percent: r.trail_percent,
                adjust: r.adjust,
                display_quantity: r.display_quantity,
            })
            .collect();
        // …and the orders held for their release (`held_release.rs`), after them, each with its
        // release and account: one `is_empty` while nothing is held.
        if !self.held_releases.is_empty() {
            held_exits.extend(self.held_release_views());
        }
        // The ONE maintenance-rate source for the GUI liq badge: the operator's watchdog config
        // when armed, else the same default the watchdog would run with. (A per-symbol
        // venue-reported rate would take precedence here once an adapter carries one — none
        // does today; see `vike_model::pool_breached`'s precedence note.)
        let mm_rate = self
            .config
            .margin_call
            .as_ref()
            .map(|c| c.maintenance_margin_requirement)
            .unwrap_or_else(|| {
                vike_model::MarginCallConfig::default().maintenance_margin_requirement
            });
        let mut snap = crate::snapshot::build(
            self.sequence,
            &self.engine,
            &self.extra_engines,
            self.config.seed_cash,
            self.config.price_config,
            mm_rate,
            // The ring already holds RENDERED lines; publish is a refcount bump per entry (see
            // `CoreThread::note_event`). No mutation, no formatting at publish.
            &self.recent,
            &self.bars,
            // The mount list is the cached one when no row moved (a refcount bump), else freshly
            // built (`CoreThread::published_mount_rows`); the held exits are moved in, not copied
            // (see `snapshot::build`).
            mount_rows,
            &self.fault,
            self.market.drops.load(Ordering::Relaxed),
            self.rejected.load(Ordering::Relaxed),
            recon,
            held_exits,
        );
        // Wave 5d: overlay the reconcile-path per-position coin deltas (side map, not derived from
        // the engine) so the greeks tool can fold a Deribit perp/future leg. Cheap clone on the
        // publish path (per event on a sporadic feed); empty (a no-op clone) until a venue reports
        // a coin delta.
        snap.reconcile_coin_deltas = self.reconcile_coin_deltas.clone();
        // The operator's armed CORE-HELD reduce-only stops (`core_held.rs`): an empty `Vec` (no
        // allocation) while the node holds no conditional at all, one `HashMap::is_empty` test.
        snap.core_held_stops = self.core_held_stop_views();
        // EVERY emulated order, in one list (`emulated.rs`, decision 0275): two `is_empty` tests while the
        // entity holds none.
        if !self.emulated.order_emulation_arms.is_empty() || !self.emulated.working.is_empty() {
            snap.emulated_orders = self.emulated_views();
        }
        // A node with engines in several currency groups (decision 0183, item 13): the first LIVE engine's
        // group is the primary one, and the other groups' drawdown curves ride the snapshot. A node of one
        // currency publishes its one row only once the latch has swept, for the alert to read the latch's
        // peak; with no `max_drawdown` this is one `bool` and one `Option` test, nothing allocated.
        if self.ledger_mixed || self.pnl_curve_peak.is_some() {
            self.state_groups(&mut snap.portfolio);
        }
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

    /// Build the GUI-facing `ReconcileBlock` from the held-alert store (Task 17): one `ReconcileAlertView`
    /// per currently-held alert (insertion order — oldest first) plus the last-pass timestamp.
    /// Called at every publish, like `mount_views` — and publish runs per event on a sporadic
    /// feed, so this is inside the gated core hop: O(held alerts), empty on a box with none held.
    fn reconcile_block(&self) -> vike_execution::ReconcileBlock {
        vike_execution::ReconcileBlock {
            alerts: self
                .reconcile_alerts
                .iter()
                .map(|(&id, a)| {
                    // A row's fills name one instrument; their net is what the confirm would book.
                    let fills = a.proposed_events.iter().filter_map(|e| match e {
                        vike_model::events::Event::Fill(f) => Some(f),
                        _ => None,
                    });
                    vike_execution::ReconcileAlertView {
                        id,
                        kind: format!("{:?}", a.kind),
                        detail: a.detail.clone(),
                        proposed_event_count: a.proposed_events.len(),
                        // The raising pass's own routing key — the venue itself for a venue with one
                        // account, so this reads as the exchange id on every box with no `[accounts]`
                        // table and names the book on one that has them.
                        account: a.route_key.clone(),
                        fill_symbol: fills.clone().next().map(|f| f.symbol.into()),
                        fill_qty: fills.map(|f| f64::from(f.side) * f.last_qty).sum(),
                    }
                })
                .collect(),
            last_pass_timestamp: self.reconcile_last_pass_timestamp,
        }
    }

    /// Snapshot the key runtime health counters for the mmap mirror (audit co9), SUMMED across the
    /// primary engine + every extra engine so a cross-venue core reports totals. `conflated_market_
    /// drops` and `rejected_commands` are the runtime-wide atomics; the rest are per-engine folds
    /// (`stranded_terminal_drops`/`dropped_terminal_on_live`/`dropped_unknown_order_client_id`/
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
            c.dropped_unknown_order_client_id += e.dropped_unknown_order_client_id;
            c.dropped_nonfinite += e.dropped_nonfinite;
        }
        // Core-level order-restore counts (the v3 slots): not an engine fold, so not summed above.
        c.restored_adopted = self.restore_counters.restored_adopted;
        c.restored_gone = self.restore_counters.restored_gone;
        c.restored_filled_while_down = self.restore_counters.restored_filled_while_down;
        c.tag_overwrite_orphaned = self.restore_counters.tag_overwrite_orphaned;
        c
    }

    /// Write a full-state `Snapshot` record (spec §A "snapshot-as-command"): every engine (primary +
    /// extras) via [`Engine::snapshot_state`], the order client id `(session, seq)` for restart
    /// continuity, and `state_hash` as the determinism fence the T5 replay checks against. Asks the
    /// journal's syncer thread to carry the checkpoint to disk (a Snapshot is the durability checkpoint
    /// the next replay resumes from) and resets the cadence counter. No-op when journaling is off,
    /// so callers may invoke it unconditionally.
    ///
    /// # ⚠ This runs on the FOLD THREAD — the one the `p99 < 10µs` gate protects
    ///
    /// Both cadence call sites (`dispatch`'s tail and `pump_client`'s [`Self::maybe_cadence_snap`])
    /// fire from inside the single-writer fold, once per `snapshot_every` journaled records — 1024
    /// at the production default. So **nothing in here may block**, and until #932 something did:
    /// this function ended with `Journal::flush()`, the blocking whole-mapping `msync` whose
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
        let mut engines: Vec<vike_execution::EngineSnapshot> =
            Vec::with_capacity(1 + self.extra_engines.len());
        // The `Snapshot` is bounded by what is LIVE (`Engine::journal_snapshot`): an ended order the
        // engine leaves out still owes the late-retry memory its id, so it rides `recent_order_client_ids` instead.
        let mut left_out: Vec<(String, i64)> = Vec::new();
        let mut primary = self.engine.journal_snapshot();
        Self::note_left_out(&self.engine, now_ms, &mut left_out);
        // A STOPPED engine (decision 0169) whose account started again must not share its route key with the
        // engine that replaced it in the journal: the boot restore keys engines by route key and would find two
        // carriers and apply neither, so the account would come back without its positions, PnL and fill dedup
        // (and a lost fill dedup lets a reconcile redelivery be booked twice). An extra is simply left out. The
        // PRIMARY cannot be (the replay takes the first snapshot as the primary), so its tombstone is journaled
        // under a key no engine carries and the restore notes it as skipped. (Default-account snapshots carry
        // `route_key: None`, so "apply the newest carrier" cannot tell this from a pre-v17 journal's two
        // accounts of one venue.) Either way the stopped engine's own realized PnL and fees do not carry over
        // to the next boot: the account starts again, and the new engine's books are the account's.
        if self.is_retired(0)
            && self.extra_engines.iter().any(|(_, later)| later.route_key == self.engine.route_key)
        {
            primary.route_key =
                Some(format!("{}{RETIRED_ROUTE_KEY_SUFFIX}", self.engine.route_key));
        }
        // Each engine's fill ledger takes the floor its `Snapshot` states, so this process refuses what a restart
        // from this `Snapshot` would (`Account::adopt_fill_floor`): a fill with no stamp the window cannot vouch for.
        self.engine.account.adopt_fill_floor(primary.fill_floor_ms);
        engines.push(primary);
        let mut floors: Vec<(usize, Option<i64>)> = Vec::new();
        for (i, (_, e)) in self.extra_engines.iter().enumerate() {
            if self.is_retired(i + 1)
                && self.extra_engines[i + 1..]
                    .iter()
                    .any(|(_, later)| later.route_key == e.route_key)
            {
                continue;
            }
            let snap = e.journal_snapshot();
            floors.push((i, snap.fill_floor_ms));
            engines.push(snap);
            Self::note_left_out(e, now_ms, &mut left_out);
        }
        for (i, floor) in floors {
            self.extra_engines[i].1.account.adopt_fill_floor(floor);
        }
        let (order_client_id_session, order_client_id_sequence) = self.order_client_id_gen.state();
        let hash = vike_execution::state_hash(&engines);
        // `arm_sequence` rides the Snapshot next to `order_client_id_sequence`, mirroring it exactly: both are the id
        // counters a restart must resume, and the Snapshot is the ONE restore base — a value derived
        // from the surviving record set instead (the pre-v7 shape) is prune-UNSAFE, because
        // `prune_before_latest_snap` deletes whole early segments out from under the count.
        let arm_sequence = self.arm_sequence;
        // Emulator PR-3: the resting conditional BOOKS ride the Snapshot too — the only durable home
        // for a trailing arm's CURRENT ratcheted extreme (it moves on non-journaled market data,
        // so neither the write-ahead ARM `Command` nor the `OrderEmulationArmed` seed record can carry
        // it). Captured in fire order (books map insertion order, then each book's). O(arms) at
        // snapshot cadence, zero cost on the per-message fold. Deliberately NOT folded into
        // `hash` (which stays `state_hash(&engines)`): book contents move fenced state only
        // through a FIRE, and every fire has its own write-ahead record — see `crate::replay`.
        let mut conditionals: Vec<vike_journal::JournalSnapshotOrderEmulationArm> = Vec::new();
        for ((venue, symbol), book) in &self.emulated.order_emulation_arms {
            for (arm_id, o) in book.iter() {
                conditionals.push(vike_journal::JournalSnapshotOrderEmulationArm {
                    arm_id: arm_id.to_string(),
                    // A plain stop or trailing stop keeps its bytes; any other arm goes under
                    // `kind_terms`, which an older build skips instead of arming a plain stop.
                    terms: vike_journal::OrderEmulationRecord::of_arm(
                        venue.clone(),
                        symbol.clone(),
                        &o.order,
                        o.trigger_by,
                        // An operator's core-held reduce-only stop: the flag lives on the arm's
                        // side record (`OrderEmulationArmRoute`), not in the gated book, so a restart gets it back.
                        self.emulated.cond_engine.get(arm_id).is_some_and(|a| a.reduce_only),
                    ),
                    // WHICH ACCOUNT the arm fires onto (journal v17), so a restart re-routes a
                    // restored stop to its own book. Only where it is ambiguous — two engines of
                    // one exchange — so a single-account core's Snapshot bytes do not move and pay no
                    // lookup; per arm, at snapshot cadence.
                    route_key: if self.multi_account {
                        self.emulated
                            .cond_engine
                            .get(arm_id)
                            .map(|a| self.engine(a.engine).route_key.clone())
                    } else {
                        None
                    },
                });
            }
        }
        // Live-runtime OTO/OCO: the resting contingency book rides the Snapshot too — the ONLY durable
        // home for a HELD exit (a leg the runtime is keeping off the venue until its parent fills;
        // an ACTIVE leg is already in `engines` via the registry). Captured in the book's own
        // insertion order (arm/cancel iteration order); each held leg carries its resolved request
        // so a restart can re-submit it. O(legs) at snapshot cadence, nothing on the per-message
        // fold. NOT folded into `hash` (same fence argument as the conditionals above): the book
        // moves fenced state only through a fill that releases/cancels, and that fill is itself a
        // journaled `Command` event replay re-folds through the re-seeded book — see `crate::replay`.
        let contingencies: Vec<vike_journal::JournalSnapshotContingency> = self
            .contingency
            .snapshot()
            .into_iter()
            .map(|l| vike_journal::JournalSnapshotContingency {
                held_request: self.held_orders.get(&l.order_client_id).cloned(),
                order_client_id: l.order_client_id,
                parent: l.parent,
                linked: l.linked,
                active: l.active,
                contingency_type: l.kind,
            })
            .collect();
        // Multi-mount durability (gap D): the per-mount ATTRIBUTION ledgers ride the Snapshot too — the
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
        let files = &self.config.mount_strategy_files;
        let mount_attribute: Vec<vike_journal::JournalSnapshotMountAttribution> = self
            .mount_ids
            .iter()
            .zip(self.mount_attribution.iter())
            .zip(self.mounts.iter())
            .zip(self.mount_latched.iter())
            .zip(self.mount_loss_base.iter())
            .filter(|(((_, slot), _), _)| slot.is_some())
            .flat_map(|((((mount_id, a), slot), &latched), &loss_base)| {
                // The mount's strategy code rides its own row (additive, `None` writes no key), so the journal
                // alone names the strategy its ids carry.
                let order_client_id_code = slot.as_ref().and_then(|m| m.order_client_id_code);
                // A leg's row carries its key (additive, a mount's own row writes none). Written BEFORE the own
                // row: a build from before the key reads each row as the whole ledger, last one wins.
                let legs =
                    a.legs.iter().map(move |leg| vike_journal::JournalSnapshotMountAttribution {
                        mount_id: mount_id.clone(),
                        size: leg.attr.size,
                        average_price: leg.attr.average_price,
                        realized_pnl: leg.attr.realized_pnl,
                        fees_paid: leg.attr.fees_paid,
                        venue: Some(leg.venue.clone()),
                        symbol: Some(leg.symbol.clone()),
                        ..Default::default()
                    });
                legs.chain(std::iter::once(vike_journal::JournalSnapshotMountAttribution {
                    mount_id: mount_id.clone(),
                    size: a.size,
                    average_price: a.average_price,
                    realized_pnl: a.realized_pnl,
                    fees_paid: a.fees_paid,
                    // The per-mount budget latch rides the Snapshot (additive, `false` writes no key), so a restart
                    // puts a spent latch back (`assemble_core`); one bool per live mount at snapshot cadence.
                    latched,
                    // …and so does the loss budget's base after an operator's un-latch (additive, zero writes no
                    // key, decision 0239): without it the restart judges the whole history again.
                    loss_base,
                    venue: None,
                    symbol: None,
                    order_client_id_code,
                    // …and the strategy file it runs (additive, `None` writes no key), so a `Snapshot` still says
                    // which file traded after the prune took its `StrategyFileMounted` record.
                    strategy_file: files.get(mount_id).cloned(),
                }))
            })
            .collect();
        // Journal v18: each gateway ring's resume point rides the Snapshot too — an account quiet since
        // the last Snapshot has no `GatewayEvent` left in the tail the prune keeps, so this is the only
        // place its cursor survives. O(gateway accounts) at snapshot cadence, and EMPTY (so no key
        // on the wire and no allocation worth the name) on a box with no gateway.
        let gateway_cursors: Vec<(String, vike_model::EventRingCursor)> =
            self.gateway_cursors.iter().map(|(key, c)| (key.to_string(), *c)).collect();
        // The orders held for their release ride the Snapshot too: they are in no registry and at no venue,
        // so this is their only durable home (`held_release.rs`). Empty, so no key, while none is held.
        let held_releases: Vec<vike_journal::OrderHeldRelease> =
            self.held_releases.values().cloned().collect();
        // The working emulated orders ride the Snapshot too (`emulated.rs`): their only durable home. Empty,
        // so no key, while none is worked.
        let emulated = self.emulated_snap();
        // Every mount's closed-trade ledger rides the Snapshot too (`closed_trades.rs`'s "A restart"): how far the
        // archive holds its rows on disk, the rows after that and its lots; a restart reads the archive, adds
        // these and books the tail's fills on them. Empty, so no key, on a mount-less core. O(lots + unsynced
        // rows) at snapshot cadence, never on the per-message fold.
        let closed_trades = self.closed_trades_for_snap();
        let recent_order_client_ids: std::borrow::Cow<'_, [(String, i64)]> = if left_out.is_empty()
        {
            std::borrow::Cow::Borrowed(self.recent_order_client_ids.carried())
        } else {
            std::borrow::Cow::Owned(self.recent_order_client_ids.carried_with(left_out))
        };
        let j = self.journal.as_mut().expect("early return above when journal is None");
        j.append_snap(
            now_ms,
            &engines,
            &order_client_id_session,
            order_client_id_sequence,
            arm_sequence,
            vike_journal::JournalSnapshotParts {
                order_emulation_arms: &conditionals,
                contingencies: &contingencies,
                mount_attribution: &mount_attribute,
                gateway_cursors: &gateway_cursors,
                held_releases: &held_releases,
                // The late-retry memory this boot was handed, and the ids of the ended orders the engines
                // left out (empty, so no key, on a core given none that left none out).
                recent_order_client_ids: &recent_order_client_ids,
                emulated: &emulated,
                closed_trades: &closed_trades,
            },
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
        // CLEAN stop still forces the tail: the exit `Snapshot` this same function writes during
        // teardown is followed by `CoreThread::run` dropping `self` — journal included — inside the
        // th-core thread, and `Journal::drop` JOINS the syncer after its final whole-segment
        // `msync`. So `CoreHandle::shutdown_and_join` still returns only once the checkpoint is on
        // disk.
        j.queue_sync();
        self.journaled_since_snap = 0;
    }

    /// Push `(order client id, created_ms)` of each ENDED order `e`'s `Snapshot` leaves out (`journal_left_out`) and was created
    /// within the late-retry window of `now_ms` (`RECENT_ORDER_CLIENT_ID_WINDOW_MS`, the span the boot keeps of the memory):
    /// the id a retry must still be refused under after a restart, which the engine registry carried before the
    /// `Snapshot` was bounded. An order with no `created_ms` (rebuilt from a venue report) counts at `now_ms`, as the
    /// boot counts it.
    fn note_left_out(e: &Engine<C>, now_ms: i64, left_out: &mut Vec<(String, i64)>) {
        let floor = now_ms.saturating_sub(
            crate::runtime::recent_order_client_ids::RECENT_ORDER_CLIENT_ID_WINDOW_MS,
        );
        left_out.extend(
            e.journal_left_out()
                .map(|(order_client_id, o)| {
                    (order_client_id.to_string(), o.created_ms.unwrap_or(now_ms))
                })
                .filter(|&(_, at)| at >= floor),
        );
    }
}
