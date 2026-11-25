//! The EMULATED orders (decision 0275): a `pegged` order or a limit with a visible size that the
//! venue does not hold, worked by the core through ONE plain limit at a time. The route is
//! `vike_venues::emulation_of` (native where the row wires it, emulated here, refused where no honest
//! emulation exists); the law is `vike_execution::emulated`'s, the one implementation the simulator runs too.
//!
//! * **Accept** (`lower_submit`, an operator's order and a strategy's alike). The parent is judged as
//!   any order (its account, the ambiguity gate, the mint and its `MintedSubmit`, a stopped account, a
//!   held release first) and preflighted as its CHILD would be (`vike_venues::emulated_child`), then the
//!   pre-trade gate judges it WHOLE (`Engine::judge_whole`: a per-order ceiling is never passed
//!   slice by slice). Then it is armed here instead of sent: it enters no engine registry and reaches no
//!   venue, its owner hears ONE `Accepted`, and one ring line says it is emulated.
//! * **Drive** (`run_loop`'s drain-loop boundary, never per message; one `is_empty` per pass while the
//!   book is empty; never on a replay core). The child events and fills heard since the last pass are
//!   fed to the law first, then one market hook: a chase reads the FRESH touch of its engine's price
//!   board (both sides under `PriceOnlyConfig::max_price_age`; a stale or missing touch holds the child
//!   where it is and the row says why), an iceberg any new market message. The law acts on a
//!   `LiveBroker` whose position is the PARENT's own filled size (never the account's), and its three
//!   tagged buffers become [`vike_execution::EmulatedStep`]s sent through the core's own command door, so each
//!   is journaled write-ahead and a replay re-applies it with no law, no clock and no price.
//! * **Children** are plain limits with the parent's venue, symbol, account, lifetime and flags
//!   (`emulated_child`), minted as the parent's OWNER: a mount's through `lower_strategy_intent` (its
//!   labels, its ledger, its ownership line), an operator's on the parent's engine. A child is in no
//!   tag registry: its events and fills reach the law here (the order-event and fill lanes ask
//!   [`CoreThread::emulated_hears_event`] / [`CoreThread::emulated_hears_fill`] first, one `is_empty`
//!   each), and its fills reach the owner under the PARENT's tag.
//! * **The owner sees ONE order**: `Accepted` at arm, each child fill under its tag, and one terminal
//!   event at the end (`Filled`, or `Canceled` in the law's words), when the parent is registered on its
//!   engine as the order it ended as.
//! * **Cancel, modify, mass-cancel, unmount.** A cancel of the parent pulls its live child and ends it.
//!   A modify is refused in words (cancel it and send it again). A mass-cancel or market exit in scope
//!   ends it (its child is cancelled by that command). An unmount's cancel of its mount's orders names
//!   the parent too (`order_client_id_mount`), so it ends the same way. A shutdown cancels nothing.
//! * **Restart: re-adopted, never cancelled.** The book rides every `Snapshot` (`Snapshot.emulated`, with each
//!   law's saved state). At assembly each law is rebuilt and awaits its child's confirmation with no
//!   deadline: the reconcile pass's synthetic `Accepted` adopts a resting child, its `RESTORE_GONE_REASON`
//!   cancel ends the parent in the law's words, and silence waits (decision 0192). A parent restored with
//!   no live child gives the slot up at its first hook, which places. The one exception is a law whose
//!   saved state does not read (`vike_execution::emulated`'s contract): it places nothing more and pulls
//!   its child, so the parent ends once no child lives.
//!
//! **One entity.** `crate::emulator::OrderEmulationBook` holds these WORKING kinds beside the TRIGGER kinds of today's
//! conditional books (`crate::emulator`), whose law, journal records and fold checks are unchanged.
//!
//! Nothing here is on the per-message fold.

use super::*;
use crate::emulator::{OrderEmulation, OrderEmulationChildReport};
use vike_execution::emulated::{DONE_EPS, OrderEmulationLaw};
use vike_execution::{EmulatedEnd, EmulatedStep, EmulatedVerb};
use vike_model::{Fill, OrderEvent, OrderEventKind, OrderType};
use vike_venues::OrderEmulationKind;

