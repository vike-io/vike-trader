//! The PERP fill label, folded through a REAL `Engine` (and the `vike_execution::Account`
//! behind it): every execution the binance-family perp lane reports must move the POSITION of an
//! engine started the way production starts a perp, and the history replay of a trade the stream
//! already delivered must not move it a second time.
//!
//! ## The label contract
//!
//! Production starts a binance-family perp on its CATALOG spelling, `BTCUSDT.P`
//! (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` aster row; the Trade window sends the same
//! spelling), and an engine folds a fill only when the fill's `symbol` is that exact string:
//! `vike_execution::Engine::accepts_symbol` is string equality, and the "own order on any
//! symbol" route (`owns_fill_symbol`) needs the fill to name its ORDER's `.P` symbol too. A fill,
//! partial or liquidation labelled with the frame's bare `o.s` (`BTCUSDT`) advances the ORDER to
//! FILLED (its lifecycle wrap routes by order client id) while the POSITION, realized PnL and
//! commission never move; a bare-labelled reconcile report row likewise drops a synthesized
//! `MissingFill` and raises `PositionOnlyExternal` + `OrphanLocalPosition` on every pass.
//!
//! So the shared family code labels every perp-lane symbol `<exchange symbol>` +
//! `vike_venues::PERP_SUFFIX` — ONE place for both venues — and labels by the FRAME's symbol, never
//! by the started one: the user-data stream is account-wide, so a foreign `ETHUSDT` fill must become
//! `ETHUSDT.P`, never `BTCUSDT.P`.
//!
//! ## The replay risk, and the two tests that hold it
//!
//! A fill that FOLDS from the stream is delivered AGAIN by the history replay (the `run_loop` gap
//! sentinel five seconds after the last command, and the resync supervisor on every reconnect). The
//! fill-side guard is `Account::apply_fill`'s ledger, keyed on the bare `trade_id`, and it holds only
//! if the stream's `o.t` and the replay's `userTrades[].id` render to the SAME string for the same
//! trade — `the_history_replay_of_a_streamed_fill_does_not_double_the_position` asserts exactly that,
//! then the position. A LIQUIDATION is guarded by a DIFFERENT set (the engine's `seen_liquidation_ids`),
//! which the account ledger never sees, so the replay must re-emit a liquidation trade the way the
//! stream does — as a `PositionLiquidated`, never as a fill —
//! `the_history_replay_of_a_streamed_liquidation_does_not_close_twice` holds that.
//!
//! ## Controls
//!
//! bybit linear and okx SWAP label with the venue's own symbol, which IS what their engines are
//! started on (`BTCUSDT`, `BTC-USDT-SWAP`); they must keep folding. The binance SPOT lane keeps its
//! bare label, because a spot engine on the bare `BTCUSDT`.
//!
//! Frames are the venues' own where one is committed: the binance-family `ORDER_TRADE_UPDATE`
//! shapes come from the frozen `fixtures/r6/perp.json` (Binance's wire — aster's perp stream is
//! Binance-verbatim, which is why the two venues share one mapper), bybit's and binance spot's
//! from their sanitized captures under `crates/bridges/<venue>/tests/fixtures/captured/`. Every
//! mapper is called with the arguments its production pump passes it.

use serde_json::{Value, json};
use std::assert_matches;

use vike_bridge_core::execution_drive::bare_fills;
use vike_execution::recon::{ReconcileDivergence, ReconcilePolicy, diff, resolve};
use vike_execution::testing::{RecordingClient, fold};
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
use vike_model::OrderStatus;
use vike_model::RiskLimits;
use vike_model::events::{Event, OrderSubmitted, PositionLiquidated};
use vike_model::{FillReport, OrderRequest, PositionStatusReport};

/// The catalog spelling a binance-family perp is started on.
const ENGINE_SYMBOL: &str = "BTCUSDT.P";
/// The same instrument as the venue names it on the wire.
const WIRE: &str = "BTCUSDT";
/// Float equality for quantities built from short decimal strings.
const EPS: f64 = 1e-12;

// ===================================================================================================
// The two binance-family perp lanes: each venue's REAL perp mapper, replay mapper and reconcile
// parsers, reached through the venue's own public names.
// ===================================================================================================

#[derive(Clone, Copy)]
struct FamilyLane {
    venue: &'static str,
    /// The perp user-data mapper, called as the production pump calls it: `(frame, venue, the
    /// engine series symbol as the absent-`s` fallback)`.
    decode: fn(&Value, &str, &str) -> Vec<Event>,
    /// The perp history replay (`allOrders` + `userTrades`), called as `perp_history_events` calls
    /// it: `(orders, trades, venue, the engine series symbol)`.
    replay: fn(&Value, &Value, &str, &str) -> Vec<Event>,
    /// The perp `userTrades` reconcile parser (the venue's 1-arg wrapper).
    parse_fills: fn(&str) -> Result<Vec<FillReport>, String>,
    /// The perp `positionRisk` reconcile parser (the venue's 1-arg wrapper).
    parse_positions: fn(&str) -> Result<Vec<PositionStatusReport>, String>,
}

