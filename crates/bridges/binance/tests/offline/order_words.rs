//! **One status word, one reading, in every reader of the family.** The word-by-reader table, read
//! through each reader's own public door, and what a real engine folds when a fill arrives after a
//! `PENDING_CANCEL` row: that word is not an end, so the fill's wrap still lands on the order. Aster reads the
//! same functions; `crates/bridges/aster/src/execution/resync_tests.rs` proves the replay half over its wrapper.

use serde_json::{Value, json};
use vike_binance::execution::event_mapper::spot::map_execution_report;
use vike_binance::execution::rest::spot::VenueRestBinanceSpot;
use vike_binance::family::execution::confirm::found_events;
use vike_binance::family::execution::event_mapper::perp::map_perp_stream;
use vike_binance::family::execution::rest_reads::FoundOrder;
use vike_binance::family::execution::resync::{map_history, map_perp_history};
use vike_binance::family::recon::{parse_perp_open_orders, parse_spot_open_orders};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
use vike_model::events::Event;
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderType, RiskLimits, SymbolProperties};

use crate::time_in_force_gate::NullSigner;

/// `word`, what `recon` reports, what `found_events` emits, what both history mappers emit (the row with
/// no trades), and what a `-2022` cancel-replace settles to when the venue's by-order-client-id read says `word`.
struct Cell {
    word: &'static str,
    reconcile: OrderStatus,
    confirm: &'static [&'static str],
    history: &'static [&'static str],
    settle: &'static str,
}

const fn cell(
    word: &'static str,
    recon: OrderStatus,
    confirm: &'static [&'static str],
    history: &'static [&'static str],
    settle: &'static str,
) -> Cell {
    Cell { word, reconcile: recon, confirm, history, settle }
}

const LIVE: &[&str] = &["accepted"];
const NOTHING: &[&str] = &[];
const REFUSED: &str = "modify-rejected";

const TABLE: &[Cell] = &[
    cell("NEW", OrderStatus::Accepted, LIVE, NOTHING, "modified"),
    cell("PENDING_NEW", OrderStatus::Accepted, LIVE, NOTHING, REFUSED),
    cell("NEW_INSURANCE", OrderStatus::Accepted, LIVE, NOTHING, REFUSED),
    cell("NEW_ADL", OrderStatus::Accepted, LIVE, NOTHING, REFUSED),
    cell("PARTIALLY_FILLED", OrderStatus::PartiallyFilled, LIVE, NOTHING, "modified"),
    cell("FILLED", OrderStatus::Filled, LIVE, NOTHING, REFUSED),
    cell("CANCELED", OrderStatus::Canceled, &["accepted", "canceled"], &["canceled"], "canceled"),
    // Not an end, as recon reads it: the history arm used to end the order here.
    cell("PENDING_CANCEL", OrderStatus::PendingCancel, LIVE, NOTHING, REFUSED),
    cell("REJECTED", OrderStatus::Rejected, &["rejected"], &["rejected"], "canceled"),
    cell("EXPIRED", OrderStatus::Expired, &["accepted", "expired"], &["expired"], "canceled"),
    // An end, as recon reads it: the history mappers used to have no arm for it.
    cell(
        "EXPIRED_IN_MATCH",
        OrderStatus::Expired,
        &["accepted", "expired"],
        &["expired"],
        "canceled",
    ),
    // A word newer than the table, and a row with none: live everywhere.
    cell("SOME_NEWER_WORD", OrderStatus::Accepted, LIVE, NOTHING, REFUSED),
    cell("", OrderStatus::Accepted, LIVE, NOTHING, REFUSED),
];

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderAccepted(_) => "accepted",
            Event::OrderCanceled(_) => "canceled",
            Event::OrderExpired(_) => "expired",
            Event::OrderRejected(_) => "rejected",
            Event::OrderModified(_) => "modified",
            Event::OrderModifyRejected(_) => REFUSED,
            other => panic!("{other:?}"),
        })
        .collect()
}

/// A cancel-replace the venue refuses `-2022`, then a by-order-client-id read answering `status`.
struct ReplaceRefused {
    status: Value,
}
impl RestTransport for ReplaceRefused {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        match method {
            "POST" => Err(VenueApiError { code: -2022, msg: "replace failed".into() }),
            _ => Ok(self.status.clone()),
        }
    }
    vike_bridge_core::no_public_call!();
}

/// What a `-2022` settles to on `word`, at the very terms the order was asked to take.
fn settled(word: &str) -> Vec<&'static str> {
    let client = VenueRestBinanceSpot {
        signer: NullSigner,
        transport: ReplaceRefused {
            status: json!({"orderId": 9, "status": word, "price": "65700", "origQty": "0.0001"}),
        },
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.0001, ..Default::default() },
        base_asset: "BTC".to_string(),
        link_id: None,
    };
    let order = OrderRequest {
        order_client_id: "c".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.0001,
        order_type: OrderType::Limit,
        price: Some(65_700.0),
        ..Default::default()
    };
    kinds(&client.modify_order(&order, None, None))
}