impl<C: ExecutionClient> CoreThread<C> {
    /// Arm `req` (minted, journaled, preflighted as its child on engine `engine_index`) on `route` instead of
    /// sending it; `lower_submit` asks `vike_venues::emulation_of` first. The pre-trade gate judges the
    /// WHOLE parent here, and a refusal refuses it whole. One `Accepted` reaches its owner.
    pub(super) fn arm_emulated(
        &mut self,
        engine_index: usize,
        owner: EngineRoute,
        req: OrderRequest,
        route: OrderEmulationKind,
        now: i64,
    ) {
        let order_client_id = req.order_client_id.clone();
        let whole = OrderRequest {
            order_client_id: order_client_id.clone(),
            price: if route == OrderEmulationKind::Iceberg { req.price } else { None },
            ..vike_venues::emulated_child(&req)
        };
        if let Err((denial, why)) = self.engine_mut(engine_index).judge_whole(&whole, now) {
            let reason =
                format!("EMULATED_PARENT_DENIED: the pre-trade gate refuses it whole: {why}");
            let refusal_id =
                denial.and_then(|denial| vike_model::events::refusal_id_of(denial.id().word()));
            self.synthesize_capability_reject(engine_index, &req, &reason, refusal_id, now);
            return;
        }
        let mount_id = match owner {
            EngineRoute::Mount(m) => self.mount_ids.get(m).cloned(),
            _ => None,
        };
        let (venue, symbol) = (self.engine(engine_index).venue.to_string(), req.symbol.clone());
        self.note(format!("order {order_client_id} {}", route.note(&venue)));
        let route_key = self.engine(engine_index).route_key.to_string();
        let law = OrderEmulationLaw::of(&req, route, i64::MAX);
        self.emulated.working.insert(
            order_client_id.clone(),
            OrderEmulation {
                route_key,
                mount_id,
                request: req,
                law: Some(law),
                child: None,
                children: Vec::new(),
                filled: 0.0,
                average_price: 0.0,
                child_qty: 0.0,
                child_filled: 0.0,
                heard: Vec::new(),
                seen_ms: i64::MIN,
                waiting: None,
                unconfirmed: false,
            },
        );
        // A mount-less engine collects no order events until it is told to: the law hears its child's.
        let engine = self.engine_mut(engine_index);
        engine.collect_applied_fills = true;
        engine.order_events.push(vike_execution::execution_engine::OrderEventOut {
            venue,
            symbol,
            event: OrderEvent { order_client_id, tag: None, kind: OrderEventKind::Accepted },
        });
        self.dirty = true;
    }

    /// The boundary drive (`run_loop`, a live core only): every parent, in accepted order.
    pub(crate) fn drive_emulated(&mut self, now: i64) {
        let order_client_ids: Vec<String> = self.emulated.working.keys().cloned().collect();
        for order_client_id in order_client_ids {
            self.drive_parent(&order_client_id, now);
        }
    }

