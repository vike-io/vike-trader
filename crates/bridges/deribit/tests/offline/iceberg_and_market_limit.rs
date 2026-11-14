//! Deribit's iceberg (`display_amount`) and market-to-limit (`market_limit`) over the local WebSocket
//! stand-in in `crate::fake_deribit_websocket`. No venue, no credentials, no `#[ignore]`.
//!
//! What these pin, frame by frame:
//!
//! * a `market_to_limit` goes out as the venue's own `market_limit`, with no price and no lifetime
//!   (before it was built it was refused by name; it was never a plain `market`);
//! * a limit with a `display_qty` carries `display_amount` on the amount's lot grid, and an edit of it
//!   restates the slice; a limit without one is byte-identical to before;
//! * a display the venue cannot take (an option, a combo, a kind that is not a limit, a non-positive
//!   slice) is refused BY NAME with nothing on the socket, never sent at the order's full size;
//! * the restart reconcile reads a `market_limit` row as a `MarketToLimit`.
//!
//! The venue's behaviour behind both kinds (does a partial `market_limit` rest at its first fill's price,
//! what does a resting iceberg read back as) is the testnet drill's,
//! `tests/deribit_smokes/deribit_iceberg_and_market_limit_testnet_smoke.rs`, which this wave does not run.

use std::assert_matches;

use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_deribit::execution::rest::VenueRestDeribit;
use vike_deribit::reconcile::parse_open_orders;
use vike_model::events::Event;
use vike_model::{AssetClass, OrderRequest, OrderType, SymbolProperties};

use crate::fake_deribit_websocket::{
    FakeDeribit, ORDER_ID, SYMBOL, Session, outcome, transport_against,
};

const ORDER_CLIENT_ID: &str = "vike-kind-1";

/// `BTC-PERPETUAL`'s captured grid (tick 0.5, lot 10) and a captured BTC option row's.
fn grids() -> (SymbolProperties, String, SymbolProperties) {
    let all: Value =
        serde_json::from_str(include_str!("../fixtures/deribit_get_instruments.json")).unwrap();
    let perp = all["future"]["result"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["instrument_name"] == SYMBOL))
        .expect("the captured BTC-PERPETUAL row");
    let option = &all["option_btc"]["result"][0];
    (
        vike_deribit::catalog::properties_of(perp).expect("a perp grid"),
        option["instrument_name"].as_str().expect("a name").to_string(),
        vike_deribit::catalog::properties_of(option).expect("an option grid"),
    )
}

fn perp_grid() -> SymbolProperties {
    grids().0
}

fn rest_on(fake: &FakeDeribit, symbol: &str, properties: SymbolProperties) -> VenueRestDeribit {
    VenueRestDeribit::new(transport_against(fake), symbol, properties, "BTC")
}

/// A BUY of `qty` on the perpetual, of `kind`, priced `price`.
fn buy(kind: OrderType, qty: f64, price: Option<f64>) -> OrderRequest {
    OrderRequest {
        order_client_id: ORDER_CLIENT_ID.to_string(),
        venue: "deribit".to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty,
        order_type: kind,
        price,
        timestamp: 1_700_000_000_000,
        ..Default::default()
    }
}

/// The one frame the venue received for `method`.
fn sent(fake: &FakeDeribit, method: &str) -> Value {
    let mut all = fake.params_of(method);
    assert_eq!(all.len(), 1, "exactly one {method} reached the venue");
    all.remove(0)
}

/// THE WIRE SHAPE of a market-to-limit: `market_limit`, nothing else beside the plain market keys.
#[test]
fn a_market_to_limit_goes_out_as_a_market_limit() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    assert_matches!(
        outcome(rest.submit_order(&buy(OrderType::MarketToLimit, 20.0, None))),
        Event::OrderAccepted(a) if a.venue_order_id.as_ref().map(|v| v.as_str()) == Some(ORDER_ID)
    );
    assert_eq!(
        sent(&fake, "private/buy"),
        json!({
            "instrument_name": SYMBOL,
            "amount": 20.0,
            "type": "market_limit",
            "label": ORDER_CLIENT_ID,
            "post_only": false,
        })
    );
}

/// An iceberg: the limit's whole frame, with `display_amount` floored to the lot grid like `amount`.
#[test]
fn a_limit_with_a_display_quantity_carries_display_amount_on_the_lot_grid() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut req = buy(OrderType::Limit, 2_000.0, Some(40_000.0));
    req.display_quantity = Some(1_004.0);
    assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));
    assert_eq!(
        sent(&fake, "private/buy"),
        json!({
            "instrument_name": SYMBOL,
            "amount": 2_000.0,
            "type": "limit",
            "label": ORDER_CLIENT_ID,
            "post_only": false,
            "price": 40_000.0,
            "display_amount": 1_000.0,
        })
    );
}

