use super::*;
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ConditionalIntent, OrderIntent, RiskGate, RiskLimits, TradingState,
};
use vike_model::OrderRequest;

// `test_core` and `core_with` are the runtime's shared white-box builders; the child modules below
// reach them through `use super::*`. The sibling-cancel knob test (`bracket_contingency`) takes
// `core_with` to flip `oco_cancel_sibling_on_dead_exit` on; every other field stays at its inert
// default.
use crate::runtime::test_support::{core_of, core_with, engine_on, sim_engine_with, test_core};

/// `test_core` over an arbitrary client + optional EXTRA engines — needed by the mid-expansion
/// fill regression (a client that yields queued events on `poll_events`) and the multi-engine
/// MarketExit walk.
fn test_core_with<C: ExecutionClient>(
    client: C,
    extra: Vec<(f64, ExecutionEngine<C>)>,
) -> CoreThread<C> {
    core_of(sim_engine_with(client), extra, CoreConfig::default())
}

fn extra_engine(venue: &str, symbol: &str) -> ExecutionEngine<QueuedEventClient> {
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

impl vike_exec::ExecutionClient for QueuedEventClient {
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.pending.pop_front()
    }
    fn submit(&mut self, request: &OrderRequest) {
        self.submissions.push(request.clone());
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancels.push(client_order_id.to_string());
    }
}

/// The venue-event sequence a real fill arrives as, for `coid` on (venue, symbol).
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
/// `Fold::Applied` turned all nine of them red at once, which is how the gap in this fixture was
/// found. `RecordingClient` emits nothing of its own, so the sequence has to be spelled here in
/// full; every REAL adapter emits `[OrderSubmitted, OrderAccepted|OrderRejected]` synchronously
/// at submit (the emitter split — see `vike_binance::exec`/`vike_aster::spot` and the paper
/// exchange's own `submit`), so this now matches what a venue actually delivers.
fn fill_events(
    coid: &str,
    venue: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> Vec<vike_model::events::Event> {
    use vike_model::events::*;
    let fill = FillEvent {
        // minted by this helper, not read off a wire — same `t-<coid>` bytes as before
        trade_id: TradeId::prefixed("t-", coid),
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    };
    vec![
        // Initialized -> Submitted. See this fn's doc: omitting this made every later hop
        // illegal, and the bracket tests only passed because the drive ignored the verdict.
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 }),
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(format!("v-{coid}").into()),
            ts: 0,
        }),
        Event::Fill(fill.clone()),
        Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts: 0 }),
    ]
}

fn market_req(coid: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        ..Default::default()
    })
}

// Bar has no Default and carries funding/bid/ask/symbol beyond OHLCV — build it in full.
fn mk_bar(ts: i64, low: f64, close: f64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
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

#[path = "apply/bracket_contingency.rs"]
#[cfg(test)]
mod bracket_contingency;
#[path = "apply/combo.rs"]
#[cfg(test)]
mod combo;
#[path = "apply/conditionals_preflight.rs"]
#[cfg(test)]
mod conditionals_preflight;
#[path = "apply/market_exit.rs"]
#[cfg(test)]
mod market_exit;
#[path = "apply/shutdown_sweep.rs"]
#[cfg(test)]
mod shutdown_sweep;
