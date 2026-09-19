//! `vike-strategy` — the strategy-authoring layer, extracted from `vike-model` so the bottom
//! domain crate stays pure data.
//!
//! Contains the position-executor STATE MACHINE ([`position_executor::PositionExecutor`] + its
//! barrier-evaluation helpers), the CONTROLLER framework ([`controller::Controller`],
//! [`controller::ControllerHarness`], [`controller::MomentumController`]), and the
//! funding-rate CARRY controller ([`funding_carry::FundingCarryController`] + its pure ranking /
//! entry / exit decision core). All are pure, deterministic, injected-clock logic — no I/O — so they
//! run bit-identically on the backtest and live engines, which is exactly why they are
//! engine-agnostic over the `vike_model::Broker` / `Strategy` traits.
//!
//! It also holds the portable REFERENCE STRATEGIES — [`grid_dca::Grid`] /
//! [`grid_dca::DcaAccumulate`], [`pairs::PairsZScore`], [`trailing_scalper::TrailingScalper`] and
//! [`funding_capture::FundingCapture`] — each written as a generic `impl<B: Broker> Strategy<B>`
//! so the SAME type runs in the backtest sweep harness and live unchanged. They live here rather
//! than in `vike-backtest` for the reason this crate exists: a strategy is authored against the
//! `vike_model::Broker` / `Strategy` traits, never against the simulator, so a consumer that wants
//! the strategies should not have to compile the engine to get them. [`registry`] is the name ->
//! strategy lookup that resolves them, and it is GENERIC over the broker
//! (`strategy_by_name<B: HftBroker>`) precisely so the SAME table serves `vike-backtest`'s
//! `SimBroker` and `vike-core`'s `LiveBroker`. It used to live in `vike-backtest`, returning a
//! `Box<dyn Strategy<SimBroker>>` — a type only the simulator can name — which is why the headless
//! daemon could not run a strategy it had backtested without depending on the whole simulator.
//!
//! **Layering:** depends on `vike-model`, `vike-indicators` (for `pairs`' half-life regime gate)
//! and `vike-mm` (the registry's two maker arms) — down-only, and in particular NOT on
//! `vike-backtest`, which depends on this crate. The
//! PARAM types these consume — [`vike_model::TripleBarrier`] and [`vike_model::ControllerParams`] —
//! deliberately stay in `vike-model` because they are transitively part of the `StrategyParams`
//! payload the `vike-exec` ingest lane carries, and `vike-exec` sits below any strategy layer.
//! This crate imports them back down.
//!
//! **Where the engine-driven tests live:** a strategy's PURE tests (param readers, decision cores,
//! barrier math) stay beside their module here. The tests that fold a strategy through the REAL
//! `StrategyEngine`/`SimBroker` cannot — that would invert the dependency — so they live in
//! `vike-backtest`'s `tests/` as integration tests, alongside the existing `sport_taker_engine.rs`.

pub mod controller;
pub mod funding_capture;
pub mod funding_carry;
pub mod grid_dca;
pub mod pairs;
pub mod position_executor;
pub mod registry;
pub mod trailing_scalper;

pub use controller::{Controller, ControllerHarness, MomentumController};
pub use funding_capture::FundingCapture;
pub use funding_carry::{
    CarryCloseReason, FundingCarryController, FundingQuote, RankedCarry, best_carry_to_open,
    carry_leg_side, net_carry_edge, rank_best_carry, roundtrip_taker_cost, should_close_carry,
};
pub use grid_dca::{AnchorMode, DcaAccumulate, Grid};
pub use pairs::{PairsZScore, beta_neutral_qtys, rolling_mean_sd_last, should_enter, should_exit};
pub use position_executor::{
    BarrierHit, BarrierKind, EntryKind, ExecutorOutcome, ExecutorState, PositionExecutor,
    PositionIntent, RefreshMode, RefreshPolicy, RetryPolicy, evaluate_barriers,
    evaluate_barriers_at_price, stop_loss_price, take_profit_price, time_barrier_hit,
};
pub use registry::{
    BuyHold, Capability, Gate, LIVE_CAPABLE, PARAM_GATES, PARAM_KEYS, PARAM_ROUTES,
    PORTABLE_STRATEGIES, ParamKeys, ParamRoutes, ParamType, ParamTypeError, RegistryError,
    RouteKind, RouteMismatch, SCRIPT_ONLY, SIMULATOR_ONLY, capability, misrouted_params,
    mistyped_params, param_gate, param_keys, param_routes, resolved_params, strategy_by_name,
    unarmable_params, unknown_params,
};
pub use trailing_scalper::TrailingScalper;
