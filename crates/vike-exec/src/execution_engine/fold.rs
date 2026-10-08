//! The venue-event fold: the engine's `EventHandler::on_event` and its lifecycle helpers.

use vike_model::FiniteNumbers;
use vike_model::events::{Event, FillEvent, OrderLiquidated, PositionLiquidated};

use crate::bus::{EventHandler, Fold, Outbox};
use crate::order::OrderStatus;

use super::{AppliedFill, ExecutionClient, ExecutionEngine, OrderEventOut};

impl<C: ExecutionClient> ExecutionEngine<C> {
    /// Most-recent live order on this symbol/side — the leg a liquidation force-closed.
    /// One-way (BOTH) keys by symbol only; hedge (LONG/SHORT) also requires the order's leg
    /// (from request.side: +1 → LONG, -1 → SHORT) to match. Reversed insertion-order scan,
    /// first non-terminal wins. None if nothing matches (Account still flattens by key).
    fn coid_for_position(&self, ev: &PositionLiquidated) -> Option<String> {
        let want_side = match ev.position_side {
            vike_model::events::PositionSide::Long => Some("LONG"),
            vike_model::events::PositionSide::Short => Some("SHORT"),
            _ => None,
        };
        for (coid, mo) in self.registry.iter().rev() {
            // ⚠ POSITIVE selection on the FSM's OWN allowed-from set, not a negation of a few
            // statuses: newest-first, an order `order.rs`'s `transition` refuses for
            // `OrderLiquidated` (e.g. still SUBMITTED) would shadow the ACCEPTED leg actually
            // force-closed. `can_receive_liquidation` makes the picker and the applier one
            // predicate, so they cannot drift.
            if mo.request.symbol != ev.symbol || !mo.status.can_receive_liquidation() {
                continue;
            }
            if let Some(want) = want_side {
                let order_leg = if mo.request.side > 0 { "LONG" } else { "SHORT" };
                if order_leg != want {
                    continue;
                }
            }
            return Some(coid.clone());
        }
        None
    }

    fn lifecycle_coid(event: &Event) -> Option<&str> {
        // OrderLiquidated is NOT lifecycle-dispatched (the PositionLiquidated branch applies it).
        match event {
            Event::OrderSubmitted(e) => Some(&e.client_order_id),
            Event::OrderAccepted(e) => Some(&e.client_order_id),
            Event::OrderTriggered(e) => Some(&e.client_order_id),
            Event::OrderPartiallyFilled(e) => Some(&e.client_order_id),
            Event::OrderFilled(e) => Some(&e.client_order_id),
            Event::OrderCanceled(e) => Some(&e.client_order_id),
            Event::OrderRejected(e) => Some(&e.client_order_id),
            Event::OrderExpired(e) => Some(&e.client_order_id),
            Event::OrderDenied(e) => Some(&e.client_order_id),
            // RUST-NATIVE modify — folds via `mo.apply` (self-transition) + persist_order.
            Event::OrderModified(e) => Some(&e.client_order_id),
            // RUST-NATIVE cancel/modify-reject advisories — a non-terminal self-transition, so the
            // failed intent is journaled.
            Event::OrderCancelRejected(e) => Some(&e.client_order_id),
            Event::OrderModifyRejected(e) => Some(&e.client_order_id),
            _ => None,
        }
    }

    /// Coid-ownership routing for a bare [`Event::Fill`] whose symbol is NOT mounted (the
    /// fill-drop hazard). True iff the fill names an order THIS engine manages (registry hit on
    /// its `client_order_id`) AND carries position truth for that order — the order's own
    /// `request.symbol` (a single-leg order on a never-mounted instrument, e.g. a deribit option
    /// ticket) or one of its `combo_legs` symbols (the real per-leg position deltas). Without it
    /// such a fill is symbol-filtered: order Filled via the wrap, `Account` flat.
    ///
    /// The combo-INSTRUMENT net print matches NEITHER arm (a combo request's `symbol` is EMPTY by
    /// `build_combo`'s contract, and the venue-minted combo id is not a leg) and deliberately
    /// stays OUT of the fold: Deribit reports one combo execution as N leg rows PLUS one
    /// aggregate net-price row under the combo instrument, and books NO position under the combo
    /// id (`vike-deribit`'s `deribit_combo_fill_probe`). Folding the net print would mint a
    /// phantom position at the net price and double-count PnL.
    ///
    /// Hot-fold budget: mounted-symbol fills short-circuit at [`Self::accepts_symbol`];
    /// label-less fills return on the empty check without hashing; only LABELED foreign-symbol
    /// fills pay the one registry probe. No logging on any path (the p99 gate).
    fn owns_fill_symbol(&self, fill: &FillEvent) -> bool {
        if fill.client_order_id.is_empty() {
            return false;
        }
        let Some(mo) = self.registry.get(&fill.client_order_id) else {
            return false;
        };
        let sym = fill.symbol.as_str();
        sym == mo.request.symbol || mo.request.combo_legs.iter().any(|l| l.symbol == sym)
    }