    fn drive_parent(&mut self, order_client_id: &str, now: i64) {
        let Some(p) = self.emulated.working.get(order_client_id) else { return };
        let (route_key, mount_id) = (p.route_key.clone(), p.mount_id.clone());
        let Some(engine_index) = self.engine_index_for_route_key(RouteKey::declared(&route_key))
        else {
            let why = format!("its account {route_key} runs no engine on this node now");
            return self.say_waiting(order_client_id, Some(why));
        };
        if let Some(mid) = mount_id.as_deref()
            && !self.mount_ids.iter().zip(&self.mounts).any(|(id, m)| id == mid && m.is_some())
        {
            return self
                .say_waiting(order_client_id, Some(format!("its strategy {mid} is not mounted")));
        }
        let p = &self.emulated.working[order_client_id];
        let (venue, symbol, side) =
            (self.engine(engine_index).venue.to_string(), p.request.symbol.clone(), p.request.side);
        let route = p.law.as_ref().map_or(OrderEmulationKind::Iceberg, OrderEmulationLaw::route);
        let max_age = self.config.price_only.as_ref().map_or(MAX_PRICE_AGE, |po| po.max_price_age);
        let max_age_ms = i64::try_from(max_age.as_millis()).unwrap_or(i64::MAX);
        let cell = self.engine(engine_index).price_board.cell(&venue, &symbol).cloned();
        let fresh = |o: Option<(f64, i64)>| {
            o.filter(|(px, ts)| {
                px.is_finite() && *px > 0.0 && now.saturating_sub(*ts) <= max_age_ms
            })
        };
        // The market hook this pass hands the law, if any, and why the parent waits otherwise.
        let (hook, why) = match route {
            OrderEmulationKind::Chase => {
                let (bid, ask) =
                    cell.as_ref().map_or((None, None), |c| (fresh(c.bid), fresh(c.ask)));
                match (bid, ask) {
                    (Some((b, bt)), Some((a, at))) => {
                        let ts = bt.max(at);
                        ((ts > p.seen_ms).then(|| quote(ts, b, a, &symbol)), None)
                    }
                    _ => (
                        None,
                        Some(format!(
                            "no fresh touch of {symbol} on {venue}: a chase follows the book and never \
                             prices off a stale or missing one"
                        )),
                    ),
                }
            }
            OrderEmulationKind::Iceberg => {
                let ts = cell.as_ref().map_or(i64::MIN, |c| {
                    [c.mark, c.bid, c.ask, c.last_trade, c.bar_close]
                        .iter()
                        .filter_map(|o| o.map(|(_, t)| t))
                        .max()
                        .unwrap_or(i64::MIN)
                });
                ((ts > p.seen_ms).then(|| quote(ts, f64::NAN, f64::NAN, &symbol)), None)
            }
        };
        let why = if p.unconfirmed { p.waiting.clone() } else { why };
        if hook.is_none() && p.heard.is_empty() {
            return self.say_waiting(order_client_id, why);
        }
        let p = self.emulated.working.get_mut(order_client_id).expect("looked up above");
        let heard = std::mem::take(&mut p.heard);
        let position = f64::from(side) * p.filled;
        if let Some(q) = &hook {
            p.seen_ms = q.timestamp;
        }
        let Some(mut law) = p.law.take() else { return };
        let mut ctx = self.law_context(engine_index, &symbol, position, now);
        for h in &heard {
            match h {
                OrderEmulationChildReport::Fill(f) => law.hooks().on_fill(&mut ctx, f),
                OrderEmulationChildReport::Event(e) => law.hooks().on_order_event(&mut ctx, e),
            }
        }
        if let Some(q) = &hook {
            law.hooks().on_quote_tick(&mut ctx, q);
        }
        let tag = law.tag();
        let refusal = law.refusal();
        if let Some(p) = self.emulated.working.get_mut(order_client_id) {
            p.law = Some(law);
        }
        self.say_waiting(order_client_id, why);
        for intent in self.child_intents(order_client_id, ctx, tag) {
            let step = EmulatedStep {
                parent: order_client_id.to_string(),
                mount_id: mount_id.clone(),
                verb: EmulatedVerb::Child(intent),
            };
            let _ = self.handle(Ingest::Command(Command::Emulated(Box::new(step))));
        }
        // The law placed nothing more and no child lives: the parent ends, once.
        if let Some(words) = refusal
            && let Some(p) = self.emulated.working.get(order_client_id)
            && p.child.is_none()
        {
            let end = if p.filled >= p.request.qty * (1.0 - DONE_EPS) {
                EmulatedEnd::Filled
            } else {
                EmulatedEnd::Canceled(words)
            };
            self.end_by_step(order_client_id, mount_id, end);
        }
    }

