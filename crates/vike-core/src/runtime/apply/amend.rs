//! The ONE amend of a resting order, above the bridges: `OrderIntent::Modify` of a live order, whatever
//! the venue does underneath. Decision 0244 is the record; `vike_venues::venue_amend_mode` is the table
//! that says, per venue and per order kind, which of the three ways applies; `vike_execution::amend` is the
//! pure state machine this file carries out.
//!
//! * **Native** — the venue amends in place: the engine's `modify_order` as it always was (risk gate,
//!   grid snap, `ExecutionClient::modify`, the venue's own `OrderModified` or non-terminal
//!   `OrderModifyRejected`). Nothing here touches it.
//! * **Cancel-then-new** — the venue cannot (a binance perpetual STOP is an Algo Service order, a
//!   polymarket order has no amend at all). The node places a NEW order with the changed terms and
//!   cancels the old one: a REDUCE-ONLY stop is placed FIRST and the old one is cancelled once the new
//!   one is answered (the position is never without a stop); anything else is cancelled first and
//!   placed when the venue says it is gone (a limit order loses its place in the queue, and the
//!   words say so). The ordering is read off the REPLACEMENT: a fired stop-limit is replaced by the
//!   plain limit it released, which goes cancel-first. The replacement is a fresh order in every
//!   respect the rest of the node cares about (a minted order client id, the standard submit tail, the mount's
//!   tag and ownership); its order client id is the old one's next generation `…R<n>` where the engine's shape
//!   carries one (decision 0270: `CoreThread::order_client_id_successor` hands it to the mint), and
//!   `OrderRecord::replaces` keeps the OPERATOR'S first order client id as the name of the logical order, so a
//!   cancel or modify that names it reaches the live member of the chain ([`CoreThread::amend_head`]).
//! * **Refused** — an engine/venue that cannot amend refuses in words, which is the engine's own
//!   refusal (`Engine::modify_order`); this file adds none.
//!
//! # Where it is driven
//!
//! A modify is a COMMAND: [`CoreThread::modify_resting`] runs when it is applied. The second half of an
//! amend waits for the venue, so it is driven at the drain-loop BOUNDARY ([`CoreThread::drive_amends`],
//! once per drained batch and only while an amend is in flight), reading the registry's status of the two
//! orders. No venue message is looked at and nothing is added to the fold: the `run` loop's one added
//! read is `self.amends.is_empty()` beside the timer wheel's, per pass and not per message.
//!
//! ⚠ **A REPLAY core never places a replacement at its own boundary** (`CoreConfig::replay_core`): its
//! batches are not the live core's, so its `drive_amends` reads no clock (a `JournalReplayQueueClock` read takes the
//! next record's stamp) and leaves a cancel-first amend waiting. The journal drives it:
//! [`CoreThread::place_journaled_replacement`] places the replacement at its `MintedSubmit` (the one naming
//! the order it `replaces`), at that record's stamp, where the live boundary placed it. A new-first one
//! was already placed inside the Modify the replay re-applied, so its record changes nothing there.
//!
//! # What is journaled
//!
//! Nothing new. The Modify command is journaled as it always was; the replacement rides the standard
//! submit path, so its order client id is minted and journaled by `journal_minted_submit`, whose `MintedSubmit` names
//! the order it `replaces` (read off `Engine::replacing`): the ONE record of a replacement, for an
//! operator's order and a mount's alike. A mount's is lowered by `lower_strategy_intent`, which writes no
//! write-ahead `StrategySubmit`: a replay re-applying one would place the order a second time. A restore
//! gives the replacement its predecessor's owner off that key. Its lifecycle is the venue's own journaled
//! events. The
//! lineage lives on the registry entry (`OrderRecord::replaces`, absent when `None`, so a snapshot and
//! its hash are unchanged for every order never replaced). The in-flight table below is core-local, like
//! the other cold tables beside it: after a restart in the middle of an amend the state is read back
//! from the venue (both orders rest, both are reduce-only for a stop, so the position stays covered).

use super::*;
use vike_execution::amend::{AmendAct, AmendEnd, AmendOrderStatus, AmendPlacement, AmendReplace};
use vike_model::events::OrderModifyRejected;
use vike_model::orders::client_id::next_generation;
use vike_venues::venue_amend_mode::{AmendKind, AmendMode, amend_words};