    /// Refuse one money-lane event whose folded f64s were not all finite: count it, say so loudly,
    /// and answer [`Fold::Dropped`]. The ONE policy site for [`vike_model::FiniteNumbers`] (the
    /// predicate is pure; this decides what a violation DOES).
    ///
    /// **Drop, not halt**: halting on one malformed frame would hand any single venue a kill
    /// switch over every OTHER venue in the process. Dropping costs this one event, RECOVERABLY —
    /// reconcile raises the gap as `MissingFill` / `PositionDrift` — while a folded NaN propagates
    /// through `compute_fill` into `avg_px` and makes every later comparison false, so reconcile
    /// cannot even SEE the divergence.
    ///
    /// **ERROR, not warn**: no venue legitimately sends a non-finite number, so one occurrence is a
    /// venue bug or a compromised socket, and both want an operator.
    ///
    /// `#[cold]` + `#[inline(never)]` keep this formatting off the per-message fold the
    /// `p99 < 10µs` gate measures.
    #[cold]
    #[inline(never)]
    fn refuse_nonfinite(&mut self, lane: &str, symbol: &str, coid: &str) -> Fold {
        self.dropped_nonfinite += 1;
        tracing::error!(
            target: "vike_exec::oms",
            lane,
            venue = %self.venue,
            symbol,
            coid,
            "REFUSED a venue event carrying a NON-FINITE number (NaN/inf) — no venue sends this; \
             the event was dropped so it cannot poison the ledger. Treat a nonzero \
             dropped_nonfinite as a venue fault or a compromised feed, and reconcile the venue."
        );
        Fold::Dropped
    }
}

/// A terminal lifecycle event: one that (in a legal transition) closes the order. Used by the
/// `dropped_terminal_on_live` check to tell a genuinely-lost terminal from a benign replay.
fn is_terminal_event(event: &Event) -> bool {
    matches!(
        event,
        Event::OrderFilled(_)
            | Event::OrderCanceled(_)
            | Event::OrderRejected(_)
            | Event::OrderExpired(_)
            | Event::OrderDenied(_)
    )
}

/// A venue LIVENESS or EXECUTION event: the venue asserting the order is working (`OrderAccepted`)
/// or has executed (`OrderPartiallyFilled`/`OrderFilled`). Distinct from [`is_terminal_event`]:
/// `OrderAccepted`/`OrderPartiallyFilled` are non-terminal, but all three prove the venue considered
/// the order LIVE — so one arriving (and failing to apply) on an already-terminalized order is the
/// stranded-position signal `stranded_terminal_drops` counts.
fn is_liveness_or_fill_event(event: &Event) -> bool {
    matches!(
        event,
        Event::OrderAccepted(_) | Event::OrderPartiallyFilled(_) | Event::OrderFilled(_)
    )
}

/// A KILL terminal: an order closed WITHOUT completing (venue/gate rejected, canceled, expired, or
/// denied it). Excludes `Filled` (legitimate completion — a duplicate fill is benign, not a strand)
/// and `Liquidated` (a live perp force-close, not `is_terminal`). A venue liveness/fill event landing
/// on one of these is a contradiction: the order the venue is executing is one we believe we killed.
fn is_kill_terminal(status: OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::Rejected | OrderStatus::Canceled | OrderStatus::Expired | OrderStatus::Denied
    )
}

