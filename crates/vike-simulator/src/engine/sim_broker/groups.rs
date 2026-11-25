//! Order lists in the backtest ([`vike_model::OrderList`]: an OCO pair, an OTO chain, an OUO
//! group, a bracket, a ladder): [`OrderBrokerSimulator::submit_group`] and its two shapes, and the resolution of
//! a leg's fill by the ONE contingency resolver the live core and the paper book use
//! ([`vike_execution::ContingencyBook::on_fill`]) and the ONE OUO spend ([`vike_execution::spend_ouo_reduce`]).
//!
//! - **Handles.** A leg's handle is [`leg_handle`] of its `OrderResting::qid`, minted from
//!   [`GROUP_QUEUE_ID`] up: the book is keyed by it, and the queue model's own identities (minted from 1)
//!   never reach that range. A plain order keeps qid 0 and is never in the book, so a run that places
//!   no list pays one compare or one `is_empty` per fill and every frozen path is byte-identical.
//! - **Intake, whole or nothing** (decision 0219's shape): the list's links (`build_order_list`), each
//!   leg's kind and terms (`order_terms_refusal`), a reduce-only live leg's position and the pre-trade
//!   gate on every live leg are judged first; one refusal refuses the list, in words, in
//!   [`OrderBrokerSimulator::dropped`], and nothing rests. The mount leverage cap judges the list WHOLE
//!   there, after its links and terms, as ONE order: each leg at its own price, linked legs as
//!   alternatives, its held children each counting where its parent filled, as the live drain does
//!   (`vike_execution::risk::judge_mount_leverage`); a child's arm at its parent's fill meets the other
//!   gates only.
//! - **Trailing.** A trailing leg (`vike_model::OrderListLeg::trail`, its activation the trigger) is
//!   judged by the pre-trade gate as a `Stop` with no price and trails from the mark from when it
//!   rests, as every trailing verb's order does: at its placing, or for an OTO child at its parent's
//!   fill (`OrderBrokerSimulator::seed_trailing`).
//! - **OTO.** A child is HELD off the book ([`OrderLists`]'s `parked`), so it commits no margin and fills
//!   nothing, as the core holds it. Its parent's first fill judges it by the pre-trade gate and rests
//!   it at its full size, from the next event. A parent that ends without a fill cancels its held
//!   children, in words ([`PARENT_ENDED`] in [`OrderBrokerSimulator::dropped`]; the paper book's
//!   `parent-expired` cascade), when the fill pass it ended in closes ([`OrderBrokerSimulator::close_group_pass`]).
//! - **OCO.** A leg's fill cancels its siblings, one that triggered on the same event included: a leg
//!   gone from the book never fills ([`OrderBrokerSimulator::group_leg_size`]).
//! - **OUO**, the paper book's law (`vike-paper`'s `apply_contingency` and `resolve_in_pass`): a fill of
//!   q by a ladder's stop (`ContingencyBook::ouo_shares_its_fill`) takes q IN TOTAL off its resting
//!   targets, farthest from the fill first ([`vike_execution::spend_ouo_reduce`], ruling 2 of wave 4); any
//!   other OUO fill takes q off EACH resting sibling. A sibling reduced to nothing is cancelled. A
//!   sibling that triggered on the SAME event fills what the pass's earlier sibling fills left it (its
//!   size before the one-bar cap, less them), or is cancelled when nothing is left; a ladder's target
//!   after its stop is cancelled as an OCO sibling is.
//! - **Spot.** On a spot (cash) symbol a leg is judged by the spot rule with its group's live legs as
//!   ONE claim, the group's largest set (`vike_execution::ContingencyBook::linked_exits`, the live core's
//!   own), so a ladder's stop and its targets each covering the long are released
//!   (`OrderBrokerSimulator::cash_refusal`).

use vike_execution::risk::MountLeverageLeg;
use vike_execution::{ContingencyBook, reduced_quantity, spend_ouo_reduce};
use vike_model::{
    ContingencyType, LadderSpec, OrderDrop, OrderKind, OrderList, OrderListLeg, OrderRequest,
    OrderResting, OrderType, TimeInForce,
};

