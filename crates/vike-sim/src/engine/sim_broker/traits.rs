//! `SimBroker` as the strategy's `Broker`, and the single-symbol `HftBroker` maker surface.

use vike_marketdata::Bar;
use vike_model::{Broker, HftBroker, OrderKind, OrderType, WorkingOrder};

use super::state::SimBroker;

/// The sim engine core is the strategy's `Broker` in backtest: DIRECT mutation, monomorphized
/// dispatch (never `dyn` — the hot loop must not pay a vtable per verb). Portable strategies
/// (`impl<B: Broker> Strategy<B>`) get exactly this common-denominator surface; backtest-rich
/// verbs (weighted/bracket submits, cancel_all, target-percent, schedule) stay inherent on
/// `SimBroker` for `impl Strategy<SimBroker>` strategies.
impl Broker for SimBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        // raw = true: the portable surface has no sizer concept (neither does the live path)
        self.submit(symbol, side, qty, 0.0, true, None);
    }

    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        SimBroker::submit_limit(self, symbol, side, qty, price, 0.0, true, None);
    }

    fn position(&self, symbol: &str) -> f64 {
        // The STRATEGY's view: the response-latency shadow while the opt-in latency gate is armed
        // (see `SimBroker::shadow_pos`), exchange truth otherwise — and `shadow_pos` is EMPTY on
        // every path but an armed `run_ticks`, so this is the frozen line verbatim there.
        // `position_of` is the un-shadowed read for anything that needs the exchange's own state.
        let si = self.idx(symbol);
        if !self.shadow_pos.is_empty() {
            return self.shadow_pos[si];
        }
        self.sym[si].pos.size
    }

    fn price(&self, symbol: &str) -> f64 {
        self.price_of(symbol)
    }

    fn equity(&self) -> f64 {
        self.equity_now()
    }

    fn bars(&self, symbol: &str) -> &[Bar] {
        // closed bars up to & including the current step (tick runs may outrun the bar series)
        let si = self.idx(symbol);
        let series: &Vec<Bar> = &self.bars[si];
        let end = (self.index + 1).min(series.len());
        &series[..end]
    }

    fn index(&self) -> usize {
        self.index
    }

    fn now(&self) -> i64 {
        self.now
    }

    /// Answered from the replayed L2 book with EXACTLY the law the fill path uses
    /// ([`vike_marketdata::book_taker_price`]) — so a strategy that gates on this read and then
    /// submits that size gets the fill it was quoted, and the two can never drift apart into a
    /// live-vs-backtest divergence.
    ///
    /// `None` on every book-less path (bar engine, vector kernel, tick replay with no `Tick::Book`
    /// series, unknown symbol) and whenever displayed depth cannot cover `qty`.
    fn quote_vwap(&self, symbol: &str, side: i32, qty: f64) -> Option<f64> {
        let si = self.symbols.iter().position(|s| s == symbol)?;
        vike_marketdata::book_taker_price(self.book_at(si)?, side, qty, None)
    }

    /// Displayed size at `limit_px` or better, straight off the replayed book. `0.0` on every
    /// book-less path and for an unknown symbol — which correctly sizes a depth-sizing strategy
    /// down to no trade rather than to an imaginary one.
    fn depth_within_price(&self, symbol: &str, side: i32, limit_px: f64) -> f64 {
        let Some(si) = self.symbols.iter().position(|s| s == symbol) else { return 0.0 };
        self.book_at(si).map_or(0.0, |b| b.quantity_for_price(side, limit_px))
    }
}

impl SimBroker {
    /// The symbol index the single-symbol [`HftBroker`] surface targets: the FIRST (index 0), which
    /// IS the mounted (venue, symbol) series for a maker backtest — you mount the maker on a
    /// one-symbol `SimBroker`. The `HftBroker` trait is single-symbol scoped by contract (no symbol
    /// argument on its verbs); a multi-symbol `SimBroker` is a portfolio construct that does not
    /// mount an `HftBroker` strategy.
    pub(super) const HFT_SI: usize = 0;
}