    /// End `order_client_id` through a journaled step, pulling its live child first.
    fn end_by_step(&mut self, order_client_id: &str, mount_id: Option<String>, end: EmulatedEnd) {
        if let Some(child) =
            self.emulated.working.get(order_client_id).and_then(|p| p.child.clone())
        {
            let step = EmulatedStep {
                parent: order_client_id.to_string(),
                mount_id: mount_id.clone(),
                verb: EmulatedVerb::Child(OrderIntent::Cancel(child)),
            };
            let _ = self.handle(Ingest::Command(Command::Emulated(Box::new(step))));
        }
        let step = EmulatedStep {
            parent: order_client_id.to_string(),
            mount_id,
            verb: EmulatedVerb::End(end),
        };
        let _ = self.handle(Ingest::Command(Command::Emulated(Box::new(step))));
    }

    /// The law's buffered child verbs as intents on the parent's live child: submits first, then the
    /// re-prices (the price alone: a modify's size is the order's total), then the cancels, as the strategy
    /// drain orders them. A verb naming no live child is dropped (the tag names nothing).
    fn child_intents(&self, order_client_id: &str, ctx: LiveBroker, tag: &str) -> Vec<OrderIntent> {
        let Some(p) = self.emulated.working.get(order_client_id) else { return Vec::new() };
        let mut out = Vec::new();
        for s in ctx.submissions.into_iter().filter(|s| s.tag.as_deref() == Some(tag)) {
            let Some(price) = s.price else { continue };
            let mut child = vike_venues::emulated_child(&p.request);
            (child.side, child.qty, child.price) = (s.side, s.qty, Some(price));
            out.push(OrderIntent::Submit(Box::new(child)));
        }
        for m in ctx.modifications.into_iter().filter(|m| m.tag == tag) {
            if let (Some(child), Some(price)) = (p.child.clone(), m.new_price) {
                out.push(OrderIntent::Modify {
                    order_client_id: child,
                    new_qty: None,
                    new_price: Some(price),
                    new_trigger_price: None,
                });
            }
        }
        if ctx.cancels.iter().any(|t| t == tag)
            && let Some(child) = p.child.clone()
        {
            out.push(OrderIntent::Cancel(child));
        }
        out
    }

    /// The `LiveBroker` the law acts on: the PARENT's own filled size as its position, the engine's
    /// grid and minimums for the order's symbol, `now`. Its buffers are read back by
    /// [`Self::child_intents`]; nothing else of it is.
    fn law_context(
        &self,
        engine_index: usize,
        symbol: &str,
        position: f64,
        now: i64,
    ) -> LiveBroker {
        let engine = self.engine(engine_index);
        LiveBroker {
            data_delay: None,
            inputs: None,
            closed_trades: None,
            positions: Vec::new(),
            prices: Vec::new(),
            bar_views: Vec::new(),
            leg_values: None,
            position,
            average_price: 0.0,
            price: 0.0,
            equity: 0.0,
            bars: Arc::default(),
            index: 0,
            now,
            contract_value: engine.account.base_value_of(symbol),
            quote_rate: engine.account.quote_rate_of(symbol),
            lot_size: engine.gate.limits.lot_size.unwrap_or(0.0),
            tick_size: engine.gate.limits.tick_size.unwrap_or(0.0),
            min_qty: engine.gate.limits.min_qty.unwrap_or(0.0),
            min_notional: engine.gate.limits.min_notional.unwrap_or(0.0),
            submissions: Vec::new(),
            modifications: Vec::new(),
            cancels: Vec::new(),
            brackets: Vec::new(),
            order_lists: Vec::new(),
            combos: Vec::new(),
            order_emulation_arms: Vec::new(),
            mass_cancel: false,
            forecasts: Vec::new(),
        }
    }

    /// Set the parent's waiting words; a change is said once on the ring and republished.
    fn say_waiting(&mut self, order_client_id: &str, why: Option<String>) {
        let Some(p) = self.emulated.working.get_mut(order_client_id) else { return };
        if p.waiting == why {
            return;
        }
        p.waiting = why.clone();
        self.dirty = true;
        if let Some(w) = why {
            self.note(format!("emulated order {order_client_id} waits: {w}"));
        }
    }

