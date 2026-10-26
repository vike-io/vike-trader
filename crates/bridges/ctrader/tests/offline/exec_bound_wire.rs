//! The WIRE SHAPE of a `reduce_only` resting exit on a hedged cTrader account, read off the `Command`
//! the actor would be given: it carries the `positionId` of the open position it closes (a bracket's
//! stop-loss and take-profit both carry the same one). Sent plain, with no position, such an order
//! OPENS a position when it triggers: measured on the hedged demo account on 2026-10-25 (a locked pair),
//! and the same account closed the position when the order carried its `positionId` (2026-10-26).
//! `crates/bridges/ctrader/CLAUDE.md`'s "A hedged account binds a reduce-only resting exit".
//!
//! Deliberately SELF-CONTAINED and built on the public surface that existed before the binding did (a
//! bare symbol map, no account type: read as hedged), so the same file runs against the code that sent
//! these orders plain and against the code that refused them, and FAILS on both: that is its job. The
//! rest of the binding (capacity, FIFO, refusals, orphans, the netted twin) is `exec_resting.rs`.

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

const LONG: i64 = 700_001;

/// A client over a command channel whose book holds ONE long EURUSD position of 1000 units
/// (`100_000` centi-units, grid step `100_000`), and the channels to read what it did.
fn rig() -> (CtraderExec, mpsc::Receiver<Command>, tokio::sync::mpsc::Receiver<Ingest>) {
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
    let mut book = PositionBook::default();
    book.upsert(LONG, TrackedPosition { symbol_id: 1, side: 1, volume: 100_000, open_ts: 1_000 });
    let (tx, commands) = mpsc::channel();
    let shared = ConnShared {
        tx,
        symbols: Arc::new(SymbolMap::from_symbols(&light, &full)),
        ctid: 99,
        orders: Arc::new(Mutex::new(HashMap::new())),
        positions: Arc::new(Mutex::new(book)),
        closes: Arc::new(Mutex::new(CloseTracker::default())),
    };
    let (events, rx) = event_channel(64);
    // A sentinel path nobody creates: the client is wired and the halt stays unengaged.
    let halt = std::env::temp_dir().join(format!("vike-ctrader-bound-wire-{}", std::process::id()));
    (CtraderExec::from_shared(shared, events).with_halt_path(halt.join("HALT")), commands, rx)
}

fn new_orders(commands: &mpsc::Receiver<Command>) -> Vec<ProtoOaNewOrderReq> {
    let mut out = Vec::new();
    while let Ok(c) = commands.try_recv() {
        if let Command::NewOrder(o) = c {
            out.push(o);
        }
    }
    out
}

fn rejections(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(ing) = rx.try_recv() {
        if let Ingest::Event(Event::OrderRejected(r)) = ing {
            out.push(r.reason.to_string());
        }
    }
    out
}

/// A `reduce_only` SELL of 1000 EURUSD, the exit shape of a bracket (`vike_model::build_bracket`).
fn exit(coid: &str, order_type: OrderType, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: -1,
        qty: 1000.0,
        order_type,
        reduce_only: true,
        price: Some(price),
        trigger_price: Some(price),
        ..Default::default()
    }
}

/// The 2026-10-25 evidence, shaped: a long is open and a `reduce_only` SELL STOP (or a take-profit LIMIT)
/// goes out carrying the long's `positionId`, at the long's volume, and is not rejected.
#[test]
fn a_reduce_only_stop_and_limit_go_out_carrying_the_open_positions_id() {
    for (what, request, wire_type) in [
        ("stop", exit("b7ctstopa", OrderType::Stop, 1.12206), ProtoOaOrderType::Stop),
        ("limit", exit("b7cttp", OrderType::Limit, 1.1500), ProtoOaOrderType::Limit),
    ] {
        let (mut exec, commands, mut rx) = rig();
        exec.submit(&request);
        assert!(rejections(&mut rx).is_empty(), "a reduce_only {what} with a position was refused");
        let sent = new_orders(&commands);
        let [order] = &sent[..] else {
            panic!("{what}: expected one NewOrder, got {}", sent.len())
        };
        assert_eq!(order.position_id, Some(LONG), "{what}: sent plain, it would OPEN a position");
        assert_eq!(order.order_type, wire_type as i32, "{what}");
        assert_eq!(order.trade_side, ProtoOaTradeSide::Sell as i32, "{what}");
        assert_eq!(order.volume, 100_000, "{what}");
    }
}

/// A bracket arms its stop-loss AND its take-profit on the SAME position at the same time, each the full
/// position volume: both go out bound to it, neither starves the other.
#[test]
fn a_bracket_stop_and_take_profit_both_go_out_bound_to_the_same_position() {
    let (mut exec, commands, mut rx) = rig();
    exec.submit(&exit("sl-1", OrderType::Stop, 1.0950));
    exec.submit(&exit("tp-1", OrderType::Limit, 1.1100));
    assert!(rejections(&mut rx).is_empty(), "one leg of the bracket was lost");
    let sent = new_orders(&commands);
    let [sl, tp] = &sent[..] else { panic!("expected both legs, got {}", sent.len()) };
    assert_eq!((sl.client_order_id.as_deref(), sl.position_id), (Some("sl-1"), Some(LONG)));
    assert_eq!((tp.client_order_id.as_deref(), tp.position_id), (Some("tp-1"), Some(LONG)));
    assert_eq!((sl.volume, tp.volume), (100_000, 100_000));
}