/// Who placed the order being amended, so its replacement is placed the same way.
#[derive(Debug, Clone)]
pub(crate) enum AmendRoute {
    /// An operator's order, or any order no mount minted: placed again on the engine it rests on.
    Engine,
    /// A strategy mount's order: placed through the mount's own intent door, so the order client id is
    /// attributed to the mount and owned under its tag. `key` is the tag's key in `strategy_tags`.
    Mount { index: usize, tag: Option<String>, key: Option<String> },
}

/// One cancel-then-new amend in flight. Core-local: never hashed, snapped or journaled; empty at boot.
#[derive(Debug)]
pub(crate) struct AmendInFlight {
    /// The order client id the OPERATOR first named: the name of the logical order across every replacement.
    root: String,
    /// The live order being replaced.
    old: String,
    /// The replacement's order client id, once it is placed.
    new: Option<String>,
    /// The engine both orders rest on.
    engine_index: usize,
    route: AmendRoute,
    /// The replacement as it will be placed: no order client id yet, no contingency links.
    body: OrderRequest,
    fsm: AmendReplace,
    /// Someone cancelled the logical order while the amend was in flight: end it, place nothing.
    withdrawn: bool,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The live member of the chain `order_client_id` belongs to when it is NOT `order_client_id` itself, else `None`.
    ///
    /// An operator names the order by the order client id they were given. After a cancel-then-new amend that
    /// order client id is a terminal order and its replacement carries a new one, linked by
    /// `OrderRecord::replaces`; a command naming ANY member of the chain therefore reaches the live
    /// one. Cold: the registry scan runs only when `order_client_id` itself is not live, which for an
    /// order that was never replaced is the "unknown or finished" answer it has always been.
    pub(crate) fn amend_head(&self, engine_index: usize, order_client_id: &str) -> Option<String> {
        let engine = self.engine(engine_index);
        match engine.registry.get(order_client_id) {
            // live: it is the head. Finished and never part of a chain: there is nothing to follow.
            Some(mo) if mo.status.is_live() || mo.replaces.is_none() => return None,
            _ => {}
        }
        let root = engine
            .registry
            .get(order_client_id)
            .and_then(|mo| mo.replaces.as_deref())
            .unwrap_or(order_client_id);
        engine
            .registry
            .iter()
            .rev()
            .find(|(_, mo)| mo.replaces.as_deref() == Some(root) && mo.status.is_live())
            .map(|(c, _)| c.clone())
    }

    /// Is a cancel-then-new amend of the order `order_client_id` (its old or its new order) in flight?
    pub(crate) fn amend_in_flight(&self, order_client_id: &str) -> bool {
        self.amends
            .iter()
            .any(|a| a.old == order_client_id || a.new.as_deref() == Some(order_client_id))
    }

    /// The first order client id of the chain `order_client_id` belongs to (itself when it was never replaced).
    fn amend_root(&self, engine_index: usize, order_client_id: &str) -> String {
        self.engine(engine_index)
            .registry
            .get(order_client_id)
            .and_then(|mo| mo.replaces.clone())
            .unwrap_or_else(|| order_client_id.to_string())
    }