use super::sizing_gate::REDUCE_ONLY_REFUSED;
use super::state::{OrderBrokerSimulator, SizingGateTerms};
use super::traits::{limit_and_trigger, priced_kind};

/// The base of the `OrderResting::qid`s order-list legs take (the first leg is one above it). The
/// queue model mints its identities from 1, one per order it sees, so it never reaches this range.
pub(crate) const GROUP_QUEUE_ID: u64 = 1 << 62;

/// A leg's handle: its qid in words, `g1` for the first leg a run places.
pub(crate) fn leg_handle(qid: u64) -> String {
    format!("g{}", qid - GROUP_QUEUE_ID)
}

/// [`leg_handle`]'s inverse.
pub(crate) fn leg_queue_id(handle: &str) -> Option<u64> {
    handle.strip_prefix('g')?.parse::<u64>().ok().map(|n| n + GROUP_QUEUE_ID)
}

/// The words a held OTO child is cancelled in when its parent ended without a fill (a cap veto, the
/// grid, a reduce-only drop, an implicit stop's cancel): it can never arm.
pub(crate) const PARENT_ENDED: &str = "parent-ended: the order that arms it ended without a fill";

/// The order lists of one run: the order-client-id-linked book over every leg's handle, the legs minted so far,
/// the OTO children held until their parent fills (`(symbol index, gate terms, order)`), and the
/// current fill pass: each leg it triggered at its size before the one-bar cap (`(symbol index,
/// qid, size)`), and each leg it filled (`(handle, qty, whether that OUO fill shares its quantity)`).
#[derive(Default)]
pub(crate) struct OrderLists {
    pub(crate) book: ContingencyBook,
    minted: u64,
    parked: Vec<(usize, SizingGateTerms, OrderResting)>,
    pass_sizes: Vec<(usize, u64, f64)>,
    pass_fills: Vec<(String, f64, bool)>,
    /// The handle of the leg whose pre-trade judgement is in progress (set around its
    /// `OrderBrokerSimulator::gate_order` and cleared after, live's `Engine::linked_exits`), so the spot
    /// rule counts it with its group.
    placing: Option<String>,
}

/// The kinds a leg rests as here: those an order list's leg can state in full today.
fn leg_kind_refusal(r: &OrderRequest) -> Option<String> {
    let kind_ok = matches!(
        r.order_type,
        OrderType::Market
            | OrderType::Limit
            | OrderType::Stop
            | OrderType::StopLimit
            | OrderType::TakeProfit
            | OrderType::Trailing
    );
    if !kind_ok || r.time_in_force != TimeInForce::Gtc {
        return Some(format!(
            "an order list's leg rests here as a GTC market, limit, stop, stop-limit, take-profit \
             or trailing order, not a {} {} one",
            r.time_in_force.as_word(),
            r.order_type
        ));
    }
    if r.order_type == OrderType::Limit && !r.price.is_some_and(|p| p > 0.0) {
        return Some("a limit leg needs a price above zero".to_string());
    }
    vike_model::order_terms_refusal(
        &r.order_type,
        r.price,
        r.trigger_price,
        r.trail,
        None,
        None,
        None,
    )
}

/// The terms the pre-trade gate judges a leg by: its own, a trailing stop's as a `Stop` with no price
/// (`OrderBrokerSimulator::rest_trailing`'s rule, and the tagged `vike_model::OrderBroker::place`'s).
fn gate_terms(r: &OrderRequest) -> SizingGateTerms {
    match r.order_type {
        OrderType::Trailing => {
            SizingGateTerms { order_type: OrderType::Stop, price: None, trigger: None }
        }
        _ => SizingGateTerms {
            order_type: r.order_type.clone(),
            price: r.price,
            trigger: r.trigger_price,
        },
    }
}