    /// [`Command::Emulated`]'s arm, on a live core and a replay core alike: lower one child verb as the
    /// parent's owner's order (and link a placed child to its parent), or end the parent.
    pub(crate) fn apply_emulated_step(&mut self, step: EmulatedStep) {
        let now = self.engine.now_ms;
        let EmulatedStep { parent, mount_id, verb } = step;
        let Some(p) = self.emulated.working.get(&parent) else {
            self.note(format!("emulated step for {parent} not applied: no such emulated order"));
            return;
        };
        let route_key = p.route_key.clone();
        match verb {
            EmulatedVerb::Child(intent) => {
                let placed = match &intent {
                    OrderIntent::Submit(req) => Some(req.qty),
                    _ => None,
                };
                let mount =
                    mount_id.and_then(|mid| self.mount_ids.iter().position(|id| *id == mid));
                let unspecified = vike_model::OrderCancelIntent::Unspecified;
                let order_client_ids = match (
                    mount,
                    self.engine_index_for_route_key(RouteKey::declared(&route_key)),
                ) {
                    (Some(m), _) => self.lower_strategy_intent(intent, now, m, unspecified, &[]),
                    (None, Some(e)) => {
                        self.apply_intent_routed(intent, now, unspecified, EngineRoute::Engine(e))
                    }
                    (None, None) => {
                        self.note(format!(
                            "emulated order {parent}: its account {route_key} runs no engine; the \
                             child verb is not sent"
                        ));
                        Vec::new()
                    }
                };
                if let (Some(qty), Some(child)) = (placed, order_client_ids.into_iter().next()) {
                    self.note(format!("emulated order {parent}: child {child} placed"));
                    self.emulated.child_of.insert(child.clone(), parent.clone());
                    if let Some(p) = self.emulated.working.get_mut(&parent) {
                        p.children.push(child.clone());
                        p.child = Some(child);
                        (p.child_qty, p.child_filled) = (qty, 0.0);
                    }
                }
            }
            EmulatedVerb::End(end) => self.end_emulated(&parent, end, now),
        }
    }

    /// End `order_client_id`: it leaves the book, is registered on its engine as the order it ended as (so it is
    /// listed once more, finished), and its owner hears the one terminal event. Its children stay mapped,
    /// so their late events reach no owner as a stranger's.
    fn end_emulated(&mut self, order_client_id: &str, end: EmulatedEnd, now: i64) {
        let Some(p) = self.emulated.working.shift_remove(order_client_id) else { return };
        let Some(engine_index) = self.engine_index_for_route_key(RouteKey::declared(&p.route_key))
        else {
            self.note(format!(
                "emulated order {order_client_id} ended (its account {} runs no engine)",
                p.route_key
            ));
            return;
        };
        let (status, kind, words) = match &end {
            EmulatedEnd::Filled => (
                vike_model::OrderStatus::Filled,
                OrderEventKind::Filled,
                format!("FILLED: {} of {}", p.filled, p.request.qty),
            ),
            EmulatedEnd::Canceled(why) => (
                vike_model::OrderStatus::Canceled,
                OrderEventKind::Canceled { reason: why.clone() },
                format!("ENDED: {why} ({} of {} filled)", p.filled, p.request.qty),
            ),
        };
        let mut mo = vike_model::OrderRecord::new(p.request.clone());
        mo.status = status;
        mo.created_ms = Some(now);
        mo.filled_qty = p.filled;
        mo.average_fill_price = p.average_price;
        let engine = self.engine_mut(engine_index);
        engine.registry.insert(order_client_id.to_string(), mo);
        engine.order_events.push(vike_execution::execution_engine::OrderEventOut {
            venue: p.request.venue.clone(),
            symbol: p.request.symbol.clone(),
            event: OrderEvent { order_client_id: order_client_id.to_string(), tag: None, kind },
        });
        // An emulated parent enters a registry only at its end (decision 0275), so that is its one report.
        self.report_registered(engine_index, order_client_id);
        self.note(format!("emulated order {order_client_id} {words}"));
        self.dirty = true;
    }

