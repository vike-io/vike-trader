//! Engine — the live composition root: account/gate/registry/client for ONE
//! (venue, symbol).
//!
//! `submit_order` runs the gate and publishes `OrderDenied` on veto, else calls `client.submit`.
//! `on_event` folds the venue stream: a bare `FillEvent` → `Account::apply_fill`, which is
//! IDEMPOTENT PER `trade_id` itself (the reconnect dedup lives on `Account`, see `account.rs`'s
//! module doc; this fold reads the [`crate::account::FillFold`] verdict back), admitted by mounted
//! symbol (`accepts_symbol`) or by order client id ownership (`owns_fill_symbol`: fills of orders THIS engine
//! manages fold even under never-mounted symbols, a combo's aggregate net-price print never does);
//! `Order*` lifecycle → the `OrderRecord` registry with the SEPARATE `seen_fsm_trade_ids` wrap
//! dedup; liquidation frames dedup via `seen_liquidation_ids`. The cancel-vs-fill race guard (LEAN
//! `CancelPendingOrders`): a venue-seeded PENDING_CANCEL order is restored to its pre-cancel live
//! status before a fill wrap / cancel-reject applies, so the FSM table never drops venue truth.
//! This engine folds state only: durable persistence is the runtime's command journal and its
//! materialized `exec_fill`/`execution_order` series.

use indexmap::IndexMap;
use std::collections::HashSet;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderDenied};
use vike_model::{OrderDenial, OrderRefusalId};

use crate::Account;
use crate::bus::EventBusOutbox;
use crate::risk::RiskGate;
use crate::risk::types::RiskContext;
use vike_model::OrderCancelIntent;
use vike_model::OrderRecord;
use vike_model::TradingState;

// Re-exported so `execution_engine::X` and the crate-root `vike_execution::X` are the paths imported.
mod client;
mod reconcile;
// The in-process doubles exist only under `cfg(test)` or the `test-support` feature (a DEV edge
// only): a `TestExecutionClient` fills every order instantly, so it must never ship in the
// order-signing daemon. `crates/vike-ops/tests/architecture/test_surface_gate.rs` refuses an
// ungated double and a shipped edge enabling the feature alike.
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_clients;

pub use client::{ExecutionClient, JournalCannotWrite, OrderSendFence, RestoredOrder};
pub use fold::FillDrop;
pub use funding::{FundingDue, FundingNote};
pub use order_registry::OrderRegistry;
pub use queries::{
    AdmitOutcome, MAX_EXTRA_SYMBOLS, NothingToReduce, UNADMIT_QUIET_MS, UnadmitOutcome,
    UnansweredCommand,
};
pub use reconcile::{AppliedFill, AppliedFx, OrderEventOut, ReconcileSnapshot};
pub use snapshot::{ENDED_ORDER_KEEP_MS, JOURNAL_CLOSED_PNLS};

mod fold;
mod funding;
mod order_registry;
mod queries;
mod reconcile_apply;
mod snapshot;
mod valuation;

/// One open position priced only off a STALE last-known value: a row of
/// [`Engine::stale_marks`] (`age_ms` = how far past the engine clock the value's
/// timestamp sits). NOT journaled — a live health signal derived from non-persisted market data,
/// like the `PriceBoard` it reads.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleMark {
    pub venue: String,
    pub symbol: String,
    pub position_side: vike_model::events::PositionSide,
    pub source: crate::price_board::PriceSource,
    pub age_ms: i64,
}

/// Reconcile drift tolerance: a position-size / balance divergence within
/// `DRIFT_ABS_TOLERANCE + DRIFT_REL_TOLERANCE * |venue|` is float noise (venue rounding, last-ulp wire
/// re-encode), NOT drift. The absolute floor covers values near zero.
const DRIFT_REL_TOLERANCE: f64 = 1e-6;
const DRIFT_ABS_TOLERANCE: f64 = 1e-8;

/// **The ONE absolute quantity tolerance**: two quantities within it are the same quantity. It is
/// `ReconcileLocalView::quantity_tolerance` for [`Engine::local_view`] and the "flat" width of `recon::resolve`'s
/// `unpriced_position_hold` (a venue qty within it is dust, the same line `diff` draws), and every other
/// absolute size comparison reads it rather than restating `1e-9`: vike-core's reconcile-stop cover, the
/// paper book's partial-or-whole verdict, the gatewayhub's session link and the bridges' fill and size
/// compares. A RELATIVE slack is another rule (`crate::emulated::DONE_EPS`).
pub const LOCAL_VIEW_QUANTITY_TOLERANCE: f64 = 1e-9;

/// The reason on the [`Event::OrderModifyRejected`] that `Engine::modify_order` publishes
/// for a venue whose amend convention is `AmendSemantics::Unsupported`, after the venue name and a
/// colon. A constant so one recogniser can match it, as `crate::halt::HALT_REJECT_REASON` is
/// matched for the halt refusal.
pub const MODIFY_UNSUPPORTED_REASON: &str =
    "this venue has no order modify; the order keeps its terms";

/// The reason on the [`Event::OrderRejected`] of a submit, and on the [`Event::OrderModifyRejected`] of an amend
/// (after the venue name and a colon), that the journal could not record ([`JournalCannotWrite`]): decision 0279's
/// addendum of 2026-11-23.
pub const JOURNAL_CANNOT_WRITE_REASON: &str = "NOT SENT: the command journal could not record this \
     order within 1 s (the filesystem stalled, or the disk is full, read-only or failing), so nothing \
     that can open risk leaves; cancels and orders that close a position still do";

/// The reason on the [`Event::OrderModifyRejected`] for an order whose lifetime nobody knows
/// (`vike_model::OrderRecord`'s `lifetime_unknown`), when the amend would send a lifetime: the registry's
/// default would replace the order's own in silence.
pub const MODIFY_LIFETIME_UNKNOWN_REASON: &str = "this order came back from the venue after a restart and \
     nothing remembers its lifetime, and this amend would send one, so it was NOT sent and the order keeps \
     its terms: cancel it and place the new terms";

/// The reason on the [`Event::OrderModifyRejected`] for a modify of a trigger this client cannot move
/// in place: `ExecutionClient::modify_trigger`'s default answer, after the venue name and a colon.
pub const TRIGGER_AMEND_UNSUPPORTED_REASON: &str = "this venue's adapter cannot move a resting \
     order's trigger in place, so nothing was sent and the order keeps its terms: cancel it and place \
     one at the new trigger";

/// The reason on the [`Event::OrderModifyRejected`] for a modify that names a trigger price the order
/// cannot take: its kind has no trigger, or its trigger has fired and it rests as the limit it released.
pub const TRIGGER_AMEND_NO_TRIGGER_REASON: &str = "this order has no trigger to move (its kind has \
     none, or its trigger has fired), so nothing was sent and the order keeps its terms";

/// The reason on the [`Event::OrderModifyRejected`] for a modify that names a `new_price` on a
/// market-if-touched (a take-profit resting with no limit). Its one price is its trigger, and a limit
/// landed on it (`vike_model::modified_prices`) would turn it into a limit-if-touched: on a
/// cancel-then-new venue the replacement could then be refused AFTER the original was cancelled.
pub const MIT_PRICE_AMEND_REASON: &str = "this market-if-touched order has no limit price to move, \
     so nothing was sent and the order keeps its terms: move its trigger with a new trigger price";

/// The reason on the [`Event::OrderModifyRejected`] for a modify of a trailing order
/// (`OrderType::is_trailing`). No adapter moves a resting trailing order in place, and a cancel-then-new
/// would restart its trail from the price at placement, so the stop could step back: a price, an
/// activation, a trail or an offset is never moved, and its size moves only on a client that declares
/// its own amend convention (the paper book, in place). The paper book refuses in these words too.
pub const TRAILING_AMEND_REASON: &str = "a resting trailing order is not changed in place, and placing \
     it again would restart its trail, so nothing was sent and the order keeps its terms: cancel it and \
     place a new one";

/// The reason on the [`Event::OrderModifyRejected`] for a modify that names a price or a trigger price
/// on an ADJUSTABLE stop (`OrderRequest::adjust`). Once its level is reached the order becomes its
/// target in place (`vike_model::apply_stop_adjust`: another stop, a stop-limit or a trailing stop),
/// so the price named has no one meaning, and a re-place would have to restate an adjustment the venue
/// may already have made. Its size moves only on a client that moves it in place (the paper book). The
/// paper book, this engine's modify and its cancel-then-new plan refuse in these words.
pub const ADJUSTABLE_AMEND_REASON: &str = "an adjustable stop's price is not changed: its adjustment \
     may already have made it another stop, a stop-limit or a trailing stop, so nothing was sent and \
     the order keeps its terms: cancel it and place a new one";

/// Is this modify of a trailing order refused ([`TRAILING_AMEND_REASON`])? Every one is, except a
/// size-only change on a client that moves it in place (`in_place`).
fn trailing_amend_refused(
    req: &OrderRequest,
    new_price: Option<f64>,
    new_trigger_price: Option<f64>,
    in_place: bool,
) -> bool {
    req.order_type.is_trailing()
        && (new_price.is_some() || new_trigger_price.is_some() || !in_place)
}

/// Is this modify of an ADJUSTABLE stop refused ([`ADJUSTABLE_AMEND_REASON`])? As a trailing order's:
/// every one is, before and after its adjustment, except a size-only change on a client that moves it
/// in place (`in_place`).
fn adjustable_amend_refused(
    req: &OrderRequest,
    new_price: Option<f64>,
    new_trigger_price: Option<f64>,
    in_place: bool,
) -> bool {
    req.adjust.is_some() && (new_price.is_some() || new_trigger_price.is_some() || !in_place)
}

/// Does this modify name a `new_price` on a market-if-touched ([`MIT_PRICE_AMEND_REASON`])?
fn mit_price_amend(req: &OrderRequest, new_price: Option<f64>) -> bool {
    req.order_type == vike_model::OrderType::TakeProfit
        && req.price.is_none()
        && new_price.is_some()
}

/// Has this resting order's trigger fired? `OrderTriggered`, or any fill (only a fired trigger order
/// has one): a stop-limit or a take-profit then rests as the plain limit it released.
fn trigger_fired(status: vike_model::OrderStatus, filled_qty: f64) -> bool {
    status == vike_model::OrderStatus::Triggered || filled_qty > 0.0
}

/// A modify's two prices in the CLIENT seam's terms, `(new_price, new_trigger_price)`. A stop's one price
/// is its trigger and rides `new_price`, as it always has (an explicit trigger wins,
/// `vike_model::modified_prices`), so a trigger reaches `ExecutionClient::modify_trigger` only on a
/// stop-limit or a take-profit. With no trigger named this is `(new_price, None)` for every kind. `None`
/// when a trigger price is named on an order with no trigger to move: its kind has none, or `fired`.
fn seam_prices(
    order_type: &vike_model::OrderType,
    fired: bool,
    new_price: Option<f64>,
    new_trigger_price: Option<f64>,
) -> Option<(Option<f64>, Option<f64>)> {
    if vike_model::modified_price_is_trigger(order_type) {
        return Some((new_trigger_price.or(new_price), None));
    }
    let (limit, trigger) = vike_model::modified_prices(order_type, new_price, new_trigger_price);
    if new_trigger_price.is_some() && (trigger.is_none() || fired) {
        return None;
    }
    Some((limit, trigger))
}

/// True when locally-folded `local` and venue-truth `venue` differ by more than the reconcile
/// drift tolerance — the single decision site for [`Engine::diff_snapshot`].
#[inline]
fn drift_diverges(local: f64, venue: f64) -> bool {
    (local - venue).abs() > DRIFT_ABS_TOLERANCE + DRIFT_REL_TOLERANCE * venue.abs()
}