impl OrderBrokerSimulator {
    /// An ORDER LIST on `spec.symbol`: its legs linked OTO, OCO or OUO as
    /// [`vike_model::build_order_list`] states them, every leg on this run's one account (the venue
    /// and account it names are the live door's to judge). Returns each leg's handle, in leg order;
    /// nothing when refused, whole, in words, in [`Self::dropped`] (this module's doc says what is
    /// judged and how each link resolves on a fill). A leg's sizes are its own (no sizer), its weight
    /// 0. An order list inside [`Self::submit_released`] is refused: a release holds one order.
    pub fn submit_group(&mut self, spec: &OrderList) -> Vec<String> {
        let si = self.index(&spec.symbol);
        if self.is_resolved_index(si) {
            return Vec::new(); // a settled market takes nothing, unrecorded (`push_pending`)
        }
        // `g1` first; a portfolio run's leg carries its mount (`mounts.rs`)
        let first = GROUP_QUEUE_ID + self.mount_bits() + self.groups.minted + 1;
        let handles: Vec<String> =
            (0..spec.legs.len() as u64).map(|k| leg_handle(first + k)).collect();
        let legs = match self.judge_group(si, spec, &handles) {
            Ok(legs) => legs,
            Err(why) => {
                for h in &handles {
                    self.groups.book.remove(h); // the links judging noted
                }
                if let Some(why) = why {
                    let qty = spec.legs.first().map_or(0.0, |l| l.qty);
                    self.dropped.push(OrderDrop {
                        symbol: self.symbols[si].clone(),
                        reason: why,
                        size: qty,
                        weight: 0.0,
                    });
                }
                return Vec::new();
            }
        };
        self.groups.minted += handles.len() as u64;
        for (k, (r, size)) in legs.into_iter().enumerate() {
            let mut o = vike_model::OrderResting::from_request(&r);
            o.size = size;
            o.reduce_only = r.reduce_only;
            o.qid = first + k as u64;
            if r.parent_order_id.is_some() {
                self.groups.parked.push((si, gate_terms(&r), o));
            } else {
                self.seed_leg(si, &mut o);
                self.push_pending(si, o);
            }
        }
        handles
    }

    /// Two legs on `symbol`, each cancelling the other on its fill: an OCO pair (a straddle of two
    /// entries, or a stop and a target over a position). Their links are set here (no parent, each
    /// linked to the other, `Oco`), whatever `a` and `b` stated.
    pub fn submit_oco(&mut self, symbol: &str, a: OrderListLeg, b: OrderListLeg) -> Vec<String> {
        let link = |mut leg: OrderListLeg, other: usize| {
            leg.parent = None;
            leg.contingency = Some(ContingencyType::Oco);
            leg.linked = vec![other];
            leg
        };
        let legs = vec![link(a, 1), link(b, 0)];
        let spec =
            OrderList { venue: String::new(), symbol: symbol.to_string(), legs, account: None };
        self.submit_group(&spec)
    }

    /// A bracket LADDER ([`vike_model::build_ladder`]): the entry, then, armed by its first fill, a
    /// reduce-only stop for its whole size and N partial targets, the stop and each target linked OUO.
    /// A ladder the builder refuses is dropped in its words.
    pub fn submit_ladder(&mut self, spec: &LadderSpec) -> Vec<String> {
        match vike_model::build_ladder(spec) {
            Ok(list) => self.submit_group(&list),
            Err(why) => {
                let si = self.index(&spec.symbol);
                self.dropped.push(OrderDrop {
                    symbol: self.symbols[si].clone(),
                    reason: why,
                    size: spec.qty,
                    weight: 0.0,
                });
                Vec::new()
            }
        }
    }

