//! The exit arms of `apply_intent_routed` — `MassCancel`, `Flatten` and `MarketExit` — one method
//! each. `MarketExit` re-enters the dispatcher for its mass-cancel leg and its flatten legs, and
//! `Flatten` re-enters it for each leg's `Submit`.

use super::*;

impl<C: ExecutionClient> CoreThread<C> {
    /// The [`OrderIntent::MassCancel`] arm of [`Self::apply_intent_routed`]. It submits nothing,
    /// so it always returns an empty list.
    pub(super) fn lower_mass_cancel(
        &mut self,
        venue: Option<String>,
        symbol: Option<String>,
        account: Option<vike_model::accounts::account_keys::AccountLabel>,
        cancel_intent: CancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
        // **THE PAYLOAD MAY NAME ITS ACCOUNT, and that is resolved before anything is
        // cancelled** — `reduce_route_for_account`'s four answers. `None` hands back
        // `route` untouched, so every account-less cancel below (the dead-man, the shutdown
        // sweep, a strategy's `mass_cancel`, the unscoped panic button) runs exactly the
        // arm it always ran. A named account the core does not hold, or one named with no
        // venue, is refused by name and touches nothing: returning here is the whole of
        // "never widened".
        let Some(route) =
            self.reduce_route_for_account("mass_cancel", route, venue.as_deref(), account.as_ref())
        else {
            self.pump_client();
            return Vec::new();
        };
        // A payload-NAMED account narrows the core-held protection too — see the clear
        // calls below. An account-less cancel keeps the venue-wide clear it always had.
        let narrowed = account.is_some();
        match (venue, symbol) {
            (None, None) => {
                self.engine.mass_cancel_with_intent(cancel_intent);
                for (_, e) in self.extra_engines.iter_mut() {
                    e.mass_cancel_with_intent(cancel_intent);
                }
                self.clear_conditional_scope(None, None, None);
                // Drop every HELD bracket exit + the whole contingency book (the live legs are
                // canceled at the venue above; clearing the book here stops their subsequent
                // `OrderCanceled` events from re-alerting as "protective exit died").
                self.held_orders.clear();
                self.contingency.clear();
            }
            (Some(v), sym) => {
                // ⚠ A VENUE IS AN EXCHANGE, NOT AN ACCOUNT OF ONE. This resolved a single
                // engine through `route_of`, whose fallback is
                // `RouteKey::sole_account_of` — the venue's DEFAULT account — so on a
                // process holding two accounts of one exchange an operator's
                // "cancel everything on binance" emptied one book and left the other
                // resting. `exit_scope_engines` keeps the ONE-engine answer wherever the
                // caller actually named a book (a labelled mount's own intent, or
                // `sweep_link_deadman`'s explicit per-account fan) and fans over every
                // account of the exchange where it did not.
                //
                // A payload that NAMED an account arrives here with `route` already
                // resolved to that account's engine, so `exit_scope_engines` answers with
                // it alone — the operator's `mass-cancel binance ALT` is ALT's book, never
                // the venue's.
                let engines = self.exit_scope_engines(route, Some(&v));
                for &eidx in &engines {
                    self.eng_mut(eidx).mass_cancel_with_intent(cancel_intent);
                }
                // Held exits in scope have no venue order, so clear them explicitly (live legs
                // self-clean via their venue `OrderCanceled`).
                //
                // ⚠ NARROWED to the cancelled engines when the payload named an account:
                // both stores are keyed by the EXCHANGE, so a venue-wide clear under a
                // named account would strip the OTHER account's stop-losses and held
                // bracket exits while leaving its positions and entries live. `None` keeps
                // the venue-wide clear — the account-less shape, whose engines ARE every
                // account of the venue.
                let scope = narrowed.then_some(engines.as_slice());
                self.clear_held_scope(Some(&v), sym.as_deref(), scope);
                self.clear_conditional_scope(Some(&v), sym.as_deref(), scope);
            }
            (None, Some(_)) => {
                self.note("MassCancel: symbol without venue is ignored".to_string());
            }
        }
        self.pump_client();
        Vec::new()
    }

