//! The ONE amend of a resting order, above the bridges: `OrderIntent::Modify` of a live order, whatever
//! the venue does underneath. Decision 0244 is the record; `vike_venues::venue_amend_mode` is the table
//! that says, per venue and per order kind, which of the three ways applies; `vike_exec::amend` is the
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
//!   words say so). The replacement is a fresh order in every respect the rest of the node cares
//!   about (a minted coid, the standard submit tail, the mount's tag and ownership), and
//!   `ManagedOrder::replaces` keeps the OPERATOR'S first coid as the name of the logical order, so a
//!   cancel or modify that names it reaches the live member of the chain ([`CoreThread::amend_head`]).
//! * **Refused** — an engine/venue that cannot amend refuses in words, which is the engine's own
//!   refusal (`ExecutionEngine::modify_order`); this file adds none.
//!
//! # Where it is driven
//!
//! A modify is a COMMAND: [`CoreThread::modify_resting`] runs when it is applied. The second half of an
//! amend waits for the venue, so it is driven at the drain-loop BOUNDARY ([`CoreThread::drive_amends`],
//! once per drained batch and only while an amend is in flight), reading the registry's status of the two
//! orders. No venue message is looked at and nothing is added to the fold: the `run` loop's one added
//! read is `self.amends.is_empty()` beside the timer wheel's, per pass and not per message.
//!
//! # What is journaled
//!
//! Nothing new. The Modify command is journaled as it always was; the replacement rides the standard
//! submit path, so its coid is minted and journaled by `journal_minted_submit` (an operator's) or the
//! write-ahead `StrategySubmit` (a mount's), and its lifecycle is the venue's own journaled events. The
//! lineage lives on the registry entry (`ManagedOrder::replaces`, absent when `None`, so a snapshot and
//! its hash are unchanged for every order never replaced). The in-flight table below is core-local, like
//! the other cold tables beside it: after a restart in the middle of an amend the state is read back
//! from the venue (both orders rest, both are reduce-only for a stop, so the position stays covered).

use super::*;
use vike_exec::amend::{Act, End, Placement, Replace, Seen};
use vike_model::events::OrderModifyRejected;
use vike_venues::venue_amend_mode::{AmendKind, AmendMode, amend_words};

/// Who placed the order being amended, so its replacement is placed the same way.
#[derive(Debug, Clone)]
pub(crate) enum AmendRoute {
    /// An operator's order, or any order no mount minted: placed again on the engine it rests on.
    Engine,
    /// A strategy mount's order: placed through the mount's own intent door, so the coid is
    /// attributed to the mount and owned under its tag. `key` is the tag's key in `strategy_tags`.
    Mount { idx: usize, tag: Option<String>, key: Option<String> },
}

/// One cancel-then-new amend in flight. Core-local: never hashed, snapped or journaled; empty at boot.
#[derive(Debug)]
pub(crate) struct AmendInFlight {
    /// The coid the OPERATOR first named: the name of the logical order across every replacement.
    root: String,
    /// The live order being replaced.
    old: String,
    /// The replacement's coid, once it is placed.
    new: Option<String>,
    /// The engine both orders rest on.
    eidx: usize,
    route: AmendRoute,
    /// The replacement as it will be placed: no coid yet, no contingency links.
    body: OrderRequest,
    fsm: Replace,
    /// Someone cancelled the logical order while the amend was in flight: end it, place nothing.
    withdrawn: bool,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The live member of the chain `coid` belongs to when it is NOT `coid` itself, else `None`.
    ///
    /// An operator names the order by the coid they were given. After a cancel-then-new amend that
    /// coid is a terminal order and its replacement carries a new one, linked by
    /// `ManagedOrder::replaces`; a command naming ANY member of the chain therefore reaches the live
    /// one. Cold: the registry scan runs only when `coid` itself is not live, which for an
    /// order that was never replaced is the "unknown or finished" answer it has always been.
    pub(crate) fn amend_head(&self, eidx: usize, coid: &str) -> Option<String> {
        let eng = self.eng(eidx);
        match eng.registry.get(coid) {
            // live: it is the head. Finished and never part of a chain: there is nothing to follow.
            Some(mo) if mo.status.is_live() || mo.replaces.is_none() => return None,
            _ => {}
        }
        let root = eng.registry.get(coid).and_then(|mo| mo.replaces.as_deref()).unwrap_or(coid);
        eng.registry
            .iter()
            .rev()
            .find(|(_, mo)| mo.replaces.as_deref() == Some(root) && mo.status.is_live())
            .map(|(c, _)| c.clone())
    }