    /// [`Self::submit_group`]'s whole-or-nothing intake: every leg's request and the size it rests at,
    /// or the refusal (`None` when the pre-trade gate refused a leg and already said so in its words).
    /// The legs' links go into the book before the legs are judged, so a spot leg is judged with its
    /// group (`Self::cash_refusal`); the caller takes them out again on a refusal.
    fn judge_group(
        &mut self,
        si: usize,
        spec: &OrderList,
        handles: &[String],
    ) -> Result<Vec<(OrderRequest, f64)>, Option<String>> {
        if self.releasing.is_some() {
            return Err(Some("an order list takes no release".to_string()));
        }
        let reqs = vike_model::build_order_list(spec, handles)?;
        if let Some(why) = reqs.iter().find_map(leg_kind_refusal) {
            return Err(Some(why));
        }
        for (r, h) in reqs.iter().zip(handles) {
            let (parent, linked) = (r.parent_order_id.clone(), r.linked_order_ids.clone());
            self.groups.book.insert(h.clone(), r.contingency_type, parent, linked);
        }
        // The mount leverage cap judges the list WHOLE, now, after its links and terms and before the
        // engine's refusals below, as the live drain does: every leg at its own price, linked (OCO/OUO) legs
        // as alternatives, a held OTO child counting where its parent filled. `gate_order` leaves the cap to
        // this for every list leg (`OrderLists::placing`), at its placing and at its parent's fill.
        let judged: Vec<MountLeverageLeg> = spec
            .legs
            .iter()
            .map(|l| MountLeverageLeg {
                side: l.side,
                quantity: l.qty,
                price: l.price.or(l.trigger_price),
                parent: l.parent,
                linked: l.linked.clone(),
            })
            .collect();
        if let Some(why) = self.leverage_refusal(&[(si, judged.as_slice())]) {
            return Err(Some(why));
        }
        let pos = self.symbol[si].pos.size;
        let mut legs = Vec::with_capacity(reqs.len());
        for (r, h) in reqs.into_iter().zip(handles) {
            let mut size = r.qty;
            // a held child meets the other gates when its parent fills, against the account as it is then
            if r.parent_order_id.is_none() {
                if r.reduce_only {
                    if !vike_model::is_reducing_direction(r.side, pos) {
                        return Err(Some(REDUCE_ONLY_REFUSED.to_string()));
                    }
                    size = size.min(pos.abs());
                }
                let SizingGateTerms { order_type, price, trigger } = gate_terms(&r);
                self.groups.placing = Some(h.clone());
                size = self.gate_order(si, r.side, size, 0.0, order_type, price, trigger);
                self.groups.placing = None;
                if size <= 0.0 {
                    return Err(None);
                }
            }
            legs.push((r, size));
        }
        Ok(legs)
    }

    /// Whether an order-list leg is being judged (`OrderLists`' `placing`), at its placing or at its parent's
    /// fill: the mount leverage cap judged its list whole ([`Self::judge_group`]), so `gate_order` does not.
    pub(super) fn placing_a_list_leg(&self) -> bool {
        self.groups.placing.is_some()
    }

    /// The linked group of the order-list leg being placed (`OrderLists`' `placing`), its sets of legs as
    /// their queue_ids ([`vike_execution::ContingencyBook::linked_exits`], the live core's own), and the set the
    /// leg belongs to; `None` while no leg is being placed or for a leg with no group.
    pub(super) fn placing_sets(&self) -> Option<(Vec<Vec<u64>>, usize)> {
        let h = self.groups.placing.as_deref()?;
        let linked = self.groups.book.linked_exits(h)?;
        let queue_ids = |set: &Vec<String>| set.iter().filter_map(|c| leg_queue_id(c)).collect();
        Some((linked.sets.iter().map(queue_ids).collect(), linked.own))
    }

    /// Note the order-list legs among the orders this event's fill pass triggered on `symbols[si]`,
    /// each at its size before the one-bar cap (`resolve_intrabar_fills`), which a plain OUO sibling's
    /// remainder is reckoned from ([`Self::group_leg_size`]). Every fill lane calls it before the cap;
    /// nothing is noted while no list is placed.
    pub(crate) fn open_group_pass(&mut self, si: usize, triggered: &[(OrderResting, f64)]) {
        if self.groups.book.is_empty() {
            return;
        }
        let legs = triggered.iter().filter(|(o, _)| o.qid >= GROUP_QUEUE_ID);
        self.groups.pass_sizes.extend(legs.map(|(o, _)| (si, o.qid, o.size)));
    }