    /// The `Modify` arm's LIVE-order half (a held bracket exit is edited in place by the caller before
    /// this is reached). Takes the mode from the venue table through the engine and acts on it:
    /// cancel-then-new starts an amend, every other answer is the engine's native path unchanged.
    /// `new_trigger_price` rides both: the replacement is placed at it, and a native amend sends it
    /// (`Engine::modify_order_with_trigger`, whose client refuses in words a trigger it
    /// cannot move).
    pub(super) fn modify_resting(
        &mut self,
        order_client_id: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
        now: i64,
    ) {
        let engine_index = self.order_client_id_venue.get(&order_client_id).copied().unwrap_or(0);
        // An operator's CORE-HELD stop is named by its arm id and has no order in the registry: the engine
        // would take the modify for an unknown order client id and say nothing. Say what to do instead.
        if !self.emulated.cond_engine.is_empty()
            && self.emulated.cond_engine.get(&order_client_id).is_some_and(|a| a.reduce_only)
        {
            self.refuse_amend(
                engine_index,
                &order_client_id,
                format!(
                    "{order_client_id} is a stop this node holds itself (its venue rests none): it is \
                     moved by `cancel {order_client_id}` and a new stop, not by a modify"
                ),
                now,
            );
            return;
        }
        let head = self.amend_head(engine_index, &order_client_id).unwrap_or(order_client_id);
        self.engine_mut(engine_index).now_ms = now;
        let (mode, moves_trigger_of_kind) =
            self.engine(engine_index).registry.get(&head).map_or((None, false), |mo| {
                let engine = self.engine(engine_index);
                if new_trigger_price.is_some() {
                    let kind = AmendKind::of(&mo.request.order_type);
                    (
                        engine.trigger_amend_mode_of(&mo.request),
                        kind.is_some_and(|k| k != AmendKind::Limit),
                    )
                } else {
                    (engine.amend_mode_of(&mo.request), false)
                }
            });
        // A trigger move the venue table refuses (decision 0244 item 8: an adapter that amends the kind
        // in place but whose trigger move is unproven) is refused HERE: the engine's native path would
        // hand it to a client that may send it. A limit's trigger falls through to the engine's words.
        if moves_trigger_of_kind && mode == Some(AmendMode::Refused) {
            let venue = self.engine(engine_index).venue.clone();
            self.refuse_amend(
                engine_index,
                &head,
                format!(
                    "{venue}: {}",
                    vike_execution::execution_engine::TRIGGER_AMEND_UNSUPPORTED_REASON
                ),
                now,
            );
            return;
        }
        if mode == Some(AmendMode::CancelThenNew) {
            self.begin_replace(engine_index, head, new_qty, new_price, new_trigger_price, now);
            return;
        }
        let mut outbox = EventBusOutbox::default();
        self.engine_mut(engine_index).modify_order_with_trigger(
            &head,
            new_qty,
            new_price,
            new_trigger_price,
            now,
            &mut outbox,
        );
        // A RiskGate veto publishes a NON-TERMINAL `OrderModifyRejected` (the order keeps its terms), so
        // this drives the same outbox path every other intent does — the rejection reaches the event
        // stream, the recent-events ring and any mounted strategy instead of being dropped on the floor.
        self.publish_and_drive_outbox(engine_index, outbox, now);
    }