    /// The Cancel arm's half: `order_client_id` is an emulated parent, so its live child is pulled and it ends
    /// cancelled. `false` iff it is not one. Deterministic: a replay of the journaled cancel does the same.
    pub(crate) fn cancel_emulated(&mut self, order_client_id: &str) -> bool {
        if self.emulated.working.is_empty() {
            return false;
        }
        let Some(p) = self.emulated.working.get(order_client_id) else { return false };
        if let Some(child) = p.child.clone() {
            let engine_index = self.engine_of_order_client_id(&child).unwrap_or(0);
            self.engine_mut(engine_index)
                .cancel_order_with_intent(&child, vike_model::OrderCancelIntent::Unspecified);
        }
        let now = self.engine.now_ms;
        self.end_emulated(
            order_client_id,
            EmulatedEnd::Canceled("canceled by its owner".to_string()),
            now,
        );
        true
    }

    /// The Modify arm's half: an emulated parent is never amended (decision 0275), refused in the words
    /// the simulator refuses it in (`vike_execution::emulated::modify_refusal`). `false` iff `order_client_id` is not one.
    pub(crate) fn refuse_emulated_modify(&mut self, order_client_id: &str) -> bool {
        if self.emulated.working.is_empty() || !self.emulated.working.contains_key(order_client_id)
        {
            return false;
        }
        let why = vike_execution::emulated::modify_refusal(order_client_id);
        tracing::warn!(target: "vike_core::core", order_client_id = %order_client_id, "{why}");
        self.note(why);
        true
    }

    /// A mass-cancel's or market exit's half: end every parent in its `(venue, symbol)` scope (`None` =
    /// any), narrowed to `engines` when the payload named an account. The command cancels the children.
    pub(crate) fn clear_emulated(
        &mut self,
        venue: Option<&str>,
        symbol: Option<&str>,
        engines: Option<&[usize]>,
    ) {
        if self.emulated.working.is_empty() {
            return;
        }
        let doomed: Vec<String> = self
            .emulated
            .working
            .iter()
            .filter(|(_, p)| {
                venue.is_none_or(|v| p.request.venue == v)
                    && symbol.is_none_or(|s| p.request.symbol == s)
                    && engines.is_none_or(|set| {
                        self.engine_index_for_route_key(RouteKey::declared(&p.route_key))
                            .is_some_and(|i| set.contains(&i))
                    })
            })
            .map(|(c, _)| c.clone())
            .collect();
        let now = self.engine.now_ms;
        for c in doomed {
            self.end_emulated(&c, EmulatedEnd::Canceled("canceled (mass-cancel)".to_string()), now);
        }
    }

    /// The order-event lane's question: is `oe` a CHILD's? Then the book takes it (the live child is
    /// let go on a terminal one, and a live core hands it to the law at the next pass under the law's
    /// own tag) and no owner hears it. One map lookup, asked only while the book is not empty.
    pub(crate) fn emulated_hears_event(
        &mut self,
        oe: &vike_execution::execution_engine::OrderEventOut,
    ) -> bool {
        let order_client_id = oe.event.order_client_id.as_str();
        let Some(parent) = self.emulated.child_of.get(order_client_id).cloned() else {
            return false;
        };
        let terminal = oe.event.kind.is_terminal();
        if terminal {
            self.emulated.child_of.remove(order_client_id);
        }
        let live = !self.config.replay_core;
        if let Some(p) = self.emulated.working.get_mut(&parent) {
            if terminal {
                p.children.retain(|c| c != order_client_id);
                if p.child.as_deref() == Some(order_client_id) {
                    p.child = None;
                }
            }
            if p.unconfirmed {
                p.unconfirmed = false;
                p.waiting = None;
            }
            if live {
                let tag = p.law.as_ref().map(|l| l.tag().to_string());
                p.heard
                    .push(OrderEmulationChildReport::Event(OrderEvent { tag, ..oe.event.clone() }));
            }
            self.dirty = true;
        }
        true
    }

