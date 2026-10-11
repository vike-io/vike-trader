//! `CoreThread::dispatch` — the single-writer fold over one `Ingest` message.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Consumes the message — the dominant Event path moves straight into the bus with no
    /// hot-path clone (the same rationale that keeps `Ingest::Event` unboxed).
    pub(crate) fn dispatch(&mut self, msg: Ingest) {
        // one wall-clock stamp per message: submit_order's now_ms AND every persistence
        // updated_ts in this dispatch (Python called its now_ms lambda at each use site;
        // one read per message is the same clock, minus mid-dispatch drift)
        self.engine.now_ms = self.config.clock.now_ms();
        let now_all = self.engine.now_ms;
        for (_, e) in self.extra_engines.iter_mut() {
            e.now_ms = now_all;
        }
        // DEAD-MAN'S SWITCH freshness observe (trading-hardening): the ONE cheap per-message store —
        // record `now_all` as the freshest data/event ingest ts, so the boundary sweep can tell how
        // long the feed has been silent. Gated on the switch being armed (`Option::is_some`-cheap
        // via `if let`), so when disabled (default) this is a single null-check branch, no store, no
        // log, no allocation — the `p99 < 10µs` fold stays byte-identical. Only genuine liveness
        // messages advance it: venue events + market data (quote/trade/book/mark/closed-bar), NEVER
        // control (Command), the periodic waker (Watchdog), a health-status change (StreamStatus), or
        // a historical bar SEED (BarSeed).
        if let Some(dm) = self.deadman.as_mut()
            && matches!(
                msg,
                Ingest::Event(_)
                    | Ingest::Market
                    | Ingest::BarClose(_)
                    | Ingest::Quote(_)
                    | Ingest::Trade(_)
                    | Ingest::Book(_)
            )
        {
            dm.observe(now_all);
        }
        // WRITE-AHEAD JOURNAL (spec §A): journal the exec-lane message HERE — after the wall-clock
        // stamp, BEFORE the `match msg` fold below — so a crash mid-fold still finds the message on
        // disk and replays it. `append_cmd` serializes `&msg` by BORROW (no clone on the p99<10µs
        // hot path; the `match msg` below still owns and consumes it); the returned seq is unused.
        // Only exec-lane verbs are journaled — market/bar/tick lanes are not command state.
        // `Command::Shutdown` never reaches here (intercepted in `handle`); it gets a Snap, not a
        // Cmd. The whole hook is skipped (one `Option::is_some`) when journaling is off.
        // The cadence SNAPSHOT decision is deferred to AFTER the fold (tail of this fn) so a snap
        // reflects THIS message's effect; only the append + counter bump (the durability write-
        // ahead) run here, before the `match msg` fold. `journaled` is captured now (it gates the
        // after-fold snap to the same exec-lane messages) because `match msg` below consumes `msg`.
        let journaled = self.journal.is_some()
            && match msg {
                // The hot Event arm stays FIRST (one discriminant test, exactly as before).
                Ingest::Event(_) => true,
                // Runtime mount/unmount (split-plane B5) is deliberately NOT journaled: it is
                // session TOPOLOGY, not order state. The mount-less replay core has no
                // `strategy_factory` and could never re-resolve a strategy, so a journaled mount
                // would replay as a refusal note — a divergence, not a restore. Restart survival
                // is instead DAEMON-LEVEL STATE (documented on `Command::MountStrategy` too): the
                // mount arm records the spec in the `crate::mount_topology` sidecar (gated on
                // `state_dir`, atomic rewrite, unmount removes it) and the composition root
                // replays that file at startup through this same command path — so the journal's
                // replay determinism fence stays over exactly what re-folds, while the journal's
                // own `StrategySubmit` records still carry the mount id for attribution. The
                // unmount's cancels are replay-neutral exactly like `latch_mount`'s: the venue's
                // authoritative `OrderCanceled` is journaled as its own `Ingest::Event` and
                // replays independently.
                Ingest::Command(Command::MountStrategy(_) | Command::UnmountStrategy { .. }) => {
                    false
                }
                Ingest::Command(_) => true,
                // Pure waker: journaled only when a state-mutating wall-clock sweep can ride it
                // (see `journal_waker_records`). One bool read; nothing else on this path moved.
                Ingest::Watchdog => self.journal_waker_records,
                Ingest::Market
                | Ingest::BarSeed(_)
                | Ingest::BarClose(_)
                | Ingest::Quote(_)
                | Ingest::Trade(_)
                | Ingest::Book(_)
                | Ingest::StreamStatus(_)
                | Ingest::Flow(_) => false,
            };
        if journaled {
            self.journal
                .as_mut()
                .expect("journaled implies journal.is_some()")
                .append_cmd(now_all, &msg)
                .expect("journal append");
            self.journaled_since_snap += 1;
        }
        match msg {
            Ingest::Event(ev) => {
                if let (Some(tid), Event::Fill(f)) = (self.config.panic_on_trade_id.as_deref(), &ev)
                    && f.trade_id == tid
                {
                    panic!("injected fault: trade_id {tid}");
                }
                // Live-runtime OTO/OCO: capture this REAL-venue event's effect on the contingency
                // book BEFORE `ev` is consumed by the bus (the shared `contingency_terminal`
                // classifier `pump_client` also uses), then drive it AFTER the engine has folded the
                // event. A terminal `OrderFilled` ARMS this leg's held OTO children + cancels its OCO
                // siblings (matching the paper oracle's `apply_contingency`); a terminal-WITHOUT-fill
                // (`Canceled`/`Rejected`/`Expired`) cascade-drops the children that can now never arm
                // (the oracle's `expire_children_of`). A partial fill leaves the group resting.
                let contingency_ev = self.contingency_terminal(&ev);
                // ⚠ THE FOLD'S VERDICT GATES THE DRIVE (hostile-venue hardening). `contingency_ev`
                // is classified from the EVENT ALONE, so before this the bracket/OCO/OTO machinery
                // reacted to events the engine had REFUSED — "invalid transitions are dropped" did
                // not extend to it. A venue frame naming an unknown coid is dropped and counted into
                // `dropped_unknown_coid`, yet a fabricated `OrderFilled` on one OCO leg still
                // cancelled its SIBLING (name the take-profit ⇒ the stop-loss goes and the position
                // is left NAKED), and a fabricated fill on a bracket entry still armed its held OTO
                // children into real venue orders. Legitimate events are untouched: they fold
                // `Applied`, so an accepted terminal drives exactly what it drove before.
                //
                // ONLY THIS LANE IS GATED. `pump_client` and `publish_and_drive_outbox` carry
                // events synthesized IN-PROCESS, not venue frames — and gating the latter would be
                // an outright REGRESSION: `gate_and_register` publishes `OrderDenied` for an order
                // it deliberately never registered, so that event folds `Dropped` BY DESIGN, and
                // suppressing its drive would orphan a denied bracket entry's held children forever
                // (`contingency_terminal`'s own doc: "a denied bracket entry must cascade-drop its
                // held children, never orphan them"). A venue cannot reach that path: `Event` is
                // never deserialized from a venue payload — every venue frame goes through a
                // per-venue `event_mapper`, and no bridge constructs `OrderDenied`.
                let fold = match self.route_event(&ev) {
                    Some(0) => self.bus.publish(ev, &mut self.engine),
                    Some(i) => self.bus.publish(ev, &mut self.extra_engines[i - 1].1),
                    // §5.4 — UNATTRIBUTED. A foreign fill on a venue this process runs several
                    // accounts of: no stamped key, no order of ours, and two engines claim its
                    // symbol. It is REPORTED rather than folded into the venue's default account's
                    // ledger, which is the sentence `engine_idx_for_venue_symbol`'s own doc has
                    // carried since the ambiguity was made to answer `None` — *"that is reconcile's
                    // territory, not this lane's"*. Reconcile sees the venue's own position report
                    // per account and re-converges whatever this leaves; folding it here would put
                    // a stranger's size and price into a book that never traded them, and no later
                    // pass can tell that apart from a real position.
                    //
                    // `Fold::Dropped` is the honest answer for the contingency drive below: nothing
                    // moved, so nothing this fill might have named may arm or cancel.
                    None => {
                        let (venue, symbol) = match &ev {
                            Event::Fill(f) => (f.venue.to_string(), f.symbol.to_string()),
                            // every other event: not this tap's business (audited 2026-10, I-21)
                            _ => (String::new(), String::new()),
                        };
                        tracing::warn!(
                            target: "vike_core::core",
                            venue = %venue,
                            symbol = %symbol,
                            "UNATTRIBUTED venue fill: it names no order this process placed and \
                             several accounts of this venue claim its symbol, so NOTHING was \
                             folded. Each account's own reconcile pass re-converges its book \
                             against the venue's per-account position report."
                        );
                        self.note(format!(
                            "UNATTRIBUTED fill on {venue} {symbol}: names no order of ours and \
                             several accounts claim the symbol (nothing folded)"
                        ));
                        Fold::Dropped
                    }
                };
                self.pump_client();
                match contingency_ev {
                    // The `fold` guards sit on the ARMS rather than wrapping the whole `match`, so
                    // the overwhelmingly common no-bracket path — `contingency_terminal`
                    // early-returns `None` on an empty book — never evaluates the comparison.
                    Some((coid, true)) if fold == Fold::Applied => {
                        self.drive_contingency_on_fill(&coid, now_all);
                        // fold the release submits / sibling cancels the drive just queued
                        self.pump_client();
                    }
                    Some((coid, false)) if fold == Fold::Applied => {
                        self.drive_contingency_on_terminal(&coid)
                    }
                    // A terminal-shaped event the engine REFUSED. Per-order and fault-adjacent
                    // (never per-tick), and it is the one signature a fabricated venue frame leaves.
                    // A terminal-shaped event the engine REFUSED. Per-order and fault-adjacent
                    // (never per-tick), and it is the one signature a fabricated venue frame leaves.
                    Some((coid, _)) => tracing::warn!(
                        target: "vike_exec::oms",
                        coid = %coid,
                        "contingency drive SUPPRESSED: the engine dropped this terminal event \
                         (unknown coid / illegal transition / replay), so it arms and cancels \
                         nothing"
                    ),
                    None => {}
                }
                self.dirty = true;
            }
            Ingest::Command(cmd) => {
                match cmd {
                    Command::Order(intent) => {
                        self.apply_intent(intent, self.engine.now_ms);
                    }
                    // Live-parameter plane (audit co8): route a typed params update to ONE mount (the
                    // one `mount_id` names, else the sole mount on (venue, symbol, interval); an
                    // ambiguous update is refused with a note) and run its `on_params_updated`
                    // hook — a running strategy is re-tuned WITHOUT unmounting (which would lose
                    // queue position). A RARE control command (a GUI/operator re-tune), NOT a market
                    // message, so it is off the p99<10µs event fold; it touches NO OMS state (only
                    // the mounted strategy's tunables + any orders that hook buffers, drained the one
                    // live path).
                    Command::UpdateParams(u) => self.drive_strategy_params(&u),
                    Command::SetTradingState(st) => {
                        self.engine.trading_state = st;
                        for (_, e) in self.extra_engines.iter_mut() {
                            e.trading_state = st;
                        }
                    }
                    // LIVE per-symbol leverage change — write the target venue's engine's
                    // `im_by_symbol` (rare operator/GUI verb; off the hot fold).
                    //
                    // ⚠ Deliberately `venue`, not `route_key`, and deliberately a FAN-OUT rather
                    // than `engine_idx_for_route_key`: this is the one command in this file that
                    // already addresses ENGINES BY VENUE and applies to every match. Identical
                    // today (one engine per venue), and left alone because the operator's verb is
                    // genuinely venue-shaped — `MarginUpdate` has no field that could name one
                    // account of two. Wiring a second account has to decide what "set binance
                    // leverage" means before this site can move; guessing now would silently pick
                    // one of the two accounts, which is worse than the fan-out.
                    Command::SetMargin(u) => {
                        let MarginUpdate { venue, symbol, im_requirement } = *u;
                        if self.engine.venue == venue {
                            self.engine
                                .gate
                                .limits
                                .im_by_symbol
                                .insert(symbol.clone(), im_requirement);
                        }
                        for (_, e) in self.extra_engines.iter_mut() {
                            if e.venue == venue {
                                e.gate.limits.im_by_symbol.insert(symbol.clone(), im_requirement);
                            }
                        }
                    }
                    // ReconcileSnapshot carries no venue — primary only; extra venues
                    // reconcile BEFORE spawn (their engines are built by the caller).
                    // Audit exec#2 DRIFT DETECTION: before venue truth overwrites the
                    // locally-folded Account, diff the two and surface any divergence
                    // (position size / authoritative balance / open-order set) through the
                    // GUI-visible recent-events ring + a tracing::warn — the same soft-signal
                    // channel the margin-call watchdog uses. Venue truth STILL wins the seed
                    // (apply_snapshot below is unchanged); this only ADDS the alert the silent
                    // overwrite was swallowing. Fires on every ReconcileSnapshot the core is
                    // handed — the startup/session reconcile (venue smoke tests drive exactly this
                    // path) AND, now, the opt-in `CoreHandle::spawn_periodic_reconcile` driver
                    // (default off) that re-issues ApplySnapshot on an interval so drift-checking is
                    // CONTINUOUS, not startup-only — both flow through THIS one handler with zero
                    // further change. Diffing at this ONE choke point stays the in-scope cadence: NO
                    // new wire verb or Event variant. A RECONNECT-triggered re-snapshot from the
                    // venue adapter stays the DEFERRED fork (the bridge-core A3 resync + user-data
                    // pump hold an EventSender, not a command lane, so they replay missed lifecycle
                    // Events only, never a re-snapshot). This is a
                    // rare command, NOT the per-event hot fold, so a tracing::warn here is within
                    // the per-order-boundary / fault-transition logging budget.
                    Command::ApplySnapshot(snap) => {
                        for w in self.engine.diff_snapshot(&snap) {
                            tracing::warn!(
                                target: "vike_core::reconcile",
                                venue = %self.engine.venue,
                                "{w}"
                            );
                            self.note(w);
                        }
                        self.engine.apply_snapshot(&snap);
                    }
                    Command::ReconcileReports(reports) => {
                        self.reconcile_reports(*reports);
                    }
                    Command::ConfirmRecon(id) => {
                        self.confirm_recon(id);
                    }
                    // RUNTIME strategy mount/unmount (split-plane B5) — occasional operator verbs,
                    // off the p99 event fold (the `UpdateParams` argument verbatim). Every failure
                    // is a REFUSAL note in recent-events, never a panic: unlike spawn-time
                    // assembly, a running core must keep trading through a bad mount request.
                    Command::MountStrategy(spec) => {
                        self.mount_strategy_runtime(*spec);
                    }
                    Command::UnmountStrategy { controller_id } => {
                        self.unmount_strategy_runtime(&controller_id);
                    }
                    Command::Shutdown => unreachable!("handled in handle()"),
                }
                self.dirty = true;
            }
            Ingest::Market => self.drain_market(),
            Ingest::BarSeed(seed) => {
                let BarSeed { venue, symbol, interval, bars } = *seed;
                let series = self.bars.entry((venue, symbol, interval)).or_default();
                series.closed = Arc::new(bars);
                series.forming = None; // the live stream refreshes it
                self.dirty = true;
            }
            Ingest::BarClose(update) => {
                let BarUpdate { venue, symbol, interval, bar } = *update;
                let key = (venue, symbol, interval);
                // STAMP THE SERIES SYMBOL ONCE, here, so every downstream consumer sees the same
                // bar: the paper client (which already got a stamped COPY below), the closed-bar
                // cache that `Broker::bars` hands back, and the strategy's `on_bar`.
                //
                // Live bars arrive with `symbol: None` — `vike_bridge_core::klines::kline_to_bar`
                // builds them from a venue kline that carries no vike symbol — and the runtime is
                // the first place that KNOWS it (`key.1`). Withholding it made a mount's `on_bar`
                // structurally unable to tell one series from another, which is why a two-leg
                // strategy could not work live even after its ORDERS were routed correctly.
                //
                // Safe for every existing mount BECAUSE of the opt-in routing lane: a mount that
                // declares no extra symbols ignores the `symbol` argument of the `Broker` verbs
                // entirely (`resolve_intent_symbol`), so a strategy that now passes a REAL symbol
                // where it used to pass `""` routes exactly where it did before. Before that lane
                // existed this stamp would have sent a real symbol to venues, which is why it was
                // withheld.
                //
                // A bar that ALREADY carries a symbol is left alone (a backtest/seeded bar names
                // its own instrument, and the replayed value is the source of truth).
                let mut bar = bar;
                if bar.symbol.is_none() {
                    bar.symbol = Some(key.1.clone());
                }
                let last_ts = self.bars.get(&key).and_then(|s| s.closed.last().map(|b| b.ts));
                let append = match last_ts {
                    // reconnect overlap: the same window re-closes — replace idempotently
                    Some(t) if bar.ts == t => {
                        if let Some(series) = self.bars.get_mut(&key)
                            && let Some(last) = Arc::make_mut(&mut series.closed).last_mut()
                        {
                            *last = bar.clone();
                        }
                        false
                    }
                    Some(t) if bar.ts < t => false, // stale replay — drop
                    _ => true,
                };
                if append {
                    // Paper fills for the engine's own (venue, symbol) fold BEFORE the bar
                    // joins the cache: their `on_fill` deliveries see history through bar
                    // i-1 at the standing mark — the backtest fill-phase view (fills happen
                    // at bar i's OPEN; bar i's close is not knowable at that moment).
                    // ⚠ EVERY engine of this exchange, not just its default account. The bar is
                    // the paper exchange's fill clock (`ExecutionClient::on_bar`; a real venue
                    // adapter's default impl ignores it), and it was delivered only to the engine
                    // the venue string resolves — so a SECOND account whose credentials were absent
                    // at mount and fell back to `vike_paper::PaperExecutionClient` held a book
                    // nothing ever filled: its strategy's orders rested forever, never filling and
                    // never terminalizing, with no error anywhere.
                    //
                    // `engines_of_venue` is `[default]` on every single-account core, so the
                    // sequence below — including `pump_client`, which already drains every
                    // engine — is unchanged there.
                    for eidx in self.engines_of_venue(&key.0) {
                        if self.eng(eidx).accepts_symbol(&key.1) {
                            // The series symbol is stamped once at the top of this arm now, so a
                            // multi-book paper client can route and every consumer agrees on the
                            // same bar; single-book clients ignore `bar.symbol` (r7 law intact).
                            self.eng_mut(eidx).client.on_bar(&bar);
                            self.pump_client();
                            self.dispatch_applied_fills();
                        }
                    }
                    let series = self.bars.entry(key.clone()).or_default();
                    Arc::make_mut(&mut series.closed).push(bar.clone());
                }
                // a close supersedes any forming state of that (or an older) window
                if let Some(series) = self.bars.get_mut(&key)
                    && series.forming.as_ref().is_some_and(|f| f.ts <= bar.ts)
                {
                    series.forming = None;
                }
                self.dirty = true;
                if append {
                    self.drive_strategy(&key, &bar);
                }
            }
            Ingest::Quote(qu) => {
                let QuoteUpdate { venue, symbol, quote } = *qu;
                let px = quote.mid();
                let ts = quote.ts;
                if let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                    && self.eng(eidx).accepts_symbol(&symbol)
                {
                    self.eng_mut(eidx)
                        .price_board
                        .set_quote(&venue, &symbol, quote.bid, quote.ask, ts);
                }
                self.drive_strategy_tick(&venue, &symbol, px, ts, |s, ctx| {
                    s.on_quote_tick(ctx, &quote)
                });
                // The cross-venue REFERENCE lane (xEMM): the same touch, delivered to any mount on
                // a DIFFERENT venue that declared this `(venue, symbol)` as a leg. Disjoint from
                // the tick lane above by construction (`Audience::Tick` and `Audience::Reference`
                // test the venue `==` and `!=`), so no mount is dispatched twice. `any_mount_ref`
                // is false for every runtime without a cross-venue mount, making this one bool
                // load on the fold.
                if self.any_mount_ref {
                    self.drive_strategy_reference_quote(
                        &venue,
                        &symbol,
                        quote.bid,
                        quote.ask,
                        quote.bid_size,
                        quote.ask_size,
                        ts,
                    );
                }
            }
            Ingest::Trade(tu) => {
                let TradeUpdate { venue, symbol, trade } = *tu;
                let px = trade.price;
                let ts = trade.ts;
                if let Some(eidx) = self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                    && self.eng(eidx).accepts_symbol(&symbol)
                {
                    self.eng_mut(eidx).price_board.set_last_trade(&venue, &symbol, px, ts);
                }
                self.drive_strategy_tick(&venue, &symbol, px, ts, |s, ctx| {
                    s.on_trade_tick(ctx, &trade)
                });
            }
            Ingest::Book(bu) => {
                // `book` is an `Arc<L2Book>` (perf audit finding #1): every read below and the
                // `&L2Book` handed to `on_order_book` reach through `Deref`, and the drop at the
                // end of this arm is a refcount DECREMENT — not the full two-`BTreeMap` teardown
                // a by-value book cost this thread, the one the `p99 < 10µs` gate measures.
                // The producing pump keeps folding into the same allocation via `Arc::make_mut`.
                let BookUpdate { venue, symbol, book } = *bu;
                // A two-sided top is needed to mark/price the step; a one-sided/empty book has
                // no mid, so the strategy sees the next (fuller) update (v1 simplification).
                if let Some(px) = book.mid() {
                    let now = self.engine.now_ms;
                    // The derived TOP, read once. Sizes are bound too (they used to be discarded)
                    // because the cross-venue reference lane below delivers a full `QuoteTick`: a
                    // maker sizing against the reference venue's displayed depth needs them, and a
                    // book's derived L1 must be indistinguishable from that venue's native L1.
                    let top = (book.best_bid(), book.best_ask());
                    if let (Some(BookLevel { price: bb, .. }), Some(BookLevel { price: ba, .. })) =
                        top
                        && let Some(eidx) =
                            self.engine_idx_for_route_key(RouteKey::sole_account_of(&venue))
                        && self.eng(eidx).accepts_symbol(&symbol)
                    {
                        self.eng_mut(eidx).price_board.set_quote(&venue, &symbol, bb, ba, now);
                    }
                    self.drive_strategy_tick(&venue, &symbol, px, now, |s, ctx| {
                        s.on_order_book(ctx, &book)
                    });
                    // The cross-venue REFERENCE lane (xEMM) — the book twin of the quote arm's
                    // call above. A foreign venue's L2 is delivered as its DERIVED L1, not as an
                    // `L2Book`: `L2Book` carries neither a venue nor a symbol
                    // (`vike_marketdata::orderbook`), so a strategy handed two venues' books could not
                    // attribute them, and stamping either onto that journaled serde payload would
                    // be a wire change. The touch is what a reference-priced maker consumes anyway.
                    if self.any_mount_ref
                        && let (
                            Some(vike_model::BookLevel { price: bb, qty: bq }),
                            Some(vike_model::BookLevel { price: ba, qty: aq }),
                        ) = top
                    {
                        self.drive_strategy_reference_quote(&venue, &symbol, bb, ba, bq, aq, now);
                    }
                }
            }
            // audit C3 + co6: the watchdog SWEEP now runs at the drain-loop boundary, driven by the
            // `DeadlineTimerWheel` (see `run` + `drive_due_timers`), NOT here. This message is kept
            // purely as the periodic WAKER (the OS thread injects it so an idle core reaches the
            // boundary on cadence) — and it is still JOURNALED (the `matches!` above), which is what
            // keeps a watchdog session's journal refused for replay (`replay.rs`), preserving the
            // existing "watchdog sweep is not a deterministic function of the journal" exclusion.
            Ingest::Watchdog => {}
            // Feed-health status CHANGE (net-hardening §B) — an OCCASIONAL control event (the
            // producer fires it only on a StreamStatus transition), so this is NOT the per-tick
            // hot path. Touches NO OMS fold: it only runs the mounted strategy's on_feed_status
            // hook + drains any orders that hook buffered (e.g. a "pull my quotes" mass_cancel)
            // through the one live path.
            Ingest::StreamStatus(su) => {
                let StreamStatusUpdate { venue, symbol, stream: _, status } = *su;
                // THE CONNECTION-STATE DEAD-MAN's ONE observe hook (M13). This arm — not the
                // tick/quote/trade/bar/book fold — is where the switch learns anything, which is
                // what keeps the p99<10µs core hop byte-identical: a status change is an
                // OCCASIONAL control event the producer fires only on a transition. Gated on
                // `is_some`, so a default core pays one `Option` branch on a message it already
                // pays a strategy dispatch for. A RECOVERY is the one thing worth a line: the
                // trip's own `warn!` lives in the sweep, and this is its `info!` twin.
                if let Some(ldm) = self.link_deadman.as_mut()
                    && ldm.observe(&venue, &symbol, status, self.engine.now_ms)
                        == LinkObservation::Recovered
                {
                    tracing::info!(
                        target: "vike_core::link_deadman",
                        %venue,
                        %symbol,
                        "LINK RECOVERED after a dead-man trip: the switch re-arms for the next \
                         outage. Halted and the HALT sentinel do NOT clear — an operator un-halts"
                    );
                }
                self.drive_strategy_feed_status(&venue, &symbol, status);
            }
            // Per-side FLOW-TOXICITY update (RTDS wallet-toxicity guard) — an OCCASIONAL control
            // event (the producer fires it at toxicity cadence, not per market message), so this is
            // NOT the per-tick hot path. Touches NO OMS fold: it only runs the mounted strategy's
            // on_flow hook + drains any orders that hook buffered through the one live path. The
            // exact twin of the StreamStatus arm above.
            Ingest::Flow(fu) => {
                let FlowUpdate { venue, symbol, flow } = *fu;
                self.drive_strategy_flow(&venue, &symbol, flow);
            }
        }
        // CADENCE SNAPSHOT (spec §A): taken AFTER the fold above so the snap reflects THIS message's
        // effect. Taken before (as the write-ahead append is), a cadence snap would omit the just-
        // journaled message while that message sits at a LOWER seq than the snap — and replay
        // (latest-Snap-wins, apply seq > snap) would silently drop it: a <=1-message loss on a
        // cadence-snap restore. `journaled` gates this to the same exec-lane messages the counter
        // tracks (never a market/bar/tick) and is false when journaling is off, so the disabled
        // path stays zero-overhead and byte-identical.
        if journaled
            && self.journaled_since_snap
                >= self
                    .config
                    .journal
                    .as_ref()
                    .expect("journal is Some only when config.journal is (assemble_core)")
                    .snapshot_every
        {
            self.write_snap(now_all);
        }
    }
}
