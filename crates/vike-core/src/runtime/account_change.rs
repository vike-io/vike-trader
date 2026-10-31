//! The cold arm that APPENDS an engine to a RUNNING core (decision 0169, slice S1) or STOPS one (slice
//! S2): an account that became active, or inactive, while the process runs. A stop never removes an
//! engine: phase one cancels the account's own orders, tombstones the index
//! (`CoreThread::retired_engines`) and lists the account NOT RUNNING (`stop_engine_hot`); phase two, sent
//! once the venue has answered those cancels, begins the detach of its client (`detach_engine_hot`).
//!
//! The single-writer fold is the only mutator of the engine list, so the change arrives as an
//! `Ingest::Accounts` message ([`vike_exec::AccountChange`]) and is applied BETWEEN two messages: after
//! the one before it folded and before the next, which is the only place an append can be atomic
//! for the events queued behind it (the new account's own event lane is released only after this
//! message is queued, so its first event folds after the engine exists). It is never journaled: the
//! `Snap` written at the end of it is what carries the new engine into the journal.
//!
//! What an append keeps true, each a line below:
//!
//! * **An engine's index is its attribution key for ever.** It goes at the END of `extra_engines`;
//!   nothing is moved, reused or compacted.
//! * **Config, not state, is imposed as `spawn_core_multi` imposes it** on every extra engine
//!   (equity seed, the one price config, the mark staleness windows, applied-fill capture).
//! * **The risk state follows the set** (decision 0169 item 8): an engine added while the core is
//!   `Reducing` or `Halted` starts in that state, and the drawdown latch's peak rises by the seed
//!   (the capital base grew by it; an addition is not a new high, so a curve already in drawdown
//!   stays in it).
//! * **`multi_account` is recomputed from the engines**, the same way assembly computes it, because
//!   a venue that gains a second account turns on the mark mirror and the ambiguity gate.

use super::*;

