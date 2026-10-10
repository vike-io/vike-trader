//! Venue-event ingest: route an event to its engine, publish it, drain the market slot, pump in-process clients.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Publish one event to the engine at `idx` through the bus.
    pub(crate) fn publish_to(&mut self, idx: usize, ev: Event) {
        if idx == 0 {
            self.bus.publish(ev, &mut self.engine);
        } else {
            self.bus.publish(ev, &mut self.extra_engines[idx - 1].1);
        }
    }

    /// Route an inbound venue event: venue-tagged payloads by venue, order-lifecycle
    /// replies via the coid map, everything else (and every miss) to the primary —
    /// exactly today's behavior when no extra engines exist.
    ///
    /// The venue-tagged arms feed a venue string to
    /// [`Self::engine_idx_for_route_key`], which is correct while every engine's route key IS its
    /// canonical venue. The coid arm below is the shape that already survives a second account
    /// without a wire change: `coid_venue` maps a client-order-id to a routing INDEX, resolved at
    /// SUBMIT time when the engine was unambiguous, so an order-lifecycle reply needs nothing on
    /// the payload to find its engine.
    ///
    /// A venue-tagged payload reaches that coid map only through the two-account branch below,
    /// which closes the gap with the THREE answers a payload can carry, in order of exactness:
    ///
    /// 1. its own stamped route key ([`event_route_key`] — [`Event::AccountState`],
    ///    [`Event::Funding`] and [`Event::PositionLiquidated`]: every venue-tagged payload with no
    ///    CLIENT-ORDER-ID, which is that function's membership rule);
    /// 2. its CLIENT-ORDER-ID ([`event_coid`], which lists [`Event::Fill`] for exactly this reason),
    ///    resolved through `coid_venue` — exact for every order this process placed;
    /// 3. its symbol ([`event_symbol`]), and ONLY when exactly one engine of the venue claims it.
    ///
    /// ⚠ Step 1 said "[`Event::AccountState`] only, because it is the one such payload with
    /// neither a coid nor a symbol" until the commit that stamped the other two. Funding and
    /// liquidations DO carry a symbol — that was the whole basis for leaving them unstamped, and
    /// it is why they fell to step 3 and then, on a shared symbol, past it. Read
    /// [`event_route_key`]'s own doc for what that cost; the membership rule lives there, so this
    /// list must never restate it as a narrower one.
    ///
    /// ⚠ Step 3 used to be step 2 and was described as "exact by the mount's collision rule". That
    /// rule — no two active accounts of one venue armed on one symbol — is DELETED
    /// (`vike_config::venue_accounts`: two accounts on one instrument is an ordinary spread), so
    /// the symbol became the LAST resort and [`Self::engine_idx_for_venue_symbol`] answers `None`
    /// on ambiguity rather than taking the first match. That function's own doc carries the one
    /// residual this leaves (a FOREIGN fill on a shared symbol).
    pub(crate) fn route_event(&self, ev: &Event) -> Option<usize> {
        let venue = match ev {
            Event::Fill(f) => Some(f.venue.as_str()),
            Event::AccountState(a) => Some(a.venue.as_str()),
            Event::Funding(f) => Some(f.venue.as_str()),
            Event::PositionLiquidated(p) => Some(p.venue.as_str()),
            Event::OrderSubmitted(_)
            | Event::OrderAccepted(_)
            | Event::OrderRejected(_)
            | Event::OrderDenied(_)
            | Event::OrderTriggered(_)
            | Event::OrderPartiallyFilled(_)
            | Event::OrderFilled(_)
            | Event::OrderCanceled(_)
            | Event::OrderExpired(_)
            | Event::OrderLiquidated(_)
            | Event::OrderModified(_)
            | Event::PositionOpened(_)
            | Event::PositionChanged(_)
            | Event::PositionClosed(_)
            | Event::OrderCancelRejected(_)
            | Event::OrderModifyRejected(_) => None,
        };
        if let Some(v) = venue {
            // ⚠ TWO ACCOUNTS OF ONE EXCHANGE: the venue lookup above resolves the FIRST engine
            // whose route key matches, and a second account's route key is `venue#LABEL`, so every
            // venue-tagged payload of BOTH accounts would fold into the default account's book —
            // the exact defect `ExecutionEngine::route_key`'s own doc names as the reason the field
            // exists. The wire cannot help: a `FillEvent` labelled `"binance"` says nothing about
            // WHICH binance account it belongs to.
            //
            // ⚠ THAT RULE WAS RELAXED, and this comment used to say what to do about it: the
            // symbol was an exact account key only because `vike_config` refused to arm two ACTIVE
            // accounts of one venue on one symbol, and it was written that "if that rule is ever
            // relaxed, this branch stops being sound". The rule IS relaxed — two accounts on one
            // instrument is an ordinary spread — so the ORDER below changed and the symbol became
            // the last resort rather than the answer:
            //
            //   1. the payload's own stamped route key — `AccountState`, `Funding` and
            //      `PositionLiquidated`, i.e. EVERY payload with no coid, all stamped by the mount;
            //   2. its CLIENT-ORDER-ID, resolved through `coid_venue` — recorded at SUBMIT, when
            //      the engine was unambiguous. `Event::Fill` carries one, and this is exact with
            //      nothing added to any wire, which is why it is preferred over the symbol rather
            //      than added beside it;
            //   3. the symbol, and only when EXACTLY ONE engine of the venue claims it
            //      (`engine_idx_for_venue_symbol` now answers `None` on ambiguity).
            //
            // Guarded by [`Self::multi_account`], so a single-account process takes the same
            // `return` it always took, having paid one predictable compare.
            if self.multi_account {
                // THE PAYLOAD'S OWN ANSWER, consulted first because it is the only EXACT one that
                // needs no rule held elsewhere to stay sound. `AccountState` is the one venue-tagged
                // payload with no symbol — an account-wide balance snapshot — so before this it fell
                // through to the venue lookup below and a second account's balances folded into the
                // FIRST account's book while its fills and positions routed correctly.
                //
                // The key is stamped by the MOUNT (`vike_mount::account_event_sender`), not by a
                // bridge: a venue adapter holds one credential set and cannot name an account. A
                // key that names no mounted engine falls through to the symbol/venue lookup rather
                // than being dropped here — the same unknown-key affordance
                // `engine_idx_for_route_key` has everywhere else.
                //
                // ⚠ It covers `Funding` and `PositionLiquidated` as well as `AccountState`, and
                // that is the whole of what makes step 3 a genuine last resort rather than the
                // ONLY answer for two of the three coid-less payloads. See `event_route_key`.
                if let Some(key) = event_route_key(ev)
                    && let Some(i) = self.engine_idx_for_route_key(RouteKey::declared(key))
                {
                    return Some(i);
                }
                // THE ORDER'S OWN ANSWER. `coid_venue` maps a client-order-id to a routing INDEX,
                // recorded at SUBMIT time when the engine was unambiguous, so a venue-tagged
                // payload naming one of OUR orders needs nothing on the wire to find its engine.
                // `Event::Fill` is the payload that carries a coid, and it is the one whose
                // misrouting moves a position into the wrong book.
                if let Some(coid) = event_coid(ev)
                    && let Some(&i) = self.coid_venue.get(coid)
                {
                    return Some(i);
                }
                if let Some(symbol) = event_symbol(ev)
                    && let Some(i) = self.engine_idx_for_venue_symbol(v, symbol)
                {
                    return Some(i);
                }
                // **§5.4 — THE LAST RUNG, AND THE ONLY PAYLOAD IT REFUSES TO FOLD.**
                //
                // A `Fill` that has fallen through all three rungs on a venue this process runs
                // SEVERAL accounts of is, by construction, a fill naming no order this process
                // placed (`coid_venue` records every submit), on an instrument two of this venue's
                // accounts both trade. It is a FOREIGN fill, and
                // [`Self::engine_idx_for_venue_symbol`]'s own doc already declared it and already
                // assigned it — *"that is reconcile's territory, not this lane's"*. Folding it into
                // the venue's DEFAULT account writes a position and a realized PnL into a book that
                // never traded it, which is worse than reporting it unattributed.
                //
                // ⚠ **A FILL, AND ONLY A FILL — the other three venue-tagged payloads keep the
                // venue default, and dropping them here would be a catastrophic regression rather
                // than a safety fix.** `vike_exec::EventSender::stamp` stamps the route key onto
                // exactly `AccountState`, `Funding` and `PositionLiquidated` for every non-default
                // account, and stamps NOTHING when the key equals the payload's venue — which is
                // the default account's key by construction. So for those three an UNSTAMPED
                // payload reaching this rung IS the default account's, exactly, and refusing it
                // would stop the default account's balances, funding debits and liquidations from
                // folding at all on any multi-account venue. `Fill` is deliberately outside that
                // stamp (its coid is the exact handle), so it is the one payload whose arrival here
                // says nothing about which account it belongs to. The spec says "a foreign fill"
                // and it means the word.
                //
                // Guarded by [`Self::multi_account`] and then by the per-venue count, so a
                // single-account process — and a multi-account process's OTHER venues — take the
                // identical `return` they always took.
                if matches!(ev, Event::Fill(_)) && self.accounts_of_venue(v) > 1 {
                    return None;
                }
            }
            return Some(self.engine_idx_for_route_key(RouteKey::sole_account_of(v)).unwrap_or(0));
        }
        if let Some(coid) = event_coid(ev)
            && let Some(&i) = self.coid_venue.get(coid)
        {
            return Some(i);
        }
        Some(0)
    }

    pub(crate) fn drain_market(&mut self) {
        let (bar_close_ticks, ticks, forming) = {
            let mut st = self.market.state.lock().unwrap();
            st.marker_in_flight = false;
            (
                st.bar_close_slots.drain(..).map(|(_, t)| t).collect::<Vec<_>>(),
                st.slots.drain(..).map(|(_, t)| t).collect::<Vec<_>>(),
                st.forming.drain(..).collect::<Vec<_>>(),
            )
        };
        // ONE PRICE CONCEPT IN THE ACCOUNT MARK SLOT. The `PriceBoard` keeps a candle close and a
        // venue mark in DIFFERENT slots, so the resolver sees each under its true source — but the
        // account slot is a single untagged scalar the pre-trade gate, the margin-call law and
        // `LiveBroker.price` all read, so it must not alternate between concepts at whatever
        // cadence the feeds interleave. The precedence law lives INSIDE `Account::set_mark_from`
        // (see `MarkSource`) — not here — precisely so no lane can route around it; this arm just
        // names the concept it carries. Board slots are unconditional: they are source-tagged.
        let now = self.engine.now_ms;
        for t in bar_close_ticks {
            let eng = match self.engine_idx_for_route_key(RouteKey::sole_account_of(&t.venue)) {
                Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                _ => &mut self.engine,
            };
            eng.account.set_mark_from(&t.venue, &t.symbol, t.px, MarkSource::BarClose, now);
            eng.price_board.set_bar_close(&t.venue, &t.symbol, t.px, t.ts);
            self.mirror_venue_price(
                &t.venue,
                &t.symbol,
                t.px,
                MarkSource::BarClose,
                now,
                Some(t.ts),
            );
            self.dirty = true;
        }
        for t in ticks {
            {
                let eng = match self.engine_idx_for_route_key(RouteKey::sole_account_of(&t.venue)) {
                    Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                    _ => &mut self.engine,
                };
                eng.account.set_mark_from(&t.venue, &t.symbol, t.px, MarkSource::VenueMark, now);
                eng.price_board.set_mark(&t.venue, &t.symbol, t.px, t.ts);
            }
            self.mirror_venue_price(
                &t.venue,
                &t.symbol,
                t.px,
                MarkSource::VenueMark,
                now,
                Some(t.ts),
            );
            self.dirty = true;
            // Cross-symbol underlying-mark routing ("Option B"): a drained venue mark is ALSO the
            // underlying-spot feed for any maker mounted on a DIFFERENT (e.g. PM token) symbol that
            // declares THIS (venue, symbol) as its `underlying_symbol`. Early-returns inside when no
            // mount does, so a run with no underlying-anchored maker never leaves the mark write above.
            self.drive_strategy_mark(&t.venue, &t.symbol, t.px, t.ts);
            // Mark-lane conditional triggering (w2 `trigger_by`): a `Some(Mark)` arm evaluates
            // ONLY here, off the mark tick — the one lane that reproduces a mark-triggering
            // venue's (hyperliquid) SL/TP timing. Guarded by `has_mark_arms` inside, so a book
            // with no Mark arms costs one integer compare on this hot mark drain.
            self.fire_conditionals_at_mark(&t.venue, &t.symbol, t.px, t.ts);
        }
        for (key, bar) in forming {
            let series = self.bars.entry(key).or_default();
            // ignore a forming update older than the last close (reconnect race)
            if series.closed.last().is_none_or(|last| bar.ts > last.ts) {
                series.forming = Some(bar);
                self.dirty = true;
            }
        }
    }

    /// Pump venue events the in-process client synthesized (paper/test clients — `PaperExecution
    /// Client`, `TestExecutionClient`); real venue clients return `None` here, their events arrive
    /// over the ingest channel and are journaled at [`dispatch`](Self::dispatch)'s write-ahead site.
    ///
    /// SINK-ENABLEMENT: a client-synthesized event bypasses the ingest lane, so it would never hit
    /// that write-ahead site — historically the reason a *paper* session's fill stream was absent
    /// from the journal (documented in `tests/replay_fence.rs`'s module doc: a synthesizing client
    /// emits Submitted/Accepted/Fill/Filled INSIDE the submit dispatch, which "never reach the
    /// journal — replay could not reproduce them"). We journal each polled event HERE as the SAME
    /// `Ingest::Event` record a real venue event rides, so a paper session's fills are durable (the
    /// live tearsheet reads them back via `CommandJournal::read_all`) AND replay-reproducible. This
    /// runs on the cold bar-close / event-arm path — never the p99 event fold — and every hook is a
    /// single `Option::is_some` no-op when journaling is off, so the default path stays byte-identical.
    pub(crate) fn pump_client(&mut self) {
        // One wall-clock stamp for the whole drain: these synthesized events are a CONSEQUENCE of the
        // message dispatch that reached `pump_client`, so they share its `now_ms` (the same
        // "one now per message" discipline `dispatch` applies), not a fresh clock read per event.
        let now_ms = self.engine.now_ms;
        while let Some(ev) = self.engine.client.poll_events() {
            let ev = self.journal_pumped_event(now_ms, ev);
            // Live-runtime OTO/OCO: client-SYNTHESIZED fills (the paper exchange, and any in-process
            // client that emits via `poll_events`) surface HERE, not on the `Ingest::Event` lane — so
            // the contingency drive must run here too or a bracket never resolves on the paper/
            // backtest path. Classify BEFORE the bus consumes `ev`; drive AFTER it folds. The drive's
            // own submits/cancels queue further client events that THIS loop then drains (the drive
            // itself never pumps — see its doc). Gated on a non-empty book (byte-identical otherwise).
            //
            // ⚠ DELIBERATELY NOT GATED on the fold verdict, unlike the `Ingest::Event` venue lane.
            // `poll_events` is the IN-PROCESS client seam (the paper exchange and test clients);
            // real venue clients return `None` here, so nothing an attacker controls arrives on this
            // path and the hardening buys nothing. It would cost something, though:
            // `crates/vike-sim/tests/r7_gate.rs` pins backtest == paper BIT-FOR-BIT, and this
            // is the paper side of that equality.
            let drive = self.contingency_terminal(&ev);
            self.bus.publish(ev, &mut self.engine);
            match drive {
                Some((coid, true)) => self.drive_contingency_on_fill(&coid, now_ms),
                Some((coid, false)) => self.drive_contingency_on_terminal(&coid),
                None => {}
            }
            self.maybe_cadence_snap(now_ms);
        }
        for i in 0..self.extra_engines.len() {
            while let Some(ev) = self.extra_engines[i].1.client.poll_events() {
                let ev = self.journal_pumped_event(now_ms, ev);
                let drive = self.contingency_terminal(&ev);
                self.bus.publish(ev, &mut self.extra_engines[i].1);
                match drive {
                    Some((coid, true)) => self.drive_contingency_on_fill(&coid, now_ms),
                    Some((coid, false)) => self.drive_contingency_on_terminal(&coid),
                    None => {}
                }
                self.maybe_cadence_snap(now_ms);
            }
        }
    }

    /// Classify a folded event for the live-runtime OTO/OCO drive: `Some((coid, true))` for a FULL
    /// fill (arm held children + cancel OCO siblings), `Some((coid, false))` for a terminal-WITHOUT-
    /// fill (cascade-drop held children / clean the dead leg), `None` for everything else. Gated on a
    /// non-empty book so the no-bracket path is a single `is_empty` read. Shared by `pump_client`
    /// (client-synthesized events), the `Ingest::Event` arm (real venue events), AND the synchronous
    /// submit outboxes (`publish_and_drive_outbox`) — the ONE classifier every fold site consults so
    /// they cannot drift. `OrderDenied` (a RiskGate veto / Halted, emitted SYNCHRONOUSLY by
    /// `submit_order`) is a terminal-without-fill here: a denied bracket entry must cascade-drop its
    /// held children, never orphan them.
    pub(crate) fn contingency_terminal(&self, ev: &Event) -> Option<(String, bool)> {
        if self.contingency.is_empty() {
            return None;
        }
        match ev {
            Event::OrderFilled(e) => Some((e.client_order_id.clone(), true)),
            Event::OrderCanceled(e) => Some((e.client_order_id.clone(), false)),
            Event::OrderRejected(e) => Some((e.client_order_id.clone(), false)),
            Event::OrderDenied(e) => Some((e.client_order_id.clone(), false)),
            Event::OrderExpired(e) => Some((e.client_order_id.clone(), false)),
            Event::Fill(_)
            | Event::OrderSubmitted(_)
            | Event::OrderAccepted(_)
            | Event::OrderTriggered(_)
            | Event::OrderPartiallyFilled(_)
            | Event::OrderLiquidated(_)
            | Event::OrderModified(_)
            | Event::PositionOpened(_)
            | Event::PositionChanged(_)
            | Event::PositionClosed(_)
            | Event::AccountState(_)
            | Event::Funding(_)
            | Event::PositionLiquidated(_)
            | Event::OrderCancelRejected(_)
            | Event::OrderModifyRejected(_) => None,
        }
    }

    /// Publish a SYNCHRONOUS submit outbox to engine `idx` and drive the contingency book for any
    /// terminal in it — the submit-site twin of the `Ingest::Event`/`pump_client` drive. A synchronous
    /// `submit_order`/`submit_order_batch` can emit `OrderDenied` (RiskGate veto / Halted) or a
    /// capability `OrderRejected` right here, drained by a bare `publish_to`; without driving it, a
    /// DENIED bracket entry would leave its held OTO children orphaned forever (never armed, never
    /// removed, re-Snapped every checkpoint). Publish FIRST (the engine folds the terminal), THEN
    /// drive — the same order the other two fold sites use. Byte-identical no-bracket path (the
    /// classifier early-returns on an empty book).
    ///
    /// ⚠ DELIBERATELY NOT GATED ON THE FOLD VERDICT, unlike the `Ingest::Event` venue lane. This is
    /// not an oversight to be tidied up: `gate_and_register` publishes `OrderDenied` for an order it
    /// deliberately never REGISTERED, so that event folds `Fold::Dropped` BY DESIGN (it lands on
    /// `on_event`'s unknown-coid branch and even moves `dropped_unknown_coid`). Gating here would
    /// therefore suppress exactly the cascade-drop this function exists to perform, orphaning a
    /// denied bracket entry's held children forever. These events are engine-synthesized and
    /// unreachable by a venue — `Event` is never deserialized from a venue payload — so the hostile
    /// -venue argument that gates the other lane does not apply.
    pub(crate) fn publish_and_drive_outbox(&mut self, idx: usize, mut outbox: Outbox, now: i64) {
        while let Some(ev) = outbox.0.pop_front() {
            let drive = self.contingency_terminal(&ev);
            self.publish_to(idx, ev);
            match drive {
                Some((coid, true)) => self.drive_contingency_on_fill(&coid, now),
                Some((coid, false)) => self.drive_contingency_on_terminal(&coid),
                None => {}
            }
        }
    }
}
