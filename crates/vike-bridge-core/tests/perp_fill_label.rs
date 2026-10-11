//! The PERP fill label, folded through a REAL `ExecutionEngine` (and the `vike_exec::Account`
//! behind it): every execution the binance-family perp lane reports must move the POSITION of an
//! engine mounted the way production mounts a perp, and the history replay of a trade the stream
//! already delivered must not move it a second time.
//!
//! ## The label contract
//!
//! Production mounts a binance-family perp on its CATALOG spelling, `BTCUSDT.P`
//! (`crates/vike-tradehub/src/wired_markets.rs`'s `ASTER_MARKET`; the Trade window sends the same
//! spelling), and an engine folds a fill only when the fill's `symbol` is that exact string:
//! `vike_exec::ExecutionEngine::accepts_symbol` is string equality, and the "own order on any
//! symbol" route (`owns_fill_symbol`) needs the fill to name its ORDER's `.P` symbol too. A fill,
//! partial or liquidation labelled with the frame's bare `o.s` (`BTCUSDT`) advances the ORDER to
//! FILLED (its lifecycle wrap routes by client order id) while the POSITION, realized PnL and
//! commission never move; a bare-labelled reconcile report row likewise drops a synthesized
//! `MissingFill` and raises `PositionOnlyExternal` + `OrphanLocalPosition` on every pass.
//!
//! So the shared family code labels every perp-lane symbol `<exchange symbol>` +
//! `vike_catalog::PERP_SUFFIX` — ONE place for both venues — and labels by the FRAME's symbol, never
//! by the mounted one: the user-data stream is account-wide, so a foreign `ETHUSDT` fill must become
//! `ETHUSDT.P`, never `BTCUSDT.P`.
//!
//! ## The replay risk, and the two tests that hold it
//!
//! A fill that FOLDS from the stream is delivered AGAIN by the history replay (the `run_loop` gap
//! sentinel five seconds after the last command, and the resync supervisor on every reconnect). The
//! fill-side guard is `Account::apply_fill`'s ledger, keyed on the bare `trade_id`, and it holds only
//! if the stream's `o.t` and the replay's `userTrades[].id` render to the SAME string for the same
//! trade — `the_history_replay_of_a_streamed_fill_does_not_double_the_position` asserts exactly that,
//! then the position. A LIQUIDATION is guarded by a DIFFERENT set (the engine's `seen_liq_ids`),
//! which the account ledger never sees, so the replay must re-emit a liquidation trade the way the
//! stream does — as a `PositionLiquidated`, never as a fill —
//! `the_history_replay_of_a_streamed_liquidation_does_not_close_twice` holds that.
//!
//! ## Controls
//!
//! bybit linear and okx SWAP label with the venue's own symbol, which IS what their engines are
//! mounted on (`BTCUSDT`, `BTC-USDT-SWAP`); they must keep folding. The binance SPOT lane keeps its
//! bare label, because a spot engine is mounted on the bare `BTCUSDT`.
//!
//! Frames are the venues' own where one is committed: the binance-family `ORDER_TRADE_UPDATE`
//! shapes come from the frozen `fixtures/r6/perp.json` (Binance's wire — aster's perp stream is
//! Binance-verbatim, which is why the two venues share one mapper), bybit's and binance spot's
//! from their sanitized captures under `crates/bridges/<venue>/tests/fixtures/captured/`. Every
//! mapper is called with the arguments its production pump passes it.

use serde_json::{Value, json};
use std::assert_matches;

use vike_exec::recon::{Divergence, ReconPolicy, diff, resolve};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, OrderStatus, Outbox, RiskGate,
};
use vike_model::RiskLimits;
use vike_model::events::{Event, FillEvent, OrderSubmitted, PositionLiquidated};
use vike_model::{FillReport, OrderRequest, PositionStatusReport};

