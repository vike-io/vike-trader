//! `OrderBrokerSimulator` as the strategy's one broker, `OrderBroker`: the portable order seam (decision 0269)
//! and the reads.

use vike_marketdata::Bar;
use vike_model::{
    OrderBroker, OrderDrop, OrderEventKind, OrderKind, OrderList, OrderListLeg, OrderPlacement,
    OrderResting, OrderType,
};

use super::groups::{GROUP_QUEUE_ID, leg_handle, leg_queue_id};
use super::state::{OrderBrokerSimulator, SizingGateTerms};

impl OrderBrokerSimulator {
    /// The symbol index `""` names on the seam (`OrderBrokerSimulator::seam_si`): the FIRST (index 0), which IS
    /// the mounted (venue, symbol) series — a maker backtest mounts the maker on a one-symbol
    /// `OrderBrokerSimulator`.
    pub(super) const HFT_SI: usize = 0;
}

/// The sim engine core is the strategy's broker in backtest: DIRECT mutation, monomorphized dispatch
/// (never `dyn` — the hot loop must not pay a vtable per verb). Backtest-rich verbs (weighted/bracket
/// submits, cancel_all, target-percent, schedule) stay inherent on `OrderBrokerSimulator` for
/// `impl Strategy<OrderBrokerSimulator>` strategies. Every verb and read takes `""` as the mounted symbol,
/// [`OrderBrokerSimulator::HFT_SI`] (`OrderBrokerSimulator::seam_si`), as the live broker does (decision 0277).
///
/// The backtest ORDER seam ([`OrderBroker`], decision 0269): every kind and group placed, amended
/// and cancelled by tag on the symbol it names (`OrderBrokerSimulator::seam_si`: `""` is the mounted
/// [`OrderBrokerSimulator::HFT_SI`], one the run does not hold is refused by name, decision 0277); a pegged
/// order and an iceberg the venue does not show trade the mounted symbol alone, the one the emulated
/// book works. Each verb keeps its inherent twin's rules: a tagged order rests in the symbol's ordinary book
/// (`OrderBrokerSimulatorSymbolState::pending`), judged at its submit by the order-terms rule, the reduce-only rule and
/// the pre-trade gate as [`OrderBrokerSimulator::submit_stop_reduce`] and the trigger-kind verbs judge theirs,
/// and it fills on every fill lane by exactly the law an untagged order of its kind fills by (an
/// iceberg its venue shows itself by [`OrderBrokerSimulator::submit_limit_iceberg`]'s; a pegged order and any
/// other iceberg by the one emulation law, `emulated.rs`, their ends sent as every seam order's); an
/// order list is [`OrderBrokerSimulator::submit_group`]'s, its legs found by their handles. The tag names the
/// order by its symbol and its engine-local identity (`OrderResting::qid`, `OrderBrokerSimulator::order_tags`),
/// which is how its fill names the tag ([`OrderBrokerSimulator::tag_of`]). A run that places none is
/// byte-identical: the map stays empty and the untagged lanes never read it.
///
/// A tagged plain LIMIT (`OrderPlacement::Limit`, neither reduce-only nor triggered) placed while
/// `StrategyEngineConfig::optimistic_quotes` is set is a QUOTE of the maker law (`OrderBrokerSimulatorSymbolState::quotes`), as is
/// an emulated order's child on every run: it rests in the same book, is judged, committed, amended
/// and cancelled as any order placed by tag, and fills by `StrategyEngine::fill_tagged`'s law. Its end
/// reaches the strategy as every seam order's does ([`OrderBrokerSimulator::ended_seam_orders`]: `Filled`,
/// `Canceled`, `Denied` at its intake), as live sends a filled quote's end and a maker frees its side
/// on it (the owner's ruling of 2026-11-12); an emulated order's child alone ends unheard, its parent's
/// law hearing its fills. Without the option it is an ordinary limit like any other.
///
/// An amend and a cancel by tag take the paths the other order actions take: under the opt-in
/// latency gate each waits in flight like any order action and lands in the order it was sent, so it
/// reaches an order still in flight ([`OrderBrokerSimulator::amend_queue_id`], [`OrderBrokerSimulator::cancel_queue_id`]); under the
/// opt-in queue model an amend UP is a new order at the back of the queue; a spot sell raised past
/// what the account holds, and then any amend the pre-trade gate refuses (`RiskGate::check_modify`,
/// `OrderBrokerSimulator::amend_refusal`), is refused as live's `gate_modify` refuses it, keeping its terms.
/// Both are judged when the amend is sent, as live judges them. An emulated parent is never amended:
/// an amend of one is refused in `dropped` in the live core's words (`emulated.rs`).
impl OrderBroker for OrderBrokerSimulator {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        if self.latched_drop(symbol, qty) {
            return; // a portfolio mount its loss budget latched (`mounts.rs`)
        }
        let Some(si) = self.order_si(symbol) else {
            return self.refuse_unheld(symbol, tag, qty);
        };
        if tag.is_empty() {
            return self.place_untagged(si, side, qty, order, reduce_only);
        }
        // the broker's key of the tag (a portfolio run's mount's, `mounts.rs`); words keep `named`
        let (named, key) = (tag, self.tag_key(tag));
        let tag = key.as_ref();
        // An emulated order's child: its refusals below are reported by no end event (`refused`).
        let child = super::emulated::is_child_tag(tag);
        let quote = child
            || (self.optimistic_quotes
                && !reduce_only
                && matches!(order, OrderPlacement::Limit { .. }));
        let refused = |b: &mut Self, why: &str| {
            if !child {
                b.refused(tag, why);
            }
        };
        if self.is_resolved_index(si) {
            // a settled market takes nothing, unrecorded (`push_pending`)
            return refused(self, "the market has settled");
        }
        let symbol_name = self.symbols[si].clone();
        if self.releasing.is_some() {
            self.dropped.push(OrderDrop {
                symbol: symbol_name,
                reason: "a tagged order takes no release".into(),
                size: qty,
                weight: 0.0,
            });
            return refused(self, "a tagged order takes no release");
        }
        if self.tag_busy(tag) {
            let why = vike_model::tag_busy_reason(named);
            let drop =
                OrderDrop { symbol: symbol_name, reason: why.clone(), size: qty, weight: 0.0 };
            self.dropped.push(drop);
            return refused(self, &why);
        }
        // a peg, and a visible size the run's venue does not show itself, are the one emulation
        // law's (`emulated.rs`, decision 0275), on the mounted symbol; a shown one is the full-size
        // law, judged below
        // shortcut: a portfolio run refuses both, as an emulated child would mint under the mount of
        // whichever hook drives it; upgrade when the emulated book keys its parents by mount.
        let emulated = self.is_mounted()
            && match order {
                OrderPlacement::Pegged { .. } => true,
                OrderPlacement::Iceberg { price, display } => {
                    !self.shows_itself(side, qty, price, display)
                }
                _ => false,
            };
        if emulated {
            let why = "a pegged order or an iceberg the venue does not show is not simulated in a \
                       portfolio run";
            self.dropped.push(OrderDrop {
                symbol: symbol_name,
                reason: why.into(),
                size: qty,
                weight: 0.0,
            });
            return self.refused(tag, why);
        }
        match order {
            OrderPlacement::Pegged { .. } | OrderPlacement::Iceberg { .. }
                if si != Self::HFT_SI =>
            {
                let why = "a pegged order or an iceberg trades the run's mounted symbol alone";
                self.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: why.into(),
                    size: qty,
                    weight: 0.0,
                });
                return self.refused(tag, why);
            }
            OrderPlacement::Pegged { peg } => {
                return self.place_emulated(tag, qty, reduce_only, |b| {
                    b.submit_pegged_tagged(tag, side, qty, peg)
                });
            }
            OrderPlacement::Iceberg { price, display }
                if !self.shows_itself(side, qty, price, display) =>
            {
                return self.place_emulated(tag, qty, reduce_only, |b| {
                    b.submit_limit_display_tagged(tag, side, qty, price, display)
                });
            }
            _ => {}
        }
        // The request as live sends it (`OrderPlacement::request_terms`): its kind, its limit
        // (`price`), its trigger, its trail.
        let (order_type, price, trigger, trail) = order.request_terms();
        let kind = match order_type {
            OrderType::Limit => OrderKind::Limit,
            OrderType::Stop => OrderKind::Stop,
            OrderType::StopLimit => OrderKind::StopLimit,
            OrderType::TakeProfit => OrderKind::TakeProfit,
            OrderType::Trailing => OrderKind::Trailing,
            OrderType::Market => OrderKind::Market,
            other => unreachable!("`OrderPlacement::request_terms` states no {other:?} but a peg"),
        };
        let terms =
            vike_model::order_terms_refusal(&order_type, price, trigger, trail, None, None, None)
                .or_else(|| match order {
                    OrderPlacement::Iceberg { display, .. } => self.iceberg_refusal(qty, display),
                    _ => None,
                });
        if let Some(why) = terms {
            refused(self, &why);
            self.dropped.push(OrderDrop {
                symbol: symbol_name,
                reason: why,
                size: qty,
                weight: 0.0,
            });
            return;
        }
        // The gate judges each kind as its untagged twin does; a trailing stop as a `Stop` with no
        // price (`rest_trailing`'s rule).
        let gate = match order {
            OrderPlacement::Trailing { .. } => (OrderType::Stop, None, None),
            _ => (order_type, price, trigger),
        };
        let mut size = qty;
        // judged at its fill alone under `InBarRules::reduce_only_at_fill`
        if reduce_only && !self.in_bar.rules.reduce_only_at_fill {
            // the mount leverage cap on the STATED size, before the clamp (`Self::leverage_drop`)
            if self.leverage_drop(si, (side, size, 0.0), gate.1.or(gate.2)) {
                return refused(self, "refused by the pre-trade gate");
            }
            size = self.reduce_only_size(si, side, size);
            if size <= 0.0 {
                return refused(self, "a reduce-only order with nothing to reduce");
            }
        }
        let size = self.gate_order(si, side, size, 0.0, gate.0, gate.1, gate.2);
        if size <= 0.0 {
            return refused(self, "refused by the pre-trade gate");
        }
        // A limit kind's one price is its limit; a stop's, a stop-limit's and an if-touched order's
        // `price` is their trigger and `limit` their limit; a trailing order's trigger its activation.
        let mut o = OrderResting::new(kind, side, size);
        o.reduce_only = reduce_only;
        match kind {
            OrderKind::Market | OrderKind::Limit => o.price = price,
            OrderKind::Trailing => {
                o.trail = trail;
                self.seed_trailing(si, &mut o, trigger);
            }
            _ => {
                o.price = trigger;
                o.limit = price;
            }
        }
        o.qid = self.next_queue_id();
        // not for a child: its parent's book is out for the drive placing it, so every parent placed
        // through the seam would read as ended (`drive_emulated`)
        if !child {
            self.forget_ended_tags();
        }
        self.order_tags.insert(tag.to_string(), (si, o.qid));
        if quote {
            self.symbol[si].quotes.push((o.qid, tag.to_string()));
        }
        self.push_pending(si, o);
    }

    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        if self.latched_drop(symbol, legs.first().map_or(0.0, |(_, leg)| leg.qty)) {
            return; // a portfolio mount its loss budget latched (`mounts.rs`)
        }
        let Some(si) = self.order_si(symbol) else {
            for (tag, leg) in &legs {
                self.refuse_unheld(symbol, tag, leg.qty);
            }
            return;
        };
        let (tags, legs): (Vec<String>, Vec<OrderListLeg>) = legs.into_iter().unzip();
        let tags: Vec<String> = tags.iter().map(|t| self.seam_tag_key(t).into_owned()).collect();
        let symbol = self.symbols[si].clone();
        let spec = OrderList { venue: String::new(), symbol, legs, account: None };
        let handles = self.submit_group(&spec);
        self.forget_ended_tags();
        if handles.is_empty() {
            for tag in &tags {
                self.refused(tag, "the order list was refused whole");
            }
        }
        for (tag, qid) in tags.into_iter().zip(handles.iter().filter_map(|h| leg_queue_id(h))) {
            self.order_tags.insert(tag, (si, qid));
        }
    }

    fn amend(
        &mut self,
        tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        // an emulated parent (`emulated.rs`): refused in words, as live refuses it
        if self.refuse_emulated_modify(tag) {
            return;
        }
        let Some(&(si, qid)) = self.order_tags.get(self.seam_tag_key(tag).as_ref()) else { return };
        // The mount leverage cap judges the amend as live's drain judges it (`Self::amend_leverage_legs`),
        // alone, before the engine's refusals below; refused, the order keeps its terms.
        if let Some(legs) = self.amend_leverage_legs((si, qid), (new_qty, new_price, new_trigger))
            && let Some(why) = self.leverage_refusal(&[(si, legs.as_slice())])
        {
            let symbol_name = self.symbols[si].clone();
            let size = legs.last().map_or(0.0, |l| l.quantity);
            self.dropped.push(OrderDrop { symbol: symbol_name, reason: why, size, weight: 0.0 });
            return;
        }
        // A spot (cash) sell RAISED past what the account holds is refused as live's `gate_modify`
        // refuses it (the order's own size left out of the sells already out); then the pre-trade
        // gate judges the order as amended, as live's does. A refused amend keeps its terms. The
        // order is the one the account sees: one still in flight, or amended in flight, counts; a
        // held OTO child is judged when it arms.
        let raised = match (self.cash_by_si[si], new_qty) {
            (true, Some(q)) => {
                self.seam_order_out(si, qid).filter(|o| q > o.size).map(|o| (o.side, q))
            }
            _ => None,
        };
        if let Some((side, q)) = raised
            && let Some(why) = self.cash_refusal(si, side, q, Some(qid))
        {
            let symbol_name = self.symbols[si].clone();
            self.dropped.push(OrderDrop { symbol: symbol_name, reason: why, size: q, weight: 0.0 });
            return;
        }
        if self.risk_gate.is_some()
            && let Some(mut o) = self.seam_order_out(si, qid)
        {
            amend_order(&mut o, None, new_qty, new_price, new_trigger);
            let terms = seam_gate_terms(&o);
            if let Some(why) = self.amend_refusal(si, o.side, o.size, terms, Some(qid)) {
                let symbol_name = self.symbols[si].clone();
                self.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: why,
                    size: o.size,
                    weight: 0.0,
                });
                return;
            }
        }
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(0, new_qty.unwrap_or(0.0), new_price);
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let action = crate::latency::LatencyModelInFlightAction::AmendQueueId {
                si,
                qid,
                new_qty,
                new_price,
                new_trigger,
            };
            if !g.submit(now, &desc, action) {
                self.record_latency_reject(si, new_qty.unwrap_or(0.0), 0.0);
            }
            return;
        }
        self.amend_queue_id(si, qid, new_qty, new_price, new_trigger);
    }

    fn cancel(&mut self, tag: &str) {
        // an emulated parent (`emulated.rs`): its child is pulled and it ends, `Canceled`
        if self.cancel_emulated(tag) {
            return;
        }
        let key = self.seam_tag_key(tag);
        let tag = key.as_ref();
        let Some(&(si, qid)) = self.order_tags.get(tag) else { return };
        // An order that rests nowhere is past cancelling, and keeps its tag: one triggered beside the
        // fill under way still names it when its own fill comes; its end is filed then.
        if !self.leg_lives(si, qid) {
            return;
        }
        // Under the opt-in latency gate the cancel is HELD in flight, as `cancel_all`'s is.
        if self.latency.is_some() {
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let action = crate::latency::LatencyModelInFlightAction::CancelQueueId {
                si,
                qid,
                tag: tag.to_string(),
            };
            if !g.submit(now, &crate::latency::LatencyOrder::NONE, action) {
                // a rejected CANCEL is the dangerous one: the order stays resting
                self.record_latency_reject(si, 0.0, 0.0);
            }
            return;
        }
        self.cancel_queue_id(si, qid, tag);
    }

    /// The exchange's own book of the symbol (never the latency gate's shadow, which carries no
    /// price); `0.0` for one the run does not hold, as for a flat one.
    fn average_price(&self, symbol: &str) -> f64 {
        self.seam_si(symbol).map_or(0.0, |si| self.average_price_of_slot(si))
    }

    /// The run's symbols, in the order the run lists them (`OrderBrokerSimulator::symbols`), never the
    /// cross-section order a `DecideMode` walks them in; a portfolio run's mount's own, its own first.
    fn symbols(&self) -> &[String] {
        match self.mounts.get(self.mount) {
            Some(m) => &m.names,
            None => &self.symbols,
        }
    }

    fn place_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
    ) {
        self.file_exit_leg(symbol, exit, tag, side, qty, order);
    }

    /// Refused as an order refused at its intake is: in [`Self::dropped`], and `Denied` before the
    /// next bar.
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        let Some(si) = self.order_si(symbol) else {
            return self.refuse_unheld(symbol, tag, qty);
        };
        let symbol_name = self.symbols[si].clone();
        self.dropped.push(OrderDrop {
            symbol: symbol_name,
            reason: reason.to_string(),
            size: qty,
            weight: 0.0,
        });
        let key = self.tag_key(tag);
        self.refused(&key, reason);
    }
    /// Recorded at the broker's `now` (a bar step's are moved to its close by the bar loop, where it
    /// decided), `""` as the run's own symbol and any other name as named (a forecast is not an order:
    /// a symbol the run does not hold is not refused). Nothing else moves.
    fn publish_forecast(&mut self, symbol: &str, forecast: vike_model::Forecast) {
        let symbol = match self.seam_si(symbol) {
            Some(si) => self.symbols[si].clone(),
            None => symbol.to_string(),
        };
        self.forecasts.push(vike_model::ForecastRow { timestamp: self.now, symbol, forecast });
    }

    fn place_at_close(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, reduce_only: bool) {
        self.file_at_close(symbol, tag, side, qty, reduce_only);
    }

    /// The fill the strategy lab measured for this close (set A's fills-a-day rows, 1958 and 1192, on
    /// their list-a since `level` is the fill's exact decimal price: P5.1, 2026-11-13): a
    /// trailing order activated at `level`, which `InBarRules::trail_from_fill` judges from the walk,
    /// with a vanishing trail. The order-terms rule refuses a trail of zero (no venue takes one);
    /// `f64::MIN_POSITIVE` is absorbed exactly by every price's subtraction, so the walk fires it as
    /// a zero trail would.
    fn place_close_from_fill(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, level: f64) {
        let order = OrderPlacement::Trailing { trail: f64::MIN_POSITIVE, activation: Some(level) };
        self.place(symbol, tag, side, qty, order, true);
    }

    /// `InBarRules::reduce_only_at_fill`, the reference's backtest-only rule.
    fn rests_reduce_only_while_flat(&self) -> bool {
        self.in_bar.rules.reduce_only_at_fill
    }
    fn position(&self, symbol: &str) -> f64 {
        self.position_of_slot(self.index(symbol))
    }

    fn price(&self, symbol: &str) -> f64 {
        self.price_of(symbol)
    }

    fn equity(&self) -> f64 {
        self.equity_now()
    }

    fn bars(&self, symbol: &str) -> &[Bar] {
        self.bars_of_slot(self.index(symbol))
    }

    fn index(&self) -> usize {
        self.index
    }

    fn now(&self) -> i64 {
        self.now
    }

    /// Answered from the replayed L2 book with EXACTLY the law the fill path uses
    /// ([`vike_model::ContractValue::taker_price`] at the symbol's value: the walk's VWAP on a linear
    /// contract, its harmonic average on an inverse one) — so a strategy that gates on this read and
    /// then submits that size gets the fill it was quoted, and the two can never drift apart into a
    /// live-vs-backtest divergence.
    ///
    /// `None` on every book-less path (bar engine, vector kernel, tick replay with no `Tick::Book`
    /// series, unknown symbol) and whenever displayed depth cannot cover `qty`.
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        let si = self.seam_si(symbol)?;
        self.value_of_index(si).taker_price(self.book_at(si)?, side, qty, None)
    }

    /// Displayed size at `limit_px` or better, straight off the replayed book. `0.0` on every
    /// book-less path and for an unknown symbol — which correctly sizes a depth-sizing strategy
    /// down to no trade rather than to an imaginary one.
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        let Some(si) = self.seam_si(symbol) else { return 0.0 };
        self.book_at(si).map_or(0.0, |b| b.quantity_for_price(side, limit_px))
    }

    /// The grid this broker's pre-trade gate holds `symbol` to (`""` names the mounted symbol,
    /// `OrderBrokerSimulator::gate_grid`), with the tick a strategy reads by the owner's precedence of
    /// 2026-11-19 (`OrderBrokerSimulator::strategy_tick`): the stated `risk_limits` tick, the dated
    /// properties record's, then the symbol's CURRENT kept row's (`StrategyEngineConfig::kept_ticks`).
    /// `None` when nothing is known, or for an unknown symbol.
    fn instrument_grid(&self, symbol: &str) -> Option<vike_model::Grid> {
        self.instrument_grid_of_slot(self.seam_si(symbol)?)
    }

    /// The value every ledger site of this run books `symbol` at (`OrderBrokerSimulator::value_of_index`; `""`
    /// names the mounted symbol). `None` for an unknown symbol.
    fn contract_value(&self, symbol: &str) -> Option<vike_model::ContractValue> {
        self.value_by_si.get(self.seam_si(symbol)?).copied()
    }

    /// The input's bars closed by now, from the run's as-of view (`StrategyEngineConfig::inputs`).
    fn input_bars(&self, name: &str) -> &[Bar] {
        self.inputs.closed_bars(name)
    }

    /// The input's whole series, to the run's last event (`StrategyEngineConfig::input_whole`; `None`
    /// when the run serves none).
    fn input_whole(&self, name: &str) -> Option<&[Bar]> {
        self.inputs.whole_bars(name)
    }

    // The index reads (`vike_model::OrderBroker`'s `_at` reads): each name read above is `seam_si` (or
    // the input view's name search) and then the slot bodies below, which each index read reaches
    // through `slot_of` (a portfolio run's index is the mount's, `mounts.rs`), so an index answers what
    // its name does.

    fn position_at(&self, index: usize) -> f64 {
        self.position_of_slot(self.slot_of(index))
    }

    fn price_at(&self, index: usize) -> f64 {
        self.symbol[self.slot_of(index)].price
    }

    fn average_price_at(&self, index: usize) -> f64 {
        self.average_price_of_slot(self.slot_of(index))
    }

    fn bars_at(&self, index: usize) -> &[Bar] {
        self.bars_of_slot(self.slot_of(index))
    }

    fn contract_value_at(&self, index: usize) -> Option<vike_model::ContractValue> {
        self.value_by_si.get(self.slot_of(index)).copied()
    }

    fn input_names(&self) -> &[String] {
        self.inputs.names()
    }

    fn input_bars_at(&self, index: usize) -> &[Bar] {
        self.inputs.closed_bars_at(index)
    }

    fn input_whole_at(&self, index: usize) -> Option<&[Bar]> {
        self.inputs.whole_bars_at(index)
    }

    /// [`Self::instrument_grid`] by the symbol's index. `None` when nothing is known, or for an unknown
    /// index.
    fn instrument_grid_at(&self, index: usize) -> Option<vike_model::Grid> {
        self.instrument_grid_of_slot(self.slot_of(index))
    }

    /// Every history read is a prefix of a series fixed at construction: `bars` of the run's `Rc` series
    /// (cut at the step), the inputs of the series the view sorted (`InputView`'s module doc: only how many
    /// bars have closed moves), and the closed rows of a ledger that only appends
    /// (`vike_trade_ledger::TradeClosedLedger::apply`).
    fn history_only_grows(&self) -> bool {
        true
    }

    /// The engine's own closed-trade ledger, which it keeps on every run (`StrategyEngineConfig::closed_trades`
    /// arms only its report); a portfolio run's mount's own (`mounts.rs`), as a live mount's.
    fn closed_trades(&self) -> Option<&[vike_model::TradeClosedRow]> {
        match self.mounts.get(self.mount) {
            Some(m) => Some(m.closed.rows()),
            None => Some(self.closed_trades.rows()),
        }
    }
}

