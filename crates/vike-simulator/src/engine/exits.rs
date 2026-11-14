//! Protective exits: the stop-breach checks and the OCO sibling-cancel after a protective fill; and
//! the one-bar cap over one event's triggered orders, with the bar-path order of the order-list
//! entries among them.

use vike_fills::fill_resolution::resolve_intrabar_fills;
use vike_marketdata::Bar;
use vike_model::{OrderKind, OrderResting, Strategy};

use super::{OrderBrokerSimulator, StrategyEngine};

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// The one-bar cap over the orders one event triggered on `symbols[si]`
    /// (`vike_fills::fill_resolution::resolve_intrabar_fills`: adverse first, the reductions capped
    /// to the position), counted in `OrderBrokerSimulator::intrabar_both_hit`. With two or more order-list
    /// ENTRIES among them (`OrderBrokerSimulator::is_group_entry`) it is the shared
    /// `resolve_intrabar_fills_path_first`, the paper book's too: the entries take the bar's path
    /// order BEFORE the cap is allocated, so the leg the path reaches first gets the cap first; an
    /// exit keeps its adverse-first place and share. One order is returned as it came.
    ///
    /// An ALL-OR-NONE order the cap cannot leave whole comes back at size 0 (the shared rule): it did
    /// not fill, and it rests on at its full size, as in the paper book. Its caller skips the size 0.
    /// It is found again by its ORDER ID (`OrderResting::qid`, which `OrderBrokerSimulator::submit_limit_attributed`
    /// stamps on every all-or-none order), never by its terms: two identical exits of different sizes
    /// never swap.
    pub(super) fn resolve_triggered(
        &mut self,
        si: usize,
        triggered: Vec<(OrderResting, f64)>,
        event: &Bar,
    ) -> Vec<(OrderResting, f64)> {
        if triggered.len() < 2 {
            return triggered;
        }
        let mut whole: Vec<OrderResting> =
            triggered.iter().filter(|(o, _)| o.all_or_none).map(|(o, _)| o.clone()).collect();
        let pos = self.core.symbol[si].pos.size;
        let entries = triggered.iter().filter(|(o, _)| self.core.is_group_entry(o)).count();
        let (resolved, both) = if entries < 2 {
            resolve_intrabar_fills(triggered, pos)
        } else {
            let entry: Vec<bool> =
                triggered.iter().map(|(o, _)| self.core.is_group_entry(o)).collect();
            vike_fills::fill_resolution::resolve_intrabar_fills_path_first(
                triggered, pos, &entry, event,
            )
        };
        self.core.intrabar_both_hit += both;
        for (o, _) in resolved.iter().filter(|(o, _)| o.all_or_none && o.size <= 1e-12) {
            if let Some(k) = whole.iter().position(|w| w.qid == o.qid) {
                self.core.symbol[si].pending.push(whole.swap_remove(k));
            }
        }
        resolved
    }

    /// Two or more order-list ENTRIES triggered on one event (a straddle's two legs,
    /// `OrderBrokerSimulator::is_group_entry`) on a lane with no one-bar cap (the granular sub-bars, the
    /// shared-cash gated lane) fill in the order the bar's path reaches their fill prices
    /// (`vike_fills::fill_model::path_order`, the paper book's rule too), so the first one's OCO
    /// cancels the other before it fills: the plan's ASSUMPTION for entries, which a bar cannot
    /// show. They are reordered among their own slots; every other order keeps its place. A no-op
    /// unless two of them are there. The capped lanes take [`Self::resolve_triggered`].
    pub(super) fn order_group_entries(&self, fills: &mut [(OrderResting, f64)], event: &Bar) {
        if fills.len() < 2 {
            return;
        }
        let slots: Vec<usize> =
            (0..fills.len()).filter(|&i| self.core.is_group_entry(&fills[i].0)).collect();
        if slots.len() < 2 {
            return;
        }
        let prices: Vec<f64> = slots.iter().map(|&i| fills[i].1).collect();
        let order = vike_fills::fill_model::path_order(event, &prices);
        let entries: Vec<(OrderResting, f64)> =
            order.iter().map(|&k| fills[slots[k]].clone()).collect();
        for (&i, e) in slots.iter().zip(entries) {
            fills[i] = e;
        }
    }

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
        let stop = self.core.symbol[si].stop;
        let pos = self.core.symbol[si].pos;
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
        let mut probe = OrderResting::new(OrderKind::Stop, side, pos.size.abs());
        probe.price = Some(stop);
        let probe_bar = Bar {
            timestamp: ts,
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
        if let Some(fill) = self.core.apply_fill(si, side, pos.size.abs(), fp, ts, false, false) {
            self.core.symbol[si].stop = None;
            self.cancel_protective_exits(si, was_long);
            self.fire_on_fill(fill);
        }
    }

    /// Protective-stop breach BEFORE this step's fills (skips granular symbols).
    pub(super) fn check_protective_stops(&mut self, current_index: usize, skip: &[bool]) {
        for si in 0..self.core.symbols.len() {
            if skip[si] {
                continue;
            }
            // Read the crossing scalars without cloning the Bar (String-allocating); the armed-stop
            // guard lives in `check_stop`, which no-ops when nothing is armed.
            let (open, low, high, ts) = {
                let bar = &self.core.bars[si][current_index];
                (bar.open, bar.low, bar.high, bar.timestamp)
            };
            self.check_stop(si, open, low, high, ts);
        }
    }

    /// OCO sibling-cancel after a protective-stop fill (`_cancel_protective_exits`): the resting
    /// closing-side exits are the fired stop's kind-linked OCO siblings — the shared
    /// `vike_execution::is_protective_exit_sibling` law (ONE resolver with the paper book's
    /// order-client-id-linked `ContingencyBook` half). Runs from `check_stop` on EVERY lane since
    /// trigger-law wave 2 (it was granular-only). The maker law's quotes (`OrderBrokerSimulatorSymbolState::quotes`) are
    /// no exits of it and stay.
    fn cancel_protective_exits(&mut self, si: usize, was_long: bool) {
        let closing_side = if was_long { -1 } else { 1 };
        let st = &mut self.core.symbol[si];
        let quotes = &st.quotes;
        st.pending.retain(|o| {
            quotes.iter().any(|(q, _)| *q == o.qid)
                || !vike_execution::is_protective_exit_sibling(o.kind, o.side, closing_side)
        });
    }
}