    /// Close the fill pass every lane ends, its notes forgotten. A leg the pass triggered that rests
    /// nowhere now (not on its book, not held, not in flight) and is still in the book ended without a
    /// fill (a filled one left the book at its fill): it leaves the book, as the paper book retires
    /// one. A held OTO child whose parent rests nowhere is cancelled, in the words of
    /// [`PARENT_ENDED`]: a parent that filled armed its children at the fill, so this one ended
    /// without a fill. One `is_empty` while no list is placed.
    pub(crate) fn close_group_pass(&mut self) {
        let noted = std::mem::take(&mut self.groups.pass_sizes);
        self.groups.pass_fills.clear();
        if self.groups.book.is_empty() {
            return;
        }
        for (si, qid, _) in noted {
            if !self.leg_lives(si, qid) {
                self.groups.book.remove(&leg_handle(qid));
            }
        }
        let ended: Vec<(usize, String)> = self
            .groups
            .parked
            .iter()
            .filter_map(|(si, _, c)| {
                let parent = leg_queue_id(self.groups.book.parent_of(&leg_handle(c.qid))?)?;
                (!self.leg_lives(*si, parent)).then(|| (*si, leg_handle(c.qid)))
            })
            .collect();
        for (si, child) in ended {
            self.cancel_group_leg(si, &child, Some(PARENT_ENDED));
        }
    }

    /// Whether the leg `qid` of `symbols[si]` rests anywhere: on the book, held, or in flight (any
    /// order's `qid`: a tagged order's too, `vike_model::OrderBroker`'s).
    pub(super) fn leg_lives(&self, si: usize, qid: u64) -> bool {
        self.leg_rests(si, qid)
            || self.latency.iter().flat_map(|g| g.in_flight_orders(si, None)).any(|o| o.qid == qid)
    }

    /// Whether the leg `qid` of `symbols[si]` is on the book or held NOW: one still in flight under
    /// the opt-in latency model is neither yet, so an amend or a cancel delivered before it finds
    /// nothing, as a venue does.
    pub(super) fn leg_rests(&self, si: usize, qid: u64) -> bool {
        self.symbol[si].pending.iter().any(|o| o.qid == qid)
            || self.groups.parked.iter().any(|p| p.2.qid == qid)
    }

    /// The size the order `o`, triggered on this pass, may fill: its own for any order that is no
    /// order-list leg (one compare); `None` for a leg that never fills, cancelled by a sibling's fill
    /// or by the pass (then off the book here). A leg whose sibling filled EARLIER IN THIS PASS
    /// (`vike-paper`'s `resolve_in_pass`, the same law): an OCO leg, or any leg after a fill that
    /// shares its quantity (a ladder's target after its stop), is cancelled; a leg of any other OUO
    /// group fills what is left of its size before the cap less those fills, within the cap, or is
    /// cancelled when nothing is.
    pub(crate) fn group_leg_size(&mut self, si: usize, o: &OrderResting) -> Option<f64> {
        if o.qid < GROUP_QUEUE_ID {
            return Some(o.size);
        }
        let h = leg_handle(o.qid);
        if !self.groups.book.contains(&h) {
            return None;
        }
        let siblings = self.groups.book.siblings_of(&h);
        let earlier: Vec<&(String, f64, bool)> =
            self.groups.pass_fills.iter().filter(|(c, ..)| siblings.contains(c)).collect();
        if earlier.is_empty() {
            return Some(o.size);
        }
        let ouo = self.groups.book.kind_of(&h) == Some(ContingencyType::Ouo);
        let left = if !ouo || earlier.iter().any(|(_, _, shares)| *shares) {
            None
        } else {
            let by: f64 = earlier.iter().map(|(_, q, _)| q).sum();
            let sizes = &self.groups.pass_sizes;
            let size = sizes.iter().find(|(_, q, _)| *q == o.qid).map_or(o.size, |p| p.2);
            reduced_quantity(size, by)
        };
        match left {
            Some(left) => Some(o.size.min(left)),
            None => {
                self.cancel_group_leg(si, &h, None);
                None
            }
        }
    }

    /// Whether `o` is an order-list ENTRY: a leg no parent arms, that is not reduce-only (two of them
    /// triggering on one event fill in the bar's path order, `StrategyEngine::order_group_entries`).
    pub(crate) fn is_group_entry(&self, o: &OrderResting) -> bool {
        if o.qid < GROUP_QUEUE_ID || o.reduce_only {
            return false;
        }
        let h = leg_handle(o.qid);
        self.groups.book.contains(&h) && self.groups.book.parent_of(&h).is_none()
    }

