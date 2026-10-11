//! Protective exits: the stop-breach checks and the OCO sibling-cancel after a protective fill.

use vike_model::{Bar, OrderKind, Strategy, WorkingOrder};

use super::{SimBroker, StrategyEngine};

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// Protective-stop breach for ONE symbol against a bar's `open`/`low`/`high`/`ts`: if an
    /// armed stop is crossed (long: `low <= stop`; short: `high >= stop`) close the position at
    /// the shared trigger oracle's price and fire `on_fill`. Trigger-law wave 2 (law-map A3/A4)
    /// unified the three inline copies' semantics:
    ///
    /// - the FILL PRICE is `vike_model::order_fill_price`'s Stop arm — a bar that OPENS through
    ///   the stop fills at the gapped open (ADVERSE), exactly like a resting stop order; a
    ///   non-gapping breach still fills at the stop, byte-identical to the retired at-stop law
    ///   (on the tick lane the event is a one-price bar, so a through-print fills at the print);
    /// - the OCO sibling-cancel runs on EVERY lane (coarse / granular / tick — it was
    ///   granular-only, letting a wide bar fire the stop AND the TP into a spurious reversal).
    ///
    /// No-ops when no stop is armed / flat.
    pub(super) fn check_stop(&mut self, si: usize, open: f64, low: f64, high: f64, ts: i64) {
        let stop = self.core.sym[si].stop;
        let pos = self.core.sym[si].pos;
        let (Some(stop), true) = (stop, pos.size != 0.0) else {
            return;
        };
        // Session gate (opt-in; no-op by default), checked only once a stop is actually ARMED so
        // the counter measures refused stop passes rather than every quiet event. Unlike the
        // staleness discipline — which deliberately exempts protective stops so a stale price
        // cannot strand live risk — a CLOSED venue cannot fill a stop at all. The stop stays
        // armed and fires on the first in-session event at that event's price, which is the
        // adverse-gap arm documented above. Through [`Self::defer_session`], the ONE writer of
        // `session_deferrals` — the armed-stop guard above already established the "only count a
        // refused pass" precondition, exactly as the tagged lanes' empty-map check does.
        if self.defer_session(si, ts) {
            return;
        }
        let was_long = pos.size > 0.0;
        let side = if was_long { -1 } else { 1 };
        // A throwaway probe order + bar hand the armed stop level to the ONE trigger oracle.
        // The Stop arm reads `open`/`high`/`low` only — `close` is a placeholder, and a bare
        // crossing check carries no volume/quote context — and never mutates the probe.
        let mut probe = WorkingOrder::new(OrderKind::Stop, side, pos.size.abs());
        probe.price = Some(stop);
        let probe_bar = Bar {
            ts,
            open,
            high,
            low,
            close: open,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        };
        let Some(fp) = vike_model::order_fill_price(&mut probe, &probe_bar) else {
            return;
        };
        if let Some(fill) = self.core.apply_fill(si, side, pos.size.abs(), fp, ts, false) {
            self.core.sym[si].stop = None;
            self.cancel_protective_exits(si, was_long);
            self.fire_on_fill(fill);
        }
    }

    /// Protective-stop breach BEFORE this step's fills (skips granular symbols).
    pub(super) fn check_protective_stops(&mut self, cur_idx: usize, skip: &[bool]) {
        for si in 0..self.core.symbols.len() {
            if skip[si] {
                continue;
            }
            // Read the crossing scalars without cloning the Bar (String-allocating); the armed-stop
            // guard lives in `check_stop`, which no-ops when nothing is armed.
            let (open, low, high, ts) = {
                let bar = &self.core.bars[si][cur_idx];
                (bar.open, bar.low, bar.high, bar.ts)
            };
            self.check_stop(si, open, low, high, ts);
        }
    }

    /// OCO sibling-cancel after a protective-stop fill (`_cancel_protective_exits`): the resting
    /// closing-side exits are the fired stop's kind-linked OCO siblings — the shared
    /// `vike_exec::is_protective_exit_sibling` law (ONE resolver with the paper book's
    /// coid-linked `ContingencyBook` half). Runs from `check_stop` on EVERY lane since
    /// trigger-law wave 2 (it was granular-only).
    fn cancel_protective_exits(&mut self, si: usize, was_long: bool) {
        let closing_side = if was_long { -1 } else { 1 };
        self.core.sym[si]
            .pending
            .retain(|o| !vike_exec::is_protective_exit_sibling(o.kind, o.side, closing_side));
    }
}