const ASTER: FamilyLane = FamilyLane {
    venue: "aster",
    decode: vike_aster::execution::event_mapper::perp::map_aster_perp,
    replay: vike_aster::execution::resync::map_aster_perp_history,
    parse_fills: vike_aster::reconcile::parse_perp_user_trades,
    parse_positions: vike_aster::reconcile::parse_perp_position_risk,
};

/// Binance's perp mapper for an account with no broker link id (these frames carry bare ids).
fn binance_decode(frame: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    vike_binance::execution::event_mapper::perp::map_binance_perp(frame, venue, symbol, None)
}

/// Binance's perp replay for an account with no broker link id (these rows carry bare ids).
fn binance_replay(orders: &Value, trades: &Value, venue: &str, symbol: &str) -> Vec<Event> {
    vike_binance::execution::resync::map_binance_perp_history(orders, trades, venue, symbol, None)
}

const BINANCE: FamilyLane = FamilyLane {
    venue: "binance",
    decode: binance_decode,
    replay: binance_replay,
    parse_fills: vike_binance::reconcile::parse_perp_user_trades,
    parse_positions: vike_binance::reconcile::parse_perp_position_risk,
};

/// Every family case below is a plain `fn(FamilyLane)`; this instantiates each one ONCE PER VENUE
/// (`aster::<case>`, `binance::<case>`), so a failure names its venue in the test name and one
/// venue's red can never hide the other's — the two venues share the code under test, which is
/// exactly why both are run.
macro_rules! per_venue {
    ($($case:ident),* $(,)?) => {
        mod aster {
            $( #[test] fn $case() { super::$case(super::ASTER) } )*
        }
        mod binance {
            $( #[test] fn $case() { super::$case(super::BINANCE) } )*
        }
    };
}

per_venue!(
    a_streamed_perp_fill_moves_the_position_of_the_engine_on_the_catalog_spelling,
    a_partial_perp_fill_moves_the_position_before_the_order_completes,
    a_perp_fill_with_no_local_order_on_the_engine_symbol_folds,
    a_perp_fill_for_another_symbol_keeps_its_own_label,
    a_venue_liquidation_closes_the_perp_position,
    a_reconcile_missing_fill_folds_into_the_perp_position,
    a_reconcile_position_report_names_the_local_perp_position,
    a_reconcile_pass_after_a_streamed_fill_raises_nothing,
    the_history_replay_of_a_streamed_fill_does_not_double_the_position,
    the_history_replay_of_a_streamed_liquidation_does_not_close_twice,
);

// ===================================================================================================
// Engine scaffolding.
// ===================================================================================================

/// A REAL engine over a REAL `Account`, started on `(venue, started)` — the `vike_tradehub_core` shape:
/// no `extra_symbols`.
fn engine(venue: &str, engine_symbol: &str) -> Engine<RecordingClient> {
    Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        engine_symbol,
    )
}

/// Submit a limit order on `symbol` through the engine's own gate (so it is REGISTERED), then fold
/// the Rust-side `OrderSubmitted` half of the emitter split.
fn submit(
    engine: &mut Engine<RecordingClient>,
    venue: &str,
    symbol: &str,
    order_client_id: &str,
    side: i32,
    qty: f64,
) {
    let req = OrderRequest {
        order_client_id: order_client_id.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(60_000.0),
        timestamp: 1,
        ..Default::default()
    };
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&req, 1, &mut outbox);
    assert!(
        engine.registry.contains_key(order_client_id),
        "[{venue}] the RiskGate refused the test order {order_client_id}"
    );
    engine.on_event(
        &Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.into(),
            timestamp: 1,
        }),
        &mut outbox,
    );
}

/// The engine's net position on `symbol`, summed over every position side (hedge buckets
/// included), read through the same `local_view` the reconcile driver reads.
fn position(engine: &Engine<RecordingClient>, symbol: &str) -> f64 {
    engine.local_view().positions.iter().filter(|((s, _), _)| s == symbol).map(|(_, q)| *q).sum()
}

fn status(engine: &Engine<RecordingClient>, order_client_id: &str) -> Option<OrderStatus> {
    engine.registry.get(order_client_id).map(|mo| mo.status)
}

fn liquidations(events: &[Event]) -> Vec<&PositionLiquidated> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::PositionLiquidated(l) => Some(l),
            _ => None,
        })
        .collect()
}

/// The labels a decoded batch carries, for failure messages.
fn labels(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(format!("Fill:{}", f.symbol)),
            Event::PositionLiquidated(l) => Some(format!("PositionLiquidated:{}", l.symbol)),
            _ => None,
        })
        .collect()
}

// ===================================================================================================
// Binance-family frames, from the frozen r6 fixture.
// ===================================================================================================

