//! vike-exec — the live-exec fold: orders, accounting, the pre-trade gate and reconciliation.
//!
//! Folds venue events into state; durable persistence is vike-core's journal. The module map:
//! - [`execution_engine`]: `ExecutionEngine`, the composition root (event fold, snapshot seed,
//!   reconcile apply, resolver-priced equity) and the [`ExecutionClient`] venue-adapter seam.
//! - [`order`]: the `OrderStatus` FSM + `ManagedOrder` (`apply` is the sole mutator).
//! - [`account`]: `Account`, the per-venue fill-fold read-model. The cross-venue view is a TYPE
//!   here but no fold: [`Portfolio`] inside [`CoreSnapshot`] (below) is plain data, and vike-core's
//!   `snapshot::build` fills it (its `equity_total` folds each venue's
//!   [`account::Account::equity_all`] in registration order).
//! - `read_model` (private; every type re-exported at this root): the published read model —
//!   [`CoreSnapshot`], [`Portfolio`], [`VenueBlock`], the row views and [`MountBudget`]. The live
//!   core builds one per publish; every reader names it as `vike_exec::X`, so reading what the
//!   core publishes does not link the core.
//! - [`risk`] / [`halt`]: the pre-trade `RiskGate` and the halt-sentinel egress predicate. The
//!   gate's CONFIGURATION (`RiskLimits`, the `[risk]` table's `ProfileRisk`) is vike-model's.
//! - [`bus`]: defer-and-deliver FIFO (`EventBus` + `Outbox` handler seam); [`contingency`]: the
//!   one OTO/OCO resolver; [`engine_snapshot`]: the journal DTO; [`price_board`] /
//!   [`margin_call`]: priced reads and the margin-call model.
//! - [`recon`]: the normalized reconciliation core (diff, resolve, policy).
//! - [`lanes`]: the producer-side ingest lane types (`Command`/`Ingest` + the fire-and-forget
//!   senders venue adapters hold). The consumer, the single-writer core thread, lives in
//!   `vike-core`; this crate never depends on it.
//! - The coid generator is `vike_model::orders::client_order_id`'s: it went down because
//!   `client_order_id` is a FIELD of `vike_model::OrderRequest`, and `vike-cli` pre-mints a coid
//!   without linking this crate. [`client_order_id_u64`] is the integer-keyed prototype twin.
//!
//! Crate-specific traps: `crates/vike-exec/CLAUDE.md`. The accounting golden is
//! `tests/parity/cross_venue_equity_parity.rs`.

pub mod account;
pub mod affinity;
pub mod bus;
pub mod client_order_id_u64; // strong-types prototype spike
pub mod contingency;
pub mod engine_snapshot;
pub mod execution_engine;
pub mod halt;
pub mod lanes;
pub mod margin_call;
pub mod order;
pub mod price_board;
// The published read model (what vike-core's `snapshot::build` fills on every publish). Private:
// its types are named from this crate's root only (`pub use read_model::{…}` below), one name each.
mod read_model;
pub mod recon;
pub mod risk;
pub mod route_key;

pub use account::{
    Account, BalanceMode, DEFAULT_MARK_STALENESS_MS, FillFold, MarkSource, PositionEntry,
    PositionKey,
};
pub use bus::{EventBus, EventHandler, Fold, Outbox};
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
pub use read_model::{
    CoreSnapshot, HeldOrderView, MountBudget, MountRowKind, MountView, OrderView, Portfolio,
    PositionView, ReconAlertView, ReconBlock, VenueBlock,
};
pub use recon::{Divergence, LocalView, OwnedLocalState, Recon};
pub use risk::types::{RiskContext, RiskVerdict};
pub use risk::{RiskGate, TradingState};
pub use route_key::RouteKey;