/// The backtest HFT maker surface ([`HftBroker`]): tagged resting-limit submit / modify / cancel +
/// the mounted symbol's signed position. This is what lets `SpreadMaker` (and any future
/// `impl<B: HftBroker> Strategy<B>` maker) mount in the backtest engine — the crossing fills of the
/// resting tagged limits are applied by `StrategyEngine::fill_tagged` on the standard bar and tick
/// paths. Single-symbol scoped (see [`SimBroker::HFT_SI`]); the verbs never touch the untagged
/// `pending` lane, so every existing (parity-gated) backtest is byte-for-byte unaffected.
impl HftBroker for SimBroker {
    fn position(&self) -> f64 {
        // The maker's OWN view of its inventory: the response-latency shadow while the opt-in
        // gate is armed, exchange truth otherwise (`shadow_pos` is empty on every other path, so
        // this is the frozen line verbatim). `SpreadMaker` polls this on every requote to drive
        // its skew and its A-S reservation price — it is the single most important read the
        // response leg has to cover. See `SimBroker::shadow_pos`.
        if !self.shadow_pos.is_empty() {
            return self.shadow_pos[Self::HFT_SI];
        }
        self.sym[Self::HFT_SI].pos.size
    }

    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        // A passive resting LIMIT (maker) at `price`. RAW: no sizer, no leverage cap — the portable
        // HFT surface, exactly like `Broker::submit_limit`. Inserting under `tag` REPLACES any prior
        // resting order for that tag (a re-submit), matching the live tag→coid registry's overwrite.
        // A RESOLVED market refuses new quotes, exactly as `push_pending` refuses new orders — the
        // maker keeps re-quoting on every tick, so the tag must simply not rest (nothing recorded).
        if self.is_resolved_idx(Self::HFT_SI) {
            return;
        }
        // ⚠ RAW means no SIZER and no leverage cap — it never meant no RISK GATE. This verb is how
        // `vike-mm` places every quote it has, so leaving it ungated meant the maker's whole
        // backtest ran outside the operator's `[risk]` budget while live judged each quote.
        // `weight` is 0.0: the portable HFT surface carries no per-order weight, matching the
        // `raw` market path.
        let qty =
            self.gate_order(Self::HFT_SI, side, qty, 0.0, OrderType::Limit, Some(price), None);
        if qty <= 0.0 {
            return;
        }
        let mut o = WorkingOrder::new(OrderKind::Limit, side, qty);
        o.price = Some(price);
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(side, qty, Some(price));
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &desc,
                crate::latency::InFlightAction::SubmitTagged { tag: tag.to_string(), order: o },
            );
            if !sent {
                self.record_latency_reject(Self::HFT_SI, qty, 0.0);
            }
            return;
        }
        self.sym[Self::HFT_SI].tagged.insert(tag.to_string(), o);
    }

    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        // Re-price / re-size the resting order IN PLACE (there is no queue to lose in the sim — see
        // `fill_tagged`'s documented limits). No-op if the tag is unknown or already filled/canceled
        // (its entry was removed) — the trait contract, and what the live `modify_order` gate does on
        // a terminal order.
        if self.latency.is_some() {
            let desc = crate::latency::LatencyOrder::new(0, new_qty.unwrap_or(0.0), new_price);
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &desc,
                crate::latency::InFlightAction::ModifyTagged {
                    tag: tag.to_string(),
                    new_qty,
                    new_price,
                },
            );
            if !sent {
                self.record_latency_reject(Self::HFT_SI, new_qty.unwrap_or(0.0), 0.0);
            }
            return;
        }
        if let Some(o) = self.sym[Self::HFT_SI].tagged.get_mut(tag) {
            if let Some(q) = new_qty {
                // An amend UP is a new order at the back of the queue on every real venue
                // (an amend DOWN keeps priority). Clearing the engine-local identity is how
                // that is signalled to the OPT-IN queue model, which re-seeds on an identity
                // change; nothing else reads `qid`, so the default path is unaffected.
                if q > o.size {
                    o.qid = 0;
                }
                o.size = q;
            }
            if let Some(p) = new_price {
                o.price = Some(p);
            }
        }
    }

    fn cancel_tagged(&mut self, tag: &str) {
        // Pull ONE resting quote and leave every other tag untouched; no-op on an unknown/terminal
        // tag. `shift_remove` (not `swap_remove`) keeps the survivors in insertion order for a
        // deterministic replay. Under the opt-in latency gate the pull is HELD in flight.
        if self.latency.is_some() {
            let now = self.now;
            let g = self.latency.as_mut().expect("checked Some");
            let sent = g.submit(
                now,
                &crate::latency::LatencyOrder::NONE,
                crate::latency::InFlightAction::CancelTagged { tag: tag.to_string() },
            );
            if !sent {
                // a rejected CANCEL is the dangerous one: the quote stays resting
                self.record_latency_reject(Self::HFT_SI, 0.0, 0.0);
            }
            return;
        }
        self.sym[Self::HFT_SI].tagged.shift_remove(tag);
    }
}