fn r6_mapper_frames() -> Vec<Value> {
    let root =
        vike_bridge_core::execution_drive::r6_fixture(env!("CARGO_MANIFEST_DIR"), "perp.json");
    root["mapper"].as_array().expect("r6 mapper cases").iter().map(|c| c["frame"].clone()).collect()
}

/// The first committed `ORDER_TRADE_UPDATE` frame whose order block satisfies `pred`.
fn r6_otu(pred: impl Fn(&Value) -> bool) -> Value {
    r6_mapper_frames()
        .into_iter()
        .find(|f| f["e"] == "ORDER_TRADE_UPDATE" && pred(&f["o"]))
        .expect("the r6 fixture carries the ORDER_TRADE_UPDATE shape this test needs")
}

fn is_liquidation_order_client_id(o: &Value) -> bool {
    o["c"].as_str().is_some_and(|c| c.contains("autoclose"))
}

/// `x=NEW` for `order_client_id` on `wire`.
fn otu_new(wire: &str, order_client_id: &str) -> Value {
    let mut f = r6_otu(|o| o["x"] == "NEW");
    f["o"]["s"] = json!(wire);
    f["o"]["c"] = json!(order_client_id);
    f
}

/// `x=TRADE` — one execution of `qty` at `px` for `order_client_id` on `wire`, order status `x_status` after
/// it, venue trade id `t`.
fn otu_trade(
    wire: &str,
    order_client_id: &str,
    x_status: &str,
    side: &str,
    qty: &str,
    px: &str,
    t: u64,
) -> Value {
    let mut f = r6_otu(|o| o["x"] == "TRADE" && !is_liquidation_order_client_id(o));
    let o = &mut f["o"];
    o["s"] = json!(wire);
    o["c"] = json!(order_client_id);
    o["X"] = json!(x_status);
    o["S"] = json!(side);
    o["l"] = json!(qty);
    o["L"] = json!(px);
    o["t"] = json!(t);
    o["ps"] = json!("BOTH");
    f
}

/// A venue LIQUIDATION execution (`autoclose-` order client id) of `qty` at `px`, trade id `t`.
fn otu_liquidation(wire: &str, side: &str, qty: &str, px: &str, t: u64) -> Value {
    let mut f = r6_otu(|o| o["x"] == "TRADE" && is_liquidation_order_client_id(o));
    let o = &mut f["o"];
    o["s"] = json!(wire);
    o["S"] = json!(side);
    o["l"] = json!(qty);
    o["L"] = json!(px);
    o["t"] = json!(t);
    o["ps"] = json!("BOTH");
    f
}

/// A fapi `allOrders` row (the replay's order list).
fn all_orders_row(order_id: u64, order_client_id: &str, status: &str) -> Value {
    json!({"orderId": order_id, "clientOrderId": order_client_id, "symbol": WIRE, "status": status,
           "origQty": "0.002", "executedQty": "0.002", "updateTime": 1_751_600_000_500_i64})
}

/// A fapi `userTrades` row — the shape both the replay and the reconcile parser read.
fn user_trade_row(id: u64, order_id: u64, side: &str, qty: &str, px: &str, comm: &str) -> Value {
    json!({"symbol": WIRE, "id": id, "orderId": order_id, "side": side, "price": px, "qty": qty,
           "commission": comm, "commissionAsset": "USDT", "maker": false, "buyer": side == "BUY",
           "positionSide": "BOTH", "time": 1_751_600_000_456_i64})
}

// ===================================================================================================
// (a)-(d): the streamed executions fold.
// ===================================================================================================

/// (a) An order-client-id-linked fill of the engine's own order moves the position of the engine on
/// the catalog spelling — and is LABELLED with that spelling.
fn a_streamed_perp_fill_moves_the_position_of_the_engine_on_the_catalog_spelling(lane: FamilyLane) {
    let v = lane.venue;
    let order_client_id = "perp-a";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.002);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, order_client_id), v, ENGINE_SYMBOL));
    let fill = (lane.decode)(
        &otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.002", "62584.5", 998_877),
        v,
        ENGINE_SYMBOL,
    );
    fold(&mut engine_under_test, &fill);

    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        (pos - 0.002).abs() < EPS,
        "[{v}] the fill was labelled {:?}; the engine mounted on {ENGINE_SYMBOL:?} left the order {:?} \
             and the position at {pos} — the fill was DROPPED from the position bookkeeping",
        labels(&fill),
        status(&engine_under_test, order_client_id)
    );
    assert_eq!(
        status(&engine_under_test, order_client_id),
        Some(OrderStatus::Filled),
        "[{v}] order status"
    );
    let bare = bare_fills(&fill);
    assert_eq!(bare.len(), 1, "[{v}] one bare fill");
    assert_eq!(
        bare[0].symbol.as_str(),
        ENGINE_SYMBOL,
        "[{v}] the fill's label is the catalog spelling"
    );
}