/// The kind an amend's prices land on (`vike_model::modified_prices`) for a resting order of `kind`, `None`
/// for one that amends its size only (a market or trailing order).
pub(super) fn priced_kind(kind: OrderKind) -> Option<OrderType> {
    match kind {
        OrderKind::Limit => Some(OrderType::Limit),
        OrderKind::Stop => Some(OrderType::Stop),
        OrderKind::StopLimit => Some(OrderType::StopLimit),
        OrderKind::TakeProfit => Some(OrderType::TakeProfit),
        _ => None,
    }
}

/// The limit and the trigger of the resting order `o`, as an order request states them: a limit's one price
/// is its limit, a stop kind's `price` its trigger and `limit` its limit.
pub(super) fn limit_and_trigger(o: &OrderResting) -> (Option<f64>, Option<f64>) {
    if o.kind == OrderKind::Limit { (o.price, None) } else { (o.limit, o.price) }
}

/// An amend by tag of the resting (or held) order `o`, in place: its size, then its prices where
/// `vike_model::modified_prices` puts them on its kind (a stop's one price is its trigger, kept in
/// `price`; a stop-limit's and an if-touched order's limit in `limit`). A market or trailing order
/// amends its size only. A held OTO child's gate `terms` follow, so it is judged as amended when it
/// arms.
fn amend_order(
    o: &mut OrderResting,
    terms: Option<&mut SizingGateTerms>,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    new_trigger: Option<f64>,
) {
    if let Some(q) = new_qty {
        o.size = q;
    }
    let Some(order_type) = priced_kind(o.kind) else { return };
    let (limit, trigger) = vike_model::modified_prices(&order_type, new_price, new_trigger);
    if o.kind == OrderKind::Limit {
        o.price = limit.or(o.price);
    } else {
        o.price = trigger.or(o.price);
        o.limit = limit.or(o.limit);
    }
    if let Some(t) = terms {
        t.price = limit.or(t.price);
        t.trigger = trigger.or(t.trigger);
    }
}

