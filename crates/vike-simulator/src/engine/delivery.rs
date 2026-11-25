//! Fill and order delivery: the latency-gated `on_fill` path, due-fill and due-order release, the volume clamp.

use vike_model::{Fill, OrderDrop, OrderEvent, OrderKind, OrderResting, Strategy};

use super::{OrderBrokerSimulator, StrategyEngine};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// Deliver a fill to the strategy.
    ///
    /// Immediate unless the opt-in latency gate is ARMED, in which case the fill is held until
    /// `exchange_timestamp + response()` — the response leg. `exchange_timestamp` is `core.now`, the timestamp of the
    /// tick whose matching produced the fill. Held fills are released by
    /// [`Self::deliver_due_fills`] at the top of each subsequent tick, and flushed once at the
    /// end of `run_ticks` so nothing is silently swallowed.
    pub(super) fn fire_on_fill(&mut self, fill: Fill) {
        // a portfolio run's fill into the book of the mount that placed it (no-op on a plain run)
        self.core.attribute_fill(&fill);
        self.deliver_fill(fill);
    }

    /// [`Self::fire_on_fill`] for a fill the lane already put into its mount's book
    /// (`OrderBrokerSimulator::attribute_fill`): the three lanes that arm an OTO child on the fill
    /// (`OrderBrokerSimulator::resolve_group_fill`) attribute it first, so the child's leverage cap
    /// reads a mount book that holds the parent fill.
    pub(super) fn deliver_fill(&mut self, fill: Fill) {
        // the venue knows the order filled now; a held fill's end waits for it (`deliver_order_ends`)
        if let Some(tag) = &fill.tag {
            self.core.note_seam_fill(tag);
        }
        if let Some(g) = self.core.latency.as_ref() {
            let desc = crate::latency::LatencyOrder::new(fill.side, fill.size, Some(fill.price));
            let visible_ns = self
                .core
                .now
                .saturating_mul(1_000_000)
                .saturating_add(g.response_ns(self.core.now, &desc));
            self.deferred_fills.push((visible_ns, fill));
            return;
        }
        // what `InBarRules::queue_order` and `InBarRules::trail_from_fill` read while it is delivered
        self.core.in_bar.delivering = Some(fill.price);
        self.strategy.on_fill(&mut self.core, &fill);
        self.core.in_bar.delivering = None;
    }

    /// Release every response-delayed fill whose visibility stamp has arrived at `timestamp_ms`, in
    /// occurrence order. No-op (and no allocation) when nothing is deferred.
    pub(super) fn deliver_due_fills(&mut self, ts_ms: i64) {
        if self.deferred_fills.is_empty() {
            return;
        }
        let now_ns = ts_ms.saturating_mul(1_000_000);
        let mut due: Vec<Fill> = Vec::new();
        let mut still: Vec<(i64, Fill)> = Vec::new();
        for (vis, f) in self.deferred_fills.drain(..) {
            if vis <= now_ns {
                due.push(f);
            } else {
                still.push((vis, f));
            }
        }
        self.deferred_fills = still;
        for f in due {
            // The shadow moves with the DELIVERY, not with the match — that is what makes a
            // position-polling strategy (`SpreadMaker`) actually feel the response leg. Advanced
            // BEFORE the callback so `on_fill` reads an inventory that already includes this fill.
            self.core.advance_shadow(&f);
            // The callback may itself submit — which re-enters the (still armed) gate, so the
            // reaction is delayed by a fresh entry latency, exactly like the live path.
            self.strategy.on_fill(&mut self.core, &f);
        }
    }

    /// Deliver EVERY still-held fill regardless of its visibility stamp — the end-of-run flush.
    /// A backtest must not end with fills the strategy was never told about (its `on_stop`
    /// accounting would disagree with `TradingRecord`).
    pub(super) fn flush_deferred_fills(&mut self) {
        for (_, f) in std::mem::take(&mut self.deferred_fills) {
            self.core.advance_shadow(&f);
            self.strategy.on_fill(&mut self.core, &f);
        }
    }

    /// Deliver the ends of the orders the strategy placed by tag (`OrderBrokerSimulator::ended_seam_orders`) to
    /// `Strategy::on_order_event`, as live sends them: after the order's fill (live fires `on_fill`
    /// first) and before the strategy's next market hook. Both run loops call it right before every
    /// market hook (`on_bar`; `on_quote_tick`, `on_trade_tick`, `on_order_book`). An end whose order's
    /// fill the opt-in latency model still holds waits for that fill's delivery. No-op, and nothing
    /// allocated, for a run that placed no order by tag.
    pub(super) fn deliver_order_ends(&mut self) {
        let mut ends = self.core.ended_seam_orders();
        if !self.deferred_fills.is_empty() {
            let held = |e: &OrderEvent| self.deferred_fills.iter().any(|(_, f)| f.tag == e.tag);
            let (wait, now): (Vec<_>, Vec<_>) = ends.into_iter().partition(held);
            self.core.seam_ended.extend(wait);
            ends = now;
        }
        for event in ends {
            self.strategy.on_order_event(&mut self.core, &event);
        }
    }

    /// Apply every in-flight order action whose delivery stamp has arrived at `timestamp_ms`. No-op
    /// when the gate is not armed.
    pub(super) fn deliver_due_orders(&mut self, ts_ms: i64) {
        // The drain is scoped so the `&mut core.latency` borrow ends before `apply_in_flight`
        // takes `&mut core` (the queue is drained into an owned Vec).
        let due = {
            let Some(g) = self.core.latency.as_mut() else { return };
            if g.in_flight_len() == 0 {
                return;
            }
            g.drain_due(ts_ms)
        };
        for f in due {
            self.core.apply_in_flight(f.action);
        }
    }

    /// The per-fill volume cap (`volume_limit`): clamp `size` down to `volume_limit * vol`,
    /// recording the dropped remainder under `"volume_cap"`. The ONE copy of the clamp shared
    /// verbatim by every fill-dispatch site — the three `fill_pending*` lanes (via
    /// [`Self::dispatch_fill`]) and both `fill_step_gated` loops. Returns the (possibly clamped) size.
    /// A market-to-limit's remainder, and an opening auction's, is not dropped: it rests
    /// (`StrategyEngine::rest_remainder`). An ALL-OR-NONE limit the cap would cut is held WHOLE: `0.0`,
    /// nothing dropped, and it rests as it was.
    ///
    /// `vol` is in the unit the symbol's series counts: the order's own unit (a linear base, an
    /// inverse venue's contracts), compared with the size; or, for a series that counts the settle
    /// coin (`StrategyEngineConfig::coin_volume`, deribit's inverse candles), the coin the order is worth at
    /// the fill `price`, scaled down by the share of it the cap allows.
    pub(super) fn volume_clamp(
        &mut self,
        si: usize,
        o: &OrderResting,
        size: f64,
        vol: f64,
        price: f64,
    ) -> f64 {
        let mut fill_size = size;
        if let Some(vl) = self.core.volume_limit {
            let allowed = vl * vol;
            let capped = match self.core.coin_volume_of(si, fill_size, price) {
                None => (fill_size > allowed).then_some(allowed),
                Some(coin) => (coin > allowed).then(|| fill_size * (allowed / coin)),
            };
            if let Some(allowed) = capped {
                if o.all_or_none {
                    return 0.0;
                }
                if !matches!(o.kind, OrderKind::MarketToLimit | OrderKind::AuctionOpenToLimit) {
                    let dropped = fill_size - allowed;
                    let symbol_name = self.core.symbols[si].clone();
                    self.core.dropped.push(OrderDrop {
                        symbol: symbol_name,
                        reason: vike_model::OrderDenial::VolumeCap.to_string(),
                        size: dropped,
                        weight: 0.0,
                    });
                }
                fill_size = allowed;
            }
        }
        fill_size
    }
}