    /// Say a modify cannot start, in words: a non-terminal `OrderModifyRejected` on `order_client_id` and a note.
    fn refuse_amend(
        &mut self,
        engine_index: usize,
        order_client_id: &str,
        words: String,
        now: i64,
    ) {
        tracing::warn!(target: "vike_core::core", order_client_id = %order_client_id, "{words}");
        self.note(format!("AMEND {order_client_id} refused: {words}"));
        let mut outbox = EventBusOutbox::default();
        outbox.publish(Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order_client_id.to_string(),
            reason: words.into(),
            timestamp: now,
        }));
        self.publish_and_drive_outbox(engine_index, outbox, now);
    }

    /// Refuse a cancel-first amend of a bracket's leg `head` (`old_request` is its request), in words.
    fn refuse_cancel_first_leg(
        &mut self,
        engine_index: usize,
        head: &str,
        old_request: &OrderRequest,
        now: i64,
    ) {
        self.refuse_amend(
            engine_index,
            head,
            format!(
                "{} cannot change a {} in place and this order is a leg of a bracket: cancel the \
                 bracket's leg and place the new one by hand",
                old_request.venue,
                old_request.order_type.as_str()
            ),
            now,
        );
    }

    /// Start the cancel-then-new amend of the live order `head`.
    fn begin_replace(
        &mut self,
        engine_index: usize,
        head: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
        now: i64,
    ) {
        let root = self.amend_root(engine_index, &head);
        if self.amends.iter().any(|a| a.root == root) {
            self.refuse_amend(
                engine_index,
                &head,
                format!("an amend of order {root} is already in flight; wait for it to end"),
                now,
            );
            return;
        }
        if new_qty.is_none() && new_price.is_none() && new_trigger_price.is_none() {
            self.refuse_amend(
                engine_index,
                &head,
                "a modify names neither a quantity nor a price".into(),
                now,
            );
            return;
        }
        let Some(old_request) =
            self.engine(engine_index).registry.get(&head).map(|mo| mo.request.clone())
        else {
            return;
        };
        // A cancel-first amend of a bracket's leg would break the group's link (the leg is cancelled,
        // and its replacement is not the leg the siblings name). A reduce-only stop goes new-first and
        // the book is re-keyed to the replacement, which keeps the link.
        if AmendPlacement::of(&old_request) == AmendPlacement::CancelFirst
            && self.contingency.contains(&head)
        {
            self.refuse_cancel_first_leg(engine_index, &head, &old_request, now);
            return;
        }
        let mut outbox = EventBusOutbox::default();
        let planned = self.engine_mut(engine_index).plan_replacement(
            &head,
            new_qty,
            new_price,
            new_trigger_price,
            now,
            &mut outbox,
        );
        let Some(mut body) = planned else {
            self.publish_and_drive_outbox(engine_index, outbox, now);
            return;
        };
        body = strip_contingency_links(body);
        body.order_client_id = String::new();
        body.timestamp = now;
        // The REPLACEMENT decides the ordering and the words, not the order it replaces:
        // `plan_replacement` replaces a FIRED stop-limit or limit-if-touched by the plain limit it
        // released, and placing that new-first would rest two reduce-only limits at once (an exit
        // doubled). Every other replacement keeps the old order's kind and flag, so this is the
        // ordering judged above; only a released kind can arrive here cancel-first on a bracket's leg.
        let placement = AmendPlacement::of(&body);
        if placement == AmendPlacement::CancelFirst && self.contingency.contains(&head) {
            self.refuse_cancel_first_leg(engine_index, &head, &old_request, now);
            return;
        }
        let route = self.amend_route_of(&head);
        let kind = AmendKind::of(&body.order_type).unwrap_or(AmendKind::Limit);
        let terms = replacement_terms(&body);
        self.note(format!(
            "AMEND {root} (new {terms}): {}",
            amend_words(AmendMode::CancelThenNew, kind, body.reduce_only)
        ));
        let mut a = AmendInFlight {
            root,
            old: head,
            new: None,
            engine_index,
            route,
            body,
            fsm: AmendReplace::begin(placement, now),
            withdrawn: false,
        };
        match placement {
            AmendPlacement::NewFirst => {
                if self.place_replacement(&mut a, now).is_none() {
                    self.finish_amend(&a, AmendEnd::NewRefused { old_stands: true }, now);
                    return;
                }
            }
            AmendPlacement::CancelFirst => {
                self.engine_mut(engine_index).cancel_order(&a.old);
            }
        }
        self.amends.push(a);
        self.pump_client();
    }

    /// How the order `order_client_id` was placed: by a mount (its tag and the tag's key) or not.
    pub(crate) fn amend_route_of(&self, order_client_id: &str) -> AmendRoute {
        if let Some((key, _)) =
            self.strategy_tags.iter().find(|(_, v)| v.as_str() == order_client_id)
        {
            // `{mount_index}|{venue}|{symbol}|{tag}`; the tag is the remainder, which may hold a `|`.
            let mut parts = key.splitn(4, '|');
            let index = parts.next().and_then(|i| i.parse::<usize>().ok());
            let tag = parts.nth(2).map(str::to_string);
            if let (Some(index), Some(tag)) = (index, tag) {
                return AmendRoute::Mount { index, tag: Some(tag), key: Some(key.clone()) };
            }
        }
        match self.order_client_id_mount.get(order_client_id) {
            Some(&index) => AmendRoute::Mount { index, tag: None, key: None },
            None => AmendRoute::Engine,
        }
    }

    /// Place the replacement through the standard submit path and record its lineage. `None` when the
    /// submit refused before an order client id existed (no account, ambiguous route): the refusal is in the notes.
    fn place_replacement(&mut self, a: &mut AmendInFlight, now: i64) -> Option<String> {
        let body = a.body.clone();
        // The replacement's id is the old one's NEXT GENERATION (`…R<n+1>`, decision 0270) through the mint
        // path: `lower_submit`'s mint takes this pending successor, so it is journaled as every minted id is.
        // Cleared below whatever the submit did (a refusal before the mint leaves it untaken).
        self.order_client_id_successor = self.amend_successor(a.engine_index, &a.old);
        // The engine's spot rule must not count the order this one replaces as a second sell: placed new-first,
        // both are live until the old one's cancel is answered (`Engine::replacing`).
        self.engine_mut(a.engine_index).replacing = Some(a.old.clone());
        // ...nor the other legs of the group the old one is a leg of at their sum
        // (`vike_execution::ContingencyBook::linked_exits`).
        let linked = self.contingency.linked_exits(&a.old);
        self.engine_mut(a.engine_index).linked_exits = linked;
        let order_client_ids = match &a.route {
            AmendRoute::Engine => self.apply_intent_routed(
                OrderIntent::Submit(Box::new(body)),
                now,
                OrderCancelIntent::Unspecified,
                EngineRoute::Engine(a.engine_index),
            ),
            // No `StrategySubmit`: a replay places the replacement off the Modify it re-applies (new-first)
            // or off its `MintedSubmit` (cancel-first), and a re-applied record of its own would place it twice.
            AmendRoute::Mount { index, tag, .. } => self.lower_strategy_intent(
                OrderIntent::Submit(Box::new(body)),
                now,
                *index,
                OrderCancelIntent::Unspecified,
                tag.as_slice(),
            ),
        };
        self.order_client_id_successor = None;
        self.engine_mut(a.engine_index).replacing = None;
        self.engine_mut(a.engine_index).linked_exits = None;
        let new = order_client_ids.into_iter().next()?;
        // Every member of the chain names its root, the first order included (it names itself), so a
        // finished order that was NEVER replaced is told apart from one that was without a scan.
        for member in [&a.old, &new] {
            if let Some(mo) = self.engine_mut(a.engine_index).registry.get_mut(member) {
                mo.replaces = Some(a.root.clone());
            }
        }
        // The mount's tag now names the replacement (the old order client id's terminal retires nothing: that
        // removal is value-guarded). A failed amend puts it back (`finish_amend`).
        if let AmendRoute::Mount { key: Some(key), .. } = &a.route {
            self.strategy_tags.insert(key.clone(), new.clone());
        }
        a.new = Some(new.clone());
        Some(new)
    }

    /// The id the replacement of `old` on engine `engine_index` takes: `old`'s next generation under the engine's
    /// replacement cap ([`vike_model::orders::client_id::next_generation`]), skipping one an earlier
    /// attempt left in use (`order_id_in_use`). `None` (a fresh mint) for a v1 id, a shape without `R`,
    /// or past the cap; `OrderRecord::replaces` links the chain either way. A pure read of the registry,
    /// so a replay core derives the same id.
    fn amend_successor(&self, engine_index: usize, old: &str) -> Option<String> {
        let max_gen = self.order_client_id_plans[engine_index].shape().max_gen;
        let mut next = next_generation(old, max_gen)?;
        while self.order_id_in_use(engine_index, &next) {
            next = next_generation(&next, max_gen)?;
        }
        Some(next)
    }

    /// Someone cancelled the logical order `order_client_id` (an operator's cancel, a batch cancel, a mass
    /// cancel): an amend in flight on it must not place a new order after the old one is gone.
    /// `order_client_id = None` withdraws every amend (a mass cancel or flatten names no order client id).
    pub(crate) fn withdraw_amends(&mut self, order_client_id: Option<&str>) {
        for a in self.amends.iter_mut() {
            let named = order_client_id
                .is_none_or(|c| c == a.root || c == a.old || a.new.as_deref() == Some(c));
            if named {
                a.withdrawn = true;
            }
        }
    }

    /// The drain-loop boundary's driver: advance every amend in flight one pass. The caller has
    /// gated on `!self.amends.is_empty()`. A replay core places no replacement here (module doc).
    pub(crate) fn drive_amends(&mut self, now: i64) {
        self.dirty = true; // the registry and the notes may move: publish them
        let may_place = !self.config.replay_core;
        let mut amends = std::mem::take(&mut self.amends);
        amends.retain_mut(|a| !self.drive_one(a, now, may_place));
        // an amend started while these ran (none can: begin_replace is a command) would be kept
        amends.append(&mut self.amends);
        self.amends = amends;
    }

    /// REPLAY core: the journal's record of the replacement of `old` (its `MintedSubmit` naming the order
    /// it `replaces`, re-applied by `crates/vike-core/src/replay/refold.rs`'s `replay_tail`) places it HERE,
    /// at that record's stamp `now`, as the live boundary did: the same pass over that one amend, allowed to
    /// place. A replacement already placed (new-first, inside the Modify) is left alone. `false` when no
    /// amend of `old` is in flight.
    pub(crate) fn place_journaled_replacement(&mut self, old: &str, now: i64) -> bool {
        let Some(i) = self.amends.iter().position(|a| a.old == old) else { return false };
        let mut a = self.amends.remove(i);
        if a.new.is_some() || !self.drive_one(&mut a, now, true) {
            self.amends.insert(i, a);
        }
        true
    }

    /// One pass over one amend; `true` when it has ended. `may_place: false` (a replay core's own
    /// boundary) leaves an amend that would place its replacement now exactly as it was.
    fn drive_one(&mut self, a: &mut AmendInFlight, now: i64, may_place: bool) -> bool {
        let engine_index = a.engine_index;
        let status = |this: &Self, order_client_id: &str| {
            AmendOrderStatus::of(
                this.engine(engine_index).registry.get(order_client_id).map(|mo| mo.status),
            )
        };
        if a.withdrawn {
            if let Some(n) = a.new.clone() {
                self.engine_mut(engine_index).cancel_order(&n);
            }
            self.pump_client();
            self.finish_amend(a, AmendEnd::OldGone, now);
            return true;
        }
        let old = status(self, &a.old);
        let new = a.new.as_deref().map_or(AmendOrderStatus::Unknown, |n| status(self, n));
        if a.fsm.take_cancel_advisory(now) {
            self.note(format!(
                "AMEND {}: the venue has not confirmed the cancel of {} yet; the new terms are placed \
                 the moment it does",
                a.root, a.old
            ));
        }
        let before = a.fsm.clone();
        let step = a.fsm.step(old, new, now);
        if step.send == AmendAct::SubmitNew && !may_place {
            a.fsm = before; // the journal's record places it (`place_journaled_replacement`)
            return false;
        }
        let mut end = step.end;
        match step.send {
            AmendAct::Nothing => {}
            AmendAct::SubmitNew => {
                if self.place_replacement(a, now).is_none() {
                    end = Some(AmendEnd::NewRefused { old_stands: false });
                }
            }
            AmendAct::CancelOld => {
                // The replacement is ANSWERED and the old stop is about to go: from here the bracket's
                // book keys the leg by the replacement, so a take-profit fill cancels the stop that is
                // in force. Not before: until it is answered the new order may be refused, and a
                // refusal of a book member reads as "the protective exit died unfilled".
                if step.end.is_none()
                    && let Some(n) = a.new.as_deref()
                {
                    self.contingency.rename(&a.old, n);
                    self.ouo.rekey(&a.old, n);
                }
                self.engine_mut(engine_index).cancel_order(&a.old);
            }
            AmendAct::CancelNew => {
                if let Some(n) = a.new.clone() {
                    self.engine_mut(engine_index).cancel_order(&n);
                }
            }
        }
        if step.send != AmendAct::Nothing {
            // fold the events an in-process client (the paper exchange) queued for what was just sent
            self.pump_client();
        }
        match end {
            Some(end) => {
                self.finish_amend(a, end, now);
                true
            }
            None => false,
        }
    }

    /// An amend has ended: tell the operator in words, put the bracket's book right, and leave the old
    /// order standing wherever it can.
    fn finish_amend(&mut self, a: &AmendInFlight, end: AmendEnd, now: i64) {
        let new = a.new.as_deref().unwrap_or("");
        let reason = a
            .new
            .as_deref()
            .and_then(|n| self.engine(a.engine_index).rejection_reason(n))
            .map(str::to_string);
        let words = end.words(&a.old, new, reason.as_deref());
        // A bracket leg is re-keyed to the replacement once it is answered (`AmendAct::CancelOld`, above). The
        // book follows the LOGICAL leg: driven as the terminal the leg really had when the amend ends
        // with it gone. Before the re-key the old order is still the book's leg and its own events
        // drove the book, so there is nothing to put right.
        if !new.is_empty() && self.contingency.contains(new) {
            // The old order's fills after the re-key reached no drive (it left the book): only what of
            // them was not driven yet is driven now, so no fill is spent twice.
            let (filled, px) = self
                .engine(a.engine_index)
                .registry
                .get(&a.old)
                .map_or((0.0, 0.0), |mo| (mo.filled_qty, mo.average_fill_price));
            let undriven = self.ouo.undriven(&a.old, filled);
            match end {
                AmendEnd::OldFilled => self.drive_contingency_on_fill(new, undriven, px, true, now),
                AmendEnd::OldGone | AmendEnd::NewGone | AmendEnd::BothGone => {
                    if undriven > 1e-12 {
                        self.drive_contingency_on_fill(new, undriven, px, false, now);
                    }
                    self.drive_contingency_on_terminal(new);
                }
                AmendEnd::Replaced
                | AmendEnd::OldNotCancelled
                | AmendEnd::CancelUnconfirmed
                | AmendEnd::NewRefused { .. }
                | AmendEnd::NewSilent { .. } => {
                    if undriven > 1e-12 {
                        self.drive_contingency_on_fill(new, undriven, px, false, now);
                    }
                }
            }
        }
        if !end.old_stands() {
            self.ouo.forget_order(&a.old);
        }
        // An OUO leg's size moved again while this amend was in flight: one more resize, to it.
        self.resize_after_amend(&a.old, new, now);
        // The old order stands: the mount's tag names it again (it was pointed at the replacement when
        // that was placed, and the replacement's refusal retires a tag that names IT).
        if end.old_stands()
            && let AmendRoute::Mount { key: Some(key), .. } = &a.route
            && end != AmendEnd::OldNotCancelled
        {
            self.strategy_tags.insert(key.clone(), a.old.clone());
        }
        if end.succeeded() {
            tracing::info!(target: "vike_core::core", root = %a.root, old = %a.old, new = %new, "amend: {words}");
            self.note(format!("AMEND {}: {words}", a.root));
            return;
        }
        // A failed amend is a NON-terminal rejection of the modify, on the order that still stands
        // (or, when none does, on the name the operator used), never a silent loss.
        let on = if end.old_stands() { a.old.as_str() } else { a.root.as_str() };
        self.refuse_amend(a.engine_index, on, words, now);
    }
}