/// (b) A PARTIAL fill moves the position at once — the resting remainder must not hold the booking
/// back until the next command or reconnect happens to replay it.
fn a_partial_perp_fill_moves_the_position_before_the_order_completes(lane: FamilyLane) {
    let v = lane.venue;
    let order_client_id = "perp-b";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.002);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, order_client_id), v, ENGINE_SYMBOL));

    let partial = (lane.decode)(
        &otu_trade(WIRE, order_client_id, "PARTIALLY_FILLED", "BUY", "0.001", "62584.5", 1_001),
        v,
        ENGINE_SYMBOL,
    );
    fold(&mut engine_under_test, &partial);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        (pos - 0.001).abs() < EPS,
        "[{v}] partial labelled {:?}: order {:?} but position {pos} — DROPPED",
        labels(&partial),
        status(&engine_under_test, order_client_id)
    );
    assert_eq!(
        status(&engine_under_test, order_client_id),
        Some(OrderStatus::PartiallyFilled),
        "[{v}]"
    );

    fold(
        &mut engine_under_test,
        &(lane.decode)(
            &otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.001", "62590.1", 1_002),
            v,
            ENGINE_SYMBOL,
        ),
    );
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!((pos - 0.002).abs() < EPS, "[{v}] both executions booked, got {pos}");
    assert_eq!(status(&engine_under_test, order_client_id), Some(OrderStatus::Filled), "[{v}]");
}

/// (d) A fill the engine has NO order for — an order placed outside vike on the same account and
/// symbol, with an empty or a foreign order client id — still folds into the engine symbol's
/// position, exactly as it does on bybit and okx.
fn a_perp_fill_with_no_local_order_on_the_engine_symbol_folds(lane: FamilyLane) {
    let v = lane.venue;
    for (n, order_client_id) in ["", "web_1234567"].into_iter().enumerate() {
        let mut engine_under_test = engine(v, ENGINE_SYMBOL);
        let fill = (lane.decode)(
            &otu_trade(
                WIRE,
                order_client_id,
                "FILLED",
                "SELL",
                "0.003",
                "62000.0",
                2_000 + n as u64,
            ),
            v,
            ENGINE_SYMBOL,
        );
        fold(&mut engine_under_test, &fill);
        let pos = position(&engine_under_test, ENGINE_SYMBOL);
        assert!(
            (pos + 0.003).abs() < EPS,
            "[{v}] order client id {order_client_id:?}: fill labelled {:?}, position {pos} — DROPPED",
            labels(&fill)
        );
    }
}

/// (d) The stream is ACCOUNT-WIDE: a fill on ANOTHER symbol must keep ITS OWN perp label — never
/// the started one — so it lands on its own engine and never on this one.
fn a_perp_fill_for_another_symbol_keeps_its_own_label(lane: FamilyLane) {
    let v = lane.venue;
    let fill = (lane.decode)(
        &otu_trade("ETHUSDT", "", "FILLED", "BUY", "0.1", "2400.0", 3_000),
        v,
        ENGINE_SYMBOL,
    );
    // The money first: the engine on the fill's OWN perp books it…
    let mut eth = engine(v, "ETHUSDT.P");
    fold(&mut eth, &fill);
    let pos = position(&eth, "ETHUSDT.P");
    assert!(
        (pos - 0.1).abs() < EPS,
        "[{v}] the ETHUSDT.P engine left the position at {pos}: the fill was labelled {:?}",
        labels(&fill)
    );
    // …the BTC engine on the same account books nothing…
    let mut btc = engine(v, ENGINE_SYMBOL);
    fold(&mut btc, &fill);
    assert_eq!(
        position(&btc, ENGINE_SYMBOL),
        0.0,
        "[{v}] the BTC engine must not book an ETH fill"
    );
    assert_eq!(position(&btc, "ETHUSDT.P"), 0.0, "[{v}] nor hold it under any symbol");
    // …because the label came from the FRAME, never from the start.
    let bare = bare_fills(&fill);
    assert_eq!(bare.len(), 1, "[{v}] one bare fill");
    assert_eq!(bare[0].symbol.as_str(), "ETHUSDT.P", "[{v}] labelled from the frame's own `s`");
}

/// A venue LIQUIDATION closes the local perp position.
fn a_venue_liquidation_closes_the_perp_position(lane: FamilyLane) {
    let v = lane.venue;
    let order_client_id = "perp-liq";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.5);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, order_client_id), v, ENGINE_SYMBOL));
    fold(
        &mut engine_under_test,
        &(lane.decode)(
            &otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.5", "60000.0", 4_000),
            v,
            ENGINE_SYMBOL,
        ),
    );
    assert!(
        (position(&engine_under_test, ENGINE_SYMBOL) - 0.5).abs() < EPS,
        "[{v}] setup: long 0.5 is open"
    );

    let liquidation =
        (lane.decode)(&otu_liquidation(WIRE, "SELL", "0.5", "58000.0", 4_001), v, ENGINE_SYMBOL);
    assert!(
        liquidation.iter().any(|e| matches!(e, Event::PositionLiquidated(_))),
        "[{v}] an autoclose execution decodes to PositionLiquidated: {liquidation:?}"
    );
    fold(&mut engine_under_test, &liquidation);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        pos.abs() < EPS,
        "[{v}] liquidation labelled {:?} left the position at {pos} — DROPPED",
        labels(&liquidation)
    );
}

