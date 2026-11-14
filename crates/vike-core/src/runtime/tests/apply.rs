use super::*;
use vike_execution::MarkSource;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, ConditionalIntent, OrderIntent, RiskGate};
use vike_model::OrderRequest;
use vike_model::RiskLimits;
use vike_model::TradingState;

// `test_core` and `core_with` are the runtime's shared white-box builders; the child modules below
// reach them through `use super::*`. The sibling-cancel knob test (`bracket_contingency`) takes
// `core_with` to flip `oco_cancel_sibling_on_dead_exit` on; every other field stays at its inert
// default.
use crate::runtime::test_support::{core_of, core_with, sim_engine_with, test_core};
use vike_execution::testing::engine_on;

/// `test_core` over an arbitrary client + optional EXTRA engines — needed by the mid-expansion
/// fill regression (a client that yields queued events on `poll_events`) and the multi-engine
/// MarketExit walk.
fn test_core_with<C: ExecutionClient>(client: C, extra: Vec<(f64, Engine<C>)>) -> CoreThread<C> {
    core_of(sim_engine_with(client), extra, CoreConfig::default())
}

fn extra_engine(venue: &str, symbol: &str) -> Engine<QueuedEventClient> {
    engine_on(venue, symbol, QueuedEventClient::default())
}

/// A `RecordingClient` whose `poll_events` drains a queue the TEST seeds. That is the whole
/// point: it lets a fill be made to land INSIDE the mass-cancel's own `pump_client()`, i.e.
/// after the operator hit the panic button and before the flatten legs are derived.
#[derive(Debug, Default)]
struct QueuedEventClient {
    submissions: Vec<OrderRequest>,
    cancels: Vec<String>,
    pending: std::collections::VecDeque<vike_model::events::Event>,
}

impl vike_execution::ExecutionClient for QueuedEventClient {
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.pending.pop_front()
    }
    fn submit(&mut self, request: &OrderRequest) {
        self.submissions.push(request.clone());
    }
    fn cancel(&mut self, order_client_id: &str) {
        self.cancels.push(order_client_id.to_string());
    }
}

/// The venue-event sequence a real fill arrives as, for `order_client_id` on (venue, symbol).
///
/// ⚠ `OrderSubmitted` FIRST IS LOAD-BEARING, not decoration. The FSM's table is
/// `Initialized --OrderSubmitted--> Submitted --OrderAccepted--> Accepted --OrderFilled-->
/// Filled`, so without the first hop `OrderAccepted` is ILLEGAL from `Initialized`, the order
/// never leaves `Initialized`, and the closing `OrderFilled` is illegal too — the whole
/// sequence is refused and `dropped_terminal_on_live` moves.
///
/// This helper used to omit it, and the bracket/OCO/OTO tests below still passed — because the
/// contingency drive ran off the EVENT rather than off the fold's verdict, so it armed and
/// cancelled legs for a sequence the engine had rejected end to end. Gating the drive on
/// `EventBusFold::Applied` turned all nine of them red at once, which is how the gap in this fixture was
/// found. `RecordingClient` emits nothing of its own, so the sequence has to be spelled here in
/// full; every REAL adapter emits `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously
/// at submit (the emitter split — see `vike_binance::execution`/`vike_aster::execution::rest::spot` and the paper
/// exchange's own `submit`), so this now matches what a venue actually delivers.
fn fill_events(
    order_client_id: &str,
    venue: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> Vec<vike_model::events::Event> {
    use vike_model::events::*;
    let fill = FillEvent {
        // minted by this helper, not read off a wire — same `t-<order client id>` bytes as before
        trade_id: TradeId::prefixed("t-", order_client_id),
        order_client_id: order_client_id.to_string(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        timestamp: 0,
        mark_price: Some(px),
        position_side: vike_model::events::PositionSide::Both,
    };
    vec![
        // Initialized -> Submitted. See this fn's doc: omitting this made every later hop
        // illegal, and the bracket tests only passed because the drive ignored the verdict.
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.to_string(),
            timestamp: 0,
        }),
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        }),
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id.to_string(),
            fill,
            timestamp: 0,
        }),
    ]
}

/// Walk `order_client_id` from `Initialized` to `Accepted` exactly as a real adapter does — the emitter
/// split's synchronous `[OrderSubmitted, OrderAccepted]` pair, the one every real adapter emits at
/// submit.
///
/// ⚠ REQUIRED before cancelling/expiring a resting order in these tests. `OrderCanceled` is
/// legal only from `Accepted`/`Triggered`/`PartiallyFilled`/`PendingCancel` — NEVER from
/// `Initialized` — and `RecordingClient` emits nothing of its own, so an order it "submitted"
/// sits at `Initialized` until a test says otherwise. Same fixture gap `fill_events` had: while
/// the contingency drive ignored the fold's verdict, cancelling an `Initialized` order still
/// drove the cascade, so the shortcut was invisible.
fn accept(c: &mut CoreThread<RecordingClient>, order_client_id: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderAccepted(vike_model::events::OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(format!("v-{order_client_id}").into()),
        timestamp: 0,
    })));
}

fn market_request(order_client_id: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    })
}

// Bar has no Default and carries funding/bid/ask/symbol beyond OHLCV — build it in full.
fn mk_bar(ts: i64, low: f64, close: f64) -> vike_marketdata::Bar {
    vike_marketdata::Bar {
        timestamp: ts,
        open: 100.0,
        high: 100.0,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[path = "apply/amend.rs"]
#[cfg(test)]
mod amend;
#[path = "apply/bracket_contingency.rs"]
#[cfg(test)]
mod bracket_contingency;
#[path = "apply/combo.rs"]
#[cfg(test)]
mod combo;
#[path = "apply/conditionals_preflight.rs"]
#[cfg(test)]
mod conditionals_preflight;
#[path = "apply/held_order_client_id.rs"]
#[cfg(test)]
mod held_order_client_id;
#[path = "apply/market_exit.rs"]
#[cfg(test)]
mod market_exit;
#[path = "apply/order_list.rs"]
#[cfg(test)]
mod order_list;
#[path = "apply/shutdown_sweep.rs"]
#[cfg(test)]
mod shutdown_sweep;
#[path = "apply/stop_rest_gate.rs"]
#[cfg(test)]
mod stop_rest_gate;
