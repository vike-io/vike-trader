//! `build_books` — folds the user's OWN deribit working orders + positions out of a
//! [`vike_core::CoreSnapshot`]'s `orders`/`positions` into the per-instrument
//! [`InstrumentBook`] map the options chain paints its markers from (the options-chain-orders
//! feature). Pure (no egui) so it is unit-tested in CI. Mirrors how the DOM tool reads
//! `snap.orders`/`snap.positions` for its working-order + position affordances — same venue
//! filter, same signed-size resolver (`dom_math::signed_position_size`).

use std::collections::BTreeMap;
use vike_chart::{InstrumentBook, WorkingOrderLite};
use vike_core::{OrderView, PositionView};

/// The one venue whose chain shows orders/positions (Deribit options).
const DERIBIT: &str = "deribit";

/// Build the `instrument_name -> InstrumentBook` lookup the options chain borrows. Keyed by the
/// order/position `symbol` (which IS the deribit instrument name, e.g. `BTC-25JUL25-60000-C`).
///
/// - Working orders: one [`WorkingOrderLite`] per LIVE (non-terminal) deribit order, aggregated
///   into the instrument's `working` vec in snapshot (registry) order.
/// - Position: the NET signed size across that instrument's position legs
///   ([`dom_math::signed_position_size`] resolves the sign — one-way legs carry it in the size,
///   hedge-mode legs in `position_side`).
///
/// Non-deribit orders/positions are ignored, so an instrument only appears once it has at least a
/// working order or a position leg — a strike with neither never gets a map entry (the renderer
/// then paints nothing there, byte-identical to before the feature).
pub fn build_books(
    orders: &[OrderView],
    positions: &[PositionView],
) -> BTreeMap<String, InstrumentBook> {
    let mut books: BTreeMap<String, InstrumentBook> = BTreeMap::new();
    for o in orders.iter().filter(|o| o.venue == DERIBIT && !o.status.is_terminal()) {
        books.entry(o.symbol.clone()).or_default().working.push(WorkingOrderLite {
            coid: o.client_order_id.clone(),
            side: o.side,
            qty: o.qty,
            price: o.price,
            filled_qty: o.filled_qty,
        });
    }
    for p in positions.iter().filter(|p| p.venue == DERIBIT) {
        books.entry(p.symbol.clone()).or_default().position_qty +=
            crate::dom_math::signed_position_size(p.size, &p.position_side);
    }
    books
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_exec::OrderStatus;

    fn order(venue: &str, sym: &str, coid: &str, side: i32, status: OrderStatus) -> OrderView {
        OrderView {
            client_order_id: coid.into(),
            venue: venue.into(),
            symbol: sym.into(),
            side,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(0.02),
            trigger_price: None,
            status,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
        }
    }

    fn position(venue: &str, sym: &str, side: &str, size: f64) -> PositionView {
        PositionView {
            venue: venue.into(),
            symbol: sym.into(),
            position_side: side.into(),
            size,
            avg_px: 100.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liq_price: 0.0,
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
            position("deribit", "ETH-P", "BOTH", -2.0),
            position("bybit", "ETHUSDT", "BOTH", 5.0), // non-deribit → excluded
        ];
        let books = build_books(&orders, &positions);
        assert_eq!(books.len(), 2, "only the two deribit instruments key the map");
        assert!(books.contains_key("BTC-C"));
        assert!(books.contains_key("ETH-P"));
        assert!(!books.contains_key("BTCUSDT"));
        assert!(!books.contains_key("ETHUSDT"));
    }

    #[test]
    fn aggregates_working_orders_per_instrument() {
        let orders = [
            order("deribit", "BTC-C", "a", 1, OrderStatus::Accepted),
            order("deribit", "BTC-C", "b", -1, OrderStatus::Accepted),
            order("deribit", "BTC-P", "c", 1, OrderStatus::Accepted),
        ];
        let books = build_books(&orders, &[]);
        assert_eq!(books["BTC-C"].working.len(), 2, "two resting orders on BTC-C");
        assert_eq!(books["BTC-C"].working[0].coid, "a", "snapshot order preserved");
        assert_eq!(books["BTC-C"].working[1].side, -1);
        assert_eq!(books["BTC-P"].working.len(), 1);
        assert_eq!(books["BTC-C"].position_qty, 0.0, "no position → flat");
    }

    #[test]
    fn terminal_orders_are_excluded() {
        let orders = [
            order("deribit", "BTC-C", "live", 1, OrderStatus::Accepted),
            order("deribit", "BTC-C", "done", 1, OrderStatus::Filled), // terminal → excluded
        ];
        let books = build_books(&orders, &[]);
        assert_eq!(books["BTC-C"].working.len(), 1, "only the live order survives");
        assert_eq!(books["BTC-C"].working[0].coid, "live");
    }

    #[test]
    fn position_qty_is_net_signed() {
        // one-way "BOTH" leg carries the sign in the size; a short is negative.
        let short = [position("deribit", "BTC-C", "BOTH", -3.0)];
        assert_eq!(build_books(&[], &short)["BTC-C"].position_qty, -3.0);
        // hedge-mode legs carry direction in the string, magnitude in size → summed net.
        let hedged =
            [position("deribit", "ETH-P", "LONG", 4.0), position("deribit", "ETH-P", "SHORT", 1.0)];
        assert_eq!(build_books(&[], &hedged)["ETH-P"].position_qty, 3.0, "4 long − 1 short = +3");
    }

    #[test]
    fn working_and_position_coexist_on_one_instrument() {
        let orders = [order("deribit", "BTC-C", "a", 1, OrderStatus::Accepted)];
        let positions = [position("deribit", "BTC-C", "BOTH", 2.0)];
        let books = build_books(&orders, &positions);
        let b = &books["BTC-C"];
        assert_eq!(b.working.len(), 1);
        assert_eq!(b.position_qty, 2.0);
    }
}
