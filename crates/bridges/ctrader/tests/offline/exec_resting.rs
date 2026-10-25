//! A RESTING order (limit / stop) against an open position RESTS: `CtraderExec::submit` sends it as a
//! `ProtoOANewOrderReq`, and only a MARKET order is ever turned into a close-by-position-id. Before
//! this suite existed `submit` ran the reduce planner before it looked at the order type, so an
//! opposite-side stop or limit up to the open size (every bracket's stop-loss and take-profit leg,
//! `vike_model::build_bracket`: `reduce_only`, opposite side) became an IMMEDIATE market close and a
//! protective stop never protected.
//!
//! No socket: the client is built over a plain command channel (`CtraderExec::from_shared`), so each
//! test reads the `Command` the actor would have been given. The socket twins of the market paths
//! (`tests/exec_close.rs`) stay as they are; the market test here only pins that the fix left the
//! close route byte-for-byte. A resting order carries NO `positionId` on the wire (as before): see
//! `crates/bridges/ctrader/CLAUDE.md`'s "A resting exit rests" for why it is not bound.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

use vike_ctrader::conn::{Command, ConnShared};
use vike_ctrader::exec::CtraderExec;
use vike_ctrader::positions::{CloseTracker, PositionBook, TrackedPosition};
use vike_ctrader::proto::{
    ProtoOaLightSymbol, ProtoOaNewOrderReq, ProtoOaOrderType, ProtoOaSymbol, ProtoOaTradeSide,
};
use vike_ctrader::symbols::SymbolMap;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

/// EURUSD, symbol id 1, five digits, a 0.01-lot grid (`100_000` centi-units = 1000 units).
fn symbols() -> SymbolMap {
    let light = vec![ProtoOaLightSymbol {
        symbol_id: 1,
        symbol_name: Some("EURUSD".into()),
        ..Default::default()
    }];
    let full = vec![ProtoOaSymbol {
        symbol_id: 1,
        digits: 5,
        min_volume: Some(100_000),
        step_volume: Some(100_000),
        max_volume: Some(10_000_000_000),
        lot_size: Some(10_000_000),
        ..Default::default()
    }];
    SymbolMap::from_symbols(&light, &full)
}

const FIRST: i64 = 700_001;
const SECOND: i64 = 700_002;

/// A client over a command channel whose book holds the given LONG EURUSD positions
/// (`(position_id, volume_centi)`, the older first), and the channels to read what it did.
fn rig(
    longs: &[(i64, i64)],
) -> (CtraderExec, mpsc::Receiver<Command>, tokio::sync::mpsc::Receiver<Ingest>) {
    let (tx, commands) = mpsc::channel();
    let mut book = PositionBook::default();
    for (n, &(position_id, volume)) in longs.iter().enumerate() {
        book.upsert(
            position_id,
            TrackedPosition { symbol_id: 1, side: 1, volume, open_ts: 1_000 + n as i64 },
        );
    }
    let shared = ConnShared {
        tx,
        symbols: Arc::new(symbols()),
        ctid: 99,
        orders: Arc::new(Mutex::new(HashMap::new())),
        positions: Arc::new(Mutex::new(book)),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    let (events, rx) = event_channel(64);
    // A sentinel path nobody creates: the client is wired and the halt stays unengaged.
    let halt = std::env::temp_dir().join(format!("vike-ctrader-resting-{}", std::process::id()));
    (CtraderExec::from_shared(shared, events).with_halt_path(halt.join("HALT")), commands, rx)
}

/// What a submit put on the command channel: the new orders and the close legs, in order.
#[derive(Default)]
struct Sent {
    new_orders: Vec<ProtoOaNewOrderReq>,
    closes: Vec<(i64, i64)>,
}

fn sent(commands: &mpsc::Receiver<Command>) -> Sent {
    let mut out = Sent::default();
    while let Ok(c) = commands.try_recv() {
        match c {
            Command::NewOrder(o) => out.new_orders.push(o),
            Command::ClosePosition { position_id, volume, .. } => {
                out.closes.push((position_id, volume));
            }
            _ => {}
        }
    }
    out
}

fn rejected(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderRejected(r)) = ing {
            out.push(r.reason.to_string());
        }
    }
    out
}

/// A EURUSD SELL of `qty` units; the caller sets the type and prices.
fn sell(coid: &str, qty: f64, order_type: OrderType, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: -1,
        qty,
        order_type,
        reduce_only,
        ..Default::default()
    }
}

fn stop(coid: &str, qty: f64, trigger: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest { trigger_price: Some(trigger), ..sell(coid, qty, OrderType::Stop, reduce_only) }
}

fn limit(coid: &str, qty: f64, price: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest { price: Some(price), ..sell(coid, qty, OrderType::Limit, reduce_only) }
}

/// The bracket's stop-loss leg (`reduce_only` SELL STOP, sized to the entry) against the long it
/// guards RESTS as a STOP `NewOrder` at its trigger. It is not a market close: nothing may go on
/// the wire as a `ProtoOAClosePositionReq`.
#[test]
fn a_reduce_only_sell_stop_against_a_long_rests_as_a_stop() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&stop("sl-1", 1000.0, 1.0950, true));
    let got = sent(&commands);
    assert!(got.closes.is_empty(), "a resting stop was turned into a close: {:?}", got.closes);
    let [order] = &got.new_orders[..] else {
        panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
    };
    assert_eq!(order.order_type, ProtoOaOrderType::Stop as i32);
    assert_eq!(order.trade_side, ProtoOaTradeSide::Sell as i32);
    assert_eq!(order.volume, 100_000);
    assert_eq!(order.stop_price, Some(1.0950));
    assert_eq!(order.limit_price, None);
    assert_eq!(order.client_order_id.as_deref(), Some("sl-1"));
    assert_eq!(
        order.position_id, None,
        "no positionId: the wire shape of a resting order is as before"
    );
    assert!(rejected(&mut rx).is_empty(), "a resting exit is not a rejection");
}