// ===================================================================================================
// (c): reconcile.
// ===================================================================================================

/// (c) A fill the stream never delivered is raised by reconcile as a `MissingFill`, and the event
/// `resolve` synthesizes for it must FOLD into the perp position.
fn a_reconcile_missing_fill_folds_into_the_perp_position(lane: FamilyLane) {
    let v = lane.venue;
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    let body = Value::Array(vec![user_trade_row(5_000, 7_000, "BUY", "0.002", "62584.5", "0.05")])
        .to_string();
    let fills = (lane.parse_fills)(&body).expect("userTrades parses");

    let local = engine_under_test.local_view();
    let divs = diff(&[], &fills, &[], &local.as_view(), None);
    assert_matches!(
        divs.as_slice(),
        [ReconcileDivergence::MissingFill(_)],
        "[{v}] one MissingFill: {divs:?}"
    );
    // `hybrid` is the preset under which a MissingFill AUTO-folds (`quarantine`, the shipped
    // default, holds it for an operator) — the synthesized event is the same either way.
    let recon = resolve(divs, &ReconcilePolicy::hybrid(), None, None);
    fold(&mut engine_under_test, &recon.events);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        (pos - 0.002).abs() < EPS,
        "[{v}] reconcile synthesized {:?} and the engine left the position at {pos} — DROPPED",
        labels(&recon.events)
    );
    assert_eq!(fills[0].symbol, ENGINE_SYMBOL, "[{v}] the fill report carries the perp label");
}

/// (c) The POSITION check: the venue's `positionRisk` row for an open perp position must name the
/// same key as the local book, so an agreed position raises NOTHING — not a
/// `PositionOnlyExternal` + `OrphanLocalPosition` pair on every pass.
fn a_reconcile_position_report_names_the_local_perp_position(lane: FamilyLane) {
    let v = lane.venue;
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    // Open the local position through the REPLAY lane, whose label has always been the started
    // series symbol — so this test isolates the REPORT side of the comparison.
    let replay = (lane.replay)(
        &json!([all_orders_row(7_100, "perp-pos", "FILLED")]),
        &json!([user_trade_row(5_100, 7_100, "BUY", "0.5", "60000.0", "0.1")]),
        v,
        ENGINE_SYMBOL,
    );
    fold(&mut engine_under_test, &replay);
    assert!(
        (position(&engine_under_test, ENGINE_SYMBOL) - 0.5).abs() < EPS,
        "[{v}] setup: local long 0.5"
    );

    let body = json!([{"symbol": WIRE, "positionAmt": "0.500", "entryPrice": "60000.0",
                           "markPrice": "60100.0", "positionSide": "BOTH",
                           "updateTime": 1_751_600_001_000_i64}])
    .to_string();
    let positions = (lane.parse_positions)(&body).expect("positionRisk parses");
    let local = engine_under_test.local_view();
    let divs = diff(&[], &[], &positions, &local.as_view(), None);
    assert!(
        divs.is_empty(),
        "[{v}] venue row {:?} vs local {ENGINE_SYMBOL:?}: an agreed position raised {divs:?}",
        positions.iter().map(|p| p.symbol.as_str()).collect::<Vec<_>>()
    );
}

/// (c) The reconcile pass AFTER a streamed fill raises nothing: the stream's `o.t` and the fill
/// report's `id` are the same trade id, so the fold the stream made is the one reconcile sees.
fn a_reconcile_pass_after_a_streamed_fill_raises_nothing(lane: FamilyLane) {
    let v = lane.venue;
    let order_client_id = "perp-rc";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.002);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, order_client_id), v, ENGINE_SYMBOL));
    fold(
        &mut engine_under_test,
        &(lane.decode)(
            &otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.002", "62584.5", 5_200),
            v,
            ENGINE_SYMBOL,
        ),
    );

    let fills = (lane.parse_fills)(
        &Value::Array(vec![user_trade_row(5_200, 7_200, "BUY", "0.002", "62584.5", "0.05")])
            .to_string(),
    )
    .unwrap();
    let positions = (lane.parse_positions)(
        &json!([{"symbol": WIRE, "positionAmt": "0.002", "entryPrice": "62584.5",
                     "positionSide": "BOTH", "updateTime": 1}])
        .to_string(),
    )
    .unwrap();
    let local = engine_under_test.local_view();
    let divs = diff(&[], &fills, &positions, &local.as_view(), None);
    assert!(divs.is_empty(), "[{v}] a reconciled stream fill re-raised {divs:?}");
}