    /// The first coid of the chain `coid` belongs to (itself when it was never replaced).
    fn amend_root(&self, eidx: usize, coid: &str) -> String {
        self.eng(eidx)
            .registry
            .get(coid)
            .and_then(|mo| mo.replaces.clone())
            .unwrap_or_else(|| coid.to_string())
    }

    /// The `Modify` arm's LIVE-order half (a held bracket exit is edited in place by the caller before
    /// this is reached). Takes the mode from the venue table through the engine and acts on it:
    /// cancel-then-new starts an amend, every other answer is the engine's native path unchanged.
    pub(super) fn modify_resting(
        &mut self,
        client_order_id: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        now: i64,
    ) {
        let eidx = self.coid_venue.get(&client_order_id).copied().unwrap_or(0);
        // An operator's CORE-HELD stop is named by its arm id and has no order in the registry: the engine
        // would take the modify for an unknown coid and say nothing. Say what to do instead.
        if !self.cond_engine.is_empty()
            && self.cond_engine.get(&client_order_id).is_some_and(|a| a.reduce_only)
        {
            self.refuse_amend(
                eidx,
                &client_order_id,
                format!(
                    "{client_order_id} is a stop this node holds itself (its venue rests none): it is \
                     moved by `cancel {client_order_id}` and a new stop, not by a modify"
                ),
                now,
            );
            return;
        }
        let head = self.amend_head(eidx, &client_order_id).unwrap_or(client_order_id);
        self.eng_mut(eidx).now_ms = now;
        let mode = self
            .eng(eidx)
            .registry
            .get(&head)
            .and_then(|mo| self.eng(eidx).amend_mode_of(&mo.request));
        if mode == Some(AmendMode::CancelThenNew) {
            self.begin_replace(eidx, head, new_qty, new_price, now);
            return;
        }
        let mut outbox = Outbox::default();
        self.eng_mut(eidx).modify_order(&head, new_qty, new_price, now, &mut outbox);
        // A RiskGate veto publishes a NON-TERMINAL `OrderModifyRejected` (the order keeps its terms), so
        // this drives the same outbox path every other intent does — the rejection reaches the event
        // stream, the recent-events ring and any mounted strategy instead of being dropped on the floor.
        self.publish_and_drive_outbox(eidx, outbox, now);
    }

