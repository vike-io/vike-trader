//! Symbol lookups, the resolution latch, order-intake plumbing and the strategy-facing reads.

use vike_marketdata::OptionKind;
use vike_model::{
    ContractOption, ContractValue, Fill, NotionalKind, OrderDrop, OrderResting, Position,
    SymbolProperties, py_sum,
};

#[cfg(doc)]
use super::knobs::DecideMode;
#[cfg(doc)]
use super::params::StrategyEngineConfig;
use super::state::{OrderBrokerSimulator, OrderHeldEntry, SizingGateTerms};

/// The unrealized PnL of `pos` at `px`, in the money its contract SETTLES in: linear `size·(px −
/// avg)·mult`, the product every site of this crate spelled (`ContractValue::unrealized_pnl`'s linear
/// arm is the same two-operand products, which commute bit for bit), inverse the coin PnL
/// `s·u·(1/avg − 1/px)` through `ContractValue::in_settle`.
#[inline]
pub(crate) fn settle_unrealized_pnl(value: ContractValue, pos: &Position, px: f64) -> f64 {
    value.in_settle(value.unrealized_pnl(pos.average_price, px, pos.size), px)
}

/// An option's intrinsic value per contract at its underlying's price `u`: a call `max(0, u −
/// strike)`, a put `max(0, strike − u)`. The one law its expiry (`OrderBrokerSimulator::option_payout`) and its
/// exercise (`OrderBrokerSimulator::exercise`) settle by.
pub(super) fn intrinsic(spec: &ContractOption, u: f64) -> f64 {
    match spec.right {
        OptionKind::Call => (u - spec.strike).max(0.0),
        OptionKind::Put => (spec.strike - u).max(0.0),
    }
}

impl OrderBrokerSimulator {
    /// `symbol`'s index, `""` the mounted one as on the order seam ([`Self::seam_si`], decision
    /// 0277: the meaning the live broker gives it); a symbol the run does not hold panics.
    pub(super) fn index(&self, symbol: &str) -> usize {
        self.seam_si(symbol).unwrap_or_else(|| panic!("unknown symbol {symbol:?}"))
    }

    /// The index of the symbol a `vike_model::OrderBroker` verb names: `""` is the mounted one
    /// ([`Self::HFT_SI`]), any other its own; `None` for a symbol the run does not hold.
    pub(super) fn seam_si(&self, symbol: &str) -> Option<usize> {
        match symbol {
            "" => Some(Self::HFT_SI),
            named => self.symbols.iter().position(|s| s == named),
        }
    }

    /// The PIT grid for `symbols[si]` at `ts`, or `None` when there is no filter source, no venue
    /// key, or no record — in which case the fill path stays unconstrained.
    pub(super) fn grid_for(&self, si: usize, ts: i64) -> Option<SymbolProperties> {
        let (venue, f) = (self.venue.as_deref()?, self.properties.as_ref()?);
        f(venue, &self.symbols[si], ts)
    }

    /// The per-symbol VENUE HOLD table in NANOSECONDS the latency gate arms with, indexed like
    /// [`Self::symbols`] — each market's `SymbolProperties::taker_hold_ms` at `ts`, resolved ONCE
    /// through the same PIT [`StrategyEngineConfig::properties`] seam the fill grid uses (so a replay reads
    /// the hold the venue declared for that market, not a global constant).
    ///
    /// `ts` is the RUN START, not each action's stamp: a venue hold is a standing property of the
    /// market, and re-resolving it per submitted action would put a PIT lookup on the order path
    /// and let the in-flight queue's ordering drift mid-run.
    ///
    /// Returns EMPTY — the byte-identical, frozen path — with no properties source, no venue key,
    /// or when no symbol declares a hold (which is every venue but Polymarket).
    pub(crate) fn taker_hold_table_ns(&self, ts: i64) -> Vec<i64> {
        if self.venue.is_none() || self.properties.is_none() {
            return Vec::new();
        }
        let table: Vec<i64> = (0..self.symbols.len())
            .map(|si| {
                self.grid_for(si, ts)
                    .map_or(0, |p| i64::from(p.taker_hold_ms).saturating_mul(1_000_000))
            })
            .collect();
        if table.iter().all(|&h| h == 0) { Vec::new() } else { table }
    }