/// The terms the pre-trade gate judges the order `o` by, as [`OrderBroker::place`] and an order
/// list's leg (`groups.rs`'s `gate_terms`) are judged at their placing: a stop's one price is its
/// trigger, a stop-limit's and a take-profit's limit their `limit`, a trailing order a `Stop` with no
/// price; a market order has none. The seam places no other kind.
fn seam_gate_terms(o: &OrderResting) -> SizingGateTerms {
    let (order_type, price, trigger) = match o.kind {
        OrderKind::Limit => (OrderType::Limit, o.price, None),
        OrderKind::Stop => (OrderType::Stop, None, o.price),
        OrderKind::StopLimit => (OrderType::StopLimit, o.limit, o.price),
        OrderKind::TakeProfit => (OrderType::TakeProfit, o.limit, o.price),
        OrderKind::Trailing => (OrderType::Stop, None, None),
        _ => (OrderType::Market, None, None),
    };
    SizingGateTerms { order_type, price, trigger }
}

impl OrderBrokerSimulator {
    /// The STRATEGY's view of slot `si`'s position: the response-latency shadow while the opt-in latency
    /// gate is armed (see `OrderBrokerSimulator::shadow_pos`), exchange truth otherwise — and `shadow_pos`
    /// is EMPTY on every path but an armed `run_ticks`, so this is the frozen line verbatim there.
    /// `position_of` is the un-shadowed read for anything that needs the exchange's own state.
    fn position_of_slot(&self, si: usize) -> f64 {
        if !self.shadow_pos.is_empty() {
            return self.shadow_pos[si];
        }
        self.symbol[si].pos.size
    }