/// Live composition root. Drive orders with `submit_order`; read `.account` / `.registry`.
pub struct Engine<C: ExecutionClient> {
    pub account: Account,
    /// Read-side price board: per-source price cells the resolver walks on COLD paths only. Fed
    /// alongside `account.set_mark` at the write sites; never read in the fold. Deliberately
    /// outside `EngineSnapshot` (journal hash-fence freeze).
    pub price_board: crate::price_board::PriceBoard,
    /// Resolver knobs for the ENGINE-INTERNAL decision sites (the pre-trade gate's
    /// `resolved_equity` / `resolved_margin_in_use_by` reads). The runtime sets it from
    /// `CoreConfig::price_config` at spawn, the source every vike-core caller passes, so one config
    /// governs every resolver read. Default permissive (mark enabled, no freshness windows). NOT
    /// serialized: config, re-imposed by the runtime on restore like `equity_seed`.
    pub price_config: crate::price_board::PriceBoardConfig,
    pub gate: RiskGate,
    pub client: C,
    /// The journal's send fence (decision 0279), run by [`Self::fence_send`] before every
    /// call that sends an order effect out through `client`. `None` (the default) waits for nothing.
    pub send_fence: Option<OrderSendFence>,
    /// The CANONICAL venue id (`"binance"`): the key every per-venue capability table is looked
    /// up under ([`vike_venues::capabilities_for`], [`vike_venues::amend_semantics`],
    /// [`vike_venues::fee_schedule_for`], `vike_venues::venue_time_in_force::venue_time_in_force`,
    /// [`fn@vike_venues::venue_margin_support`]), and the label of this engine's `Account` position
    /// keys, its registry's `OrderRequest::venue`s and its published `VenueBlock::venue`, so every
    /// self-comparison in this file reads THIS field, not [`Self::route_key`].
    ///
    /// ⚠ It must stay a roster id ([`vike_model::VENUES`]). Every one of those tables is
    /// fail-closed on a miss — `capabilities_for` answers `VenueCapabilities::UNSUPPORTED` (preflight rejects every
    /// order) and `amend_semantics` answers `AmendSemantics::Unknown`, which SILENTLY changes what
    /// a modify's quantity means. So an account-distinguishing suffix goes on [`Self::route_key`]:
    /// decorating THIS field is the `"binance#2"` trap, gated by
    /// `crates/vike-execution/tests/engine/route_key.rs`.
    pub venue: String,
    /// The ROUTING key: which ENGINE an inbound venue-tagged payload belongs to, unique per venue
    /// ACCOUNT rather than per venue. `vike_core`'s `CoreThread::engine_index_for_route_key` matches
    /// on this and nothing else.
    ///
    /// **Not a synonym of [`Self::venue`].** `venue` answers "what does this venue support" (shared
    /// by every account on it), `route_key` "which of this process's engines is this fill for". As
    /// one field, two accounts of one venue could not share a process: labelling both `"binance"`
    /// folded both accounts' fills into the first match's book (and reconcile's `PositionDrift`
    /// rewrote each onto the other's number), while `"binance#2"` broke every capability lookup.
    ///
    /// **[`Self::new`] sets it EQUAL to `venue`, and nothing in this workspace sets it otherwise**,
    /// so every routing decision is the single-field behaviour by construction.
    ///
    /// ⚠ If something ever does set it: `vike_boot::live_lock::LiveLock::acquire` uses it as a
    /// FILENAME component (`<state_dir>/LIVE-<route_key>.lock`), so it must be path-safe — no `/`,
    /// `\`, or `..`. Nothing validates it today because nothing sets it.
    pub route_key: String,
    pub symbol: String,
    /// **The tier of the account this engine runs** — and so what its orders go to, since an
    /// account runs at the tier its active row states or not at all (decision 0156): `paper` is a
    /// simulated book (no order leaves the process), `demo` the venue's demo/testnet/sandbox
    /// account, `live` a real-money one. Published per account as [`crate::VenueBlock::tier`].
    ///
    /// Set by the mount (`vike_tradehub_core`'s `assemble_engine` and `paper_engine`) through
    /// [`Self::set_tier`], the one place that builds an engine around a venue client. [`Self::new`]
    /// seeds `Paper` whatever client it is handed; nothing infers the tier from the client.
    /// [`Self::from_snapshot`] takes it from the snapshot when that carries one
    /// ([`crate::EngineSnapshot::tier`]) and seeds `Paper` otherwise.
    ///
    /// ⚠ **`Paper` is the SEED, not proof**: an engine that says `Paper` has not been shown to be a
    /// simulated book. A caller that builds an engine around a real client must set this itself.
    /// (`EngineMode`, the second paper/demo/live enum that once stood here to name a paper
    /// engine under a `live` account, went with decision 0156.) Only a tier that was SET (or that
    /// is not `Paper`) is written to the snapshot, so a boot restore can tell "this was a paper
    /// book" from "nobody said".
    pub tier: vike_model::AccountTier,
    /// Set by [`Self::set_tier`]: the mount TOLD this engine its tier. It decides one case alone,
    /// `Paper`: [`crate::EngineSnapshot::tier`] is written when the tier is anything but `Paper`, or
    /// when this is set, so an engine that was never told (the seed) writes nothing and keeps its
    /// canonical JSON, while a `paper` account's engine says so.
    tier_set: bool,
    /// The effective fee schedule the mount resolved for this engine: the LIVE account-actual rate
    /// when a `ReconcileClient::fetch_fee_rates` producer returned one, else the static
    /// [`vike_venues::fee_schedule_for`] default. Read-only, for cost display (the snapshot's
    /// `VenueBlock.fee_schedule`); it does NOT drive the fold — live fills report the venue's real
    /// commission and a paper engine's `ExecutionClientPaper` carries its own schedule. `None`
    /// until the mount sets it (a GUI-only / test engine never does).
    pub fee_schedule: Option<vike_model::FeeSchedule>,
    /// `Account::apply_account_state` quote-asset selector (default `DEFAULT_QUOTE_ASSET`, a guess).
    /// It is also THE ACCOUNT'S CURRENCY, the one every foreign-quoted symbol's rate is expressed in;
    /// read [`Self::account_currency_known`] before trusting it as that, and set it with
    /// [`Self::set_account_currency`], which also normalises the code.
    pub quote_asset: String,
    /// Set by [`Self::set_account_currency`]: the node TOLD this engine its currency. It is read only
    /// through [`Self::account_currency_known`], which also counts any `quote_asset` other than the
    /// constructor's guess as told, so the flag decides one case alone: an account whose real
    /// currency IS `"USDT"`. That is why it is the only part of this fact in `EngineSnapshot`
    /// (`account_currency_set`, written for that case only): the rest is `quote_asset`, which the
    /// snapshot always carried, so a replay takes the verdict the live engine took and no engine
    /// that was never told, or told another currency, changes a byte of its canonical JSON.
    account_currency_set: bool,
    /// Set by [`Self::set_account_currency_assumed`]: the currency the node told this engine ([`Self::set_account_currency`]) is
    /// only the ruling's ASSUMED USD (the venue stated none, or the read failed: decision 0189), not the venue's
    /// word. PRESENTATION only (`VenueBlock::currency_assumed`): nothing folds on it, so it is neither journaled nor in
    /// `EngineSnapshot`, and the assembly that told the currency tells this again at every start.
    account_currency_assumed: bool,
    /// Set by [`Self::set_native_time_release`]: this account's VENUE holds a TIME-only release itself (decision
    /// 0267's 2.6: IBKR's `goodAfterTime`). Configuration the assembly tells at every start, carried in
    /// [`crate::EngineSnapshot::native_time_release`] (not hashed) so a replay core rebuilt from a `Snapshot` decides
    /// a submit's release as the live engine did.
    native_time_release: bool,
    /// The 2-character ACCOUNT CODE this engine stamps into its order client ids (decision 0270: base36 of the
    /// account's vike.db `account.id`), set by the composition root; `OrderClientIdAccountCode::NONE` (`00`) until it is.
    /// Configuration, never read by the fold: carried in [`crate::EngineSnapshot::account_code`] (not hashed)
    /// so a replay core rebuilt from a `Snapshot` builds the minting plan the live engine had.
    pub account_code: vike_model::orders::client_id::OrderClientIdAccountCode,
    /// The LENGTH of the venue's configured broker code (Binance's `BINANCE_BROKER_CODE`; never the value), the
    /// input the engine's id shape is fitted to (decision 0270); `0` for none. Carried like
    /// [`Self::account_code`].
    pub broker_code_len: u8,
    /// **When this process started the account's link to its venue** (wall clock, ms): set by the composition
    /// root right before it asks its backend for the account's venue adapter (`vike_tradehub_core`'s
    /// `make_engine_for_account`, the boot's and a hot start's one path), so the link came up at or after it.
    /// `None` for an engine no venue link was started for (paper, a replay core's, a test's). Read by ONE cold step,
    /// the reconcile pass's booking of our own fills that landed while the node was down (`vike_core`'s
    /// `book_own_fills_filled_while_down`, decisions 0043 and 0120 as amended 2026-11-20), whose window ends at the
    /// earliest of these over the engines that may share the account's book ([`Self::venue_book`]). Configuration:
    /// never read by the fold, never in `EngineSnapshot` or `state_hash`, not journaled.
    pub venue_link_started_ms: Option<i64>,
    /// **The venue BOOK this engine's account trades**, as the arming table resolved it: the identity the
    /// shared-book warning compares (`vike_config::shared_books`), set by the composition root beside
    /// [`Self::venue_link_started_ms`]. `None` when it could not be told (a venue whose credentials name no account
    /// and no book was recorded) or no venue link was started. Read by the same cold step only: an engine of the same
    /// venue and tier on the same book, or on a book either side cannot tell, may fold the account's fills, so its
    /// link start bounds the window too. Configuration like [`Self::venue_link_started_ms`].
    pub venue_book: Option<vike_model::BookId>,
    /// [`Self::ledger_currency_key`]'s answer, kept so the publish interns nothing (it runs per event on a
    /// sporadic feed). Derived from `quote_asset` and `account_currency_set` by `refresh_ledger_currency`, which
    /// the three places that write them call: the constructor, [`Self::set_account_currency`] and
    /// [`Self::from_snapshot`]. ⚠ A direct write to `quote_asset` does not refresh it; say a currency
    /// through `set_account_currency`.
    ledger_currency: crate::Interned,
    /// perp: force reduce_only on submit_close flattens (read by the GUI ticket path)
    pub reduce_only_on_close: bool,
    /// Every order this engine holds, ended ones too (decision 0207), with the index of the live ones the
    /// per-publish and per-submit walks read ([`OrderRegistry::live`]).
    pub registry: OrderRegistry,
    pub trading_state: TradingState,
    /// wall-clock the runtime stamps before each dispatch (persistence `updated_ts`)
    pub now_ms: i64,
    /// When true (a REPLAY core's engines only: `vike_core`'s assembly sets it), every pre-trade decision that read
    /// data the command journal does not hold, a mark, a price or a book (`RiskGate::last_verdict_read_market_data`,
    /// the paper cash rule's prices, and the mark floor a snapshot-built engine never runs), files its order client
    /// id in [`Self::market_dependent`]: a replay may decide such an order otherwise than the live core did, so its
    /// execution reports are not regenerated (package 2's C.1b). Default false: the live engine pays one bool per
    /// order and files nothing.
    pub record_market_dependent: bool,
    /// The orders whose pre-trade decision read market data, while [`Self::record_market_dependent`].
    pub market_dependent: Vec<String>,
    /// When true (the runtime sets it iff a strategy is mounted), every fill the account fold
    /// ACCEPTS (post-dedup) is also buffered in `applied_fills` for `Strategy::on_fill`
    /// delivery. Capturing at the one `apply_fill` site means a WS reconnect replay can never
    /// double-fire the handler. Default false so a GUI-only engine never grows the buffer.
    pub collect_applied_fills: bool,
    /// fills accepted since the runtime last drained (see `collect_applied_fills`)
    pub applied_fills: Vec<AppliedFill>,
    /// NON-FILL order-lifecycle transitions (accept/reject/deny/cancel/expire) captured for
    /// `Strategy::on_order_event` delivery, gated by the SAME `collect_applied_fills` flag. Each
    /// carries the order's (venue, symbol) so the runtime routes it to the owning mount like
    /// `applied_fills`. Not serialized (deliveries are not replayed).
    pub order_events: Vec<OrderEventOut>,
    /// seed for the per-fill `equity_after` snapshot (the runtime's `seed_cash`)
    pub equity_seed: f64,
    /// **A COIN LEDGER's configured seed held in its settle coin** (decision 0258): the seed divided by the coin's
    /// price at the core's first risk pass (bar or clock) that can price it, whether or not the drawdown latch is
    /// armed ([`Self::hold_seed_in_coin`]), worth that coin at every later price. `None` until then and on every
    /// other engine. The latch's base and the sizing cap
    /// ([`Self::cap_at_coin_seed`]) read it. Core-local, NOT serialized: a restart converts the seed again.
    pub coin_seed: Option<f64>,
    /// Multi-symbol opt-in (RUST-NATIVE): additional symbols this engine accepts venue events for
    /// (fills/funding/liquidations fold into the one multi-symbol Account). Empty (default) = the
    /// single-symbol filter.
    pub extra_symbols: Vec<String>,
    /// [`Self::extra_symbols`] as the read model publishes it: the last list [`Self::published_extra_symbols`]
    /// was asked for, kept so a publish that finds the symbols unchanged shares it instead of copying
    /// every name. A cache of a public field that tests and builders assign directly, so it holds no
    /// state of its own: it is compared with the field on every read, never trusted, and it is outside
    /// `EngineSnapshot`. A `Mutex` only so the engine stays `Sync`; one thread reads it.
    extra_symbols_published: std::sync::Mutex<crate::SymbolList>,
    // There is no `seen_trade_ids` field: the bare-`Event::Fill` dedup ledger is `Account`'s own
    // set, consulted inside `Account::apply_fill`, so no path can bypass it and no second copy can
    // disagree; `Self::seen_trade_ids()` reads it through for the snapshot/`local_view`
    // (`EngineSnapshot::seen_trade_ids` on the wire). The two sets below guard DIFFERENT
    // aggregates: the `OrderRecord` FSM's `filled_qty`/`average_fill_price`, and the liquidation lane.
    // The FSM set maps each id to the engine day it was folded on, what the journal `Snapshot` windows it
    // by (`crate::account::fill_window`); the liquidation set is one id per liquidation and rides whole.
    // The FSM set grows by one id per fill wrap, like the account's, so it is the same
    // `TradeIdLedger`: its growth never rehashes every id on the core thread.
    seen_fsm_trade_ids: crate::account::trade_id_ledger::TradeIdLedger<u32>,
    /// How many days of fold [`Self::journal_snapshot`] keeps of the two trade-id sets: sized to the node's
    /// reconcile read-back by [`Self::size_fill_window`], [`crate::account::fill_window::WIDEST_WINDOW_DAYS`]
    /// until then. Configuration, carried in [`crate::EngineSnapshot::fill_window_days`] (not hashed) so a replay
    /// core keeps what the live engine kept; a boot restore leaves the mount's.
    fill_window_days: u32,
    seen_liquidation_ids: HashSet<String>,
    /// How many ENDED orders the snapshot this engine was rebuilt from left out of its registry
    /// ([`crate::EngineSnapshot::ended_orders_left_out`]), so the next [`Self::journal_snapshot`] counts
    /// them too. `0` on every engine `new` built, and after `restore_runtime_state` (which restores no
    /// registry).
    ended_orders_left_out: u64,
    /// Count of terminal events that failed to apply while the order was still LIVE (a genuinely
    /// lost terminal — the order may be stranded). Benign idempotent replays on already-terminal
    /// orders are NOT counted. Read by monitoring/tests; never affects the fold.
    pub closing_events_dropped_order_still_live: u64,
    /// Count of lifecycle events whose order client id was not in the registry (pre-restart order absent from
    /// reconcile, external-account order, or a bug). The event is still dropped — this only makes
    /// the drop visible.
    pub events_dropped_order_unknown: u64,
    /// Count of venue LIVENESS/EXECUTION events (`OrderAccepted`/`OrderPartiallyFilled`/
    /// `OrderFilled`) dropped because the order was ALREADY terminalized via a KILL path
    /// (`Rejected`/`Canceled`/`Expired`/`Denied`): the signature of a premature terminal
    /// (classically the watchdog's stage-2 phantom-reject racing a slow venue confirm) leaving a
    /// real venue position STRANDED. `closing_events_dropped_order_still_live` misses it (its
    /// `!status_after.is_terminal()` guard is false once the order is terminal). Benign same-state
    /// terminal replays are NOT counted. Observability only: never changes WHAT is dropped. Not
    /// serialized (a live health signal, like `events_dropped_order_unknown`).
    pub events_dropped_order_already_closed: u64,
    /// HOSTILE-VENUE observability: count of money-lane events (`Fill`/`Funding`/`AccountState`/
    /// `PositionLiquidated`) refused because a folded f64 was NOT FINITE — see
    /// [`vike_model::FiniteNumbers`] for how `"NaN"` reaches a typed field off the wire, and why
    /// one such value poisons the ledger IRRECOVERABLY.
    ///
    /// **Never routine**: the other drop counters have benign explanations, a non-finite venue
    /// number has NONE, so the fold logs it at ERROR. Never affects anything but the one event it
    /// rejects.
    pub events_dropped_number_not_finite: u64,
    /// WHY a registry order was refused: the `reason` of the `OrderRejected` (or `OrderDenied`)
    /// that terminalized it and that event's `refusal_id`, keyed by order client id, for the published
    /// `OrderView::reason` and `OrderView::refusal_id`.
    /// The FSM keeps no reason (`OrderRecord` carries the request and the lifecycle state only),
    /// and the reason otherwise lives only in the event journal. A SIDE TABLE on purpose: it is
    /// not in `EngineSnapshot`, `state_hash` or the journal's `Snapshot`, so nothing recorded moves;
    /// a restored order is therefore listed without a reason. Bounded at
    /// `MAX_REJECTION_REASONS`, oldest evicted. A gate veto never enters the registry (it has
    /// no `OrderView` row), so its reason is not here. Written only on a rejection, never on the
    /// per-message path.
    rejection_reasons: IndexMap<String, (String, Option<String>)>,
    /// The LIMIT price of the MODIFY this engine sent to the venue on each order and the venue has not
    /// answered yet (`OrderModified` / `OrderModifyRejected` forget it), with the engine clock of the
    /// send, keyed by order client id, for the published `OrderView::pending_price`. The registry keeps
    /// the resting terms until the venue confirms, and the published snapshot is coalesced on top of
    /// that, so a reader that sizes a qty-only modify (the control door) would otherwise price it at a
    /// stale limit. A SIDE TABLE on purpose, like `rejection_reasons`: not in `EngineSnapshot`,
    /// `state_hash` or the journal's `Snapshot` (a restart has no modify in flight). Written only by
    /// `modify_order` and cleared on the two modify answers, never on the per-message path; bounded at
    /// `MAX_PENDING_MODIFIES` (oldest evicted) and read only for `PENDING_MODIFY_TTL_MS`, so an answer
    /// that never comes cannot leave a price behind for good (the next priced modify also sweeps the
    /// entries past it and those of orders that ended unanswered). Only a modify of the LIMIT price is
    /// kept (`vike_model::modified_price_is_trigger`): a stop's modify moves its trigger, which the
    /// view's `price` does not carry.
    pending_modify_price: IndexMap<String, (f64, i64)>,
    /// The engine clock of the last MODIFY this engine sent on each order, or of the last modify
    /// ANSWER it folded (`OrderModified`, `OrderModifyRejected`), whichever is later, keyed by client
    /// order id: what a reconcile pass asks before it compares the order's terms with the venue's row
    /// ([`Self::amend_settling`]). A row read before the venue took an amend, or before its answer
    /// folded here, disagrees with the book for a moment and is no drift. A SIDE TABLE like
    /// `pending_modify_price` (not in `EngineSnapshot`, `state_hash` or the `Snapshot`), written only by a
    /// modify send and the two answers' cold fold arm, never on the per-message path; bounded at
    /// `MAX_PENDING_MODIFIES` (oldest evicted) and swept of entries past `AMEND_SETTLE_MS` at each write.
    amend_marks: IndexMap<String, i64>,
    /// The engine clock of the first CANCEL this engine sent on each order that the venue has not answered
    /// (`OrderCanceled` / `OrderCancelRejected` forget it), keyed by order client id: what the core's watchdog
    /// rung for unanswered commands reads (`vike_core`'s `sweep_unanswered_commands`, which also forgets an
    /// order that ended another way, so the fold's fill arms stay untouched). A SIDE TABLE like
    /// `pending_modify_price` (not in `EngineSnapshot`, `state_hash` or the journal's `Snapshot`: a restart
    /// has no cancel in flight), written only by a cancel send and the cold answer arms of the fold; a cancel
    /// sent while `MAX_UNANSWERED_COMMANDS` are unanswered is not noted.
    unanswered_cancels: IndexMap<String, i64>,
    /// [`Self::unanswered_cancels`] for a MODIFY (`OrderModified` / `OrderModifyRejected` forget it).
    unanswered_modifies: IndexMap<String, i64>,
    /// The VENUE's own net position per symbol, as the last reconcile pass for this account
    /// reported it (`vike_core`'s `reconcile_reports` replaces the whole map each pass; empty until
    /// the first). It is EVIDENCE for one decision only: [`Self::cover_position_size`], which lets a
    /// reduce-only order close a position the venue holds and this book has not folded (reconcile
    /// HOLDS it under `quarantine`) instead of being refused as an opening order by the
    /// account-exposure lane. Never read by the fold, never in `EngineSnapshot` or `state_hash`; a
    /// replay rebuilds it from the journaled `ReconcileReports`.
    pub venue_reported_positions: std::collections::HashMap<String, f64>,
    /// The venue's own "nothing to reduce" answers to this engine's reduce-only closes, waiting for
    /// the core's cold sweep to take them (`Engine::take_nothing_to_reduce`). Written only
    /// on a typed rejection (`RejectCause::NothingToReduce`), from the same cold `OrderRejected` arm
    /// of the lifecycle fold that records `rejection_reasons`; never on the per-message path. Not in
    /// `EngineSnapshot`, `state_hash` or the journal's `Snapshot` (the claim is re-derived by the next
    /// refused close), so nothing recorded moves.
    nothing_to_reduce: Vec<NothingToReduce>,
    /// The fills the account refused as UNPROVEN (`crate::account::FillFold::Unproven`: no stamp, an id the
    /// windowed ledger does not hold), waiting for the core to hold them for the operator
    /// ([`Self::take_unproven_fills`]). Written only by the cold refusal of the `Fill` arm, never on an
    /// ordinary fill; not in `EngineSnapshot`, `state_hash` or the journal's `Snapshot`, like `nothing_to_reduce`
    /// (a refold refuses the same fill and parks it again).
    unproven_fills: Vec<vike_model::events::FillEvent>,
    /// **The route key of the engine whose book shares this engine's ACCOUNT-exposure ceiling**, or
    /// `None` (every engine but one): the Binance lane twin names its parent (the bare venue id), the
    /// two engines being one row of the account table over one set of keys and one stated
    /// `max_exposure`. The ceiling is armed per engine, so each of the two judged only its own book
    /// and the row's combined bound was twice the figure (decision 0184). Configuration, set by the
    /// mount that builds the twin (`vike_tradehub_core`'s `assemble.rs`) and read by ONE cold timer
    /// of the core (`vike_core`'s `refresh_exposure_pairs`), which resolves the partner by this key and
    /// hands each of the two the other's exposure as [`Self::sibling_exposure`]. Never read by the
    /// fold, never in `EngineSnapshot` or `state_hash`, not journaled.
    pub exposure_partner: Option<String>,
    /// **The partner engine's gross exposure** (positions and live un-filled orders, as the account
    /// ceiling counts them: [`Self::resolved_account_exposure_excluding`] over its whole book), as the
    /// core last handed it over (about once a second), and `0.0` for an engine with no partner or
    /// before the first hand-over. The engine's `risk_context` adds it to the exposure the ceiling counts,
    /// inside the branch that runs only when the ceiling is armed, so the row is bounded by ONE
    /// figure. Only a finite number is ever written (a partner book the core cannot value leaves the
    /// last finite figure standing: a pair that cannot be valued is no worse than the 2x it
    /// replaces). Configuration-like state: never in `EngineSnapshot` or `state_hash`, not journaled
    /// (a replay has no marks either, so a replayed verdict that counted it is the class of every
    /// mark-dependent cap).
    pub sibling_exposure: f64,
    /// The order client id of the order the submit in progress REPLACES: `vike_core`'s amend machine
    /// (`crates/vike-core/src/runtime/apply/amend.rs`'s `place_replacement`) sets it around the
    /// replacement's submit and clears it after. An amend placed new-first (a reduce-only stop on a
    /// cancel-then-new venue) has both orders live for a moment, so the spot rule
    /// (`Engine::cash_oversell_denial`) leaves this one out of the sells already out. Transient: never in
    /// `EngineSnapshot` or `state_hash`, not journaled (a replay re-runs the same amend code).
    pub replacing: Option<String>,
    /// The other legs of the linked group (one order list or bracket) of the order the submit in progress
    /// places: `vike_core`'s contingency drive sets it around a linked leg's submit and clears it after. The
    /// spot rule (`Engine::cash_oversell_denial`) counts the group at its largest single claim, not
    /// the sum of its legs (a ladder's stop and its targets each cover the position, and an OUO fill takes
    /// from the others what it fills). Transient as [`Self::replacing`].
    pub linked_exits: Option<LinkedExits>,
    /// A PAPER engine's funding clocks, one per perpetual the core told it a funding rate for
    /// ([`Self::note_funding_rate`]); empty on every other engine. Snapshotted, NOT hashed
    /// (`crate::EngineSnapshot::funding_clocks`). Never read or written by the per-message fold: the
    /// funding-rate lane writes it, and only the cold `Funding` arm of the fold advances its
    /// `settled_timestamp`.
    funding_clocks: Vec<crate::FundingClock>,
}