/// The catalog spelling a binance-family perp is mounted on.
const MOUNTED: &str = "BTCUSDT.P";
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
    /// mounted series symbol as the absent-`s` fallback)`.
    decode: fn(&Value, &str, &str) -> Vec<Event>,
    /// The perp history replay (`allOrders` + `userTrades`), called as `perp_history_events` calls
    /// it: `(orders, trades, venue, the mounted series symbol)`.
    replay: fn(&Value, &Value, &str, &str) -> Vec<Event>,
    /// The perp `userTrades` reconcile parser (the venue's 1-arg wrapper).
    parse_fills: fn(&str) -> Result<Vec<FillReport>, String>,
    /// The perp `positionRisk` reconcile parser (the venue's 1-arg wrapper).
    parse_positions: fn(&str) -> Result<Vec<PositionStatusReport>, String>,
}

const ASTER: FamilyLane = FamilyLane {
    venue: "aster",
    decode: vike_aster::perp_mapper::map_aster_perp,
    replay: vike_aster::history::map_aster_perp_history,
    parse_fills: vike_aster::recon_client::parse_perp_user_trades,
    parse_positions: vike_aster::recon_client::parse_perp_position_risk,
};

const BINANCE: FamilyLane = FamilyLane {
    venue: "binance",
    decode: vike_binance::perp_mapper::map_binance_perp,
    replay: vike_binance::history::map_binance_perp_history,
    parse_fills: vike_binance::recon_client::parse_perp_user_trades,
    parse_positions: vike_binance::recon_client::parse_perp_position_risk,
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
    a_streamed_perp_fill_moves_the_position_of_the_engine_mounted_on_the_catalog_spelling,
    a_partial_perp_fill_moves_the_position_before_the_order_completes,
    a_perp_fill_with_no_local_order_on_the_mounted_symbol_folds,
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

/// A REAL engine over a REAL `Account`, mounted on `(venue, mounted)` — the `vike_mount` shape:
/// no `extra_symbols`.
fn engine(venue: &str, mounted: &str) -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        mounted,
    )
}

/// Submit a limit order on `symbol` through the engine's own gate (so it is REGISTERED), then fold
/// the Rust-side `OrderSubmitted` half of the emitter split.
fn submit(
    eng: &mut ExecutionEngine<RecordingClient>,
    venue: &str,
    symbol: &str,
    coid: &str,
    side: i32,
    qty: f64,
) {
    let req = OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(60_000.0),
        ts: 1,
        ..Default::default()
    };
    let mut outbox = Outbox::default();
    eng.submit_order(&req, 1, &mut outbox);
    assert!(
        eng.registry.contains_key(coid),
        "[{venue}] the RiskGate refused the test order {coid}"
    );
    eng.on_event(
        &Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 1 }),
        &mut outbox,
    );
}

fn fold(eng: &mut ExecutionEngine<RecordingClient>, events: &[Event]) {
    let mut outbox = Outbox::default();
    for ev in events {
        eng.on_event(ev, &mut outbox);
    }
}

/// The engine's net position on `symbol`, summed over every position side (hedge buckets
/// included), read through the same `local_view` the reconcile driver reads.
fn position(eng: &ExecutionEngine<RecordingClient>, symbol: &str) -> f64 {
    eng.local_view().positions.iter().filter(|((s, _), _)| s == symbol).map(|(_, q)| *q).sum()
}

fn status(eng: &ExecutionEngine<RecordingClient>, coid: &str) -> Option<OrderStatus> {
    eng.registry.get(coid).map(|mo| mo.status)
}

fn bare_fills(events: &[Event]) -> Vec<&FillEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect()
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
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r6/perp.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing r6 fixture {}: {e}", path.display()));
    let root: Value = serde_json::from_str(&body).expect("r6 perp fixture parses");
    root["mapper"].as_array().expect("r6 mapper cases").iter().map(|c| c["frame"].clone()).collect()
}

/// The first committed `ORDER_TRADE_UPDATE` frame whose order block satisfies `pred`.
fn r6_otu(pred: impl Fn(&Value) -> bool) -> Value {
    r6_mapper_frames()
        .into_iter()
        .find(|f| f["e"] == "ORDER_TRADE_UPDATE" && pred(&f["o"]))
        .expect("the r6 fixture carries the ORDER_TRADE_UPDATE shape this test needs")
}

