//! `vike-strategy` — the strategy-authoring layer, extracted from `vike-model` so the bottom
//! domain crate stays pure data.
//!
//! Contains the position-executor STATE MACHINE ([`position_executor::PositionExecutor`] + its
//! barrier-evaluation helpers), the CONTROLLER framework ([`controller::Controller`],
//! [`controller::ControllerHarness`], [`controller::MomentumController`]), and the
//! funding-rate CARRY controller ([`FundingCarryController`] + its pure ranking /
//! entry / exit decision core). All are pure, deterministic, injected-clock logic — no I/O — so they
//! run bit-identically on the backtest and live engines.
//!
//! It also holds the portable REFERENCE STRATEGIES — [`Grid`] / [`DcaAccumulate`],
//! [`PairsZScore`], [`TrailingScalper`] and [`FundingCapture`], all under [`strategies`] and
//! named at this root — each a generic `impl<B: Broker> Strategy<B>`, so the SAME type runs in the
//! backtest sweep harness and live unchanged, and a consumer that wants the strategies never
//! compiles the engine. [`registry`] is the name -> strategy lookup, GENERIC over the broker
//! (`strategy_by_name<B: HftBroker>`) so ONE table serves `vike-backtest`'s `SimBroker` and
//! `vike-core`'s `LiveBroker`: the daemon runs what it backtested without linking the simulator.
//!
//! **Layering:** the NORMAL dependencies are `vike-model`, `vike-indicators` (for `pairs`'
//! half-life regime gate), `vike-mm` (the registry's two maker arms) and `libm`
//! (`strategies::fair_value::trailing_sigma`'s one transcendental) — all down-only. The
//! PARAM types these consume — [`vike_model::TripleBarrier`] and [`vike_model::ControllerParams`] —
//! deliberately stay in `vike-model` because they are transitively part of the `StrategyParams`
//! payload the `vike-exec` ingest lane carries, and `vike-exec` sits below any strategy layer.
//! The one edge UP to the simulator is a DEV edge: it does not propagate, so a consumer of the
//! strategies still never compiles the simulator.
//!
//! **Where the engine-driven tests live: HERE, beside their strategies.** A strategy's pure tests
//! (param readers, decision cores, barrier math) sit in their module. The tests that fold a
//! strategy through the REAL `StrategyEngine`/`SimBroker` sit in this crate's `tests/`, over that
//! DEV edge (`crates/vike-ops/tests/architecture/layer_gate.rs` walks NORMAL edges only). The
//! reversal: they used to live in `vike-backtest`'s `tests/`, where every strategy that moved down
//! here left its gate behind; `grid_dca_engine.rs` and `funding_capture_engine.rs` came back.

// The framework every strategy is built on.
pub mod controller;
pub mod position_executor;
pub mod registry;
// The concrete strategies, apart from the framework.
pub mod strategies;
// The one broker double the unit tests share.
#[cfg(test)]
pub(crate) mod test_support;

pub use controller::{Controller, ControllerHarness, MomentumController};
pub use position_executor::{
    BarrierHit, BarrierKind, EntryKind, ExecutorOutcome, ExecutorState, PositionExecutor,
    PositionIntent, RefreshMode, RefreshPolicy, RetryPolicy, evaluate_barriers,
    evaluate_barriers_at_price, stop_loss_price, take_profit_price, time_barrier_hit,
};
pub use registry::echo::resolved_params;
pub use registry::gates::{Gate, PARAM_GATES, param_gate, unarmable_params};
pub use registry::keys::{
    PARAM_KEYS, ParamKeys, ParamType, ParamTypeError, mistyped_params, param_keys, unknown_params,
};
pub use registry::routes::{
    PARAM_ROUTES, ParamRoutes, RouteKind, RouteMismatch, misrouted_params, param_routes,
};
pub use registry::{
    BuyHold, Capability, LIVE_CAPABLE, Liveness, PORTABLE_STRATEGIES, RegistryError, SCRIPT_ONLY,
    SIMULATOR_ONLY, capability, strategy_by_name,
};
pub use strategies::cheap_np::{CheapNp, CheapNpMode, CheapNpSignal, EntryPrice, TokenId};
pub use strategies::funding_capture::FundingCapture;
pub use strategies::funding_carry::{
    CarryCloseReason, FundingCarryController, FundingQuote, RankedCarry, best_carry_to_open,
    carry_leg_side, net_carry_edge, rank_best_carry, roundtrip_taker_cost, should_close_carry,
};
pub use strategies::grid_dca::{AnchorMode, DcaAccumulate, Grid};
pub use strategies::pairs::{
    PairsZScore, beta_neutral_qtys, rolling_mean_sd_last, should_enter, should_exit,
};
pub use strategies::sport_taker::{
    FiredEntry, Signal, SignalTracker, WalletBuy, flat_stake_pnl, market_symbol, signal_symbol,
};
pub use strategies::trailing_scalper::TrailingScalper;
