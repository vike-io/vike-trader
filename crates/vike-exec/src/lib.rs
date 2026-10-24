//! vike-exec — the live-exec fold: orders, accounting, the pre-trade gate and reconciliation.
//!
//! Folds venue events into state; durable persistence is vike-core's journal. The module map:
//! - [`execution_engine`]: `ExecutionEngine`, the composition root (event fold, snapshot seed,
//!   reconcile apply, resolver-priced equity) and the [`ExecutionClient`] venue-adapter seam.
//! - The order state machine (`OrderStatus`, `ManagedOrder`) and `TradingState` are vocabulary and
//!   live in vike-model; the engine here is what folds events into them.
//! - [`account`]: `Account`, the per-venue fill-fold read-model. The cross-venue view is a TYPE
//!   here but no fold: [`Portfolio`] inside [`CoreSnapshot`] (below) is plain data, and vike-core's
//!   `snapshot::build` fills it (its `equity_total` folds each venue's
//!   [`account::Account::equity_all`] in registration order). [`RateBook`] (the private `cash`
//!   module) is the conversion book `Account::value_in` values the per-asset ledger with.
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
//!   without linking this crate.
//!
//! Crate-specific traps: `crates/vike-exec/CLAUDE.md`. The accounting golden is
//! `tests/parity/cross_venue_equity_parity.rs`.

pub mod account;
pub mod affinity;
pub mod bus;
// `RateBook`, the currency-conversion book `Account::value_in` values the per-asset ledger with.
// Private: named from this crate's root only (`pub use cash::RateBook` below), one name.
mod cash;
pub mod contingency;
pub mod engine_snapshot;
pub mod execution_engine;
pub mod halt;
pub mod lanes;
pub mod margin_call;
pub mod price_board;
// The published read model (what vike-core's `snapshot::build` fills on every publish). Private:
// its types are named from this crate's root only (`pub use read_model::{…}` below), one name each.
mod read_model;
pub mod recon;
pub mod risk;
pub mod route_key;

pub use account::{
    Account, BalanceMode, DEFAULT_MARK_STALENESS_MS, DEFAULT_QUOTE_RATE_TTL_MS, FillFold,
    MAX_QUOTE_RATE_STEP, MIN_QUOTE_RATE_CONFIRM_GAP_MS, MarkSource, PositionEntry, PositionKey,
    QUOTE_RATE_FUTURE_SKEW_MS,
};
pub use bus::{EventBus, EventHandler, Fold, Outbox};
pub use cash::RateBook;
pub use contingency::{ContingencyBook, is_protective_exit_sibling};
pub use engine_snapshot::{AccountSnapshot, EngineSnapshot, state_hash};
pub use execution_engine::{
    AdmitOutcome, CancelIntent, ExecutionClient, ExecutionEngine, MAX_EXTRA_SYMBOLS,
    ReconcileSnapshot, StaleMark,
};
pub use lanes::{
    AccountChange, BarSeed, BarSender, BarSeries, BarUpdate, BookUpdate, Command,
    ConditionalIntent, CoreGone, DataDelayUpdate, EventSender, ExecLinkUpdate, FlowUpdate,
    GatewayCursor, GatewayEvent, Ingest, MarginUpdate, MarketSender, MarketTick, MountSpec,
    OrderIntent, ParamsUpdate, QuoteUpdate, ReconcileReports, SeriesKey, StreamStatusUpdate,
    SymbolAdmission, TickSender, TradeUpdate, event_channel, market_data_channel,
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
    pub use crate::execution_engine::test_clients::RecordingClient;
    /// Fills instantly at the requested price — no fill model, no market data.
    pub use crate::execution_engine::test_clients::TestExecutionClient;
}
pub use margin_call::{LiquidationIntent, MarginCall, check_margin_call_priced};
pub use price_board::{MarkStatus, PriceCfg, PriceSource, ResolvedEquity, ResolvedPosition};
pub use read_model::{
    CoreSnapshot, GroupCurve, HeldOrderView, Interned, LedgerTotals, MountBudget, MountRowKind,
    MountView, NotRunningAccount, OrderView, Portfolio, PositionView, QuoteRateView,
    ReconAlertView, ReconBlock, SymbolList, VenueBlock,
};
pub use risk::RiskGate;
pub use risk::types::{RiskContext, RiskVerdict};
pub use route_key::RouteKey;