    /// The [`OrderIntent::Flatten`] arm of [`Self::apply_intent_routed`]: one `reduce_only` market
    /// leg per engine of the venue that holds a position in `symbol`, each lowered as a `Submit`
    /// onto the engine it was read from.
    pub(super) fn lower_flatten(
        &mut self,
        venue: String,
        symbol: String,
        account: Option<vike_model::accounts::account_keys::AccountLabel>,
        now: i64,
        cancel_intent: CancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
        // **A NAMED account narrows the flatten to that account's book** — resolved before
        // any position is read, through `reduce_route_for_account` (an unheld account is
        // refused by name and closes nothing; `venue` is never absent on this verb). `None`
        // returns `route` untouched, and everything below is then exactly the account-less
        // fan-out it always was.
        let Some(route) =
            self.reduce_route_for_account("flatten", route, Some(&venue), account.as_ref())
        else {
            self.pump_client();
            return Vec::new();
        };
        // **§4.5's LAW, the REDUCING side:** *"A risk-REDUCING venue verb that names no
        // account fans out to EVERY account of that venue. A risk-INCREASING one refuses."*
        //
        // A standalone `Flatten` names a venue and a symbol and no account, and it mints a
        // `reduce_only` MARKET sized to the position it read: it can only ever CLOSE, never
        // open. So it is the reducing side, and §4.5's licence applies in full — *"A fan-out
        // can never reach an account the sender did not mean, because the sender meant all
        // of them."*
        //
        // ⚠ **It resolved ONE engine, and that was a live defect rather than merely a
        // narrow reading.** `route_of(..).unwrap_or(0)` is the venue's DEFAULT account, so
        // on a two-account node "flatten my binance BTC" read the default account's
        // position, closed the default account's position, and left the second account's
        // position wide open — while `MarketExit` on the same venue, which is this verb plus
        // a mass-cancel, already fanned over `exit_scope_engines` and closed both. The two
        // spellings of one operator intention answered differently.
        //
        // ⚠ Each leg goes back to THE BOOK IT WAS READ OUT OF (`EngineRoute::Engine`),
        // exactly as `market_exit_flatten_legs` does and for the identical reason: two
        // accounts holding the same symbol mint BYTE-IDENTICAL requests, so a leg routed by
        // its payload would flatten the default book twice (the second re-resolving the
        // size the first was still closing, i.e. REVERSING it) and never touch the other.
        //
        // BYTE-IDENTICAL at one account: `exit_scope_engines` answers with exactly
        // `{ route_of(..).unwrap_or(0) }` whenever the venue has at most one engine — the
        // routed engine when the route names one, that venue's sole engine otherwise, and
        // `vec![0]` for a venue this core runs none of, which is the `unwrap_or(0)` this
        // replaces. `positions_size_of` is then read from the same engine as before and the
        // leg lowers onto the same engine as before.
        let mut coids = Vec::new();
        for eidx in self.exit_scope_engines(route, Some(&venue)) {
            let pos = self.eng(eidx).position_size_of(&symbol, "BOTH");
            if pos.abs() <= 1e-12 {
                continue;
            }
            let req = OrderRequest {
                client_order_id: String::new(),
                venue: venue.clone(),
                symbol: symbol.clone(),
                side: vike_model::closing_side(pos),
                qty: pos.abs(),
                order_type: "market".to_string(),
                reduce_only: true,
                ts: now,
                ..Default::default()
            };
            coids.extend(self.apply_intent_routed(
                OrderIntent::Submit(Box::new(req)),
                now,
                cancel_intent,
                EngineRoute::Engine(eidx),
            ));
        }
        coids
    }

