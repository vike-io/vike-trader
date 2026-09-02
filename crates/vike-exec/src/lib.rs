//! vike-exec — the vike trading core's live-exec ledger (R0 scope: Account).
//!
//! Exact ports (origin/main `023d6e8`) of:
//! - `exec/accounting.py::Account`  → [`account::Account`] (per-venue fill-fold read-model)
//!
//! Cross-venue account aggregation is NOT a type here: the live runtime's `CoreSnapshot`
//! (`vike-core`) is the one aggregator — its `equity_total` folds each venue's
//! [`account::Account::equity_all`] by the same `py_sum`-in-registration-order law. The former
//! `portfolio::Portfolio` / `cross_venue::CrossVenueDriver` read-models were dead shadows of it and
//! were removed; the cross-venue equity-summation golden is re-anchored onto `Account::equity_all`
//! in `tests/cross_venue_equity_parity.rs`.
//!
//! R5(a) adds the live-exec core logic:
//! - `exec/order.py`  → [`order`]: the OrderStatus FSM + `ManagedOrder` (apply sole-mutator)
//! - `exec/risk.py`   → [`risk`]: `RiskGate::check` (pre-trade, pure) + `clamp_leverage`
//! - `exec/coid.py`   → [`client_order_id`]: `ClientOrderIdGenerator` (`<8-hex-session><seq>`, venue-safe charset)
//! - `exec/bus.py`    → [`bus`]: defer-and-deliver FIFO (`EventBus` + `Outbox` handler seam)
//! - `exec/live_oms.py` → [`execution_engine`]: `ExecutionEngine` (triple-dedup event fold, snapshot seed)
//!
//! R5(b) adds the runtime around that logic (plan §1's pinned architecture):
//! - [`lanes`]: the producer-side ingest lane types (`Command`/`Ingest` + the fire-and-forget
//!   senders venue adapters hold — `EventSender`/`MarketSender`/`BarSender`/`TickSender`). The
//!   consumer (the single-writer core thread, `CoreSnapshot`, the R7 strategy mount) lives in
//!   `vike-core` (crate-reorg Phase 2, spec D3) — this crate never depends on it.
//!
//! Deliberately deferred out of R5 (explicit re-scopes, not omissions): `arm_spec`
//! (ExecArmSpec env-var resolution — lands with venue_config/credentials in R6, its only
//! consumers), the `order_router` seam (R7 with conditionals — `bus::Outbox` is its hook
//! point), live market data + chart migration (R5c), venue adapters (R6).

pub mod account;
pub mod affinity;
pub mod bus;
/// The coid generator, MOVED DOWN to `vike-model` and re-exported here verbatim so every historical
/// path (`vike_exec::ClientOrderIdGenerator`, `vike_exec::client_order_id::write_decimal`,
/// `vike_exec::is_valid_crypto_coid`) still resolves unchanged.
///
/// It went down because `client_order_id` is a FIELD of `vike_model::OrderRequest` and
/// `is_valid_crypto_coid` is a venue charset rule about that field — and, concretely, because
/// `vike-cli` must pre-mint a coid for a remote submit (the tradehub node refuses an empty one) and
/// linking THIS crate to get it would drag tokio + `core_affinity` into a CLI whose whole identity
/// is being light. See `vike_model::client_order_id`'s module doc for the full argument.
pub use vike_model::client_order_id;
pub mod client_order_id_u64; // strong-types prototype spike
pub mod contingency;
pub mod engine_snapshot;
pub mod execution_engine;
pub mod halt;
pub mod lanes;
pub mod margin_call;
pub mod order;
pub mod price_board;
pub mod recon;
pub mod risk;
pub mod risk_profile;
pub mod route_key;

pub use account::{
    Account, BalanceMode, FillFold, MarkSource, PositionEntry, PositionKey,
    DEFAULT_MARK_STALENESS_MS,
};
pub use bus::{EventBus, EventHandler, Fold, Outbox};
pub use client_order_id::{is_valid_crypto_coid, ClientOrderIdGenerator};
pub use contingency::{is_protective_exit_sibling, ContingencyBook};
pub use engine_snapshot::{state_hash, AccountSnapshot, EngineSnapshot};
pub use execution_engine::{
    CancelIntent, ExecutionClient, ExecutionEngine, ReconcileSnapshot, StaleMark,
};
pub use lanes::{
    event_channel, market_data_channel, BarSeed, BarSender, BarSeries, BarUpdate, BookUpdate,
    Command, ConditionalIntent, CoreGone, EventSender, FlowUpdate, Ingest, MarginUpdate,
    MarketSender, MarketTick, MountSpec, OrderIntent, ParamsUpdate, QuoteUpdate, ReconcileReports,
    SeriesKey, StreamStatusUpdate, TickSender, TradeUpdate,
};

/// Test-support fakes — NEVER trade against these (the `event_channel()` convention).
/// The one real simulated venue is `vike_backtest::paper::PaperExecutionClient`; these exist
/// only so vike-exec's own tests can exercise the OMS plumbing without depending on
/// vike-backtest (which sits above this crate).
pub mod testing {
    /// Records submits/cancels without emitting any venue events.
    pub use crate::execution_engine::RecordingClient;
    /// Fills instantly at the requested price — no fill model, no market data.
    pub use crate::execution_engine::TestExecutionClient;
}
pub use margin_call::{
    check_margin_call, check_margin_call_priced, LiquidationIntent, MarginCall, MarginCallConfig,
};
pub use order::{InvalidOrderTransition, ManagedOrder, OrderStatus};
pub use price_board::{
    MarkStatus, PriceCfg, PriceSource, Resolution, ResolvedEquity, ResolvedPosition,
};
pub use recon::{
    Divergence, DivergenceKind, LocalView, OwnedLocalState, Recon, ReconAlert, ReconMode,
    ReconPolicy,
};
pub use risk::{
    clamp_leverage, fillable_veto, impact_veto, round_to, scoped_impact_veto, take_scope,
    ImpactDeny, PriceCollar, ResolvedGrid, RiskContext, RiskGate, RiskLimits, RiskVerdict,
    SymbolGrid, TakeScope, TradingState,
};
pub use risk_profile::{GridSource, ProfileError, ProfileRisk};
pub use route_key::RouteKey;
