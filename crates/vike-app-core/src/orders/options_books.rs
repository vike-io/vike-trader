//! `build_books` — folds the user's OWN deribit working orders + positions out of a
//! [`vike_execution::CoreSnapshot`]'s `orders`/`positions` into the per-instrument maps the options
//! chain paints its markers from (`vike_chart::OptionChainInputs`'s `working` and `positions`, the
//! options-chain-orders feature). Pure (no egui) so it is unit-tested in CI. Mirrors how the Trade
//! window's glue reads `snap.orders`/`snap.positions` for its working-order + position affordances
//! (as the DOM's did before it) — a venue filter, and the same signed-size resolver
//! (`dom_math::signed_position_size`).

use std::collections::BTreeMap;
use vike_execution::{OrderView, PositionView};
use vike_model::Position;

/// The one venue whose chain shows orders/positions (Deribit options).
const DERIBIT: &str = "deribit";

/// Build the two `instrument_name -> …` lookups the options chain borrows: our working orders and
/// our net position. Keyed by the order/position `symbol` (which IS the deribit instrument name,
/// e.g. `BTC-25JUL25-60000-C`).
///
/// - Working orders: the order client id of every LIVE (non-terminal) deribit order row, aggregated
///   into the instrument's vec in snapshot (registry) order: plain data, so the chart links no
///   execution crate (the marker counts them and cancels the first).
/// - Position: the NET signed size across that instrument's position legs, in
///   [`Position::size`] ([`super::dom_math::signed_position_size`] resolves the sign — one-way legs
///   carry it in the size, hedge-mode legs in `position_side`). `average_price` is not carried (left
///   `0.0`): the chain paints the size alone.
///
/// Non-deribit orders/positions are ignored, so an instrument only appears once it has at least a
/// working order or a position leg — a strike with neither never gets a map entry (the renderer
/// then paints nothing there, byte-identical to before the feature).
pub fn build_books(
    orders: &[OrderView],
    positions: &[PositionView],
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, Position>) {
    let mut working: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for o in orders.iter().filter(|o| o.venue == DERIBIT && !o.status.is_terminal()) {
        working.entry(o.symbol.clone()).or_default().push(o.order_client_id.clone());
    }
    let mut net: BTreeMap<String, Position> = BTreeMap::new();
    for p in positions.iter().filter(|p| p.venue == DERIBIT) {
        net.entry(p.symbol.to_string()).or_default().size +=
            crate::orders::dom_math::signed_position_size(p.size, p.position_side);
    }
    (working, net)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::OrderStatus;
    use vike_model::events::PositionSide;

    fn order(
        venue: &str,
        symbol_name: &str,
        order_client_id: &str,
        side: i32,
        status: OrderStatus,
    ) -> OrderView {
        OrderView {
            data_delay: None,
            order_client_id: order_client_id.into(),
            venue: venue.into(),
            account: None,
            symbol: symbol_name.into(),
            side,
            qty: 1.0,
            order_type: vike_model::OrderType::Limit,
            price: Some(0.02),
            trigger_price: None,
            status,
            venue_order_id: None,
            filled_qty: 0.0,
            average_fill_price: 0.0,
            reason: None,
            pending_price: None,
            trail: None,
            limit_offset: None,
            attributes: None,
            trail_percent: None,
            adjust: None,
            release: None,
            display_quantity: None,
        }
    }

    fn position(venue: &str, symbol_name: &str, side: PositionSide, size: f64) -> PositionView {
        PositionView {
            venue: venue.into(),
            symbol: symbol_name.into(),
            position_side: side,
            size,
            average_price: 100.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liquidation_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }

    #[test]
    fn keys_only_deribit_instruments() {
        let orders = [
            order("deribit", "BTC-C", "a", 1, OrderStatus::Accepted),
            order("binance", "BTCUSDT", "b", 1, OrderStatus::Accepted), // non-deribit → excluded
        ];
        let positions = [
            position("deribit", "ETH-P", PositionSide::Both, -2.0),
            position("bybit", "ETHUSDT", PositionSide::Both, 5.0), // non-deribit → excluded
        ];
        let (working, net) = build_books(&orders, &positions);
        assert_eq!(working.keys().collect::<Vec<_>>(), ["BTC-C"], "only the deribit order keys");
        assert_eq!(net.keys().collect::<Vec<_>>(), ["ETH-P"], "only the deribit position keys");
    }

    #[test]
    fn aggregates_working_orders_per_instrument() {
        let orders = [
            order("deribit", "BTC-C", "a", 1, OrderStatus::Accepted),
            order("deribit", "BTC-C", "b", -1, OrderStatus::Accepted),
            order("deribit", "BTC-P", "c", 1, OrderStatus::Accepted),
        ];
        let (working, net) = build_books(&orders, &[]);
        assert_eq!(working["BTC-C"], ["a", "b"], "two resting orders on BTC-C, in snapshot order");
        assert_eq!(working["BTC-P"], ["c"]);
        assert!(net.is_empty(), "no position → flat");
    }

    #[test]
    fn terminal_orders_are_excluded() {
        let orders = [
            order("deribit", "BTC-C", "live", 1, OrderStatus::Accepted),
            order("deribit", "BTC-C", "done", 1, OrderStatus::Filled), // terminal → excluded
        ];
        let (working, _) = build_books(&orders, &[]);
        assert_eq!(working["BTC-C"], ["live"], "only the live order survives");
    }

    #[test]
    fn position_quantity_is_net_signed() {
        // one-way "BOTH" leg carries the sign in the size; a short is negative.
        let short = [position("deribit", "BTC-C", PositionSide::Both, -3.0)];
        assert_eq!(build_books(&[], &short).1["BTC-C"].size, -3.0);
        // hedge-mode legs carry direction in the string, magnitude in size → summed net.
        let hedged = [
            position("deribit", "ETH-P", PositionSide::Long, 4.0),
            position("deribit", "ETH-P", PositionSide::Short, 1.0),
        ];
        assert_eq!(build_books(&[], &hedged).1["ETH-P"].size, 3.0, "4 long − 1 short = +3");
    }

    #[test]
    fn working_and_position_coexist_on_one_instrument() {
        let orders = [order("deribit", "BTC-C", "a", 1, OrderStatus::Accepted)];
        let positions = [position("deribit", "BTC-C", PositionSide::Both, 2.0)];
        let (working, net) = build_books(&orders, &positions);
        assert_eq!(working["BTC-C"].len(), 1);
        assert_eq!(net["BTC-C"].size, 2.0);
    }
}
