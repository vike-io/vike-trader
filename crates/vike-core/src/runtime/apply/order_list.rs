//! `lower_order_list`: the [`OrderIntent::OrderList`] arm of `apply_intent_routed` (wave 4 of the
//! order kinds). A list (an OCO pair, an OTO chain, an OUO group, a ladder: `vike_model::OrderList`)
//! names ONE venue, ONE symbol and ONE account, so every leg lands on one engine by construction
//! (decision 0254: every leg of a group on one account). It is lowered as `lower_bracket` lowers a
//! bracket:
//!
//! 1. the spec's account resolves first, an ambiguous venue is refused, and a list whose links the
//!    contingency book could not resolve, or whose legs name only some of their order ids
//!    (`vike_model::build_order_list`'s words), is refused, all BEFORE an order client id is minted, so no sequence
//!    gap is left;
//! 2. one order client id per leg, minted, or the ones the legs name (a client's retry-safe list: an id this node
//!    already holds refuses the list WHOLE, nothing sent, in a single submit's words for a reused id),
//!    each leg journaled as a `MintedSubmit` (the write-ahead record carries the spec, and that frame
//!    raised the journal to `vike_journal`'s order-list format, so a build that cannot read it refuses
//!    the journal: ruling 1 of wave 4);
//! 3. the ATOMIC preflight (decision 0219): every leg is judged before any is sent, each by
//!    `vike_venues::preflight_order_at` and then the list whole by `vike_venues::order_list_refusal` (an
//!    exit the row cannot rest reduce-only, an OUO leg the venue cannot resize, a venue whose exits must
//!    cover a whole position). One refused leg refuses EVERY leg, each a terminal reject in words, and
//!    nothing is sent. A stated `paper` engine is not asked the list question, as `preflight_order_at`
//!    does not ask it the reduce-only kinds (decision 0201: its book holds and resizes the exits itself);
//! 4. what passes is placed as a bracket is (`place_linked_group`): every leg's links in the
//!    `ContingencyBook`, every leg with an unfilled parent HELD off the venue, the rest sent plain.
//!
//! Cold: one command, never the per-message fold. The fills it causes are driven by
//! `apply/contingency.rs` (an OUO leg's fill resizes its siblings through the amend of `apply/amend.rs`).

use super::*;
use vike_model::orders::client_id::{OrderClientIdLabels, OrderClientIdLegRole};

impl<C: ExecutionClient> CoreThread<C> {
    /// Lower one order list (this module's doc). Returns every leg's order client id once they are minted, refused
    /// or placed, and nothing when it refused before the mint.
    pub(super) fn lower_order_list(
        &mut self,
        spec: vike_model::OrderList,
        now: i64,
        route: EngineRoute,
    ) -> Vec<String> {
        let route = match spec.account.as_ref() {
            None => route,
            Some(account) => match self.route_for_payload_account(route, &spec.venue, account) {
                Some(resolved) => resolved,
                None => {
                    self.refuse_unheld_account("order list", &spec.venue, account);
                    self.pump_client();
                    return Vec::new();
                }
            },
        };
        if let Some(candidates) = self.ambiguous_accounts(route, &spec.venue) {
            self.refuse_ambiguous("order list", &spec.venue, &candidates);
            self.pump_client();
            return Vec::new();
        }
        // Validate before the mint: the list's own words for links the book could not resolve, or for
        // legs that name only some of their order ids.
        let named = spec.client_order_client_ids();
        let probe: Vec<String> = match &named {
            Ok(Some(named)) => named.clone(),
            _ => (0..spec.legs.len()).map(|i| i.to_string()).collect(),
        };
        if let Err(why) = named.clone().and_then(|_| vike_model::build_order_list(&spec, &probe)) {
            self.note(format!(
                "order list on {} {} REFUSED, nothing was sent: {why}",
                spec.venue, spec.symbol
            ));
            self.pump_client();
            return Vec::new();
        }
        let routed = self.route_of(route, &spec.venue);
        let engine_index = routed.unwrap_or(0);
        let order_client_ids: Vec<String> = match named {
            // A list that names its legs' ids is a client's retry-safe list: one id this node already
            // holds refuses it WHOLE, nothing sent, as a single submit's reused id is refused.
            Ok(Some(named)) => {
                if let Some(reused) =
                    named.iter().find(|c| self.order_id_in_use(engine_index, c)).cloned()
                {
                    self.refuse_order_list_order_client_id(engine_index, &spec, &reused);
                    self.pump_client();
                    return Vec::new();
                }
                named
            }
            // ONE stem for the list on the routed engine's plan (decision 0270), each leg the stem plus its
            // intent and its `OrderClientIdLegRole`; a list with more than 35 legs of one role takes one plain id per leg.
            // A v1 session mints each leg whole, in leg order, as before.
            _ => match OrderClientIdLegRole::of(&spec.legs) {
                Some(roles) => {
                    let stem = self.mint_stem(route, engine_index);
                    spec.legs
                        .iter()
                        .zip(roles)
                        .map(|(leg, role)| {
                            let intent = self.order_client_id_intent(
                                engine_index,
                                leg.side,
                                leg.reduce_only,
                                &spec.symbol,
                            );
                            stem.leg(&mut self.order_client_id_gen, intent, role)
                        })
                        .collect()
                }
                None => {
                    let (src, strat) = self.order_client_id_who(route);
                    spec.legs
                        .iter()
                        .map(|leg| {
                            let intent = self.order_client_id_intent(
                                engine_index,
                                leg.side,
                                leg.reduce_only,
                                &spec.symbol,
                            );
                            self.order_client_id_plans[engine_index].mint(
                                &mut self.order_client_id_gen,
                                OrderClientIdLabels::new(src, strat, intent),
                            )
                        })
                        .collect()
                }
            },
        };
        // `lower_bracket`'s rule: a held leg is released long after the caller that knew the account.
        if self.order_client_id_row_needed(engine_index) {
            for c in &order_client_ids {
                self.order_client_id_venue.insert(c.clone(), engine_index);
            }
        }
        let mut orders = vike_model::build_order_list(&spec, &order_client_ids)
            .expect("the probe above built this list");
        for o in orders.iter_mut() {
            o.timestamp = now;
        }
        let rk = self.journal_route_key(engine_index, &spec.venue);
        if let Some(journal) = self.journal.as_mut() {
            for o in &orders {
                journal.append_minted_submit(now, o, rk.as_deref()).expect("journal append");
                self.journaled_since_snap += 1;
            }
        }
        self.engine_mut(engine_index).now_ms = now;
        if self.is_retired(engine_index) {
            for o in &orders {
                self.refuse_for_stopped_account(engine_index, o, now);
            }
            self.pump_client();
            return order_client_ids;
        }
        let tier = self.engine(engine_index).stated_tier();
        let refusal = {
            let capabilities_venue = self.capabilities_venue(routed, &spec.venue);
            let leg = orders.iter().enumerate().find_map(|(i, o)| {
                vike_venues::preflight_order_at(o, capabilities_venue, tier)
                    .err()
                    .map(|d| (Some(i), d.to_string()))
            });
            leg.or_else(|| {
                if tier == Some(vike_model::AccountTier::Paper) {
                    return None;
                }
                vike_venues::order_list_refusal(capabilities_venue, &orders).map(|r| (None, r))
            })
            .map(|(i, r)| (i, r, capabilities_venue.to_string()))
        };
        if let Some((culprit, reason, venue)) = refusal {
            // A leg its own preflight refused carries the reason and its siblings name it; a list the
            // venue refuses whole carries the list's words on every leg.
            let sibling_reason = culprit.map(|ci| {
                format!(
                    "ORDER_LIST_ATOMIC_REFUSED: culprit={} leg={ci} venue={venue}",
                    orders[ci].order_client_id
                )
            });
            for (i, o) in orders.iter().enumerate() {
                let r = match &sibling_reason {
                    Some(s) if culprit != Some(i) => s,
                    _ => &reason,
                };
                self.synthesize_capability_reject(
                    engine_index,
                    o,
                    r,
                    vike_model::events::refusal_id_of(
                        vike_model::OrderRefusalId::VenueCapability.word(),
                    ),
                    now,
                );
            }
            self.note(format!(
                "order list of {} legs on {venue} {} REFUSED WHOLE, nothing was sent: {reason}",
                orders.len(),
                spec.symbol
            ));
            self.pump_client();
            return order_client_ids;
        }
        self.place_linked_group(engine_index, routed, orders, "order list leg", now);
        order_client_ids
    }

