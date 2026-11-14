//! **Two accounts on one key: each private-socket pump hands over the rows of its own book only.**
//!
//! Bybit's private socket carries every order and execution of the KEY, in every book. the CI box runs two
//! accounts on one demo key: `DEMO` (row 4, linear, with admitted spot pairs) and `INV` (row 19, the
//! inverse `BTCUSD`). Measured there before the fix: `DEMO`'s fills `vtfpB82250000` (2026-11-08) and
//! `ac8916990` (2026-11-09) reached the core through `INV`'s ring as well, the second one 13 ms AHEAD of
//! `DEMO`'s own copy, and `DEMO`'s stop `pd4bystp1` (2026-11-10) got a second terminal from `INV`'s pump.
//!
//! The decoder each pump runs (`perp_frame_decoder`) now keeps the rows whose `category` is its mount's
//! book: `inverse` for an inverse mount, every other book for a linear one. A row with no `category` is
//! kept, and an account alone on its key hands over every row of its books exactly as the raw mapper
//! maps it. The frames are scripted, as the pump would hand them over; nothing here opens a socket.

use serde_json::{Value, json};
use vike_bybit::execution::event_mapper::map_bybit_perp;
use vike_bybit::execution::user_data::perp_frame_decoder;
use vike_model::events::Event;

/// the CI box's two mounts on the one key.
const DEMO_MOUNT: &str = "ETHUSDT";
const INVERSE_MOUNT: &str = "BTCUSD";

/// An `order`-topic row of `category`.
fn order_row(category: &str, symbol: &str, order_client_id: &str, status: &str) -> Value {
    json!({
        "category": category, "symbol": symbol, "orderId": format!("v-{order_client_id}"),
        "orderLinkId": order_client_id, "orderStatus": status, "cancelType": "CancelByUser",
        "updatedTime": "5"
    })
}

/// A whole-fill `execution`-topic `Trade` row of `category`.
fn execution_row(category: &str, symbol: &str, order_client_id: &str, exec_id: &str) -> Value {
    json!({
        "category": category, "symbol": symbol, "orderId": format!("v-{order_client_id}"),
        "orderLinkId": order_client_id, "execType": "Trade", "execId": exec_id, "side": "Sell",
        "execPrice": "2119.06", "execQty": "0.01", "execFee": "0.01165483",
        "feeCurrency": "USDT", "isMaker": false, "cumExecQty": "0.01", "orderQty": "0.01",
        "leavesQty": "0", "markPrice": "2118.96", "positionIdx": 0, "execTime": "7"
    })
}

/// An `execution.fast` row of `category` (the slim shape: no fee, no quantities but its own).
fn fast_row(category: &str, symbol: &str, order_client_id: &str, exec_id: &str) -> Value {
    json!({
        "category": category, "symbol": symbol, "orderId": format!("v-{order_client_id}"),
        "orderLinkId": order_client_id, "execId": exec_id, "side": "Sell", "execPrice": "2119.06",
        "execQty": "0.01", "isMaker": false, "execTime": "6"
    })
}

fn frame(topic: &str, rows: Vec<Value>) -> Value {
    json!({"topic": topic, "creationTime": 1, "data": rows})
}

/// The life of one `DEMO` linear order as the socket shows it: accepted, filled (the fast hint and the
/// full row), and a stop of the same account cancelled.
fn demo_frames() -> Vec<Value> {
    vec![
        frame("order", vec![order_row("linear", "ETHUSDT", "ac8916990", "New")]),
        frame("execution.fast", vec![fast_row("linear", "ETHUSDT", "ac8916990", "E-demo")]),
        frame("execution", vec![execution_row("linear", "ETHUSDT", "ac8916990", "E-demo")]),
        frame("order", vec![order_row("linear", "ETHUSDT", "pd4bystp1", "Deactivated")]),
        // an admitted spot pair of the same account, and its fill
        frame("order", vec![order_row("spot", "DOGEUSDT", "vtdoge1", "New")]),
        frame("execution", vec![execution_row("spot", "DOGEUSDT", "vtdoge1", "E-doge")]),
    ]
}