    fn average_price_of_slot(&self, si: usize) -> f64 {
        let pos = self.symbol[si].pos;
        if pos.size == 0.0 { 0.0 } else { pos.average_price }
    }

    /// Slot `si`'s closed bars up to & including the current step (tick runs may outrun the bar series).
    fn bars_of_slot(&self, si: usize) -> &[Bar] {
        let series: &[Bar] = &self.bars[si];
        let end = (self.index + 1).min(series.len());
        &series[..end]
    }

    /// The gate's grid of slot `si` (`OrderBrokerSimulator::gate_grid`) with the tick a strategy reads
    /// (`OrderBrokerSimulator::strategy_tick`, the owner's precedence of 2026-11-19, the current kept
    /// row's tick last). `None` when nothing is known, or for an unknown slot.
    fn instrument_grid_of_slot(&self, si: usize) -> Option<vike_model::Grid> {
        self.symbols.get(si)?;
        let gate = self.gate_grid(si);
        let tick_size = self.strategy_tick(si, gate);
        let none =
            vike_model::Grid { tick_size: None, lot_size: None, min_notional: None, min_qty: None };
        super::lookups::grid_stating_any(vike_model::Grid { tick_size, ..gate.unwrap_or(none) })
    }

    /// [`OrderBroker::amend`]'s body, at once or when its in-flight action lands: `amend_order` on the
    /// order `qid`, resting or held. Under the opt-in queue model an amend UP of a resting order is a
    /// new order at the back of the queue, as on every real venue: it takes a new identity, which its
    /// tag, its fees so far, its place among the maker law's quotes and the in-bar rules' facts on it
    /// (the exit it is a leg of, whether it meets this bar's close: `InBar::follow`) follow. A group
    /// leg keeps its handle-keyed identity, and with it its place. An order not on the book by then
    /// is not reached. Judged before it is sent ([`OrderBroker::amend`]), never here.
    pub(crate) fn amend_queue_id(
        &mut self,
        si: usize,
        qid: u64,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        if let Some(k) = self.symbol[si].pending.iter().position(|o| o.qid == qid) {
            // a new trigger goes on the grid as a placed one does (`Self::triggers_on_grid`)
            let tick = new_price.or(new_trigger).and_then(|_| self.tick_of(si));
            let o = &mut self.symbol[si].pending[k];
            let up = new_qty.is_some_and(|q| q > o.size);
            amend_order(o, None, new_qty, new_price, new_trigger);
            vike_execution::risk::working_trigger_on_grid(o, tick);
            if up && self.queue_model && qid < GROUP_QUEUE_ID {
                let fresh = self.next_queue_id();
                self.symbol[si].pending[k].qid = fresh;
                if let Some(tag) = self.tag_of(qid) {
                    self.order_tags.insert(tag, (si, fresh));
                }
                if let Some(fees) = self.order_fees.swap_remove(&qid) {
                    self.order_fees.insert(fresh, fees);
                }
                if let Some(quote) = self.symbol[si].quotes.iter_mut().find(|(q, _)| *q == qid) {
                    quote.0 = fresh;
                }
                self.in_bar.follow(qid, fresh);
            }
        } else if let Some((terms, o)) = self.parked_leg_mut(qid) {
            amend_order(o, Some(terms), new_qty, new_price, new_trigger);
        }
    }