fn is_liq_coid(o: &Value) -> bool {
    o["c"].as_str().is_some_and(|c| c.contains("autoclose"))
}

/// `x=NEW` for `coid` on `wire`.
fn otu_new(wire: &str, coid: &str) -> Value {
    let mut f = r6_otu(|o| o["x"] == "NEW");
    f["o"]["s"] = json!(wire);
    f["o"]["c"] = json!(coid);
    f
}

/// `x=TRADE` — one execution of `qty` at `px` for `coid` on `wire`, order status `x_status` after
/// it, venue trade id `t`.
fn otu_trade(
    wire: &str,
    coid: &str,
    x_status: &str,
    side: &str,
    qty: &str,
    px: &str,
    t: u64,
) -> Value {
    let mut f = r6_otu(|o| o["x"] == "TRADE" && !is_liq_coid(o));
    let o = &mut f["o"];
    o["s"] = json!(wire);
    o["c"] = json!(coid);
    o["X"] = json!(x_status);
    o["S"] = json!(side);
    o["l"] = json!(qty);
    o["L"] = json!(px);
    o["t"] = json!(t);
    o["ps"] = json!("BOTH");
    f
}

/// A venue LIQUIDATION execution (`autoclose-` client order id) of `qty` at `px`, trade id `t`.
fn otu_liquidation(wire: &str, side: &str, qty: &str, px: &str, t: u64) -> Value {
    let mut f = r6_otu(|o| o["x"] == "TRADE" && is_liq_coid(o));
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
fn all_orders_row(order_id: u64, coid: &str, status: &str) -> Value {
    json!({"orderId": order_id, "clientOrderId": coid, "symbol": WIRE, "status": status,
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

/// (a) A coid-linked fill of the engine's own order moves the position of the engine mounted on
/// the catalog spelling — and is LABELLED with that spelling.
fn a_streamed_perp_fill_moves_the_position_of_the_engine_mounted_on_the_catalog_spelling(
    lane: FamilyLane,
) {
    let v = lane.venue;
    let coid = "perp-a";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.002);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, coid), v, MOUNTED));
    let fill = (lane.decode)(
        &otu_trade(WIRE, coid, "FILLED", "BUY", "0.002", "62584.5", 998_877),
        v,
        MOUNTED,
    );
    fold(&mut eng, &fill);

    let pos = position(&eng, MOUNTED);
    assert!(
        (pos - 0.002).abs() < EPS,
        "[{v}] the fill was labelled {:?}; the engine mounted on {MOUNTED:?} left the order {:?} \
             and the position at {pos} — the fill was DROPPED from the position bookkeeping",
        labels(&fill),
        status(&eng, coid)
    );
    assert_eq!(status(&eng, coid), Some(OrderStatus::Filled), "[{v}] order status");
    let bare = bare_fills(&fill);
    assert_eq!(bare.len(), 1, "[{v}] one bare fill");
    assert_eq!(bare[0].symbol.as_str(), MOUNTED, "[{v}] the fill's label is the catalog spelling");
}

/// (b) A PARTIAL fill moves the position at once — the resting remainder must not hold the booking
/// back until the next command or reconnect happens to replay it.
fn a_partial_perp_fill_moves_the_position_before_the_order_completes(lane: FamilyLane) {
    let v = lane.venue;
    let coid = "perp-b";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.002);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, coid), v, MOUNTED));

    let partial = (lane.decode)(
        &otu_trade(WIRE, coid, "PARTIALLY_FILLED", "BUY", "0.001", "62584.5", 1_001),
        v,
        MOUNTED,
    );
    fold(&mut eng, &partial);
    let pos = position(&eng, MOUNTED);
    assert!(
        (pos - 0.001).abs() < EPS,
        "[{v}] partial labelled {:?}: order {:?} but position {pos} — DROPPED",
        labels(&partial),
        status(&eng, coid)
    );
    assert_eq!(status(&eng, coid), Some(OrderStatus::PartiallyFilled), "[{v}]");

    fold(
        &mut eng,
        &(lane.decode)(
            &otu_trade(WIRE, coid, "FILLED", "BUY", "0.001", "62590.1", 1_002),
            v,
            MOUNTED,
        ),
    );
    let pos = position(&eng, MOUNTED);
    assert!((pos - 0.002).abs() < EPS, "[{v}] both executions booked, got {pos}");
    assert_eq!(status(&eng, coid), Some(OrderStatus::Filled), "[{v}]");
}