/// Every reader against the table. A disagreement names its cell and both readings.
#[test]
fn every_reader_reads_every_word_the_one_way() {
    let mut differ = Vec::new();
    for c in TABLE {
        let word = c.word;
        let row = json!([{
            "symbol": "BTCUSDT", "orderId": 1, "clientOrderId": "c", "status": word, "type": "LIMIT",
            "side": "BUY", "origQty": "1", "executedQty": "0", "updateTime": 5
        }]);
        let body = row.to_string();
        let spot_report = parse_spot_open_orders(&body, "binance", None).unwrap()[0].status;
        let perp_report = parse_perp_open_orders(&body, "binance", None).unwrap()[0].status;
        for (reader, got) in [("recon spot", spot_report), ("recon perp", perp_report)] {
            if got != c.reconcile {
                differ.push(format!("{word:?} {reader}: {got:?}, the table {:?}", c.reconcile));
            }
        }
        let found = FoundOrder { order_id: "1".into(), status: word.into() };
        let spot_history = map_history(&row, &json!([]), "binance", "BTCUSDT", None);
        let perp_history = map_perp_history(&row, &json!([]), "binance", "BTCUSDT.P", None);
        let settle_want: &[&str] = &[c.settle];
        for (reader, got, want) in [
            ("confirm", kinds(&found_events("c", 5, &found)), c.confirm),
            ("history spot", kinds(&spot_history), c.history),
            ("history perp", kinds(&perp_history), c.history),
            ("settle_replace", settled(word), settle_want),
        ] {
            if got != want {
                differ.push(format!("{word:?} {reader}: {got:?}, the table {want:?}"));
            }
        }
    }
    assert!(differ.is_empty(), "{} cells differ:\n{}", differ.len(), differ.join("\n"));
}

const SPOT: &str = "BTCUSDT";
const PERP: &str = "BTCUSDT.P";

/// A real engine holding `order_client_id` for 0.002 on `symbol`, resting at the venue.
fn engine_holding(symbol: &str, order_client_id: &str) -> Engine<RecordingClient> {
    let mut engine = Engine::new(
        Account::new(1_000_000.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        symbol,
    );
    let mut order = OrderRecord::new(OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 0.002,
        order_type: OrderType::Limit,
        price: Some(60_000.0),
        ..Default::default()
    });
    order.status = OrderStatus::Accepted;
    engine.registry.insert(order_client_id.to_string(), order);
    engine
}

fn fold(engine: &mut Engine<RecordingClient>, events: &[Event]) {
    let mut outbox = EventBusOutbox::default();
    for event in events {
        engine.on_event(event, &mut outbox);
    }
}

/// The `allOrders` row of order 7, `word`, its order client id `order_client_id`.
fn row(order_client_id: &str, word: &str) -> Value {
    json!([{"orderId": 7, "clientOrderId": order_client_id, "status": word, "updateTime": 5}])
}

/// The `myTrades`/`userTrades` row of trade `id` of order 7 (both grammars: the union reads either).
fn trade(id: u64, qty: &str) -> Value {
    json!({
        "id": id, "orderId": 7, "price": "60000", "qty": qty, "commission": "0",
        "commissionAsset": "USDT", "time": 6, "side": "BUY", "maker": false, "positionSide": "BOTH"
    })
}

/// What the order and the book say: status, the order's filled quantity, the position on `symbol`, and the
/// count of fills the fold refused on an order it believed ended.
fn state(
    engine: &Engine<RecordingClient>,
    order_client_id: &str,
    symbol: &str,
) -> (OrderStatus, f64, f64, u64) {
    let position: f64 = engine
        .local_view()
        .positions
        .iter()
        .filter(|((s, _), _)| s == symbol)
        .map(|(_, q)| *q)
        .sum();
    let order = &engine.registry[order_client_id];
    (order.status, order.filled_qty, position, engine.events_dropped_order_already_closed)
}

/// **The late fill the row is about, on the spot stream.** A replay reads `PENDING_CANCEL`, the cancel loses, the fill
/// comes on the stream: the order ends FILLED with its quantity, beside the position it booked.
#[test]
fn a_late_stream_fill_after_a_pending_cancel_row_fills_the_order_on_spot() {
    let mut engine = engine_holding(SPOT, "c1");
    fold(
        &mut engine,
        &map_history(&row("c1", "PENDING_CANCEL"), &json!([]), "binance", SPOT, None),
    );
    let frame = json!({
        "e": "executionReport", "s": SPOT, "x": "TRADE", "X": "FILLED", "S": "BUY", "c": "c1",
        "i": 7, "T": 6, "l": "0.002", "L": "60000", "n": "0", "N": "USDT", "t": 100, "m": false
    });
    fold(&mut engine, &map_execution_report(&frame, "binance", SPOT, None));
    assert_eq!(state(&engine, "c1", SPOT), (OrderStatus::Filled, 0.002, 0.002, 0));
}

