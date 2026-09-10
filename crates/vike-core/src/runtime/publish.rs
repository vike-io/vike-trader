//! Coalesced snapshot publish + the recent-events drains + the opt-in mmap counter mirror — split
//! out of the runtime fold module (behavior byte-identical; the block moved verbatim). `use super::*`
//! re-exports the parent runtime module's full import set + items, so nothing about resolution changes.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Fold the bus delivery log into the bounded recent-events ring (journal feed).
    ///
    /// PER-MESSAGE PATH - `handle()` calls this after every `Ingest`, inside the
    /// `p99 core-hop < 10us` gate. It captures each event's few rendering inputs
    /// ([`crate::recent::EventNote`]) and formats NOTHING; the line is rendered at the coalesced
    /// publish that actually reads the ring (perf audit 2026-07-28, finding #2).
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
        // empty on every non-bracket run — a single `is_empty` fast path off the fold.
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
        // coalesced publish path; empty (a no-op clone) until a venue reports a coin delta.
        snap.recon_coin_deltas = self.recon_coin_deltas.clone();
        self.snapshot.store(Arc::new(snap));
        self.dirty = false;
        if let Some(repaint) = &self.config.repaint {
            repaint();
        }
        // Opt-in mmap counter mirror (audit co9): copy the key health counters into the external
        // file at THIS coalesced publish cadence — the same rare, off-the-fold point that just built
        // the snapshot above. The per-message fold (`p99 < 10µs`) is byte-identical; the only new
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
    /// Cold path only, called at coalesced publish cadence like `mount_views` — never the
    /// per-message fold.
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
                })
                .collect(),
            last_pass_ts: self.recon_last_pass_ts,
        }
    }

    /// Snapshot the key runtime health counters for the mmap mirror (audit co9), SUMMED across the
    /// primary engine + every extra engine so a cross-venue core reports totals. `conflated_market_
    /// drops` and `rejected_commands` are the runtime-wide atomics; the rest are per-engine folds
    /// (`stranded_terminal_drops`/`dropped_terminal_on_live`/`dropped_unknown_coid`/
    /// `dropped_nonfinite`) plus each
    /// Reads only — called at publish cadence, never the per-message fold. The `exec_db_*` slots
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
}
