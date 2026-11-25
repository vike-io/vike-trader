//! `CoreThread::dispatch` — the single-writer fold over one `Ingest` message.

use super::*;

/// How many UNATTRIBUTED events of each KIND are logged in full before the rest are only counted.
const UNATTRIBUTED_FULL_LINES: u8 = 5;
/// Core-clock milliseconds between two summaries of the events that were only counted.
const UNATTRIBUTED_WINDOW_MS: i64 = 10_000;
/// The kinds [`CoreThread::refuse_unattributed`] tells apart, in the order of [`UnattributedLog`]'s
/// arrays: a fill, a funding payment, a liquidation, an account state, anything else.
const UNATTRIBUTED_KINDS: [&str; 5] = ["fill", "funding", "liquidation", "account state", "event"];

/// The order client id and the words of a FIRED core-held stop's close (one of `fired`'s) that the venue ENDED UNFILLED
/// (`OrderRejected`, `OrderCanceled`), `None` for any other event and for any other order's end: what
/// `CoreThread::note_fired_stop_end` hears. The membership is asked BEFORE anything is copied (the review of batch 1d,
/// finding 5: every cancel and reject of the watch's ten minutes was copied). Out of line and cold: `fold_venue_event`
/// calls it only while a fired core-held stop is watched.
#[cold]
#[inline(never)]
fn fired_stop_end(fired: &[OrderEmulationStopClose], event: &Event) -> Option<(String, String)> {
    let (order_client_id, reason) = match event {
        Event::OrderRejected(r) => (&r.order_client_id, &r.reason),
        Event::OrderCanceled(c) => (&c.order_client_id, &c.reason),
        _ => return None,
    };
    fired
        .iter()
        .any(|fs| fs.order_client_ids.iter().any(|c| c == order_client_id))
        .then(|| (order_client_id.clone(), reason.to_string()))
}