    /// Resolve ONE fill of `filled` at `px` of the order `o` on `symbols[si]`, after it booked and
    /// before the strategy hears of it; a no-op for an order that is no order-list leg. OTO arms its
    /// held children, OCO cancels its siblings, OUO reduces them (this module's doc); the leg then
    /// leaves the book unless a remainder of it still rests.
    pub(crate) fn resolve_group_fill(&mut self, si: usize, o: &OrderResting, filled: f64, px: f64) {
        if o.qid < GROUP_QUEUE_ID || self.groups.book.is_empty() {
            return;
        }
        let h = leg_handle(o.qid);
        let shares = self.groups.book.ouo_shares_its_fill(&h);
        self.groups.pass_fills.push((h.clone(), filled, shares));
        let res = self.groups.book.on_fill(&h, filled);
        for child in self.groups.book.children_of(&h) {
            let qid = leg_queue_id(&child);
            let Some(k) = self.groups.parked.iter().position(|p| Some(p.2.qid) == qid) else {
                continue; // armed by an earlier fill of this leg
            };
            let (csi, terms, mut c) = self.groups.parked.remove(k);
            let SizingGateTerms { order_type, price, trigger } = terms;
            self.groups.placing = Some(child.clone());
            // judged outside a hook: under the mount that placed it (`mounts.rs`)
            let hook = std::mem::replace(&mut self.mount, Self::mount_of(c.qid));
            c.size = self.gate_order(csi, c.side, c.size, 0.0, order_type, price, trigger);
            self.mount = hook;
            self.groups.placing = None;
            if c.size > 0.0 {
                self.seed_leg(csi, &mut c);
                self.push_pending(csi, c);
            } else {
                self.cancel_group_leg(si, &child, None);
            }
        }
        for sib in res.cancel {
            self.cancel_group_leg(si, &sib, None);
        }
        // Only a sibling resting in `pending` is resized here; one that triggered on this same pass
        // is sized when the pass reaches it (`group_leg_size`).
        // shortcut: one still in flight under the opt-in latency gate keeps its size (an OUO leg is
        // a reduce-only exit, so the fill's position cap still bounds it); upgrade when an OUO group
        // of entries runs under the latency model.
        if !res.reduce.is_empty() {
            let legs: Vec<(String, f64, f64)> = res
                .reduce
                .iter()
                .filter_map(|(c, _)| {
                    let r = self.resting_leg(si, c)?;
                    Some((c.clone(), r.price.unwrap_or(px), r.size))
                })
                .collect();
            let reduced: Vec<(String, Option<f64>)> = if shares {
                spend_ouo_reduce(filled, px, legs)
            } else {
                legs.into_iter().map(|(c, _, qty)| (c, reduced_quantity(qty, filled))).collect()
            };
            for (c, left) in reduced {
                match left {
                    Some(q) => {
                        let qid = leg_queue_id(&c);
                        let st = &mut self.symbol[si];
                        if let Some(r) = st.pending.iter_mut().find(|r| Some(r.qid) == qid) {
                            r.size = q;
                        }
                    }
                    None => self.cancel_group_leg(si, &c, None),
                }
            }
        }
        if self.resting_leg(si, &h).is_none() {
            self.groups.book.remove(&h);
        }
    }

    /// The leg `handle` as it rests on `symbols[si]`'s book now, if it does.
    fn resting_leg(&self, si: usize, handle: &str) -> Option<&OrderResting> {
        let qid = leg_queue_id(handle)?;
        self.symbol[si].pending.iter().find(|o| o.qid == qid)
    }

    /// Cancel the leg `handle`: off the book, out of the list, and with it every child of it still
    /// held (it can never arm now), transitively; each held leg it takes is recorded in `why`'s
    /// words in [`Self::dropped`] when there are any. A child already armed protects what its parent
    /// filled and stays.
    pub(super) fn cancel_group_leg(&mut self, si: usize, handle: &str, why: Option<&str>) {
        let mut queue = vec![handle.to_string()];
        while let Some(h) = queue.pop() {
            let held = self.groups.book.children_of(&h).into_iter();
            queue.extend(held.filter(|c| self.groups.book.is_held(c)));
            self.groups.book.remove(&h);
            let Some(qid) = leg_queue_id(&h) else { continue };
            self.symbol[si].pending.retain(|o| o.qid != qid);
            if let Some(k) = self.groups.parked.iter().position(|p| p.2.qid == qid) {
                let (csi, _, o) = self.groups.parked.remove(k);
                if let Some(why) = why {
                    self.dropped.push(OrderDrop {
                        symbol: self.symbols[csi].clone(),
                        reason: why.to_string(),
                        size: o.size,
                        weight: 0.0,
                    });
                }
            }
        }
    }