    /// The [`OrderIntent::MarketExit`] arm of [`Self::apply_intent_routed`] — the panic button: the
    /// mass-cancel leg first, then one `Flatten` per non-flat position read AFTER it.
    pub(super) fn lower_market_exit(
        &mut self,
        venue: Option<String>,
        account: Option<vike_model::accounts::account_keys::AccountLabel>,
        now: i64,
        cancel_intent: CancelIntent,
        route: EngineRoute,
    ) -> Vec<String> {
        // **A NAMED account narrows the whole exit to that account's book, and is resolved
        // ONCE, here, before either leg runs** (`reduce_route_for_account`). The resolved
        // route then scopes the mass-cancel leg, the halt notice and the flatten walk alike,
        // which is what keeps "which books does this exit cover" a single answer. An unheld
        // account, or one named with NO venue, is refused by name and neither leg runs —
        // the venue-less case matters most here, because the arm it would otherwise reach
        // is the global exit.
        //
        // ⚠ `account: None` returns `route` untouched: the venue-wide exit and the UNSCOPED
        // panic button (`venue: None, account: None`) take exactly the path they always
        // took, and no gate of the account family can refuse them.
        let Some(route) =
            self.reduce_route_for_account("market_exit", route, venue.as_deref(), account.as_ref())
        else {
            self.pump_client();
            return Vec::new();
        };
        // COMPOUND VERB, PURE EXPANSION. Nothing here talks to an engine directly: the
        // expansion ([`Self::expand_market_exit`]) is a pure read of live state into a
        // Vec of EXISTING primitive intents, each of which is then re-entered through this
        // same `apply_intent` — so mass-cancel, minting, routing and the RiskGate all keep
        // exactly the semantics they have on every other path.
        //
        // JOURNAL/REPLAY (why no new record kind): the compound intent is journaled by the
        // EXISTING write-ahead `Cmd`/`StrategySubmit` record that carried it, and the
        // expansion is re-derived on replay from the replayed state — the same contract
        // `OrderIntent::Flatten` (position read at apply time) and `MassCancel` already
        // rely on. Replay folds the identical prefix of records, so the `Account`
        // positions this reads are identical, so the expansion is identical, so the coids
        // minted for the flatten legs are identical. ZERO new journal record kinds.
        //
        // ORDERING IS LOAD-BEARING, and the flatten legs are derived AFTER the cancel:
        // `MassCancel`'s own arm ends in `pump_client()`, which can fold venue events —
        // including a FILL that OPENS a position on a symbol that was flat when the
        // operator hit the button. A plan snapshotted before the cancel would carry no leg
        // for that symbol and the "get me out" verb would hand back an OPEN position. So
        // the cancel is applied first and `market_exit_flatten_legs` reads the post-pump
        // `Account`. (Positions already open are safe either way — `Flatten` re-resolves
        // its size via `position_size_of` at apply time, so a fill folded in between
        // shrinks/zeroes the leg rather than double-flattening.)
        //
        // BEST-EFFORT, NOT ATOMIC: against a real venue `mass_cancel` is fire-and-forget
        // over the adapter's `ExecActor` thread — the cancel ACKs arrive asynchronously on
        // the ingest lane, potentially long after these flatten market orders are on the
        // wire. Ordering the legs is the strongest guarantee the core can give locally; it
        // does NOT prevent a resting order filling after a flatten leg on a live venue.
        // Re-issue the exit if the position board is not flat afterwards.
        //
        // `TradingState` — THE PANIC BUTTON WORKS FROM A HALTED CORE, and that is the whole
        // point of it. The flatten legs are ordinary `reduce_only` submits through the SAME
        // `RiskGate`, and its kill switch admits a POSITION-COVERED reduce
        // (`vike_model::is_covered_reduce`) under `Halted` — which is exactly the shape
        // `OrderIntent::Flatten` mints (side opposite the position, qty `|position|`). So
        // they pass under `Active`, `Reducing` AND `Halted`.
        //
        // ⚠ It used to be the opposite, and the reversal is deliberate. The gate denied
        // EVERY order under `Halted`, `reduce_only` included, so this verb ran its
        // mass-cancel and then had every flatten come back `OrderDenied` — disarmed in
        // precisely the situations that reach `Halted` on their own (`enter_safe_state`
        // after a fold panic, the dead-man's switch), which are the situations an operator
        // reaches for this verb. The documented cure was "SetTradingState(Active) first,
        // then re-issue" — i.e. un-halt the whole core, including the strategy that got you
        // here, from a phone, mid-incident. A kill switch must never trap you in a position.
        //
        // ⚠ ONE RESIDUAL, and it is a property of the GRID rather than of the halt: the gate
        // re-checks coverage against the LOT-ROUNDED size, so a position sitting OFF the lot
        // grid (`|position| = 1.8` on a `1.0` lot) rounds its own flatten UP to `2.0`, which
        // would flip the position and is refused — under `Halted` only, since `Reducing`
        // trusts the flag. That leg surfaces as an ordinary `OrderDenied` like any other
        // refusal. Every size this core mints is already lot-rounded, so it takes an
        // externally-sourced position (a venue liquidation, a reconcile fold) to reach.
        let mut coids = Vec::new();
        // RISK-OFF, declared: "get me out" is the one verb whose cancels a venue must
        // never hold back under its own rate/credit budget. Behaviourally identical to the
        // `Unspecified` default on every venue today — this states the classification
        // rather than leaving a venue to infer it from an unlabeled cancel.
        coids.extend(self.apply_intent_routed(
            Self::market_exit_mass_cancel(venue.as_deref(), account.as_ref()),
            now,
            CancelIntent::RiskOff,
            route,
        ));
        // ONE engine scope for the whole verb — the same set the mass-cancel leg above
        // reached, the set the halt notice speaks for, and the set the flatten legs are
        // read from. Deriving them separately is how the cancel and the flatten came to
        // disagree about which books an exit covers.
        let halted = self
            .exit_scope_engines(route, venue.as_deref())
            .into_iter()
            .any(|idx| self.eng(idx).trading_state == TradingState::Halted);
        let legs = self.market_exit_flatten_legs(route, venue.as_deref());
        if halted && !legs.is_empty() {
            // Say so POSITIVELY. An operator who knows the core is halted has every reason
            // to expect the exit to be refused (it was, for this verb's whole life, and the
            // runbook said so), so "it went through" is the useful thing to tell them —
            // and it is what distinguishes a working exit from a silent no-op.
            tracing::warn!(
                target: "vike_core::core",
                legs = legs.len(),
                "MarketExit while HALTED: the mass-cancel ran and the flatten legs ARE \
                 being submitted — the kill switch admits position-covered reduces, so it \
                 cannot trap you in a position. No un-halt is needed."
            );
            self.note(format!(
                "MarketExit: HALTED — flattening anyway ({} leg(s)); halt admits covered reduces",
                legs.len()
            ));
        }
        // ⚠ EACH LEG GOES BACK TO THE BOOK IT WAS READ OUT OF — `EngineRoute::Engine(idx)`,
        // not the exit's own `route`. A `Flatten` carries a venue and a symbol and no
        // account, so two accounts of one exchange holding the same symbol minted
        // byte-identical legs and both resolved the venue's DEFAULT engine: it was
        // flattened twice (the second leg re-resolving the size the first one was still
        // trying to close, i.e. REVERSING the position) while the second account was never
        // flattened at all. The index is the only thing that can say which, and it is
        // carried on the leg for exactly that reason.
        for (idx, it) in legs {
            coids.extend(self.apply_intent_routed(
                it,
                now,
                cancel_intent,
                EngineRoute::Engine(idx),
            ));
        }
        coids
    }
}