/// (d) A fill the engine has NO order for — an order placed outside vike on the same account and
/// symbol, with an empty or a foreign client order id — still folds into the mounted symbol's
/// position, exactly as it does on bybit and okx.
fn a_perp_fill_with_no_local_order_on_the_mounted_symbol_folds(lane: FamilyLane) {
    let v = lane.venue;
    for (n, coid) in ["", "web_1234567"].into_iter().enumerate() {
        let mut eng = engine(v, MOUNTED);
        let fill = (lane.decode)(
            &otu_trade(WIRE, coid, "FILLED", "SELL", "0.003", "62000.0", 2_000 + n as u64),
            v,
            MOUNTED,
        );
        fold(&mut eng, &fill);
        let pos = position(&eng, MOUNTED);
        assert!(
            (pos + 0.003).abs() < EPS,
            "[{v}] coid {coid:?}: fill labelled {:?}, position {pos} — DROPPED",
            labels(&fill)
        );
    }
}

/// (d) The stream is ACCOUNT-WIDE: a fill on ANOTHER symbol must keep ITS OWN perp label — never
/// the mounted one — so it lands on its own engine and never on this one.
fn a_perp_fill_for_another_symbol_keeps_its_own_label(lane: FamilyLane) {
    let v = lane.venue;
    let fill = (lane.decode)(
        &otu_trade("ETHUSDT", "", "FILLED", "BUY", "0.1", "2400.0", 3_000),
        v,
        MOUNTED,
    );
    // The money first: the engine mounted on the fill's OWN perp books it…
    let mut eth = engine(v, "ETHUSDT.P");
    fold(&mut eth, &fill);
    let pos = position(&eth, "ETHUSDT.P");
    assert!(
        (pos - 0.1).abs() < EPS,
        "[{v}] the ETHUSDT.P engine left the position at {pos}: the fill was labelled {:?}",
        labels(&fill)
    );
    // …the BTC engine on the same account books nothing…
    let mut btc = engine(v, MOUNTED);
    fold(&mut btc, &fill);
    assert_eq!(position(&btc, MOUNTED), 0.0, "[{v}] the BTC engine must not book an ETH fill");
    assert_eq!(position(&btc, "ETHUSDT.P"), 0.0, "[{v}] nor hold it under any symbol");
    // …because the label came from the FRAME, never from the mount.
    let bare = bare_fills(&fill);
    assert_eq!(bare.len(), 1, "[{v}] one bare fill");
    assert_eq!(bare[0].symbol.as_str(), "ETHUSDT.P", "[{v}] labelled from the frame's own `s`");
}

/// A venue LIQUIDATION closes the local perp position.
fn a_venue_liquidation_closes_the_perp_position(lane: FamilyLane) {
    let v = lane.venue;
    let coid = "perp-liq";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.5);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, coid), v, MOUNTED));
    fold(
        &mut eng,
        &(lane.decode)(
            &otu_trade(WIRE, coid, "FILLED", "BUY", "0.5", "60000.0", 4_000),
            v,
            MOUNTED,
        ),
    );
    assert!((position(&eng, MOUNTED) - 0.5).abs() < EPS, "[{v}] setup: long 0.5 is open");

    let liq = (lane.decode)(&otu_liquidation(WIRE, "SELL", "0.5", "58000.0", 4_001), v, MOUNTED);
    assert!(
        liq.iter().any(|e| matches!(e, Event::PositionLiquidated(_))),
        "[{v}] an autoclose execution decodes to PositionLiquidated: {liq:?}"
    );
    fold(&mut eng, &liq);
    let pos = position(&eng, MOUNTED);
    assert!(
        pos.abs() < EPS,
        "[{v}] liquidation labelled {:?} left the position at {pos} — DROPPED",
        labels(&liq)
    );
}

