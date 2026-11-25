//! A tick run's equity curve kept by TIME (`EquitySampling::ByTime`, decision 0078's amendment of 2026-11-20): the
//! curve keeps the run's first priced tick and the last priced tick of each step, and every priced tick is folded into
//! what the closes cannot say of the path (`vike_analytics::TradingRecordSampling`).

use vike_analytics::TradingRecordSampling;
use vike_analytics::metrics::DrawdownWalk;

/// The by-time sampler `StrategyEngine::run_ticks` hands each priced tick's equity, the value `EveryTick` would push.
pub(super) struct EquityByTime {
    interval_ms: i64,
    /// The latest time seen, and the step it falls in: what the curve is stamped and stepped by. Feed latency
    /// (`StrategyEngineConfig::feed_latency`) delivers ticks by arrival, so a venue stamp can go back; a late tick then
    /// joins the step already open, and the closes stay one a step and in time order.
    seen: i64,
    step: i64,
    /// The latest tick's equity and time, and whether the curve holds it yet. It is not in [`Self::walk`] yet: the
    /// end-of-run sweeps may still patch the value the curve ends on ([`Self::finish`]).
    latest: Option<(f64, i64, bool)>,
    walk: DrawdownWalk,
    /// The highest and the lowest value folded: the path's peak and trough.
    max_equity: f64,
    min_equity: f64,
    reached_zero_at: Option<i64>,
    /// The first value folded, and how many were.
    first: Option<f64>,
    events: usize,
    flat: bool,
}

impl EquityByTime {
    /// A sampler of `interval_ms` steps (`> 0`; `run_ticks` keeps every tick for any other).
    pub(super) fn new(interval_ms: i64) -> Self {
        Self {
            interval_ms,
            seen: 0,
            step: 0,
            latest: None,
            walk: DrawdownWalk::default(),
            max_equity: f64::NEG_INFINITY,
            min_equity: f64::INFINITY,
            reached_zero_at: None,
            first: None,
            events: 0,
            flat: true,
        }
    }

    /// `equity` at a priced tick at `ts`: the run's first tick goes on the curve at once, and the tick before it goes
    /// on when this one opens a new step (that tick closed its step).
    pub(super) fn record(
        &mut self,
        equity: f64,
        ts: i64,
        curve: &mut Vec<f64>,
        stamps: &mut Vec<i64>,
    ) {
        let seen = if self.latest.is_some() { ts.max(self.seen) } else { ts };
        let step = seen.div_euclid(self.interval_ms);
        let kept = match self.latest {
            None => {
                curve.push(equity);
                stamps.push(ts);
                true
            }
            Some((value, at, kept)) => {
                self.fold(value, at);
                if step != self.step && !kept {
                    curve.push(value);
                    stamps.push(self.seen);
                }
                false
            }
        };
        self.latest = Some((equity, ts, kept));
        self.seen = seen;
        self.step = step;
    }

    /// End the curve with the run's last priced tick, before the end-of-run sweeps patch its last value.
    pub(super) fn close(&self, curve: &mut Vec<f64>, stamps: &mut Vec<i64>) {
        if let Some((value, _, false)) = self.latest {
            curve.push(value);
            stamps.push(self.seen);
        }
    }

    /// What the path says once the sweeps are done: the last tick folded at the value the curve ends on, as
    /// `vike_analytics::metrics::max_drawdown` walked the whole per-tick curve with its last value patched.
    pub(super) fn finish(mut self, curve: &[f64]) -> TradingRecordSampling {
        if let (Some((_, at, _)), Some(&last)) = (self.latest, curve.last()) {
            self.fold(last, at);
        }
        TradingRecordSampling {
            interval_ms: self.interval_ms,
            max_drawdown: self.walk.worst(),
            max_equity: self.max_equity,
            min_equity: self.min_equity,
            reached_zero_at: self.reached_zero_at,
            events: self.events,
            flat: self.flat,
        }
    }

    /// One event's equity, into what the record says of the path: its drawdown walk, its first zero, and the
    /// count and flatness the zero-trade diagnosis reads off a curve of every event.
    fn fold(&mut self, equity: f64, at: i64) {
        self.walk.push(equity);
        self.max_equity = self.max_equity.max(equity);
        self.min_equity = self.min_equity.min(equity);
        if equity <= 0.0 && self.reached_zero_at.is_none() {
            self.reached_zero_at = Some(at);
        }
        let first = *self.first.get_or_insert(equity);
        self.flat &= equity == first;
        self.events += 1;
    }
}
