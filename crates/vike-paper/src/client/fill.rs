//! The single-symbol fill: `emit_fill` books one resting order's fill and dual-publishes it.

use vike_fills::broker_sim::adverse_fill_price;
use vike_model::events::{Event, FillEvent, OrderFilled, OrderPartiallyFilled, TradeId};
use vike_model::{OrderKind, WorkingOrder};

use super::{PaperExecutionClient, PaperFill};

impl PaperExecutionClient {
    pub(super) fn emit_fill(
        &mut self,
        coid: &str,
        requested_qty: f64,
        order: &WorkingOrder,
        raw_px: f64,
        ts: i64,
    ) {
        // Maker/taker by ORDER KIND, not by crossing aggressiveness: a MARKETABLE limit (priced
        // through the book, filled on the next bar's open) books as a maker fill, here and in the
        // engine's `dispatch_fill`. See the classification caveat in `vike_model::money::fees` — it flips
        // the fee SIGN under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
        let is_maker = order.kind == OrderKind::Limit;
        let px = adverse_fill_price(raw_px, order.side, self.slippage);
        // `None` (the `new` path) = the byte-identical flat-rate primitive (r7 gate); `Some` = the
        // per-venue schedule the mount looked up. Slippage-adjusted `px` feeds both, as before. The
        // Deribit-options underlying accuracy (when an underlying source is wired) lives inside
        // `commission_for`; this single-symbol fill's instrument is this book's own `symbol`.
        let symbol = self.symbol.clone();
        let commission = self.commission_for(is_maker, order.size, px, &symbol, ts);
        self.position += order.side as f64 * order.size;
        // a filled order has left the resting book, so it can never expire afterwards
        // (no-op for the `Gtc` default, whose map entry was never inserted).
        self.expiry.shift_remove(coid);
        self.fill_n += 1;
        let fill = FillEvent {
            // Minted by the paper book, not read off a wire: `TradeId::prefixed` is infallible by
            // construction and renders the same `paper-<n>` bytes the `format!` did. The shape is a
            // journal key (the simulated-fill dedup set), so it must not change.
            trade_id: TradeId::prefixed("paper-", self.fill_n),
            client_order_id: coid.to_string(),
            venue: self.venue.clone().into(),
            symbol: self.symbol.clone().into(),
            side: order.side,
            last_qty: order.size,
            last_px: px,
            commission,
            commission_asset: String::new().into(),
            liquidity_side: if is_maker { "maker" } else { "taker" }.to_string().into(),
            ts,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        };
        self.fills.lock().unwrap().push(PaperFill {
            ts,
            side: order.side,
            qty: order.size,
            px,
            fee: commission,
            is_maker,
        });
        // bare fill FIRST (Account folds it), then the FSM wrap — the dual-publish rule
        self.events.push_back(Event::Fill(fill.clone()));
        // sim_exchange terminality: cumulative < requested - 1e-9 => partial
        let wrap = if order.size < requested_qty - 1e-9 {
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: coid.to_string(),
                fill,
                ts,
            })
        } else {
            Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts })
        };
        self.events.push_back(wrap);
    }
}
