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
//! **Layering:** the NORMAL dependencies are `vike-model`, `vike-indicators` (for `pairs`'
//! half-life regime gate), `vike-mm` (the registry's two maker arms) and `libm`
//! (`fair_value::trailing_sigma`'s one transcendental) — all down-only. The
//! PARAM types these consume — [`vike_model::TripleBarrier`] and [`vike_model::ControllerParams`] —
//! deliberately stay in `vike-model` because they are transitively part of the `StrategyParams`
//! payload the `vike-exec` ingest lane carries, and `vike-exec` sits below any strategy layer.
//! This crate imports them back down.
//!
//! ⚠ This paragraph used to end "and in particular NOT on `vike-backtest`, which depends on this
//! crate." There IS an edge now — a DEV one, for the reason below — and the property that sentence
//! was protecting survives it intact: a dev-dependency does not propagate, so a consumer that
//! wants the strategies still does not compile the simulator to get them.
//!
//! **Where the engine-driven tests live: HERE, beside their strategies.** A strategy's pure tests
//! (param readers, decision cores, barrier math) sit in their module. The tests that fold a
//! strategy through the REAL `StrategyEngine`/`SimBroker` sit in this crate's `tests/`, reaching
//! the simulator through the upward DEV edge the manifest declares —
//! `crates/vike-ops/tests/layer_gate.rs` walks NORMAL edges only, and the shape is routine here
//! (`vike-bridge-core` dev-depends on ten venue bridges that normal-depend on IT).
//!
//! ⚠ This too is the REVERSE of what this paragraph said until the Polymarket takers landed: the
//! gates used to live in `vike-backtest`'s `tests/` on the argument that a strategy crate "cannot"
//! reach the engine. It can, and the old arrangement ACCUMULATED — every strategy that moved down
//! here left its gate behind, so the simulator collected gates for strategies it did not own.
//! `grid_dca_engine.rs` and `funding_capture_engine.rs` were two such orphans and came back with
//! the takers.

// The Polymarket up/down fair-value taker and its two leaves, arrived from vike-backtest. They
// are `impl<B: Broker> Strategy<B>` like every other strategy here; they lived in the simulator
// only because that is where they were first written.
pub mod cheap_np;
pub mod cheap_np_ask;
pub mod controller;
pub mod funding_capture;
pub mod funding_carry;
// The shared fair-value math the taker reads — `trailing_sigma` (the `libm::log` site this crate
// takes `libm` for) plus the cheap-band predicates. Its `p_up` is a re-export of `vike_model::p_up`
// and is deliberately NOT re-exported at this crate's root: that would mint a second name for a
// vike-model symbol.
pub mod fair_value;
pub mod grid_dca;
pub mod pairs;
pub mod position_executor;
pub mod registry;
// The Polymarket sports/esports copy-trading taker, arrived from vike-backtest with the above.
pub mod sport_taker;
pub mod trailing_scalper;

pub use cheap_np::{CheapNp, CheapNpMode, CheapNpSignal, EntryPrice, TokenId};
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
    BuyHold, Capability, Gate, LIVE_CAPABLE, Liveness, PARAM_GATES, PARAM_KEYS, PARAM_ROUTES,
    PORTABLE_STRATEGIES, ParamKeys, ParamRoutes, ParamType, ParamTypeError, RegistryError,
    RouteKind, RouteMismatch, SCRIPT_ONLY, SIMULATOR_ONLY, capability, misrouted_params,
    mistyped_params, param_gate, param_keys, param_routes, resolved_params, strategy_by_name,
    unarmable_params, unknown_params,
};
pub use sport_taker::{
    FiredEntry, Signal, SignalTracker, SportTaker, WalletBuy, flat_stake_pnl, market_symbol,
    signal_symbol,
};
pub use trailing_scalper::TrailingScalper;
