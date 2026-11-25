//! Venue-event ingest: route an event to its engine, publish it, drain the market slot, pump in-process clients.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Publish one event to the engine at `index` through the bus.
    pub(crate) fn publish_to(
        &mut self,
        index: usize,
        event: Event,
    ) -> vike_execution::EventBusFold {
        if index == 0 {
            self.bus.publish(event, &mut self.engine)
        } else {
            self.bus.publish(event, &mut self.extra_engines[index - 1].1)
        }
    }

    /// Route an inbound venue event: venue-tagged payloads by venue, order-lifecycle
    /// replies via the order client id map, everything else (and every lifecycle miss) to the primary —
    /// exactly today's behavior when every venue runs its default account. A venue-tagged
    /// payload of a venue that runs none is never given to the primary (see
    /// [`Self::route_event_via`]).
    ///
    /// The venue-tagged arms feed a venue string to
    /// [`Self::engine_index_for_route_key`], which is correct while every engine's route key IS its
    /// canonical venue. The order client id arm below is the shape that already survives a second account
    /// without a wire change: `order_client_id_venue` maps an order-client-id to a routing INDEX, resolved at
    /// SUBMIT time when the engine was unambiguous, so an order-lifecycle reply needs nothing on
    /// the payload to find its engine.
    ///
    /// A venue-tagged payload reaches that order client id map only through the two-account branch below,
    /// which closes the gap with the THREE answers a payload can carry, in order of exactness:
    ///
    /// 1. its own stamped route key ([`event_route_key`] — [`Event::AccountState`],
    ///    [`Event::Funding`] and [`Event::PositionLiquidated`]: every venue-tagged payload with no
    ///    ORDER-CLIENT-ID, which is that function's membership rule);
    /// 2. its ORDER-CLIENT-ID ([`Event::order_client_id`], which answers for [`Event::Fill`] for exactly this reason),
    ///    resolved through `order_client_id_venue` — exact for every order this process placed;
    /// 3. its symbol ([`event_symbol`]), and ONLY when exactly one engine of the venue claims it.
    ///
    /// ⚠ Step 1 said "[`Event::AccountState`] only, because it is the one such payload with
    /// neither an order client id nor a symbol" until the commit that stamped the other two. Funding and
    /// liquidations DO carry a symbol — that was the whole basis for leaving them unstamped, and
    /// it is why they fell to step 3 and then, on a shared symbol, past it. Read
    /// [`event_route_key`]'s own doc for what that cost; the membership rule lives there, so this
    /// list must never restate it as a narrower one.
    ///
    /// ⚠ Step 3 used to be step 2 and was described as "exact by the mount's collision rule". That
    /// rule — no two active accounts of one venue armed on one symbol — is DELETED
    /// (`vike_config::venue_accounts`: two accounts on one instrument is an ordinary spread), so
    /// the symbol became the LAST resort and [`Self::engine_index_for_venue_symbol`] answers `None`
    /// on ambiguity rather than taking the first match. That function's own doc carries the one
    /// residual this leaves (a FOREIGN fill on a shared symbol).
    ///
    /// This is [`Self::route_event_via`] for an event with NO ring behind it (the `Ingest::Event`
    /// door, a client-synthesized event, a test).
    pub(crate) fn route_event(&self, event: &Event) -> Option<usize> {
        self.route_event_via(event, None)
    }

    /// [`Self::route_event`] for an event that may have come out of one gateway's ring
    /// (`Ingest::GatewayEvent`), whose `ring` is that ring's account in the spelling of an
    /// engine's `route_key`.
    ///
    /// ⚠ The ring is a FOURTH answer, for `Event::Fill` only, sitting between the order client id (rung 2) and
    /// the symbol (rung 3): a FOREIGN fill (a manual trade, no order client id of ours) is the one payload no
    /// other rung can attribute exactly, and the ring it arrived on names its account. Without it
    /// the symbol decided, so a manual fill in the LIVE hyperliquid ring on a symbol only the
    /// TESTNET engine claims folded into the testnet book (and the reverse); now the ring's own
    /// engine takes it, and drops it when it does not trade the symbol. A ring that names no
    /// mounted engine of the fill's venue falls through to today's order, unchanged; the order client id rung
    /// stays first, so a fill of an order this process placed routes exactly as it did.
    ///
    /// The boot refold (`crates/vike-core/src/replay/refold.rs`) replays a gateway event as the
    /// plain event on a base of engines the venue alone tells apart, where it needs no ring: its
    /// core is never `multi_account` and this rung is never reached there. On a base holding a
    /// labelled engine or two engines of one venue it replays the gateway event itself, so the ring
    /// reaches this rung exactly as it did live.
    ///
    /// ⚠ **`None` also means FOREIGN VENUE, and the answer is never an engine of another exchange.**
    /// A venue-tagged payload whose venue runs no DEFAULT account here (every account of it
    /// labelled, or none at all) is answered by [`Self::route_venue_without_default`], on a
    /// single-account node as on a multi-account one: its stamped key, order client id or ring, else the
    /// venue's only live engine, else `None`. It used to be engine 0, so a bybit fill could panic a
    /// binance engine.
    pub(crate) fn route_event_via(
        &self,
        event: &Event,
        ring: Option<vike_marketdata::Symbol>,
    ) -> Option<usize> {
        let venue = match event {
            Event::Fill(f) => Some(f.venue.as_str()),
            Event::FeeAdjustment(f) => Some(f.venue.as_str()),
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
            // the exact defect `Engine::route_key`'s own doc names as the reason the field
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
            //      `PositionLiquidated`, i.e. EVERY payload with no order client id, all stamped by the mount;
            //   2. its ORDER-CLIENT-ID, resolved through `order_client_id_venue` — recorded at SUBMIT, when
            //      the engine was unambiguous. `Event::Fill` carries one, and this is exact with
            //      nothing added to any wire, which is why it is preferred over the symbol rather
            //      than added beside it;
            //   3. the symbol, and only when EXACTLY ONE engine of the venue claims it
            //      (`engine_index_for_venue_symbol` now answers `None` on ambiguity).
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
                // The key is stamped by the MOUNT (`vike_tradehub_core::account_event_sender`), not by a
                // bridge: a venue adapter holds one credential set and cannot name an account. A
                // key that names no mounted engine falls through to the symbol/venue lookup rather
                // than being dropped here — the same unknown-key affordance
                // `engine_index_for_route_key` has everywhere else.
                //
                // ⚠ It covers `Funding` and `PositionLiquidated` as well as `AccountState`, and
                // that is the whole of what makes step 3 a genuine last resort rather than the
                // ONLY answer for two of the three order-client-id-less payloads. See `event_route_key`.
                if let Some(key) = event_route_key(event)
                    && let Some(i) = self.engine_index_for_route_key(RouteKey::declared(key))
                {
                    return Some(i);
                }
                // THE ORDER'S OWN ANSWER. `order_client_id_venue` maps an order-client-id to a routing INDEX,
                // recorded at SUBMIT time when the engine was unambiguous, so a venue-tagged
                // payload naming one of OUR orders needs nothing on the wire to find its engine.
                // `Event::Fill` is the payload that carries an order client id, and it is the one whose
                // misrouting moves a position into the wrong book.
                if let Some(order_client_id) = event.order_client_id()
                    && let Some(i) = self.engine_of_order_client_id(order_client_id)
                {
                    return Some(i);
                }
                // THE RING'S OWN ANSWER (see `route_event_via`): the account whose ring delivered
                // this FOREIGN fill, asked before the symbol because a symbol two accounts' venues
                // share says nothing about which of them traded. The venue guard keeps a ring of
                // another exchange from claiming it.
                if let Some(key) = ring
                    && matches!(event, Event::Fill(_))
                    && let Some(i) =
                        self.engine_index_for_route_key(RouteKey::declared(key.as_str()))
                    && self.engine(i).venue == v
                {
                    return Some(i);
                }
                if let Some(symbol) = event_symbol(event)
                    && let Some(i) = self.engine_index_for_venue_symbol(v, symbol)
                {
                    return Some(i);
                }
                // **§5.4 — THE LAST RUNG, AND THE ONLY PAYLOAD IT REFUSES TO FOLD.**
                //
                // A `Fill` that has fallen through all three rungs on a venue this process runs
                // SEVERAL accounts of is, by construction, a fill naming no order this process
                // placed (`order_client_id_venue` records every submit), on an instrument two of this venue's
                // accounts both trade. It is a FOREIGN fill, and
                // [`Self::engine_index_for_venue_symbol`]'s own doc already declared it and already
                // assigned it — *"that is reconcile's territory, not this lane's"*. Folding it into
                // the venue's DEFAULT account writes a position and a realized PnL into a book that
                // never traded it, which is worse than reporting it unattributed.
                //
                // ⚠ **A FILL, AND ONLY A FILL — the other three venue-tagged payloads keep the
                // venue default, and dropping them here would be a catastrophic regression rather
                // than a safety fix.** `vike_execution::EventSender::stamp` stamps the route key onto
                // exactly `AccountState`, `Funding` and `PositionLiquidated` for every non-default
                // account, and stamps NOTHING when the key equals the payload's venue — which is
                // the default account's key by construction. So for those three an UNSTAMPED
                // payload reaching this rung IS the default account's, exactly, and refusing it
                // would stop the default account's balances, funding debits and liquidations from
                // folding at all on any multi-account venue. `Fill` is deliberately outside that
                // stamp (its order client id is the exact handle), so it is the one payload whose arrival here
                // says nothing about which account it belongs to. The spec says "a foreign fill"
                // and it means the word.
                //
                // Guarded by [`Self::multi_account`] and then by the per-venue count, so a
                // single-account process — and a multi-account process's OTHER venues — take the
                // identical `return` they always took.
                // A fee ADJUSTMENT of such a fill is the same case: its order client id is one this process
                // never placed, so nothing says which account folded the fill. The engine would drop
                // it for want of the fill anyway (`FeeFold::UnknownFill`); refusing it here says so.
                if matches!(event, Event::Fill(_) | Event::FeeAdjustment(_))
                    && self.accounts_of_venue(v) > 1
                {
                    return None;
                }
            }
            // The venue's DEFAULT account, if one runs: its route key IS the bare venue, so the engine
            // is of the payload's venue by construction. Only a venue with no default account gets
            // past this lookup, and it used to get ENGINE 0 here, whichever exchange that is.
            return self
                .engine_index_for_route_key(RouteKey::sole_account_of(v))
                .or_else(|| self.route_venue_without_default(event, v, ring));
        }
        if let Some(order_client_id) = event.order_client_id()
            && let Some(i) = self.engine_of_order_client_id(order_client_id)
        {
            return Some(i);
        }
        Some(0)
    }

    /// **A venue-tagged payload of a venue that has NO default account here** — the accounts it runs
    /// are all labelled (`bybit#DEMO`), or it runs none. The answer is an engine of the payload's OWN
    /// venue or `None` (FOREIGN: the `dispatch` fold logs it as UNATTRIBUTED and folds it nowhere); it is
    /// never "engine 0", which is some other exchange's whenever this venue's engine is not the
    /// first. Folding a bybit fill into a binance engine panicked `Account::apply_fill`'s venue
    /// check; an UNSTAMPED funding payment or liquidation was booked into the wrong exchange's book
    /// (the engine has no venue check there), and a STAMPED one was dropped by the engine's own
    /// key check, so a labelled-only venue lost all of its funding, liquidations and balances.
    ///
    /// The handles a payload can carry, in the order [`Self::route_event_via`] asks them on a
    /// multi-account node:
    ///
    /// 1. its stamped route key (`AccountState`, `Funding`, `PositionLiquidated`). A stamped payload
    ///    says which account it belongs to, so the key is the whole answer: an engine of `v`, or
    ///    `None` when the key names none (nothing below may give it to another account);
    /// 2. its order client id through `order_client_id_venue` (the row a labelled account's order always gets,
    ///    [`Self::order_client_id_row_needed`]). It is not filtered by a stopped engine: a late fill of a
    ///    stopped account's own order follows its order client id to that engine;
    /// 3. for a fill, the gateway ring it arrived on, a stopped engine included. A ring that names a
    ///    DIFFERENT account of `v` (this core runs no engine for it) says the fill is that account's,
    ///    so it is `None` and nothing below may book it into the venue's only engine;
    /// 4. **the venue's ONLY live engine**, for an event that names nothing. An unlabelled event that
    ///    no handle resolved can belong to one account only when the venue has exactly one, so that
    ///    account takes it: it is no guess, and it is where the old `unwrap_or(0)` sent it whenever
    ///    that engine happened to be engine 0. With none, or with several (nothing says which), it
    ///    stays `None`. A STOPPED account (decision 0169) is no account of the exchange here, as in
    ///    [`Self::accounts_of_venue`].
    ///
    /// Each answer must itself be an engine of `v`, so a key or ring of another exchange claims
    /// nothing.
    ///
    /// `#[cold]` and out of line: the engine lookup above finds a default account on every node the
    /// latency gate measures, so none of this is on the fold's path, and a multi-account node asked
    /// the same questions before it got here (the repeat is the price of one body for both).
    #[cold]
    #[inline(never)]
    fn route_venue_without_default(
        &self,
        event: &Event,
        v: &str,
        ring: Option<vike_marketdata::Symbol>,
    ) -> Option<usize> {
        let of_venue = |i: usize| self.engine(i).venue == v;
        if let Some(key) = event_route_key(event) {
            return self
                .engine_index_for_route_key(RouteKey::declared(key))
                .filter(|&i| of_venue(i));
        }
        if let Some(order_client_id) = event.order_client_id()
            && let Some(i) = self.engine_of_order_client_id(order_client_id)
            && of_venue(i)
        {
            return Some(i);
        }
        if matches!(event, Event::Fill(_))
            && let Some(key) = ring
            && let Some(i) = self.engine_index_for_route_key(RouteKey::declared(key.as_str()))
            && of_venue(i)
        {
            return Some(i);
        }
        // The ring names an account of THIS venue that no engine here runs. The key's venue and not
        // `label_of_route_key`: a malformed label would then read as "names nothing", which is the
        // guess this rung exists to refuse.
        if matches!(event, Event::Fill(_))
            && let Some(key) = ring
            && vike_model::accounts::account_keys::venue_of_route_key(key.as_str()) == v
        {
            return None;
        }
        let mut only = None;
        for i in 0..=self.extra_engines.len() {
            if of_venue(i) && !self.is_retired(i) {
                if only.is_some() {
                    return None;
                }
                only = Some(i);
            }
        }
        only
    }

    pub(crate) fn drain_market(&mut self) {
        let (bar_close_ticks, ticks, tops, forming) = {
            let mut st = self.market.state.lock().unwrap();
            st.marker_in_flight = false;
            (
                st.bar_close_slots.drain(..).map(|(_, t)| t).collect::<Vec<_>>(),
                st.slots.drain(..).map(|(_, t)| t).collect::<Vec<_>>(),
                st.top_slots.drain(..).collect::<Vec<_>>(),
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
            let engine = match self.engine_index_for_route_key(RouteKey::sole_account_of(&t.venue))
            {
                Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                _ => &mut self.engine,
            };
            engine.account.set_mark_from(&t.venue, &t.symbol, t.price, MarkSource::BarClose, now);
            engine.price_board.set_bar_close(&t.venue, &t.symbol, t.price, t.timestamp);
            self.mirror_venue_price(
                &t.venue,
                &t.symbol,
                t.price,
                MarkSource::BarClose,
                now,
                Some(t.timestamp),
            );
            self.dirty = true;
        }
        for t in ticks {
            {
                let engine =
                    match self.engine_index_for_route_key(RouteKey::sole_account_of(&t.venue)) {
                        Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                        _ => &mut self.engine,
                    };
                engine.account.set_mark_from(
                    &t.venue,
                    &t.symbol,
                    t.price,
                    MarkSource::VenueMark,
                    now,
                );
                engine.price_board.set_mark(&t.venue, &t.symbol, t.price, t.timestamp);
            }
            self.mirror_venue_price(
                &t.venue,
                &t.symbol,
                t.price,
                MarkSource::VenueMark,
                now,
                Some(t.timestamp),
            );
            self.dirty = true;
            // Cross-symbol underlying-mark routing ("Option B"): a drained venue mark is ALSO the
            // underlying-spot feed for any maker mounted on a DIFFERENT (e.g. PM token) symbol that
            // declares THIS (venue, symbol) as its `underlying_symbol`. Early-returns inside when no
            // mount does, so a run with no underlying-anchored maker never leaves the mark write above.
            self.drive_strategy_mark(&t.venue, &t.symbol, t.price, t.timestamp);
            // Mark-lane conditional triggering (w2 `trigger_by`): a `Some(Mark)` arm evaluates
            // ONLY here, off the mark tick — the one lane that reproduces a mark-triggering
            // venue's (hyperliquid) SL/TP timing. Guarded by `has_mark_arms` inside, so a book
            // with no Mark arms costs one integer compare on this hot mark drain.
            self.fire_conditionals_at_mark(&t.venue, &t.symbol, t.price, t.price, t.timestamp);
        }
        // The node's price tap's throttled TOP of book for a symbol whose engine hears no tick of it (a lane twin's
        // or a labelled account's own symbol, decision 0208). Owner, 2026-11-15: a position is valued at the price it
        // can be closed at, so the board takes it as a side-aware QUOTE (a long at the bid, a short at the ask) on
        // every engine of the venue, never in the mark slot (where the mid outranked even a default engine's own
        // bid and ask on a shared symbol), and a stop armed on the mark lane triggers on its side (a sell on the
        // bid, a buy on the ask). The account slot, the underlying lane and the published `marks` keep the mid, as
        // the mark it was before: none of them values a position.
        for ((venue, symbol), (bid, ask, ts)) in tops {
            let mid = (bid + ask) / 2.0;
            {
                let engine =
                    match self.engine_index_for_route_key(RouteKey::sole_account_of(&venue)) {
                        Some(i) if i > 0 => &mut self.extra_engines[i - 1].1,
                        _ => &mut self.engine,
                    };
                engine.account.set_mark_from(&venue, &symbol, mid, MarkSource::VenueMark, now);
                engine.price_board.set_quote(&venue, &symbol, bid, ask, ts);
            }
            self.mirror_venue_price(&venue, &symbol, mid, MarkSource::VenueMark, now, None);
            self.mirror_venue_quote(&venue, &symbol, bid, ask, ts);
            self.dirty = true;
            self.drive_strategy_mark(&venue, &symbol, mid, ts);
            self.fire_conditionals_at_mark(&venue, &symbol, bid, ask, ts);
        }
        for (key, bar) in forming {
            let series = self.bars.entry(key).or_default();
            // ignore a forming update older than the last close (reconnect race)
            if series.closed.last().is_none_or(|last| bar.timestamp > last.timestamp) {
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
    /// live tearsheet reads them back via `Journal::read_all`) AND replay-reproducible. This
    /// runs on the cold bar-close / event-arm path — never the p99 event fold — and every hook is a
    /// single `Option::is_some` no-op when journaling is off, so the default path stays byte-identical.
    pub(crate) fn pump_client(&mut self) {
        // One wall-clock stamp for the whole drain: these synthesized events are a CONSEQUENCE of the
        // message dispatch that reached `pump_client`, so they share its `now_ms` (the same
        // "one now per message" discipline `dispatch` applies), not a fresh clock read per event.
        let now_ms = self.engine.now_ms;
        while let Some(event) = self.engine.client.poll_events() {
            let event = self.journal_pumped_event(now_ms, event);
            // Live-runtime OTO/OCO: client-SYNTHESIZED fills (the paper exchange, and any in-process
            // client that emits via `poll_events`) surface HERE, not on the `Ingest::Event` lane — so
            // the contingency drive must run here too or a bracket never resolves on the paper/
            // backtest path. Classify BEFORE the bus consumes `event`; drive AFTER it folds. The drive's
            // own submits/cancels queue further client events that THIS loop then drains (the drive
            // itself never pumps — see its doc). Gated on a non-empty book (byte-identical otherwise).
            //
            // ⚠ DELIBERATELY NOT GATED on the fold verdict, unlike the `Ingest::Event` venue lane.
            // `poll_events` is the IN-PROCESS client seam (the paper exchange and test clients);
            // real venue clients return `None` here, so nothing an attacker controls arrives on this
            // path and the hardening buys nothing. It would cost something, though:
            // `crates/vike-simulator/tests/r7_gate.rs` pins backtest == paper BIT-FOR-BIT, and this
            // is the paper side of that equality.
            let drive = self.contingency_terminal(&event);
            // Its report is written right after the fold, before the cadence `Snapshot` below can follow its cause.
            let report = self.kept_for_report(&event);
            let fold = self.bus.publish(event, &mut self.engine);
            self.journal_pumped_report(0, report, fold, now_ms);
            if let Some((order_client_id, d)) = drive {
                self.drive_contingency(&order_client_id, d, now_ms);
            }
            self.maybe_cadence_snap(now_ms);
        }
        for i in 0..self.extra_engines.len() {
            while let Some(event) = self.extra_engines[i].1.client.poll_events() {
                let event = self.journal_pumped_event(now_ms, event);
                let drive = self.contingency_terminal(&event);
                let report = self.kept_for_report(&event);
                let fold = self.bus.publish(event, &mut self.extra_engines[i].1);
                self.journal_pumped_report(i + 1, report, fold, now_ms);
                if let Some((order_client_id, d)) = drive {
                    self.drive_contingency(&order_client_id, d, now_ms);
                }
                self.maybe_cadence_snap(now_ms);
            }
        }
    }

    /// The report of one pumped event engine `index` folded with `fold`, when [`Self::kept_for_report`] kept one.
    fn journal_pumped_report(
        &mut self,
        index: usize,
        report: Option<Box<Event>>,
        fold: EventBusFold,
        now_ms: i64,
    ) {
        if let Some(event) = report {
            let cause = super::execution_report::ExecutionReportCause::Event {
                event: &event,
                fold,
                request: None,
            };
            self.journal_execution_report(Some(index), cause, None, now_ms);
        }
    }

    /// **A quote is the clock of the paper books it reaches** (decision 0285's amendment of 2026-11-16): every
    /// running engine of `venue` that states the `paper` tier and holds `symbol` hands the quote to its book
    /// (`ExecutionClient::on_quote`), which fills a resting order the moment the quote reaches it, at the side
    /// that reached it; the fills fold, then a strategy's stop resting on a paper engine is judged on the same
    /// quote (`fire_paper_conditionals`), so the market order it releases fills at it too. The `Ingest::Quote`
    /// arm calls this only while [`Self::any_paper_engine`]; out of line and cold, so the fold of a node with no
    /// paper engine pays one bool per quote and nothing else.
    #[cold]
    #[inline(never)]
    pub(crate) fn quote_paper_books(
        &mut self,
        venue: &str,
        symbol: &str,
        bid: f64,
        ask: f64,
        ts: i64,
    ) {
        let mut quoted = false;
        for engine_index in 0..=self.extra_engines.len() {
            let engine = self.engine(engine_index);
            if engine.venue != venue
                || engine.stated_tier() != Some(vike_model::AccountTier::Paper)
                || !engine.accepts_symbol(symbol)
                || self.is_retired(engine_index)
            {
                continue;
            }
            self.engine_mut(engine_index).client.on_quote(symbol, bid, ask, ts);
            quoted = true;
        }
        if quoted {
            self.pump_client();
            self.dispatch_applied_fills();
            self.fire_paper_conditionals(venue, symbol, bid, ask, ts);
        }
    }

    /// Whether mount `mount_slot` is a `fill = "close"` mount ([`CoreConfig::mount_fills_at_close`]) trading on
    /// a stated `paper` engine: its bar orders then fill at that bar's close ([`Self::fill_paper_at_close`]).
    /// One bool on a node with no paper engine.
    pub(crate) fn closes_on_paper(&self, mount_slot: usize) -> bool {
        self.any_paper_engine
            && self.engine(self.mount_engine(mount_slot)).stated_tier()
                == Some(vike_model::AccountTier::Paper)
            && self.config.mount_fills_at_close.contains(&self.mount_ids[mount_slot])
    }

    /// Open the drain of a `fill = "close"` mount's bar orders on a paper engine ([`Self::closes_on_paper`]), or do
    /// nothing: `lower_strategy_intent` files the ids it mints (`closing_ids`), and its engine's book is held
    /// (`ExecutionClient::hold_for_close`), so an order it takes waits for its bar's close whatever quote came after
    /// that bar (a sandboxed strategy's reply lands after some). [`Self::fill_paper_at_close`] closes it.
    pub(crate) fn hold_paper_close(&mut self, mount_slot: usize) {
        if self.closes_on_paper(mount_slot) {
            self.closing_ids = Some((mount_slot, Vec::new()));
            let engine_index = self.mount_engine(mount_slot);
            self.engine_mut(engine_index).client.hold_for_close(true);
        }
    }

    /// Close what [`Self::hold_paper_close`] opened for `mount_slot`: the book is released and the bar orders
    /// it filed fill at the close of their bar, stamped with the bar's end (the book reads the bar's window
    /// off its own last bar; the core hands it the width, `ExecutionClient::fill_at_close`), as the backtest's
    /// `InBarRules::fill_at_close` fills them, and the fills fold. Cold: once per such step.
    pub(crate) fn fill_paper_at_close(&mut self, mount_slot: usize) {
        if self.closing_ids.as_ref().is_none_or(|(held, _)| *held != mount_slot) {
            return;
        }
        let Some((_, ids)) = self.closing_ids.take() else { return };
        let engine_index = self.mount_engine(mount_slot);
        self.engine_mut(engine_index).client.hold_for_close(false);
        let Some(mount) = self.mounts[mount_slot].as_ref().filter(|_| !ids.is_empty()) else {
            return;
        };
        let width = vike_marketdata::interval::interval_ms(&mount.interval).unwrap_or(0);
        self.engine_mut(engine_index).client.fill_at_close(&ids, width);
        self.pump_client();
        self.dispatch_applied_fills();
    }

    /// Classify a folded event for the live-runtime OTO/OCO/OUO drive: a FULL fill (arm held children,
    /// cancel OCO siblings, reduce OUO ones), a PARTIAL fill of an `Ouo` leg (reduce its siblings by
    /// that fill: ruling 3 of wave 4, the per-fill quantity; a partial fill of any other leg leaves the
    /// group resting, as it always did), a terminal-WITHOUT-fill (cascade-drop held children / clean
    /// the dead leg), `None` for everything else. Gated on a
    /// non-empty book so the no-bracket path is a single `is_empty` read. Shared by `pump_client`
    /// (client-synthesized events), the `Ingest::Event` arm (real venue events), AND the synchronous
    /// submit outboxes (`publish_and_drive_outbox`) — the ONE classifier every fold site consults so
    /// they cannot drift. `OrderDenied` (a RiskGate veto / Halted, emitted SYNCHRONOUSLY by
    /// `submit_order`) is a terminal-without-fill here: a denied bracket entry must cascade-drop its
    /// held children, never orphan them.
    pub(crate) fn contingency_terminal(&self, event: &Event) -> Option<(String, ContingencyDrive)> {
        if self.contingency.is_empty() {
            return None;
        }
        let fill = |f: &vike_model::events::FillEvent, full| ContingencyDrive::Fill {
            qty: f.last_qty,
            price: f.last_price,
            full,
        };
        match event {
            Event::OrderFilled(e) => Some((e.order_client_id.clone(), fill(&e.fill, true))),
            Event::OrderPartiallyFilled(e)
                if self.contingency.kind_of(&e.order_client_id)
                    == Some(vike_model::ContingencyType::Ouo) =>
            {
                Some((e.order_client_id.clone(), fill(&e.fill, false)))
            }
            Event::OrderCanceled(e) => Some((e.order_client_id.clone(), ContingencyDrive::Ended)),
            Event::OrderRejected(e) => Some((e.order_client_id.clone(), ContingencyDrive::Ended)),
            Event::OrderDenied(e) => Some((e.order_client_id.clone(), ContingencyDrive::Ended)),
            Event::OrderExpired(e) => Some((e.order_client_id.clone(), ContingencyDrive::Ended)),
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
            | Event::OrderModifyRejected(_)
            | Event::FeeAdjustment(_) => None,
        }
    }

    /// Publish a SYNCHRONOUS submit outbox to engine `index` and drive the contingency book for any
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
    /// deliberately never REGISTERED, so that event folds `EventBusFold::Dropped` BY DESIGN (it lands on
    /// `on_event`'s unknown-order-client-id branch and even moves `events_dropped_order_unknown`). Gating here would
    /// therefore suppress exactly the cascade-drop this function exists to perform, orphaning a
    /// denied bracket entry's held children forever. These events are engine-synthesized and
    /// unreachable by a venue — `Event` is never deserialized from a venue payload — so the hostile
    /// -venue argument that gates the other lane does not apply.
    ///
    /// `submitted` are the requests the submit handed the engine: each one the engine REGISTERED is reported at
    /// its registration, before the outbox folds, and a deny in the outbox is reported from its request, which
    /// the engine never registered (`execution_report.rs`).
    pub(crate) fn publish_and_drive_outbox(
        &mut self,
        index: usize,
        mut outbox: EventBusOutbox,
        now: i64,
        submitted: &[OrderRequest],
    ) {
        if self.execution_reports {
            for req in submitted {
                if self.engine(index).registry.contains_key(&req.order_client_id) {
                    self.report_registered(index, &req.order_client_id);
                }
            }
        }
        while let Some(event) = outbox.0.pop_front() {
            let drive = self.contingency_terminal(&event);
            // a deny's request names its venue and symbol for `note_denied`, journaling or not
            let request = if self.execution_reports || matches!(event, Event::OrderDenied(_)) {
                event
                    .order_client_id()
                    .and_then(|id| submitted.iter().find(|r| r.order_client_id == id))
            } else {
                None
            };
            self.publish_synthesized(index, event, request);
            if let Some((order_client_id, d)) = drive {
                self.drive_contingency(&order_client_id, d, now);
            }
        }
    }
}

/// What a folded event asks of the contingency book ([`CoreThread::contingency_terminal`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ContingencyDrive {
    /// A fill of `qty` at `px` (the event's own `last_qty`/`last_price`, never the order's cumulative
    /// size); `full` = the order's terminal `OrderFilled`.
    Fill { qty: f64, price: f64, full: bool },
    /// The order ended without filling (cancelled, rejected, denied, expired).
    Ended,
}