// ===================================================================================================
// (c): reconcile.
// ===================================================================================================

/// (c) A fill the stream never delivered is raised by reconcile as a `MissingFill`, and the event
/// `resolve` synthesizes for it must FOLD into the perp position.
fn a_reconcile_missing_fill_folds_into_the_perp_position(lane: FamilyLane) {
    let v = lane.venue;
    let mut eng = engine(v, MOUNTED);
    let body = Value::Array(vec![user_trade_row(5_000, 7_000, "BUY", "0.002", "62584.5", "0.05")])
        .to_string();
    let fills = (lane.parse_fills)(&body).expect("userTrades parses");

    let local = eng.local_view();
    let divs = diff(&[], &fills, &[], &local.as_view(), None);
    assert_matches!(
        divs.as_slice(),
        [Divergence::MissingFill(_)],
        "[{v}] one MissingFill: {divs:?}"
    );
    // `hybrid` is the preset under which a MissingFill AUTO-folds (`quarantine`, the shipped
    // default, holds it for an operator) — the synthesized event is the same either way.
    let recon = resolve(divs, &ReconPolicy::hybrid(), None, None);
    fold(&mut eng, &recon.events);
    let pos = position(&eng, MOUNTED);
    assert!(
        (pos - 0.002).abs() < EPS,
        "[{v}] reconcile synthesized {:?} and the engine left the position at {pos} — DROPPED",
        labels(&recon.events)
    );
    assert_eq!(fills[0].symbol, MOUNTED, "[{v}] the fill report carries the perp label");
}

/// (c) The POSITION check: the venue's `positionRisk` row for an open perp position must name the
/// same key as the local book, so an agreed position raises NOTHING — not a
/// `PositionOnlyExternal` + `OrphanLocalPosition` pair on every pass.
fn a_reconcile_position_report_names_the_local_perp_position(lane: FamilyLane) {
    let v = lane.venue;
    let mut eng = engine(v, MOUNTED);
    // Open the local position through the REPLAY lane, whose label has always been the mounted
    // series symbol — so this test isolates the REPORT side of the comparison.
    let replay = (lane.replay)(
        &json!([all_orders_row(7_100, "perp-pos", "FILLED")]),
        &json!([user_trade_row(5_100, 7_100, "BUY", "0.5", "60000.0", "0.1")]),
        v,
        MOUNTED,
    );
    fold(&mut eng, &replay);
    assert!((position(&eng, MOUNTED) - 0.5).abs() < EPS, "[{v}] setup: local long 0.5");

    let body = json!([{"symbol": WIRE, "positionAmt": "0.500", "entryPrice": "60000.0",
                           "markPrice": "60100.0", "positionSide": "BOTH",
                           "updateTime": 1_751_600_001_000_i64}])
    .to_string();
    let positions = (lane.parse_positions)(&body).expect("positionRisk parses");
    let local = eng.local_view();
    let divs = diff(&[], &[], &positions, &local.as_view(), None);
    assert!(
        divs.is_empty(),
        "[{v}] venue row {:?} vs local {MOUNTED:?}: an agreed position raised {divs:?}",
        positions.iter().map(|p| p.symbol.as_str()).collect::<Vec<_>>()
    );
}

