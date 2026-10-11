//! Liveness sweeps: the dead-man's switch and the per-link connection dead-man.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
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
    /// **The CANCEL is scoped to the venue whose link died; the HALT is not, and the asymmetry is
    /// deliberate.** A bybit socket death must not pull a binance book — a link that is up is a
    /// link a strategy can still manage its orders through — so the cancel goes out as
    /// `OrderIntent::MassCancel { venue: Some(v), symbol: None }` through the ONE order-write
    /// vocabulary, so nothing bypasses mint/route/gate and the armed conditional books are that
    /// path's job as always. HALT, under
    /// [`DeadManAction::CancelAllAndHalt`], engages on EVERY engine plus the one cross-process
    /// sentinel file, because that file is process-wide by construction: a half-halted daemon —
    /// one venue's engine `Halted` while a sentinel every venue's `ExecActor` reads sits on disk —
    /// is a state nobody asked for and cannot be reasoned about at 3am.
    ///
    /// ⚠ **A LINK DEATH IS AN EXCHANGE FACT, SO EVERY ENGINE OF THAT EXCHANGE IS CANCELLED** —
    /// [`Self::engines_of_venue`], one `MassCancel` per match through
    /// [`Self::apply_intent_routed`] with an explicit [`super::EngineRoute::Engine`]. The
    /// account-scoped spelling this used first — one `apply_intent_with_cancel_intent` letting
    /// `apply/exit.rs`'s `MassCancel` arm resolve the engine — cancels ONE engine, the venue's DEFAULT
    /// account (`route_of` falls through to `RouteKey::sole_account_of`), so on a process holding
    /// two accounts of one exchange the labelled account's book survived the socket death. That
    /// process is not hypothetical: `vike_mount::make_engine_accounts` builds it from an
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
    /// Exactly ONE `tracing::warn!` per trip, naming the venue, the symbol, the grace and the
    /// action; the recovery `info!` lives on the observe hook in `dispatch`. The switch re-arms
    /// per link on the next `FeedStatus::Live` (see [`LinkDeadMan::observe`]); `Halted` and the
    /// sentinel do NOT clear, exactly as with the silence switch.
    pub(crate) fn sweep_link_deadman(&mut self, now: i64) {
        // Evaluate the pure trip logic; release the `self.link_deadman` borrow before acting.
        let (tripped, action) = match self.link_deadman.as_mut() {
            Some(ldm) => (ldm.check(now), ldm.action()),
            None => return, // switch disabled (default) — inert
        };
        if tripped.is_empty() {
            return; // every armed link is up, inside its grace, or already tripped this outage
        }
        let grace_ms = self.config.link_deadman.as_ref().map_or(0, |c| c.grace.as_millis() as i64);
        let halted = action.engages_halt();
        for (venue, symbol) in &tripped {
            tracing::warn!(
                target: "vike_core::link_deadman",
                %venue,
                %symbol,
                grace_ms,
                ?action,
                "LINK DEAD-MAN TRIPPED: this venue's feed has reported the link DISCONNECTED for \
                 longer than the grace — cancelling every resting order on THIS VENUE"
            );
        }
        // ONE cancel pass per VENUE, not per link: two symbols of one venue expiring in the same
        // sweep are one exchange fact, and a `MassCancel` with no symbol already pulls that
        // engine's whole book. The `warn!` above stays per link, because which links died is what
        // an operator reads.
        let mut cancelled: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for (venue, _) in &tripped {
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
            }
        }
        if halted {
            // In-process gate, every engine: the RiskGate now denies every new order.
            self.engine.trading_state = TradingState::Halted;
            for (_, e) in self.extra_engines.iter_mut() {
                e.trading_state = TradingState::Halted;
            }
            // Cross-process sentinel — the SAME file a manual `touch HALT` writes and every venue's
            // `ExecActor` submit boundary checks. Best-effort: a failed write must not panic the
            // core, which has already engaged the in-process gate above.
            if let Some(path) = self.config.link_deadman.as_ref().and_then(|c| c.halt_file.clone())
                && let Err(e) = std::fs::File::create(&path)
            {
                tracing::warn!(
                    target: "vike_core::link_deadman",
                    error = %e,
                    path = %path.display(),
                    "link dead-man: failed to write HALT sentinel (best-effort; in-process HALT still engaged)"
                );
            }
        }
        let links = tripped.iter().map(|(v, s)| format!("{v}/{s}")).collect::<Vec<_>>().join(", ");
        self.note(format!(
            "link dead-man TRIPPED on {links}: cancelled that venue's resting orders{}",
            if halted { " + HALT engaged" } else { "" }
        ));
        self.dirty = true;
    }
}