    /// [`OrderBroker::cancel`]'s body, at once or when its in-flight action lands: the order `qid` off
    /// the book (a group leg with the held children only it could arm), its fees forgotten, and its
    /// end filed under `tag` while the tag still names it (a tag re-pointed meanwhile names a newer
    /// order; an emulated order's child files none). An order not on the book by then (it filled while
    /// the cancel was in flight, or a latency model delivered the cancel ahead of it) is not reached.
    pub(crate) fn cancel_queue_id(&mut self, si: usize, qid: u64, tag: &str) {
        self.cancel_queue_id_in(si, qid, tag, "cancelled by the strategy");
    }

    /// [`Self::cancel_queue_id`], its end `Canceled` in `reason`'s words (a portfolio mount's latch
    /// cancels in its own, `mounts.rs`).
    pub(super) fn cancel_queue_id_in(&mut self, si: usize, qid: u64, tag: &str, reason: &str) {
        if !self.leg_rests(si, qid) {
            return;
        }
        if self.order_tags.get(tag) == Some(&(si, qid)) {
            self.order_tags.shift_remove(tag);
            self.seam_filled.retain(|t| t != tag);
            if !super::emulated::is_child_tag(tag) {
                let reason = reason.to_string();
                self.seam_ended.push(ended(tag, OrderEventKind::Canceled { reason }));
            }
        }
        if qid >= GROUP_QUEUE_ID {
            self.cancel_group_leg(si, &leg_handle(qid), None);
        } else {
            self.symbol[si].pending.retain(|o| o.qid != qid);
        }
        self.order_fees.swap_remove(&qid);
    }