/// A limit with no display quantity writes no `display_amount` key at all.
#[test]
fn a_limit_without_a_display_quantity_writes_no_display_key() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    assert_matches!(
        outcome(rest.submit_order(&buy(OrderType::Limit, 20.0, Some(40_000.0)))),
        Event::OrderAccepted(_)
    );
    assert!(sent(&fake, "private/buy").get("display_amount").is_none());
}

/// An edit of a resting iceberg restates its visible slice beside the new price.
#[test]
fn an_edit_of_an_iceberg_restates_its_display_amount() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut req = buy(OrderType::Limit, 2_000.0, Some(40_000.0));
    req.display_quantity = Some(1_000.0);
    assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));

    let events = rest.modify_order(&req, None, Some(40_100.0));

    assert_matches!(events.as_slice(), [Event::OrderModified(_)], "{events:?}");
    let edit = sent(&fake, "private/edit");
    assert_eq!(edit["price"], 40_100.0);
    assert_eq!(edit["amount"], 2_000.0);
    assert_eq!(edit["display_amount"], 1_000.0);
}

/// What the bridge refuses before any frame is written: a terminal reject naming the cause, and NOTHING on
/// the socket. A combo is refused before `private/create_combo` registers anything.
#[test]
fn a_display_quantity_the_venue_cannot_take_is_refused_by_name_with_nothing_on_the_wire() {
    let (perp, option, option_grid) = grids();
    assert_eq!(option_grid.asset_class, Some(AssetClass::Option), "the fixture is an option");
    let shown = |mut r: OrderRequest, d: f64| {
        r.display_quantity = Some(d);
        r
    };
    let mut on_option = shown(buy(OrderType::Limit, 1.0, Some(0.05)), 0.5);
    on_option.symbol = option.clone();
    let mut on_combo = shown(buy(OrderType::Limit, 20.0, Some(40_000.0)), 10.0);
    on_combo.combo_legs =
        vec![vike_model::OrderComboLeg { symbol: SYMBOL.into(), ratio: 1, limit: None }];
    let on_market = shown(buy(OrderType::Market, 20.0, None), 10.0);
    let zero = shown(buy(OrderType::Limit, 20.0, Some(40_000.0)), 0.0);

    for (req, symbol, grid, why) in [
        (on_option, option, option_grid, "no iceberg (`display_amount`) on an option"),
        (on_combo, SYMBOL.to_string(), perp, "shows no part of a combo order"),
        (on_market, SYMBOL.to_string(), perp, "a market order with a display quantity"),
        (zero, SYMBOL.to_string(), perp, "needs a positive display quantity"),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::OkForever]);
        let rest = rest_on(&fake, &symbol, grid);
        match outcome(rest.submit_order(&req)) {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, ORDER_CLIENT_ID);
                assert!(r.reason.contains(why), "wanted `{why}`, got `{}`", r.reason);
            }
            other => panic!("`{why}` must be a terminal reject, got {other:?}"),
        }
        assert!(fake.methods().is_empty(), "{why}: nothing may reach the socket");
    }
}

/// The restart reconcile reads a `market_limit` row (the venue's word for the kind it was placed as) as a
/// live `MarketToLimit` carrying our order client id. The row is the DOCUMENTED order object, not a capture.
#[test]
fn a_market_limit_row_reads_as_a_market_to_limit() {
    let rows = json!([{
        "amount": 20.0, "direction": "buy", "filled_amount": 10.0, "average_price": 60_000.0,
        "instrument_name": SYMBOL, "label": ORDER_CLIENT_ID, "order_id": "31", "order_state": "open",
        "order_type": "market_limit", "price": 60_000.0, "creation_timestamp": 1_700_000_000_000i64,
    }]);
    let reports = parse_open_orders(&rows.to_string()).expect("a valid body");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].order_type, OrderType::MarketToLimit);
    assert_eq!(reports[0].order_client_id.as_deref(), Some(ORDER_CLIENT_ID));
    assert_eq!((reports[0].qty, reports[0].filled_qty), (20.0, 10.0));
    // a market-to-limit takes no limit price (`order_terms_refusal`), so the row's `price` is not stated
    assert_eq!(reports[0].price, None);
}