/// (c) The reconcile pass AFTER a streamed fill raises nothing: the stream's `o.t` and the fill
/// report's `id` are the same trade id, so the fold the stream made is the one reconcile sees.
fn a_reconcile_pass_after_a_streamed_fill_raises_nothing(lane: FamilyLane) {
    let v = lane.venue;
    let coid = "perp-rc";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.002);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, coid), v, MOUNTED));
    fold(
        &mut eng,
        &(lane.decode)(
            &otu_trade(WIRE, coid, "FILLED", "BUY", "0.002", "62584.5", 5_200),
            v,
            MOUNTED,
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
    let local = eng.local_view();
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
    let coid = "perp-replay";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.002);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, coid), v, MOUNTED));
    let stream = (lane.decode)(
        &otu_trade(WIRE, coid, "FILLED", "BUY", "0.002", "62584.5", 998_877),
        v,
        MOUNTED,
    );
    fold(&mut eng, &stream);
    let pos = position(&eng, MOUNTED);
    assert!(
        (pos - 0.002).abs() < EPS,
        "[{v}] the streamed fill labelled {:?} did not move the position ({pos})",
        labels(&stream)
    );
    let balance_after_stream = eng.account.balance;

    let replay = (lane.replay)(
        &json!([all_orders_row(555_666_777, coid, "FILLED")]),
        &json!([user_trade_row(998_877, 555_666_777, "BUY", "0.002", "62584.5", "0.05")]),
        v,
        MOUNTED,
    );
    // The dedup's precondition, stated as data: one trade, one id string, one label.
    let (s, r) = (bare_fills(&stream), bare_fills(&replay));
    assert_eq!((s.len(), r.len()), (1, 1), "[{v}] one bare fill on each lane");
    assert_eq!(s[0].trade_id.as_str(), r[0].trade_id.as_str(), "[{v}] stream `t` vs replay `id`");
    assert_eq!(s[0].symbol, r[0].symbol, "[{v}] stream and replay label the trade alike");

    let refused_before = eng.account.duplicate_fills_refused;
    fold(&mut eng, &replay);
    let pos = position(&eng, MOUNTED);
    assert!((pos - 0.002).abs() < EPS, "[{v}] the replay DOUBLED the position: {pos}");
    assert_eq!(
        eng.account.duplicate_fills_refused - refused_before,
        1,
        "[{v}] the account refused the replayed copy"
    );
    assert_eq!(eng.account.colliding_fills_refused, 0, "[{v}] a re-delivery, not a collision");
    assert_eq!(
        eng.account.balance.to_bits(),
        balance_after_stream.to_bits(),
        "[{v}] ONE commission"
    );
    let filled = eng.registry[coid].filled_qty;
    assert!((filled - 0.002).abs() < EPS, "[{v}] the order's filled_qty doubled: {filled}");
}

