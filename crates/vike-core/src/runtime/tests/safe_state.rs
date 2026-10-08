//! Safe-state / watchdog / equity-sampler / readiness-gate unit tests — split out of the runtime
//! fold module verbatim (the former inline `#[cfg(test)] mod safe_state_tests` body). `use super::*`
//! re-exports the parent runtime module's items, so nothing about resolution changes.

use super::*;
use std::sync::atomic::AtomicUsize;
use tracing_test::traced_test;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, PositionEntry, RiskGate, RiskLimits};
use vike_marketdata::test_support::flat_bar_unit_volume;
use vike_model::events::{FillEvent, OrderAccepted, OrderFilled, OrderSubmitted, TradeId};
use vike_model::{QuoteTick, TradeTick};

// The runtime's shared white-box builders, reached by every child module below through
// `use super::*`. `core_with` is for the tests that need a non-default config — the readiness-gate
// ones mount a strategy and set `readiness_gate`, neither of which `test_core` allows.
use crate::runtime::test_support::{core_with, test_core};

#[path = "safe_state/core_safe_state.rs"]
#[cfg(test)]
mod core_safe_state;

#[path = "safe_state/drawdown_latch.rs"]
#[cfg(test)]
mod drawdown_latch;

#[path = "safe_state/equity_sampler.rs"]
#[cfg(test)]
mod equity_sampler;

#[path = "safe_state/mount_budget.rs"]
#[cfg(test)]
mod mount_budget;

#[path = "safe_state/readiness_gate.rs"]
#[cfg(test)]
mod readiness_gate;

#[path = "safe_state/timers_price_board.rs"]
#[cfg(test)]
mod timers_price_board;

fn stuck_req(coid: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 0,
        ..Default::default()
    }
}

/// A minimal fill for `coid` on the engine's own (venue, symbol), to drive the OMS FSM in tests.
fn fill_for(coid: &str) -> FillEvent {
    FillEvent {
        // minted by this helper — same `t-<coid>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", coid),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: Some(100.0),
        position_side: "BOTH".into(),
    }
}

/// Open (or resize/close) a BOTH-side position directly on an account — the same shortcut
/// `crates/vike-core/src/snapshot/snapshot_tests.rs`'s `engine_with_position` helper uses,
/// bypassing the full OMS FSM (the sampler only reads folded `account.positions` state, so driving
/// a real fill adds nothing but noise here). Setting `size` to `0.0` mirrors what a real close
/// leaves behind: the entry is NOT removed, only zeroed — see [`CoreThread::any_position_open`]'s
/// doc.
fn set_position(
    engine: &mut ExecutionEngine<RecordingClient>,
    venue: &str,
    symbol: &str,
    size: f64,
    avg_px: f64,
) {
    engine.account.positions.insert(
        (venue.into(), symbol.into(), "BOTH".into()),
        PositionEntry { size, avg_px, ..Default::default() },
    );
}

fn quote_tick() -> QuoteTick {
    QuoteTick {
        ts: 0,
        local_ts: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "BTCUSDT".into(),
    }
}