    /// The perp funding rate for `symbols[si]` at `ts` from the optional
    /// [`StrategyEngineConfig::funding_source`] seam, or `None` when there is no source, no venue key, or
    /// no funding event at this ts — in which case no funding beyond `Bar.funding` is charged.
    pub(crate) fn funding_rate_for(&self, si: usize, ts: i64) -> Option<f64> {
        let (venue, f) = (self.venue.as_deref()?, self.funding_source.as_ref()?);
        f(venue, &self.symbols[si], ts)
    }

    /// Does `symbols[si]` take its funding from [`StrategyEngineConfig::funding_schedule`]? Its
    /// `Bar.funding` then charges nothing.
    pub(crate) fn has_funding_schedule(&self, si: usize) -> bool {
        self.funding_schedule.get(si).is_some_and(Option::is_some)
    }

    /// Charge every scheduled funding of `symbols[si]` due at or before `ts` and not yet charged, on
    /// the position held NOW (the caller runs this before the step's or tick's fills), each at its own
    /// mark, else at `price` (the bar's open, the tick's price), through
    /// `ContractValue::funding`. Folds `cash` and `funding_paid` and emits the
    /// mirror row exactly as the bar rule does. No-op without a schedule.
    pub(crate) fn charge_funding_schedule(&mut self, si: usize, ts: i64, price: f64) {
        loop {
            let Some(Some(list)) = self.funding_schedule.get(si) else { return };
            let Some(&charge) = list.get(self.funding_next[si]).filter(|c| c.timestamp_ms <= ts)
            else {
                return;
            };
            self.funding_next[si] += 1;
            let size = self.symbol[si].pos.size;
            if size == 0.0 {
                continue;
            }
            let mark = charge.mark.unwrap_or(price);
            let fc = self.value_of_index(si).funding(size, mark, charge.rate);
            self.cash -= fc;
            self.funding_paid -= fc;
            if fc != 0.0
                && let Some(sink) = &mut self.mirror_funding
            {
                sink.push(super::state::MirrorFunding {
                    symbol: self.symbols[si].clone(),
                    amount: -fc,
                    timestamp: charge.timestamp_ms,
                });
            }
        }
    }

    // --- membership / caps ---

    pub fn is_active(&self, symbol: &str) -> bool {
        self.is_active_index(self.index(symbol))
    }

    pub(crate) fn is_active_index(&self, si: usize) -> bool {
        match &self.active_mask {
            None => true,
            Some(mask) => {
                let symbol = &self.symbols[si];
                mask.iter().find(|(s, _)| s == symbol).map(|(_, m)| m[self.step]).unwrap_or(true)
            }
        }
    }

    // --- binary-resolution latch (the opt-in `StrategyEngineConfig::resolution` lane) ---

    /// Has `symbol` already resolved in this run (see [`StrategyEngineConfig::resolution`])? Once true it
    /// stays true — the market is settled, so the engine REFUSES every further order for it on
    /// both lanes. Always `false` when no resolution source is configured. A strategy can read
    /// this before quoting to avoid pushing orders that would be dropped.
    ///
    /// Total: an UNKNOWN symbol answers `false` rather than panicking (unlike the write verbs'
    /// `index()`) — see the body for why a predicate differs from a write verb here.
    pub fn is_resolved(&self, symbol: &str) -> bool {
        // TOTAL on purpose — an unknown symbol is `false`, not a panic. This is public read-only
        // getter surface a strategy calls to decide whether to quote, typically from inside
        // `on_fill` / `on_bar` where a panic aborts the whole backtest. The engine's other
        // `index()` callers are WRITE verbs, where an unknown symbol is a genuine strategy bug
        // worth failing loudly on; a predicate has an honest answer for a symbol this run never
        // registered — it has not resolved here — so it returns that instead.
        match self.symbols.iter().position(|s| s == symbol) {
            Some(si) => self.is_resolved_index(si),
            None => false,
        }
    }

