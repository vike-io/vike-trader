//! The strategy-facing order verbs and the higher-timeframe reads.

use vike_marketdata::Bar;
use vike_model::{OrderKind, OrderType, WorkingOrder, py_sum};

use super::state::SimBroker;

impl SimBroker {
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
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, stop); // sizer first, then the pre-trade gate
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Market, None, None);
        if size > 0.0 {
            let mut o = WorkingOrder::new(OrderKind::Market, side_sign, size);
            o.weight = weight;
            o.stop = stop;
            self.push_pending(si, o);
        }
    }

    pub fn submit_close(&mut self, symbol: &str) {
        let si = self.idx(symbol);
        let pos = self.sym[si].pos;
        if pos.size != 0.0 {
            let side = vike_model::closing_side(pos.size);
            let o = WorkingOrder::new(OrderKind::Market, side, pos.size.abs());
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
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, stop);
        let size =
            self.gate_order(si, side_sign, size, weight, OrderType::Limit, Some(price), None);
        if size <= 0.0 {
            return; // denied by the pre-trade gate — same shape as the market verbs
        }
        let mut o = WorkingOrder::new(OrderKind::Limit, side_sign, size);
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
        let si = self.idx(symbol);
        let last = self.sym[si].price;
        if last.is_finite()
            && last > 0.0
            && vike_fills::fill_model::post_only_would_cross(side_sign, price, last)
        {
            // recorded like every other refusal in the run's `dropped` list (symbol, reason, size, price)
            self.dropped.push((
                self.symbols[si].clone(),
                "post_only_would_cross".into(),
                size,
                price,
            ));
            return false;
        }
        self.submit_limit(symbol, side_sign, size, price, weight, raw, stop);
        true
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
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        // A stop's reference is its TRIGGER, which is what `check_inner`'s
        // `price.or(trigger_price)` falls to when there is no limit price.
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Stop, None, Some(price));
        if size <= 0.0 {
            return;
        }
        let mut o = WorkingOrder::new(OrderKind::Stop, side_sign, size);
        o.price = Some(price);
        o.weight = weight;
        self.push_pending(si, o);
    }

    /// A REDUCE-ONLY stop (`LiveBroker::submit_stop_reduce`'s twin): rests as a stop at `price` and
    /// may only shrink the position it meets. A flat book or a position on the order's own side is
    /// refused at once, in words, in [`SimBroker::dropped`]; the size is capped to the position now
    /// and again at the fill (`reduce_only_size`), so a stop whose position was closed another way
    /// drops instead of opening a new one. The size is the strategy's own (never sized by the sizer:
    /// a reduce is not an entry).
    pub fn submit_stop_reduce(&mut self, symbol: &str, side_sign: i32, size: f64, price: f64) {
        let si = self.idx(symbol);
        let size = self.reduce_only_size(si, side_sign, size);
        if size <= 0.0 {
            return;
        }
        let size = self.gate_order(si, side_sign, size, 0.0, OrderType::Stop, None, Some(price));
        if size <= 0.0 {
            return;
        }
        let mut o = WorkingOrder::new(OrderKind::Stop, side_sign, size);
        o.price = Some(price);
        o.reduce_only = true;
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
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        // A trailing order carries no price of its own — it references the mark, which is exactly
        // what `check_inner`'s `unwrap_or(ctx.mark_price)` fallback supplies.
        // It is judged as a `Stop` (it was the word "trailing", which the gate never knew): the
        // gate reads the kind ONLY in `take_scope`, where both are passive without a price.
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Stop, None, None);
        if size <= 0.0 {
            return;
        }
        let extreme_snap = self.sym[si].price;
        let mut o = WorkingOrder::new(OrderKind::Trailing, side_sign, size);
        o.trail = Some(trail);
        o.extreme = Some(extreme_snap);
        o.weight = weight;
        self.push_pending(si, o);
    }

    pub fn submit_market_close(
        &mut self,
        symbol: &str,
        side_sign: i32,
        size: f64,
        weight: f64,
        raw: bool,
    ) {
        let si = self.idx(symbol);
        let size = self.size_entry(si, side_sign, size, raw, None);
        let size = self.gate_order(si, side_sign, size, weight, OrderType::Market, None, None);
        if size > 0.0 {
            let mut o = WorkingOrder::new(OrderKind::MarketClose, side_sign, size);
            o.weight = weight;
            self.push_pending(si, o);
        }
    }

    /// Pull every untagged resting order for `symbol`. Under the opt-in latency gate the cancel
    /// is HELD in flight — which is the whole point: a cancel issued on the tick that would
    /// have filled the order arrives at the matching engine too late to save it.
    pub fn cancel_all(&mut self, symbol: &str) {
        let si = self.idx(symbol);
        if self.latency.is_some() {
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &crate::latency::LatencyOrder::NONE,
                crate::latency::InFlightAction::CancelAll { si },
            );
            if !sent {
                // a rejected CANCEL is the dangerous one: the orders stay resting
                self.record_latency_reject(si, 0.0, 0.0);
            }
            return;
        }
        self.sym[si].pending.clear();
    }

    /// The STRATEGY-level target verb (`Strategy._engine_target`): sizes through the ONE shared
    /// sizing law `vike_model::units_from_percent` (`pct·eq/(price·multiplier)`) — the SAME
    /// converter the live `order_target_percent` verb uses (`vike-core::runtime::broker`), so a
    /// mult≠1 instrument sizes identically live and in backtest. 1e-12 dead-band, submitted raw.
    /// At mult==1 `price·1.0 == price` exactly, so this is bit-identical to the former
    /// `pct·eq/price` (and the price==0 guard is folded into `units_from_percent`'s `denom>0.0`).
    pub fn strategy_order_target_percent(&mut self, symbol: &str, pct: f64) {
        let price = self.price_of(symbol);
        let mult = self.multiplier_of(symbol);
        let target = vike_model::units_from_percent(pct, self.equity_now(), price, mult);
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
    /// ⚠ An unknown SYMBOL still panics, in `idx`; that is a separate defect.
    ///
    /// ⚠ **This `None` is a SUBSTITUTE for an unmet spec clause, not the clause itself.**
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §9.1(2) required the
    /// undeclared-timeframe failure to become a LOAD-TIME refusal — "checked against the
    /// strategy's declared timeframe requirement" — before the `timeframes` profile key shipped.
    /// That check was not built (strategies declare no timeframe requirement, and giving them one
    /// is a different plan), and the loud `panic!("timeframe {tf:?} not registered")` this method
    /// used to carry became this quiet `None`. So a strategy asking for a timeframe the profile
    /// did not declare now runs on NO higher-TF data instead of aborting. The trade is accepted
    /// because it is latent — this method and [`Self::forming_for`] have zero callers anywhere in
    /// this tree — and it closes when a strategy-declared timeframe requirement exists.
    pub fn bars_for(&self, symbol: &str, tf: &str) -> Option<&[Bar]> {
        let si = self.idx(symbol);
        let (_, ms, coarse) = self.sym[si].tf.iter().find(|(name, _, _)| name == tf)?;
        let window_start = self.now - self.now.rem_euclid(*ms);
        let idx = coarse.partition_point(|b| b.ts < window_start);
        Some(&coarse[..idx])
    }

    /// The partially-formed coarse bar of `tf` for `symbol`, or `None`.
    ///
    /// ⚠ `None` now carries TWO meanings: no base bars have landed in the current window yet, and
    /// the timeframe is not registered at all. They were one panic and one `None` before; a caller
    /// that must tell them apart should ask `bars_for` first, which answers only the second.
    pub fn forming_for(&self, symbol: &str, tf: &str) -> Option<Bar> {
        let si = self.idx(symbol);
        let (_, ms, _) = self.sym[si].tf.iter().find(|(name, _, _)| name == tf)?;
        let window_start = self.now - self.now.rem_euclid(*ms);
        let base = &self.bars[si];
        let lo = base.partition_point(|b| b.ts < window_start);
        let hi = base.partition_point(|b| b.ts <= self.now);
        let window = &base[lo..hi];
        if window.is_empty() {
            return None;
        }
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        for b in window {
            high = high.max(b.high);
            low = low.min(b.low);
        }
        Some(Bar {
            ts: window_start,
            open: window[0].open,
            high,
            low,
            close: window[window.len() - 1].close,
            volume: py_sum(window.iter().map(|b| b.volume)),
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
    }
}
