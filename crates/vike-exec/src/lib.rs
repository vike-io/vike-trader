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
/// The cancel-then-new AMEND state machine (pure): what to send next and how an amend ended.
pub mod amend;
pub mod bus;
// `RateBook`, the currency-conversion book `Account::value_in` values the per-asset ledger with.
// Private: named from this crate's root only (`pub use cash::RateBook` below), one name.
mod cash;
pub mod contingency;
pub mod engine_snapshot;
pub mod execution_engine;
pub mod halt;
// The bounded ingest-lane wait the test binaries share; named from `testing` only (one name).
#[cfg(any(test, feature = "test-support"))]
mod ingest_wait;
pub mod lanes;
pub mod margin_call;
pub mod price_board;
// The published read model (what vike-core's `snapshot::build` fills on every publish). Private:
// its types are named from this crate's root only (`pub use read_model::{…}` below), one name each.
mod read_model;
pub mod recon;
pub mod risk;
pub mod route_key;
// The event builders and finders the test binaries share; named from `testing` only (one name).
#[cfg(any(test, feature = "test-support"))]
mod test_events;

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
    NothingToReduce, ReconcileSnapshot, StaleMark, UNADMIT_QUIET_MS, UnadmitOutcome,
};
pub use lanes::{
    AccountChange, BarSeed, BarSender, BarSeries, BarUpdate, BookUpdate, Command,
    ConditionalIntent, CoreGone, DataDelayUpdate, EventSender, ExecLinkUpdate, FlowUpdate,
    GatewayCursor, GatewayEvent, Ingest, MarginUpdate, MarketSender, MarketTick, MountSpec,
    OrderIntent, ParamsUpdate, QuoteUpdate, ReconcileReports, RoutedOrder, SeriesKey,
    StreamStatusUpdate, SymbolAdmission, TickSender, TradeUpdate, event_channel,
    market_data_channel,
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
    /// The one `ExecutionEngine<RecordingClient>` builder this crate's own test binaries share.
    pub use crate::execution_engine::test_clients::EngineBuilder;
    /// Records submits/cancels without emitting any venue events.
    pub use crate::execution_engine::test_clients::RecordingClient;
    /// Fills instantly at the requested price — no fill model, no market data.
    pub use crate::execution_engine::test_clients::TestExecutionClient;
    /// A binance `BTCUSDT` market order, the reason a refused order was denied with, and the drain
    /// of a client's queued venue events through the engine's fold.
    pub use crate::execution_engine::test_clients::{denied_reason, market_order, pump};
    /// The default engine on a venue/symbol, and the same over a BOXED client, so engines of
    /// differing client types share one core.
    pub use crate::execution_engine::test_clients::{dyn_engine, engine_on};
    /// Drains an ingest lane until a picked event or a deadline — a live smoke's terminal wait.
    pub use crate::ingest_wait::await_event;
    /// Waits (bounded) for the next event on an ingest lane — the `exec_actor_*` binaries' helper.
    pub use crate::ingest_wait::recv_event;
    /// [`recv_event`] with the caller's bound — for a live venue whose round trips take seconds.
    pub use crate::ingest_wait::recv_event_within;
    /// Builders and finders: the resting limit order, a scripted accept, a cancel, the first
    /// reject, the first bare fill; and the seeded walk generator.
    pub use crate::test_events::{
        Lcg, accept_submit, bare_fill, canceled, rejection, resting_limit,
    };

    /// Fold `events` through `engine` in order, as the core would hand them over; what the folds
    /// put in the outbox is dropped.
    pub fn fold(
        engine: &mut crate::ExecutionEngine<RecordingClient>,
        events: &[vike_model::events::Event],
    ) {
        use crate::EventHandler;
        let mut outbox = crate::Outbox::default();
        for ev in events {
            engine.on_event(ev, &mut outbox);
        }
    }

    /// One frozen parity fixture, `dir/name` parsed as JSON. `dir` is the caller's (a
    /// `CARGO_MANIFEST_DIR`-relative path: `src/` may not bake one) and `exporter` is the deleted script
    /// that once wrote the set, named in the panic as PROVENANCE, not as an instruction: regeneration is
    /// not a supported operation (`fixtures/README.md`). vike-exec's r5 and vike-binance's r6 suites
    /// each carried the loader.
    pub fn frozen_fixture(dir: &std::path::Path, name: &str, exporter: &str) -> serde_json::Value {
        let path = dir.join(name);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            panic!(
                "{path:?} missing — it is COMMITTED and frozen; its exporter ({exporter}) was deleted \n                 with the Python app, so restore the file from git rather than regenerating it"
            )
        });
        serde_json::from_str(&text).unwrap()
    }

    /// A one-way (`Both`) position report of `qty` on `venue`/`symbol`, as a reconcile client returns
    /// one.
    ///
    /// DEFAULTS: `avg_px` `100.0`, `ts` `0`, the default margin mode, no isolated margin, no delta. A
    /// test whose fold reads the report's time or price states it with struct update over this.
    pub fn position_report(
        venue: &str,
        symbol: &str,
        qty: f64,
    ) -> vike_model::PositionStatusReport {
        vike_model::PositionStatusReport {
            venue: venue.into(),
            symbol: symbol.into(),
            position_side: vike_model::events::PositionSide::Both,
            qty,
            avg_px: 100.0,
            ts: 0,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        }
    }

    /// A one-way (`Both`) cross-margined position of `size` on `venue`/`symbol`, as the read model
    /// publishes it.
    ///
    /// DEFAULTS: `avg_px`, `unrealized`, `leverage` and `liq_price` all `0.0`, no mark source, no
    /// isolated margin. A test that reads one of those states it with struct update over this.
    pub fn position_view(venue: &str, symbol: &str, size: f64) -> crate::PositionView {
        crate::PositionView {
            venue: venue.into(),
            symbol: symbol.into(),
            position_side: vike_model::events::PositionSide::Both,
            size,
            avg_px: 0.0,
            unrealized: 0.0,
            mark_source: None,
            leverage: 0.0,
            liq_price: 0.0,
            margin_mode: vike_model::MarginMode::Cross,
            isolated_margin: None,
        }
    }
}
pub use margin_call::{LiquidationIntent, MarginCall, check_margin_call_priced};
pub use price_board::{MarkStatus, PriceCfg, PriceSource, ResolvedEquity, ResolvedPosition};
pub use read_model::{
    CoreHeldStopView, CoreSnapshot, GroupCurve, HeldOrderView, Interned, LedgerTotals, MountBudget,
    MountRowKind, MountView, NotRunningAccount, OrderView, Portfolio, PositionView, QuoteRateView,
    ReconAlertView, ReconBlock, SymbolList, VenueBlock,
};
pub use risk::RiskGate;
pub use risk::types::{RiskContext, RiskVerdict};
pub use route_key::RouteKey;