/// The linked group of the order being placed ([`Engine::linked_exits`]): its legs in SETS, each
/// set's legs summing (a ladder's targets each hold a part) and the sets ALTERNATIVES to one another (the
/// stop, or the targets: whichever fills first takes the other's size), so the group claims the largest
/// set. `own` is the set the order being placed belongs to; the other order client ids are its live legs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct LinkedExits {
    pub sets: Vec<Vec<String>>,
    pub own: usize,
}

/// The bound on `Engine::rejection_reasons`: the newest this many refusals keep their
/// reason.
const MAX_REJECTION_REASONS: usize = 1024;

/// The bound on `Engine::pending_modify_price`: the newest this many modifies in flight keep
/// their price.
const MAX_PENDING_MODIFIES: usize = 256;

/// The bound on `Engine::unanswered_cancels` and on `Engine::unanswered_modifies`: a command sent while this
/// many of its kind are unanswered is not watched (nothing is evicted, so a watched one is never forgotten).
const MAX_UNANSWERED_COMMANDS: usize = 4096;

/// How long (engine clock) a modify's price is published as pending when the venue has not answered:
/// far over any venue's amend round trip and the publisher's coalescing, short enough that a lost
/// answer does not leave a stale price on the order for long.
pub(crate) const PENDING_MODIFY_TTL_MS: i64 = 30_000;