/// The perp lane's twin, through what its pump really calls.
#[test]
fn a_late_stream_fill_after_a_pending_cancel_row_fills_the_order_on_the_perp() {
    let mut engine = engine_holding(PERP, "c1");
    fold(
        &mut engine,
        &map_perp_history(&row("c1", "PENDING_CANCEL"), &json!([]), "binance", PERP, None),
    );
    let frame = json!({
        "e": "ORDER_TRADE_UPDATE", "T": 6,
        "o": {"s": SPOT, "c": "c1", "x": "TRADE", "X": "FILLED", "S": "BUY", "i": 7, "l": "0.002",
              "L": "60000", "n": "0", "N": "USDT", "t": 100, "m": false, "ps": "BOTH"}
    });
    fold(&mut engine, &map_perp_stream(&frame, "binance", SPOT, None));
    assert_eq!(state(&engine, "c1", PERP), (OrderStatus::Filled, 0.002, 0.002, 0));
}

/// The fill comes on the next replay instead, and the first replay already carried a partial one: it folds
/// once, the second completes the order, and the repeat of the first folds nothing.
#[test]
fn a_replay_of_the_fills_after_a_pending_cancel_row_completes_the_order() {
    for (symbol, map) in [
        (SPOT, map_history as fn(&Value, &Value, &str, &str, Option<&str>) -> Vec<Event>),
        (PERP, map_perp_history as fn(&Value, &Value, &str, &str, Option<&str>) -> Vec<Event>),
    ] {
        let mut engine = engine_holding(symbol, "c1");
        let first = json!([trade(100, "0.001")]);
        fold(&mut engine, &map(&row("c1", "PENDING_CANCEL"), &first, "binance", symbol, None));
        let both = json!([trade(100, "0.001"), trade(101, "0.001")]);
        fold(&mut engine, &map(&row("c1", "FILLED"), &both, "binance", symbol, None));
        assert_eq!(
            state(&engine, "c1", symbol),
            (OrderStatus::Filled, 0.002, 0.002, 0),
            "{symbol}"
        );
    }
}

/// An order the reconcile seeded AT `PENDING_CANCEL`, through the production parser and `reregister_orders`: the
/// replay's row says nothing to it, and the fill lands through the fold's cancel-race guard.
#[test]
fn a_late_fill_lands_on_an_order_seeded_at_pending_cancel() {
    let mut engine = engine_holding(SPOT, "c0");
    let seeded = json!([{
        "symbol": SPOT, "orderId": 8, "clientOrderId": "c2", "status": "PENDING_CANCEL", "type": "LIMIT",
        "side": "BUY", "origQty": "0.002", "executedQty": "0", "updateTime": 4
    }]);
    let reports = parse_spot_open_orders(&seeded.to_string(), "binance", None).unwrap();
    assert_eq!(engine.reregister_orders(&reports), 1);
    assert_eq!(engine.registry["c2"].status, OrderStatus::PendingCancel);

    fold(
        &mut engine,
        &map_history(&row("c2", "PENDING_CANCEL"), &json!([]), "binance", SPOT, None),
    );
    assert_eq!(engine.registry["c2"].status, OrderStatus::PendingCancel, "the row ended nothing");

    let frame = json!({
        "e": "executionReport", "s": SPOT, "x": "TRADE", "X": "FILLED", "S": "BUY", "c": "c2",
        "i": 8, "T": 6, "l": "0.002", "L": "60000", "n": "0", "N": "USDT", "t": 100, "m": false
    });
    fold(&mut engine, &map_execution_report(&frame, "binance", SPOT, None));
    assert_eq!(state(&engine, "c2", SPOT), (OrderStatus::Filled, 0.002, 0.002, 0));
}

/// The cancel wins after all: the row after `PENDING_CANCEL` ends the order, so the word strands nothing, and a
/// cancel's end that a self-trade prevention wrote instead (`EXPIRED_IN_MATCH`) ends one too.
#[test]
fn the_row_after_pending_cancel_ends_the_order() {
    for (last, ended) in
        [("CANCELED", OrderStatus::Canceled), ("EXPIRED_IN_MATCH", OrderStatus::Expired)]
    {
        let mut engine = engine_holding(SPOT, "c1");
        fold(
            &mut engine,
            &map_history(&row("c1", "PENDING_CANCEL"), &json!([]), "binance", SPOT, None),
        );
        fold(&mut engine, &map_history(&row("c1", last), &json!([]), "binance", SPOT, None));
        assert_eq!(state(&engine, "c1", SPOT), (ended, 0.0, 0.0, 0), "{last}");
    }
}