    /// A trailing leg's extreme and activation as it starts to rest ([`Self::seed_trailing`], every
    /// trailing verb's): at its placing, or for an OTO child when its parent's fill arms it. Any other
    /// kind is left as it is.
    fn seed_leg(&self, si: usize, o: &mut OrderResting) {
        if o.kind == OrderKind::Trailing {
            let activation = o.activation;
            self.seed_trailing(si, o, activation);
        }
    }

    /// The held OTO child `qid` (waiting for its parent's fill) with the gate terms it is judged by when
    /// it arms, for an amend by tag to edit both in place.
    pub(super) fn parked_leg_mut(
        &mut self,
        qid: u64,
    ) -> Option<(&mut SizingGateTerms, &mut OrderResting)> {
        self.groups.parked.iter_mut().find(|p| p.2.qid == qid).map(|(_, t, o)| (t, o))
    }

    /// [`Self::cancel_all`]'s share: the held children on `symbols[si]` go at once, as held orders do.
    pub(super) fn drop_parked_legs(&mut self, si: usize) {
        self.groups.parked.retain(|p| p.0 != si);
    }

    /// What the mount leverage cap judges an amend of the order `qid` on `symbols[si]` to `new_qty` at
    /// `new_price` / `new_trigger` as, or `None` when nothing: the one amend rule live's `amend_leverage_hold`
    /// asks too (`vike_execution::risk::mount_leverage_amend`), the order as it rests (`o.size` what still
    /// rests, live's `qty − filled`), a PARKED OTO child with the unfilled ancestors that arm it
    /// ([`Self::unfilled_ancestors`]), as its list was.
    pub(super) fn amend_leverage_legs(
        &self,
        (si, qid): (usize, u64),
        terms: (Option<f64>, Option<f64>, Option<f64>),
    ) -> Option<Vec<MountLeverageLeg>> {
        let parked = || self.groups.parked.iter().find(|p| p.2.qid == qid).map(|p| p.2.clone());
        let (o, held) = match self.seam_order_out(si, qid) {
            Some(o) => (o, false),
            None => (parked()?, true),
        };
        let (limit, trigger) = limit_and_trigger(&o);
        let order = vike_execution::risk::MountLeverageAmended {
            side: o.side,
            left: o.size,
            kind: priced_kind(o.kind),
            limit,
            trigger,
            discretion: o.discretionary_amount,
            reduce_only: o.reduce_only,
        };
        let ancestors = if held { self.unfilled_ancestors(si, qid) } else { Vec::new() };
        let held_row = self.leverage_row_held(si);
        vike_execution::risk::mount_leverage_amend(&order, terms, held_row, ancestors)
    }

    /// The unfilled ancestors of the order-list leg `qid` on `symbols[si]`, root first, each armed by the one
    /// before (its `parent`): a parked one all of it, a resting one what still rests.
    fn unfilled_ancestors(&self, si: usize, qid: u64) -> Vec<MountLeverageLeg> {
        let mut chain = Vec::new();
        let mut handle = leg_handle(qid);
        while let Some(parent) = self.groups.book.parent_of(&handle).map(str::to_string) {
            let Some(parent_qid) = leg_queue_id(&parent) else { break };
            let parked = self.groups.parked.iter().find(|p| p.2.qid == parent_qid);
            let resting = || self.seam_order_out(si, parent_qid);
            let Some(o) = parked.map(|p| p.2.clone()).or_else(resting) else { break };
            let (terms, discretion) = (limit_and_trigger(&o), o.discretionary_amount);
            let price = vike_execution::risk::mount_leverage_price(o.side, terms, discretion);
            let quantity = o.size;
            chain.push(MountLeverageLeg { side: o.side, quantity, price, ..Default::default() });
            handle = parent;
        }
        chain.reverse();
        for (k, leg) in chain.iter_mut().enumerate() {
            leg.parent = k.checked_sub(1);
        }
        chain
    }
}