/// The life of one `INV` inverse order as the socket shows it.
fn inverse_frames() -> Vec<Value> {
    vec![
        frame("order", vec![order_row("inverse", "BTCUSD", "pd4byinv2", "New")]),
        frame("execution.fast", vec![fast_row("inverse", "BTCUSD", "pd4byinv2", "E-inv")]),
        frame("execution", vec![execution_row("inverse", "BTCUSD", "pd4byinv2", "E-inv")]),
        frame("order", vec![order_row("inverse", "BTCUSD", "pd4byinv2", "Cancelled")]),
    ]
}

/// Everything one pump of `mount` hands the core for `frames`, run through ONE decoder in order.
fn handed_over(mount: &str, frames: &[Value]) -> Vec<Event> {
    let decode = perp_frame_decoder(mount);
    frames.iter().flat_map(&decode).collect()
}

/// What the raw mapper makes of `frames` for `mount`: what a pump handed over before the fix.
fn mapped(mount: &str, frames: &[Value]) -> Vec<Event> {
    frames.iter().flat_map(|f| map_bybit_perp(f, "bybit", mount)).collect()
}

/// The measured leak: `DEMO`'s order, fill and cancel rows reach `INV`'s pump, and it hands over nothing.
#[test]
fn the_inverse_accounts_pump_hands_over_none_of_the_linear_accounts_rows() {
    vike_bybit::history::kept_list_for_tests();
    let leaked = handed_over(INVERSE_MOUNT, &demo_frames());
    assert!(leaked.is_empty(), "INV's pump handed over DEMO's rows: {leaked:?}");
}

/// The other way round: `INV`'s inverse order reaches `DEMO`'s pump, which hands over nothing.
#[test]
fn the_linear_accounts_pump_hands_over_none_of_the_inverse_accounts_rows() {
    let leaked = handed_over(DEMO_MOUNT, &inverse_frames());
    assert!(leaked.is_empty(), "DEMO's pump handed over INV's rows: {leaked:?}");
}

/// One frame carrying a row of each book: each pump keeps its own row, as the mapper maps it alone.
#[test]
fn a_frame_of_both_books_hands_each_pump_its_own_row() {
    vike_bybit::history::kept_list_for_tests();
    let demo = execution_row("linear", "ETHUSDT", "ac8916990", "E-demo");
    let inverse = execution_row("inverse", "BTCUSD", "pd4byinv2", "E-inv");
    let both = [frame("execution", vec![demo.clone(), inverse.clone()])];
    assert_eq!(
        format!("{:?}", handed_over(DEMO_MOUNT, &both)),
        format!("{:?}", mapped(DEMO_MOUNT, &[frame("execution", vec![demo])])),
    );
    assert_eq!(
        format!("{:?}", handed_over(INVERSE_MOUNT, &both)),
        format!("{:?}", mapped(INVERSE_MOUNT, &[frame("execution", vec![inverse])])),
    );
}

/// An account alone on its key hands over every row of its own books exactly as before: the linear one its
/// linear and spot rows, the inverse one its inverse rows, and either a row that names no `category`.
#[test]
fn an_account_alone_on_its_key_hands_over_its_books_as_before() {
    vike_bybit::history::kept_list_for_tests();
    for (mount, frames) in [(DEMO_MOUNT, demo_frames()), (INVERSE_MOUNT, inverse_frames())] {
        let got = handed_over(mount, &frames);
        assert!(!got.is_empty(), "{mount}");
        assert_eq!(format!("{got:?}"), format!("{:?}", mapped(mount, &frames)), "{mount}");
    }
    let mut no_category = execution_row("linear", "ETHUSDT", "c9", "E9");
    no_category.as_object_mut().expect("a row").remove("category");
    let frames = [frame("execution", vec![no_category])];
    for mount in [DEMO_MOUNT, INVERSE_MOUNT] {
        let got = handed_over(mount, &frames);
        assert_eq!(got.len(), 2, "{mount}: the Fill and its wrap");
        assert_eq!(format!("{got:?}"), format!("{:?}", mapped(mount, &frames)), "{mount}");
    }
}