/// The take-profit leg (`reduce_only` SELL LIMIT) rests as a LIMIT `NewOrder`.
#[test]
fn a_reduce_only_sell_limit_take_profit_rests_as_a_limit() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&limit("tp-1", 1000.0, 1.1500, true));
    let got = sent(&commands);
    assert!(got.closes.is_empty(), "a resting limit was turned into a close: {:?}", got.closes);
    let [order] = &got.new_orders[..] else {
        panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
    };
    assert_eq!(order.order_type, ProtoOaOrderType::Limit as i32);
    assert_eq!(order.limit_price, Some(1.1500));
    assert_eq!(order.stop_price, None);
    assert_eq!(order.volume, 100_000);
    assert_eq!(order.position_id, None);
    assert!(rejected(&mut rx).is_empty());
}

/// A PLAIN resting opposite order (no `reduce_only`) up to the open size rests too: it is an order
/// like any other, whose intent the flag did not state.
#[test]
fn a_plain_resting_opposite_limit_rests() {
    // The long is twice the order's size, so the order is a strict part of it (the grid minimum is
    // one thousand units, so the order cannot be smaller than that).
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 200_000)]);
    exec.submit(&limit("lim-1", 1000.0, 1.1500, false));
    let got = sent(&commands);
    assert!(got.closes.is_empty(), "a resting limit was turned into a close: {:?}", got.closes);
    let [order] = &got.new_orders[..] else {
        panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
    };
    assert_eq!(order.order_type, ProtoOaOrderType::Limit as i32);
    assert_eq!(order.volume, 100_000);
    assert_eq!(order.position_id, None);
    assert!(rejected(&mut rx).is_empty());
}

/// The size and the book make no difference to a resting order: one spanning two hedged positions,
/// and one larger than the position it reduces, both rest (neither is a close nor a rejection).
#[test]
fn a_resting_exit_rests_whatever_the_size_against_the_book() {
    let (mut exec, commands, mut rx) = rig(&[(FIRST, 60_000), (SECOND, 40_000)]);
    exec.submit(&stop("span-1", 1000.0, 1.0950, true)); // 100_000 over 60_000 + 40_000
    let (mut exec_big, commands_big, mut rx_big) = rig(&[(FIRST, 100_000)]);
    exec_big.submit(&stop("big-1", 2000.0, 1.0950, true)); // 200_000 over one 100_000 position
    for (got, rx) in [(sent(&commands), &mut rx), (sent(&commands_big), &mut rx_big)] {
        assert!(got.closes.is_empty(), "a resting stop was turned into a close: {:?}", got.closes);
        let [order] = &got.new_orders[..] else {
            panic!("expected exactly one NewOrder, got {}", got.new_orders.len());
        };
        assert_eq!(order.order_type, ProtoOaOrderType::Stop as i32);
        assert_eq!(order.position_id, None);
        assert!(rejected(rx).is_empty());
    }
}

/// A STOP-LIMIT and a TAKE-PROFIT trigger are kinds this bridge does not map. They are REJECTED
/// ("unmappable order"), as with no position open; they used to be market-closed before the mapper
/// ever saw them.
#[test]
fn an_unmapped_conditional_kind_is_rejected_not_market_closed() {
    for kind in [OrderType::StopLimit, OrderType::TakeProfit] {
        let (mut exec, commands, mut rx) = rig(&[(FIRST, 100_000)]);
        let req = OrderRequest {
            price: Some(1.0940),
            trigger_price: Some(1.0950),
            ..sell("odd-1", 1000.0, kind.clone(), true)
        };
        exec.submit(&req);
        let got = sent(&commands);
        assert!(got.closes.is_empty(), "{kind:?} was turned into a close: {:?}", got.closes);
        assert!(got.new_orders.is_empty(), "{kind:?} reached the wire");
        let reasons = rejected(&mut rx);
        assert_eq!(reasons.len(), 1, "{kind:?}: exactly one terminal rejection, got {reasons:?}");
        assert!(reasons[0].contains("unmappable"), "{kind:?}: {reasons:?}");
        assert!(
            reasons[0].contains(kind.as_str()),
            "{kind:?}: the rejection must name the order type it refuses, got {reasons:?}"
        );
    }
}

/// The unchanged half, pinned: a MARKET sell up to the open size closes by position id (no
/// `NewOrder`), the `reduce_only` market variant likewise and CAPPED at the position; a market sell
/// past the open size is a flip and stays a plain `NewOrder`.
#[test]
fn a_market_order_still_closes_by_position_id() {
    let (mut exec, commands, _rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("mkt-1", 1000.0, OrderType::Market, false));
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)]);
    assert!(got.new_orders.is_empty(), "a market flatten opened a hedge");

    let (mut exec, commands, _rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("mkt-2", 5000.0, OrderType::Market, true)); // reduce_only caps at the position
    let got = sent(&commands);
    assert_eq!(got.closes, vec![(FIRST, 100_000)]);
    assert!(got.new_orders.is_empty());

    let (mut exec, commands, _rx) = rig(&[(FIRST, 100_000)]);
    exec.submit(&sell("mkt-3", 5000.0, OrderType::Market, false)); // a flip: a plain new order
    let got = sent(&commands);
    assert!(got.closes.is_empty());
    let [order] = &got.new_orders[..] else { panic!("a flip is one NewOrder") };
    assert_eq!(order.order_type, ProtoOaOrderType::Market as i32);
    assert_eq!(order.position_id, None);
}