// ===================================================================================================
// The replay risk: the history replay of a trade the stream already folded.
// ===================================================================================================

/// The gap sentinel / reconnect replay re-delivers a trade the stream ALREADY folded. The two
/// copies must share one `trade_id` string — the stream's `o.t` and the replay's `userTrades[].id`
/// — and the position must move ONCE.
fn the_history_replay_of_a_streamed_fill_does_not_double_the_position(lane: FamilyLane) {
    let v = lane.venue;
    let order_client_id = "perp-replay";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.002);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, order_client_id), v, ENGINE_SYMBOL));
    let stream = (lane.decode)(
        &otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.002", "62584.5", 998_877),
        v,
        ENGINE_SYMBOL,
    );
    fold(&mut engine_under_test, &stream);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        (pos - 0.002).abs() < EPS,
        "[{v}] the streamed fill labelled {:?} did not move the position ({pos})",
        labels(&stream)
    );
    let balance_after_stream = engine_under_test.account.balance;

    let replay = (lane.replay)(
        &json!([all_orders_row(555_666_777, order_client_id, "FILLED")]),
        &json!([user_trade_row(998_877, 555_666_777, "BUY", "0.002", "62584.5", "0.05")]),
        v,
        ENGINE_SYMBOL,
    );
    // The dedup's precondition, stated as data: one trade, one id string, one label.
    let (s, r) = (bare_fills(&stream), bare_fills(&replay));
    assert_eq!((s.len(), r.len()), (1, 1), "[{v}] one bare fill on each lane");
    assert_eq!(s[0].trade_id.as_str(), r[0].trade_id.as_str(), "[{v}] stream `t` vs replay `id`");
    assert_eq!(s[0].symbol, r[0].symbol, "[{v}] stream and replay label the trade alike");

    let refused_before = engine_under_test.account.duplicate_fills_refused;
    fold(&mut engine_under_test, &replay);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!((pos - 0.002).abs() < EPS, "[{v}] the replay DOUBLED the position: {pos}");
    assert_eq!(
        engine_under_test.account.duplicate_fills_refused - refused_before,
        1,
        "[{v}] the account refused the replayed copy"
    );
    assert_eq!(
        engine_under_test.account.colliding_fills_refused, 0,
        "[{v}] a re-delivery, not a collision"
    );
    assert_eq!(
        engine_under_test.account.balance.to_bits(),
        balance_after_stream.to_bits(),
        "[{v}] ONE commission"
    );
    let filled = engine_under_test.registry[order_client_id].filled_qty;
    assert!((filled - 0.002).abs() < EPS, "[{v}] the order's filled_qty doubled: {filled}");
}

/// A LIQUIDATION the stream already folded must not be closed a SECOND time by the replay: the
/// engine guards a liquidation with `seen_liquidation_ids`, which the account's fill ledger never sees, so
/// a replay that re-emitted the trade as a bare FILL would fold it as a fresh execution — closing an
/// already-closed position flips it to the opposite side.
///
/// ⚠ A position is OPENED AGAIN between the stream's liquidation and the replay, because a flat
/// book cannot tell the dedup from `vike_execution::Account::apply_liquidation`'s flat clamp: on a flat
/// book a replayed liquidation is a no-op whether or not `seen_liquidation_ids` recognised it as the
/// stream's, so a replay minting a DIFFERENT id would pass there. With a live position under it, a
/// replay the dedup does not catch closes that position, and only the shared id keeps it open.
fn the_history_replay_of_a_streamed_liquidation_does_not_close_twice(lane: FamilyLane) {
    let v = lane.venue;
    let order_client_id = "perp-open";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.5);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, order_client_id), v, ENGINE_SYMBOL));
    fold(
        &mut engine_under_test,
        &(lane.decode)(
            &otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.5", "60000.0", 6_000),
            v,
            ENGINE_SYMBOL,
        ),
    );
    // Without this the case is vacuous on a broken label: a dropped OPENING fill leaves the
    // book flat, and a flat book reads exactly like a correctly liquidated one.
    assert!(
        (position(&engine_under_test, ENGINE_SYMBOL) - 0.5).abs() < EPS,
        "[{v}] setup: long 0.5 is open"
    );
    let liquidation =
        (lane.decode)(&otu_liquidation(WIRE, "SELL", "0.5", "58000.0", 6_001), v, ENGINE_SYMBOL);
    fold(&mut engine_under_test, &liquidation);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        pos.abs() < EPS,
        "[{v}] the streamed liquidation labelled {:?} did not close the position ({pos})",
        labels(&liquidation)
    );

    // A NEW position, opened after the liquidation and before the replay arrives (see the doc).
    let reopen = "perp-reopen";
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, reopen, 1, 0.2);
    fold(&mut engine_under_test, &(lane.decode)(&otu_new(WIRE, reopen), v, ENGINE_SYMBOL));
    fold(
        &mut engine_under_test,
        &(lane.decode)(
            &otu_trade(WIRE, reopen, "FILLED", "BUY", "0.2", "58100.0", 6_002),
            v,
            ENGINE_SYMBOL,
        ),
    );
    assert!(
        (position(&engine_under_test, ENGINE_SYMBOL) - 0.2).abs() < EPS,
        "[{v}] setup: long 0.2 is reopened"
    );

    let replay = (lane.replay)(
        &json!([all_orders_row(8_000, "autoclose-12345", "FILLED")]),
        &json!([user_trade_row(6_001, 8_000, "SELL", "0.5", "58000.0", "2.5")]),
        v,
        ENGINE_SYMBOL,
    );
    fold(&mut engine_under_test, &replay);
    // The money first: the position opened after the liquidation survives its replay…
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        (pos - 0.2).abs() < EPS,
        "[{v}] the replayed liquidation moved the position opened after it from 0.2 to {pos}; it \
         emitted {:?}",
        labels(&replay)
    );
    // …because the dedup's precondition holds, stated as data like the fill case's: one
    // liquidation on each lane, one id string.
    let (s, r) = (liquidations(&liquidation), liquidations(&replay));
    assert_eq!((s.len(), r.len()), (1, 1), "[{v}] one liquidation on each lane");
    assert_eq!(
        s[0].trade_id.as_str(),
        r[0].trade_id.as_str(),
        "[{v}] stream `t` vs replay `id` of the liquidation trade"
    );
}

