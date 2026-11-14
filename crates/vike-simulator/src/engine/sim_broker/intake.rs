//! The strategy-facing order verbs and the higher-timeframe reads.

use vike_marketdata::Bar;
use vike_model::{
    OrderAttributes, OrderDrop, OrderKind, OrderRelease, OrderResting, OrderStopAdjust, OrderType,
    TimeInForce,
};

#[cfg(doc)]
use super::params::StrategyEngineConfig;
use super::state::OrderBrokerSimulator;

impl OrderBrokerSimulator {
    // --- order intake (strategy-facing verbs) ---

    pub fn submit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
        stop: Option<f64>,
    ) {
        let si = self.index(symbol);
        let size = self.size_entry(si, side_sign, size, raw, stop); // sizer first, then the pre-trade gate
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Market, None, None);
        if size > 0.0 {
            let mut o = OrderResting::new(OrderKind::Market, side_sign, size);
            o.weight = weight;
            o.stop = stop;
            self.push_pending(si, o);
        }
    }

    /// A market order closing the whole position. It takes no pre-trade gate, but on a spot (cash)
    /// symbol it meets the spot rule as any sell does: a close of a long whose base other sells
    /// already hold is refused in words (`Self::cash_refusal`).
    pub fn submit_close(&mut self, symbol: &str) {
        let si = self.index(symbol);
        let pos = self.symbol[si].pos;
        if pos.size != 0.0 {
            let side = vike_model::closing_side(pos.size);
            if let Some(why) = self.cash_refusal(si, side, pos.size.abs(), None) {
                self.dropped.push(OrderDrop {
                    symbol: self.symbols[si].clone(),
                    reason: why,
                    size: pos.size.abs(),
                    weight: 0.0,
                });
                return;
            }
            let o = OrderResting::new(OrderKind::Market, side, pos.size.abs());
            self.push_pending(si, o);
        }
    }

    pub fn submit_limit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        raw: bool,
        stop: Option<f64>,
    ) {
        let si = self.index(symbol);
        let size = self.size_entry(si, side_sign, size, raw, stop);
        let size =
            self.gate_order(si, side_sign, size, weight, OrderType::Limit, Some(price), None);
        if size <= 0.0 {
            return; // denied by the pre-trade gate — same shape as the market verbs
        }
        let mut o = OrderResting::new(OrderKind::Limit, side_sign, size);
        o.price = Some(price);
        o.weight = weight;
        o.stop = stop;
        self.push_pending(si, o);
    }

    /// A MAKER-ONLY limit order ([`vike_model::TimeInForce::PostOnly`]): it rests, or it is refused
    /// — a limit that would TAKE liquidity against the symbol's last price is dropped (nothing
    /// rests, nothing fills; the run's `dropped` list says `post_only_would_cross`), exactly as the
    /// paper book and every venue refuse it (`vike_fills::fill_model::post_only_would_cross`, the
    /// one law). Returns `true` when the order was handed on (it may still be denied by the sizer
    /// / pre-trade gate, as any limit is).
    /// With no price yet (the symbol has not printed) there is nothing to judge against and the
    /// order rests. The engine's other limit verb is [`Self::submit_limit`], unchanged.
    pub fn submit_limit_post_only(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        raw: bool,
        stop: Option<f64>,
    ) -> bool {
        let si = self.index(symbol);
        let last = self.symbol[si].price;
        if last.is_finite()
            && last > 0.0
            && vike_fills::fill_model::post_only_would_cross(side_sign, price, last)
        {
            // recorded like every other refusal in the run's `dropped` list (symbol, reason, size, price)
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: "post_only_would_cross".into(),
                size,
                weight: price,
            });
            return false;
        }
        self.submit_limit(symbol, side_sign, size, price, weight, raw, stop);
        true
    }

    /// A LIMIT with its ATTRIBUTES ([`OrderAttributes`]: all-or-none, sweep-to-fill, a discretionary
    /// amount), one verb for the three. Refused in the words of the doors' one rule
    /// (`vike_model::attributes_refusal`, on a `gtc` limit) and, under [`StrategyEngineConfig::queue_model`],
    /// an all-or-none or a discretionary limit in words of its own: no queue reading states its law.
    /// Then the sizer and the pre-trade gate, which prices a discretionary limit at its WORST price, L
    /// + d for a buy and L − d for a sell, as live does. It rests as the `Limit` at `price`:
    ///
    /// - SWEEP-TO-FILL is the limit itself on every lane, the queue model included: one book already
    ///   fills a marketable limit within its price, so a router's sweep of many books adds nothing;
    /// - ALL-OR-NONE fills only on an event that can fill all of it: a volume cap
    ///   ([`StrategyEngineConfig::volume_limit`]) that would cut it holds it WHOLE, no fill and nothing in
    ///   [`Self::dropped`]; the SL/TP cap never cuts it either (it rests on whole, found again by the
    ///   order id this verb stamps, `OrderResting::qid`); otherwise it is the limit (an L1 quote has no
    ///   depth, and the L2 lane fills a limit whole or rests it);
    /// - a DISCRETIONARY limit is judged at L ± d and books as a TAKER on every lane
    ///   (`vike_model::order_fill_price`'s discretionary law, the tick and L2 tiers too).
    pub fn submit_limit_attributed(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        attributes: OrderAttributes,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let tif = TimeInForce::Gtc;
        let why = vike_model::attributes_refusal(&OrderType::Limit, tif, Some(&attributes))
            .or_else(|| {
                (self.queue_model && attributes.all_or_none).then(|| {
                    "an all-or-none limit has no queue law: no reading of one FIFO queue proves an \
                     execution that fills it whole"
                        .to_string()
                })
            })
            .or_else(|| {
                (self.queue_model && attributes.discretionary_amount.is_some()).then(|| {
                    "a discretionary limit has no queue law: its displayed queue is at its limit \
                     while its executions take liquidity inside its discretion"
                        .to_string()
                })
            });
        if let Some(why) = why {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        let mut o = OrderResting::new(OrderKind::Limit, side_sign, 0.0);
        o.price = Some(price);
        o.weight = weight;
        o.all_or_none = attributes.all_or_none;
        o.discretionary_amount = attributes.discretionary_amount;
        let size = self.size_entry(si, side_sign, size, raw, None);
        let worst = Some(o.discretion_limit(price));
        o.size = self.gate_order(si, side_sign, size, weight, OrderType::Limit, worst, None);
        if o.size > 0.0 {
            // its own id, by which the SL/TP cap finds it again when it holds it whole
            if o.all_or_none {
                o.qid = self.next_queue_id();
            }
            self.push_pending(si, o);
        }
    }

    pub fn submit_stop(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        // A stop's reference is its TRIGGER, which is what `check_inner`'s
        // `price.or(trigger_price)` falls to when there is no limit price.
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Stop, None, Some(price));
        if size <= 0.0 {
            return;
        }
        let mut o = OrderResting::new(OrderKind::Stop, side_sign, size);
        o.price = Some(price);
        o.weight = weight;
        self.push_pending(si, o);
    }

    /// A REDUCE-ONLY stop (`LiveBroker::submit_stop_reduce`'s twin): rests as a stop at `price` and
    /// may only shrink the position it meets. A flat book or a position on the order's own side is
    /// refused at once, in words, in [`OrderBrokerSimulator::dropped`]; the size is capped to the position now
    /// and again at the fill (`reduce_only_size`), so a stop whose position was closed another way
    /// drops instead of opening a new one. The size is the strategy's own (never sized by the sizer:
    /// a reduce is not an entry).
    pub fn submit_stop_reduce(&mut self, symbol: &str, side_sign: i32, size: f64, price: f64) {
        let si = self.index(symbol);
        // judged at its fill alone under `InBarRules::reduce_only_at_fill`
        let size = if self.in_bar.rules.reduce_only_at_fill {
            size
        } else {
            self.reduce_only_size(si, side_sign, size)
        };
        if size <= 0.0 {
            return;
        }
        let size = self.gate_order(si, side_sign, size, 0.0, OrderType::Stop, None, Some(price));
        if size <= 0.0 {
            return;
        }
        let mut o = OrderResting::new(OrderKind::Stop, side_sign, size);
        o.price = Some(price);
        o.reduce_only = true;
        self.push_pending(si, o);
    }

    /// A STOP-LIMIT order (`LiveBroker::submit_stop_limit`'s twin): a stop at `trigger` whose fill
    /// is a limit at `limit`. It triggers as a stop does; a limit the trigger event does not reach
    /// turns it, in place, into the plain resting `Limit` at `limit`, filled and booked as a maker
    /// from then on (`vike_model::order_fill_price`'s law, on every fill lane). The trigger goes on
    /// the grid as it joins the book (`push_pending`), as a limit's price by its side; the limit is
    /// the caller's.
    pub fn submit_stop_limit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trigger: f64,
        limit: f64,
        weight: f64,
        raw: bool,
    ) {
        let kind = OrderKind::StopLimit;
        self.submit_trigger_kind(symbol, kind, side_sign, size, trigger, Some(limit), weight, raw);
    }

    /// An IF-TOUCHED order (`LiveBroker::submit_take_profit`'s twin): touched at `trigger` from the
    /// favourable side (a buy at or below it, a sell at or above). With no `limit` it fills at the
    /// touch as a taker (market-if-touched); with one it takes the stop-limit's limit part
    /// (limit-if-touched), resting as the `Limit` at `limit` when the touch does not reach it.
    pub fn submit_take_profit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trigger: f64,
        limit: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let kind = OrderKind::TakeProfit;
        self.submit_trigger_kind(symbol, kind, side_sign, size, trigger, limit, weight, raw);
    }

    /// The two verbs above. Terms the ONE order-terms rule (`vike_model::order_terms_refusal`, the
    /// live strategy drain's) refuses are dropped in its words, so a backtest never rests what live
    /// refuses; then the sizer and the pre-trade gate, judging the request as live sends it (the
    /// limit is its price, the trigger its trigger price); then the order rests.
    fn submit_trigger_kind(
        &mut self,
        symbol: &str,
        kind: OrderKind,
        side_sign: i32,
        size: f64,
        trigger: f64,
        limit: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let order_type =
            if kind == OrderKind::StopLimit { OrderType::StopLimit } else { OrderType::TakeProfit };
        if let Some(why) = vike_model::order_terms_refusal(
            &order_type,
            limit,
            Some(trigger),
            None,
            None,
            None,
            None,
        ) {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, order_type, limit, Some(trigger));
        if size <= 0.0 {
            return;
        }
        let mut o = OrderResting::new(kind, side_sign, size);
        o.price = Some(trigger);
        o.limit = limit;
        o.weight = weight;
        self.push_pending(si, o);
    }

    /// An ADJUSTABLE stop (IB's adjustable stop; the auto trailing stop is its
    /// `vike_model::OrderStopAdjustTo::Trailing` target): a stop at `stop`, or a stop-limit with `limit`, that
    /// becomes `adjust.to` IN PLACE, once, on the first event that reaches `adjust.at` without firing
    /// it, judged by its new terms from the NEXT event (`vike_model::order_fill_price`'s adjustment law,
    /// which every fill lane takes). Refused in the words of the doors' rules
    /// (`vike_model::order_terms_refusal` for the stop, `vike_model::stop_adjust_refusal` for the
    /// adjustment); the pre-trade gate then judges the stop live sends (the limit its price, the stop
    /// its trigger), never its target.
    pub fn submit_stop_adjustable(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        stop: f64,
        limit: Option<f64>,
        adjust: OrderStopAdjust,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let (kind, order_type) = match limit {
            Some(_) => (OrderKind::StopLimit, OrderType::StopLimit),
            None => (OrderKind::Stop, OrderType::Stop),
        };
        let why =
            vike_model::order_terms_refusal(&order_type, limit, Some(stop), None, None, None, None)
                .or_else(|| {
                    vike_model::stop_adjust_refusal(
                        &order_type,
                        side_sign,
                        Some(stop),
                        Some(&adjust),
                    )
                });
        if let Some(why) = why {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, order_type, limit, Some(stop));
        if size <= 0.0 {
            return;
        }
        let mut o = OrderResting::new(kind, side_sign, size);
        o.price = Some(stop);
        o.limit = limit;
        o.weight = weight;
        o.adjust = Some(Box::new(adjust));
        self.push_pending(si, o);
    }

    pub fn submit_trailing(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trail: f64,
        weight: f64,
        raw: bool,
    ) {
        let kind = OrderKind::Trailing;
        self.rest_trailing(
            symbol,
            kind,
            side_sign,
            size,
            Some(trail),
            None,
            None,
            None,
            weight,
            raw,
        );
    }

    /// A trailing stop that WAITS: nothing can fire until the price reaches `activation` (a sell
    /// protects a long, so the price must come up to it; a buy protects a short, so it must come
    /// down to it), and from there it trails by `trail`, at `extreme - trail` for a sell and
    /// `extreme + trail` for a buy. Activation and the stop can fall in one bar.
    ///
    /// A mark already past the level at this call activates it at once, trailing from that mark. The
    /// level rides on [`OrderResting::activation`] (the plain trailing law reads none), and
    /// [`Self::fill_price_for`] is what reads it, on every fill lane, for as long as the order lives.
    pub fn submit_trailing_activated(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trail: f64,
        activation: f64,
        weight: f64,
        raw: bool,
    ) {
        let (kind, trail, activation) = (OrderKind::Trailing, Some(trail), Some(activation));
        self.rest_trailing(
            symbol, kind, side_sign, size, trail, None, activation, None, weight, raw,
        );
    }

    /// A TRAILING-LIMIT order ([`OrderType::TrailingLimit`]): a trailing stop by `trail` whose fill
    /// is a limit. It fires by the trailing law at the stop level P; its limit L is then P less
    /// `limit_offset` for a sell, P plus it for a buy, and it takes the stop-limit's limit part with
    /// the fire price as A: the better of L and A when the event reaches L, else it rests, in place,
    /// as the plain `Limit` at L, a maker from then on (`vike_model::order_fill_price`'s
    /// trailing-limit law). With an `activation` it waits for that level first and is walked along
    /// the bar's path, as [`Self::submit_trailing_activated`] is; with none it trails from the mark
    /// at once, by the tier's own law.
    ///
    /// Terms the ONE order-terms rule (`vike_model::order_terms_refusal`, with the trail and the
    /// offset) refuses are dropped in its words, so a backtest never rests what live refuses; the
    /// pre-trade gate then judges the request live sends (no price, the activation its trigger).
    pub fn submit_trailing_limit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trail: f64,
        limit_offset: f64,
        activation: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let offset = Some(limit_offset);
        let order_type = OrderType::TrailingLimit;
        let why = vike_model::order_terms_refusal(
            &order_type,
            None,
            activation,
            Some(trail),
            offset,
            None,
            None,
        );
        if let Some(why) = why {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        let (kind, trail) = (OrderKind::TrailingLimit, Some(trail));
        self.rest_trailing(
            symbol, kind, side_sign, size, trail, None, activation, offset, weight, raw,
        );
    }

    /// A TRAILING TAKE-PROFIT ([`OrderType::TrailingTakeProfit`], IB `TRAIL MIT`, and `TRAIL LIT`
    /// with a `limit_offset`): the if-touched twin of [`Self::submit_trailing`]. A sell tracks the
    /// LOWEST price since it rests and fires when the price rises `trail` above it, a buy tracks the
    /// highest and fires on a fall `trail` below it; with no offset it fills at the touch (or the open
    /// a gap carried past it, an improvement) as a taker, with one it takes the stop-limit's limit
    /// part at the trigger less the offset for a sell (plus it for a buy), resting as the `Limit` there
    /// when the event does not reach it (`vike_model::order_fill_price`'s trailing take-profit law). It
    /// trails from the mark at once: it takes no activation in this build.
    ///
    /// Terms the ONE order-terms rule refuses are dropped in its words, and so is an order on a
    /// symbol that has not printed (nothing to trail from, as live refuses one without a mark); the
    /// pre-trade gate then judges the request live sends (no price, no trigger).
    pub fn submit_trailing_take_profit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        trail: f64,
        limit_offset: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let (order_type, trail) = (OrderType::TrailingTakeProfit, Some(trail));
        self.submit_trailing_terms(
            symbol,
            order_type,
            side_sign,
            size,
            trail,
            None,
            None,
            limit_offset,
            weight,
            raw,
        );
    }

    /// A trailing order whose distance is a PERCENT of its extreme (`vike_model::OrderRequest::
    /// trail_percent`, above 0 and below 100): `order_type` is one of the three trailing kinds
    /// (`trailing`, `trailing_limit` with its `limit_offset`, `trailing_take_profit` with an optional
    /// one), each by its own law with the distance extreme × `percent` / 100 at every step
    /// (`vike_model::OrderResting::trail_distance`, the one place a percent becomes a price), the
    /// activation walk included. Refused as [`Self::submit_trailing_take_profit`] refuses (any other
    /// kind in the order-terms rule's words; a trailing take-profit takes no `activation`).
    pub fn submit_trailing_percent(
        &mut self,
        symbol: &str,
        order_type: OrderType,
        side_sign: i32,
        size: f64,
        percent: f64,
        activation: Option<f64>,
        limit_offset: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let percent = Some(percent);
        self.submit_trailing_terms(
            symbol,
            order_type,
            side_sign,
            size,
            None,
            percent,
            activation,
            limit_offset,
            weight,
            raw,
        );
    }

    /// The two verbs above: the order-terms rule, a mark to trail from when nothing waits for an
    /// activation, then [`Self::rest_trailing`].
    fn submit_trailing_terms(
        &mut self,
        symbol: &str,
        order_type: OrderType,
        side_sign: i32,
        size: f64,
        trail: Option<f64>,
        percent: Option<f64>,
        activation: Option<f64>,
        offset: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let priced = self.symbol[si].price > 0.0;
        let unpriced = activation.is_none() && !priced;
        let why = vike_model::order_terms_refusal(
            &order_type,
            None,
            activation,
            trail,
            offset,
            None,
            percent,
        )
        .or_else(|| {
            unpriced.then(|| {
                format!(
                    "a {order_type} order trails from {symbol}'s last price, and it has \
                             none yet"
                )
            })
        });
        if let Some(why) = why {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        let kind = match order_type {
            OrderType::TrailingLimit => OrderKind::TrailingLimit,
            OrderType::TrailingTakeProfit => OrderKind::TrailingTakeProfit,
            _ => OrderKind::Trailing,
        };
        self.rest_trailing(
            symbol, kind, side_sign, size, trail, percent, activation, offset, weight, raw,
        );
    }

    /// Every trailing verb: a `Trailing` stop, a `TrailingLimit` or a `TrailingTakeProfit`, trailing
    /// by `trail` or by `trail_percent` (exactly one, the caller's rule having judged it).
    fn rest_trailing(
        &mut self,
        symbol: &str,
        kind: OrderKind,
        side_sign: i32,
        size: f64,
        trail: Option<f64>,
        trail_percent: Option<f64>,
        activation: Option<f64>,
        limit_offset: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        let (order_type, trigger) = match kind {
            // as live sends it: no price, the activation its trigger price
            OrderKind::TrailingLimit => (OrderType::TrailingLimit, activation),
            // as live sends it: no price, no trigger (it takes no activation)
            OrderKind::TrailingTakeProfit => (OrderType::TrailingTakeProfit, None),
            // A trailing stop carries no price of its own — it references the mark, which is
            // exactly what `check_inner`'s `unwrap_or(ctx.mark_price)` fallback supplies. It is
            // judged as a `Stop`, as it was before the gate knew the word "trailing": the gate reads
            // the kind ONLY in `take_scope`, where both are passive without a price.
            _ => (OrderType::Stop, None),
        };
        let size = self.gate_order(si, side_sign, size, weight, order_type, None, trigger);
        if size <= 0.0 {
            return;
        }
        let mut o = OrderResting::new(kind, side_sign, size);
        o.limit_offset = limit_offset;
        o.trail = trail;
        o.trail_percent = trail_percent;
        self.seed_trailing(si, &mut o, activation);
        o.weight = weight;
        self.push_pending(si, o);
    }

    /// A trailing order's extreme and activation: it trails from `symbols[si]`'s mark now, or with an
    /// `activation` level it waits for the price to reach it (a mark already past it activates it at
    /// once, trailing from that mark). Every trailing verb's, the tagged one's
    /// (`vike_model::OrderBroker::place`) too.
    pub(super) fn seed_trailing(&self, si: usize, o: &mut OrderResting, activation: Option<f64>) {
        // `InBarRules::trail_from_fill`: placed while a fill is delivered, it waits for the walk
        if self.in_bar.rules.trail_from_fill
            && self.in_bar.delivering.is_some()
            && activation.is_some()
        {
            o.activation = activation;
            o.extreme = None;
            return;
        }
        let extreme_snap = self.symbol[si].price;
        o.extreme = Some(extreme_snap);
        if let Some(level) = activation {
            o.activation = activation;
            // No extreme yet means not activated. An unprinted symbol's mark judges nothing.
            o.extreme = (extreme_snap > 0.0
                && vike_model::trailing_activation_reached(o.side, extreme_snap, level))
            .then_some(extreme_snap);
        }
    }

    pub fn submit_market_close(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Market, None, None);
        if size > 0.0 {
            let mut o = OrderResting::new(OrderKind::MarketClose, side_sign, size);
            o.weight = weight;
            self.push_pending(si, o);
        }
    }

    /// A MARKET-TO-LIMIT order ([`OrderType::MarketToLimit`]): it fills as a market order does on
    /// its first event, and what the lane's volume cap leaves of it rests, in place, as the plain
    /// `Limit` at the price that first fill booked at, a maker from then on (nothing filled: it
    /// waits as it was). With no cap a bar fills it whole, so there it IS a market order. On the
    /// L2 lane it takes only the best opposite level, at that level's price, and a level that
    /// cannot cover it rests it whole as the `Limit` there (`vike_fills::fill_model`'s
    /// `FillModelL2Book`).
    pub fn submit_market_to_limit(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        let order_type = OrderType::MarketToLimit;
        let size = self.gate_order(si, side_sign, size, weight, order_type, None, None);
        if size > 0.0 {
            let mut o = OrderResting::new(OrderKind::MarketToLimit, side_sign, size);
            o.weight = weight;
            self.push_pending(si, o);
        }
    }

    /// A MARKET- or LIMIT-ON-OPEN order ([`TimeInForce::AtOpen`]): it takes part ONCE, in the
    /// opening auction of the symbol's NEXT session by its session calendar (the one
    /// [`StrategyEngineConfig::session_gate`] resolves), at the open of that session's first event. A
    /// market one fills there; a limit one when that open is no worse than `limit`, and otherwise
    /// the auction is over and the order ENDS (`auction_passed` in [`Self::dropped`]), never
    /// resting. The auction instant rides [`OrderResting::release`], so the order is held until
    /// it. A symbol with no calendar, or one that never closes (24/7), has no auction: refused at
    /// once, in words, in [`Self::dropped`].
    pub fn submit_at_open(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        limit: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let kind = OrderKind::AuctionOpen;
        self.submit_auction(symbol, kind, side_sign, size, limit, weight, raw);
    }

    /// The closing twin of [`Self::submit_at_open`] ([`TimeInForce::AtClose`]): ONCE, at the close
    /// of the LAST bar of the first session that closes while the order is live
    /// (`Self::closes_session`; on sub-bars, the last sub-bar's close). Not
    /// [`Self::submit_market_close`], which is the close of whichever bar comes next. A tick replay
    /// cannot name a session's last tick in time, so it ends the order (`auction_close_needs_bars`).
    pub fn submit_at_close(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        limit: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let kind = OrderKind::AuctionClose;
        self.submit_auction(symbol, kind, side_sign, size, limit, weight, raw);
    }

    /// A MARKET-TO-LIMIT at the opening auction ([`OrderType::MarketToLimit`] at
    /// [`TimeInForce::AtOpen`], IB `MTL` + `AUC`): exactly the market [`Self::submit_at_open`] (held
    /// until the open of the symbol's next session, filled there as a market order, a taker), except
    /// what the lane's volume cap leaves of it, which rests IN PLACE as the plain `Limit` at the price
    /// the auction booked, a maker from then on; an auction that fills none of it rests it whole as the
    /// `Limit` at that open. Refused as the auction verbs refuse (no auction on the symbol, a price
    /// condition), judged by `vike_model::lifetime_refusal` as live judges `at-open` on this kind.
    pub fn submit_market_to_limit_at_open(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
    ) {
        let kind = OrderKind::AuctionOpenToLimit;
        self.submit_auction(symbol, kind, side_sign, size, None, weight, raw);
    }

    /// The auction verbs: the order-terms and lifetime rules, the auction the calendar names, then
    /// the sizer and the pre-trade gate, judging the request live sends (a market order, a limit at
    /// `limit`, or a market-to-limit). A refusal is dropped in words.
    fn submit_auction(
        &mut self,
        symbol: &str,
        kind: OrderKind,
        side_sign: i32,
        size: f64,
        limit: Option<f64>,
        weight: f64,
        raw: bool,
    ) {
        let si = self.index(symbol);
        let order_type = match (kind, limit) {
            (OrderKind::AuctionOpenToLimit, _) => OrderType::MarketToLimit,
            (_, Some(_)) => OrderType::Limit,
            (_, None) => OrderType::Market,
        };
        let tif = if kind == OrderKind::AuctionClose {
            TimeInForce::AtClose
        } else {
            TimeInForce::AtOpen
        };
        // live from now, or from the later release it was sent inside (`submit_released`)
        let from = self.releasing.map_or(self.now, |r| r.at.max(self.now));
        // the first open after the session `from` is in closes: `None` when it never closes
        let next_open = self.session.get(si).copied().flatten().and_then(|cal| {
            let close = cal.next_close(from)?;
            cal.next_open(close)
        });
        let time_in_force_word = tif.as_word();
        let why = vike_model::order_terms_refusal(&order_type, limit, None, None, None, None, None)
            .or_else(|| vike_model::lifetime_refusal(&order_type, tif, None, None, self.now))
            .or_else(|| {
                next_open.is_none().then(|| {
                    format!(
                        "time in force `{time_in_force_word}`: {symbol} has no auction (no session calendar, \
                         or one that never closes)"
                    )
                })
            })
            .or_else(|| {
                // a price condition met after the auction would hold the order onto a bar that
                // is not its auction
                self.releasing.is_some_and(|r| r.price.is_some()).then(|| {
                    format!(
                        "time in force `{time_in_force_word}` takes a time release, not a price condition"
                    )
                })
            });
        if let Some(why) = why {
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, order_type, limit, None);
        if size <= 0.0 {
            return;
        }
        let mut o = OrderResting::new(kind, side_sign, size);
        o.price = limit;
        o.weight = weight;
        if kind != OrderKind::AuctionClose {
            o.release = next_open.map(|at| OrderRelease { at, price: None });
        }
        self.push_pending(si, o);
    }

    /// An ICEBERG limit (`vike_model::OrderRequest::display_quantity`): `display` of `size` shows and
    /// the rest is hidden. No bar or tick shows a queue, and a trade through the price takes the
    /// hidden part too, so it rests and fills exactly as the full-size [`Self::submit_limit`]
    /// does. Under [`StrategyEngineConfig::queue_model`] it is REFUSED, in words, in [`Self::dropped`]: a
    /// hidden part's place in the queue has no law here, which only an L3 feed could state
    /// (decision 0045). A `display` below zero is refused too.
    ///
    /// A `display` of ZERO is a HIDDEN order (the venue shows none of it), by the same law: the
    /// full-size limit on bars, sub-bars, ticks and L2, refused under the queue model in its own
    /// words. Assumption: a hidden order's lost priority behind the size displayed at its own price is
    /// not modelled (optimistic).
    pub fn submit_limit_iceberg(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        price: f64,
        display: f64,
        weight: f64,
        raw: bool,
        stop: Option<f64>,
    ) {
        if let Some(why) = self.iceberg_refusal(size, display) {
            let si = self.index(symbol);
            self.dropped.push(OrderDrop {
                symbol: self.symbols[si].clone(),
                reason: why,
                size,
                weight,
            });
            return;
        }
        self.submit_limit(symbol, side_sign, size, price, weight, raw, stop);
    }

    /// Why a limit of `size` showing `display` is refused here, if it is: under the queue model
    /// always (decision 0045), else by the doors' one rule (zero or more, below the order's size).
    /// [`Self::submit_limit_iceberg`]'s and the order seam's (`OrderPlacement::Iceberg`) one judgement.
    pub(crate) fn iceberg_refusal(&self, size: f64, display: f64) -> Option<String> {
        if self.queue_model {
            Some(if display == 0.0 {
                "a hidden order has no queue law: where it stands needs L3 (decision 0045)"
                    .to_string()
            } else {
                "an iceberg has no queue law: its hidden part needs L3 (decision 0045)".to_string()
            })
        } else {
            vike_model::display_refusal(&vike_model::OrderType::Limit, size, Some(display))
        }
    }

    /// Every order `submit` places is HELD until `release` holds ([`OrderRelease`], FIX EffectiveTime),
    /// as the live core holds one (`vike-core`'s `held_release.rs`): kept OFF the book, in its own
    /// table (`OrderBrokerSimulatorSymbolState::held`), so it commits no margin and takes part in no event before, on
    /// any fill lane. From the first event at or after `release.at` it is released by its own kind's
    /// law. A bar's ts is its open and a bar has no time inside it, so an instant inside a bar
    /// releases the order from the NEXT bar. A price condition (a CONDITIONAL order,
    /// `vike_model::OrderReleasePrice`) is read on the condition symbol's last seen price, the mark
    /// before each event, so the order takes part from the first event after both hold. An
    /// opening-auction order waits for the first open after its release, and an auction order
    /// takes no price condition.
    ///
    /// Judged as the core judges it: at the submit (the core's accept) the order-terms rule, the
    /// sizer, a release not in the future (`vike_model::lifetime_refusal`'s words), a condition on
    /// a symbol outside the run and one read on another venue (`vike_model::OrderReleasePrice::venue`:
    /// the simulator names an instrument by its symbol alone, where the live core holds one) are
    /// refused, in [`Self::dropped`]; the pre-trade gate judges it AT
    /// its release, against the account as it is then (`Self::release_held`). A tagged quote takes
    /// none and is refused inside it.
    pub fn submit_released(&mut self, release: OrderRelease, submit: impl FnOnce(&mut Self)) {
        let outer = self.releasing.replace(release);
        submit(self);
        self.releasing = outer;
    }

    /// A MARKET order held until `release` holds: `LiveBroker::submit_market_at`'s twin, the
    /// portable market verb (raw, no weight) inside [`Self::submit_released`].
    pub fn submit_market_at(&mut self, symbol: &str, side: i32, qty: f64, release: OrderRelease) {
        self.submit_released(release, |b| b.submit(symbol, side, qty, 0.0, true, None));
    }

    /// Pull every resting order for `symbol` but the maker law's quotes (`OrderBrokerSimulatorSymbolState::quotes`), which
    /// a cancel by tag pulls. Under the opt-in latency gate the cancel
    /// is HELD in flight — which is the whole point: a cancel issued on the tick that would
    /// have filled the order arrives at the matching engine too late to save it. Held orders
    /// ([`Self::submit_released`]) are dropped at once: no venue holds them, as in the live core, and
    /// so are an order list's held OTO children ([`Self::submit_group`]) and every combo with a leg
    /// on `symbol` ([`Self::submit_combo`], which the latency gate never holds).
    pub fn cancel_all(&mut self, symbol: &str) {
        let si = self.index(symbol);
        self.symbol[si].held.clear();
        self.drop_parked_legs(si);
        self.drop_combos_on(si);
        if self.latency.is_some() {
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &crate::latency::LatencyOrder::NONE,
                crate::latency::LatencyModelInFlightAction::CancelAll { si },
            );
            if !sent {
                // a rejected CANCEL is the dangerous one: the orders stay resting
                self.record_latency_reject(si, 0.0, 0.0);
            }
            return;
        }
        self.pull_book_orders(si);
    }

    /// The STRATEGY-level target verb (`Strategy._engine_target`): sizes through the ONE shared
    /// sizing law `vike_model::ContractValue::units_worth` (`pct·eq/(price·multiplier)` on a
    /// LINEAR value, `r` passed as 1.0: a converted symbol's value already carries its rate,
    /// `StrategyEngineConfig::conversions`) — the
    /// SAME law the live `order_target_percent` verb uses (`vike-core::runtime::broker`), so a
    /// mult≠1 instrument sizes identically live and in backtest. 1e-12 dead-band, submitted raw.
    /// At mult==1 `price·1.0 == price` exactly, so this is bit-identical to the former
    /// `pct·eq/price`. With no usable price (`price <= 0.0`) it HOLDS, live's own guard: read as a
    /// target of 0 units it would close the position on a zero or negative print.
    ///
    /// The value is the symbol's own (`value_of_index`, the linear multiplier unless the run listed
    /// another) and the equity `sizing_equity`'s: on a COIN LEDGER the coin valued at the contract's
    /// price, so an inverse symbol sizes `pct · equity` dollars at its face, as live does.
    pub fn strategy_order_target_percent(&mut self, symbol: &str, pct: f64) {
        let price = self.price_of(symbol);
        if price <= 0.0 {
            return;
        }
        let si = self.index(symbol);
        let value = self.value_of_index(si);
        // r = 1.0 (the value carries any rate) and the law always answers; `0.0` is its own no-size answer
        let target = value.units_worth(pct * self.sizing_equity(si), price, 1.0).unwrap_or(0.0);
        let pos = self.position_of(symbol).size;
        let delta = target - pos;
        if delta.abs() > 1e-12 {
            self.submit(symbol, if delta > 0.0 { 1 } else { -1 }, delta.abs(), 0.0, true, None);
        }
    }

    // --- higher-TF reads (mirror the single engine, per symbol) ---

    /// The completed coarse bars of `tf` for `symbol`, look-ahead safe: only windows that have
    /// fully elapsed at `self.now` are visible.
    ///
    /// `None` means the timeframe is not registered — declare it in `[engine] timeframes`.
    /// ⚠ An unknown SYMBOL still panics, in `index`; that is a separate defect.
    ///
    /// ⚠ **This `None` is a SUBSTITUTE for an unmet spec clause, not the clause itself.**
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §9.1(2) required the
    /// undeclared-timeframe failure to become a LOAD-TIME refusal — "checked against the
    /// strategy's declared timeframe requirement" — before the `timeframes` profile key shipped.
    /// That check was not built (strategies declare no timeframe requirement, and giving them one
    /// is a different plan), and the loud `panic!("timeframe {tf:?} not registered")` this method
    /// used to carry became this quiet `None`. So a strategy asking for a timeframe the profile
    /// did not declare now runs on NO higher-TF data instead of aborting. The trade is accepted
    /// because it is latent — this method has zero callers anywhere in this tree — and it closes
    /// when a strategy-declared timeframe requirement exists.
    pub fn bars_for(&self, symbol: &str, tf: &str) -> Option<&[Bar]> {
        let si = self.index(symbol);
        let (_, ms, coarse) = self.symbol[si].tf.iter().find(|(name, _, _)| name == tf)?;
        let window_start = self.now - self.now.rem_euclid(*ms);
        let index = coarse.partition_point(|b| b.timestamp < window_start);
        Some(&coarse[..index])
    }
}