    /// The fill lane's question: is `f` a CHILD's? Then it counts toward its parent (and, when it
    /// completed the child, lets the child go), a live core hands it to the law at the next pass, and
    /// the answer is the parent's order client id, whose tag the owner hears the fill under. The child is complete
    /// when the registry says so (`completed`) OR when the fills the book counted reach the size it was
    /// placed with: a whole fill arrives as the account's fill and the lifecycle's `OrderFilled`, and this
    /// lane may run between the two, so the book's own count decides it, alike on a replay. One map lookup,
    /// asked only while the book is not empty.
    pub(crate) fn emulated_hears_fill(
        &mut self,
        f: &vike_model::events::FillEvent,
        completed: bool,
    ) -> Option<String> {
        let order_client_id = f.order_client_id.as_str();
        let parent = self.emulated.child_of.get(order_client_id)?.clone();
        let live_child = self
            .emulated
            .working
            .get(&parent)
            .is_some_and(|p| p.child.as_deref() == Some(order_client_id));
        let completed = completed
            || (live_child
                && self.emulated.working.get(&parent).is_some_and(|p| {
                    p.child_filled + f.last_qty >= p.child_qty * (1.0 - DONE_EPS)
                }));
        let live = !self.config.replay_core;
        let value = self
            .emulated
            .working
            .get(&parent)
            .and_then(|p| self.engine_index_for_route_key(RouteKey::declared(&p.route_key)))
            .map(|i| self.engine(i).account.contract_value_of(&f.symbol))
            .unwrap_or_default();
        if let Some(p) = self.emulated.working.get_mut(&parent) {
            p.average_price =
                value.average_price(p.filled, p.average_price, f.last_qty, f.last_price);
            p.filled += f.last_qty;
            if live_child {
                p.child_filled += f.last_qty;
            }
            // The child stays MAPPED until its terminal lifecycle event (`emulated_hears_event`), so that
            // event, which may follow this fill, reaches no owner as a stranger's.
            if completed && p.child.as_deref() == Some(order_client_id) {
                p.child = None;
            }
            if live {
                let tag = p.law.as_ref().map(|l| l.tag().to_string());
                p.heard.push(OrderEmulationChildReport::Fill(Fill {
                    side: f.side,
                    size: f.last_qty,
                    price: f.last_price,
                    fee: f.commission,
                    timestamp: f.timestamp,
                    is_maker: f.liquidity_side.is_maker(),
                    symbol: f.symbol.to_string(),
                    tag: tag.clone(),
                }));
                if completed {
                    p.heard.push(OrderEmulationChildReport::Event(OrderEvent {
                        order_client_id: order_client_id.to_string(),
                        tag,
                        kind: OrderEventKind::Filled,
                    }));
                }
            }
            self.dirty = true;
        }
        Some(parent)
    }

