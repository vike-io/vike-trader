//! Order-event and applied-fill dispatch onto the owning mounts, and terminal-coid pruning.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Deliver buffered NON-FILL order-lifecycle transitions (accept / reject / deny / cancel /
    /// expire) to the mounted strategy that OWNS each order — the occasional-order-outcome twin of
    /// [`Self::dispatch_applied_fills`], and the delivery half of `Strategy::on_order_event`
    /// (position-executor stage 3). Each `vike_exec::OrderEventOut` was captured at the engine
    /// fold WITH its (venue, symbol) — from the just-advanced registry order, or the request for a
    /// `RiskGate` deny (a denied order never reaches the registry) — so this routes it to the mount
    /// that MINTED the order ([`Self::mount_for_coid`]), exactly as a fill is routed, builds the same
    /// `LiveBroker` snapshot, calls the hook, and drains any orders it buffers through the one live
    /// path. An order NO mount minted (an operator ticket, a reconcile-adopted venue order, a
    /// liquidation) reaches NO mount: it is not an order any strategy caused, so no `on_order_event`
    /// hears it (decision 0116); the account and the residual row take it.
    ///
    /// Fills are NOT here (they stay on [`Self::dispatch_applied_fills`], which is why
    /// `OrderLifecycle::from_event` returns `None` for them — no double-delivery). An order event
    /// folds at ORDER cadence (a submit reply / a cancel), never per quote or bar, so this touches
    /// the `p99 < 10µs` per-market-message fold NOT AT ALL. `now` is the dispatch wall-clock stamp
    /// (as in `on_feed_status`/`on_params_updated`), `price` the standing mark (else `0.0`). NOT
    /// warmup-gated (an order outcome is an account fact regardless of bar count, mirroring
    /// `on_fill`). An event with no owning mount is dropped (an order no live strategy minted, or one
    /// whose mount is unmounted); see the note below. An empty buffer is a single length check +
    /// return — zero cost when nothing is pending or no strategy is mounted.
    fn dispatch_order_events(&mut self) {
        if self.engine.order_events.is_empty()
            && self.extra_engines.iter().all(|(_, e)| e.order_events.is_empty())
        {
            return;
        }
        if self.mounts.is_empty() {
            self.engine.order_events.clear();
            for (_, e) in self.extra_engines.iter_mut() {
                e.order_events.clear();
            }
            return;
        }
        let mut evs = std::mem::take(&mut self.engine.order_events);
        for (_, e) in self.extra_engines.iter_mut() {
            evs.append(&mut e.order_events);
        }
        let now = self.engine.now_ms; // stamped at the top of dispatch(), like feed_status/params
        for mut oe in evs {
            // A TERMINAL outcome makes this order's attribution entry retirable. Queued HERE, at the
            // top, BEFORE the routing `continue` below, so an event whose mount cannot be resolved
            // still bounds the map. Nothing is erased yet — see `note_terminal_coid`. The
            // classification is `OrderEventKind::is_terminal`, spelled once in vike-model, so a new
            // variant cannot default to "not terminal" here while a strategy treats it as a death.
            if oe.event.kind.is_terminal() {
                self.note_terminal_coid(&oe.event.client_order_id, now);
            }
            // Route to the mount that MINTED this order (bug B) — exactly the key
            // `dispatch_applied_fills` routes a fill by. An order NO mount minted (operator ticket,
            // liquidation, adopted venue order) is nobody's: it reaches no mount (decision 0116), and
            // neither does a straggler of an unmounted mount.
            let Some(idx) = self.mount_for_coid(&oe.event.client_order_id) else {
                // The operator's trace of a TERMINAL outcome that used to reach a strategy: only when
                // a live mount sits on the pair, so the 64-line ring is not churned by events no
                // mount ever heard. Never reached on a mount-less core (the early return above);
                // on a mounted one it runs at ORDER cadence, never per market message.
                if oe.event.kind.is_terminal() && self.mount_sits_on(&oe.venue, &oe.symbol) {
                    self.note(format!(
                        "order event {:?} for {:?} ({} {}) reached no strategy: no mount minted it",
                        oe.event.kind, oe.event.client_order_id, oe.venue, oe.symbol
                    ));
                }
                continue;
            };
            let mount_key = {
                let m = self.mounts[idx].as_ref().expect("just found");
                (m.venue.clone(), m.symbol.clone(), m.interval.clone())
            };
            // Stamp the strategy's OWN name for this order — the tagged-order contract's missing
            // learning half (`OrderLifecycle::tag`). A TERMINAL transition retires the entry in the
            // same step, because after it the tag names nothing; a non-terminal `Accepted` only
            // reads, so the order stays addressable by tag exactly as before.
            // A tag ALREADY on the event rides through: the one producer is a synthetic refusal
            // (`deny_restored_tag`), whose id the registry cannot name but whose tag the mount keys
            // on. Every engine-captured event arrives tagless, so the lookup below decides those.
            let resolved = if oe.event.kind.is_terminal() {
                self.retire_tag_for_coid(idx, &oe.event.client_order_id)
            } else {
                self.tag_for_coid(idx, &oe.event.client_order_id)
            };
            oe.event.tag = oe.event.tag.take().or(resolved);
            // The MOUNT's engine — this event is being delivered to `idx`, so its context must be
            // that mount's book. `EngineRoute::Mount` falls back to the payload venue for a
            // cross-venue order, which is what a declared foreign leg's event is.
            let eidx = self.route_of(EngineRoute::Mount(idx), &oe.venue).unwrap_or(0);
            // no price rides an order event — use the standing mark if one exists, else 0.0
            let mark = self.eng(eidx).account.mark_of(&oe.venue, &oe.symbol).unwrap_or(0.0);
            // the order-event step sees the SAME closed-bar history on_bar would (the mount's series)
            let bars_arc =
                self.bars.get(&mount_key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
            let index = bars_arc.len().saturating_sub(1);
            // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol mount,
            // which is what keeps this byte-identical). The DISPATCHING symbol here is the ORDER's,
            // which for a declared-multi mount is routinely a leg's rather than the mount's own —
            // so without this an `on_order_event` reading `position(mount_symbol)` fell through to
            // the scalar below and answered about the order's instrument instead.
            let views = self.declared_views(idx, None, &oe.venue);
            // take/replace so the strategy call can't alias the rest of the core state
            let mut mount = self.mounts[idx].take().expect("just found");
            let mut ctx = LiveBroker {
                positions: views.positions,
                prices: views.prices,
                bar_views: views.bar_views,
                position: self.eng(eidx).position_size_of(&oe.symbol, "BOTH"),
                price: mark,
                equity: self.eng(eidx).sizing_equity(self.seed_of(eidx), &self.config.price_cfg),
                bars: bars_arc,
                index,
                now,
                multiplier: self.eng(eidx).account.multiplier_of(&oe.symbol),
                lot_size: self.eng(eidx).gate.limits.lot_size.unwrap_or(0.0),
                submissions: Vec::new(),
                modifications: Vec::new(),
                cancels: Vec::new(),
                brackets: Vec::new(),
                conditionals: Vec::new(),
                mass_cancel: false,
            };
            mount.strategy.on_order_event(&mut ctx, &oe.event);
            self.mounts[idx] = Some(mount);
            self.drain_broker(ctx, &oe.venue, &oe.symbol, now, idx);
        }
        self.dirty = true;
    }

    /// Deliver account-applied fills to the mounted strategy — the live twin of the backtest
    /// engine's synchronous `on_fill` firing point. Each fill was captured post-dedup at the
    /// ONE `Account::apply_fill` site ([`ExecutionEngine::applied_fills`]) WITH its per-fill
    /// position/equity snapshot, so a WS reconnect replay can never double-fire and a
    /// multi-fill batch shows each handler the state after ITS fill, not the batch's.
    /// `ctx.price` is the standing mark (in the paper bar path that is the PRIOR close —
    /// the backtest fill-phase price), falling back to the fill's own price pre-first-mark.
    /// NOT warmup-gated: the backtest `fire_on_fill` is unconditional, and a fill is an account
    /// fact regardless of bar count.
    ///
    /// **Only the mount that MINTED a fill's order hears it** ([`Self::mount_for_coid`]). A fill for
    /// a coid no mount minted (an operator ticket, a reconcile-synthesised `EXT-*`, a Polymarket
    /// settlement, a liquidation, a straggler of an unmounted mount) reaches NO mount's `on_fill`
    /// and books into no live mount's ledger, yet the ACCOUNT folded it already, so a strategy sees
    /// it through `Broker::position` on its next hook (decision 0116). The backtest has no operator
    /// tickets to deliver, so this is parity (the accepted gap is settlement and liquidation, which
    /// the simulator still fires; see the decision). One asymmetry: a straggler of an UNMOUNTED
    /// mount still moves that tombstone's ledger (`coid_mount` outlives the slot) while delivery
    /// skips it.
    /// Fills folded WHILE a handler runs are delivered next round — bounded rounds here;
    /// leftovers drain at the next message, the idle branch, and teardown.
    pub(crate) fn dispatch_applied_fills(&mut self) {
        // Deliver NON-FILL order-lifecycle transitions FIRST (accept/reject/cancel/expire/deny), so
        // an accept-then-fill sequence reaches the strategy in order. Cheap when nothing is buffered
        // (a length check). Folded into this one method so ALL its call sites — per-message, idle,
        // and the two teardown drains — deliver order events too, with no new call sites.
        self.dispatch_order_events();
        for _ in 0..8 {
            // NB `break`, not `return`: the prune sweep at the tail must run on the COMMON path
            // (nothing buffered) too, or `coid_mount` would only ever be bounded on a dispatch that
            // happened to carry fills.
            if self.engine.applied_fills.is_empty()
                && self.extra_engines.iter().all(|(_, e)| e.applied_fills.is_empty())
            {
                break;
            }
            if self.mounts.is_empty() {
                self.engine.applied_fills.clear();
                for (_, e) in self.extra_engines.iter_mut() {
                    e.applied_fills.clear();
                }
                break;
            }
            let mut fills = std::mem::take(&mut self.engine.applied_fills);
            for (_, e) in self.extra_engines.iter_mut() {
                fills.append(&mut e.applied_fills);
            }
            for af in fills {
                let f = &af.fill;
                // steal/core-per-mount-budget ATTRIBUTION: fold this fill into its ORIGINATING
                // mount's ledger (coid -> mount), the weighted-average-cost fold `Account::fold`
                // itself runs (both call `vike_model::compute_fill`), so a mount that trades one
                // symbol tracks the same position the account does. Keyed by coid — the SAME key the
                // strategy routing below now uses, so ledger and callback can never disagree about
                // which mount owns a fill; a fill whose coid no mount minted (a manual ticket /
                // margin-call liquidation) is simply not attributed (see the residual view).
                // Off the p99 hot fold (fills are order cadence); runs even with no budget
                // (attribution is always-on + read-only).
                let attributed = self.coid_mount.get(f.client_order_id.as_str()).copied();
                if let Some(midx) = attributed
                    && midx < self.mount_attr.len()
                {
                    let aeidx = self.route_of(EngineRoute::Mount(midx), &f.venue).unwrap_or(0);
                    let mult = self.eng(aeidx).account.multiplier_of(&f.symbol);
                    let attr = &mut self.mount_attr[midx];
                    let out = vike_model::compute_fill(
                        attr.size,
                        attr.avg_px,
                        f.side,
                        f.last_qty,
                        f.last_px,
                        mult,
                    );
                    attr.size = out.new_size;
                    attr.avg_px = out.new_avg_px;
                    if out.closing_qty > 0.0 {
                        attr.realized_pnl += out.realized_pnl;
                    }
                    attr.fees_paid += f.commission;
                    // …and the ledger as it now stands, for the next boot (decision 0113). A LIVE
                    // mount's only: a tombstone's straggler moves the dead slot's ledger in memory,
                    // but the file forgot that mount at its unmount and must not grow it back.
                    if self.order_owners.is_some() && self.mounts[midx].is_some() {
                        let a = self.mount_attr[midx];
                        let rec = crate::order_owners::OwnerRecord::Ledger {
                            mount_id: self.mount_ids[midx].clone(),
                            size: a.size,
                            avg_px: a.avg_px,
                            realized_pnl: a.realized_pnl,
                            fees_paid: a.fees_paid,
                            ts: self.config.clock.now_ms(),
                        };
                        self.record_owner(rec);
                    }
                }
                // A FULLY-FILLED order is terminal too — but `OrderLifecycle::from_event` returns
                // `None` for fills, so it never appears on the order-event lane above. This is the
                // ONLY place a filled order's attribution entry can be made retirable, and for a
                // maker it is the MAJORITY of them. The registry's post-fold status is the authority
                // on whether this fill completed the order. Queued, never erased here: the linger
                // window is exactly what protects the NEXT fill on this coid if one still comes.
                // ⚠ THE ORDER's OWN ENGINE, from the submit-time `coid_venue` map, falling back to
                // the venue lookup for a coid this process never submitted. The registry read below
                // asks "did THIS order complete", and asking the wrong account's registry answers
                // about an order it has never heard of — which reads as "not completed" and leaves
                // the attribution entry unretirable.
                let reidx = self
                    .coid_venue
                    .get(f.client_order_id.as_str())
                    .copied()
                    .or_else(|| self.engine_idx_for_route_key(RouteKey::sole_account_of(&f.venue)))
                    .unwrap_or(0);
                let completed = self
                    .eng(reidx)
                    .registry
                    .get(f.client_order_id.as_str())
                    .is_some_and(|mo| mo.status.is_terminal());
                if completed {
                    let (coid, ts) = (f.client_order_id.to_string(), self.engine.now_ms);
                    self.note_terminal_coid(&coid, ts);
                }
                // Route each fill to the mount that MINTED its coid (bug B) — the same exact key the
                // attribution fold just used, so for a LIVE mount the ledger and the callback cannot
                // disagree about which mount a fill belongs to (an UNMOUNTED mount's tombstone ledger
                // still takes its straggler; delivery skips it). A fill no mount minted reaches NO
                // mount (decision 0116): the account already took it, a strategy reads the move through
                // `broker.position`, and it sits in the residual row, not in any ledger.
                let Some(idx) = self.mount_for_coid(f.client_order_id.as_str()) else {
                    // Only when a live mount sits on the pair (where delivery used to happen); at
                    // fill cadence, never per market message.
                    if self.mount_sits_on(&f.venue, &f.symbol) {
                        self.note(format!(
                            "fill {} {} {} @ {} for {:?} reached no strategy: no mount minted its order",
                            f.side, f.last_qty, f.symbol, f.last_px, f.client_order_id
                        ));
                    }
                    continue;
                };
                let mount_key = {
                    let m = self.mounts[idx].as_ref().expect("just found");
                    (m.venue.clone(), m.symbol.clone(), m.interval.clone())
                };
                let fill = Fill {
                    side: f.side,
                    size: f.last_qty,
                    price: f.last_px,
                    fee: f.commission,
                    ts: f.ts,
                    is_maker: f.liquidity_side.is_maker(),
                    symbol: f.symbol.to_string(),
                };
                let feidx = self.route_of(EngineRoute::Mount(idx), &f.venue).unwrap_or(0);
                let mark = self.eng(feidx).account.mark_of(&f.venue, &f.symbol).unwrap_or(0.0);
                let bars_arc =
                    self.bars.get(&mount_key).map(|s| Arc::clone(&s.closed)).unwrap_or_default();
                // pre-seed the cache may be empty: index 0 with empty bars — the same
                // contract as the tick lane (strategies must not index blindly pre-bars)
                let index = bars_arc.len().saturating_sub(1);
                // BEFORE the take: the declared per-symbol read tables (empty for a single-symbol
                // mount, which is what keeps this byte-identical). ⚠ The FILL's symbol is the
                // dispatching one, so it gets NO row and `position(f.symbol)` keeps answering from
                // the `af.position_after` scalar below — the state after THIS fill rather than after
                // the whole batch, which is the guarantee `applied_fills`' per-fill snapshot exists
                // for. Every OTHER declared instrument now answers about itself instead of about the
                // filled one — a misread `vike_mm::xemm`'s `HedgeLedger` used to name as one of its
                // three reasons for folding its own fill stream rather than reading the `Broker`
                // seam. That doc is amended: two of the three were THIS defect and its cross-venue
                // sibling; what survives is intrinsic (a symbol-less `HftBroker::position`, and
                // in-flight hedge state no broker read of any shape can see).
                let views = self.declared_views(idx, Some(&f.symbol), &f.venue);
                // The TAG this completed order was resting under, retired in the same step (see
                // `retire_tag_for_coid`). Resolved BEFORE the mount is taken below, because the
                // lookup builds its key from the mount's own (venue, symbol). `None` for a partial
                // fill, an untagged order, or a coid whose tag has already been re-pointed at a
                // newer quote.
                let completed_tag = if completed {
                    self.retire_tag_for_coid(idx, &f.client_order_id)
                } else {
                    None
                };
                let mut mount = self.mounts[idx].take().expect("just found");
                let mut ctx = LiveBroker {
                    positions: views.positions,
                    prices: views.prices,
                    bar_views: views.bar_views,
                    position: af.position_after,
                    price: if mark > 0.0 { mark } else { fill.price },
                    equity: af.equity_after,
                    bars: bars_arc,
                    index,
                    now: fill.ts,
                    multiplier: self.eng(feidx).account.multiplier_of(&f.symbol),
                    lot_size: self.eng(feidx).gate.limits.lot_size.unwrap_or(0.0),
                    submissions: Vec::new(),
                    modifications: Vec::new(),
                    cancels: Vec::new(),
                    brackets: Vec::new(),
                    conditionals: Vec::new(),
                    mass_cancel: false,
                };
                mount.strategy.on_fill(&mut ctx, &fill);
                // ...and, when THIS fill completed the order, the order's DEATH — the third way a
                // resting order dies, and for a maker the most common. `on_fill` above cannot carry
                // it: `Fill` has no client-order-id, no tag and no remaining quantity, so a strategy
                // could only guess (and guesses wrong on any partial-fill sequence). Delivered
                // SECOND and on the SAME `LiveBroker` snapshot, so the money is folded before the
                // slot is freed and anything either hook buffers drains together through the one
                // live path below. `completed` is the post-fold registry status already read above —
                // the authority — so this adds no new judgement, only a delivery.
                if completed {
                    mount.strategy.on_order_event(
                        &mut ctx,
                        &vike_model::OrderLifecycle {
                            client_order_id: f.client_order_id.to_string(),
                            tag: completed_tag,
                            kind: vike_model::OrderEventKind::Filled,
                        },
                    );
                }
                self.mounts[idx] = Some(mount);
                let (venue, symbol, ts) = (f.venue, f.symbol, f.ts);
                self.drain_broker(ctx, &venue, &symbol, ts, idx);
            }
            self.dirty = true;
        }
        // AFTER every delivery round, never before: a fill buffered for THIS dispatch must have had
        // its chance to find its owner before any entry is retired.
        self.prune_terminal_coids();
    }

    /// Queue `coid`'s attribution entry for retirement, stamped `now` — called when its order is
    /// first observed TERMINAL (a non-fill lifecycle transition in [`Self::dispatch_order_events`],
    /// or a fill that COMPLETED it in [`Self::dispatch_applied_fills`]).
    ///
    /// **This ERASES NOTHING.** Removing `coid_mount`'s entry on the terminal itself is the obvious
    /// implementation and it is wrong. `Event::Fill` is a SEPARATE event from the FSM terminal that
    /// accompanies it, the two carry no ordering contract, and `Account::apply_fill` folds a fill
    /// regardless of FSM state (the terminal-drop guard is on the FSM lane only) — so a partial fill
    /// racing its own cancel ack, or a reconnect re-delivering executions, lands after the order is
    /// terminal. With the entry already gone that fill is UNATTRIBUTED, and an unattributed fill
    /// silently books into the residual row instead of the mount that traded it: the very defect the
    /// previous commit fixed for the budget-latch flatten, reintroduced through the back door. A
    /// bound on a map is not worth that. So the entry LINGERS for [`COID_PRUNE_LINGER_MS`] and
    /// [`Self::prune_terminal_coids`] retires it later.
    ///
    /// Only coids `coid_mount` actually holds are queued (an operator ticket or a reconcile-adopted
    /// venue order was never attributed and has nothing to retire). A repeat — an order seen
    /// terminal more than once — simply queues a second entry; the later `remove` is a no-op, which
    /// is cheaper than de-duplicating on the way in.
    fn note_terminal_coid(&mut self, coid: &str, now: i64) {
        if self.coid_mount.contains_key(coid) {
            self.coid_terminal.push_back((coid.to_string(), now));
        }
    }

    /// Retire every queued attribution entry that has been terminal for at least
    /// [`COID_PRUNE_LINGER_MS`] — the drain half of [`Self::note_terminal_coid`], and the bound on
    /// `coid_mount`'s growth.
    ///
    /// The queue is append-ordered on a monotone core clock, so the eligible entries are a PREFIX:
    /// pop from the front while the head has aged out, stop at the first that has not. An empty
    /// queue — and a queue whose head is still fresh, which is the steady state — costs one
    /// `front()` and one comparison, which is why this can sit on `dispatch_applied_fills`'s tail
    /// without touching the `p99 < 10µs` budget (that method is called per message, but every one of
    /// its bodies is already gated behind an emptiness check).
    fn prune_terminal_coids(&mut self) {
        let now = self.engine.now_ms;
        let aged = |q: &VecDeque<(String, i64)>| {
            q.front().is_some_and(|(_, ts)| now.saturating_sub(*ts) >= COID_PRUNE_LINGER_MS)
        };
        while aged(&self.coid_terminal) {
            if let Some((coid, _)) = self.coid_terminal.pop_front()
                && self.coid_mount.remove(&coid).is_some()
            {
                // A restored order that has been terminal past the linger is no longer a candidate
                // for adoption either: the collection is bounded the way `coid_mount` is.
                self.restored_orders.shift_remove(&coid);
                self.record_owner(crate::order_owners::OwnerRecord::Forget { coid });
            }
        }
    }

    /// **THE one write of a mount's ownership of an order it minted**: `coid` belongs to mount
    /// `mount_idx` in [`Self::coid_mount`], and — when the ownership file is on — that is recorded
    /// as an `own` under the mount's ID, so the order still has its owner after a restart (decision
    /// 0113). Called by every site that lowers an order a mount's strategy caused: the strategy
    /// submit choke point ([`Self::apply_strategy_intent`]), a fired stop
    /// ([`Self::submit_fired`]) and the budget latch's flatten ([`Self::latch_mount`]).
    ///
    /// NOT for an undeclared-symbol refusal id (`deny_undeclared_symbol` writes `coid_mount`
    /// itself): nothing was submitted, so there is no order to own across a restart. Order
    /// cadence, never the per-message fold; the record is one channel send.
    ///
    /// The same ONE record also says WHAT the order was, so a restart can put it back under its
    /// strategy tag: `tag` is the name the strategy submitted it under (only
    /// [`Self::drain_broker`]'s tagged submit knows it, and it knows it BEFORE this call, which is
    /// why it is a parameter and not a second record), and the venue, symbol and account are read
    /// off the order the engine just registered ([`Self::minted_facts`]). Pass `None` when the
    /// order carries no tag.
    pub(crate) fn own_coid(&mut self, coid: String, mount_idx: usize, tag: Option<&str>) {
        if self.order_owners.is_some() {
            let (venue, symbol, account) = self.minted_facts(&coid, mount_idx);
            let rec = crate::order_owners::OwnerRecord::Own {
                coid: coid.clone(),
                mount_id: self.mount_ids[mount_idx].clone(),
                ts: self.config.clock.now_ms(),
                venue,
                symbol,
                account,
                tag: tag.map(str::to_string),
            };
            self.record_owner(rec);
        }
        self.coid_mount.insert(coid, mount_idx);
    }

    /// `(venue, symbol, account)` of the order `coid` that `mount_idx` just minted, for its `own`
    /// record: read off the order's request in its engine's registry (or the held-exit store, for a
    /// bracket leg not yet released). All `None` when neither holds it — a RiskGate denial never
    /// reaches a registry — and then the entry restores ownership only.
    ///
    /// `account` is the MOUNT's label (`StrategyMount::account`, the text of a named label, `None`
    /// for the default account) and only when the order went to the mount's own engine; an order on
    /// a declared foreign-venue leg went to an engine the mount's account says nothing about, so it
    /// is `None`. Order cadence (one lookup, a few clones), never the per-message fold.
    fn minted_facts(
        &self,
        coid: &str,
        mount_idx: usize,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let eidx = self.coid_venue.get(coid).copied().unwrap_or(0);
        let Some(req) = self
            .eng(eidx)
            .registry
            .get(coid)
            .map(|mo| &mo.request)
            .or_else(|| self.held_orders.get(coid))
        else {
            return (None, None, None);
        };
        let account = if self.mount_engine.get(mount_idx) == Some(&eidx) {
            self.mounts[mount_idx]
                .as_ref()
                .and_then(|m| m.account.as_ref())
                .and_then(|a| a.text())
                .map(str::to_string)
        } else {
            None
        };
        (Some(req.venue.clone()), Some(req.symbol.clone()), account)
    }

    /// Put ONE restored order back under its owner at `slot` — the second half of restoring an
    /// ownership entry (the first, `coid_mount`, is written by the caller exactly as before).
    /// Called by [`assemble_core`] for every entry whose mount has a slot at boot and by
    /// [`Self::mount_strategy_runtime`] for every pending entry a runtime mount takes over. Does
    /// nothing at all under [`CoreConfig::restore_orders_off`].
    ///
    /// 1. **The strategy tag.** An entry with a venue, a symbol and a tag is filed back under
    ///    `{slot}|…|{tag}` in `strategy_tags` ([`Self::tag_key`], the mount's own series), so a
    ///    lifecycle event for the order is stamped with the tag the strategy keys on
    ///    ([`Self::tag_for_coid`] / [`Self::retire_tag_for_coid`]) and `cancel_tagged` finds it.
    ///    Entries arrive oldest first, so when two restored orders carry one tag the NEWER wins,
    ///    which is what the live registry did when the second was submitted.
    /// 2. **The engine.** An order on a non-primary engine gets its `coid_venue` row, the same row
    ///    a submit writes, so its lifecycle events route to the engine that holds it and not to the
    ///    primary. The engine is the labelled account's route key when the entry names one, else
    ///    the mount's own routing. An engine this core cannot resolve (an account no longer run)
    ///    leaves the row unset, which is today's primary fallback, and says nothing here.
    /// 3. **The collection.** The entry joins [`Self::restored_orders`], whatever its facts, for
    ///    the reconcile adoption step.
    pub(crate) fn restore_owned_order(
        &mut self,
        slot: usize,
        order: crate::order_owners::OwnedOrder,
    ) {
        if self.config.restore_orders_off {
            return;
        }
        if let (Some(venue), Some(symbol), Some(tag)) = (&order.venue, &order.symbol, &order.tag) {
            let key = self.tag_key(slot, venue, symbol, tag);
            self.strategy_tags.insert(key, order.coid.clone());
        }
        if let Some(venue) = order.venue.as_deref()
            && let Some(eidx) = self.restored_engine(slot, venue, order.account.as_deref())
            && eidx != 0
        {
            self.coid_venue.insert(order.coid.clone(), eidx);
        }
        self.restored_orders.insert(order.coid.clone(), order);
    }

    /// The engine index a restored order of `venue` (and `account` label text, when it named one)
    /// lives on; see [`Self::restore_owned_order`].
    fn restored_engine(&self, slot: usize, venue: &str, account: Option<&str>) -> Option<usize> {
        use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
        match account {
            None => self.route_of(EngineRoute::Mount(slot), venue),
            Some(text) => {
                let label = AccountLabel::parse(text).ok()?;
                let key = route_key_of(venue, &label);
                self.engine_idx_for_route_key(RouteKey::declared(&key))
            }
        }
    }

    /// Send one record to the ownership file; nothing when it is off.
    pub(crate) fn record_owner(&mut self, rec: crate::order_owners::OwnerRecord) {
        if let Some(log) = self.order_owners.as_mut() {
            log.record(rec);
        }
    }

    /// Whether a LIVE mount sits on `(venue, symbol)`, its own series or a declared leg — the pair the
    /// retired first-mount fallback used to deliver an unminted event for, and so the only pair where
    /// "reached no strategy" is news. It gates the recent-events note and nothing else: it routes
    /// nothing. A leg's venue comes from [`Self::leg_venue`].
    fn mount_sits_on(&self, venue: &str, symbol: &str) -> bool {
        (0..self.mounts.len()).any(|i| {
            let Some(m) = self.mounts[i].as_ref() else {
                return false;
            };
            (m.venue == venue && m.symbol == symbol)
                || self.mount_symbols[i]
                    .iter()
                    .any(|leg| leg.symbol == symbol && self.leg_venue(i, symbol, &m.venue) == venue)
        })
    }
}