    /// `resolved` is EMPTY unless a resolution source is configured, so the default (parity-gated)
    /// order-intake path costs exactly one `is_empty` branch and never indexes.
    #[inline]
    pub(crate) fn is_resolved_index(&self, si: usize) -> bool {
        !self.resolved.is_empty() && self.resolved[si]
    }

    /// Latch `symbols[si]` as resolved (idempotent). Called by the engine's resolution sweep
    /// BEFORE it settles, so the settlement's own `on_fill` re-entry is already refused.
    pub(crate) fn mark_resolved(&mut self, si: usize) {
        if !self.resolved.is_empty() {
            self.resolved[si] = true;
        }
    }

    // --- option-expiry payout (the opt-in `StrategyEngineConfig::option_specs` lane) ---

    /// The PER-CONTRACT intrinsic settlement value for `symbols[si]` when it is a configured option
    /// that has expired at/before `probe_timestamp`, else `None`. CALL → `max(0, u − strike)`,
    /// PUT → `max(0, strike − u)`, where `u` is the engine's own mark for the option's `underlying`
    /// symbol ([`Self::price_of`] — the same last-seen-price field `equity_now` marks against).
    ///
    /// Returns `None` — the option is NOT settled — when: there is no source; the symbol is not an
    /// option (the source returns `None`); the spec names NO underlying (`underlying: None`, also
    /// reported once per symbol with a `tracing` warning — the rule is
    /// `crates/vike-simulator/src/engine/settlement.rs`'s module doc); the option has not yet expired
    /// (`probe_timestamp < expiry_ms`); or the `underlying` names no registered symbol (no mark to settle
    /// against). The per-contract value is intentionally NOT scaled by the contract size here —
    /// the shared [`Self::settle_at_payout`] fold applies the option symbol's multiplier (its
    /// contract size) to `qty × payout`, exactly as it did on the entry fill, so
    /// `max(0, u − K)·contract_size·qty` falls out with no second scaling and no risk of
    /// double-applying it.
    pub(crate) fn option_payout(&mut self, si: usize, probe_timestamp: i64) -> Option<f64> {
        let source = self.option_specs.as_ref()?;
        let spec = source(&self.symbols[si])?;
        let Some(underlying) = spec.underlying.as_deref() else {
            self.report_option_without_underlying(si);
            return None;
        };
        if probe_timestamp < spec.expiry_ms {
            return None;
        }
        let under_si = self.symbols.iter().position(|s| s == underlying)?;
        let u = self.symbol[under_si].price;
        Some(intrinsic(&spec, u))
    }

    /// Warn — once per symbol, through the `option_no_underlying_reported` latch — that
    /// `symbols[si]`'s option spec names no underlying, so a held position in it is never settled
    /// at expiry. Called on every probe of such a spec; only the first one logs.
    fn report_option_without_underlying(&mut self, si: usize) {
        let Some(reported) = self.option_no_underlying_reported.get_mut(si) else {
            return;
        };
        if *reported {
            return;
        }
        *reported = true;
        tracing::warn!(
            symbol = %self.symbols[si],
            "option spec names no underlying: the option has no engine mark to settle against, \
             so a position in it is NOT settled at expiry and stays held (marked at its own last \
             price); give the spec an `underlying` that is a registered symbol of the run"
        );
    }

