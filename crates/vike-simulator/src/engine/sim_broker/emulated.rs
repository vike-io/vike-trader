//! The EMULATED orders, the simulator's half (decision 0275): a `pegged` order or a limit with a visible
//! size that the run's venue does not hold, worked through ONE plain tagged limit at a time by the SAME
//! law the live core runs (`vike_execution::emulated`). The core's half is `crates/vike-core/src/runtime/emulated.rs`.
//!
//! * **Accept** ([`OrderBrokerSimulator::submit_pegged_tagged`], [`OrderBrokerSimulator::submit_limit_display_tagged`], the twins
//!   of the live core's `LiveBroker` verbs, on the mounted symbol). The route is
//!   `vike_venues::emulation_of` with the run's venue (`StrategyEngineConfig::default_venue`; none is an id off the
//!   roster): a visible size the venue shows itself is [`OrderBrokerSimulator::submit_limit_iceberg`]'s full-size law,
//!   byte for byte as before; a peg is always emulated, the simulator holding no pegged law; where no
//!   emulation exists the row's refusal stands. An emulated parent is judged by the order-terms and display
//!   rules and its law's own terms, then by the pre-trade gate WHOLE (the core's `judge_whole`), every
//!   refusal in [`OrderBrokerSimulator::dropped`] in its words.
//! * **The book** is `OrderBrokerSimulatorSymbolState::emulated`, by the strategy's tag in accepted order: each parent holds its
//!   request, its law, what its children filled and what they told it since its last drive.
//! * **Drive** ([`OrderBrokerSimulator::drive_emulated`], both run loops, after an event's fills and BEFORE the
//!   strategy's hook): each law hears its children's fills first, then the event's ONE market hook, over a
//!   view of the broker ([`OrderBrokerLaw`]) whose `position("")` is the parent's own signed filled size
//!   (never the account's: the strategy's other orders are no progress of it). A law that places nothing more
//!   ends its parent once no child of it rests, and one that ends it unfilled says why in `dropped`: a chase
//!   on a bar run so ends in `vike_execution::emulated::BAR_RUN_REFUSAL`'s words; an iceberg runs on bars.
//! * **Children** are plain tagged limits under an internal tag the parent owns (`child_tag`), placed
//!   through the order seam's `place` and resting in `pending` as quotes of the maker law
//!   (`OrderBrokerSimulatorSymbolState::quotes`) on every run, whatever `StrategyEngineConfig::optimistic_quotes` says, so that law
//!   fills them on every lane, the queue model's included. A child's fill counts toward its parent and
//!   reaches the strategy under the PARENT's tag ([`OrderBrokerSimulator::hear_child_fill`]). A child fires no order
//!   event (a strategy's own quote's end does), so the law frees a child's slot by its fills
//!   (`vike_execution::emulated::OrderEmulationChildSlot::on_fill`) and the strategy hears no `Accepted` nor end of a child.
//! * **Cancel, modify.** `OrderBroker::cancel` of a parent's tag pulls its child and ends it, and a new
//!   parent sent by its verb under a working parent's tag replaces it the same way (the order seam's
//!   `place` refuses a busy tag instead, P1); an `OrderBroker::amend` of it is refused in the core's words
//!   (`vike_execution::emulated::modify_refusal`).
//!   A settled market ends every parent.
//!
//! One difference from the core is stated, not hidden: the core works a parent at its drain-loop boundary,
//! after the hook that sent it, and re-prices once per drained batch; here a parent sent in a hook first
//! works on the NEXT event, and every event is its own batch.

use vike_execution::emulated::{ADOPTION_DEADLINE_MS, DONE_EPS, OrderEmulationLaw, modify_refusal};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::{
    AccountTier, Fill, OrderBroker, OrderDrop, OrderListLeg, OrderPeg, OrderPlacement,
    OrderRequest, OrderType,
};
use vike_venues::OrderEmulationKind;

use super::state::OrderBrokerSimulator;

/// What a child's tag starts with, before its parent's own tag: a control character no strategy types,
/// so a child's tag never meets one of the strategy's.
const CHILD_TAG: &str = "\u{0}emulated:";

/// The tag the child of the parent `tag` rests under.
fn child_tag(tag: &str) -> String {
    format!("{CHILD_TAG}{tag}")
}

/// Whether `tag` is an emulated parent's child's (`child_tag`): a quote of the maker law on every run.
pub(super) fn is_child_tag(tag: &str) -> bool {
    tag.starts_with(CHILD_TAG)
}

