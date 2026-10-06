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
    /// path. An order NO mount minted falls back to the first mount on its (venue, symbol).
    ///
    /// Fills are NOT here (they stay on [`Self::dispatch_applied_fills`], which is why
    /// `OrderLifecycle::from_event` returns `None` for them — no double-delivery). An order event
    /// folds at ORDER cadence (a submit reply / a cancel), never per quote or bar, so this touches
    /// the `p99 < 10µs` per-market-message fold NOT AT ALL. `now` is the dispatch wall-clock stamp
    /// (as in `on_feed_status`/`on_params_updated`), `price` the standing mark (else `0.0`). NOT
    /// warmup-gated (an order outcome is an account fact regardless of bar count, mirroring
    /// `on_fill`). No mount on the pair ⇒ the event is dropped (an order no live strategy owns). An
    /// empty buffer is a single length check + return — zero cost when nothing is pending or no
    /// strategy is mounted.
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
            // liquidation, adopted venue order) falls back to the historical FIRST mount on
            // (venue, symbol); no mount at all = drop.
            let Some(idx) = self
                .mount_for_coid(&oe.event.client_order_id)
                .or_else(|| self.mount_owning(&oe.venue, &oe.symbol))
            else {
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
            oe.event.tag = if oe.event.kind.is_terminal() {
                self.retire_tag_for_coid(idx, &oe.event.client_order_id)
            } else {
                self.tag_for_coid(idx, &oe.event.client_order_id)
            };
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
    /// NOT warmup-gated: the backtest `fire_on_fill` is unconditional, and live fills
    /// (manual tickets, reconcile-era executions) are account facts regardless of bar count.
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
                // attribution fold just used, so the ledger and the callback can never disagree
                // about which mount a fill belongs to. A fill no mount minted falls back to the
                // historical FIRST mount on (venue, symbol); no mount at all = drop.
                let Some(idx) = self
                    .mount_for_coid(f.client_order_id.as_str())
                    .or_else(|| self.mount_owning(&f.venue, &f.symbol))
                else {
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
            if let Some((coid, _)) = self.coid_terminal.pop_front() {
                self.coid_mount.remove(&coid);
            }
        }
    }
}