    /// EVERY emulated order as a row of the one published list (`CoreSnapshot::emulated_orders`): the
    /// trigger kinds' arms, then the working parents. Called from `publish` only while the entity holds
    /// one (an arm row in `cond_engine` or a working parent).
    pub(crate) fn emulated_views(&self) -> Vec<vike_execution::OrderEmulationView> {
        use vike_model::accounts::account_keys::label_of_route_key;
        let mut out = Vec::new();
        for ((venue, symbol), book) in &self.emulated.order_emulation_arms {
            for (id, arm) in book.iter() {
                let o = &arm.order;
                let route_key = self
                    .emulated
                    .cond_engine
                    .get(id)
                    .map(|a| self.engine(a.engine).route_key.to_string());
                out.push(vike_execution::OrderEmulationView {
                    id: id.to_string(),
                    venue: venue.clone(),
                    account: route_key
                        .and_then(|k| label_of_route_key(venue, &k))
                        .filter(|l| !l.is_default()),
                    symbol: symbol.clone(),
                    side: o.side,
                    qty: o.size,
                    kind: trigger_word(o.kind),
                    reduce_only: self.emulated.cond_engine.get(id).is_some_and(|a| a.reduce_only),
                    trigger: Some(arm.trigger_level()),
                    price: o.limit,
                    trail: o.trail,
                    peg: None,
                    display_quantity: None,
                    filled: 0.0,
                    average_price: 0.0,
                    child: None,
                    waiting: self.stop_holds.contains_key(id).then(|| {
                        "restored: held until its account's first reconcile pass".to_string()
                    }),
                    rearms: self.emulated.cond_engine.get(id).map_or(0, |a| a.rearms),
                    held: self.stop_holds.contains_key(id),
                });
            }
        }
        for (order_client_id, p) in &self.emulated.working {
            let r = &p.request;
            let iceberg = r.order_type != OrderType::Pegged;
            out.push(vike_execution::OrderEmulationView {
                id: order_client_id.clone(),
                venue: r.venue.clone(),
                account: label_of_route_key(&r.venue, &p.route_key).filter(|l| !l.is_default()),
                symbol: r.symbol.clone(),
                side: r.side,
                qty: r.qty,
                kind: if iceberg { OrderEmulationKind::Iceberg } else { OrderEmulationKind::Chase }
                    .as_word(),
                reduce_only: r.reduce_only,
                trigger: None,
                price: if iceberg { r.price } else { None },
                trail: None,
                peg: if iceberg { None } else { r.peg },
                display_quantity: if iceberg { r.display_quantity } else { None },
                filled: p.filled,
                average_price: p.average_price,
                child: p.child.clone(),
                waiting: p.waiting.clone(),
                rearms: 0,
                held: false,
            });
        }
        out
    }

    /// The book as the `Snapshot` carries it (`Snapshot.emulated`); empty, so no key, while it is empty.
    pub(crate) fn emulated_snap(&self) -> Vec<vike_journal::JournalSnapshotOrderEmulation> {
        self.emulated
            .working
            .values()
            .map(|p| vike_journal::JournalSnapshotOrderEmulation {
                route_key: p.route_key.clone(),
                mount_id: p.mount_id.clone(),
                request: p.request.clone(),
                child: p.child.clone(),
                children: p.children.clone(),
                filled: p.filled,
                average_price: p.average_price,
                child_qty: p.child_qty,
                child_filled: p.child_filled,
                law: p
                    .law
                    .as_ref()
                    .map_or(serde_json::Value::Null, OrderEmulationLaw::saved::<LiveBroker>),
            })
            .collect()
    }

    /// Assembly: each restored parent's engine collects order events again, and the ring says what is
    /// worked again. Cold: once per boot.
    pub(super) fn rework_restored_emulated(&mut self) {
        if self.emulated.working.is_empty() {
            return;
        }
        self.dirty = true;
        let keys: Vec<(String, String)> =
            self.emulated.working.iter().map(|(c, p)| (c.clone(), p.route_key.clone())).collect();
        for (_, key) in &keys {
            if let Some(i) = self.engine_index_for_route_key(RouteKey::declared(key)) {
                self.engine_mut(i).collect_applied_fills = true;
            }
        }
        self.note(format!(
            "RESTORE EMULATED: {} emulated order(s) worked again, each awaiting its child's confirmation \
             ({})",
            keys.len(),
            keys.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
}

/// The word a trigger kind is listed under (`vike_execution::OrderEmulationView::kind`), the capability rows'
/// kind word. The book holds no other kind (`OrderEmulationArm::of`).
fn trigger_word(kind: vike_model::OrderKind) -> &'static str {
    use vike_model::OrderKind as K;
    match kind {
        K::Stop => "stop",
        K::Trailing => "trailing",
        K::StopLimit => "stop_limit",
        K::TakeProfit => "take_profit",
        K::TrailingLimit => "trailing_limit",
        K::TrailingTakeProfit => "trailing_take_profit",
        _ => "conditional",
    }
}

/// A market hook for the law: the touch (`NaN` sides for an iceberg, which reads none) at `ts`.
fn quote(ts: i64, bid: f64, ask: f64, symbol: &str) -> vike_marketdata::TickQuote {
    vike_marketdata::TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: symbol.into(),
    }
}