/// One working emulated parent of `OrderBrokerSimulatorSymbolState::emulated`.
pub(crate) struct EmulatedOrder {
    /// The parent as accepted.
    request: OrderRequest,
    law: OrderEmulationLaw,
    /// The tag its child rests under (`child_tag`).
    child: String,
    /// What its children filled, unsigned.
    filled: f64,
    /// Its children's fills since its last drive.
    heard: Vec<Fill>,
}

/// The event a drive hands every law: the one the strategy's hook is about to see.
pub(crate) enum OrderEmulationMarketHook<'a> {
    Bar(&'a Bar),
    Quote(&'a TickQuote),
    Trade(&'a TickTrade),
    Book(&'a L2Book),
}

/// The broker one parent's law acts on: the `OrderBrokerSimulator`'s every read and verb, but its position is the
/// parent's own signed filled size and its tagged verbs name the parent's child, whatever tag the law uses.
struct OrderBrokerLaw<'a> {
    sim: &'a mut OrderBrokerSimulator,
    child: &'a str,
    position: f64,
}

/// The law's child verbs (`vike_execution::emulated::OrderEmulationChildSlot`'s plain limit, its price amend, its cancel) name
/// the parent's child, whatever tag the law uses, in the tagged lane its children always rested in; any
/// other verb reaches the simulator's seam as itself.
impl OrderBroker for OrderBrokerLaw<'_> {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        match order {
            OrderPlacement::Limit { .. } if !reduce_only => {
                OrderBroker::place(self.sim, "", self.child, side, qty, order, false);
            }
            _ => OrderBroker::place(self.sim, symbol, tag, side, qty, order, reduce_only),
        }
    }
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        OrderBroker::place_group(self.sim, symbol, legs);
    }
    fn amend(
        &mut self,
        _law_tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        debug_assert!(new_trigger.is_none(), "a child is a plain limit: no trigger to amend");
        OrderBroker::amend(self.sim, self.child, new_qty, new_price, None);
    }
    fn cancel(&mut self, _law_tag: &str) {
        OrderBroker::cancel(self.sim, self.child);
    }
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        OrderBroker::deny(self.sim, symbol, tag, qty, reason);
    }
    fn publish_forecast(&mut self, symbol: &str, forecast: vike_model::Forecast) {
        OrderBroker::publish_forecast(self.sim, symbol, forecast);
    }
    fn average_price(&self, symbol: &str) -> f64 {
        OrderBroker::average_price(&*self.sim, symbol)
    }
    /// The law's own position (`""`) is its parent's filled size; a named symbol is the simulator's.
    fn position(&self, symbol: &str) -> f64 {
        if symbol.is_empty() { self.position } else { OrderBroker::position(&*self.sim, symbol) }
    }
    fn price(&self, symbol: &str) -> f64 {
        OrderBroker::price(&*self.sim, symbol)
    }
    fn equity(&self) -> f64 {
        OrderBroker::equity(&*self.sim)
    }
    fn bars(&self, symbol: &str) -> &[Bar] {
        OrderBroker::bars(&*self.sim, symbol)
    }
    fn index(&self) -> usize {
        OrderBroker::index(&*self.sim)
    }
    fn now(&self) -> i64 {
        OrderBroker::now(&*self.sim)
    }
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        self.sim.quote_vwap(symbol, side, qty)
    }
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        self.sim.depth_within_price(symbol, side, limit_px)
    }
    /// The grid the engine's own gate applies (`vike_execution::emulated`'s rule), never a kept row's tick:
    /// that is a strategy's read alone.
    fn instrument_grid(&self, symbol: &str) -> Option<vike_model::Grid> {
        self.sim.gate_grid(self.sim.seam_si(symbol)?)
    }
    fn contract_value(&self, symbol: &str) -> Option<vike_model::ContractValue> {
        self.sim.contract_value(symbol)
    }
    fn input_bars(&self, name: &str) -> &[Bar] {
        self.sim.input_bars(name)
    }
    fn input_whole(&self, name: &str) -> Option<&[Bar]> {
        self.sim.input_whole(name)
    }
    fn closed_trades(&self) -> Option<&[vike_model::TradeClosedRow]> {
        self.sim.closed_trades()
    }
}

impl OrderBrokerSimulator {
    /// A TAGGED **pegged** order on the mounted symbol (`LiveBroker::submit_pegged_tagged`'s twin): it rests
    /// at `peg`'s anchor moved by its offset. The simulator holds no pegged law, so it is EMULATED (the
    /// module doc): one plain limit at a time, re-priced on every move of its anchor, its fills under `tag`.
    /// Refused in words in [`Self::dropped`] by the order-terms rule, the law's terms, a venue with no
    /// emulated peg (the row's words) and the pre-trade gate. An amend of it is refused in words.
    pub fn submit_pegged_tagged(&mut self, tag: &str, side: i32, qty: f64, peg: OrderPeg) {
        let req = OrderRequest {
            order_type: OrderType::Pegged,
            peg: Some(peg),
            ..self.parent_request(side, qty)
        };
        let why = vike_model::order_terms_refusal(
            &req.order_type,
            None,
            None,
            None,
            None,
            Some(&peg),
            None,
        )
        .or(self.emulation_route(&req).err());
        self.arm_emulated(tag, req, why, OrderEmulationKind::Chase);
    }