/// The replacement's terms on the amend's note, naming the price a modify of its kind moves
/// (`vike_model::modified_price_is_trigger`): a limit's price; a stop-limit's or a take-profit's LIMIT, with
/// the trigger it keeps beside it; every other kind's trigger. `order ls` shows the limit price column and
/// never a trigger, so the line carries the trigger wherever there is one.
fn replacement_terms(body: &OrderRequest) -> String {
    let px = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |x| x.to_string());
    match body.order_type {
        vike_model::OrderType::Limit => format!("price {}, qty {}", px(body.price), body.qty),
        vike_model::OrderType::StopLimit | vike_model::OrderType::TakeProfit => format!(
            "limit {}, trigger {}, qty {}",
            px(body.price),
            px(body.trigger_price),
            body.qty
        ),
        _ => format!("trigger {}, qty {}", px(body.trigger_price), body.qty),
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    use crate::runtime::test_support::core_of;
    use vike_execution::testing::{RecordingClient, engine_on};
    use vike_model::OrderType::{Limit, Stop, StopLimit, TakeProfit};

    fn body(
        order_type: vike_model::OrderType,
        price: Option<f64>,
        trigger: Option<f64>,
    ) -> OrderRequest {
        OrderRequest { order_type, price, trigger_price: trigger, qty: 2.0, ..Default::default() }
    }

    /// A stop-limit's and a take-profit's modify moves the LIMIT, so the note names it (the trigger rides
    /// beside it); a limit's and a stop's lines are the bytes they always were.
    #[test]
    fn the_note_names_the_price_the_modify_moved() {
        let cases = [
            (body(Limit, Some(101.5), None), "price 101.5, qty 2"),
            (body(Stop, None, Some(90_000.0)), "trigger 90000, qty 2"),
            (body(StopLimit, Some(94.5), Some(95.0)), "limit 94.5, trigger 95, qty 2"),
            (body(TakeProfit, Some(105.5), Some(105.0)), "limit 105.5, trigger 105, qty 2"),
            (body(TakeProfit, None, Some(105.0)), "limit -, trigger 105, qty 2"),
        ];
        for (b, want) in cases {
            assert_eq!(replacement_terms(&b), want, "{}", b.order_type);
        }
        for kind in [StopLimit, TakeProfit] {
            assert!(
                !vike_model::modified_price_is_trigger(&kind),
                "{kind}: the line follows the model's rule"
            );
        }
    }

    /// A reduce-only SELL of 2 of `order_type` on deribit's `X`.
    fn exit(
        order_type: vike_model::OrderType,
        price: Option<f64>,
        trigger: Option<f64>,
    ) -> OrderRequest {
        OrderRequest {
            venue: "deribit".into(),
            symbol: "X".into(),
            side: -1,
            reduce_only: true,
            ..body(order_type, price, trigger)
        }
    }

    /// **The ordering is the REPLACEMENT's.** A fired reduce-only stop-limit or limit-if-touched is replaced by
    /// the plain limit it released (A2's `plan_replacement`: kind `limit`, the limit kept, no trigger): read
    /// off the old order that would go new-first and rest two reduce-only limits at once; read off the
    /// replacement it goes cancel-first. Every other replacement keeps the old kind and flag (the plan never
    /// changes them), so its ordering is the old one's, exactly as before.
    #[test]
    fn a_released_limit_is_placed_cancel_first_and_every_other_kind_as_before() {
        let released = exit(Limit, Some(94.5), None);
        for fired in
            [exit(StopLimit, Some(94.5), Some(95.0)), exit(TakeProfit, Some(94.5), Some(95.0))]
        {
            assert_eq!(
                AmendPlacement::of(&fired),
                AmendPlacement::NewFirst,
                "{}",
                fired.order_type
            );
        }
        assert_eq!(
            AmendPlacement::of(&released),
            AmendPlacement::CancelFirst,
            "never two reduce-only limits"
        );
        for (old, want) in [
            (exit(Stop, None, Some(95.0)), AmendPlacement::NewFirst),
            (exit(StopLimit, Some(94.5), Some(95.0)), AmendPlacement::NewFirst),
            (exit(TakeProfit, None, Some(95.0)), AmendPlacement::NewFirst),
            (exit(Limit, Some(94.5), None), AmendPlacement::CancelFirst),
            (
                OrderRequest { reduce_only: false, ..exit(Stop, None, Some(95.0)) },
                AmendPlacement::CancelFirst,
            ),
        ] {
            assert_eq!(AmendPlacement::of(&old), want, "{}", old.order_type);
        }
    }

    /// **The carrier acts on the replacement it places.** A fired (`Triggered`) reduce-only stop-limit amended
    /// through `begin_replace`: the first thing sent, the amend's ordering and its words follow the
    /// replacement `plan_replacement` returned. Without A2's release (this branch) that is a stop-limit,
    /// placed new-first; with it (from the merge of `research/ok1-paper`) it is the released limit, so the old
    /// order is cancelled first and nothing new is sent yet. Each arm asserts its own shape.
    #[test]
    fn the_carrier_acts_on_the_replacement_it_places() {
        let mut c = core_of(
            engine_on("deribit", "X", RecordingClient::default()),
            vec![],
            CoreConfig::default(),
        );
        let req = Box::new(exit(StopLimit, Some(94.5), Some(95.0)));
        let order_client_id = c.apply_intent(OrderIntent::Submit(req), 0).remove(0);
        for event in [
            Event::OrderSubmitted(vike_model::events::OrderSubmitted {
                order_client_id: order_client_id.clone(),
                timestamp: 0,
            }),
            Event::OrderAccepted(vike_model::events::OrderAccepted {
                order_client_id: order_client_id.clone(),
                venue_order_id: Some(format!("v-{order_client_id}").into()),
                timestamp: 0,
            }),
            Event::OrderTriggered(vike_model::events::OrderTriggered {
                order_client_id: order_client_id.clone(),
                timestamp: 1,
            }),
        ] {
            c.dispatch(Ingest::Event(event));
        }
        assert_eq!(c.engine.registry[&order_client_id].status, vike_model::OrderStatus::Triggered);

        c.begin_replace(0, order_client_id.clone(), None, Some(94.0), None, 10);

        let a = c.amends.last().expect("an amend is in flight");
        let sent = &c.engine.client;
        match AmendPlacement::of(&a.body) {
            AmendPlacement::CancelFirst => {
                assert_eq!(
                    (a.body.order_type.clone(), a.body.trigger_price),
                    (Limit, None),
                    "A2's shape"
                );
                assert_eq!(sent.cancels, vec![order_client_id], "the old order is cancelled first");
                assert_eq!(sent.submissions.len(), 1, "and nothing new rests beside it");
                assert!(c.recent.iter().any(|l| l.contains("LOSES ITS PLACE IN THE QUEUE")));
            }
            AmendPlacement::NewFirst => {
                assert_eq!(a.body.order_type, StopLimit, "the shape before A2's release");
                assert!(sent.cancels.is_empty(), "new first: {:?}", sent.cancels);
                assert_eq!(sent.submissions.len(), 2, "the replacement is placed first");
            }
        }
    }
}