/// binance's opt-in TRADE_LITE early fill rides the same label: it folds into the perp position,
/// and its authoritative `ORDER_TRADE_UPDATE` twin (same `t`) does not book the trade again.
#[test]
fn the_binance_trade_lite_early_fill_folds_once_into_the_perp_position() {
    let v = "binance";
    let order_client_id = "perp-lite";
    let mut engine_under_test = engine(v, ENGINE_SYMBOL);
    submit(&mut engine_under_test, v, ENGINE_SYMBOL, order_client_id, 1, 0.002);
    let decode = |f: &Value| {
        vike_binance::execution::event_mapper::perp::map_binance_perp_opts(
            f,
            v,
            ENGINE_SYMBOL,
            true,
            None,
        )
    };
    fold(&mut engine_under_test, &decode(&otu_new(WIRE, order_client_id)));
    let lite = json!({"e": "TRADE_LITE", "E": 1, "T": 1, "s": WIRE, "q": "0.002", "p": "62584.5",
                      "m": false, "c": order_client_id, "S": "BUY", "L": "62584.5", "l": "0.002", "t": 9_100,
                      "i": 555_666_777_u64});
    let early = decode(&lite);
    fold(&mut engine_under_test, &early);
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!(
        (pos - 0.002).abs() < EPS,
        "the early fill labelled {:?} did not move the position ({pos})",
        labels(&early)
    );
    fold(
        &mut engine_under_test,
        &decode(&otu_trade(WIRE, order_client_id, "FILLED", "BUY", "0.002", "62584.5", 9_100)),
    );
    let pos = position(&engine_under_test, ENGINE_SYMBOL);
    assert!((pos - 0.002).abs() < EPS, "the authoritative twin booked the trade again: {pos}");
    assert_eq!(status(&engine_under_test, order_client_id), Some(OrderStatus::Filled));
}

// ===================================================================================================
// Controls: bybit linear, okx SWAP and the binance SPOT lane label with what their engines start.
// ===================================================================================================

fn captured(venue: &str, kind: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../bridges")
        .join(venue)
        .join("tests/fixtures/captured")
        .join(format!("{kind}.json"));
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing captured fixture {}: {e}", path.display()));
    let root: Value = serde_json::from_str(&body).expect("captured fixture parses");
    root["frames"][0].clone()
}

/// bybit's captured `execution` row, patched to one execution of `this` (cumulative `cum` of
/// `total`) for `order_client_id`.
fn bybit_fill(
    order_client_id: &str,
    exec_id: &str,
    this: &str,
    cum: &str,
    total: &str,
    leaves: &str,
) -> Value {
    let mut f = captured("bybit", "ws_fill");
    let row = &mut f["data"][0];
    row["orderLinkId"] = json!(order_client_id);
    row["execId"] = json!(exec_id);
    row["execQty"] = json!(this);
    row["cumExecQty"] = json!(cum);
    row["orderQty"] = json!(total);
    row["leavesQty"] = json!(leaves);
    f
}

fn okx_fill(order_client_id: &str, trade_id: &str, this: &str, cum: &str, state: &str) -> Value {
    json!({"arg": {"channel": "orders", "instType": "SWAP"}, "data": [
        {"instId": "BTC-USDT-SWAP", "clOrdId": order_client_id, "state": state, "tradeId": trade_id,
         "fillSz": this, "fillPx": "62584.5", "fillFee": "-0.01", "fillFeeCcy": "USDT",
         "side": "buy", "posSide": "net", "execType": "T", "accFillSz": cum, "sz": "0.002",
         "fillTime": "1"}]})
}