    /// Say a modify cannot start, in words: a non-terminal `OrderModifyRejected` on `coid` and a note.
    fn refuse_amend(&mut self, eidx: usize, coid: &str, words: String, now: i64) {
        tracing::warn!(target: "vike_core::core", coid = %coid, "{words}");
        self.note(format!("AMEND {coid} refused: {words}"));
        let mut outbox = Outbox::default();
        outbox.publish(Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: coid.to_string(),
            reason: words.into(),
            ts: now,
        }));
        self.publish_and_drive_outbox(eidx, outbox, now);
    }

    /// Start the cancel-then-new amend of the live order `head`.
    fn begin_replace(
        &mut self,
        eidx: usize,
        head: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        now: i64,
    ) {
        let root = self.amend_root(eidx, &head);
        if self.amends.iter().any(|a| a.root == root) {
            self.refuse_amend(
                eidx,
                &head,
                format!("an amend of order {root} is already in flight; wait for it to end"),
                now,
            );
            return;
        }
        if new_qty.is_none() && new_price.is_none() {
            self.refuse_amend(
                eidx,
                &head,
                "a modify names neither a quantity nor a price".into(),
                now,
            );
            return;
        }
        let Some(old_req) = self.eng(eidx).registry.get(&head).map(|mo| mo.request.clone()) else {
            return;
        };
        let placement = Placement::of(&old_req);
        // A cancel-first amend of a bracket's leg would break the group's link (the leg is cancelled,
        // and its replacement is not the leg the siblings name). A reduce-only stop goes new-first and
        // the book is re-keyed to the replacement, which keeps the link.
        if placement == Placement::CancelFirst && self.contingency.contains(&head) {
            self.refuse_amend(
                eidx,
                &head,
                format!(
                    "{} cannot change a {} in place and this order is a leg of a bracket: cancel the \
                     bracket's leg and place the new one by hand",
                    old_req.venue,
                    old_req.order_type.as_str()
                ),
                now,
            );
            return;
        }
        let mut outbox = Outbox::default();
        let planned =
            self.eng_mut(eidx).plan_replacement(&head, new_qty, new_price, now, &mut outbox);
        let Some(mut body) = planned else {
            self.publish_and_drive_outbox(eidx, outbox, now);
            return;
        };
        body = strip_contingency_links(body);
        body.client_order_id = String::new();
        body.ts = now;
        let route = self.amend_route_of(&head);
        let kind = AmendKind::of(&old_req.order_type).unwrap_or(AmendKind::Limit);
        // The new terms ride the line: `order ls` shows a stop's limit price column, never its trigger.
        let px = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |x| x.to_string());
        let terms = match body.order_type {
            vike_model::OrderType::Limit => format!("price {}, qty {}", px(body.price), body.qty),
            _ => format!("trigger {}, qty {}", px(body.trigger_price), body.qty),
        };
        self.note(format!(
            "AMEND {root} (new {terms}): {}",
            amend_words(AmendMode::CancelThenNew, kind, old_req.reduce_only)
        ));
        let mut a = AmendInFlight {
            root,
            old: head,
            new: None,
            eidx,
            route,
            body,
            fsm: Replace::begin(placement, now),
            withdrawn: false,
        };
        match placement {
            Placement::NewFirst => {
                if self.place_replacement(&mut a, now).is_none() {
                    self.finish_amend(&a, End::NewRefused { old_stands: true }, now);
                    return;
                }
            }
            Placement::CancelFirst => {
                self.eng_mut(eidx).cancel_order(&a.old);
            }
        }
        self.amends.push(a);
        self.pump_client();
    }

    /// How the order `coid` was placed: by a mount (its tag and the tag's key) or not.
    pub(crate) fn amend_route_of(&self, coid: &str) -> AmendRoute {
        if let Some((key, _)) = self.strategy_tags.iter().find(|(_, v)| v.as_str() == coid) {
            // `{mount_idx}|{venue}|{symbol}|{tag}`; the tag is the remainder, which may hold a `|`.
            let mut parts = key.splitn(4, '|');
            let idx = parts.next().and_then(|i| i.parse::<usize>().ok());
            let tag = parts.nth(2).map(str::to_string);
            if let (Some(idx), Some(tag)) = (idx, tag) {
                return AmendRoute::Mount { idx, tag: Some(tag), key: Some(key.clone()) };
            }
        }
        match self.coid_mount.get(coid) {
            Some(&idx) => AmendRoute::Mount { idx, tag: None, key: None },
            None => AmendRoute::Engine,
        }
    }

    /// Place the replacement through the standard submit path and record its lineage. `None` when the
    /// submit refused before a coid existed (no account, ambiguous route): the refusal is in the notes.
    fn place_replacement(&mut self, a: &mut AmendInFlight, now: i64) -> Option<String> {
        let body = a.body.clone();
        let coids = match &a.route {
            AmendRoute::Engine => self.apply_intent_routed(
                OrderIntent::Submit(Box::new(body)),
                now,
                CancelIntent::Unspecified,
                EngineRoute::Engine(a.eidx),
            ),
            AmendRoute::Mount { idx, tag, .. } => self.apply_strategy_intent_with_cancel_intent(
                OrderIntent::Submit(Box::new(body)),
                now,
                *idx,
                CancelIntent::Unspecified,
                tag.as_deref(),
            ),
        };
        let new = coids.into_iter().next()?;
        // Every member of the chain names its root, the first order included (it names itself), so a
        // finished order that was NEVER replaced is told apart from one that was without a scan.
        for member in [&a.old, &new] {
            if let Some(mo) = self.eng_mut(a.eidx).registry.get_mut(member) {
                mo.replaces = Some(a.root.clone());
            }
        }
        // The mount's tag now names the replacement (the old coid's terminal retires nothing: that
        // removal is value-guarded). A failed amend puts it back (`finish_amend`).
        if let AmendRoute::Mount { key: Some(key), .. } = &a.route {
            self.strategy_tags.insert(key.clone(), new.clone());
        }
        a.new = Some(new.clone());
        Some(new)
    }

    /// Someone cancelled the logical order `coid` (an operator's cancel, a batch cancel, a mass
    /// cancel): an amend in flight on it must not place a new order after the old one is gone.
    /// `coid = None` withdraws every amend (a mass cancel or flatten names no coid).
    pub(crate) fn withdraw_amends(&mut self, coid: Option<&str>) {
        for a in self.amends.iter_mut() {
            let named =
                coid.is_none_or(|c| c == a.root || c == a.old || a.new.as_deref() == Some(c));
            if named {
                a.withdrawn = true;
            }
        }
    }

    /// The drain-loop boundary's driver: advance every amend in flight one pass. The caller has
    /// gated on `!self.amends.is_empty()`.
    pub(crate) fn drive_amends(&mut self, now: i64) {
        self.dirty = true; // the registry and the notes may move: publish them
        let mut amends = std::mem::take(&mut self.amends);
        amends.retain_mut(|a| !self.drive_one(a, now));
        // an amend started while these ran (none can: begin_replace is a command) would be kept
        amends.append(&mut self.amends);
        self.amends = amends;
    }

    /// One pass over one amend; `true` when it has ended.
    fn drive_one(&mut self, a: &mut AmendInFlight, now: i64) -> bool {
        let eidx = a.eidx;
        let status = |this: &Self, coid: &str| {
            Seen::of(this.eng(eidx).registry.get(coid).map(|mo| mo.status))
        };
        if a.withdrawn {
            if let Some(n) = a.new.clone() {
                self.eng_mut(eidx).cancel_order(&n);
            }
            self.pump_client();
            self.finish_amend(a, End::OldGone, now);
            return true;
        }
        let old = status(self, &a.old);
        let new = a.new.as_deref().map_or(Seen::Unknown, |n| status(self, n));
        if a.fsm.take_cancel_advisory(now) {
            self.note(format!(
                "AMEND {}: the venue has not confirmed the cancel of {} yet; the new terms are placed \
                 the moment it does",
                a.root, a.old
            ));
        }
        let step = a.fsm.step(old, new, now);
        let mut end = step.end;
        match step.send {
            Act::Nothing => {}
            Act::SubmitNew => {
                if self.place_replacement(a, now).is_none() {
                    end = Some(End::NewRefused { old_stands: false });
                }
            }
            Act::CancelOld => {
                // The replacement is ANSWERED and the old stop is about to go: from here the bracket's
                // book keys the leg by the replacement, so a take-profit fill cancels the stop that is
                // in force. Not before: until it is answered the new order may be refused, and a
                // refusal of a book member reads as "the protective exit died unfilled".
                if step.end.is_none()
                    && let Some(n) = a.new.as_deref()
                {
                    self.contingency.rename(&a.old, n);
                }
                self.eng_mut(eidx).cancel_order(&a.old);
            }
            Act::CancelNew => {
                if let Some(n) = a.new.clone() {
                    self.eng_mut(eidx).cancel_order(&n);
                }
            }
        }
        if step.send != Act::Nothing {
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
    fn finish_amend(&mut self, a: &AmendInFlight, end: End, now: i64) {
        let new = a.new.as_deref().unwrap_or("");
        let reason =
            a.new.as_deref().and_then(|n| self.eng(a.eidx).rejection_reason(n)).map(str::to_string);
        let words = end.words(&a.old, new, reason.as_deref());
        // A bracket leg is re-keyed to the replacement once it is answered (`Act::CancelOld`, above). The
        // book follows the LOGICAL leg: driven as the terminal the leg really had when the amend ends
        // with it gone. Before the re-key the old order is still the book's leg and its own events
        // drove the book, so there is nothing to put right.
        if !new.is_empty() && self.contingency.contains(new) {
            match end {
                End::OldFilled => self.drive_contingency_on_fill(new, now),
                End::OldGone | End::NewGone | End::BothGone => {
                    self.drive_contingency_on_terminal(new);
                }
                End::Replaced
                | End::OldNotCancelled
                | End::CancelUnconfirmed
                | End::NewRefused { .. }
                | End::NewSilent { .. } => {}
            }
        }
        // The old order stands: the mount's tag names it again (it was pointed at the replacement when
        // that was placed, and the replacement's refusal retires a tag that names IT).
        if end.old_stands()
            && let AmendRoute::Mount { key: Some(key), .. } = &a.route
            && end != End::OldNotCancelled
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
        self.refuse_amend(a.eidx, on, words, now);
    }
}