impl<C: ExecutionClient> EventHandler for ExecutionEngine<C> {
    fn on_event(&mut self, event: &Event, _outbox: &mut Outbox) -> Fold {
        if let Event::Fill(fill) = event {
            if !self.accepts_symbol(&fill.symbol) && !self.owns_fill_symbol(fill) {
                // account-wide WS stream: not this engine's order — OR our own combo's aggregate
                // net-price print, which must never fold (see `owns_fill_symbol`).
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD, BEFORE the dedup insert so a rejected fill does not burn its
            // `trade_id` (a well-formed retransmission with the same id must still fold). See
            // `vike_model::FiniteNumbers`.
            if !fill.numbers_finite() {
                return self.refuse_nonfinite("Fill", &fill.symbol, &fill.client_order_id);
            }
            // Reconnect-replay dedup. ⚠ The check is inside `Account::apply_fill`, on the aggregate
            // that owns the money; this site only OBSERVES the verdict, because several paths can
            // deliver an already-folded fill (`exec_actor::run_loop`'s history resync,
            // `run_resync_supervisor`'s replay, hyperliquid's per-reconnect `userFills` snapshot)
            // and a guard in front of the mutation protects only the callers that remember it. The
            // guard is TOTAL: `fill.trade_id` is a `TradeId`, which cannot be empty (#1341).
            if self.account.apply_fill(fill) == crate::account::FillFold::Duplicate {
                return Fold::Dropped; // reconnect replay — the account refused it, nothing moved
            }
            if let Some(mp) = fill.mark_price {
                // `mark_price` is deliberately NOT part of `numbers_finite` (a decorative field must
                // not discard a money event); `Account::set_mark_from`, THE single writer of the
                // mark slot, guards it. ⚠ `mp > 0.0` alone does NOT: `+inf > 0.0` is TRUE.
                if mp > 0.0 {
                    // `fill.mark_price` is the venue's mark at fill time — a genuine venue mark.
                    // Aged on the CORE clock (`now_ms`), not `fill.ts`: see `set_mark_from`.
                    self.account.set_mark_from(
                        &fill.venue,
                        &fill.symbol,
                        mp,
                        crate::MarkSource::VenueMark,
                        self.now_ms,
                    );
                    self.price_board.set_mark(&fill.venue, &fill.symbol, mp, fill.ts);
                }
            }
            if self.collect_applied_fills {
                // Snapshot the state HERE, per fill — a multi-fill batch must deliver each
                // `on_fill` with the position/equity after THAT fill, exactly matching the
                // backtest engine's synchronous firing point (fold → fire, one at a time).
                self.applied_fills.push(AppliedFill {
                    // Keyed on the FILL's symbol, not the engine's primary: `position_size` reads
                    // `self.symbol`, another instrument on an engine with `extra_symbols`. The
                    // `"BOTH"` bucket stays on purpose (hedge mode is a separate concern).
                    position_after: self.position_size_of(&fill.symbol, "BOTH"),
                    // ⚠ CAPPED: a strategy-facing equity (the `ctx.equity` a `Strategy::on_fill`
                    // handler sizes from), computed from `Account::equity_all` rather than
                    // `resolved_equity`, so a grep for the obvious symbol misses it; uncapped it
                    // would be the one hole in the ceiling. `Self::cap_sizing_equity` is the
                    // comparison shared with `Self::sizing_equity`.
                    equity_after: self.cap_sizing_equity(self.account.equity_all(self.equity_seed)),
                    fill: fill.clone(),
                });
            }
            return Fold::Applied;
        }
        if let Some(coid) = Self::lifecycle_coid(event) {
            let coid = coid.to_string();
            if !self.registry.contains_key(&coid) {
                // A lifecycle event for an unknown order (pre-restart order absent from reconcile,
                // an external-account order, or a bug): dropped, but counted + logged at debug (not
                // warn: a shared exchange account legitimately streams other sources' orders).
                self.dropped_unknown_coid += 1;
                tracing::debug!(target: "vike_exec::execution_engine", coid = %coid, "lifecycle event for unknown order dropped");
                return Fold::Dropped;
            }
            // FSM-side fill dedup: a reconnect-replayed fill re-emits the wrap, which would re-run
            // accumulate_fill and double-count filled_qty/avg_fill_px. A SEPARATE set: the
            // Account's ledger was already consumed by the preceding bare FillEvent.
            // `Option<&TradeId>`: `None` means only "this lifecycle event carries no fill", and
            // `Some` is always a real id (an empty-string sentinel once conflated the two).
            let tid: Option<&vike_model::events::TradeId> = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill.trade_id),
                Event::OrderFilled(w) => Some(&w.fill.trade_id),
                _ => None,
            };
            if let Some(tid) = tid
                && self.seen_fsm_trade_ids.contains(tid.as_str())
            {
                return Fold::Dropped; // reconnect replay — the FSM already advanced for this fill
            }
            // HOSTILE-VENUE GUARD, the FSM side: a fill WRAP's embedded `FillEvent` is a second,
            // independent path to a poisoned number (`accumulate_fill` folds it into
            // `filled_qty`/`avg_fill_px`), and a NaN `avg_fill_px` makes every later
            // `remaining_qty` comparison false, so the order never completes.
            let wrap_fill = match event {
                Event::OrderPartiallyFilled(w) => Some(&w.fill),
                Event::OrderFilled(w) => Some(&w.fill),
                _ => None,
            };
            if let Some(f) = wrap_fill
                && !f.numbers_finite()
            {
                return self.refuse_nonfinite("fill wrap", &f.symbol, &coid);
            }
            let Some(mo) = self.registry.get_mut(&coid) else {
                return Fold::Dropped; // unreachable (checked above) — but never panic in the fold
            };
            // --- Cancel-vs-fill race guard (LEAN `CancelPendingOrders` semantics) ---
            // A local cancel never mutates status (`cancel_order` is fire-and-forget), but an order
            // can sit at PENDING_CANCEL via venue-status seeding (`reregister_orders` / the
            // journal-view path), and the r5 fixture-pinned FSM table rejects fills from that state
            // and leaves a cancel-reject inert — dropping venue truth and stranding the order until
            // a reconcile reap mislabels a FILLED order CANCELED. So restore the pre-cancel live
            // status HERE, before the one `apply` site (`ManagedOrder::resolve_pending_cancel`),
            // and the event applies through the UNCHANGED table. Only a fill wrap or the venue's
            // cancel-reject resolve it; `OrderCanceled` does NOT (PENDING_CANCEL → CANCELED is the
            // legal ack edge). Fold-deterministic: no new state, replay and state-hash unchanged.
            if matches!(
                event,
                Event::OrderPartiallyFilled(_)
                    | Event::OrderFilled(_)
                    | Event::OrderCancelRejected(_)
            ) && mo.resolve_pending_cancel()
            {
                // Per-ORDER fault-adjacent boundary (the race is rare) — never per-message.
                tracing::warn!(
                    target: "vike_exec::oms",
                    coid = %coid,
                    restored = mo.status.as_str(),
                    "cancel-vs-fill race: PENDING_CANCEL order restored to its pre-cancel status so the venue event applies"
                );
            }
            let apply_result = mo.apply(event);
            let status_after = mo.status; // Copy — releases the &mut mo borrow for the counter below
            if apply_result.is_err() {
                // Normally a benign idempotent/out-of-order WS replay — but a TERMINAL event
                // failing on a still-LIVE order is a genuinely-lost terminal (e.g. OrderCanceled
                // before OrderAccepted): the order can stay live forever. Count + warn on THAT case
                // only (recovery needs reconcile).
                if is_terminal_event(event) && !status_after.is_terminal() {
                    self.dropped_terminal_on_live += 1;
                    tracing::warn!(
                        target: "vike_exec::execution_engine",
                        coid = %coid,
                        status = status_after.as_str(),
                        "terminal event dropped on a live order (out-of-order); order may be stranded"
                    );
                } else if is_liveness_or_fill_event(event) && is_kill_terminal(status_after) {
                    // A venue liveness/execution event dropped onto an order we ALREADY killed (the
                    // watchdog stage-2 phantom-reject signature): a live position may be stranded.
                    // The branch above cannot see it (`is_terminal()` is TRUE here). Still dropped;
                    // counted + warned.
                    self.stranded_terminal_drops += 1;
                    tracing::warn!(
                        target: "vike_exec::execution_engine",
                        coid = %coid,
                        status = status_after.as_str(),
                        "venue liveness/fill event dropped onto an already-terminalized order; a live position may be STRANDED (premature terminal, e.g. watchdog phantom-reject)"
                    );
                }
                return Fold::Dropped; // idempotent/out-of-order WS replay — skip (Fix 2)
            }
            if let Some(tid) = tid {
                // mark seen only after a successful apply (`Some` ⇒ a real id).
                self.seen_fsm_trade_ids.insert(tid.to_string());
            }
            // Capture the NON-FILL lifecycle transition for `Strategy::on_order_event`
            // (`from_event` is None for fills, which take the `on_fill` lane). (venue, symbol)
            // come from the advanced order. A DENIED order is captured at the RiskGate veto site
            // instead (it never enters the registry).
            if self.collect_applied_fills
                && let Some(lc) = vike_model::OrderLifecycle::from_event(event)
                && let Some((venue, symbol)) = self
                    .registry
                    .get(&coid)
                    .map(|mo| (mo.request.venue.clone(), mo.request.symbol.clone()))
            {
                self.order_events.push(OrderEventOut { venue, symbol, event: lc });
            }
            return Fold::Applied;
        }
        if let Event::Funding(ev) = event {
            if !self.accepts_symbol(&ev.symbol) {
                return Fold::Dropped;
            }
            // …and the ROUTING backstop, the twin of the `AccountState` arm's: a stamped key naming
            // another account of this exchange must not fold here. Load-bearing: `accepts_symbol`
            // is TRUE on both engines when two accounts trade one instrument. An unstamped payload
            // (`None`, every single-account box) skips it.
            if let Some(key) = ev.route_key
                && key.as_str() != self.route_key
            {
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD: `amount` lands straight on `balance`/`funding_paid`.
            if !ev.numbers_finite() {
                return self.refuse_nonfinite("Funding", &ev.symbol, "");
            }
            self.account.apply_funding(ev);
            return Fold::Applied;
        }
        if let Event::AccountState(ev) = event {
            // LABEL vs LABEL, so it reads `venue`, NOT `route_key`: `vike_core`'s `route_event` has
            // ALREADY chosen this engine, and this filters an account-wide stream ("is this
            // payload from my exchange"). `AccountState::venue` is always a canonical roster id,
            // so comparing it against a decorated route_key would silently drop every
            // AccountState for that engine.
            if ev.venue != self.venue {
                return Fold::Dropped;
            }
            // …and the SECOND filter, about routing, firing only on a stamped `route_key`
            // (`vike_mount::account_event_sender`). `vike_core::CoreThread::route_event` already
            // sends a stamped payload to its engine; this is the backstop for a key naming NO
            // mounted engine, which falls through to the venue lookup and would otherwise
            // overwrite the DEFAULT account's balance with a labelled account's money. Unstamped
            // (`None`: every single-account box, and every correction `vike_exec::recon::resolve`
            // synthesizes) skips it.
            if let Some(key) = ev.route_key
                && key.as_str() != self.route_key
            {
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD: an AUTHORITATIVE balance assignment — the most damaging of the
            // four, since it OVERWRITES `balance` outright (and flips `balance_mode`) rather than
            // accumulating into it.
            if !ev.numbers_finite() {
                return self.refuse_nonfinite("AccountState", "", "");
            }
            let qa = self.quote_asset.clone();
            self.account.apply_account_state(ev, &qa);
            return Fold::Applied;
        }
        if let Event::PositionLiquidated(ev) = event {
            if !self.accepts_symbol(&ev.symbol) {
                return Fold::Dropped;
            }
            // The routing backstop (as in the funding arm), BEFORE the dedup: `seen_liq_ids` is
            // per-engine, so it cannot stand in for routing. The most damaging arm:
            // `apply_liquidation` CLOSES the position and books the realized PnL.
            if let Some(key) = ev.route_key
                && key.as_str() != self.route_key
            {
                return Fold::Dropped;
            }
            // HOSTILE-VENUE GUARD: `liq_price` reaches `compute_fill` and `fee` reaches `balance`.
            if !ev.numbers_finite() {
                return self.refuse_nonfinite("PositionLiquidated", &ev.symbol, "");
            }
            // Liquidation dedup: a WS reconnect can replay a partial liq frame. Mirror the
            // FillEvent guard — an empty trade_id skips dedup and always applies (the legacy
            // whole-flatten path); a distinct id closes its own clamped qty exactly once.
            if !ev.trade_id.is_empty() {
                if self.seen_liq_ids.contains(ev.trade_id.as_str()) {
                    return Fold::Dropped; // reconnect replay — drop
                }
                self.seen_liq_ids.insert(ev.trade_id.to_string());
            }
            self.account.apply_liquidation(ev);
            if let Some(coid) = self.coid_for_position(ev)
                && let Some(mo) = self.registry.get_mut(&coid)
            {
                let liq = Event::OrderLiquidated(OrderLiquidated {
                    client_order_id: mo.client_order_id().to_string(),
                    liq_price: ev.liq_price,
                    ts: ev.ts,
                });
                // ⚠ `coid_for_position` selects on the FSM's own allowed-from set, so this cannot
                // be refused; log rather than discard — if it fires, the picker and the applier
                // have drifted apart.
                if let Err(e) = mo.apply(&liq) {
                    tracing::warn!(
                        target: "vike_exec::execution_engine",
                        coid = %coid,
                        status = ?mo.status,
                        error = %e,
                        "liquidation leg refused the OrderLiquidated transition — the picker \
                         and the FSM disagree about which orders can receive it"
                    );
                }
            }
            return Fold::Applied;
        }
        // PositionOpened/Changed/Closed: derived read-model events — no engine state to fold.
        Fold::Dropped
    }
}