    /// A TAGGED limit with a VISIBLE size on the mounted symbol (`LiveBroker::submit_limit_display_tagged`'s
    /// twin): `display` of `qty` shows at `price`. A venue that shows a visible size itself gets
    /// [`Self::submit_limit_iceberg`]'s full-size law, byte for byte (its tag names nothing there: that law
    /// is untagged); everywhere else it is EMULATED (the module doc): one plain limit of `display` at a time
    /// at `price`, its fills under `tag`. Refused in words in [`Self::dropped`] by the order-terms and display
    /// rules, the law's terms, a venue with no emulation (the row's words) and the pre-trade gate.
    pub fn submit_limit_display_tagged(
        &mut self,
        tag: &str,
        side: i32,
        qty: f64,
        price: f64,
        display: f64,
    ) {
        let req = OrderRequest {
            order_type: OrderType::Limit,
            price: Some(price),
            display_quantity: Some(display),
            ..self.parent_request(side, qty)
        };
        let route = self.emulation_route(&req);
        if route == Ok(None) {
            let symbol = self.symbols[Self::HFT_SI].clone();
            return self.submit_limit_iceberg(&symbol, side, qty, price, display, 0.0, true, None);
        }
        let why = vike_model::order_terms_refusal(
            &req.order_type,
            Some(price),
            None,
            None,
            None,
            None,
            None,
        )
        .or_else(|| vike_model::display_refusal(&req.order_type, qty, Some(display)))
        .or(route.err());
        self.arm_emulated(tag, req, why, OrderEmulationKind::Iceberg);
    }

    /// A parent's request before its kind: the run's venue, the mounted symbol, the side and the size.
    pub(super) fn parent_request(&self, side: i32, qty: f64) -> OrderRequest {
        OrderRequest {
            venue: self.venue.clone().unwrap_or_default(),
            symbol: self.symbols[Self::HFT_SI].clone(),
            side,
            qty,
            ..OrderRequest::default()
        }
    }

    /// The route of `req` on the run's venue by `vike_venues::emulation_of`: `Ok(Some(route))` emulated,
    /// `Ok(None)` the venue's own (a visible size it shows), `Err` refused in the row's words. A peg is asked
    /// as a paper book's (`AccountTier::Paper`): no book vike runs holds one, so it is emulated even where the
    /// venue's row holds it natively.
    pub(super) fn emulation_route(
        &self,
        req: &OrderRequest,
    ) -> Result<Option<OrderEmulationKind>, String> {
        let venue = self.venue.as_deref().unwrap_or("");
        let pegged = req.order_type == OrderType::Pegged;
        let tier = pegged.then_some(AccountTier::Paper);
        if let Some(route) = vike_venues::emulation_of(req, venue, tier) {
            return Ok(Some(route));
        }
        match vike_venues::preflight_order_at(req, venue, None) {
            Err(deny) => Err(deny.to_string()),
            Ok(()) if pegged => Err(format!(
                "no emulated pegged order on {venue} (a chase there could not re-price its child in \
                 place), and the simulator holds none of its own"
            )),
            Ok(()) => Ok(None),
        }
    }