/// How long (engine clock) after a modify was sent or answered a reconcile pass leaves the order's
/// terms alone ([`Engine::amend_settling`]): one pass at the 60 s reconcile interval default.
/// A pass reads the venue's rows before it reaches the fold, so a row it compares may predate an amend
/// answered since the previous pass; a drift that outlives this is the next pass's to raise.
pub const AMEND_SETTLE_MS: i64 = 60_000;

/// What [`Engine::quote_asset`] is until somebody sets it: the constructor's guess, not a
/// statement about any account (`Engine::account_currency_known`). `pub` so a reader that asks "is this
/// label still the guess?" (vike-tradehub-core's restored engines) reads this one spelling.
pub const DEFAULT_QUOTE_ASSET: &str = "USDT";

impl<C: ExecutionClient> Engine<C> {
    /// Wait on [`Self::send_fence`], if any (decision 0279). Every
    /// call in this crate that sends an order effect out through `client` comes right after one, and
    /// a caller outside it that calls `client` directly calls this first. `Err` when the journal could not
    /// record the effect: the caller sends it only if it cannot open risk (decision 0279's addendum of
    /// 2026-11-23: a cancel, a confirm, [`Self::leaves_unrecorded`]).
    #[inline]
    pub fn fence_send(&self) -> Result<(), JournalCannotWrite> {
        match &self.send_fence {
            Some(f) => f.wait(),
            None => Ok(()),
        }
    }

    /// Whether `request` may leave although the journal could not record it: only an exit the position COVERS
    /// (`vike_model::is_covered_reduce` on the coverage the gate judges it by), the one order a halt admits, so a
    /// journal outage never traps a position. Anything else is refused NOT SENT ([`JOURNAL_CANNOT_WRITE_REASON`]).
    pub fn leaves_unrecorded(&self, request: &OrderRequest) -> bool {
        let position = self.cover_position_size(
            &request.symbol,
            request.reduce_only,
            request.side,
            &request.order_client_id,
        );
        vike_model::is_covered_reduce(request.side, position, request.qty)
    }

