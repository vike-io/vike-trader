//! `apply_intent` — THE order-write lowering site: every OrderIntent from every origin (external
//! Command path, LiveBroker strategy drain, conditional FIRE, margin-call liquidation) is meant to
//! flow through here — mint → route → RiskGate (inside `submit_order`) → client — so order
//! submission has one gated, mint-consistent path. (A source-scan test in the control-boundary
//! tests enforces that no engine submit call lives outside this file and its `apply/` children.) `use super::*` re-exports
//! the parent fold module's imports.

use super::*;

mod combo;
mod conditional;
mod contingency;
mod exit;
mod routed;
mod submit;

/// Does this request's reserved contingency slots declare it a member of an OTO/OCO group? Plain
/// orders (the byte-identical path) answer `false` and are never entered into the contingency book.
/// The `build_bracket` lowering stamps `contingency_type = "OTO"` on the entry and
/// `parent_order_id`/`linked_order_ids` on the exits — any of the three marks a leg.
fn has_contingency_links(req: &OrderRequest) -> bool {
    req.parent_order_id.is_some()
        || !req.linked_order_ids.is_empty()
        || req.contingency_type.is_some()
}

/// Strip the OTO/OCO linkage from a request about to go to the VENUE. The core emulates OTO/OCO in
/// its own [`vike_exec::ContingencyBook`], so the venue — or an OCO-enforcing test client like the
/// paper exchange — must see a PLAIN order. Left intact, a client that also holds OTO children would
/// RE-hold a just-released exit (whose parent already filled and can never re-arm it there),
/// dead-locking the fill. No mounted venue enforces OCO/OTO natively today; a future native-OCO
/// venue would pass the linkage through here instead of stripping it.
fn strip_contingency_links(mut req: OrderRequest) -> OrderRequest {
    req.parent_order_id = None;
    req.linked_order_ids.clear();
    req.contingency_type = None;
    req.order_list_id = None;
    req
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The ONE [`OrderIntent::MassCancel`] leg of a [`OrderIntent::MarketExit`], scoped exactly as
    /// the exit is (`None` ⇒ every engine + every conditional book; `Some(v)` ⇒ every engine OF
    /// THAT EXCHANGE that the exit's own route reaches + that venue's books).
    ///
    /// ⚠ The second half said "that engine", singular, and it was the defect rather than the
    /// description: the arm resolved ONE engine through `CoreThread::route_of`, so a venue-scoped
    /// exit on a two-account node cancelled the default account's book and left the second
    /// account's resting orders live — free to fill straight after the flatten legs and put the
    /// operator back in. The arm fans over [`Self::exit_scope_engines`] now.
    ///
    /// `account` is the EXIT's own, carried onto the leg so the cancel narrows exactly as the exit
    /// does — including the core-held protection the `MassCancel` arm clears (held bracket exits,
    /// armed conditionals), which a leg carrying `None` would clear for every account of the venue
    /// even under a route that names one engine. The exit has already resolved (or refused) the
    /// account before this leg is built, so the leg's own resolution cannot refuse.
    pub(crate) fn market_exit_mass_cancel(
        venue: Option<&str>,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
    ) -> OrderIntent {
        OrderIntent::MassCancel {
            venue: venue.map(|v| v.to_string()),
            symbol: None,
            account: account.cloned(),
        }
    }

    /// The FLATTEN legs of a [`OrderIntent::MarketExit`]: ONE [`OrderIntent::Flatten`] per non-flat
    /// position, PAIRED WITH THE ENGINE INDEX IT WAS READ FROM, walked engine by engine (primary
    /// first, then extras in registration order) and, within an engine, in the `Account`
    /// `positions` IndexMap's own insertion order. `Flatten` itself re-resolves the size and
    /// submits a `reduce_only` MARKET for `|position|` at apply time.
    ///
    /// ⚠ **THE INDEX IS HALF THE LEG, and it used to die on the line that built one.** A
    /// `Flatten` names a venue and a symbol and nothing else, so two accounts of one exchange
    /// holding a position in one symbol produced BYTE-IDENTICAL intents — and downstream
    /// `vike_exec::RouteKey::sole_account_of` resolves a venue string to that venue's DEFAULT
    /// account. Both legs landed on the default book: it was flattened twice (its own size
    /// re-resolved each time, so the second leg REVERSED it), and the second account was never
    /// flattened at all. The caller lowers each leg with
    /// [`super::EngineRoute::Engine`], so the order acts on the book the position was read out of.
    ///
    /// `route` is the exit's own route and it scopes the WALK through
    /// [`Self::exit_scope_engines`]: an unscoped exit reads every engine, a venue-scoped one
    /// reads every engine of that exchange, and an exit a labelled mount issued against its own
    /// venue reads that mount's engine alone — a strategy's exit has no business closing another
    /// account's position.
    ///
    /// This function READS ONLY (`&self`) — it mints nothing, sends nothing, and touches no
    /// engine. That is what makes the compound verb replay-deterministic without a journal record
    /// of its own: a replay that folded the same record prefix holds the same positions, so this
    /// returns the same intent list in the same order, so the same coids are minted downstream.
    ///
    /// **Called AFTER the mass-cancel has been applied**, never before — see the
    /// [`OrderIntent::MarketExit`] arm of [`Self::apply_intent`] for why (the mass-cancel's own
    /// `pump_client` can fold a fill that OPENS a position on a symbol that was flat a moment
    /// earlier; a list snapshotted before the cancel would have no leg for it and the exit would
    /// leave that position on).
    ///
    /// SCOPE NOTE (hedge mode): only `position_side == "BOTH"` legs are expanded, because
    /// `ExecutionEngine::position_size_of` — the size `Flatten` resolves through — is keyed
    /// `(venue, symbol, "BOTH")`. A hedge-mode LONG/SHORT leg would expand into a `Flatten` that
    /// resolves 0.0 and no-ops, so it is skipped here rather than emitted as a dead intent. Those
    /// venues need per-leg closing intents, which the primitive vocabulary does not carry yet.
    pub(crate) fn market_exit_flatten_legs(
        &self,
        route: EngineRoute,
        venue: Option<&str>,
    ) -> Vec<(usize, OrderIntent)> {
        let mut out = Vec::new();
        for idx in self.exit_scope_engines(route, venue) {
            let eng_venue = self.eng(idx).venue.clone();
            if let Some(v) = venue
                && eng_venue != v
            {
                continue;
            }
            for ((pv, symbol, side), pos) in self.eng(idx).account.positions.iter() {
                if *side != vike_model::events::PositionSide::Both
                    || pv.as_str() != eng_venue
                    || pos.size == 0.0
                {
                    continue;
                }
                out.push((
                    idx,
                    OrderIntent::Flatten {
                        venue: pv.to_string(),
                        symbol: symbol.to_string(),
                        account: None,
                    },
                ));
            }
        }
        out
    }

    /// PURE expansion of the compound [`OrderIntent::MarketExit`] ("get me out") into the existing
    /// primitive intents, in deterministic order: the mass-cancel leg
    /// ([`Self::market_exit_mass_cancel`]) followed by the flatten legs
    /// ([`Self::market_exit_flatten_legs`]) *as of right now*.
    ///
    /// INSPECTION/TEST HELPER ONLY. The live arm does NOT apply this list wholesale — it applies
    /// the mass-cancel, then RE-derives the flatten legs from the post-cancel state (see the
    /// `MarketExit` arm). Snapshotting the whole plan up front would miss a position opened by a
    /// fill the mass-cancel's own pump folded.
    ///
    /// Expands the EXTERNAL command path ([`EngineRoute::Payload`]) — the operator's panic button,
    /// which is what the replay-determinism comparison folds — and drops each leg's engine index,
    /// because the property it exists to show is that two cores mint the same INTENT LIST. Which
    /// book each leg lands on is [`Self::market_exit_flatten_legs`]'s own assertion.
    #[cfg(test)]
    pub(crate) fn expand_market_exit(&self, venue: Option<&str>) -> Vec<OrderIntent> {
        let mut out = vec![Self::market_exit_mass_cancel(venue, None)];
        out.extend(
            self.market_exit_flatten_legs(EngineRoute::Payload, venue).into_iter().map(|(_, i)| i),
        );
        out
    }

    /// Lower one [`OrderIntent`] onto the engine apply surface. `now` stamps the target engine's
    /// clock. Returns coids of orders SUBMITTED by this intent (see module + call sites).
    ///
    /// Says nothing about WHY any cancel this intent lowers is being issued, so those cancels reach
    /// the venue as [`CancelIntent::Unspecified`] — the flatten-safe value, which no venue may hold
    /// back. That is the right answer for the EXTERNAL command path (a DOM click, a tradehub
    /// ticket, a CLI verb): the operator's intent is genuinely unknown here, and guessing "routine"
    /// on their behalf could shed a cancel they meant as an exit. A caller that DOES know uses
    /// [`Self::apply_intent_with_cancel_intent`].
    pub(crate) fn apply_intent(&mut self, intent: OrderIntent, now: i64) -> Vec<String> {
        self.apply_intent_with_cancel_intent(intent, now, CancelIntent::Unspecified)
    }

    /// [`Self::apply_intent_with_cancel_intent`] routed by the PAYLOAD's venue — the external
    /// command path, and the only routing an operator ticket, a DOM click or a CLI verb can carry.
    pub(crate) fn apply_intent_with_cancel_intent(
        &mut self,
        intent: OrderIntent,
        now: i64,
        cancel_intent: CancelIntent,
    ) -> Vec<String> {
        self.apply_intent_routed(intent, now, cancel_intent, EngineRoute::Payload)
    }
}

#[path = "tests/apply.rs"]
#[cfg(test)]
mod apply_tests;