/// What [`CoreThread::refuse_unattributed`] remembers to keep the UNATTRIBUTED log bounded. Core-local
/// and boxed on first use (`CoreThread::unattributed` is `None` until an event is refused), so the
/// fold's struct pays one pointer; not serialized, not state-hashed, not published.
#[derive(Debug, Default)]
pub(crate) struct UnattributedLog {
    /// Full lines logged so far, per kind.
    full: [u8; UNATTRIBUTED_KINDS.len()],
    /// Events counted since the last summary, per kind.
    suppressed: [u32; UNATTRIBUTED_KINDS.len()],
    /// When the open summary window closes (core clock); `None` until an event is first counted.
    window_end_ms: Option<i64>,
}

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
        // One store: the published list counts an ended order's hour on THIS reading, never on an engine's
        // `now_ms`, which a strategy's order resets to its event's time (`crate::snapshot::OrderWindows::clock`).
        self.order_windows.clock(now_all);
        // DEAD-MAN'S SWITCH freshness observe (trading-hardening): the ONE cheap per-message store —
        // record `now_all` as the freshest data/event ingest ts, so the boundary sweep can tell how
        // long the feed has been silent. Gated on the switch being armed (`Option::is_some`-cheap
        // via `if let`), so when disabled (default) this is a single null-check branch, no store, no
        // log, no allocation — the `p99 < 10µs` fold stays byte-identical. Only genuine liveness
        // messages advance it: venue events + market data (quote/trade/book/mark/closed-bar), NEVER
        // control (Command), the periodic waker (Watchdog), a health-status change (StreamStatus), a
        // trade-link transition (ExecLink), or
        // a historical bar SEED (BarSeed).
        if let Some(dm) = self.deadman.as_mut()
            && matches!(
                msg,
                Ingest::Event(_)
                    | Ingest::GatewayEvent(_)
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
        // disk and replays it. `append_command` serializes `&msg` by BORROW (no clone on the p99<10µs
        // hot path; the `match msg` below still owns and consumes it); the returned seq is unused.
        // Only exec-lane verbs are journaled — market/bar/tick lanes are not command state.
        // `Command::Shutdown` never reaches here (intercepted in `handle`); it gets a Snapshot, not a
        // Command. The whole hook is skipped (one `Option::is_some`) when journaling is off.
        // The cadence SNAPSHOT decision is deferred to AFTER the fold (tail of this fn) so a snap
        // reflects THIS message's effect; only the append + counter bump (the durability write-
        // ahead) run here, before the `match msg` fold. `journaled` is captured now (it gates the
        // after-fold snap to the same exec-lane messages) because `match msg` below consumes `msg`.
        let journaled = self.journal.is_some()
            && match msg {
                // The hot Event arm stays FIRST (one discriminant test, exactly as before).
                Ingest::Event(_) => true,
                // A gateway event is a venue event like the one above: journaled, as ONE record with its
                // cursor (journal v18), unless the ring redelivered it (see `gateway_redelivered`: a
                // record the fold below skips must not be journaled either, or a replay would fold
                // it). Placed AFTER the hot arm on purpose.
                Ingest::GatewayEvent(ref g) => !self.gateway_redelivered(g),
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
                //
                // `ResumeVenue` is the same kind of fact: it releases the link dead-man's overlay,
                // which the journal never held and a replay core could not re-apply (decision 0180).
                //
                // `CancelReconcileOrder` (decision 0196, rule (b)) is the same kind of fact: it reads the
                // held-row store (which a replay never rebuilds), sends one venue cancel and folds
                // nothing, so there is nothing for a replay to re-apply.
                //
                // `UnlatchMount` (decision 0239) is a mount-topology fact too: the replay core has no mount to
                // un-latch, and a new journaled variant is one a build from before it cannot parse. It is
                // durable through the `Snapshot` its arm writes (`JournalSnapshotMountAttribution::latched` and `loss_base`).
                Ingest::Command(
                    Command::MountStrategy(_)
                    | Command::UnmountStrategy { .. }
                    | Command::ResumeVenue { .. }
                    | Command::CancelReconcileOrder(_)
                    | Command::UnlatchMount { .. },
                ) => false,
                // An admission stating a conversion rate that is not a finite number is refused by the
                // engine below and is NOT journaled: `serde_json` writes the NaN as `null`, which a
                // replay reads back as no rate at all, and a symbol with no rate and no currency is
                // admitted at par. The live run and the replay must refuse the same commands, so the
                // command that would differ never reaches the journal (`states_non_finite_rate`).
                Ingest::Command(Command::AdmitSymbol(ref a)) if a.states_non_finite_rate() => false,
                // An account-scoped state or hold that would change NOTHING (an unknown or stopped
                // account, a release with no hold, a state the engine already holds) is not journaled:
                // it would stamp the segment format 21 and close the rollback door for nothing, and a
                // replay core, which has no retirement, could fold what this core refused
                // (`account_command_lands`). Per COMMAND, never reached by the hot Event arm above.
                Ingest::Command(
                    ref c @ (Command::SetAccountTradingState { .. } | Command::HoldAccount { .. }),
                ) => self.account_command_lands(c),
                // A node-wide word a faulted core refuses (`set_trading_state`) is not journaled either: a replay
                // core has no fault and would apply it, so a restart would come back reopened.
                Ingest::Command(Command::SetTradingState(st)) => !self.reopens_a_faulted_core(st),
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
                | Ingest::Flow(_)
                | Ingest::DataDelay(_) => false,
                // A venue's funding RATE is market data: what a paper engine books from it is journaled
                // as its own `Event::Funding` record (`runtime/funding.rs`), so a replay needs no rate.
                Ingest::FundingRate(_) => false,
                // A trade-link transition is a transport fact about THIS process's connection, not
                // order state: not journaled (the twin of `StreamStatus` above), so a restart begins
                // with every link presumed up until its client discloses otherwise.
                Ingest::TradeLink(_) => false,
                // An account change carries a live engine, not data: session TOPOLOGY like a runtime mount, never
                // journaled (it could not be serialized). The `Snapshot` it writes at its end is what carries
                // the new engine into the journal (`account_change.rs`).
                Ingest::Accounts(_) => false,
                // A sandboxed strategy's reply carries a closure, not data; the orders it applies are
                // journaled at the drain as `StrategySubmit`, which is what a replay re-applies.
                Ingest::MountReply(_) => false,
            };
        if journaled {
            self.journal
                .as_mut()
                .expect("journaled implies journal.is_some()")
                .append_command(now_all, &msg)
                .expect("journal append");
            self.journaled_since_snap += 1;
        }
        match msg {
            Ingest::Event(event_value) => self.fold_venue_event(event_value, now_all, None),
            // A venue event that came over a gateway's trade link (journal v18). Journaled above as
            // ONE record with its cursor; folded here by the SAME helper as the arm above, so the two
            // doors cannot disagree about a venue event. The cursor is bookkeeping for the next `Snapshot`
            // and the next resume, never fold input: it touches no engine state.
            //
            // ⚠ A REDELIVERY (same epoch, `seq` at or below the held cursor) is dropped whole, before
            // the journal and the fold: the engines dedup fills by trade id, but a funding payment is
            // a bare `+=`, an `AccountState` overwrites, and the journal and `exec_fill` would
            // each hold the event twice. The gate sits on this arm only; the hot `Event` arm pays
            // nothing for it.
            Ingest::GatewayEvent(g) => {
                if self.gateway_redelivered(&g) {
                    tracing::debug!(
                        target: "vike_core::core",
                        route_key = %g.route_key,
                        gateway_epoch = g.cursor.gateway_epoch,
                        seq = g.cursor.sequence,
                        "gateway event redelivered at or below the held cursor: dropped"
                    );
                } else {
                    let GatewayEvent { event, route_key, cursor } = *g;
                    self.note_gateway_cursor(route_key, cursor);
                    // The ring's account rides into the fold: it routes a FOREIGN fill
                    // (`route_event_via`), and nothing else reads it past the cursor above.
                    self.fold_venue_event(event, now_all, Some(route_key));
                }
            }
            Ingest::Command(cmd) => {
                match cmd {
                    Command::Order(intent) => {
                        // An operator's cancel the registry cannot act on is said out loud, never
                        // accepted in silence. Only HERE (the external command path): a strategy's
                        // cancel of a registry-less order, lowered by `apply_intent_routed`, stays
                        // quiet. A cold command, off the fold; applying `intent` is unchanged.
                        self.report_unheld_cancel(&intent);
                        self.apply_intent(intent, self.engine.now_ms);
                    }
                    // A journal replay's order, or a held order's release (`held_release.rs`, the one
                    // live sender), lowered onto the engine its route key names. Out of line and cold,
                    // like the other rare verbs below.
                    Command::OrderOn(o) => self.apply_routed_order(*o),
                    // One step of an EMULATED order (`emulated.rs`): sent by the live core's own boundary
                    // drive, re-applied by a replay core. Cold, per child verb.
                    Command::Emulated(step) => self.apply_emulated_step(*step),
                    // Live-parameter plane (audit co8): route a typed params update to ONE mount (the
                    // one `mount_id` names, else the sole mount on (venue, symbol, interval); an
                    // ambiguous update is refused with a note) and run its `on_params_updated`
                    // hook — a running strategy is re-tuned WITHOUT unmounting (which would lose
                    // queue position). A RARE control command (a GUI/operator re-tune), NOT a market
                    // message, so it is off the p99<10µs event fold; it touches NO OMS state (only
                    // the mounted strategy's tunables + any orders that hook buffers, drained the one
                    // live path).
                    Command::UpdateParams(u) => self.drive_strategy_params(&u),
                    // The operator's node-wide state (the venue-scoped link halts keep their say
                    // over a venue whose link is still down). A cold control verb, off the fold.
                    Command::SetTradingState(st) => self.set_trading_state(st),
                    // ONE account's state (the stricter of it and the node's floor wins; a link-held
                    // engine is skipped). Journaled like the node-wide word, per COMMAND (rare), off the
                    // fold: this arm runs for an operator verb or an account-watcher edge only.
                    Command::SetAccountTradingState { route_key, state } => {
                        self.set_account_trading_state(&route_key, state)
                    }
                    // The account watcher's closing-only hold of a kept-up lane twin: a floor a node-wide
                    // word cannot loosen. Same posture: per command, journaled, off the fold.
                    Command::HoldAccount { route_key, hold } => self.hold_account(&route_key, hold),
                    // The operator's `resume-venue` (decision 0180): ONE venue's link-dead-man halt,
                    // and nothing else. A cold control verb, off the fold; not journaled (above).
                    Command::ResumeVenue { venue, force } => self.resume_venue(&venue, force),
                    // LIVE per-symbol leverage change — write the target venue's engine's
                    // `initial_margin_by_symbol` (rare operator/GUI verb; off the hot fold).
                    //
                    // ⚠ Deliberately `venue`, not `route_key`, and deliberately a FAN-OUT rather
                    // than `engine_index_for_route_key`: this is the one command in this file that
                    // already addresses ENGINES BY VENUE and applies to every match. Identical
                    // today (one engine per venue), and left alone because the operator's verb is
                    // genuinely venue-shaped — `MarginUpdate` has no field that could name one
                    // account of two. Wiring a second account has to decide what "set binance
                    // leverage" means before this site can move; guessing now would silently pick
                    // one of the two accounts, which is worse than the fan-out.
                    Command::SetMargin(u) => {
                        let MarginUpdate {
                            venue,
                            symbol,
                            initial_margin_requirement: initial_margin_requirement_value,
                        } = *u;
                        if self.engine.venue == venue {
                            self.engine
                                .gate
                                .limits
                                .initial_margin_by_symbol
                                .insert(symbol.clone(), initial_margin_requirement_value);
                        }
                        for (_, e) in self.extra_engines.iter_mut() {
                            if e.venue == venue {
                                e.gate
                                    .limits
                                    .initial_margin_by_symbol
                                    .insert(symbol.clone(), initial_margin_requirement_value);
                            }
                        }
                    }
                    // ADMIT one symbol into ONE account's engine (decision 0173): rare, off the hot fold,
                    // journaled like every command so a replay re-applies it with the facts it carries.
                    // Addressed by ROUTE KEY, not `SetMargin`'s venue fan-out: admitting a symbol into
                    // every account of an exchange would grant a permission nobody named.
                    //
                    // The same record GIVES a symbol back when it states `release` (decision 0205): one
                    // record shape for both, because a build from before the key must still read the
                    // journal (`SymbolAdmission::release`'s note); the engine decides what it may release.
                    Command::AdmitSymbol(a) if a.release => self.release_symbol(&a),
                    Command::AdmitSymbol(a) => {
                        match self.engine_index_for_route_key(RouteKey::declared(&a.route_key)) {
                            None => {
                                self.note(format!(
                                    "AdmitSymbol REFUSED: no engine for route key {}",
                                    a.route_key
                                ));
                            }
                            Some(engine_index) => {
                                match self.engine_mut(engine_index).admit_symbol(&a) {
                                    vike_execution::AdmitOutcome::Refused(why) => {
                                        tracing::warn!(
                                            target: "vike_core::core",
                                            route_key = %a.route_key,
                                            symbol = %a.symbol,
                                            why,
                                            "AdmitSymbol REFUSED"
                                        );
                                        self.note(format!(
                                            "AdmitSymbol REFUSED: {} on {} ({why})",
                                            a.symbol, a.route_key
                                        ));
                                    }
                                    vike_execution::AdmitOutcome::Admitted => {
                                        tracing::info!(
                                            target: "vike_core::core",
                                            route_key = %a.route_key,
                                            symbol = %a.symbol,
                                            "symbol admitted"
                                        );
                                        self.note(format!(
                                            "AdmitSymbol: {} admitted on {}",
                                            a.symbol, a.route_key
                                        ));
                                        // A labelled account now trades a symbol its venue's quotes must
                                        // also reach (`rebuild_admitted_fanout`).
                                        self.rebuild_admitted_fanout();
                                        // …and the value it books the symbol by may be new.
                                        self.rebuild_leg_values();
                                    }
                                    // A refresh may restate the value, and the mount's own symbol may
                                    // have been declared quoted in another currency.
                                    vike_execution::AdmitOutcome::Refreshed
                                    | vike_execution::AdmitOutcome::Mounted => {
                                        self.rebuild_leg_values()
                                    }
                                }
                            }
                        }
                    }
                    Command::ReconcileReports(reports) => {
                        self.reconcile_reports(*reports);
                    }
                    Command::ConfirmReconcile(id) => {
                        self.confirm_reconcile(id);
                    }
                    // The operator's `recon cancel <id>` (decision 0196, rule (b)): ONE cancel for
                    // the order one held `UnknownOrder` row names. A cold control verb, off the
                    // fold; not journaled (above).
                    Command::CancelReconcileOrder(id) => self.cancel_reconcile_order(id),
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
                    // The operator's `trade strategy reset-budget` (decision 0239): clear ONE mount's budget latch when
                    // it is flat and quiet, re-arming its loss budget from where it stands. A cold control
                    // verb, off the fold; not journaled (above), durable through the `Snapshot` it writes.
                    Command::UnlatchMount { mount_id } => self.unlatch_mount(&mount_id),
                    Command::Shutdown => unreachable!("handled in handle()"),
                }
                self.dirty = true;
            }
            Ingest::Market => self.drain_market(),
            // A bar of an INPUT series no mount hears (decision 0271's live half, `runtime/inputs.rs`) is
            // the inputs' alone: it fills no paper order, moves no mark, fires no conditional and steps no
            // strategy, because a higher timeframe's range spans prices from before the orders resting
            // now. One `is_empty()` on a core whose mounts read no input.
            Ingest::BarSeed(seed)
                if !self.input_keys.is_empty()
                    && self.input_only(&seed.venue, &seed.symbol, &seed.interval) =>
            {
                let BarSeed { venue, symbol, interval, bars } = *seed;
                self.feed_inputs(&(venue, symbol, interval), &bars);
                self.dirty = true;
            }
            Ingest::BarClose(update)
                if !self.input_keys.is_empty()
                    && self.input_only(&update.venue, &update.symbol, &update.interval) =>
            {
                let BarUpdate { venue, symbol, interval, bar, .. } = *update;
                self.feed_inputs(&(venue, symbol, interval), std::slice::from_ref(&bar));
                self.dirty = true;
            }
            Ingest::BarSeed(seed) => {
                let BarSeed { venue, symbol, interval, bars } = *seed;
                let key = (venue, symbol, interval);
                // …and one a mount trades is an input of another mount too.
                if !self.input_keys.is_empty() {
                    self.feed_inputs(&key, &bars);
                }
                let series = self.bars.entry(key).or_default();
                series.closed = Arc::new(bars);
                series.forming = None; // the live stream refreshes it
                self.dirty = true;
            }
            Ingest::BarClose(update) => {
                let BarUpdate { venue, symbol, interval, bar, sides } = *update;
                let key = (venue, symbol, interval);
                if !self.input_keys.is_empty() {
                    self.feed_inputs(&key, std::slice::from_ref(&bar));
                }
                // A mount step a shared close still holds on this series' previous bar runs FIRST, before
                // this bar's fills and steps (`shared_close.rs`). One `is_empty()` while nothing is held.
                if !self.shared_closes.is_empty() {
                    self.flush_shared_closes_of(&key, bar.timestamp);
                }
                // STAMP THE SERIES SYMBOL ONCE, here, so every downstream consumer sees the same
                // bar: the paper client (which already got a stamped COPY below), the closed-bar
                // cache that `OrderBroker::bars` hands back, and the strategy's `on_bar`.
                //
                // Live bars arrive with `symbol: None` — `vike_bridge_core::klines::kline_to_bar`
                // builds them from a venue kline that carries no vike symbol — and the runtime is
                // the first place that KNOWS it (`key.1`). Withholding it made a mount's `on_bar`
                // structurally unable to tell one series from another, which is why a two-leg
                // strategy could not work live even after its ORDERS were routed correctly.
                //
                // Safe for every existing mount BECAUSE of the opt-in routing lane: a mount that
                // declares no extra symbols routes an order naming `""` or its own symbol to its own
                // and refuses any other (`resolve_intent_symbol`), so a strategy that now passes this
                // REAL (own) symbol where it used to pass `""` routes exactly where it did before. Before that lane
                // existed this stamp would have sent a real symbol to venues, which is why it was
                // withheld.
                //
                // A bar that ALREADY carries a symbol is left alone (a backtest/seeded bar names
                // its own instrument, and the replayed value is the source of truth).
                let mut bar = bar;
                if bar.symbol.is_none() {
                    bar.symbol = Some(key.1.as_str().into());
                }
                let last_timestamp =
                    self.bars.get(&key).and_then(|s| s.closed.last().map(|b| b.timestamp));
                let append = match last_timestamp {
                    // reconnect overlap: the same window re-closes — replace idempotently
                    Some(t) if bar.timestamp == t => {
                        if let Some(series) = self.bars.get_mut(&key)
                            && let Some(last) = Arc::make_mut(&mut series.closed).last_mut()
                        {
                            *last = bar.clone();
                        }
                        false
                    }
                    Some(t) if bar.timestamp < t => false, // stale replay — drop
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
                    // at mount and fell back to `vike_paper::ExecutionClientPaper` held a book
                    // nothing ever filled: its strategy's orders rested forever, never filling and
                    // never terminalizing, with no error anywhere.
                    //
                    // `engines_of_venue` is `[default]` on every single-account core, so the
                    // sequence below — including `pump_client`, which already drains every
                    // engine — is unchanged there.
                    for engine_index in self.engines_of_venue(&key.0) {
                        if self.engine(engine_index).accepts_symbol(&key.1) {
                            // The series symbol is stamped once at the top of this arm now, so a
                            // multi-book paper client can route and every consumer agrees on the
                            // same bar; single-book clients ignore `bar.symbol` (r7 law intact).
                            // A bar folded from quotes carries its sides: the paper book fills a buy
                            // on the ask's path and a sell on the bid's (owner, 2026-11-16).
                            self.engine_mut(engine_index)
                                .client
                                .on_bar_sides(&bar, sides.as_deref());
                            self.pump_client();
                            self.dispatch_applied_fills();
                        }
                    }
                    let series = self.bars.entry(key.clone()).or_default();
                    Arc::make_mut(&mut series.closed).push(bar.clone());
                }
                // a close supersedes any forming state of that (or an older) window
                if let Some(series) = self.bars.get_mut(&key)
                    && series.forming.as_ref().is_some_and(|f| f.timestamp <= bar.timestamp)
                {
                    series.forming = None;
                }
                self.dirty = true;
                if append {
                    self.drive_strategy(&key, &bar, sides.as_deref());
                }
            }
            Ingest::Quote(qu) => {
                let QuoteUpdate { venue, symbol, quote } = *qu;
                let ts = quote.timestamp;
                if let Some(engine_index) =
                    self.engine_index_for_route_key(RouteKey::sole_account_of(&venue))
                    && self.engine(engine_index).accepts_symbol(&symbol)
                {
                    self.engine_mut(engine_index)
                        .price_board
                        .set_quote(&venue, &symbol, quote.bid, quote.ask, ts);
                }
                // The venue's LABELLED accounts that were admitted this symbol (decision 0174): the
                // venue's default engine is the only one the route above reaches. One `is_empty()`
                // on every core that admitted nothing into a labelled account.
                if !self.admitted_fanout.is_empty() {
                    self.fan_out_quote(&venue, &symbol, quote.bid, quote.ask, ts);
                }
                // The paper books' clock (decision 0285's amendment of 2026-11-16): a quote fills what it
                // reaches on a paper engine, before any strategy or stop hears it. One bool on a node with
                // no paper engine (every live node).
                if self.any_paper_engine {
                    self.quote_paper_books(&venue, &symbol, quote.bid, quote.ask, ts);
                }
                self.drive_strategy_tick(&venue, &symbol, quote.bid, quote.ask, ts, |s, ctx| {
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
                let ts = trade.timestamp;
                if let Some(engine_index) =
                    self.engine_index_for_route_key(RouteKey::sole_account_of(&venue))
                    && self.engine(engine_index).accepts_symbol(&symbol)
                {
                    self.engine_mut(engine_index)
                        .price_board
                        .set_last_trade(&venue, &symbol, px, ts);
                }
                if !self.admitted_fanout.is_empty() {
                    self.fan_out_trade(&venue, &symbol, px, ts);
                }
                self.drive_strategy_tick(&venue, &symbol, px, px, ts, |s, ctx| {
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
                // no top, so the strategy sees the next (fuller) update (v1 simplification).
                // The derived TOP, read once. Sizes are bound too (they used to be discarded)
                // because the cross-venue reference lane below delivers a full `TickQuote`: a
                // maker sizing against the reference venue's displayed depth needs them, and a
                // book's derived L1 must be indistinguishable from that venue's native L1.
                let top = (book.best_bid(), book.best_ask());
                if let (
                    Some(L2BookLevel { price: bid, .. }),
                    Some(L2BookLevel { price: ask, .. }),
                ) = top
                {
                    let now = self.engine.now_ms;
                    if let Some(engine_index) =
                        self.engine_index_for_route_key(RouteKey::sole_account_of(&venue))
                        && self.engine(engine_index).accepts_symbol(&symbol)
                    {
                        self.engine_mut(engine_index)
                            .price_board
                            .set_quote(&venue, &symbol, bid, ask, now);
                    }
                    if !self.admitted_fanout.is_empty() {
                        self.fan_out_quote(&venue, &symbol, bid, ask, now);
                    }
                    self.drive_strategy_tick(&venue, &symbol, bid, ask, now, |s, ctx| {
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
                            Some(vike_marketdata::L2BookLevel { price: bb, qty: bq }),
                            Some(vike_marketdata::L2BookLevel { price: ba, qty: aq }),
                        ) = top
                    {
                        self.drive_strategy_reference_quote(&venue, &symbol, bb, ba, bq, aq, now);
                    }
                }
            }
            // audit C3 + co6: the watchdog SWEEP now runs at the drain-loop boundary, driven by the
            // `TimerWheel` (see `run` + `drive_due_timers`), NOT here. This message is kept
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
                let StreamStatusUpdate { venue, symbol, stream, status } = *su;
                // THE CONNECTION-STATE DEAD-MAN's ONE observe hook (M13). This arm — not the
                // tick/quote/trade/bar/book fold — is where the switch learns anything, which is
                // what keeps the p99<10µs core hop byte-identical: a status change is an
                // OCCASIONAL control event the producer fires only on a transition. Gated on
                // `is_some`, so a default core pays one `Option` branch on a message it already
                // pays a strategy dispatch for. A RECOVERY is the one thing worth a line: the
                // trip's own `warn!` lives in the sweep, and this is its `info!` twin.
                // A `Disconnected` of a price-only venue that holds nothing opens no window
                // (`link_window_may_open`: one `Option` test elsewhere, and only for a `Disconnected`);
                // it is REMEMBERED instead, and the sweep that finds the venue holding something opens
                // the window from it (`DeadManLink::remember_down`).
                let may_open =
                    status != FeedStatus::Disconnected || self.link_window_may_open(&venue);
                if let Some(ldm) = self.link_deadman.as_mut() {
                    let source = vike_venues::link_deadman::DeadManLinkSource::of(&stream);
                    let observed = if may_open {
                        ldm.observe(&venue, &symbol, source, status, self.engine.now_ms)
                    } else {
                        ldm.remember_down(&venue, &symbol, source);
                        DeadManLinkObservation::Quiet
                    };
                    match observed {
                        DeadManLinkObservation::Quiet => {}
                        DeadManLinkObservation::Recovered => tracing::info!(
                            target: "vike_core::link_deadman",
                            %venue,
                            %symbol,
                            %stream,
                            ?source,
                            "LINK RECOVERED after a dead-man trip: the switch re-arms for the next \
                             outage, and a venue it halted is released at the next link sweep once \
                             none of that venue's links is down"
                        ),
                        // The key is NOT clear: another source still holds its own trip, so the
                        // line above would be false. Say which source came back and that the key
                        // is still tripped.
                        DeadManLinkObservation::RecoveredWhileAnotherSourceStaysTripped => {
                            tracing::info!(
                                target: "vike_core::link_deadman",
                                %venue,
                                %symbol,
                                %stream,
                                ?source,
                                "LINK SOURCE RECOVERED after a dead-man trip, but another source of \
                                 this venue/symbol is STILL tripped: only this source re-armed, and \
                                 the venue stays halted until none of its links is down"
                            )
                        }
                    }
                }
                self.drive_strategy_feed_status(&venue, &symbol, status);
            }
            // An account's TRADE LINK went down or came back (`vike_execution::TradeLinkUpdate`) — an
            // OCCASIONAL control event, fired only on a transition. Touches NO OMS fold and runs no
            // strategy hook: it files the link's state for the stuck-order watchdog, which defers an
            // account's pre-ack orders while the link is down (`runtime/execution_link.rs`). Not
            // journaled, and it never advances the silence dead-man's freshness (see the `matches!`
            // at the top of this fn).
            Ingest::TradeLink(u) => self.note_execution_link(*u, now_all),
            // Decision 0169: an account became active while the process runs. A COLD control message
            // (once per account start), applied between two messages so the events queued behind it
            // fold into an engine that exists; touches no existing engine (`account_change.rs`).
            Ingest::Accounts(change) => self.apply_account_change(*change, now_all),
            // Per-side FLOW-TOXICITY update (RTDS wallet-toxicity guard) — an OCCASIONAL control
            // event (the producer fires it at toxicity cadence, not per market message), so this is
            // NOT the per-tick hot path. Touches NO OMS fold: it only runs the mounted strategy's
            // on_flow hook + drains any orders that hook buffered through the one live path. The
            // exact twin of the StreamStatus arm above.
            Ingest::Flow(fu) => {
                let FlowUpdate { venue, symbol, flow } = *fu;
                self.drive_strategy_flow(&venue, &symbol, flow);
            }
            // A stream's delay label CHANGED (decision 0140) — an OCCASIONAL control event (the
            // producer fires it when the label is first known and on each change, never per market
            // message). Touches NO OMS fold and runs no strategy hook: it files the label for the
            // submit stamp and on the pair's mounts (`runtime/data_delay.rs`). Nothing logged here.
            Ingest::DataDelay(du) => {
                let DataDelayUpdate { venue, symbol, stream: _, delay } = *du;
                self.note_data_delay(&venue, &symbol, delay);
            }
            // A venue's public funding rate (about a second per perpetual, never per trade): a PAPER
            // engine's funding clock, and the settlement it passed booked as a journaled funding event.
            // Out of line and cold (`runtime/funding.rs`); a live engine is never charged from it.
            Ingest::FundingRate(fu) => self.note_funding_rate(*fu),
            // A sandboxed strategy's answer to one hook (decision 0273 item 3): a COLD control message, one
            // per answered hook, applied as that mount's own orders through the drain (`hooks.rs`).
            Ingest::MountReply(reply) => self.apply_mount_reply(*reply),
        }
        // CADENCE SNAPSHOT (spec §A): taken AFTER the fold above so the snap reflects THIS message's
        // effect. Taken before (as the write-ahead append is), a cadence snap would omit the just-
        // journaled message while that message sits at a LOWER seq than the snap — and replay
        // (latest-Snapshot-wins, apply seq > snap) would silently drop it: a <=1-message loss on a
        // cadence-snap restore. `journaled` gates this to the same exec-lane messages the counter
        // tracks (never a market/bar/tick) and is false when journaling is off, so the disabled
        // path stays zero-overhead and byte-identical. The fills this message folded reach their mounts
        // first (`write_snap_after_delivery`), or the snap holds them in the account and not in the mount.
        if journaled
            && self.journaled_since_snap
                >= self
                    .config
                    .journal
                    .as_ref()
                    .expect("journal is Some only when config.journal is (assemble_core)")
                    .snapshot_every
        {
            self.write_snap_after_delivery(now_all);
        }
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The ONE fold of a venue event, shared by the `Ingest::Event` and `Ingest::GatewayEvent` arms of
    /// [`Self::dispatch`] (the second is the first plus a cursor, so it must not grow a second copy of
    /// this body). `now_all` is the dispatch's single clock read; `ring` is the gateway ring's
    /// account (`GatewayEvent::route_key`) for the second arm and `None` for the first.
    ///
    /// `#[inline(always)]` keeps the hot `Event` arm what it was before the body moved out of it: the
    /// one place the p99<10µs gate runs through is still a single function with the fold inline, and
    /// the `GatewayEvent` arm gets its own copy of the code, not a call the hot arm pays for.
    #[inline(always)]
    fn fold_venue_event(
        &mut self,
        event: Event,
        now_all: i64,
        ring: Option<vike_marketdata::Symbol>,
    ) {
        if let (Some(tid), Event::Fill(f)) = (self.config.panic_on_trade_id.as_deref(), &event)
            && f.trade_id == tid
        {
            panic!("injected fault: trade_id {tid}");
        }
        // Live-runtime OTO/OCO: capture this REAL-venue event's effect on the contingency
        // book BEFORE `event` is consumed by the bus (the shared `contingency_terminal`
        // classifier `pump_client` also uses), then drive it AFTER the engine has folded the
        // event. A terminal `OrderFilled` ARMS this leg's held OTO children + cancels its OCO
        // siblings (matching the paper oracle's `apply_contingency`); a terminal-WITHOUT-fill
        // (`Canceled`/`Rejected`/`Expired`) cascade-drops the children that can now never arm
        // (the oracle's `expire_children_of`). A partial fill leaves the group resting.
        let contingency_event = self.contingency_terminal(&event);
        // An adopted operator's stop (no mount owns it) may be the order this event ENDS: its ownership
        // line is forgotten after the fold. One `is_empty()` read while none is adopted; the work is
        // out of line (`forget_ended_operator_stops`, `#[inline(never)]`).
        let may_end_a_stop = !self.operator_stops.is_empty()
            && matches!(
                event,
                Event::OrderCanceled(_)
                    | Event::OrderFilled(_)
                    | Event::OrderExpired(_)
                    | Event::OrderRejected(_)
            );
        // A fired core-held stop's close the venue ends unfilled: its words, for the re-arm sweep (`core_held.rs`). One
        // `is_empty()` read while no fired stop is watched; the capture is out of line (`fired_stop_end`, `#[cold]`) and copies
        // only that close's words.
        let stop_end = if self.fired_stops.is_empty() {
            None
        } else {
            fired_stop_end(&self.fired_stops, &event)
        };
        // ⚠ THE FOLD'S VERDICT GATES THE DRIVE (hostile-venue hardening). `contingency_event`
        // is classified from the EVENT ALONE, so before this the bracket/OCO/OTO machinery
        // reacted to events the engine had REFUSED — "invalid transitions are dropped" did
        // not extend to it. A venue frame naming an unknown order client id is dropped and counted into
        // `events_dropped_order_unknown`, yet a fabricated `OrderFilled` on one OCO leg still
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
        // `ring` is `None` on the hot `Event` arm (a constant after inlining), so that arm still
        // makes the call it always made.
        let routed = match ring {
            None => self.route_event(&event),
            Some(_) => self.route_event_via(&event, ring),
        };
        // A venue LIQUIDATION of a position a mount holds is booked into its closed trades after the fold
        // (`closed_trades.rs`'s "What is booked"): it reaches no strategy as a fill. One length read on a
        // mount-less core, one discriminant test beside it; the facts are read out of line.
        let liquidated = if !self.mounts.is_empty()
            && let (Event::PositionLiquidated(p), Some(engine_index)) = (&event, routed)
        {
            Some((engine_index, self.liquidation_of(engine_index, p)))
        } else {
            None
        };
        // The OMS states this fold (`execution_report.rs`): the event is kept for its report before the bus consumes
        // it. One bool and one length on an event that names no order (the gated empty-id fill); the copy and the
        // report are out of line.
        let report = self.kept_for_report(&event);
        let fold = match routed {
            Some(0) => self.bus.publish(event, &mut self.engine),
            Some(i) => self.bus.publish(event, &mut self.extra_engines[i - 1].1),
            // §5.4 — UNATTRIBUTED. A foreign fill on a venue this process runs several
            // accounts of: no stamped key, no order of ours, and two engines claim its
            // symbol. It is REPORTED rather than folded into the venue's default account's
            // ledger, which is the sentence `engine_index_for_venue_symbol`'s own doc has
            // carried since the ambiguity was made to answer `None` — *"that is reconcile's
            // territory, not this lane's"*. Reconcile sees the venue's own position report
            // per account and re-converges whatever this leaves; folding it here would put
            // a stranger's size and price into a book that never traded them, and no later
            // pass can tell that apart from a real position.
            //
            // `EventBusFold::Dropped` is the honest answer for the contingency drive below: nothing
            // moved, so nothing this fill might have named may arm or cancel.
            //
            // The same arm takes a venue-tagged event of a venue with NO default account here when
            // no stamped key, order id or ring names one of its labelled accounts and the venue does
            // not have exactly ONE live account (`route_venue_without_default`): that event used to
            // go to engine 0, i.e. to ANOTHER exchange's engine, where a fill panicked the venue
            // assert, an unstamped funding payment or liquidation was booked into the wrong book,
            // and a stamped one was dropped by the engine's own key check. It is refused here, never
            // folded elsewhere.
            //
            // The body is OUT OF LINE (`refuse_unattributed`, `#[cold]`): this function is
            // `#[inline(always)]` into both `dispatch` arms, so anything written here is copied twice
            // into the fold the latency gate measures.
            None => {
                self.refuse_unattributed(&event, now_all);
                EventBusFold::Dropped
            }
        };
        if let Some(event) = report {
            let cause = super::execution_report::ExecutionReportCause::Event {
                event: &event,
                fold,
                request: None,
            };
            self.journal_execution_report(routed, cause, ring, now_all);
        }
        self.pump_client();
        if may_end_a_stop {
            self.forget_ended_operator_stops();
        }
        if let Some((order_client_id, why)) = stop_end
            && fold == EventBusFold::Applied
        {
            self.note_fired_stop_end(&order_client_id, why);
        }
        if let Some((engine_index, liquidation)) = liquidated
            && fold == EventBusFold::Applied
        {
            self.book_liquidation(engine_index, liquidation);
        }
        match contingency_event {
            // The `fold` guards sit on the ARMS rather than wrapping the whole `match`, so
            // the overwhelmingly common no-bracket path — `contingency_terminal`
            // early-returns `None` on an empty book — never evaluates the comparison.
            Some((order_client_id, ContingencyDrive::Ended)) if fold == EventBusFold::Applied => {
                self.drive_contingency_on_terminal(&order_client_id)
            }
            Some((order_client_id, drive)) if fold == EventBusFold::Applied => {
                self.drive_contingency(&order_client_id, drive, now_all);
                // fold the release submits / sibling cancels / resizes the drive just queued
                self.pump_client();
            }
            // A terminal-shaped event the engine REFUSED. Per-order and fault-adjacent
            // (never per-tick), and it is the one signature a fabricated venue frame leaves.
            // A terminal-shaped event the engine REFUSED. Per-order and fault-adjacent
            // (never per-tick), and it is the one signature a fabricated venue frame leaves.
            Some((order_client_id, _)) => tracing::warn!(
                target: "vike_execution::oms",
                order_client_id = %order_client_id,
                "contingency drive SUPPRESSED: the engine dropped this terminal event \
                 (unknown order client id / illegal transition / replay), so it arms and cancels \
                 nothing"
            ),
            None => {}
        }
        self.dirty = true;
        self.dirty = true;
    }

    /// **Say that a venue event was refused as UNATTRIBUTED — at a bounded rate.** Reached only from
    /// the `None` arm of [`Self::fold_venue_event`], so it is `#[cold]` and `#[inline(never)]`: the
    /// hot function it is inlined into carries one call and none of this.
    ///
    /// The first [`UNATTRIBUTED_FULL_LINES`] events of each KIND get the full treatment: a WARN and a
    /// recent-events note (both allocate). After that the event is only COUNTED (`u32` adds, no
    /// allocation, no log), and one summary WARN per [`UNATTRIBUTED_WINDOW_MS`] of core clock carries
    /// the counts. The window opens at the first counted event, so an event after a quiet spell never
    /// reports a stale count; a flood that stops inside a window leaves its last count unreported.
    /// The state is a lazily boxed, core-local log: not serialized, not state-hashed, not published.
    ///
    /// `now_ms` is the dispatch's own clock read; this takes none of its own (a replay core's clock
    /// hands out one recorded stamp per read).
    #[cold]
    #[inline(never)]
    fn refuse_unattributed(&mut self, event: &Event, now_ms: i64) {
        let (slot, venue, symbol) = match event {
            Event::Fill(f) => (0, f.venue.as_str(), f.symbol.as_str()),
            Event::Funding(f) => (1, f.venue.as_str(), f.symbol.as_str()),
            Event::PositionLiquidated(p) => (2, p.venue.as_str(), p.symbol.as_str()),
            Event::AccountState(a) => (3, a.venue.as_str(), ""),
            // a fee adjustment of a fill that names no order of ours: counted with the others, but named
            Event::FeeAdjustment(f) => (4, f.venue.as_str(), f.symbol.as_str()),
            // every other event: not this tap's business (audited 2026-10, I-21)
            _ => (4, "", ""),
        };
        let kind = UNATTRIBUTED_KINDS[slot];
        let log = self.unattributed.get_or_insert_with(Default::default);
        if log.full[slot] < UNATTRIBUTED_FULL_LINES {
            log.full[slot] += 1;
            tracing::warn!(
                target: "vike_core::core",
                venue,
                symbol,
                "UNATTRIBUTED venue {kind}: it names no order this process placed and no single \
                 account of its venue can own it (several accounts could, or none runs here, and \
                 nothing names one), so NOTHING was folded. Each account's own reconcile pass \
                 re-converges its book against the venue's per-account position report."
            );
            self.note(format!(
                "UNATTRIBUTED {kind} on {venue} {symbol}: names no order of ours and no single \
                 account can own it (nothing folded)"
            ));
            return;
        }
        log.suppressed[slot] = log.suppressed[slot].saturating_add(1);
        match log.window_end_ms {
            None => log.window_end_ms = Some(now_ms.saturating_add(UNATTRIBUTED_WINDOW_MS)),
            Some(end) if now_ms >= end => {
                let [fills, fundings, liquidations, account_states, others] = log.suppressed;
                log.suppressed = [0; UNATTRIBUTED_KINDS.len()];
                log.window_end_ms = Some(now_ms.saturating_add(UNATTRIBUTED_WINDOW_MS));
                tracing::warn!(
                    target: "vike_core::core",
                    fills,
                    fundings,
                    liquidations,
                    account_states,
                    others,
                    window_ms = UNATTRIBUTED_WINDOW_MS,
                    "UNATTRIBUTED venue events SUPPRESSED since the last report (each kind logs \
                     its first few in full, then only this count): nothing was folded. Each \
                     account's own reconcile pass re-converges its book."
                );
            }
            Some(_) => {}
        }
    }

    /// Is `g` an event this core already holds: the ring's cursor for its account is in the SAME
    /// epoch and at or past `g`'s `seq`? Inside one epoch the gateway delivers strictly after the
    /// cursor, so such an event is a redelivery (a reader that reconnected with events still in
    /// flight, a resume point older than the core's own). A different epoch is never a redelivery
    /// (a restarted gateway's ring begins again), and an account with no cursor yet has none to
    /// be at or below. The decision reads only the held cursor, so the journal and the live fold
    /// agree and a replay of the journal never needs it. [`EventRingCursor::after`] stays the ONE
    /// advance rule: this only asks whether there is anything to advance.
    #[inline]
    fn gateway_redelivered(&self, g: &GatewayEvent) -> bool {
        self.gateway_cursors.iter().any(|(key, held)| {
            *key == g.route_key
                && held.gateway_epoch == g.cursor.gateway_epoch
                && g.cursor.sequence <= held.sequence
        })
    }

    /// Hold the cursor of the gateway ring `route_key` names (journal v18): the resume point the next
    /// `Snapshot` stamps and a restart presents. A linear scan over one entry per gateway account; the
    /// first event of an account pushes it, later ones advance it by [`EventRingCursor::after`], the
    /// one rule the boot restore's tail fold uses too.
    #[inline]
    fn note_gateway_cursor(&mut self, route_key: vike_marketdata::Symbol, cursor: EventRingCursor) {
        match self.gateway_cursors.iter_mut().find(|(key, _)| *key == route_key) {
            Some((_, held)) => *held = held.after(cursor),
            None => self.gateway_cursors.push((route_key, cursor)),
        }
    }
}