/// A LIQUIDATION the stream already folded must not be closed a SECOND time by the replay: the
/// engine guards a liquidation with `seen_liq_ids`, which the account's fill ledger never sees, so
/// a replay that re-emitted the trade as a bare FILL would fold it as a fresh execution — closing an
/// already-closed position flips it to the opposite side.
///
/// ⚠ A position is OPENED AGAIN between the stream's liquidation and the replay, because a flat
/// book cannot tell the dedup from `vike_exec::Account::apply_liquidation`'s flat clamp: on a flat
/// book a replayed liquidation is a no-op whether or not `seen_liq_ids` recognised it as the
/// stream's, so a replay minting a DIFFERENT id would pass there. With a live position under it, a
/// replay the dedup does not catch closes that position, and only the shared id keeps it open.
fn the_history_replay_of_a_streamed_liquidation_does_not_close_twice(lane: FamilyLane) {
    let v = lane.venue;
    let coid = "perp-open";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.5);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, coid), v, MOUNTED));
    fold(
        &mut eng,
        &(lane.decode)(
            &otu_trade(WIRE, coid, "FILLED", "BUY", "0.5", "60000.0", 6_000),
            v,
            MOUNTED,
        ),
    );
    // Without this the case is vacuous on a broken label: a dropped OPENING fill leaves the
    // book flat, and a flat book reads exactly like a correctly liquidated one.
    assert!((position(&eng, MOUNTED) - 0.5).abs() < EPS, "[{v}] setup: long 0.5 is open");
    let liq = (lane.decode)(&otu_liquidation(WIRE, "SELL", "0.5", "58000.0", 6_001), v, MOUNTED);
    fold(&mut eng, &liq);
    let pos = position(&eng, MOUNTED);
    assert!(
        pos.abs() < EPS,
        "[{v}] the streamed liquidation labelled {:?} did not close the position ({pos})",
        labels(&liq)
    );

    // A NEW position, opened after the liquidation and before the replay arrives (see the doc).
    let reopen = "perp-reopen";
    submit(&mut eng, v, MOUNTED, reopen, 1, 0.2);
    fold(&mut eng, &(lane.decode)(&otu_new(WIRE, reopen), v, MOUNTED));
    fold(
        &mut eng,
        &(lane.decode)(
            &otu_trade(WIRE, reopen, "FILLED", "BUY", "0.2", "58100.0", 6_002),
            v,
            MOUNTED,
        ),
    );
    assert!((position(&eng, MOUNTED) - 0.2).abs() < EPS, "[{v}] setup: long 0.2 is reopened");

    let replay = (lane.replay)(
        &json!([all_orders_row(8_000, "autoclose-12345", "FILLED")]),
        &json!([user_trade_row(6_001, 8_000, "SELL", "0.5", "58000.0", "2.5")]),
        v,
        MOUNTED,
    );
    fold(&mut eng, &replay);
    // The money first: the position opened after the liquidation survives its replay…
    let pos = position(&eng, MOUNTED);
    assert!(
        (pos - 0.2).abs() < EPS,
        "[{v}] the replayed liquidation moved the position opened after it from 0.2 to {pos}; it \
         emitted {:?}",
        labels(&replay)
    );
    // …because the dedup's precondition holds, stated as data like the fill case's: one
    // liquidation on each lane, one id string.
    let (s, r) = (liquidations(&liq), liquidations(&replay));
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
    let coid = "perp-lite";
    let mut eng = engine(v, MOUNTED);
    submit(&mut eng, v, MOUNTED, coid, 1, 0.002);
    let decode = |f: &Value| vike_binance::perp_mapper::map_binance_perp_opts(f, v, MOUNTED, true);
    fold(&mut eng, &decode(&otu_new(WIRE, coid)));
    let lite = json!({"e": "TRADE_LITE", "E": 1, "T": 1, "s": WIRE, "q": "0.002", "p": "62584.5",
                      "m": false, "c": coid, "S": "BUY", "L": "62584.5", "l": "0.002", "t": 9_100,
                      "i": 555_666_777_u64});
    let early = decode(&lite);
    fold(&mut eng, &early);
    let pos = position(&eng, MOUNTED);
    assert!(
        (pos - 0.002).abs() < EPS,
        "the early fill labelled {:?} did not move the position ({pos})",
        labels(&early)
    );
    fold(&mut eng, &decode(&otu_trade(WIRE, coid, "FILLED", "BUY", "0.002", "62584.5", 9_100)));
    let pos = position(&eng, MOUNTED);
    assert!((pos - 0.002).abs() < EPS, "the authoritative twin booked the trade again: {pos}");
    assert_eq!(status(&eng, coid), Some(OrderStatus::Filled));
}

// ===================================================================================================
// Controls: bybit linear, okx SWAP and the binance SPOT lane label with what their engines mount.
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
/// `total`) for `coid`.
fn bybit_fill(
    coid: &str,
    exec_id: &str,
    this: &str,
    cum: &str,
    total: &str,
    leaves: &str,
) -> Value {
    let mut f = captured("bybit", "ws_fill");
    let row = &mut f["data"][0];
    row["orderLinkId"] = json!(coid);
    row["execId"] = json!(exec_id);
    row["execQty"] = json!(this);
    row["cumExecQty"] = json!(cum);
    row["orderQty"] = json!(total);
    row["leavesQty"] = json!(leaves);
    f
}

fn okx_fill(coid: &str, trade_id: &str, this: &str, cum: &str, state: &str) -> Value {
    json!({"arg": {"channel": "orders", "instType": "SWAP"}, "data": [
        {"instId": "BTC-USDT-SWAP", "clOrdId": coid, "state": state, "tradeId": trade_id,
         "fillSz": this, "fillPx": "62584.5", "fillFee": "-0.01", "fillFeeCcy": "USDT",
         "side": "buy", "posSide": "net", "execType": "T", "accFillSz": cum, "sz": "0.002",
         "fillTime": "1"}]})
}