/// bybit linear: a partial then a completing fill of the engine's own order, then a fill with no
/// local order — all fold into the engine on bare `BTCUSDT`, the label the venue sends.
#[test]
fn control_bybit_linear_fills_fold_into_their_engine_symbol() {
    let (v, engine_symbol) = ("bybit", "BTCUSDT");
    let decode =
        |f: &Value| vike_bybit::execution::event_mapper::map_bybit_perp(f, v, engine_symbol);
    let order_client_id = "bybit-a";
    let mut engine_under_test = engine(v, engine_symbol);
    submit(&mut engine_under_test, v, engine_symbol, order_client_id, 1, 0.002);
    let mut accepted = captured("bybit", "ws_accepted");
    accepted["data"][0]["orderLinkId"] = json!(order_client_id);
    fold(&mut engine_under_test, &decode(&accepted));
    fold(
        &mut engine_under_test,
        &decode(&bybit_fill(order_client_id, "e-1", "0.001", "0.001", "0.002", "0.001")),
    );
    assert!(
        (position(&engine_under_test, engine_symbol) - 0.001).abs() < EPS,
        "bybit partial books"
    );
    assert_eq!(status(&engine_under_test, order_client_id), Some(OrderStatus::PartiallyFilled));
    fold(
        &mut engine_under_test,
        &decode(&bybit_fill(order_client_id, "e-2", "0.001", "0.002", "0.002", "0")),
    );
    assert!((position(&engine_under_test, engine_symbol) - 0.002).abs() < EPS, "bybit full books");
    assert_eq!(status(&engine_under_test, order_client_id), Some(OrderStatus::Filled));

    let mut ext = engine(v, engine_symbol);
    fold(&mut ext, &decode(&bybit_fill("", "e-3", "0.001", "0.001", "0.001", "0")));
    assert!(
        (position(&ext, engine_symbol) - 0.001).abs() < EPS,
        "bybit order-client-id-less fill books"
    );
}

/// okx SWAP: the same three shapes on the engine on `BTC-USDT-SWAP`.
#[test]
fn control_okx_swap_fills_fold_into_their_engine_symbol() {
    let (v, engine_symbol) = ("okx", "BTC-USDT-SWAP");
    let decode =
        |f: &Value| vike_okx::execution::event_mapper::map_okx_perp(f, v, engine_symbol, 1.0);
    let order_client_id = "okxa";
    let mut engine_under_test = engine(v, engine_symbol);
    submit(&mut engine_under_test, v, engine_symbol, order_client_id, 1, 0.002);
    fold(
        &mut engine_under_test,
        &decode(&json!({"arg": {"channel": "orders", "instType": "SWAP"}, "data": [
            {"instId": engine_symbol, "clOrdId": order_client_id, "state": "live", "ordId": "o-1", "uTime": "1"}]})),
    );
    fold(
        &mut engine_under_test,
        &decode(&okx_fill(order_client_id, "t-1", "0.001", "0.001", "partially_filled")),
    );
    assert!((position(&engine_under_test, engine_symbol) - 0.001).abs() < EPS, "okx partial books");
    assert_eq!(status(&engine_under_test, order_client_id), Some(OrderStatus::PartiallyFilled));
    fold(
        &mut engine_under_test,
        &decode(&okx_fill(order_client_id, "t-2", "0.001", "0.002", "filled")),
    );
    assert!((position(&engine_under_test, engine_symbol) - 0.002).abs() < EPS, "okx full books");
    assert_eq!(status(&engine_under_test, order_client_id), Some(OrderStatus::Filled));

    let mut ext = engine(v, engine_symbol);
    fold(&mut ext, &decode(&okx_fill("", "t-3", "0.001", "0.001", "filled")));
    assert!(
        (position(&ext, engine_symbol) - 0.001).abs() < EPS,
        "okx order-client-id-less fill books"
    );
}

/// binance SPOT keeps its BARE label — a spot engine on `BTCUSDT`, and the perp suffix
/// must never leak onto the spot lane.
#[test]
fn control_the_binance_spot_lane_keeps_its_bare_label() {
    let (v, engine_symbol) = ("binance", WIRE);
    let mut frame = captured("binance", "ws_fill");
    frame["event"]["c"] = json!("spot-a");
    frame["event"]["X"] = json!("FILLED");
    let evs = vike_binance::execution::event_mapper::spot::map_binance_private(
        &frame,
        v,
        engine_symbol,
        None,
    );
    let bare = bare_fills(&evs);
    assert_eq!(bare.len(), 1, "one spot fill: {evs:?}");
    assert_eq!(bare[0].symbol.as_str(), WIRE, "the spot lane is labelled with the bare symbol");

    let mut engine_under_test = engine(v, engine_symbol);
    fold(&mut engine_under_test, &evs);
    assert!(
        position(&engine_under_test, engine_symbol).abs() > 0.0,
        "the spot fill books into the spot engine"
    );
}