    /// The order `qid` on `symbols[si]` as the account sees it: on the book, or still in flight under
    /// the opt-in latency model, every amend and cancel of it still in flight applied in the order it
    /// was sent. `None` for an order no venue holds (a held OTO child, judged when it arms) or one
    /// resting nowhere.
    pub(super) fn seam_order_out(&self, si: usize, qid: u64) -> Option<OrderResting> {
        use crate::latency::LatencyModelInFlightAction as A;
        let mut out = self.symbol[si].pending.iter().find(|o| o.qid == qid).cloned();
        for f in self.latency.iter().flat_map(|g| &g.queue) {
            match &f.action {
                A::Push { order, .. } if order.qid == qid => out = Some(order.clone()),
                A::AmendQueueId { qid: q, new_qty, new_price, new_trigger, .. } if *q == qid => {
                    if let Some(o) = out.as_mut() {
                        amend_order(o, None, *new_qty, *new_price, *new_trigger);
                    }
                }
                A::CancelQueueId { qid: q, .. } if *q == qid => out = None,
                _ => {}
            }
        }
        out
    }

    /// The tag the order `qid` was placed under through [`OrderBroker`], or `None` for every other
    /// order (one `is_empty` while none was placed): what a fill of that order names
    /// ([`vike_model::Fill::tag`]). A tag re-pointed at a newer order no longer names the old one. In a
    /// portfolio run the tag is keyed, and an untagged order names its mount's untagged key
    /// (`mounts.rs`), so its fill reaches the mount that placed it.
    pub(crate) fn tag_of(&self, qid: u64) -> Option<String> {
        if self.order_tags.is_empty() && !self.is_mounted() {
            return None;
        }
        // a qid is minted once per run, so it names one order whatever its symbol
        let tag = self.order_tags.iter().find(|(_, (_, q))| *q == qid).map(|(tag, _)| tag.clone());
        match tag {
            None if self.is_mounted() => Some(Self::untagged_key(Self::mount_of(qid))),
            tag => tag,
        }
    }