    /// Does this node already hold an order under `order_client_id` on engine `engine_index`: in the registry (whatever its
    /// status: a list's retry finds its first entry still unanswered), held until its parent fills, in
    /// the contingency book, or held for its release? Or does it REMEMBER one from before its last
    /// restart (the late-retry memory, decision 0270's D7)? `lower_combo` asks it of a combo's given id,
    /// `lower_bracket` of a sender's stem's legs, and `apply/amend.rs` of a replacement's `R<n>`.
    pub(crate) fn order_id_in_use(&self, engine_index: usize, order_client_id: &str) -> bool {
        self.engine(engine_index).registry.contains_key(order_client_id)
            || self.held_orders.contains_key(order_client_id)
            || self.contingency.contains(order_client_id)
            || self.held_releases.contains_key(order_client_id)
            || self.recent_order_client_ids.contains(order_client_id)
    }

    /// Refuse the list `spec` WHOLE because its leg's id `order_client_id` is in use: that id in a single
    /// submit's words for it ([`Self::refuse_order_id_in_use`]), and the list's line.
    #[cold]
    #[inline(never)]
    fn refuse_order_list_order_client_id(
        &mut self,
        engine_index: usize,
        spec: &vike_model::OrderList,
        order_client_id: &str,
    ) {
        self.refuse_order_id_in_use(engine_index, order_client_id);
        self.note(format!(
            "order list of {} legs on {} {} REFUSED WHOLE, nothing was sent: its order id {order_client_id} is \
             already used",
            spec.legs.len(),
            spec.venue,
            spec.symbol
        ));
    }

    /// A single submit's words for an id [`Self::order_id_in_use`] found (`refuse_held_order_client_id`,
    /// `refuse_held_release_order_client_id`, `refuse_recent_order_client_id`, or a held leg's line); the group's own line is
    /// its caller's.
    #[cold]
    #[inline(never)]
    pub(super) fn refuse_order_id_in_use(&mut self, engine_index: usize, order_client_id: &str) {
        match self.engine(engine_index).registry.get(order_client_id).map(|mo| mo.status) {
            Some(status) => self.refuse_held_order_client_id(order_client_id, status),
            None if self.held_releases.contains_key(order_client_id) => {
                self.refuse_held_release_order_client_id(order_client_id)
            }
            None if !self.held_orders.contains_key(order_client_id)
                && !self.contingency.contains(order_client_id) =>
            {
                self.refuse_recent_order_client_id(order_client_id);
            }
            None => {
                let message = format!(
                    "order {order_client_id} REFUSED, nothing sent: order client id {order_client_id} is already used by a leg this \
                     node holds until its parent fills: pick a new order client id"
                );
                tracing::warn!(target: "vike_core::core", order_client_id = %order_client_id, "{message}");
                self.note(message);
            }
        }
    }
}