    /// Arm the parent `req` under `tag` once nothing refuses it (`why`, then its law's own terms, a release,
    /// then the pre-trade gate judging it WHOLE as a limit at its price, none for a peg); its law is
    /// `vike_execution::emulated::OrderEmulationLaw::of` the request on `route`, for the size the gate leaves. A settled
    /// market takes nothing, unrecorded, as the tagged lane.
    fn arm_emulated(
        &mut self,
        tag: &str,
        req: OrderRequest,
        why: Option<String>,
        route: OrderEmulationKind,
    ) {
        let law = |qty| {
            OrderEmulationLaw::of(&OrderRequest { qty, ..req.clone() }, route, ADOPTION_DEADLINE_MS)
        };
        let si = Self::HFT_SI;
        if self.is_resolved_index(si) {
            return;
        }
        let why = why.or_else(|| law(req.qty).refusal()).or_else(|| {
            self.releasing.is_some().then(|| "a tagged order takes no release".to_string())
        });
        if let Some(why) = why {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size: req.qty,
                weight: 0.0,
            });
            return;
        }
        let qty = self.gate_order(si, req.side, req.qty, 0.0, OrderType::Limit, req.price, None);
        if qty <= 0.0 {
            return;
        }
        self.cancel_emulated(tag);
        let parent = EmulatedOrder {
            law: law(qty),
            request: OrderRequest { qty, ..req },
            child: child_tag(tag),
            filled: 0.0,
            heard: Vec::new(),
        };
        self.symbol[si].emulated.insert(tag.to_string(), parent);
    }

    /// Work every emulated parent of `symbols[si]` on `hook`, in accepted order (the module doc's drive).
    /// Nothing while none works, so a run that sends none is byte-identical.
    pub(crate) fn drive_emulated(&mut self, si: usize, hook: &OrderEmulationMarketHook<'_>) {
        if self.symbol[si].emulated.is_empty() {
            return;
        }
        // Taken out for the pass and put back: a law's verbs reach the broker, never the book.
        let mut book = std::mem::take(&mut self.symbol[si].emulated);
        book.retain(|_, p| self.work_parent(p, hook));
        self.symbol[si].emulated = book;
    }

    /// One parent's drive: its children's fills, then the hook. `false` once it ends.
    fn work_parent(&mut self, p: &mut EmulatedOrder, hook: &OrderEmulationMarketHook<'_>) -> bool {
        let heard = std::mem::take(&mut p.heard);
        let position = f64::from(p.request.side) * p.filled;
        let law = p.law.hooks();
        let mut view = OrderBrokerLaw { sim: &mut *self, child: &p.child, position };
        for fill in &heard {
            law.on_fill(&mut view, fill);
        }
        match *hook {
            OrderEmulationMarketHook::Bar(b) => law.on_bar(&mut view, b),
            OrderEmulationMarketHook::Quote(q) => law.on_quote_tick(&mut view, q),
            OrderEmulationMarketHook::Trade(t) => law.on_trade_tick(&mut view, t),
            OrderEmulationMarketHook::Book(b) => law.on_order_book(&mut view, b),
        }
        let Some(why) = p.law.refusal() else { return true };
        // It places nothing more: it ends once its child rests nowhere, in flight included (a
        // cancel of it still in flight counts as landed).
        let child = self.order_tags.get(p.child.as_str()).copied();
        if child.is_some_and(|(si, qid)| self.seam_order_out(si, qid).is_some()) {
            return true;
        }
        if p.filled < p.request.qty * (1.0 - DONE_EPS) {
            let symbol_name = self.symbols[Self::HFT_SI].clone();
            self.dropped.push(OrderDrop {
                symbol: symbol_name,
                reason: why,
                size: p.request.qty - p.filled,
                weight: 0.0,
            });
        }
        false
    }

    /// A tagged fill of `symbols[si]`: when it is an emulated parent's child's, it counts toward the parent
    /// (which hears it at its next drive) and names the PARENT's tag, as the strategy hears it. Every other
    /// fill is untouched. A child whose parent ended (its cancel still in flight) still names the parent.
    pub(crate) fn hear_child_fill(&mut self, si: usize, fill: &mut Fill) {
        let parent = fill.tag.as_deref().and_then(|t| t.strip_prefix(CHILD_TAG));
        let Some(parent) = parent.map(str::to_string) else { return };
        if let Some(p) = self.symbol[si].emulated.get_mut(&parent) {
            p.filled += fill.size;
            p.heard.push(fill.clone());
        }
        fill.tag = Some(parent);
    }

    /// `OrderBroker::cancel`'s half: `tag` names a parent, so its child is pulled and it ends,
    /// cancelled by its owner (nothing in `dropped`). `false` iff it names none.
    pub(super) fn cancel_emulated(&mut self, tag: &str) -> bool {
        let book = &mut self.symbol[Self::HFT_SI].emulated;
        if book.is_empty() {
            return false;
        }
        let Some(parent) = book.shift_remove(tag) else { return false };
        OrderBroker::cancel(self, &parent.child);
        true
    }

    /// `OrderBroker::amend`'s half: an emulated parent is never amended (decision 0275), refused in
    /// `dropped` in the live core's words (`modify_refusal`). `false` iff `tag` names no parent.
    pub(super) fn refuse_emulated_modify(&mut self, tag: &str) -> bool {
        let book = &self.symbol[Self::HFT_SI].emulated;
        if book.is_empty() || !book.contains_key(tag) {
            return false;
        }
        self.dropped.push(OrderDrop {
            symbol: self.symbols[Self::HFT_SI].clone(),
            reason: modify_refusal(tag),
            size: 0.0,
            weight: 0.0,
        });
        true
    }
}
