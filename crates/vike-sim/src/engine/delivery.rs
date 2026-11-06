//! Fill and order delivery: the latency-gated `on_fill` path, due-fill and due-order release, the volume clamp.

use vike_model::{Fill, Strategy};

use super::{SimBroker, StrategyEngine};

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// Deliver a fill to the strategy.
    ///
    /// Immediate unless the opt-in latency gate is ARMED, in which case the fill is held until
    /// `exch_ts + response()` — the response leg. `exch_ts` is `core.now`, the timestamp of the
    /// tick whose matching produced the fill. Held fills are released by
    /// [`Self::deliver_due_fills`] at the top of each subsequent tick, and flushed once at the
    /// end of `run_ticks` so nothing is silently swallowed.
    pub(super) fn fire_on_fill(&mut self, fill: Fill) {
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
        self.strategy.on_fill(&mut self.core, &fill);
    }

    /// Release every response-delayed fill whose visibility stamp has arrived at `ts_ms`, in
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
    /// accounting would disagree with `BacktestResult`).
    pub(super) fn flush_deferred_fills(&mut self) {
        for (_, f) in std::mem::take(&mut self.deferred_fills) {
            self.core.advance_shadow(&f);
            self.strategy.on_fill(&mut self.core, &f);
        }
    }

    /// Apply every in-flight order action whose delivery stamp has arrived at `ts_ms`. No-op
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
    pub(super) fn volume_clamp(&mut self, si: usize, size: f64, vol: f64) -> f64 {
        let mut fill_size = size;
        if let Some(vl) = self.core.volume_limit {
            let allowed = vl * vol;
            if fill_size > allowed {
                let dropped = fill_size - allowed;
                let sym = self.core.symbols[si].clone();
                self.core.dropped.push((sym, "volume_cap".into(), dropped, 0.0));
                fill_size = allowed;
            }
        }
        fill_size
    }
}