/// The door the core opens a parcel with: the downcast to the `ExecutionEngine<C>` this core was
/// spawned over. A function pointer stored on the core by `spawn_core_multi` (the one place `C:
/// 'static` is known), so the fold's `impl` blocks need no new bound. A core built without it (the
/// synchronous test core) refuses every parcel.
pub(crate) fn engine_of_parcel<C: ExecutionClient + 'static>(
    parcel: Box<dyn std::any::Any + Send>,
) -> Option<ExecutionEngine<C>> {
    parcel.downcast::<ExecutionEngine<C>>().ok().map(|engine| *engine)
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Apply one [`AccountChange`]. `now_ms` is the dispatch's single clock read.
    pub(crate) fn apply_account_change(&mut self, change: AccountChange, now_ms: i64) {
        // A refusal is a note in the ring too, and a note only reaches the published snapshot when the core is
        // dirty: whatever the outcome, publish.
        self.dirty = true;
        match change {
            AccountChange::AddEngine { seed, engine } => self.add_engine_hot(seed, engine, now_ms),
            AccountChange::StopEngine { not_running } => self.stop_engine_hot(not_running, now_ms),
            AccountChange::DetachEngine { route_key } => self.detach_engine_hot(&route_key),
        }
    }

    /// **STOP one account, phase one** (decision 0169, slice S2). The engine stays where it is; what changes,
    /// in this order:
    ///
    /// 1. its resting orders are cancelled, and ONLY its own (`MassCancel` naming the account, routed at the
    ///    engine: the held exits and conditionals it narrows to are this book's too). The client stays
    ///    attached, so the venue's answers to those cancels still fold into the engine;
    /// 2. the engine is tombstoned (`retired_engines`), so a later start of the same key appends a NEW engine
    ///    and the lookups prefer it. A submit that still reaches the engine (its client is attached until the
    ///    detach) is refused by the core with a terminal `ACCOUNT_NOT_RUNNING` reject (`lower_submit`);
    /// 3. the link dead-man forgets this account's trade-link window, or an outage disclosed before the stop
    ///    would expire into a `CancelAllAndHalt` for the venue (no `Live` is coming from a link closed on
    ///    purpose);
    /// 4. the account is listed NOT RUNNING with the caller's reason, and a `Snap` names it.
    ///
    /// The session is ended by phase two ([`Self::detach_engine_hot`]), which the caller sends once the
    /// cancels are answered: a lease closed at once would drop the venue's `OrderCanceled` and leave the
    /// cancelled orders `Accepted` in this engine for good.
    ///
    /// **Risk state: nothing is rebased.** The engine keeps its seed and its own PnL in the drawdown latch's
    /// sums and in the published totals, so a stop is a zero step (the curve is bit-identical across it) and
    /// an open position keeps being valued, not pretended gone. A position is never flattened here.
    fn stop_engine_hot(&mut self, not_running: vike_exec::NotRunningAccount, now_ms: i64) {
        let key = not_running.route_key.clone();
        let Some(idx) = self
            .engine_idx_for_route_key(RouteKey::declared(&key))
            .filter(|&i| !self.is_retired(i))
        else {
            tracing::warn!(
                target: "vike_core::core",
                route_key = %key,
                "ACCOUNT STOP ignored: no running engine carries that route key"
            );
            self.note(format!("ACCOUNT STOP ignored: `{key}` is not running"));
            return;
        };
        let venue = self.eng(idx).venue.clone();
        let account = not_running.account.clone().unwrap_or_default();
        self.apply_intent_routed(
            OrderIntent::MassCancel {
                venue: Some(venue.clone()),
                symbol: None,
                account: Some(account),
            },
            now_ms,
            CancelIntent::Unspecified,
            EngineRoute::Engine(idx),
        );
        self.retired_engines.push(idx);
        self.account_holds.remove(&idx); // its hold goes with the engine (a restart is a NEW engine)
        self.rebuild_admitted_fanout();
        self.forget_stopped_trade_links(idx, &venue);
        self.drop_held_recon_of(&key);
        self.config.not_running.retain(|nr| nr.route_key != key);
        tracing::warn!(
            target: "vike_core::core",
            %venue,
            route_key = %key,
            index = idx,
            reason = %not_running.reason,
            "ACCOUNT STOPPED in the running core: its resting orders were cancelled and new orders \
             for it are refused; every other account is untouched and its position (if any) is \
             left where it is. Its session ends when the detach follows"
        );
        self.note(format!("ACCOUNT STOPPED: `{key}` (engine {idx}) — {}", not_running.reason));
        self.config.not_running.push(not_running);
        self.dirty = true;
        if self.journal.is_some() {
            self.write_snap(now_ms);
        }
    }

    /// **STOP one account, phase two**: begin the detach of the stopped engine's client. NON-blocking: the
    /// lease says its goodbye and closes on the link's own thread, and the join (`shutdown`) is never done
    /// here, on the fold, where it would stall every other account. A submit that still reaches the client
    /// afterwards gets the client's terminal `OrderRejected`, so no order vanishes. The window the link
    /// dead-man forgot at phase one is forgotten again: a link that dropped in between opened a new one, and
    /// a link closed on purpose never says `Live`.
    fn detach_engine_hot(&mut self, route_key: &str) {
        let Some(idx) = (0..=self.extra_engines.len())
            .rev()
            .find(|&i| self.is_retired(i) && self.eng(i).route_key == route_key)
        else {
            tracing::warn!(
                target: "vike_core::core",
                %route_key,
                "ACCOUNT DETACH ignored: no stopped engine carries that route key"
            );
            self.note(format!("ACCOUNT DETACH ignored: `{route_key}` was not stopped"));
            return;
        };
        let venue = self.eng(idx).venue.clone();
        self.eng_mut(idx).begin_shutdown();
        self.forget_stopped_trade_links(idx, &venue);
        // The gate belongs to the KEY: a newer running engine of it keeps its own.
        if self
            .engine_idx_for_route_key(RouteKey::declared(route_key))
            .is_some_and(|i| self.is_retired(i))
        {
            self.exec_link.remove(route_key);
        }
        tracing::warn!(
            target: "vike_core::core",
            %venue,
            %route_key,
            index = idx,
            "ACCOUNT DETACHED: the stopped account's client is detaching, its lease closes"
        );
        self.note(format!("ACCOUNT DETACHED: `{route_key}` (engine {idx})"));
    }

    /// Close the link dead-man's trade-link windows of the symbols engine `idx` traded. Two accounts that
    /// trade one `(venue, symbol)` share ONE trade-link window, so a window is kept only while a RUNNING
    /// account of that key has its own link disclosed down (its `Live` will close it); when every other
    /// account on the key is up, the window can only have been opened by the account that was stopped, and no
    /// `Live` will ever come for a link closed on purpose.
    fn forget_stopped_trade_links(&mut self, idx: usize, venue: &str) {
        let stopped = self.eng(idx);
        let symbols: Vec<String> =
            std::iter::once(stopped.symbol.clone()).chain(stopped.extra_symbols.clone()).collect();
        let owned: Vec<bool> = symbols
            .iter()
            .map(|s| {
                (0..=self.extra_engines.len()).any(|i| {
                    let e = self.eng(i);
                    i != idx
                        && !self.is_retired(i)
                        && e.venue == venue
                        && (e.symbol == *s || e.extra_symbols.contains(s))
                        && self.exec_link_is_down(&e.route_key)
                })
            })
            .collect();
        if let Some(ldm) = self.link_deadman.as_mut() {
            for (symbol, owned) in symbols.iter().zip(owned) {
                if !owned {
                    ldm.forget_trade_link(venue, symbol);
                }
            }
        }
    }

    /// Drop the held reconcile divergences of the stopped account `route_key` and their announcement state: its
    /// engine takes no more passes, so nothing would ever clear them, and an operator confirm would fold the
    /// venue's side into a book that is shut. A later start of the same account is a new engine whose first
    /// pass raises what the venue still shows, as news.
    fn drop_held_recon_of(&mut self, route_key: &str) {
        let identities: Vec<_> = self
            .recon_alerts
            .values()
            .filter(|held| held.route_key == route_key)
            .map(|held| held.identity.clone())
            .collect();
        self.recon_pass.remove(route_key);
        // ...and what was said of its unprotected positions: no pass will clear it now.
        self.recon_announce.naked.forget(route_key);
        if identities.is_empty() {
            return;
        }
        for identity in &identities {
            self.recon_announce.forget(identity);
        }
        self.recon_alerts.retain(|_, held| held.route_key != route_key);
        tracing::warn!(
            target: "vike_core::core",
            route_key,
            dropped = identities.len(),
            "ACCOUNT STOPPED: its held reconcile divergences were dropped (nothing would clear them now)"
        );
        self.note(format!(
            "ACCOUNT STOPPED: `{route_key}` — {} held reconcile divergence(s) dropped",
            identities.len()
        ));
    }

    /// Append the parcel's engine. A refusal is a note in the recent-events ring and a log line, never
    /// a panic: a running core keeps trading through a bad request, and the engine it refuses is
    /// shut down so its venue session does not outlive the refusal.
    ///
    /// `#[inline(never)]`: its one caller is the `Ingest::Accounts` arm of `dispatch`, so it was inlined
    /// into the fold's hottest function, and the currency-group lines this change added to it grew
    /// `dispatch` (`nm -S`: +252 B with no source change there) for a path that runs once per account
    /// start. Cold, off the fold. Measured with the attribute (`nm -S` on the `runtime_latency` release
    /// binary, `CoreThread<SubmitProbe>::dispatch`): 63,150 B on the release this was merged into, 57,732 B
    /// here, with `add_engine_hot` a 7,207 B symbol of its own: the fold's function is 5.4 KB SMALLER than
    /// before any of this, not larger.
    #[inline(never)]
    fn add_engine_hot(&mut self, seed: f64, parcel: Box<dyn std::any::Any + Send>, now_ms: i64) {
        let Some(open) = self.engine_door else {
            self.note(
                "ACCOUNT ADD refused: this core was built without an engine door".to_string(),
            );
            return;
        };
        let Some(mut engine) = open(parcel) else {
            self.note(
                "ACCOUNT ADD refused: the parcel is not an engine of this core's client type"
                    .to_string(),
            );
            return;
        };
        let route_key = engine.route_key.clone();
        if self
            .engine_idx_for_route_key(RouteKey::declared(&route_key))
            .is_some_and(|i| !self.is_retired(i))
        {
            // Two RUNNING engines with one route key would make one unreachable. (A STOPPED engine of
            // the key does not count: the account starting again appends a new one, decision 0169 item 7.)
            tracing::error!(
                target: "vike_core::core",
                %route_key,
                "ACCOUNT ADD refused: this core already runs an engine for that account"
            );
            self.note(format!("ACCOUNT ADD refused: `{route_key}` already runs"));
            engine.begin_shutdown();
            engine.shutdown();
            return;
        }
        // Config-not-state, exactly as `spawn_core_multi` and `assemble_core` impose it on an extra.
        engine.equity_seed = seed;
        engine.price_cfg = self.config.price_cfg;
        engine.account.set_mark_staleness_ms(self.config.mark_staleness_ms);
        engine.account.set_reconcile_staleness_ms(self.config.reconcile_mark_staleness_ms);
        engine.collect_applied_fills = self.mounts.iter().flatten().any(|m| {
            m.venue == engine.venue
                || m.symbols.iter().any(|leg| leg.venue.as_deref() == Some(engine.venue.as_str()))
        });
        engine.now_ms = now_ms;
        // Decision 0169 item 8: a hot start never reopens risk the core has closed.
        // The NODE's state (`node_floor`): a primary halted for its own venue's dead link (link dead-man),
        // or held by an account-scoped state, is not a node-wide halt and must not close the new
        // account of another venue.
        let node_state = self.node_floor;
        if node_state != TradingState::Active {
            engine.trading_state = node_state;
        }
        // The account STARTING AGAIN: its stopped engine keeps its own PnL in the sums (it is still a fact), but
        // its seed is the capital base this engine now re-states, so it is moved out of the base and out of
        // the peak with it (a restart is neither a new high nor a drawdown). The stopped PRIMARY's seed is
        // `CoreConfig::seed_cash`, not an entry of the list, and is taken out the same way: left in, the
        // account's capital would be counted twice for the rest of the process.
        // Each seed moves the peak of ITS engine's currency group (`shift_curve_peak`): the primary's
        // `pnl_curve_peak` for an engine in the primary's currency, every node today.
        //
        // ⚠ ONLY when the successor joins the tombstone's group. A successor in ANOTHER group (the account's
        // currency was told differently on this start) does not re-state the old group's capital, so taking
        // the seed out would leave that group with a zero base while the tombstone's PnL still counts in it:
        // the latch would disarm that group and stop judging the loss. The seed stays, with its peak.
        let new_ccy = engine.ledger_ccy_key();
        for i in 0..self.extra_engines.len() {
            if self.is_retired(i + 1)
                && self.extra_engines[i].1.route_key == route_key
                && self.extra_engines[i].1.ledger_ccy_key() == new_ccy
            {
                let old_seed = std::mem::take(&mut self.extra_engines[i].0);
                self.shift_curve_peak(new_ccy, -old_seed);
            }
        }
        if self.is_retired(0)
            && self.engine.route_key == route_key
            && self.engine.ledger_ccy_key() == new_ccy
        {
            let old_seed = std::mem::take(&mut self.config.seed_cash);
            self.shift_curve_peak(new_ccy, -old_seed);
        }
        // The capital base grows by the seed; so does the peak (an addition is not a new high).
        self.shift_curve_peak(new_ccy, seed);
        let venue = engine.venue.clone();
        let index = self.extra_engines.len() + 1;
        // An account in a currency group no engine the core holds is in makes the node a several-currency
        // one for good (engines never leave): said here, each time a NEW group appears.
        let new_group = new_ccy != self.engine.ledger_ccy_key()
            && self.extra_engines.iter().all(|(_, e)| e.ledger_ccy_key() != new_ccy);
        self.extra_engines.push((seed, engine));
        if new_group {
            self.ledger_mixed = true;
            self.report_currency_groups();
        }
        // A venue the link dead-man holds halted halts its new account too (the account joins the
        // halt, and is released with the rest when the venue's links are up).
        if self.link_deadman.as_ref().is_some_and(|ldm| ldm.is_venue_halted(&venue)) {
            let prior = self.eng(index).trading_state;
            if let Some(ldm) = self.link_deadman.as_mut() {
                ldm.record_halt(&venue, index, prior);
            }
            self.eng_mut(index).trading_state = TradingState::Halted;
        }
        self.rebuild_admitted_fanout();
        // A lane twin started after the core did shares its parent's ceiling from the next tick on.
        self.arm_exposure_pair_timer(now_ms);
        // A strategy MOUNTED on the account was bound, at assemble, to the engine that has just been stopped:
        // it follows the account to the new one (else every order it sends is refused and it never sees the
        // new engine's events).
        for m in 0..self.mount_engine.len() {
            let bound = self.mount_engine[m];
            if self.is_retired(bound) && self.eng(bound).route_key == route_key {
                self.mount_engine[m] = index;
            }
        }
        self.multi_account = assemble::shares_a_venue(
            std::iter::once(self.engine.venue.as_str())
                .chain(self.extra_engines.iter().map(|(_, e)| e.venue.as_str())),
        );
        // The account runs now: an order addressed to it is no longer refused as NOT RUNNING.
        self.config.not_running.retain(|nr| nr.route_key != route_key);
        if let Some(message) = crate::account_ambiguity::ambiguity_warning(
            &crate::account_ambiguity::ambiguous_venues(
                std::iter::once((self.engine.venue.as_str(), self.engine.route_key.as_str()))
                    .chain(
                        self.extra_engines
                            .iter()
                            .map(|(_, e)| (e.venue.as_str(), e.route_key.as_str())),
                    ),
            ),
        ) {
            tracing::warn!(target: "vike_core::core", "{message}");
        }
        tracing::warn!(
            target: "vike_core::core",
            %venue,
            %route_key,
            index,
            "ACCOUNT STARTED in the running core: its engine was appended (index {index}); every other \
             account's engine, orders and positions are untouched"
        );
        self.note(format!("ACCOUNT STARTED: `{route_key}` (engine {index})"));
        self.dirty = true;
        // The journal's latest Snap must name the new engine before any record refers to it.
        if self.journal.is_some() {
            self.write_snap(now_ms);
        }
    }
}