/// bybit linear: a partial then a completing fill of the engine's own order, then a fill with no
/// local order — all fold into the engine mounted on bare `BTCUSDT`, the label the venue sends.
#[test]
fn control_bybit_linear_fills_fold_into_their_mounted_symbol() {
    let (v, mounted) = ("bybit", "BTCUSDT");
    let decode = |f: &Value| vike_bybit::event_mapper::map_bybit_perp(f, v, mounted);
    let coid = "bybit-a";
    let mut eng = engine(v, mounted);
    submit(&mut eng, v, mounted, coid, 1, 0.002);
    let mut accepted = captured("bybit", "ws_accepted");
    accepted["data"][0]["orderLinkId"] = json!(coid);
    fold(&mut eng, &decode(&accepted));
    fold(&mut eng, &decode(&bybit_fill(coid, "e-1", "0.001", "0.001", "0.002", "0.001")));
    assert!((position(&eng, mounted) - 0.001).abs() < EPS, "bybit partial books");
    assert_eq!(status(&eng, coid), Some(OrderStatus::PartiallyFilled));
    fold(&mut eng, &decode(&bybit_fill(coid, "e-2", "0.001", "0.002", "0.002", "0")));
    assert!((position(&eng, mounted) - 0.002).abs() < EPS, "bybit full books");
    assert_eq!(status(&eng, coid), Some(OrderStatus::Filled));

    let mut ext = engine(v, mounted);
    fold(&mut ext, &decode(&bybit_fill("", "e-3", "0.001", "0.001", "0.001", "0")));
    assert!((position(&ext, mounted) - 0.001).abs() < EPS, "bybit coid-less fill books");
}

/// okx SWAP: the same three shapes on the engine mounted on `BTC-USDT-SWAP`.
#[test]
fn control_okx_swap_fills_fold_into_their_mounted_symbol() {
    let (v, mounted) = ("okx", "BTC-USDT-SWAP");
    let decode = |f: &Value| vike_okx::event_mapper::map_okx_perp(f, v, mounted, 1.0);
    let coid = "okxa";
    let mut eng = engine(v, mounted);
    submit(&mut eng, v, mounted, coid, 1, 0.002);
    fold(
        &mut eng,
        &decode(&json!({"arg": {"channel": "orders", "instType": "SWAP"}, "data": [
            {"instId": mounted, "clOrdId": coid, "state": "live", "ordId": "o-1", "uTime": "1"}]})),
    );
    fold(&mut eng, &decode(&okx_fill(coid, "t-1", "0.001", "0.001", "partially_filled")));
    assert!((position(&eng, mounted) - 0.001).abs() < EPS, "okx partial books");
    assert_eq!(status(&eng, coid), Some(OrderStatus::PartiallyFilled));
    fold(&mut eng, &decode(&okx_fill(coid, "t-2", "0.001", "0.002", "filled")));
    assert!((position(&eng, mounted) - 0.002).abs() < EPS, "okx full books");
    assert_eq!(status(&eng, coid), Some(OrderStatus::Filled));

    let mut ext = engine(v, mounted);
    fold(&mut ext, &decode(&okx_fill("", "t-3", "0.001", "0.001", "filled")));
    assert!((position(&ext, mounted) - 0.001).abs() < EPS, "okx coid-less fill books");
}

/// binance SPOT keeps its BARE label — a spot engine is mounted on `BTCUSDT`, and the perp suffix
/// must never leak onto the spot lane.
#[test]
fn control_the_binance_spot_lane_keeps_its_bare_label() {
    let (v, mounted) = ("binance", WIRE);
    let mut frame = captured("binance", "ws_fill");
    frame["event"]["c"] = json!("spot-a");
    frame["event"]["X"] = json!("FILLED");
    let evs = vike_binance::event_mapper::map_binance_private(&frame, v, mounted);
    let bare = bare_fills(&evs);
    assert_eq!(bare.len(), 1, "one spot fill: {evs:?}");
    assert_eq!(bare[0].symbol.as_str(), WIRE, "the spot lane is labelled with the bare symbol");

    let mut eng = engine(v, mounted);
    fold(&mut eng, &evs);
    assert!(position(&eng, mounted).abs() > 0.0, "the spot fill books into the spot engine");
}