    /// The ONE order-intake choke point for the `pending` book, a tagged quote's included: push `o`
    /// onto `symbols[si]`'s book UNLESS that symbol has resolved, in which case the order is REFUSED.
    /// Resolution is terminal — the market cannot trade again — so a strategy that submits from
    /// inside `on_fill` (the classic re-hedge / re-enter pattern) can never re-open a position in
    /// a market that settled in this very step, which would otherwise fill against the resolution
    /// bar's stale price and be settled again on the next step.
    ///
    /// Refusals are deliberately NOT recorded in `dropped`: a maker re-quoting on every tick after
    /// resolution would grow that diagnostics vec without bound. [`Self::is_resolved`] is the read
    /// a strategy or test uses instead, and the settlement itself is in [`Self::settlements`].
    ///
    /// It is also where an order submitted inside [`Self::submit_released`] is HELD instead
    /// (`OrderBrokerSimulatorSymbolState::held`, with the gate terms `Self::gate_order` deferred): the accept-time
    /// refusals run now, in words, in `dropped` — a release not in the future
    /// (`vike_model::lifetime_refusal`'s words), a price condition read on another venue (the run
    /// names an instrument by its symbol alone) and one on a symbol the run does not carry (it could
    /// never be met).
    ///
    /// A trigger joins the book on the grid (`Self::triggers_on_grid`), so it fires where a venue
    /// would have rested it.
    #[inline]
    pub(super) fn push_pending(&mut self, si: usize, mut o: OrderResting) {
        if self.is_resolved_index(si) {
            return;
        }
        self.triggers_on_grid(si, &mut o);
        if let Some(release) = self.releasing {
            let gate = self.held_terms.take();
            let (market, gtc) = (vike_model::OrderType::Market, vike_model::TimeInForce::Gtc);
            let foreign =
                release.price.filter(|c| !self.symbols.iter().any(|s| s == c.symbol.as_str()));
            // decision 0267's 2.13: reopens when the simulator keys an instrument by (venue, symbol)
            let on_venue = release.price.and_then(|c| c.venue).map(|v| {
                format!(
                    "a price condition read on another venue ({v}) is not simulated: the simulator \
                     names an instrument by its symbol alone"
                )
            });
            let why = vike_model::lifetime_refusal(&market, gtc, None, Some(release), self.now)
                .or(on_venue)
                .or_else(|| {
                    foreign.map(|c| {
                        format!("the price condition's symbol {} is not in this run", c.symbol)
                    })
                });
            match why {
                Some(why) => self.dropped.push(OrderDrop {
                    symbol: self.symbols[si].clone(),
                    reason: why,
                    size: o.size,
                    weight: o.weight,
                }),
                None => self.symbol[si].held.push(OrderHeldEntry { release, gate, order: o }),
            }
            return;
        }
        // Opt-in latency gate (tick replay only; `None` everywhere else = the frozen path).
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(o.side, o.size, o.price);
            let (now, size, weight) = (self.now, o.size, o.weight);
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &desc,
                crate::latency::LatencyModelInFlightAction::Push { si, order: o },
            );
            if !sent {
                self.record_latency_reject(si, size, weight);
            }
            return;
        }
        // last, unless `InBarRules::queue_order` places it
        let at = self.queue_at(si, &o);
        self.symbol[si].pending.insert(at, o);
    }

    /// The owner's G1, where the simulator fires a stop itself: `o`'s trigger (a stop's, a
    /// stop-limit's, a take-profit's) and the protective stop an entry carries
    /// (`OrderResting::stop`, a stop of the closing side) go on the grid the gate holds
    /// `symbols[si]` to ([`Self::tick_of`]) by the live gate's own rule
    /// (`vike_execution::risk::trigger_on_grid`). No grid: as stated, byte for byte.
    fn triggers_on_grid(&self, si: usize, o: &mut OrderResting) {
        use vike_model::{OrderKind, OrderType};
        let trigger_kind =
            matches!(o.kind, OrderKind::Stop | OrderKind::StopLimit | OrderKind::TakeProfit);
        if !trigger_kind && o.stop.is_none() {
            return; // no level to put on the grid, so no grid lookup
        }
        let tick = self.tick_of(si);
        vike_execution::risk::working_trigger_on_grid(o, tick);
        let side = -o.side;
        o.stop =
            o.stop.map(|s| vike_execution::risk::trigger_on_grid(&OrderType::Stop, side, s, tick));
    }

    /// The tick the gate holds `symbols[si]` to now ([`vike_model::OrderBroker::instrument_grid`]).
    pub(super) fn tick_of(&self, si: usize) -> Option<f64> {
        vike_model::OrderBroker::instrument_grid(self, &self.symbols[si]).and_then(|g| g.tick_size)
    }

    /// Release every held order of `symbols[si]` whose release holds at `ts` (`Release::holds`, a
    /// price condition read on [`Self::release_price`]), as the live core's release timer does: in
    /// release-time order, each is judged by the pre-trade gate NOW, against the account as it is at
    /// its release, and placed as [`Self::push_pending`] places any order, so it takes part in the
    /// event at `ts`. Every fill lane calls this before its pass over `pending`. The simulator holds
    /// no lifetime on any order, so no expiry can pass while one is held (the core's other release
    /// check). One `is_empty` while nothing is held.
    pub(crate) fn release_held(&mut self, si: usize, ts: i64) {
        if self.symbol[si].held.is_empty() {
            return;
        }
        let held = std::mem::take(&mut self.symbol[si].held);
        let (mut due, still): (Vec<OrderHeldEntry>, Vec<OrderHeldEntry>) =
            held.into_iter().partition(|h| h.release.holds(ts, |s| self.release_price(s)));
        self.symbol[si].held = still;
        due.sort_by_key(|h| h.release.at);
        for OrderHeldEntry { gate, mut order, .. } in due {
            if let Some(SizingGateTerms { order_type, price, trigger }) = gate {
                let (side, size, weight) = (order.side, order.size, order.weight);
                order.size = self.gate_order(si, side, size, weight, order_type, price, trigger);
                if order.size <= 0.0 {
                    continue;
                }
            }
            self.push_pending(si, order);
        }
    }

    /// Record an order-path action the latency model REJECTED (a negative entry latency: the
    /// request never reached the matching engine) in the existing `dropped` diagnostics channel,
    /// under the kind `"latency_reject"` — the same convention `"volume_cap"` uses.
    ///
    /// Without this a calibrated series with a rejection burst silently swallows a large fraction
    /// of a strategy's orders and the run reports nothing, leaving "why did my maker stop quoting"
    /// undiagnosable. Bounded by the number of actions the strategy issues, unlike the
    /// post-resolution refusals `push_pending` deliberately does not record.
    pub(super) fn record_latency_reject(&mut self, si: usize, size: f64, weight: f64) {
        let symbol_name = self.symbols[si].clone();
        self.dropped.push(OrderDrop {
            symbol: symbol_name,
            reason: "latency_reject".into(),
            size,
            weight,
        });
    }

    /// Apply ONE delivered in-flight action — the drain's per-action dispatch. Each arm is the
    /// zero-latency body of the matching strategy verb, so the delayed path and the immediate
    /// path share their semantics verbatim (including the resolution refusal, re-checked HERE
    /// because a market can resolve while an action is still in flight).
    pub(crate) fn apply_in_flight(&mut self, action: crate::latency::LatencyModelInFlightAction) {
        use crate::latency::LatencyModelInFlightAction as A;
        match action {
            A::Push { si, order } => {
                if !self.is_resolved_index(si) {
                    self.symbol[si].pending.push(order);
                }
            }
            A::CancelAll { si } => self.pull_book_orders(si),
            A::AmendQueueId { si, qid, new_qty, new_price, new_trigger } => {
                self.amend_queue_id(si, qid, new_qty, new_price, new_trigger);
            }
            A::CancelQueueId { si, qid, tag } => self.cancel_queue_id(si, qid, &tag),
        }
    }

    // The three open-position caps below count POSITIONS (`pos.size != 0.0`), never resting or
    // in-flight orders, and are consulted at FILL time (`dispatch_fill`), not at submit. The
    // opt-in latency gate is therefore inert for them by construction: an order held in flight is
    // no more a position than the resting `pending` order it would otherwise have been, and by the
    // time it can breach a cap it has been delivered and filled through the same check. This is
    // NOT the shape `cap_to_leverage` has — that one is a pre-trade check that sums PENDING
    // notional, so it does have to see the in-flight queue (see there).
    pub(crate) fn at_open_cap(&self) -> bool {
        let cap = self.max_open_positions;
        cap != 0 && self.symbol.iter().filter(|st| st.pos.size != 0.0).count() >= cap
    }

    pub(crate) fn at_long_cap(&self) -> bool {
        let cap = self.max_open_long;
        cap != 0 && self.symbol.iter().filter(|st| st.pos.size > 0.0).count() >= cap
    }

    pub(crate) fn at_short_cap(&self) -> bool {
        let cap = self.max_open_short;
        cap != 0 && self.symbol.iter().filter(|st| st.pos.size < 0.0).count() >= cap
    }

    // --- ATR helper ---

    /// Mean true range over the last `n` bars up to (and including) the current step.
    pub(super) fn atr(&self, si: usize, n: usize) -> f64 {
        let bars = &self.bars[si];
        if bars.is_empty() {
            return 0.0;
        }
        let end = self.step + 1;
        let start = 1.max(end.saturating_sub(n));
        if end < 2 {
            return 0.0;
        }
        let mut trs: Vec<f64> = Vec::new();
        for i in start..end {
            let bar = &bars[i];
            let previous_close = bars[i - 1].close;
            let tr = (bar.high - bar.low)
                .max((bar.high - previous_close).abs())
                .max((bar.low - previous_close).abs());
            trs.push(tr);
        }
        if trs.is_empty() { 0.0 } else { py_sum(trs.iter().copied()) / trs.len() as f64 }
    }

    // --- reads ---

    pub fn position_of(&self, symbol: &str) -> Position {
        self.symbol[self.index(symbol)].pos
    }

    pub fn price_of(&self, symbol: &str) -> f64 {
        self.symbol[self.index(symbol)].price
    }

    /// `symbols[si]`'s LINEAR factor, read off its contract value ([`Self::value_of_index`]), never a
    /// second store: an unlisted symbol's value is `ContractValue::linear` of its multiplier, so this
    /// is that multiplier bit for bit. TEST-ONLY: every money site of the engine reads the value's own
    /// methods (owner, 2026-11-09), so only the verbatim copies of the code they replaced
    /// (`crates/vike-simulator/src/engine/tests/liquidation_fold.rs`) read the bare factor.
    #[cfg(test)]
    #[inline]
    pub(crate) fn multiplier_of_index(&self, si: usize) -> f64 {
        self.value_by_si[si].per_unit
    }

    /// `symbols[si]`'s contract value ([`StrategyEngineConfig::contract_values`]): `ContractValue::linear` of
    /// its multiplier for every symbol the run did not list, so a ledger site that reads it books a
    /// linear symbol bit for bit as it did through the bare multiplier.
    #[inline]
    pub(crate) fn value_of_index(&self, si: usize) -> ContractValue {
        self.value_by_si[si]
    }

    /// The closed-trade statistics ([`StrategyEngineConfig::closed_trades`]), the open lots marked at each
    /// symbol's last price; `None` when the run did not arm them. Called once, at the end of a run.
    pub(crate) fn closed_trade_metrics(
        &self,
    ) -> Option<vike_analytics::closed_trades::TradeClosedMetrics> {
        if !self.closed_trade_report {
            return None;
        }
        let mut m = self.closed_trades.metrics(|symbol| {
            let si = self.symbols.iter().position(|s| s == symbol)?;
            Some((self.symbol[si].price, self.value_of_index(si)))
        });
        // A count of what the engine already did, no behaviour of its own.
        m.account.margin_calls = self.shortfall_cover_fills as usize;
        Some(m)
    }

    /// Step `step`'s bars are done: the closed-trade ledger ([`StrategyEngineConfig::closed_trades`]) meets
    /// each symbol's bar walk, then samples the equity at `ts` with every symbol at its close. A
    /// no-op when the run did not arm it.
    pub(crate) fn mark_closed_trades(&mut self, step: usize, ts: i64) {
        if !self.closed_trade_report {
            return;
        }
        let ledger = &mut self.closed_trades;
        for (si, symbol) in self.symbols.iter().enumerate() {
            let b = &self.bars[si][step];
            let walk = b.path_points();
            ledger.mark_bar(symbol, step, &walk);
        }
        let (symbols, symbol_name, values) = (&self.symbols, &self.symbol, &self.value_by_si);
        ledger.mark_close(ts, |symbol| {
            let si = symbols.iter().position(|s| s == symbol)?;
            Some((symbol_name[si].price, values[si]))
        });
    }

    /// An order of `size` on `symbols[si]` in the settle coin it is worth at `price`
    /// (`ContractValue::in_settle` of its `order_notional`, `|q|·u/p`), when that symbol's series
    /// counts volume in the coin ([`StrategyEngineConfig::coin_volume`]); `None` when its volume counts the
    /// order's own unit, so the volume cap and the impact model compare the size itself.
    #[inline]
    pub(crate) fn coin_volume_of(&self, si: usize, size: f64, price: f64) -> Option<f64> {
        self.coin_volume_by_si[si].then(|| {
            let value = self.value_of_index(si);
            value.in_settle(value.order_notional(size, price), price)
        })
    }

    /// `symbols[si]`'s realized plus unrealized PnL at its last price: the per-symbol curve and
    /// `per_symbol_pnl`, in the ledger's money ([`settle_unrealized_pnl`]).
    pub(crate) fn symbol_pnl(&self, si: usize) -> f64 {
        let st = &self.symbol[si];
        st.realized + settle_unrealized_pnl(self.value_of_index(si), &st.pos, st.price)
    }

    /// The equity an order on `symbols[si]` is sized and gated against, in that symbol's QUOTE currency:
    /// [`Self::equity_now`] on a linear symbol, and on an inverse one the coin ledger's equity valued at
    /// that contract's own price (decision 0258's `r`), the dollars its face and every ceiling are in.
    pub(crate) fn sizing_equity(&self, si: usize) -> f64 {
        self.in_quote(si, self.equity_now())
    }

    /// An `amount` of the ledger's money in `symbols[si]`'s QUOTE currency: itself on a linear symbol,
    /// and on an inverse one a coin amount valued at that contract's own price (decision 0258's `r`).
    pub(crate) fn in_quote(&self, si: usize, amount: f64) -> f64 {
        self.value_of_index(si).in_quote(amount, self.symbol[si].price)
    }

    /// The price that values ONE unit of a coin ledger's money in dollars (decision 0258's `r`: the
    /// contract's own price), or `None` on a run with no inverse symbol, whose money is the quote
    /// currency already. The first inverse symbol's last price, in `symbols` order.
    pub fn settle_price(&self) -> Option<f64> {
        (0..self.symbols.len())
            .find(|&si| self.value_of_index(si).kind == NotionalKind::Inverse)
            .map(|si| self.symbol[si].price)
    }

    /// THE cross-sectional walk order — every fold over the whole book goes through here rather
    /// than over `0..symbols.len()` directly, so there is ONE place that decides whether a
    /// portfolio read is a property of the instrument SET or of the list the caller typed.
    ///
    /// ⚠ Under [`DecideMode::Sequential`] this is the identity permutation, so a fold routed
    /// through it visits the same indices in the same sequence as the raw range it replaced and
    /// the result is bit-for-bit the frozen one. That equality is the whole reason the indirection
    /// is safe to add to a parity-frozen path: nothing about the default answer changes, only
    /// where the order comes from.
    #[inline]
    pub(crate) fn cross_section(&self) -> &[usize] {
        &self.cross_section
    }

    /// cash + Σ pos·price·mult (Python builtin sum → py_sum, cross-section order — the symbols
    /// order under the default [`DecideMode::Sequential`]). Each term is the symbol's
    /// [`ContractValue::cash_leg`], which IS that product on a linear symbol and the coin worth of
    /// the holding on an inverse one, so a coin ledger's equity is in its coin.
    pub fn equity_now(&self) -> f64 {
        self.cash
            + py_sum(self.cross_section().iter().map(|&si| {
                self.value_of_index(si).cash_leg(self.symbol[si].pos.size, self.symbol[si].price)
            }))
    }

    pub fn drawdown_now(&self) -> f64 {
        let eq = self.equity_now();
        let peak = self.equity_peak;
        if peak <= 0.0 {
            return 0.0;
        }
        f64::max(0.0, (peak - eq) / peak)
    }

    /// Resting untagged orders the MATCHING ENGINE holds for `symbol`.
    ///
    /// HAZARD under the opt-in latency gate: an order the strategy has submitted but that is still
    /// in flight is in NEITHER this slice nor the position — it does not exist anywhere the
    /// strategy can see it except [`Self::in_flight_of`]. The common idiom
    /// `if ctx.pending_of(s).is_empty() { ctx.submit_limit(..) }` therefore re-fires on every tick
    /// of the entry-latency window and can rest N duplicate orders; write it as
    /// `if ctx.pending_of(s).is_empty() && ctx.in_flight_of(s) == 0 { .. }` instead. (An order
    /// placed by tag is immune — a place under a tag still working is refused, `<tag> is busy`.)
    /// The slice holds every resting order, the maker law's quotes (`OrderBrokerSimulatorSymbolState::quotes`) too.
    pub fn pending_of(&self, symbol: &str) -> &[OrderResting] {
        &self.symbol[self.index(symbol)].pending
    }

    /// The orders resting on `symbol`'s book that a tag names, each with its tag, in book order: one
    /// placed through `vike_model::OrderBroker`, a tagged quote included. Not one that has filled or
    /// been cancelled, nor one still in flight.
    pub fn tagged_of(&self, symbol: &str) -> Vec<(&str, &OrderResting)> {
        let si = self.index(symbol);
        let named = |o: &OrderResting| {
            self.order_tags.iter().find(|(_, at)| **at == (si, o.qid)).map(|(tag, _)| tag.as_str())
        };
        self.symbol[si].pending.iter().filter_map(|o| named(o).map(|tag| (tag, o))).collect()
    }

    /// The armed protective stop level for `symbol`, or `None` when nothing is armed. Read-only
    /// view of the implicit bracket `submit(.., stop)` arms — the level `check_stop` breaches
    /// against, and the OCO sibling a closing-side limit fill disarms.
    pub fn protective_stop_of(&self, symbol: &str) -> Option<f64> {
        self.symbol[self.index(symbol)].stop
    }

    /// How many NEW-ORDER actions for `symbol` are held in flight by the opt-in latency gate —
    /// submitted by the strategy, not yet visible to the matching engine. Always `0` when the
    /// gate is not armed, which is every path but a `run_ticks` replay with
    /// `StrategyEngineConfig::latency_model = Some(..)`. Pair it with [`Self::pending_of`] (see the
    /// duplicate-submit hazard documented there).
    ///
    /// Total on an unknown symbol (`0`) rather than a panic, like [`Self::is_resolved`]: it is a
    /// read a strategy calls to decide whether to quote.
    pub fn in_flight_of(&self, symbol: &str) -> usize {
        let (Some(g), Some(si)) =
            (self.latency.as_ref(), self.symbols.iter().position(|s| s == symbol))
        else {
            return 0;
        };
        g.in_flight_orders_for(si)
    }

    /// Advance the strategy-visible shadow position by ONE fill the response leg has DELIVERED.
    /// No-op (and no symbol lookup) unless the gate is armed — `shadow_pos` is empty otherwise.
    /// `Fill::symbol` is always `symbols[si]` (see `apply_fill`), so the lookup always resolves;
    /// an unrecognized symbol is skipped rather than panicking inside a delivery loop.
    pub(crate) fn advance_shadow(&mut self, fill: &Fill) {
        if self.shadow_pos.is_empty() {
            return;
        }
        if let Some(si) = self.symbols.iter().position(|s| *s == fill.symbol) {
            self.shadow_pos[si] += fill.side as f64 * fill.size;
        }
    }
}