    /// Forget every tag whose order rests nowhere any more (filled, cancelled, dropped), before a new
    /// one is filed, so the map holds the live orders and what was placed since, and file each
    /// order's end ([`Self::ended_seam_orders`]), a quote's of the maker law included (its entry in
    /// `OrderBrokerSimulatorSymbolState::quotes` goes with it), but an emulated order's child's. Not while a fill is
    /// delivered: an order that triggered beside that fill rests nowhere either, and its own fill,
    /// still to come in the same pass, must name its tag.
    fn forget_ended_tags(&mut self) {
        if self.in_bar.delivering.is_some() {
            return;
        }
        let mut gone: Vec<String> = self
            .order_tags
            .iter()
            .filter(|(_, (si, qid))| !self.leg_lives(*si, *qid))
            .map(|(tag, _)| tag.clone())
            .collect();
        // the emulated book works the mounted symbol alone (`OrderBroker::place`)
        let emulated = &self.symbol[Self::HFT_SI].emulated;
        gone.extend(
            self.seam_parents.iter().filter(|t| !emulated.contains_key(t.as_str())).cloned(),
        );
        for tag in gone {
            self.order_tags.shift_remove(&tag);
            self.seam_parents.retain(|t| *t != tag);
            let filled = self.seam_filled.iter().position(|t| *t == tag);
            if let Some(at) = filled {
                self.seam_filled.swap_remove(at);
            }
            // a child's fills reach its parent's law and the strategy under the parent's tag
            if super::emulated::is_child_tag(&tag) {
                continue;
            }
            let kind = match filled {
                Some(_) => OrderEventKind::Filled,
                None => OrderEventKind::Canceled { reason: "ended in the simulator".to_string() },
            };
            self.seam_ended.push(ended(&tag, kind));
        }
        for si in 0..self.symbol.len() {
            if !self.symbol[si].quotes.is_empty() {
                let quotes = std::mem::take(&mut self.symbol[si].quotes);
                let lives = |(q, _): &(u64, String)| self.leg_lives(si, *q);
                self.symbol[si].quotes = quotes.into_iter().filter(lives).collect();
            }
        }
    }

    /// Whether the order `qid` of `symbols[si]` is a quote of the maker law (`OrderBrokerSimulatorSymbolState::quotes`).
    /// One `is_empty` for every run that places none.
    pub(crate) fn is_quote(&self, si: usize, qid: u64) -> bool {
        let quotes = &self.symbol[si].quotes;
        !quotes.is_empty() && quotes.iter().any(|(q, _)| *q == qid)
    }

    /// Whether a quote of the maker law rests on `symbols[si]`'s book.
    pub(crate) fn rests_a_quote(&self, si: usize) -> bool {
        let st = &self.symbol[si];
        !st.quotes.is_empty() && st.pending.iter().any(|o| self.is_quote(si, o.qid))
    }

    /// Whether any order but a quote of the maker law rests on `symbols[si]`'s book.
    pub(crate) fn rests_beside_quotes(&self, si: usize) -> bool {
        self.symbol[si].pending.iter().any(|o| !self.is_quote(si, o.qid))
    }

    /// The quotes resting on `symbols[si]`'s book, `(tag, qid)` in book order: what the maker law's
    /// pass visits (`StrategyEngine::fill_tagged`).
    pub(crate) fn resting_quotes(&self, si: usize) -> Vec<(String, u64)> {
        let st = &self.symbol[si];
        st.pending
            .iter()
            .filter_map(|o| st.quotes.iter().find(|(q, _)| *q == o.qid))
            .map(|(qid, tag)| (tag.clone(), *qid))
            .collect()
    }

    /// Where on `symbols[si]`'s book the quote the maker law's pass visits as `(tag, qid)` rests NOW:
    /// `qid` while it rests, else the quote `tag` names when it was placed since the pass began (one
    /// not in `seen`, the pass's snapshot), as an `on_fill` earlier in the pass may have pulled it and
    /// placed another. `None` once the tag names no resting quote.
    pub(crate) fn quote_now(
        &self,
        si: usize,
        tag: &str,
        qid: u64,
        seen: &[(String, u64)],
    ) -> Option<usize> {
        let book = &self.symbol[si].pending;
        if let Some(k) = book.iter().position(|o| o.qid == qid) {
            return Some(k);
        }
        let (at, now) = *self.order_tags.get(tag)?;
        if at != si || seen.iter().any(|(_, q)| *q == now) || !self.is_quote(si, now) {
            return None;
        }
        book.iter().position(|o| o.qid == now)
    }

    /// `cancel_all`'s pull on `symbols[si]`: every resting order but the maker law's quotes, which
    /// only a cancel by tag pulls.
    pub(crate) fn pull_book_orders(&mut self, si: usize) {
        if self.symbol[si].quotes.is_empty() {
            self.symbol[si].pending.clear();
            return;
        }
        let st = &mut self.symbol[si];
        let quotes = &st.quotes;
        st.pending.retain(|o| quotes.iter().any(|(q, _)| *q == o.qid));
    }

    /// Whether the run's venue shows a limit's visible size itself, so the full-size law fills it
    /// (`emulated.rs`'s route), rather than the emulation law.
    fn shows_itself(&self, side: i32, qty: f64, price: f64, display: f64) -> bool {
        let req = vike_model::OrderRequest {
            order_type: OrderType::Limit,
            price: Some(price),
            display_quantity: Some(display),
            ..self.parent_request(side, qty)
        };
        self.emulation_route(&req) == Ok(None)
    }

    /// A seam order `submit` hands to the emulated book (`emulated.rs`): filed under `tag`, so its end
    /// reaches the strategy as any seam order's does (`Filled` once a fill named it, else `Canceled`);
    /// one the book refused is `Denied` in the words it was dropped with. The book holds no reduce-only
    /// parent, so one is refused in words.
    fn place_emulated(
        &mut self,
        tag: &str,
        qty: f64,
        reduce_only: bool,
        submit: impl FnOnce(&mut Self),
    ) {
        if reduce_only {
            let why = "a reduce-only pegged order or iceberg has no emulation in the simulator";
            self.dropped.push(OrderDrop {
                symbol: self.symbols[Self::HFT_SI].clone(),
                reason: why.into(),
                size: qty,
                weight: 0.0,
            });
            return self.refused(tag, why);
        }
        let before = self.dropped.len();
        submit(self);
        if self.symbol[Self::HFT_SI].emulated.contains_key(tag) {
            if !self.seam_parents.iter().any(|t| t == tag) {
                self.seam_parents.push(tag.to_string());
            }
            return;
        }
        let why = match self.dropped.get(before) {
            Some(drop) => drop.reason.clone(),
            None if self.is_resolved_index(Self::HFT_SI) => "the market has settled".to_string(),
            None => "refused by the pre-trade gate".to_string(),
        };
        self.refused(tag, &why);
    }

