//! vike-exec — the live-exec fold: orders, accounting, the pre-trade gate and reconciliation.
//!
//! Folds venue events into state; durable persistence is vike-core's journal. The module map:
//! - [`execution_engine`]: `ExecutionEngine`, the composition root (event fold, snapshot seed,
//!   reconcile apply, resolver-priced equity) and the [`ExecutionClient`] venue-adapter seam.
//! - [`order`]: the `OrderStatus` FSM + `ManagedOrder` (`apply` is the sole mutator).
//! - [`account`]: `Account`, the per-venue fill-fold read-model. Cross-venue aggregation is NOT a
//!   type here: vike-core's `CoreSnapshot` is the one aggregator (its `equity_total` folds each
//!   venue's [`account::Account::equity_all`] in registration order).
//! - [`risk`] / [`risk_profile`] / [`risk_surface`] / [`halt`]: the pre-trade `RiskGate`, the
//!   TOML `[risk]` converter and its source as data, and the halt-sentinel egress predicate.
//! - [`bus`]: defer-and-deliver FIFO (`EventBus` + `Outbox` handler seam); [`contingency`]: the
//!   one OTO/OCO resolver; [`engine_snapshot`]: the journal DTO; [`price_board`] /
//!   [`margin_call`]: priced reads and the margin-call model.
//! - [`recon`]: the normalized reconciliation core (diff, resolve, policy).
//! - [`lanes`]: the producer-side ingest lane types (`Command`/`Ingest` + the fire-and-forget
//!   senders venue adapters hold). The consumer, the single-writer core thread, lives in
//!   `vike-core`; this crate never depends on it.
//! - [`client_order_id`]: the coid generator, re-exported from vike-model.
//!
//! Crate-specific traps: `crates/vike-exec/CLAUDE.md`. The accounting golden is
//! `tests/parity/cross_venue_equity_parity.rs`.

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
/// is being light. See `vike_model::orders::client_order_id`'s module doc for the full argument.
pub use vike_model::orders::client_order_id;
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
// `risk_profile`'s own source as DATA, so the profile-schema exporter in vike-backtest can publish
// the `[risk]` table's keys through the dependency edge that already exists, rather than with an
// `include_str!` reaching into this package from outside it.
pub mod risk_surface;
pub mod route_key;

pub use account::{
    Account, BalanceMode, DEFAULT_MARK_STALENESS_MS, FillFold, MarkSource, PositionEntry,
    PositionKey,
};
pub use bus::{EventBus, EventHandler, Fold, Outbox};
pub use client_order_id::{ClientOrderIdGenerator, is_valid_crypto_coid};
pub use contingency::{ContingencyBook, is_protective_exit_sibling};
pub use engine_snapshot::{AccountSnapshot, EngineSnapshot, state_hash};
pub use execution_engine::{
    CancelIntent, EngineMode, ExecutionClient, ExecutionEngine, ReconcileSnapshot, StaleMark,
};
pub use lanes::{
    BarSeed, BarSender, BarSeries, BarUpdate, BookUpdate, Command, ConditionalIntent, CoreGone,
    EventSender, FlowUpdate, Ingest, MarginUpdate, MarketSender, MarketTick, MountSpec,
    OrderIntent, ParamsUpdate, QuoteUpdate, ReconcileReports, SeriesKey, StreamStatusUpdate,
    TickSender, TradeUpdate, event_channel, market_data_channel,
};

/// Test-support fakes — NEVER trade against these (the `event_channel()` convention).
/// The one real simulated venue is `vike_paper::PaperExecutionClient`; these exist
/// only so vike-exec's own tests can exercise the OMS plumbing without depending on
/// vike-paper (which sits above this crate).
///
/// Compiled only under `cfg(test)` or the `test-support` feature, which a consumer turns on as a
/// DEV-dependency feature — so a shipped binary cannot name these at all. The doc line above stays
/// as the second belt.
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    /// Records submits/cancels without emitting any venue events.
    pub use crate::execution_engine::RecordingClient;
    /// Fills instantly at the requested price — no fill model, no market data.
    pub use crate::execution_engine::TestExecutionClient;
}
pub use margin_call::{
    LiquidationIntent, MarginCall, MarginCallConfig, check_margin_call, check_margin_call_priced,
};
pub use order::{InvalidOrderTransition, ManagedOrder, OrderStatus};
pub use price_board::{
    MarkStatus, PriceCfg, PriceSource, Resolution, ResolvedEquity, ResolvedPosition,
};
pub use recon::{Divergence, LocalView, OwnedLocalState, Recon};
pub use risk::types::{
    PriceCollar, ResolvedGrid, RiskContext, RiskLimits, RiskVerdict, SymbolGrid,
};
pub use risk::{
    ImpactDeny, RiskGate, TakeScope, TradingState, clamp_leverage, fillable_veto, impact_veto,
    round_to, scoped_impact_veto, take_scope,
};
pub use risk_profile::{GridSource, ProfileError, ProfileRisk};
pub use route_key::RouteKey;
