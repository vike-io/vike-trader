//! The single-symbol fill: `emit_fill` books one resting order's fill and dual-publishes it.

use vike_fills::broker_sim::adverse_fill_price;
use vike_model::events::{Event, FillEvent, OrderFilled, OrderPartiallyFilled, TradeId};
use vike_model::{Fill, OrderResting};

use super::ExecutionClientPaper;

impl ExecutionClientPaper {
    pub(super) fn emit_fill(
        &mut self,
        order_client_id: &str,
        requested_quantity: f64,
        order: &OrderResting,
        raw_price: f64,
        ts: i64,
    ) {
        // Maker/taker by ORDER KIND, not by crossing aggressiveness: a MARKETABLE limit (priced
        // through the book, filled on the next bar's open) books as a maker fill, here and in the
        // engine's `dispatch_fill`. See the classification caveat in `vike_model::money::fees` — it flips
        // the fee SIGN under a rebate-bearing `FeeSchedule::ProbabilityScaled`.
        let is_maker = order.books_as_maker();
        let px = adverse_fill_price(raw_price, order.side, self.slippage);
        // `None` (the `new` path) = the byte-identical flat-rate primitive (r7 gate); `Some` = the
        // per-venue schedule the mount looked up. Slippage-adjusted `px` feeds both, as before. The
        // Deribit-options underlying accuracy (when an underlying source is wired) lives inside
        // `commission_for`; this single-symbol fill's instrument is this book's own `symbol`.
        let symbol = self.symbol.clone();
        let commission = self.commission_for(is_maker, order.size, px, &symbol, ts);
        self.position += order.side as f64 * order.size;
        // a filled order has left the resting book, so it can never expire afterwards
        // (no-op for the `Gtc` default, whose map entry was never inserted).
        self.expiry.shift_remove(order_client_id);
        self.fill_n += 1;
        let fill = FillEvent {
            // Minted by the paper book, not read off a wire: `TradeId::prefixed` is infallible by
            // construction and renders the same `paper-<n>` bytes the `format!` did. The shape is a
            // journal key (the simulated-fill dedup set), so it must not change.
            trade_id: TradeId::prefixed("paper-", self.fill_n),
            order_client_id: order_client_id.to_string(),
            venue: self.venue.clone().into(),
            symbol: self.symbol.clone().into(),
            side: order.side,
            last_qty: order.size,
            last_price: px,
            commission,
            commission_asset: self.commission_asset_for(&symbol).into(),
            liquidity_side: if is_maker { "maker" } else { "taker" }.to_string().into(),
            timestamp: ts,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        };
        self.fills.lock().unwrap().push(Fill {
            side: order.side,
            size: order.size,
            price: px,
            fee: commission,
            timestamp: ts,
            is_maker,
            symbol,
            // the book knows no strategy tag; the engine stamps the strategy's own `Fill`
            tag: None,
        });
        // bare fill FIRST (Account folds it), then the FSM wrap — the dual-publish rule
        self.events.push_back(Event::Fill(fill.clone()));
        // sim_exchange terminality: cumulative < requested - 1e-9 => partial
        let wrap = if order.size < requested_quantity - 1e-9 {
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                order_client_id: order_client_id.to_string(),
                fill,
                timestamp: ts,
            })
        } else {
            Event::OrderFilled(OrderFilled {
                order_client_id: order_client_id.to_string(),
                fill,
                timestamp: ts,
            })
        };
        self.events.push_back(wrap);
    }
}
