//! Liveness sweeps: the dead-man's switch and the per-link connection dead-man.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// Does `venue` hold anything the link dead-man would be protecting: an engine of it with a live
    /// order (the predicate [`is_terminal_status`] names) or a NON-FLAT position on that venue. ⚠ NOT a
    /// mere admitted symbol (`ExecutionEngine::extra_symbols`): admissions are never released, so counting
    /// one kept a price-only venue armed for good with nothing at risk, and a hub outage past the grace
    /// then halted the whole node (HALT is not venue-scoped). Read only for the venues of
    /// [`CoreConfig::price_only`], by the cold link sweep and the status gate
    /// ([`Self::link_window_may_open`]); a venue this core runs no engine for holds nothing.
    pub(crate) fn venue_holds(&self, venue: &str) -> bool {
        self.engines_of_venue(venue).into_iter().any(|i| {
            let e = self.eng(i);
            e.registry.values().any(|mo| !is_terminal_status(mo.status))
                || e.account
                    .positions
                    .iter()
                    .any(|((v, _, _), p)| v.as_str() == venue && p.size != 0.0)
        })
    }

    /// May a `Disconnected` disclosed for `venue` OPEN a link dead-man window? Always, except for a
    /// price-only venue ([`CoreConfig::price_only`]) that holds nothing ([`Self::venue_holds`]): its
    /// window would be discarded at the next sweep anyway, but a sweep that has not run yet when
    /// something is admitted or ordered would let the idle-time window survive and trip up to a sweep
    /// interval before the grace of the thing now held. One `Option` test for every other core. Cold: the
    /// `StreamStatus` arm fires on a transition only, and the scan below runs for a `Disconnected` on a
    /// price-only venue only.
    pub(crate) fn link_window_may_open(&self, venue: &str) -> bool {
        match self.config.price_only.as_ref() {
            Some(po) if po.venues.contains(venue) => self.venue_holds(venue),
            _ => true,
        }
    }

    /// Dead-man's-switch sweep (trading-hardening; see [`crate::runtime::deadman`]) — the AUTOMATIC
    /// counterpart to the manual HALT sentinel. Runs at the drain-loop boundary on the
    /// [`super::TimerKind::DeadManSweep`] cadence (NEVER the per-message fold), driven by the injected
    /// clock's `now`. A no-op when the switch is disabled (`self.deadman` is `None`, the default) or
    /// when the feed is still fresh / has already tripped this outage.
    ///
    /// On TRIP (data/events silent past `timeout`, first time this outage): it cancels EVERY resting
    /// order through the ONE order-write vocabulary ([`Self::apply_intent`] →
    /// `OrderIntent::MassCancel`, so nothing bypasses mint/route/gate), and — for
    /// [`DeadManAction::CancelAllAndHalt`] — engages HALT two ways: the in-process
    /// `trading_state = Halted` gate on every engine (the `RiskGate` then denies every new order) AND,
    /// when [`DeadManConfig::halt_file`] is configured, a best-effort write of that SAME cross-process
    /// HALT sentinel the venue adapter's `ExecActor` submit boundary checks — so new-order refusal is
    /// consistent whether or not the runtime loop is still responsive. Exactly ONE `tracing::warn!`
    /// (a fault transition, within the logging budget) is emitted per trip. The switch re-arms itself
    /// on recovery (see [`DeadMan::check`]).
    pub(crate) fn sweep_deadman(&mut self, now: i64) {
        // Evaluate the pure trip logic; release the `self.deadman` borrow before acting on `self`.
        let action = match self.deadman.as_mut() {
            Some(dm) => match dm.check(now) {
                Some(a) => a,
                None => return, // fresh, or already tripped this outage
            },
            None => return, // switch disabled (default) — inert
        };
        tracing::warn!(
            target: "vike_core::deadman",
            ?action,
            "DEAD-MAN'S SWITCH TRIPPED: market data / venue events silent past timeout — cancelling all resting orders"
        );
        // Cancel every resting order via the single order-write path (also clears any armed
        // conditional books — a dead-man pulls ALL exposure). RISK-OFF, declared: a switch that
        // tripped because the feed went silent must not have its cancels shed by a venue budget.
        self.apply_intent_with_cancel_intent(
            OrderIntent::MassCancel { venue: None, symbol: None, account: None },
            now,
            CancelIntent::RiskOff,
        );
        let halted = action.engages_halt();
        if halted {
            // A node-wide stop replaces any venue-scoped link halt: nothing may re-arm over it.
            self.clear_link_halts();
            // In-process gate: the RiskGate now denies every new order on every engine.
            self.engine.trading_state = TradingState::Halted;
            for (_, e) in self.extra_engines.iter_mut() {
                e.trading_state = TradingState::Halted;
            }
            // Cross-process sentinel: write the SAME HALT file the venue ExecActor checks, so its own
            // submit thread also refuses new orders. Best-effort — a failed write must not panic the
            // core (the in-process gate above already stops new orders through this runtime).
            if let Some(path) = self.config.deadman.as_ref().and_then(|c| c.halt_file.clone())
                && let Err(e) = std::fs::File::create(&path)
            {
                tracing::warn!(
                    target: "vike_core::deadman",
                    error = %e,
                    path = %path.display(),
                    "dead-man's switch: failed to write HALT sentinel (best-effort; in-process HALT still engaged)"
                );
            }
        }
        self.note(format!(
            "dead-man's switch TRIPPED: cancelled all resting orders{}",
            if halted { " + HALT engaged" } else { "" }
        ));
        self.dirty = true;
    }

    /// CONNECTION-state dead-man sweep (M13; see [`crate::runtime::link_deadman`]) — the switch
    /// that trips on a socket the BRIDGE reports dead and stays quiet through a market that merely
    /// closed. Runs at the drain-loop boundary on the [`super::TimerKind::LinkDeadManSweep`]
    /// cadence (NEVER the per-message fold), driven by the injected clock's `now`. A no-op when the
    /// switch is disabled (`self.link_deadman` is `None`, the default) or when no armed link's
    /// grace has expired.
    ///
    /// **BOTH the CANCEL and the HALT are scoped to the venue whose link died** (owner ruling
    /// 2026-10-18, decision 0180: a link loss on one venue must never stop another). A bybit
    /// socket death must not pull a binance book or stop binance trading — a link that is up is a
    /// link a strategy can still manage its orders through — so the cancel goes out as
    /// `OrderIntent::MassCancel { venue: Some(v), symbol: None }` through the ONE order-write
    /// vocabulary, so nothing bypasses mint/route/gate and the armed conditional books are that
    /// path's job as always. HALT, under [`DeadManAction::CancelAllAndHalt`], sets `Halted` on
    /// every engine of THAT venue (each engine's prior state kept in the latch,
    /// [`LinkDeadMan::record_halt`]) and writes NO sentinel file: the sentinel is read by every
    /// venue's `ExecActor` and would stop them all. The venue's own gate refuses its opening
    /// orders (reduce-only covered exits still pass, as under any halt). The venue is released by
    /// [`Self::rearm_link_halted`] at a sweep once none of its links is down, back to the prior
    /// state of each engine; an operator's `SetTradingState` replaces the overlay.
    ///
    /// ⚠ **A LINK DEATH IS AN EXCHANGE FACT, SO EVERY ENGINE OF THAT EXCHANGE IS CANCELLED** —
    /// [`Self::engines_of_venue`], one `MassCancel` per match through
    /// [`Self::apply_intent_routed`] with an explicit [`super::EngineRoute::Engine`]. The
    /// account-scoped spelling this used first — one `apply_intent_with_cancel_intent` letting
    /// `apply/exit.rs`'s `MassCancel` arm resolve the engine — cancels ONE engine, the venue's DEFAULT
    /// account (`route_of` falls through to `RouteKey::sole_account_of`), so on a process holding
    /// two accounts of one exchange the labelled account's book survived the socket death. That
    /// process is not hypothetical: `vike_tradehub_core::make_engine_accounts` builds it from an
    /// `[accounts]` table, and the default account there is frequently the PAPER one (no
    /// credentials) while the labelled one is live — i.e. the cancel would have emptied a paper
    /// book and left the live orders resting behind the dead link. An empty engine list (a trip on
    /// a venue this core runs no engine for) cancels NOTHING and says so, rather than taking
    /// `route_of`'s `unwrap_or(0)` fallback into some other venue's book.
    ///
    /// ⚠ **The paragraph above is HISTORY for the arm and still the RULE for this site.** That arm
    /// fans over `CoreThread::exit_scope_engines` now (the panic button had the same defect), so an
    /// unrouted `MassCancel { venue: Some(v) }` no longer stops at the default account. This site
    /// keeps its explicit per-engine fan on purpose: naming the engine is what makes the claim
    /// legible here, it is what the empty-list refusal above is built on, and `exit_scope_engines`
    /// answers with exactly the one engine named — so the two spellings cannot double-cancel.
    ///
    /// Exactly ONE `tracing::warn!` per tripped WINDOW, naming the venue, the symbol, the SOURCE,
    /// the grace and the action (plus one more when a further source trips while another is still
    /// down); the recovery `info!` lives on the observe hook in `dispatch`. The switch re-arms
    /// per link on the next `FeedStatus::Live` (see [`LinkDeadMan::observe`]), and the halted
    /// venue is released by the first sweep after its last link is up.
    ///
    /// **Acting again for a later source is safe by construction.** The venue halt is a state whose
    /// FIRST prior the latch keeps (a second `record_halt` changes nothing), and the venue
    /// `MassCancel` re-requests the cancel of every order that is live NOW
    /// — a no-op on an empty book, and the only thing that reaches an order placed since the
    /// first trip. An order whose first cancel is still unanswered is asked again, which a venue
    /// answers as a benign unknown/already-cancelled order.
    pub(crate) fn sweep_link_deadman(&mut self, now: i64) {
        // The price-only venues that hold nothing right now are not armed: `check` discards their
        // windows. Computed before the latch is borrowed (it reads the engines); empty, and no
        // allocation, whenever `CoreConfig::price_only` is `None`.
        let idle: std::collections::BTreeSet<String> = self
            .config
            .price_only
            .as_ref()
            .map(|po| po.venues.iter().filter(|v| !self.venue_holds(v)).cloned().collect())
            .unwrap_or_default();
        // Evaluate the pure trip logic; release the `self.link_deadman` borrow before acting.
        let (tripped, action) = match self.link_deadman.as_mut() {
            Some(ldm) => (ldm.check(now, &idle), ldm.action()),
            None => return, // switch disabled (default) — inert
        };
        self.rearm_link_halted();
        if tripped.is_empty() {
            return; // every armed link is up, inside its grace, or already tripped this outage
        }
        let grace_ms = self.config.link_deadman.as_ref().map_or(0, |c| c.grace.as_millis() as i64);
        let halted = action.engages_halt();
        for LinkTrip { venue, symbol, source, also_tripped } in &tripped {
            tracing::warn!(
                target: "vike_core::link_deadman",
                %venue,
                %symbol,
                ?source,
                grace_ms,
                ?action,
                "LINK DEAD-MAN TRIPPED: this venue's feed has reported the link DISCONNECTED for \
                 longer than the grace — cancelling every resting order on THIS VENUE (and \
                 halting THIS VENUE only, if the action halts)"
            );
            if !also_tripped.is_empty() {
                // Not suppressed, and said out loud: a SECOND source of this key tripped while
                // others still hold their own trips. Acting again is deliberate (an operator may
                // have cleared HALT, and a `cancel_all` trip never halted, so orders can have been
                // placed since the first trip).
                tracing::warn!(
                    target: "vike_core::link_deadman",
                    %venue,
                    %symbol,
                    ?source,
                    ?also_tripped,
                    "LINK DEAD-MAN: a further source of this venue/symbol tripped while the \
                     sources listed in `also_tripped` are still down — acting again for it: the \
                     cancel is re-requested for whatever rests now and the venue's HALT (if the \
                     action engages it) is re-asserted"
                );
            }
        }
        // ONE cancel pass per VENUE, not per link: two symbols of one venue (or two sources of one
        // key) expiring in the same sweep are one exchange fact, and a `MassCancel` with no symbol
        // already pulls that engine's whole book. The `warn!` above stays per window, because which
        // links died is what an operator reads. The dedupe is within THIS sweep only: a window that
        // expires in a LATER sweep acts again (the latch no longer suppresses across sweeps).
        let mut cancelled: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for LinkTrip { venue, .. } in &tripped {
            if !cancelled.insert(venue.as_str()) {
                continue;
            }
            // EVERY engine of this EXCHANGE, not the venue's default account — see this method's
            // doc for the two-account case the account-scoped spelling got wrong.
            let engines = self.engines_of_venue(venue);
            if engines.is_empty() {
                tracing::warn!(
                    target: "vike_core::link_deadman",
                    %venue,
                    "link dead-man tripped for a venue this core runs no engine for — nothing to \
                     cancel. Nothing is routed to a fallback engine: that would cancel some other \
                     venue's book"
                );
                continue;
            }
            for idx in engines {
                // RISK-OFF, declared, for the same reason the silence switch declares it: a cancel
                // issued because a link died must not be shed by a venue's own cancel budget.
                self.apply_intent_routed(
                    OrderIntent::MassCancel {
                        venue: Some(venue.clone()),
                        symbol: None,
                        account: None,
                    },
                    now,
                    CancelIntent::RiskOff,
                    EngineRoute::Engine(idx),
                );
                if halted {
                    // The venue's OWN gate: the RiskGate now denies this engine's opening orders.
                    // No other venue's engine is touched and no sentinel is written.
                    let prior = self.eng(idx).trading_state;
                    if let Some(ldm) = self.link_deadman.as_mut() {
                        ldm.record_halt(venue, idx, prior);
                    }
                    self.eng_mut(idx).trading_state = TradingState::Halted;
                }
            }
            if halted {
                tracing::warn!(
                    target: "vike_core::link_deadman",
                    %venue,
                    "LINK DEAD-MAN HALTED THE VENUE: {venue}'s engines are Halted (opening orders \
                     refused, covered reduces still admitted) until its links are Live again or \
                     an operator sets a trading state. No HALT sentinel was written: every other \
                     venue keeps trading"
                );
            }
        }
        let links = tripped
            .iter()
            .map(|t| format!("{}/{} [{:?}]", t.venue, t.symbol, t.source))
            .collect::<Vec<_>>()
            .join(", ");
        self.note(format!(
            "link dead-man TRIPPED on {links}: cancelled that venue's resting orders{}",
            if halted { " + that VENUE halted (re-arms when its link is Live again)" } else { "" }
        ));
        self.dirty = true;
    }

    /// Release every venue the link dead-man halted whose links are all up again: each engine goes
    /// back to the state it held BEFORE the halt, and only if it is still `Halted` (anything else
    /// moved it on, and re-arming never loosens). Runs at every link sweep, so a `Live` is honoured
    /// within one sweep, and so is a window that was forgotten (an account stopped on purpose) or
    /// discarded (an idle price-only venue). A core that has FAULTED (`self.fault`: the panic safe
    /// state) releases nothing — only an operator loosens that.
    fn rearm_link_halted(&mut self) {
        if self.fault.is_some() {
            return;
        }
        let Some(recovered) = self.link_deadman.as_mut().map(LinkDeadMan::take_recovered) else {
            return;
        };
        for (venue, engines) in recovered {
            for (idx, prior) in engines {
                let e = self.eng_mut(idx);
                if e.trading_state == TradingState::Halted {
                    e.trading_state = prior;
                }
            }
            tracing::info!(
                target: "vike_core::link_deadman",
                %venue,
                "LINK DEAD-MAN RE-ARMED THE VENUE: every link of {venue} is up again, so its \
                 engines are back to the trading state they held before the halt"
            );
            self.note(format!("link dead-man RE-ARMED {venue}: its links are Live again"));
            self.dirty = true;
        }
    }

    /// The trading state of the NODE, as opposed to the engine at index 0: the state the primary
    /// held before the link dead-man halted its venue alone, else its own. What a node-wide
    /// decision (the drawdown latch's "already latched?", a hot-started engine's inherited state)
    /// must read, or a dead link on the primary's venue would look like a node-wide halt.
    pub(crate) fn node_trading_state(&self) -> TradingState {
        self.link_deadman
            .as_ref()
            .and_then(|ldm| ldm.prior_of(0))
            .unwrap_or(self.engine.trading_state)
    }

    /// Forget every venue-scoped link halt: an explicit trading state (the operator's, or a
    /// node-wide stop's) replaces them, and nothing may re-arm over it.
    pub(crate) fn clear_link_halts(&mut self) {
        if let Some(ldm) = self.link_deadman.as_mut() {
            ldm.clear_halts();
        }
    }

    /// RAISE every engine to at least `st`, never loosening one: the drawdown latch is node-wide, and
    /// an engine the link dead-man holds `Halted` stays so while the state it returns to rises.
    pub(crate) fn raise_trading_state(&mut self, st: TradingState) {
        if let Some(ldm) = self.link_deadman.as_mut() {
            ldm.raise_priors(st);
        }
        for idx in 0..=self.extra_engines.len() {
            let e = self.eng_mut(idx);
            if e.trading_state.strictness() < st.strictness() {
                e.trading_state = st;
            }
        }
    }
}
