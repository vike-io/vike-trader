//! The single-symbol fill: `emit_fill` books one resting order's fill and dual-publishes it.

use vike_execution::execution_engine::LOCAL_VIEW_QUANTITY_TOLERANCE;
use vike_fills::broker_sim::{adverse_fill_amount, adverse_fill_price};
use vike_model::events::{Event, FillEvent, OrderFilled, OrderPartiallyFilled, TradeId};
use vike_model::{Fill, OrderResting};

use super::ExecutionClientPaper;

impl ExecutionClientPaper {
    /// **The trade id of this book's next fill: `paper-<order client id>-<n>`**, `n` counting this book's fills (a
    /// combo's legs share an order). The engine's two fill-dedup ledgers come back from the journal at a restart
    /// and a book does not, so a bare `paper-<n>` counted from one again and the next process's first fills were
    /// refused as already folded, silently (a restored id carries no print): the order stayed `Accepted` with
    /// nothing booked (the CI box, ibkr paper, 2026-11-17). An order client id is never reused by an engine, so the id
    /// is new in every process. Ids already journaled stay as they were.
    pub(super) fn next_trade_id(&mut self, order_client_id: &str) -> TradeId {
        self.fill_n += 1;
        TradeId::prefixed("paper-", format_args!("{order_client_id}-{}", self.fill_n))
    }

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
        // The flat `slippage`, plus the opt-in impact model's charge on a taker, then the opt-in fixed
        // amount on a fill no limit bounds (`client/slippage.rs`): the simulator's `fill_terms` order. With
        // neither configured, `adverse_fill_price(raw, side, self.slippage)` exactly, as before.
        let symbol = self.symbol.clone();
        let slip = self.slippage_for(&symbol, order.size, ts, is_maker);
        let px = adverse_fill_price(raw_price, order.side, slip);
        let (px, _) =
            adverse_fill_amount(px, order.side, self.slippage_amount, order.is_limit_bounded());
        // `None` (the `new` path) = the byte-identical flat-rate primitive (r7 gate); `Some` = the
        // per-venue schedule the mount looked up. Slippage-adjusted `px` feeds both, as before. The
        // Deribit-options underlying accuracy (when an underlying source is wired) lives inside
        // `commission_for`; this single-symbol fill's instrument is this book's own `symbol`.
        let commission = self.commission_for(is_maker, order.size, px, &symbol, ts);
        self.position += order.side as f64 * order.size;
        // a filled order has left the resting book, so it can never expire afterwards
        // (no-op for the `Gtc` default, whose map entry was never inserted).
        self.expiry.shift_remove(order_client_id);
        let fill = FillEvent {
            trade_id: self.next_trade_id(order_client_id),
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
        // sim_exchange terminality: a cumulative short of the request by more than the one quantity
        // tolerance is a partial fill
        let wrap = if order.size < requested_quantity - LOCAL_VIEW_QUANTITY_TOLERANCE {
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