    /// End a registered order the journal could not record, NOT SENT: a terminal [`Event::OrderRejected`] with
    /// [`JOURNAL_CANNOT_WRITE_REASON`] into `outbox`, which the core folds like any refusal. Cold: a journal outage.
    #[cold]
    #[inline(never)]
    fn refuse_unrecorded(&self, request: &OrderRequest, now_ms: i64, outbox: &mut EventBusOutbox) {
        tracing::warn!(target: "vike_execution::oms", order_client_id = %request.order_client_id,
            "order NOT SENT: the journal could not record it");
        outbox.publish(Event::OrderRejected(vike_model::events::OrderRejected {
            order_client_id: request.order_client_id.clone(),
            reason: JOURNAL_CANNOT_WRITE_REASON.into(),
            timestamp: now_ms,
            cause: None,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::JournalCannotWrite.word(),
            ),
        }));
    }

    /// `venue` seeds BOTH [`Self::venue`] and [`Self::route_key`]: a fresh engine is the
    /// single-account shape. A second account sets [`Self::route_key`] afterwards (kept off the
    /// signature so the many construction sites need not pass one).
    ///
    /// An engine MOUNTED on an inverse contract keeps its account's books in the contract's settle coin
    /// ([`Account::set_coin_books`], decision 0258).
    pub fn new(mut account: Account, gate: RiskGate, client: C, venue: &str, symbol: &str) -> Self {
        account.set_coin_books(symbol);
        Engine {
            account,
            gate,
            client,
            send_fence: None,
            venue: venue.to_string(),
            route_key: venue.to_string(),
            symbol: symbol.to_string(),
            tier: vike_model::AccountTier::Paper,
            tier_set: false,
            fee_schedule: None,
            quote_asset: DEFAULT_QUOTE_ASSET.to_string(),
            account_currency_set: false,
            account_currency_assumed: false,
            native_time_release: false,
            account_code: vike_model::orders::client_id::OrderClientIdAccountCode::NONE,
            broker_code_len: 0,
            venue_link_started_ms: None,
            venue_book: None,
            ledger_currency: crate::Interned::default(),
            reduce_only_on_close: false,
            registry: OrderRegistry::default(),
            trading_state: TradingState::Active,
            price_board: crate::price_board::PriceBoard::default(),
            price_config: crate::price_board::PriceBoardConfig::default(),
            now_ms: 0,
            record_market_dependent: false,
            market_dependent: Vec::new(),
            collect_applied_fills: false,
            applied_fills: Vec::new(),
            order_events: Vec::new(),
            equity_seed: 0.0,
            coin_seed: None,
            extra_symbols: Vec::new(),
            extra_symbols_published: std::sync::Mutex::default(),
            seen_fsm_trade_ids: Default::default(),
            fill_window_days: crate::account::fill_window::WIDEST_WINDOW_DAYS,
            seen_liquidation_ids: HashSet::new(),
            ended_orders_left_out: 0,
            closing_events_dropped_order_still_live: 0,
            events_dropped_order_unknown: 0,
            events_dropped_order_already_closed: 0,
            events_dropped_number_not_finite: 0,
            rejection_reasons: IndexMap::new(),
            pending_modify_price: IndexMap::new(),
            amend_marks: IndexMap::new(),
            unanswered_cancels: IndexMap::new(),
            unanswered_modifies: IndexMap::new(),
            venue_reported_positions: std::collections::HashMap::new(),
            nothing_to_reduce: Vec::new(),
            unproven_fills: Vec::new(),
            exposure_partner: None,
            sibling_exposure: 0.0,
            replacing: None,
            linked_exits: None,
            funding_clocks: Vec::new(),
        }
    }

    /// Hand the core the fills the account refused as unproven since the last take (`vike_core`'s
    /// `hold_unproven_fills` holds each as a reconcile row; none is ever booked from here).
    pub fn take_unproven_fills(&mut self) -> Vec<vike_model::events::FillEvent> {
        std::mem::take(&mut self.unproven_fills)
    }

    /// Cold-start dedup seed from a durable source (e.g. the replayed command journal), applied
    /// once at startup. Delegates to [`Account::seed_seen_fill_ids`]: the same set
    /// `Account::apply_fill` consults, not a copy.
    pub fn seed_seen_trade_ids<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.account.seed_seen_fill_ids(ids);
    }

    /// The bare-fill dedup ledger, read-only, THROUGH to [`Account::seen_fill_ids`] (no copy, so
    /// this reader and the guard in `Account::apply_fill` cannot diverge). `pub` for the reconcile
    /// `ReconcileLocalView`, the `EngineSnapshot` wire and the journal cross-check.
    pub fn seen_trade_ids(&self) -> impl Iterator<Item = &str> {
        self.account.seen_fill_ids()
    }

    /// Gate the order; publish OrderDenied on veto or submit to the venue. `now_ms` is the injected
    /// clock. [`Self::gate_verdict`] then [`Self::submit_judged`]: a caller that must act between the
    /// verdict and the send (the core's coalesced denials) calls the two itself.
    pub fn submit_order(
        &mut self,
        request: &OrderRequest,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) {
        let verdict = self.gate_verdict(request, now_ms);
        self.submit_judged(request, verdict, true, now_ms, outbox);
    }

    /// [`Self::submit_order`] past its gate: register and send what `verdict` (this engine's
    /// [`Self::gate_verdict`] on `request`, nothing changed since) admitted, or publish its denial.
    /// `recorded` is whether the caller's write-ahead records of the order reached the journal: an
    /// admitted order they did not is refused NOT SENT as one the fence refuses is. Coarse per-ORDER
    /// span (NOT per-message): `skip_all` keeps the hot path free of arg formatting; only the order
    /// client id is recorded. No subscriber = a cheap no-op (the latency gate).
    #[tracing::instrument(level = "info", target = "vike_execution::oms", skip_all, fields(order_client_id = %request.order_client_id))]
    pub fn submit_judged(
        &mut self,
        request: &OrderRequest,
        verdict: crate::RiskVerdict,
        recorded: bool,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) {
        if let Some(req) = self.register_or_deny(request, verdict, now_ms, outbox) {
            if (recorded && self.fence_send().is_ok()) || self.leaves_unrecorded(&req) {
                self.client.submit(&req);
            } else {
                self.refuse_unrecorded(&req, now_ms, outbox);
            }
        }
    }

    /// The pre-trade gate's verdict on `request` judged WHOLE, registering nothing, publishing nothing
    /// and taking no rate-limit slot (`RiskGate::check_modify`'s lanes over an order with nothing
    /// executed): an EMULATED parent (decision 0275) at accept, so a parent over a per-order ceiling is
    /// refused whole and never passes slice by slice. Each child meets the full gate again when it is
    /// sent. `Err` carries the gate's denial: its lane and its words. Per accepted emulated order, never on the fold.
    pub fn judge_whole(
        &mut self,
        request: &OrderRequest,
        now_ms: i64,
    ) -> Result<(), (Option<OrderDenial>, String)> {
        let (early, overbuy_ran) = self.early_denial(request);
        if let Some(verdict) = early {
            if self.record_market_dependent {
                self.note_market_dependent(request, overbuy_ran, false);
            }
            return Err((verdict.denial, verdict.reason));
        }
        let ctx = self.risk_context(request, now_ms);
        let verdict = self.gate.check_modify(request, &ctx, 0.0);
        if self.record_market_dependent {
            self.note_market_dependent(request, true, true);
        }
        if verdict.ok { Ok(()) } else { Err((verdict.denial, verdict.reason)) }
    }

    /// The engine's own gates ahead of the `RiskGate`, in their order: the symbol admission and the spot sell
    /// rule, then the paper spot buy rule, each as the denial of its word. The flag is whether the buy rule ran
    /// (it reads prices: `note_market_dependent`). Cold: one call per order.
    fn early_denial(&self, request: &OrderRequest) -> (Option<crate::RiskVerdict>, bool) {
        let denied = crate::RiskVerdict::denied;
        let early = self
            .symbol_denial(request)
            .map(|denial| denied(denial, denial.to_string()))
            .or_else(|| {
                self.cash_oversell_denial(request).map(|why| denied(OrderDenial::CashOversell, why))
            });
        let overbuy_ran = early.is_none();
        let early = early.or_else(|| {
            self.cash_overbuy_denial(request).map(|why| denied(OrderDenial::CashOverbuy, why))
        });
        (early, overbuy_ran)
    }

    /// **File `request`'s order client id as decided on market data** ([`Self::market_dependent`]) when the decision
    /// just made read data the journal does not hold: the gate's verdict (`gate_ran`) says whether it did, the paper
    /// cash rule (`overbuy_ran`) reads prices whenever it judges a buy, and the MARK FLOOR, which a snapshot-built
    /// engine never runs (`RiskGate::require_mark`), would have judged an order on the mounted symbol of an engine that
    /// holds no mark of its own. Called only while [`Self::record_market_dependent`]: a replay core's engines.
    #[cold]
    #[inline(never)]
    fn note_market_dependent(&mut self, request: &OrderRequest, overbuy_ran: bool, gate_ran: bool) {
        let symbol = if request.symbol.is_empty() { self.symbol.as_str() } else { &request.symbol };
        let unmarked = symbol == self.symbol
            && !self
                .resolved_position_price(
                    &self.venue,
                    symbol,
                    self.gate_position_size(symbol),
                    &self.price_config,
                )
                .is_some_and(|m| m.is_finite() && m > 0.0);
        let read = (gate_ran && self.gate.last_verdict_read_market_data())
            || (overbuy_ran && self.cash_overbuy_reads_prices(request))
            || (gate_ran && unmarked);
        if read {
            self.market_dependent.push(request.order_client_id.clone());
        }
    }

    /// Risk-gate + register one intent. Returns the (possibly gate-adjusted) request to send to the
    /// venue, or `None` after publishing `OrderDenied`. Shared by `submit_order` and
    /// `submit_order_batch` so the veto/registration path is identical single or batched.
    fn gate_and_register(
        &mut self,
        request: &OrderRequest,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) -> Option<OrderRequest> {
        let verdict = self.gate_verdict(request, now_ms);
        self.register_or_deny(request, verdict, now_ms, outbox)
    }

    /// **The pre-trade gate's verdict on `request`, as [`Self::submit_order`] judges it**: the engine's own
    /// gates ([`Self::early_denial`]), then the `RiskGate` (an admitted order takes its rate-limit slot here).
    /// Registers and publishes nothing; [`Self::submit_judged`] acts on it.
    pub fn gate_verdict(&mut self, request: &OrderRequest, now_ms: i64) -> crate::RiskVerdict {
        let (denial, overbuy_ran) = self.early_denial(request);
        let gate_ran = denial.is_none();
        let verdict = match denial {
            Some(verdict) => verdict,
            None => {
                let ctx = self.risk_context(request, now_ms);
                self.gate.check(request, &ctx)
            }
        };
        if self.record_market_dependent {
            self.note_market_dependent(request, overbuy_ran, gate_ran);
        }
        verdict
    }

    /// Register what `verdict` admitted, or publish its `OrderDenied`: the second half of
    /// [`Self::gate_and_register`].
    fn register_or_deny(
        &mut self,
        request: &OrderRequest,
        verdict: crate::RiskVerdict,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) -> Option<OrderRequest> {
        let Some(req) = (if verdict.ok { verdict.request } else { None }) else {
            // Per-ORDER (not per-tick): a RiskGate veto is fault-adjacent, off the measured hop.
            tracing::warn!(target: "vike_execution::risk", order_client_id = %request.order_client_id, reason = %verdict.reason, "order denied by RiskGate");
            let denied = OrderDenied {
                order_client_id: request.order_client_id.clone(),
                refusal_id: verdict
                    .denial
                    .and_then(|d| vike_model::events::refusal_id_of(d.id().word())),
                reason: verdict.reason.into(),
                timestamp: request.timestamp,
            };
            // Capture the veto for `Strategy::on_order_event` HERE: a denied order never enters
            // the registry, so the FSM-apply capture site cannot see it. Gated on the same mount
            // flag as `applied_fills`, so a GUI-only engine and the latency gate pay nothing.
            if self.collect_applied_fills {
                self.order_events.push(OrderEventOut {
                    venue: request.venue.clone(),
                    symbol: request.symbol.clone(),
                    event: vike_model::OrderEvent {
                        order_client_id: denied.order_client_id.clone(),
                        // The ENGINE holds no tag registry — it is the runtime's, and the runtime
                        // stamps this at delivery. See `OrderEvent::tag`.
                        tag: None,
                        kind: vike_model::OrderEventKind::Denied {
                            reason: denied.reason.to_string(),
                        },
                    },
                });
            }
            outbox.publish(Event::OrderDenied(denied));
            return None;
        };
        let mut mo = OrderRecord::new(req.clone());
        mo.created_ms = Some(now_ms); // audit C3: stamp on the injected clock the watchdog sweeps on
        self.registry.insert(req.order_client_id.clone(), mo);
        Some(req)
    }

    /// Build the [`RiskContext`] one gate call judges `request` against. Shared by submit and
    /// MODIFY ([`Self::modify_order`]): a modify that raises qty is economically a bigger order,
    /// and sharing the BUILDER rather than a copy keeps both judged on the same
    /// equity/margin/mark/multiplier basis — a divergence there is exactly what the one-price law
    /// exists to stop.
    fn risk_context(&self, request: &OrderRequest, now_ms: i64) -> RiskContext {
        // Margin fields are computed ONLY when the margin lane is armed (`initial_margin_for`):
        // resolved_equity is O(positions) per order.
        //
        // THE CONTRACT VALUE IS LANE-INDEPENDENT (the #458 multiplier-in-context class).
        // `ctx.contract_value` feeds the gate's notional (`ContractValue::order_notional`, read by
        // min_notional AND max_notional_per_order — see `risk.rs`), not only the buying-power
        // lane, so it is minted HERE, outside the `initial_margin_for` arm: an engine with notional caps but
        // no margin lane (the live default with no configured leverage) must still judge a
        // inverse instrument (deribit perps) at its real KIND, as the backtest would.
        // `contract_value_of` returns the default (`Linear`, 1.0) for any symbol outside the grid.
        let value = self.account.contract_value_of(&request.symbol);
        // ...and so is the CURRENCY it is quoted in: the rate the value already carries (the gate
        // converts the venue's own `min_notional` with it) and whether it has gone stale at this
        // order's `now_ms`. `(1.0, false)` after one `Option` check on every single-currency account.
        let (quote_rate, quote_stale) = self.account.quote_leg(&request.symbol, now_ms);
        // Hedge-aware coverage basis, computed ONCE for the reversal credit and the ctx (see
        // `gate_position_size`). Keyed on the ORDER's symbol, like `multiplier` and `initial_margin_for`: an
        // engine also accepts `extra_symbols`, and reading the mount's position would judge a
        // foreign-symbol order against a MIXTURE of two instruments.
        let pos = self.gate_position_size(&request.symbol);
        // The symbol the FLOOR prices the order under: an EMPTY symbol is the mounted one (`symbol_denial`'s
        // doc: the adapters read it so and sign it on the mounted symbol), so on an ARMED engine a blank
        // symbol is looked up, and judged, at the mounted symbol's mark; looked up as `""` it finds nothing
        // and the order is priced at notional zero, which is the hole the floor closes. Only an armed
        // engine reads it so: every other engine judges a blank symbol exactly as it always did.
        // shortcut: only this lookup reads a blank symbol as the mounted one; the contract value, quote
        // leg, position and margin reads keep `""` (a blank-symbol close is never seen as covered, which
        // can only deny more), upgrade by normalising the symbol once for every read, as a change of its
        // own for the whole fleet.
        let floor_symbol: &str = if self.gate.require_mark && request.symbol.is_empty() {
            &self.symbol
        } else {
            &request.symbol
        };
        // The ENGINE's own price for the order's symbol, before any fallback: the valuation basis below
        // takes the order's price when this is absent, which hides the absence from the gate.
        let own_mark =
            self.resolved_position_price(&self.venue, floor_symbol, pos, &self.price_config);
        // The mark floor's fact. `&&` stops at the bool, so an engine without the floor compares no
        // string and looks at no mark.
        let unmarked = self.gate.require_mark
            && floor_symbol == self.symbol
            && !own_mark.is_some_and(|m| m.is_finite() && m > 0.0);
        let (equity, margin_used, closing_credit) = if let Some(initial_margin_requirement) =
            self.gate.limits.initial_margin_for(&request.symbol)
        {
            // THE gate's margin (`gate_margin_used`, the combo's too), resolver-priced so the
            // margin numerator shares `resolved_equity`'s price basis (one price law): the open
            // positions, each falling back to the ORDER symbol's `initial_margin_requirement`, plus what this engine's
            // live un-filled orders committed, or a second order is judged as if the first
            // committed nothing.
            let used = self.gate_margin_used(
                &self.price_config,
                initial_margin_requirement,
                &request.order_client_id,
            );
            // Direction-reversing order: credit the margin the close frees + the LEAN re-open
            // credit (single-requirement model: mm == im in the gate), priced through the SAME
            // resolver as `used` and `resolved_equity` (the position's own symbol/side chain):
            // a raw stale-high mark would credit margin `used` never charged.
            //
            // A `Missing` resolution credits NOTHING, matching `resolved_margin_in_use_by`,
            // which drops an unpriceable position from `used`: a position that consumed no
            // margin frees none. The two ends are CONSISTENT, not both conservative — zeroing
            // the credit denies more, while dropping the position from `used` inflates free
            // buying power (the ANTI-conservative one). Do not read `Missing` as uniformly
            // safe.
            let credit = if vike_model::is_reducing_direction(request.side, pos) {
                let px = self
                    .resolved_position_price(&self.venue, &request.symbol, pos, &self.price_config)
                    .unwrap_or(0.0);
                value.margin_credit(pos, px, initial_margin_requirement)
            } else {
                0.0
            };
            // One-price law: the SAME resolver-priced equity the watchdog/strategy/snapshot
            // read, not the mark-only `equity_all`.
            //
            // ⚠ `sizing_equity`, not `resolved_equity`: this lane SPENDS against equity, so
            // `RiskLimits::max_sizing_equity` applies and a lower figure can only refuse
            // sooner. The margin-CALL sweep keeps the uncapped resolver —
            // `Self::sizing_equity`'s doc carries the asymmetry.
            (self.sizing_equity(self.equity_seed, &self.price_config), used, credit)
        } else {
            (0.0, 0.0, 0.0)
        };
        RiskContext {
            // ⚠ NOT `pos` for a reduce-only order on a flat book: `cover_position_size` answers the
            // venue's own reported position there (reconcile's evidence), so a covered close of a
            // position this book has not folded yet is not judged an OPENING order. `pos` stays the
            // book's truth for the margin credit above, which must never credit a position the
            // engine does not hold.
            position_size: self.cover_position_size(
                &request.symbol,
                request.reduce_only,
                request.side,
                &request.order_client_id,
            ),
            // The priceless (market/stop) notional+exposure reference, on the ONE resolver basis
            // shared with equity/margin/credit (the position's own side chain, like the credit).
            // ⚠ **THE MARK FIRST, the order's own price only as a last resort**: the field is the
            // VALUATION basis, not an order price. Reading `request.price` first would silently
            //
            //  * judge `risk.rs`'s projected-exposure cap (`ctx.contract_value.gross_notional(pos +
            //    side*qty, ctx.mark_price)`) at a resting limit's price instead of the mark, and
            //  * kill `risk.rs`'s PRICE COLLAR (`|req.price - ctx.mark_price| > band`) for every
            //    limit order: with the two equal the difference is ZERO.
            //
            // The NOTIONAL lanes do not read this field for a priced order: `check_inner`'s
            // `ref_price` is the order's own price, else its trigger, else this mark (a MARKET order
            // has no price of its own, whatever it names).
            //
            // ⚠ `request.price` stays as the final fallback rather than letting an unresolvable
            // mark drop to `0.0`: a `0.0` mark makes projected exposure 0, which VACATES the
            // exposure cap at any size (`check_combo` hard-denies `"leg X: no-mark"` for this
            // reason). Keep it deliberately. ⚠ NOT a MARKET order's: it would size the order at the
            // number it names, so it takes the road of a market order that names none.
            //
            // ⚠ **NaN is the "unpriced" answer of the MARK FLOOR** (`RiskGate::require_mark`): the
            // engine's MOUNTED symbol with no mark of its own (absent, zero, negative or
            // non-finite) answers NaN instead of the order's price or 0.0, and the gate denies an
            // opening order on it `no-mark-for-mounted-symbol`. The same convention as
            // `account_exposure_excl_order` below: a non-finite figure says "cannot be valued".
            // ⚠ **NOT a freshness test.** `own_mark` is read under `self.price_config`, which every shipped
            // path leaves at `PriceBoardConfig::default()` (no freshness window), so a price the engine EVER saw
            // stands: the floor says "never priced since boot", not "priced now", and a mark from a dead
            // feed passes it for ever. An OPEN LIMIT, deliberately left open (a freshness window is a
            // behaviour change of its own).
            mark_price: if unmarked {
                f64::NAN
            } else {
                own_mark
                    // An ADMITTED symbol never takes the order's own price as its mark: that makes the
                    // collar compare the price with itself (a limit at 100x the market passes) and
                    // the exposure caps read the order's number. With no mark it is 0.0, and the gate
                    // denies an opening order on that symbol `no-reference-price`.
                    .or((!matches!(request.order_type, vike_model::OrderType::Market)
                        && !self
                            .gate
                            .limits
                            .grid_by_symbol
                            .get(request.symbol.as_str())
                            .is_some_and(|g| g.on_demand))
                    .then_some(request.price)
                    .flatten())
                    .unwrap_or(0.0)
            },
            trading_state: self.trading_state,
            now_ms,
            equity,
            margin_used,
            closing_credit,
            contract_value: value,
            quote_rate,
            quote_stale,
            // The ACCOUNT-aggregate ceiling's other half, folded ONLY when that ceiling is armed
            // (O(positions + live orders); a deployment without `max_account_exposure` pays
            // nothing). The `0.0` is inert: only that lane reads the field.
            //
            // ⚠ `request.order_client_id` is the JUDGED order, excluded from the resting-order
            // half: on an AMEND (`modify_order` builds this ctx from the projected request under
            // the resting order's own order client id) it is in the registry, and counting it as well as
            // projecting it would refuse the amend at a ceiling it was admitted under at submit.
            //
            // ⚠ PLUS the partner engine's book (`Self::sibling_exposure`, `0.0` for an engine with no
            // partner): the row's ONE ceiling, not one per engine. A non-finite own figure stays
            // non-finite (`INFINITY + x`), so the fail-closed denial is untouched. Per-order cost: one
            // f64 load and add, inside the armed branch.
            account_exposure_excl_order: if self.gate.limits.max_account_exposure.is_some() {
                self.resolved_account_exposure_excluding(
                    &request.symbol,
                    &request.order_client_id,
                    &self.price_config,
                ) + self.sibling_exposure
            } else {
                0.0
            },
        }
    }

    /// The gate's word on `request`, an order that OPENS right behind a close of the whole position in its symbol (the
    /// part of a fired stop larger than its position, `vike_core`'s `submit_fired`), on the book that close leaves: flat,
    /// its margin free. `None` admits it. The order then crosses [`Self::submit_order`] as ever, where the gate still reads
    /// the whole position (the close has not filled) and cannot judge it: a covered reduce up to the position's size, too
    /// small a flip past it.
    ///
    /// Every lane but the throttle ([`crate::RiskGate::check_modify`], nothing executed): the slot is the send's. It reads
    /// marks, which a replay does not have, so the caller journals the order only once admitted and a replay never asks.
    pub fn denial_past_close(&mut self, request: &OrderRequest, now_ms: i64) -> Option<String> {
        let mut ctx = self.risk_context(request, now_ms);
        ctx.position_size = 0.0;
        // `closing_credit` is what the position frees plus the re-open credit of ONE order that reverses it. This order only
        // opens and pays its whole margin, so its credit is the freed half: the verdict the whole reversal would get.
        // shortcut: an isolated position is credited though `margin_used` never charged it, as `risk_context` credits every
        // reversal; upgrade both together.
        ctx.closing_credit /= 2.0;
        let verdict = self.gate.check_modify(request, &ctx, 0.0);
        (!verdict.ok).then_some(verdict.reason)
    }

    /// Batch-submit (RUST-NATIVE HFT surface). Each order is risk-gated + registered independently
    /// — a denied order emits `OrderDenied` and drops from the batch, the rest still go — and the
    /// accepted orders reach the venue in ONE `client.submit_batch` call.
    #[tracing::instrument(level = "info", target = "vike_execution::oms", skip_all, fields(n = requests.len()))]
    pub fn submit_order_batch(
        &mut self,
        requests: &[OrderRequest],
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) {
        let mut accepted: Vec<OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            if let Some(req) = self.gate_and_register(r, now_ms, outbox) {
                accepted.push(req);
            }
        }
        if !accepted.is_empty() && self.fence_send().is_err() {
            let (exits, refused): (Vec<_>, Vec<_>) =
                accepted.into_iter().partition(|r| self.leaves_unrecorded(r));
            for r in &refused {
                self.refuse_unrecorded(r, now_ms, outbox);
            }
            accepted = exits;
        }
        if !accepted.is_empty() {
            self.client.submit_batch(&accepted);
        }
    }

    /// A registered order still worth acting on (cancel/confirm-send gate) — the ONE set
    /// definition is [`vike_model::OrderStatus::is_live`]; see its doc for why it is neither
    /// `!is_terminal()` (Liquidated) nor the FSM's `can_receive_cancel` allowed-from set.
    fn is_live(&self, order_client_id: &str) -> bool {
        self.registry.get(order_client_id).is_some_and(|mo| mo.status.is_live())
    }

    /// Cancel a live order by order-client-id. Idempotent: no-op if unknown or already
    /// terminal. Publishes NOTHING — the venue stream emits the authoritative OrderCanceled
    /// that advances the FSM (`on_event`).
    ///
    /// Returns whether the cancel was ISSUED to the client: `false` is the no-op above (the order is
    /// not in this registry, or is no longer live), which a caller that must not lose track of a
    /// resting order — the core's OCO drive — has to be able to tell from a cancel that went out.
    /// Every other caller ignores it.
    ///
    /// Says nothing about WHY, so the venue sees [`OrderCancelIntent::Unspecified`] — the flatten-safe
    /// value, never held back. A caller that CAN classify uses
    /// [`Engine::cancel_order_with_intent`] instead.
    pub fn cancel_order(&mut self, order_client_id: &str) -> bool {
        self.cancel_order_with_intent(order_client_id, OrderCancelIntent::Unspecified)
    }

    /// [`Engine::cancel_order`], saying WHY (see [`OrderCancelIntent`]). Same idempotence, same
    /// terminal guard, same fire-and-forget contract, same answer (`true` iff a cancel was issued) —
    /// the intent only travels; nothing in this engine reads it. Only the venue client may act on it,
    /// and only ever by holding back an [`OrderCancelIntent::Routine`] one.
    #[tracing::instrument(level = "info", target = "vike_execution::oms", skip_all, fields(order_client_id = %order_client_id, intent = ?intent))]
    pub fn cancel_order_with_intent(
        &mut self,
        order_client_id: &str,
        intent: OrderCancelIntent,
    ) -> bool {
        let issued = self.is_live(order_client_id);
        if issued {
            // A cancel leaves even when the journal could not record it (decision 0279's addendum of 2026-11-23).
            let _ = self.fence_send();
            self.client.cancel_with_intent(order_client_id, intent);
            self.note_unanswered(order_client_id, UnansweredCommand::Cancel, self.now_ms);
        }
        issued
    }

    /// Cancel a batch of orders by order-client-id (RUST-NATIVE), saying WHY (see
    /// [`OrderCancelIntent`]; a caller that cannot classify passes [`OrderCancelIntent::Unspecified`], the
    /// flatten-safe value [`Engine::cancel_order`] sends). Live orders only (the terminal
    /// guard mirrors `cancel_order`); the venue side is ONE `client.cancel_batch` call.
    #[tracing::instrument(level = "info", target = "vike_execution::oms", skip_all, fields(n = order_client_ids.len(), intent = ?intent))]
    pub fn cancel_order_batch_with_intent(
        &mut self,
        order_client_ids: &[String],
        intent: OrderCancelIntent,
    ) {
        let live: Vec<String> =
            order_client_ids.iter().filter(|c| self.is_live(c)).cloned().collect();
        if !live.is_empty() {
            let _ = self.fence_send(); // a cancel leaves unrecorded too (`cancel_order_with_intent`)
            self.client.cancel_batch_with_intent(&live, intent);
            self.note_unanswered_cancels(&live);
        }
    }

    /// Cancel ALL live orders on this engine (pull-all-quotes) in one `client.cancel_batch`,
    /// saying WHY (see [`OrderCancelIntent`]). A strategy's own "pull my quotes" is NOT a declared
    /// risk-off action, so it passes the flatten-safe [`OrderCancelIntent::Unspecified`] rather than
    /// being labeled one; the emergency callers (market-exit, dead-man, safe state) pass
    /// [`OrderCancelIntent::RiskOff`].
    pub fn mass_cancel_with_intent(&mut self, intent: OrderCancelIntent) {
        let live: Vec<String> =
            self.registry.live().map(|(order_client_id, _)| order_client_id.clone()).collect();
        if !live.is_empty() {
            let _ = self.fence_send(); // a cancel leaves unrecorded too (`cancel_order_with_intent`)
            self.client.cancel_batch_with_intent(&live, intent);
            self.note_unanswered_cancels(&live);
        }
    }

    /// Modify a live order's qty/price by order-client-id (RUST-NATIVE). Legal only while the order
    /// rests at the venue (ACCEPTED/TRIGGERED/PARTIALLY_FILLED); a SUBMITTED or terminal order is a
    /// no-op. Publishes NOTHING on success — the venue stream emits the authoritative OrderModified
    /// that advances the FSM (`on_event`), mirroring cancel.
    ///
    /// ⚠ **A VENUE THAT CANNOT AMEND REFUSES, NEVER SWALLOWS.** When the amend convention
    /// ([`ExecutionClient::amend_semantics`], else `vike_venues::amend_semantics`) is
    /// `AmendSemantics::Unsupported` this publishes a NON-TERMINAL [`Event::OrderModifyRejected`]
    /// ([`MODIFY_UNSUPPORTED_REASON`]) BEFORE the risk gate and sends nothing: the adapter's modify
    /// is `ExecutionClient::modify`'s default no-op, which used to leave the command accepted and
    /// the order untouched with no event at all.
    ///
    /// ⚠ **RISK-GATED.** A modify that RAISES qty (or price) is economically a bigger order, so the
    /// [`RiskGate`] judges the PROJECTED order (the resting request with `new_qty` / `new_price`
    /// folded in), never the delta — else a small in-cap order modified up walks past
    /// `max_notional_per_order`. A veto publishes a NON-TERMINAL [`Event::OrderModifyRejected`]
    /// (the order keeps its terms, as when a venue refuses a modify) and nothing reaches the venue.
    ///
    /// Two deliberate departures from the submit path:
    /// - **No throttle slot is consumed** ([`RiskGate::check_modify`]): the resting order paid one
    ///   at submit, and re-charging every amend would self-throttle an amend-heavy maker.
    /// - **The gate's ROUNDED request IS what goes on the wire.** The venue modify takes
    ///   `(original order, new_qty, new_price)`, and an ACCEPTED modify sends the values the gate
    ///   snapped to the tick/lot grid (`verdict.request`, as `submit_order` does), not the caller's
    ///   raw ones; a field the caller omitted stays omitted.
    ///
    /// ⚠ **WHETHER THE EXECUTED PART OF A PARTIALLY FILLED ORDER IS NETTED OUT IS A PER-VENUE
    /// FACT.** On an IN-PLACE amend venue the projected `qty` is the new TOTAL, executed lots
    /// included, while [`RiskContext::position_size`] already holds those lots, so not netting
    /// counts them twice and refuses economic no-ops (e.g. re-pricing a half-done exit under a
    /// halt). On a CANCEL-REPLACE venue a FRESH order of `new_qty` replaces the resting one, so the
    /// un-netted sum is CORRECT and netting would ADMIT what the gate should refuse.
    /// `AmendSemantics::already_in_position` is non-zero for exactly one variant, so a venue whose
    /// convention is not established keeps the conservative arithmetic.
    /// `crates/vike-execution/src/risk.rs`'s `still_executable` states which lanes consume it and the
    /// `filled_qty`-is-inside-`position_size` invariant it rests on.
    ///
    /// ⚠ **THE CLIENT IS ASKED FIRST; THE VENUE STRING ANSWERS ONLY WHEN IT DECLINES.**
    /// `vike_tradehub_core::make_engine` labels the engine with the REAL venue even when the mount fell
    /// back to `vike_paper::ExecutionClientPaper` (a `paper`-tier or inactive account, a bridge's
    /// `VenueAdapterExecutionOutcome::NotRunning`, a venue this build does not compile), and
    /// `vike_tradehub_core::build_paper_strategy_core_with` does the same, while the paper book implements a THIRD
    /// convention (its `modify` assigns the amend's quantity to the REMAINING size, so the whole
    /// `new_qty` is still coming).
    /// [`ExecutionClient::amend_semantics`] returns `None` for every venue adapter (defer to the
    /// table) and `Some(AmendSemantics::InPlaceRemaining)` for the paper exchange, which nets
    /// nothing. `crates/vike-paper/tests/paper_amend_is_not_the_venues_amend.rs` drives this mount
    /// end to end.
    pub fn modify_order(
        &mut self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) {
        self.modify_order_with_trigger(order_client_id, new_qty, new_price, None, now_ms, outbox);
    }

    /// [`Self::modify_order`] that may also move the order's TRIGGER: `new_trigger_price` beside
    /// `new_price`, landing where `vike_model::modified_prices` says. With `None` it is `modify_order`
    /// exactly. A stop's trigger rides the client's `new_price`, as it always has; a stop-limit's or a
    /// take-profit's goes to [`ExecutionClient::modify_trigger`], whose refusal is published as the
    /// non-terminal [`Event::OrderModifyRejected`]. A trigger price on an order with no trigger to move
    /// (a kind with none, or one whose trigger has fired) is refused in words
    /// ([`TRIGGER_AMEND_NO_TRIGGER_REASON`]), never dropped.
    #[tracing::instrument(level = "info", target = "vike_execution::oms", skip_all, fields(order_client_id = %order_client_id))]
    pub fn modify_order_with_trigger(
        &mut self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) {
        // Clone the resting request out (drops the borrow before &mut client); the venue modify
        // gets the full order context. `filled_qty` comes out in the same read (see this fn's doc).
        let (req, filled_qty, lifetime_unknown, fired) = match self.registry.get(order_client_id) {
            Some(mo) if mo.status.is_modifiable() => (
                mo.request.clone(),
                mo.filled_qty,
                mo.lifetime_unknown,
                trigger_fired(mo.status, mo.filled_qty),
            ),
            _ => return, // unknown, not-yet-accepted, or terminal — nothing to modify
        };
        // The CLIENT is asked first; the venue string answers only when it declines (this fn's doc,
        // "THE CLIENT IS ASKED FIRST"). Hoisted ahead of the gate below because the same answer
        // decides two things: whether an amend can exist at all, and how the gate nets executed lots.
        let semantics = self
            .client
            .amend_semantics()
            .unwrap_or_else(|| vike_venues::amend_semantics(&self.venue));
        // ⚠ A modify the venue CANNOT do must be REFUSED OUT LOUD. Every adapter without a native
        // amend takes `ExecutionClient::modify`'s default no-op, so before this check the command
        // was accepted at every door (control, GUI) and then nothing happened at the venue, with no
        // event: the resting order kept its terms and the operator could not tell a lost modify from
        // a done one. `Unsupported` is the table's word for "no amend leaves the process", pinned to
        // `venue_caps`'s `supports_modify` by `amend_semantics_agrees_with_venue_capabilities_supports_modify`;
        // `Unknown` (a venue whose amend convention is not established, an unrecognised venue string) is NOT refused,
        // because there the adapter may amend. A paper mount is never refused: the paper exchange
        // declares `InPlaceRemaining` through the client. NON-TERMINAL, like the risk veto below.
        if semantics == vike_venues::AmendSemantics::Unsupported {
            tracing::warn!(
                target: "vike_execution::oms",
                order_client_id = %order_client_id,
                venue = %self.venue,
                "modify refused: the venue declares no order amend",
            );
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, MODIFY_UNSUPPORTED_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        // A paper book answers for itself (the client's row above) and sends no lifetime anywhere.
        if lifetime_unknown
            && self.client.amend_semantics().is_none()
            && vike_venues::venue_amend::amend_sends_lifetime(&self.venue)
        {
            tracing::warn!(
                target: "vike_execution::oms",
                order_client_id = %order_client_id,
                venue = %self.venue,
                "modify refused: nothing remembers this order's lifetime, and this amend would send one",
            );
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, MODIFY_LIFETIME_UNKNOWN_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        if mit_price_amend(&req, new_price) {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, MIT_PRICE_AMEND_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        let in_place = self.client.amend_semantics().is_some();
        if trailing_amend_refused(&req, new_price, new_trigger_price, in_place) {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, TRAILING_AMEND_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        if adjustable_amend_refused(&req, new_price, new_trigger_price, in_place) {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, ADJUSTABLE_AMEND_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        let Some((new_price, new_trigger_price)) =
            seam_prices(&req.order_type, fired, new_price, new_trigger_price)
        else {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, TRIGGER_AMEND_NO_TRIGGER_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        };
        let Some(snapped) = self.gate_modify(
            order_client_id,
            &req,
            filled_qty,
            semantics,
            new_qty,
            new_price,
            new_trigger_price,
            now_ms,
            outbox,
        ) else {
            return;
        };
        let (qty, price) = (new_qty.map(|_| snapped.qty), new_price.and(snapped.price));
        let trigger = new_trigger_price.and(snapped.trigger_price);
        // Both amend calls below leave the process: one wait covers either. An amend the journal could not record
        // is NOT SENT, whatever it changes: the order keeps its terms, and a cancel or a covered exit still gets out.
        if self.fence_send().is_err() {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {}", self.venue, JOURNAL_CANNOT_WRITE_REASON).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::JournalCannotWrite.word(),
                ),
            }));
            return;
        }
        if let Some(t) = trigger
            && let Err(why) = self.client.modify_trigger(&req, qty, price, t)
        {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: format!("{}: {why}", self.venue).into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(
                    vike_model::OrderRefusalId::VenueCapability.word(),
                ),
            }));
            return;
        }
        // The price is on its way to the venue: remember it until the venue answers (the field's doc).
        if let Some(px) = price
            && !vike_model::modified_price_is_trigger(&req.order_type)
        {
            self.note_pending_modify(order_client_id, px, now_ms);
        }
        // A reconcile pass leaves this order's terms alone until the amend settles (the field's doc).
        self.note_amend(order_client_id, now_ms);
        self.note_unanswered(order_client_id, UnansweredCommand::Modify, now_ms);
        if trigger.is_none() {
            self.client.modify(&req, qty, price);
        }
    }

    /// The RiskGate's judgement of a modify, and the grid-snapped order it lets through: `None` after
    /// publishing the non-terminal [`Event::OrderModifyRejected`] veto. The ONE body shared by the
    /// native send ([`Self::modify_order`]) and the cancel-then-new replacement
    /// ([`Self::plan_replacement`]), so the two cannot judge one amend two ways. An omitted field
    /// keeps the resting value, as the venue does. `semantics` decides whether the executed lots of a
    /// partly filled order are netted out of the projection (the doc of `modify_order`).
    fn gate_modify(
        &mut self,
        order_client_id: &str,
        req: &OrderRequest,
        filled_qty: f64,
        semantics: vike_venues::AmendSemantics,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) -> Option<OrderRequest> {
        // The order AS MODIFIED — what the gate judges.
        let mut projected = req.clone();
        if let Some(q) = new_qty {
            projected.qty = q;
        }
        if let Some(p) = new_price {
            projected.price = Some(p);
        }
        if let Some(t) = new_trigger_price {
            projected.trigger_price = Some(t);
        }
        // The amend convention decides whether the executed lots are inside `projected.qty`
        // (in-place TOTAL: net them out) or not (cancel-replace / in-place REMAINING: net nothing).
        // The CLIENT is asked first — `self.venue` is only the label, and a paper mount carries the
        // real venue string (see `modify_order`'s doc); a client that declines falls to the venue table.
        let already_executed = semantics.already_in_position(filled_qty);
        // A spot (CASH) sell RAISED past what the account holds is refused as its submit would be
        // (`cash_oversell_denial`, which leaves this order's own quantity out of the sells already out, so only
        // the increase is judged): NON-TERMINAL, the order keeps its terms. A paper spot BUY that costs more
        // (its quantity or its limit raised) is judged as a new buy (`cash_overbuy_denial`, which leaves this
        // order's own cost out of the buys already out). A decrease and any other amend are not asked. Cold:
        // two compares unless the quantity or the price rises.
        let raised = new_qty.is_some_and(|q| q > req.qty);
        let dearer = new_price.is_some_and(|p| req.price.is_some_and(|old| p > old));
        if raised || dearer {
            let mut rest = projected.clone();
            rest.qty = projected.qty - already_executed;
            let why = (if raised {
                self.cash_oversell_denial(&rest).map(|why| (OrderDenial::CashOversell, why))
            } else {
                None
            })
            .or_else(|| self.cash_overbuy_denial(&rest).map(|why| (OrderDenial::CashOverbuy, why)));
            if let Some((denial, why)) = why {
                if self.record_market_dependent {
                    self.note_market_dependent(&rest, true, false);
                }
                tracing::warn!(target: "vike_execution::risk", order_client_id = %order_client_id, reason = %why, "modify denied");
                outbox.publish(Event::OrderModifyRejected(
                    vike_model::events::OrderModifyRejected {
                        order_client_id: order_client_id.to_string(),
                        reason: format!("risk: {why}").into(),
                        timestamp: now_ms,
                        refusal_id: vike_model::events::refusal_id_of(denial.id().word()),
                    },
                ));
                return None;
            }
        }
        let ctx = self.risk_context(&projected, now_ms);
        let verdict = self.gate.check_modify(&projected, &ctx, already_executed);
        if self.record_market_dependent {
            self.note_market_dependent(&projected, raised || dearer, true);
        }
        if !verdict.ok {
            // Per-ORDER, fault-adjacent — off the measured hop, exactly like the submit veto.
            tracing::warn!(
                target: "vike_execution::risk",
                order_client_id = %order_client_id,
                reason = %verdict.reason,
                "modify denied by RiskGate",
            );
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                refusal_id: verdict
                    .denial
                    .and_then(|d| vike_model::events::refusal_id_of(d.id().word())),
                reason: format!("risk: {}", verdict.reason).into(),
                timestamp: now_ms,
            }));
            return None;
        }
        // The venue is sent what the gate NORMALIZED (tick/lot grid), exactly as `submit_order` sends
        // `verdict.request`: forwarding the raw `new_*` put an off-grid price on the wire and into
        // the local order (binance/okx/deribit accepted 66714.545 on a 0.01 tick). An omitted field
        // stays omitted.
        Some(verdict.request.unwrap_or(projected))
    }

    /// How a modify of the resting order `req` is carried out on this engine: in place at the venue,
    /// by cancel-then-new, or not at all — `None` for a kind that does not rest (a market order).
    ///
    /// ⚠ **THE CLIENT IS ASKED FIRST, as for [`ExecutionClient::amend_semantics`]**: a client that
    /// declares an amend convention is not the venue it is mounted under (the paper exchange), and its
    /// book re-prices and re-sizes EVERY resting kind in place, so it is `Native` for all of them; a
    /// client that declines falls to the venue table (`vike_venues::venue_amend_mode`). The caller
    /// that acts on the answer is `vike-core`'s `runtime/apply/amend.rs`; [`Self::modify_order`]
    /// itself is the NATIVE send and never consults it.
    pub fn amend_mode_of(
        &self,
        req: &OrderRequest,
    ) -> Option<vike_venues::venue_amend_mode::AmendMode> {
        let kind = vike_venues::venue_amend_mode::AmendKind::of(&req.order_type)?;
        if self.client.amend_semantics().is_some() {
            return Some(vike_venues::venue_amend_mode::AmendMode::Native);
        }
        Some(vike_venues::venue_amend_mode::amend_mode(&self.venue, kind))
    }

    /// [`Self::amend_mode_of`] for a modify that moves the order's TRIGGER (`new_trigger_price`): the
    /// venue table's `vike_venues::venue_amend_mode::trigger_amend_mode`, the client asked first in the
    /// same way (a client that declares an amend convention moves every trigger in place).
    pub fn trigger_amend_mode_of(
        &self,
        req: &OrderRequest,
    ) -> Option<vike_venues::venue_amend_mode::AmendMode> {
        let kind = vike_venues::venue_amend_mode::AmendKind::of(&req.order_type)?;
        if self.client.amend_semantics().is_some() {
            return Some(vike_venues::venue_amend_mode::AmendMode::Native);
        }
        Some(vike_venues::venue_amend_mode::trigger_amend_mode(&self.venue, kind))
    }

    /// The order a cancel-then-new amend of `order_client_id` would PLACE, judged by the same
    /// RiskGate step as a native amend (a cancel-replace: the executed lots of a partly filled order
    /// are not netted, the replacement is a fresh order) and snapped to the same grid. `None` after
    /// publishing a non-terminal [`Event::OrderModifyRejected`] in words: the order is unknown or does
    /// not rest, the gate vetoed it, or nothing of it is left to rest.
    ///
    /// The returned request carries the OLD order's order client id; the caller gives the new order
    /// its own. `new_qty` omitted means "the same order, as much as is still unfilled". A stop-limit
    /// or limit-if-touched that has fired is replaced by the plain `Limit` it released.
    /// `new_trigger_price` moves the trigger as [`Self::modify_order_with_trigger`] does, and is refused
    /// in the same words on an order with no trigger to move.
    /// Publishes nothing on success and sends nothing: placing and cancelling are the caller's.
    pub fn plan_replacement(
        &mut self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
        now_ms: i64,
        outbox: &mut EventBusOutbox,
    ) -> Option<OrderRequest> {
        let refuse = |outbox: &mut EventBusOutbox, refusal: OrderRefusalId, reason: String| {
            outbox.publish(Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: now_ms,
                refusal_id: vike_model::events::refusal_id_of(refusal.word()),
            }));
        };
        let (mut req, filled_qty, status) = match self.registry.get(order_client_id) {
            // A replacement is a NEW order with the old one's terms, on every venue.
            Some(mo) if mo.status.is_modifiable() && mo.lifetime_unknown => {
                refuse(
                    outbox,
                    OrderRefusalId::VenueCapability,
                    format!("{}: {}", self.venue, MODIFY_LIFETIME_UNKNOWN_REASON),
                );
                return None;
            }
            Some(mo) if mo.status.is_modifiable() => (mo.request.clone(), mo.filled_qty, mo.status),
            _ => {
                refuse(
                    outbox,
                    OrderRefusalId::AmendRefused,
                    "the order is not resting at the venue, so there is nothing to change".into(),
                );
                return None;
            }
        };
        // A stop-limit or limit-if-touched the venue has fired (`OrderTriggered`, or any fill, which
        // only a fired one can have) rests as the plain limit it released: its replacement is that
        // limit, or it would re-arm a trigger already spent. Every other order keeps its kind.
        let released = trigger_fired(status, filled_qty);
        if mit_price_amend(&req, new_price) {
            refuse(
                outbox,
                OrderRefusalId::VenueCapability,
                format!("{}: {}", self.venue, MIT_PRICE_AMEND_REASON),
            );
            return None;
        }
        // A replacement would restart a trailing order's trail: never planned.
        if req.order_type.is_trailing() {
            refuse(
                outbox,
                OrderRefusalId::VenueCapability,
                format!("{}: {}", self.venue, TRAILING_AMEND_REASON),
            );
            return None;
        }
        // ...and an adjustable stop's: it would restate an adjustment the venue may have made already.
        if req.adjust.is_some() {
            refuse(
                outbox,
                OrderRefusalId::VenueCapability,
                format!("{}: {}", self.venue, ADJUSTABLE_AMEND_REASON),
            );
            return None;
        }
        let Some((new_price, new_trigger_price)) =
            seam_prices(&req.order_type, released, new_price, new_trigger_price)
        else {
            refuse(
                outbox,
                OrderRefusalId::VenueCapability,
                format!("{}: {}", self.venue, TRIGGER_AMEND_NO_TRIGGER_REASON),
            );
            return None;
        };
        if released
            && (req.order_type == vike_model::OrderType::StopLimit
                || (req.order_type == vike_model::OrderType::TakeProfit && req.price.is_some()))
        {
            req.order_type = vike_model::OrderType::Limit;
            req.trigger_price = None;
        }
        // Whatever is left to rest if the caller does not say: a replacement must not re-buy lots the
        // old order already executed.
        let unfilled = (req.qty - filled_qty).max(0.0);
        let qty = new_qty.unwrap_or(unfilled);
        if !(qty.is_finite() && qty > 0.0) {
            refuse(
                outbox,
                OrderRefusalId::AmendRefused,
                "nothing of the order is left unfilled, so there is nothing to place again".into(),
            );
            return None;
        }
        let mut snapped = self.gate_modify(
            order_client_id,
            &req,
            filled_qty,
            vike_venues::AmendSemantics::CancelReplace,
            Some(qty),
            new_price,
            new_trigger_price,
            now_ms,
            outbox,
        )?;
        // `gate_modify` writes the amend's price into `price`; a stop rests on its trigger, so the
        // price an amend names is the TRIGGER (`modified_price_is_trigger`), exactly as the venue's
        // own `OrderModified` would be folded.
        if let Some(p) = new_price
            && vike_model::modified_price_is_trigger(&req.order_type)
        {
            snapped.trigger_price = Some(snapped.price.unwrap_or(p));
            snapped.price = req.price;
        }
        Some(snapped)
    }

    /// ACTIVELY ask the venue to re-confirm one order's status by order-client-id (RUST-NATIVE).
    /// The core's stuck-order watchdog issues this during the confirm-grace so a WEDGED adapter is
    /// PRODDED rather than only waited on. Idempotent + fire-and-forget like `cancel_order`: only a
    /// still-LIVE order is confirmed (terminal/unknown → no-op), and this publishes NOTHING — the
    /// venue's authoritative answer returns over the ingest lane. `client.confirm` re-queries on
    /// the adapter's OWN thread, so no network touches the core fold. A no-op for clients without
    /// a re-query (the `ExecutionClient::confirm` default); the watchdog's stage-2 backstop covers
    /// those.
    #[tracing::instrument(level = "info", target = "vike_execution::oms", skip_all, fields(order_client_id = %order_client_id))]
    pub fn confirm_order(&mut self, order_client_id: &str) {
        if self.is_live(order_client_id) {
            let _ = self.fence_send(); // a re-query changes nothing at the venue: it leaves unrecorded too
            self.client.confirm(order_client_id);
        }
    }
}

#[cfg(test)]
mod execution_engine_tests;

#[cfg(test)]
mod mark_basis_tests;