    /// Whether `tag` is BUSY (P1, the owner's ruling of 2026-11-10): it names an order still working
    /// that no cancel was sent for — on its book, held or in flight with no cancel of it in flight —
    /// or an emulated parent still working. A place under a busy tag is refused by name
    /// (`vike_model::tag_busy_reason`): the older order keeps its tag, an `amend` changes it, a
    /// `cancel` then a `place` replaces it.
    fn tag_busy(&self, tag: &str) -> bool {
        use crate::latency::LatencyModelInFlightAction as A;
        if let Some(&(si, qid)) = self.order_tags.get(tag) {
            let pulled = self.latency.iter().flat_map(|g| &g.queue).any(|f| match &f.action {
                A::CancelQueueId { qid: q, .. } => *q == qid,
                // `cancel_all` pulls every order but the maker law's quotes
                A::CancelAll { si: s } => *s == si && !self.is_quote(si, qid),
                _ => false,
            });
            if self.leg_lives(si, qid) && !pulled {
                return true;
            }
        }
        self.seam_parents.iter().any(|t| t == tag)
            && self.symbol[Self::HFT_SI].emulated.contains_key(tag)
    }

    /// The order `tag` was refused at its intake: its end is `Denied`, in the words of `why`. An
    /// untagged one (`""`) ends unheard, as every untagged order does.
    fn refused(&mut self, tag: &str, why: &str) {
        if !tag.is_empty() {
            self.seam_ended.push(ended(tag, OrderEventKind::Denied { reason: why.to_string() }));
        }
    }

    /// An untagged seam order (decision 0277's rule 2): the inherent lane of its kind, as
    /// the deleted `Broker::submit_market` / `submit_limit` and the trigger-kind verbs place it (raw, no
    /// sizer), filed under no tag and heard by no seam event, so a strategy moved onto the seam fills
    /// and hears exactly what it did. A kind with no untagged lane here is refused into `dropped`, in
    /// words (a pegged order and an iceberg are worked by their tag).
    fn place_untagged(
        &mut self,
        si: usize,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        let symbol_name = self.symbols[si].clone();
        match (order, reduce_only) {
            (OrderPlacement::Market, false) => {
                self.submit(&symbol_name, side, qty, 0.0, true, None)
            }
            (OrderPlacement::Limit { price }, false) => OrderBrokerSimulator::submit_limit(
                self,
                &symbol_name,
                side,
                qty,
                price,
                0.0,
                true,
                None,
            ),
            (OrderPlacement::Stop { trigger }, true) => {
                self.submit_stop_reduce(&symbol_name, side, qty, trigger)
            }
            (OrderPlacement::StopLimit { trigger, limit }, false) => {
                self.submit_stop_limit(&symbol_name, side, qty, trigger, limit, 0.0, true)
            }
            (OrderPlacement::TakeProfit { trigger, limit }, false) => {
                self.submit_take_profit(&symbol_name, side, qty, trigger, limit, 0.0, true)
            }
            _ => {
                let (kind, ..) = order.request_terms();
                let only = if reduce_only { " reduce-only" } else { "" };
                let why = format!("an untagged{only} {kind:?} order has no lane: give it a tag");
                self.dropped.push(OrderDrop {
                    symbol: symbol_name,
                    reason: why,
                    size: qty,
                    weight: 0.0,
                });
            }
        }
    }

    /// The order `tag` named `symbol`, which the run (a portfolio run's mount) does not hold: refused at
    /// its intake, in words, never traded on another symbol.
    fn refuse_unheld(&mut self, symbol: &str, tag: &str, qty: f64) {
        let held_by = if self.is_mounted() { "mount" } else { "run" };
        let why = format!("{symbol} is not a symbol of this {held_by}");
        let key = self.tag_key(tag);
        self.refused(&key, &why);
        self.dropped.push(OrderDrop {
            symbol: symbol.to_string(),
            reason: why,
            size: qty,
            weight: 0.0,
        });
    }

    /// The ends of the orders placed through [`OrderBroker`] since the last call, as live sends them
    /// (`vike_model::Strategy::on_order_event`, decision 0269's wave 3): `Filled` for one a fill named,
    /// `Canceled` for one the strategy cancelled or the engine ended without a fill (a reduce-only
    /// order with nothing left to reduce, an entry the cash gate or the margin model refused at its
    /// fill, a list's sibling, a quote of the maker law the grid gated at its touch), `Denied` for one
    /// refused at its intake. Both run loops deliver them (`StrategyEngine::deliver_order_ends`) after
    /// the event's fills and before the strategy's next market hook, never while a fill is delivered.
    /// Empty, and nothing allocated, for every run that places none.
    pub(crate) fn ended_seam_orders(&mut self) -> Vec<vike_model::OrderEvent> {
        if self.order_tags.is_empty() && self.seam_ended.is_empty() && self.seam_parents.is_empty()
        {
            return Vec::new();
        }
        self.forget_ended_tags();
        std::mem::take(&mut self.seam_ended)
    }

    /// A fill naming `tag` was made (delivered at once, or held by the latency model's response leg):
    /// the order's end says `Filled`.
    pub(crate) fn note_seam_fill(&mut self, tag: &str) {
        let filed = self.order_tags.contains_key(tag) || self.seam_parents.iter().any(|t| t == tag);
        if filed && !self.seam_filled.iter().any(|t| t == tag) {
            self.seam_filled.push(tag.to_string());
        }
    }
}

/// The end of the order `tag`, as the strategy hears it.
fn ended(tag: &str, kind: OrderEventKind) -> vike_model::OrderEvent {
    vike_model::OrderEvent { order_client_id: String::new(), tag: Some(tag.to_string()), kind }
}
