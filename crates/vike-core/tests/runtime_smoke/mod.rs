//! R5(b) runtime gates: the headless full path (submit → accept → fill → fold → snapshot)
//! against the SAME golden the R5(a) engine passed, plus the channel-contract behaviors —
//! lossless exec lane, latest-wins market conflation, surfaced command rejection, and panic
//! safe-state.
//!
//! A module of the `integration` binary (`tests/integration.rs`), so its tests are named
//! `runtime_smoke::<member>::<test>`; it was its own `runtime_smoke` binary until it joined the group
//! so the kit (`tests/support/`) compiles once.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use vike_core::{CoreConfig, LiveBroker, spawn_core};
use vike_exec::testing::{RecordingClient, accept_submit};
use vike_exec::{
    Account, BalanceMode, BookUpdate, Command, ExecutionClient, ExecutionEngine, OrderIntent,
    QuoteUpdate, RiskGate,
};
use vike_marketdata::{BookLevel, L2Book, QuoteTick};
use vike_model::RiskLimits;
use vike_model::events::{Event, FillEvent, OrderCanceled, TradeId};
use vike_model::{Broker, Fill, OrderRequest, Strategy};
use vike_model::{OrderStatus, TradingState};

use crate::kit::doubles::ModifiableClient;
use crate::kit::engines::{sim_engine, sim_engine_with};
use vike_core::testkit::{mount_of, test_config};
use vike_exec::testing::engine_on;

// The kit is declared once, at the `integration` root; the members below reach `test_config` and
// `ModifiableClient` through their `use super::*`, which sees the imports above.
// `#[path]`, relative to this file's own directory, because this file is itself loaded by `#[path]`.
#[path = "bracket_spread_maker.rs"]
mod bracket_spread_maker;
#[path = "dispatch_lanes.rs"]
mod dispatch_lanes;
#[path = "headless_path.rs"]
mod headless_path;
#[path = "on_fill_and_latch.rs"]
mod on_fill_and_latch;
#[path = "strategy_hooks.rs"]
mod strategy_hooks;

fn fill(tid: &str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // `&str` (not `&'static str`) because one caller mints `w{w}-{i}` at runtime — `TradeId::new`
        // is the wire constructor and `expect` is honest here: every caller passes a non-empty id.
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

fn request(coid: &str, side: i32, qty: f64, price: Option<f64>) -> Box<OrderRequest> {
    Box::new(
        serde_json::from_value(serde_json::json!({
            "client_order_id": coid, "venue": "sim", "symbol": "BTCUSDT",
            "side": side, "qty": qty,
            "order_type": if price.is_some() { "limit" } else { "market" },
            "price": price
        }))
        .unwrap(),
    )
}

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> vike_marketdata::Bar {
    vike_marketdata::Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None, // series key carries the symbol
    }
}

/// Accepts every order and cancels on request — proves the default `submit_batch`/`cancel_batch`
/// fan-out produces one independent event stream per order.
#[derive(Default)]
struct BatchTestClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for BatchTestClient {
    fn submit(&mut self, request: &OrderRequest) {
        accept_submit(&mut self.events, request);
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.events.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "batch".into(),
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

fn batch_engine(limits: RiskLimits) -> ExecutionEngine<BatchTestClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        BatchTestClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn ext_fill(coid: &str, trade_id: &'static str, symbol: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.1,
        commission_asset: String::new().into(),
        liquidity_side: "maker".into(),
        ts: 5,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    })
}
