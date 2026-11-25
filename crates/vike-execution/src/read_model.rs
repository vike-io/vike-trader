//! The published read model: every type a core snapshot publishes (rows, venue blocks, the
//! cross-venue [`Portfolio`], [`CoreSnapshot`]) and the reads they answer.
//!
//! Plain data, no runtime: the live core (`vike-core`) BUILDS a `CoreSnapshot` from its engines on
//! every publish (`vike_core::snapshot::build`, on the latency-gated hop) and swaps it into an
//! `arc-swap` cell; every reader — the node's summary, alert and observe threads, and the GUI,
//! which rebuilds one from the wire — names these types from this crate's root
//! (`vike_execution::CoreSnapshot`). They live here, below the core, so a reader does not have to link
//! the runtime to name what the runtime publishes. [`MountBudget`] is the one input type among
//! them: a caller hands it to the core per mount, and the core publishes it back on the mount's
//! row.

use crate::{BalanceMode, PriceSource};
use vike_model::events::PositionSide;
use vike_model::{OrderStatus, OrderType, TradingState};

mod interned;
pub use interned::Interned;
mod portfolio;
mod symbol_list;
pub use portfolio::{AccountLedgerTotals, GroupCurve, Portfolio};
pub use symbol_list::SymbolList;

#[cfg(test)]
mod query_tests;

#[derive(Debug, Clone, PartialEq)]
pub struct PositionView {
    /// The venue and the symbol, as the engine's interned names ([`Interned`] reads as the `String`
    /// these were, and `vike_core::snapshot::build` copies them instead of allocating per row).
    pub venue: Interned,
    pub symbol: Interned,
    pub position_side: PositionSide,
    pub size: f64,
    pub average_price: f64,
    /// resolver-priced unrealized PnL (PR-1 `PriceBoard` chain via `resolve_equity`);
    /// 0.0 when `mark_source` is `None` (no priceable source — identical to the legacy
    /// marks-based silent-zero).
    pub unrealized: f64,
    /// which price source resolved this position's mark; `None` == unpriceable (Missing).
    pub mark_source: Option<PriceSource>,
    /// effective leverage for this position's symbol (`1/im`). 0.0 means UNKNOWN, not "none": the
    /// margin gate is off, OR the symbol has no explicit rate under per-symbol margin. Such a leg is
    /// still priced into the venue's `margin_used` and the cross pool at the fallback rate, a
    /// conservative stand-in for risk accounting rather than a rate the venue declared, so no leverage
    /// or liquidation badge is shown for it.
    pub leverage: f64,
    /// estimated liquidation mark from the ONE scope-parameterized law at the ONE maintenance
    /// rate: Isolated → closed-form `vike_model::ContractValue::isolated_liquidation_price`; Cross →
    /// `vike_model::ContractValue::cross_liquidation_price` (the shared-pool line with every other position
    /// frozen — advisory, the venue-UI shape); Cash → 0.0 (never liquidates). 0.0 when the
    /// margin gate is off, the position is flat/unmarked, it has no explicit rate (see `leverage`),
    /// or it is not liquidatable by price. "Unmarked" means no ACCOUNT mark (`Account::mark_of`),
    /// even when the price board has a price for the symbol: this badge reads the account slot while
    /// `margin_used` and equity read the board, so a board-priced cross leg without an account mark
    /// shows 0.0 here AND drops out of the other legs' pool, which makes their badges optimistic.
    /// A known gap in what is shown, not in the arithmetic; the crate page records it.
    pub liquidation_price: f64,
    /// this position's margin mode (`Cross` default = whole-account collateral). Partitions the
    /// liquidation-badge pool above; the venue-report read side writes it (`apply_snapshot`).
    pub margin_mode: vike_model::MarginMode,
    /// allocated isolated-margin wallet for this position (account currency); `None` for cross.
    /// The read-side twin of `PositionEntry::isolated_margin` — inert carrier, no math reads it.
    pub isolated_margin: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderView {
    pub order_client_id: String,
    /// owning engine's venue (cross-venue: snapshot.orders spans ALL engines)
    pub venue: String,
    /// **WHICH ACCOUNT's book this order rests in.** The holding engine's label, `None` for the
    /// default account, so a single-account node (every node today) allocates nothing for it.
    /// Derived from the engine's route key exactly as [`VenueBlock::account`] is.
    ///
    /// ⚠ It exists because `venue` cannot tell two accounts of one exchange apart: on a node that
    /// runs two, a reader filtering `orders` by venue and symbol would mix both accounts' orders,
    /// and a cancel-all built on that filter would pull the other account's orders too.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub order_type: OrderType,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    pub status: OrderStatus,
    pub venue_order_id: Option<String>,
    pub filled_qty: f64,
    pub average_fill_price: f64,
    /// The delay label of the data this order was priced off, as the live core RECORDED it when
    /// the order passed pre-trade (`vike_model::OrderRequest::data_delay`, decision 0140). `None` =
    /// no label stated. Shown, never a gate.
    pub data_delay: Option<vike_model::StreamDelay>,
    /// WHY the order was refused: the reason its `OrderRejected` (or `OrderDenied`) carried, or, on a LIVE order, the reason its last MODIFY was refused (`OrderModifyRejected`, cleared when a modify lands), as the
    /// engine remembered it (`Engine::rejection_reason`). `None` for an order that was not
    /// refused, for one restored from a snapshot, and for a refusal that carried no reason. A GATE
    /// veto is not here: a denied order never enters the registry, so it has no row at all and its
    /// reason is in [`CoreSnapshot::denied_orders`] and the event journal. Additive on the wire
    /// (`OrderViewWire::reason`).
    pub reason: Option<String>,
    /// WHICH refusal [`Self::reason`] is: the catalog id its refusing event carried
    /// (`vike_model::events::OrderRejected::refusal_id`), from the same side table (`Engine::rejection`). `None`
    /// whenever `reason` is, and for a producer that sets no id. Shown beside the reason as
    /// `error [<id>]: <text>` (`vike_utility::errors::render_refusal_or_text`), never decided on. Additive on the wire
    /// (`OrderViewWire::refusal_id`).
    pub refusal_id: Option<String>,
    /// The LIMIT price of a MODIFY this node sent to the venue that the venue has not answered yet
    /// (`Engine::pending_modify_price`): the price the order is about to rest at, while
    /// `price` still shows the one from before. `None` = no modify with a price is in flight (every
    /// order but a moment after one), and always `None` for a stop kind (its modify moves the
    /// trigger). Set by whichever surface sent the modify, which is what lets the control door size
    /// a qty-only modify at the price of one that came through ANOTHER surface
    /// (`vike-tradehub`'s `ControlLimits`). In-process only: not on the wire (`OrderViewWire`), so
    /// an observing client reads `None`. With two modifies in flight it is the last one sent.
    pub pending_price: Option<f64>,
    /// A trailing order's TRAIL, in price (`vike_model::OrderRequest::trail`); `None` on every other kind.
    /// Its activation is [`Self::trigger_price`]. On the wire as `OrderViewWire::trail`.
    pub trail: Option<f64>,
    /// A `trailing_limit` order's LIMIT OFFSET (`vike_model::OrderRequest::limit_offset`); `None` on every
    /// other kind.
    pub limit_offset: Option<f64>,
    /// A limit order's ATTRIBUTES as placed (`vike_model::OrderRequest::attributes`); `None` on every other order.
    /// On the wire as `OrderViewWire::attributes`.
    pub attributes: Option<vike_model::OrderAttributes>,
    /// A trailing order's trail as a PERCENT of the extreme (`vike_model::OrderRequest::trail_percent`), in place of
    /// [`Self::trail`]; `None` on every other order.
    pub trail_percent: Option<f64>,
    /// A stop's ADJUSTMENT as placed (`vike_model::OrderRequest::adjust`); `None` on every other order.
    pub adjust: Option<vike_model::OrderStopAdjust>,
    /// The RELEASE an order rests at its venue with (`vike_model::OrderRequest::release`): a time release the venue
    /// holds itself (IBKR's `goodAfterTime`). `None` on every other order: the core sends an order it held with none.
    pub release: Option<vike_model::OrderRelease>,
    /// The quantity the venue SHOWS of the order as placed (`vike_model::OrderRequest::display_quantity`): an iceberg's
    /// slice, zero for a HIDDEN order; `None` shows all of it. On the wire as `OrderViewWire::display_quantity`.
    pub display_quantity: Option<f64>,
}

/// One order the core DENIED before any venue saw it ([`CoreSnapshot::denied_orders`]): the pre-trade gate's veto, a
/// mount's refused intent, a halted account. The denial as the core published it, with the venue and symbol of the
/// order it refused, which the event does not carry.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderDeniedView {
    /// Which denial of this core's run it is (1, 2, …), so a reader tells a new one from one it already saw.
    pub sequence: u64,
    pub venue: String,
    pub symbol: String,
    /// The `OrderDenied` itself: the order client id, the reason, the refusal's catalog id and the time.
    pub event: vike_model::events::OrderDenied,
}

/// One EMULATED order (decision 0275): an order the core holds itself because its venue does not, in
/// ONE list for every kind. A TRIGGER kind (`stop`, `trailing`, `take_profit`, `stop_limit`,
/// `trailing_limit`, `trailing_take_profit`) rests in the core's book until its trigger fires one plain
/// order; a WORKING kind (`chase` for a pegged order, `iceberg` for a visible size) keeps ONE plain limit
/// resting at a time, which `child` names and which is a row of `orders` itself. The emulated order is
/// listed once, here, never as a row of `orders`. Empty on every node that holds none.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEmulationView {
    /// What a cancel names: the arm id of a trigger kind, the order client id of a working one.
    pub id: String,
    pub venue: String,
    /// WHICH ACCOUNT, as [`OrderView::account`]: `None` for the default account.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    pub symbol: String,
    pub side: i32,
    /// Its whole size.
    pub qty: f64,
    /// The kind's word (above).
    pub kind: &'static str,
    /// Whether it may only reduce the position it meets.
    pub reduce_only: bool,
    /// A trigger kind's trigger level as it stands now (a trailing kind's moves with its extreme);
    /// `None` for a working kind.
    pub trigger: Option<f64>,
    /// The limit price: a stop-limit's or a limit-if-touched take-profit's, an iceberg's; `None` otherwise.
    pub price: Option<f64>,
    /// A trailing kind's distance; `None` otherwise.
    pub trail: Option<f64>,
    /// A chase's anchor and offset; `None` otherwise.
    pub peg: Option<vike_model::OrderPeg>,
    /// An iceberg's visible size; `None` otherwise.
    pub display_quantity: Option<f64>,
    /// A working kind's filled size (the sum of its children's fills) and average price (`0.0` while
    /// nothing filled); `0.0` for a trigger kind, which fills only as the order it fires.
    pub filled: f64,
    pub average_price: f64,
    /// A working kind's live child; `None` between two children and for a trigger kind.
    pub child: Option<String>,
    /// Why it acts on nothing now, in words (a stale or missing price, a restored order not yet
    /// confirmed, its strategy not mounted, its account not running); `None` while it works.
    pub waiting: Option<String>,
    /// A core-held stop's re-arms after its close was refused or only partly filled (`0` = as armed);
    /// `0` for every other kind.
    pub rearms: u8,
    /// A trigger kind RESTORED from the journal and HELD until its account's first reconcile pass (a crossing
    /// price does not fire it yet): set by the core where it holds the arm (`CoreThread::stop_holds`), never
    /// inferred from `waiting`, whose words may grow. `false` for every other row.
    pub held: bool,
}

/// One order the live runtime is HOLDING off the venue: a PENDING bracket exit until its OTO parent
/// fills (live-runtime OCO/OTO), or an order held until its [`vike_model::OrderRelease`] (`vike_core`'s
/// `held_release.rs`). These are NOT in `orders` — they have no registered order and no venue order id
/// yet — so the GUI would otherwise be blind to them. A bracket exit's `parent_order_id` names the entry
/// whose fill will release it; a time-held order's `release` says when it goes. Empty on every node that
/// holds neither.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderHeldView {
    pub order_client_id: String,
    pub venue: String,
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    pub order_type: OrderType,
    pub price: Option<f64>,
    pub trigger_price: Option<f64>,
    /// the OTO entry whose fill releases this exit
    pub parent_order_id: Option<String>,
    /// WHICH ACCOUNT's book the order goes to, as [`OrderView::account`]: `None` for the default
    /// account, and for a held bracket exit (its account is its entry's).
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// When a time-held order is released (its time, and a price condition when it states one);
    /// `None` for a held bracket exit, which its parent's fill releases.
    pub release: Option<vike_model::OrderRelease>,
    /// Why a held order whose time came still waits: its price condition has no FRESH price to read
    /// (stale, missing, or no engine runs its account), in words. `None` while nothing is said: its time
    /// has not come, its fresh price does not meet the level yet, or a held bracket exit.
    pub waiting: Option<String>,
    /// The held order's terms, as [`OrderView`]'s of the same names: copied from its request, `None` where it states
    /// none.
    pub attributes: Option<vike_model::OrderAttributes>,
    /// As [`OrderView::trail_percent`].
    pub trail_percent: Option<f64>,
    /// As [`OrderView::adjust`].
    pub adjust: Option<vike_model::OrderStopAdjust>,
    /// As [`OrderView::display_quantity`].
    pub display_quantity: Option<f64>,
}

/// One ARMED, CORE-HELD reduce-only stop (the amendment of 2026-10-27 to decision 0199, second one): an operator's
/// `submit --type stop --reduce-only` on a venue whose capability row has no reduce-only stop, held in the
/// NODE's memory and journal and not at the venue. Not in `orders` (no order exists until it fires).
/// Empty on every node that holds none, and on a replay core.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEmulationStopView {
    /// The arm id the arm note named; `cancel <id>` disarms the stop.
    pub arm_id: String,
    pub venue: String,
    pub symbol: String,
    /// The account the stop fires onto: its route key (the bare venue id for the default account).
    pub route_key: String,
    pub side: i32,
    /// The armed quantity (the fire sends at most the position).
    pub qty: f64,
    pub trigger_price: f64,
    /// Restored from the journal and HELD until its account's first reconcile pass has landed (or the
    /// restored-tag window lapses): a crossing price does not fire it yet.
    pub held: bool,
    /// How many times this stop was RE-ARMED after its close was refused or only partly filled
    /// (`0` = the stop as the operator armed it).
    pub rearms: u8,
}

/// One live mount's readiness view (portfolio-observer PR-4 T5) — the snapshot-facing twin of
/// the runtime's private per-mount `MountState`. `ready == false` means the mount is still
/// PENDING (see `CoreConfig::readiness_gate`): it is RECEIVING data — its strategy hook still
/// runs every tick/bar, so any warmup/estimator proceeds normally — but its buffered order
/// intents are DISCARDED before they ever reach the venue, because its (venue, symbol) has not
/// yet resolved a price (either side) through the engine's `PriceBoard`. When the gate is off
/// (`CoreConfig::readiness_gate: false`, the default) every mount is immediately `ready: true`.
///
/// The list this rides in ([`CoreSnapshot::mounts`]) carries ONE extra row when any mount exists: the
/// [`MountViewKind::Residual`] row — see that variant. Read `kind` before treating a row as a mount.
#[derive(Debug, Clone, PartialEq)]
pub struct MountView {
    /// whether this row describes a real mount or the account-vs-mounts RESIDUAL
    pub kind: MountViewKind,
    /// The id an update addresses this mount by (`crate::ParamsUpdate::mount_id`): the STORED,
    /// sanitized mount id (`vike_core::strategy_state::mount_id_with`), i.e. the sanitized
    /// `controller_id` or the derived `{venue}__{symbol}__{interval}`. Empty on the
    /// [`MountViewKind::Residual`] row, which is no mount and so cannot be addressed.
    pub mount_id: String,
    pub venue: String,
    pub symbol: String,
    pub interval: String,
    pub ready: bool,
    /// steal/core-per-mount-budget — the per-mount fill ATTRIBUTION + BUDGET view (read-only; never
    /// drives a fold decision). Signed net position attributed to this mount on its own `symbol`
    /// (0.0 until it fills). A declared leg's position is in no field here: sizes of two instruments
    /// do not add, while its money is in the three figures below.
    pub position: f64,
    /// cumulative realized PnL NET of fees attributed to this mount.
    pub realized_pnl: f64,
    /// resolver-marked unrealized PnL on this mount's attributed net position (0.0 flat/unpriceable).
    pub unrealized_pnl: f64,
    /// gross notional exposure (resolver-priced) of this mount's attributed net position.
    pub notional: f64,
    /// this mount's optional loss/notional budget (`None` = no per-mount latch).
    pub budget: Option<MountBudget>,
    /// `true` once this mount breached its budget and latched liquidate-only.
    pub latched: bool,
    /// This mount's own LIVE tunables, read straight off its strategy (`vike_model::Strategy`'s
    /// `params`), or `None` when that strategy publishes none — its default. The read-side twin of
    /// `Command::UpdateParams`: what comes back is the bag that command takes, so an operator
    /// surface can patch one knob and send the rest back unchanged instead of re-typing (and
    /// silently reverting) the whole object.
    ///
    /// It is the strategy's CURRENT state, not the boot config the mount was built from — after a
    /// re-tune the two disagree, and only this one is what the next tick prices from.
    ///
    /// Read at every publish by `crates/vike-core/src/runtime/mount_rows.rs`'s `mount_rows_unchanged`
    /// (and stored by `crates/vike-core/src/runtime/timers.rs`'s `mount_views` when a list is
    /// rebuilt) — per event on a sporadic feed, so NOT off the gated hop (`mount_views`' comment on
    /// this field argues why the gated harnesses never reach it). `None` on the [`MountViewKind::Residual`] row, which
    /// describes no mount.
    ///
    /// ⚠ Compared with `PartialEq`, the one field the reuse check cannot compare on bits: `-0.0`
    /// equals `0.0` there, so a knob that only flips the sign of a zero is not seen as a change and
    /// the published row keeps the old zero. A `NaN` knob compares unequal to itself, so such a
    /// mount just rebuilds its list every publish (slower, never stale).
    pub params: Option<vike_model::StrategyParams>,
    /// **The currency group a [`MountViewKind::Residual`] row is in** (decision 0183, item 13): the group key
    /// ([`VenueBlock::ledger_currency`], EMPTY for the dollar group) whose accounts and mounts the row adds. EMPTY on
    /// every [`MountViewKind::Mount`] row (a mount's figures are in its own account's money, which this row does
    /// not name) and on the one Residual row of a node whose engines share a currency, so such a node publishes
    /// the rows it always did.
    pub currency: Interned,
    /// **The mount's DECLARED legs** (one broker plan, step 12) as `(venue, symbol)`, each leg's venue
    /// resolved (a same-venue leg's is the mount's): what the host keeps a price lane and a bar series
    /// open for while the mount is published, as it does for the mount's own symbol. Built cold when
    /// the mount's symbols change and shared by `Arc::clone`; `None` for a mount that declares none,
    /// and on the [`MountViewKind::Residual`] row.
    pub legs: Option<std::sync::Arc<[(String, String)]>>,
}

/// What one [`MountView`] row describes (multi-mount durability, gap E).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MountViewKind {
    /// A real strategy mount: every field describes that mount's own attributed ledger.
    #[default]
    Mount,
    /// The RESIDUAL row — `Account.realized_pnl (net of fees)` MINUS the sum of every mount row's
    /// `realized_pnl`, i.e. the realized PnL that belongs to NO mount: manual operator tickets, the
    /// account-wide margin-call sweep, reconcile-synthesised or adopted venue orders, a settlement,
    /// and (across a restart) any fill whose originating mount could not be reconstructed. The
    /// per-mount budget latch's flatten is NOT here: it is attributed to its mount (journal v14). The
    /// same set is what never reaches a strategy's `on_fill` (decision 0116), so for a LIVE mount the
    /// ledger and the callback cannot disagree about whose fill it was. (A straggler of an UNMOUNTED
    /// mount still moves that tombstone's ledger, since `order_client_id_mount` outlives the slot, while delivery
    /// skips it.)
    ///
    /// It exists because that difference was previously invisible: attribution is keyed by order client id, an
    /// un-minted order client id is simply not attributed, and nothing summed the two sides — so
    /// `Σ MountView.realized_pnl ≠ Account.realized_pnl` systematically and silently. With the row,
    /// `Σ (mount rows) + residual == account realized (net)` is an INVARIANT anything can assert,
    /// and an unexpectedly large residual is a visible signal rather than a slow leak.
    ///
    /// Its `venue`/`symbol`/`interval` are EMPTY (it is account-wide, not a series), `position`/
    /// `unrealized_pnl`/`notional` are `0.0` (only realized PnL is reconcilable this way — an
    /// unattributed OPEN position has no basis in any ledger), `budget` is `None` and `latched`
    /// `false`. Appended LAST, and only when at least one mount exists — a mount-free core publishes
    /// an empty `mounts` vec exactly as before.
    ///
    /// ⚠ **One row per currency group** (decision 0183, item 13): a row adds the accounts and the mounts of ONE
    /// currency ([`MountView::currency`] names it, EMPTY for the dollar group), because an account's realized figure
    /// in euros minus a mount's in dollars is a number in no currency. The primary group's row comes first, then
    /// one more per other group in the order an engine of it first appears; a node whose engines share a
    /// currency publishes the one row it always did.
    Residual,
}

/// Optional per-mount risk BUDGET (steal/core-per-mount-budget) — the SCOPED counterpart to the
/// account-wide `vike_core::CoreConfig::max_drawdown` latch. When a mount's attributed loss or
/// gross notional exceeds its cap, the runtime latches THAT mount liquidate-only (cancels only its
/// own resting orders + optionally flattens its own attributed net position) while every OTHER
/// mount keeps trading — so a runaway experimental mount can no longer burn the shared account and
/// take the profitable maker's global drawdown latch down with it.
///
/// Supplied via `vike_core::CoreConfig::mount_budgets`, keyed by the mount's ID. Both LATCH arms
/// `None`/`<= 0.0` is INERT — treated as no budget, so it never latches and never even arms the
/// sweep (the byte-identical path). No `Default`: every budget states its [`Self::loss_action`],
/// whose one default is `vike_model::MountLossAction`'s (decision 0249 item 6).
///
/// Its third arm, [`Self::max_leverage`], is no latch: a PRE-trade refusal of the mount's opening
/// orders, judged where they enter (its doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MountBudget {
    /// Max tolerated cumulative LOSS (realized net of fees + resolver-marked unrealized), as a
    /// POSITIVE account-currency number, before this mount latches liquidate-only. `None` (or
    /// `<= 0.0`) disables the loss arm.
    pub max_loss: Option<f64>,
    /// Max tolerated GROSS notional exposure (resolver-priced) of this mount's attributed net
    /// position before it latches liquidate-only. `None` (or `<= 0.0`) disables the notional arm.
    pub max_notional: Option<f64>,
    /// What the latch does: `Flatten` ALSO submits a reduce-only MARKET to flatten the mount's
    /// attributed net position (journaled write-ahead like a margin-call liquidation), not just
    /// cancel its resting orders; `CancelOnly` leaves the position for the operator.
    pub loss_action: vike_model::MountLossAction,
    /// **The mount's LEVERAGE cap** (decision 0249's leverage arm, the mount grain under decision
    /// 0181's account figure): the mount's gross notional AFTER an opening order (every row of its
    /// OWN attributed ledger at its price and contract value, the order netted into its own row),
    /// divided by its account engine's SIZING equity (decision 0048's acting equity), may not exceed
    /// it. `None`, or a figure that is not finite and `>= 1.0`, is OFF.
    ///
    /// A SOFT PRE-trade budget and nothing else: [`crate::risk::judge_mount_leverage`] judges it where
    /// the mount's verbs enter (`vike_core`'s `drain_broker`, and the backtest's
    /// `OrderBrokerSimulator::leverage_refusal`), each verb ALONE against the mount's own rows, so several
    /// verbs of one callback are not summed and orders resting from earlier ones are not counted (the
    /// engine's `RiskGate` is the account's hard limit). A verb over it is refused by name
    /// ([`crate::risk::MOUNT_LEVERAGE_REFUSAL`]); a covered reduce of the mount's own row, a cancel, a
    /// size-only decrease and a mass cancel pass. An amend that grows an order is judged on its growth; an
    /// amend that names a price is judged at its new price on what it will rest. It is
    /// NEVER a latch (no arm of [`Self::is_active`]), never a venue write, and not part of `RiskLimits`,
    /// so no `state_hash` or journal byte moves.
    pub max_leverage: Option<f64>,
}

impl MountBudget {
    /// Whether either LATCH arm (`max_loss`, `max_notional`) is a positive cap. A budget with both
    /// `None`/`<= 0.0` never latches — treated as absent (keeps `any_mount_budget` false, i.e. the
    /// byte-identical path).
    ///
    /// ⚠ [`Self::max_leverage`] is deliberately NOT an arm of it: the leverage cap refuses an order
    /// before it is sent and never latches, so a budget that states it alone arms no sweep
    /// (`sweep_mount_budgets_in` skips it), latches nothing and restores no latch at a start.
    pub fn is_active(&self) -> bool {
        self.max_loss.is_some_and(|v| v > 0.0) || self.max_notional.is_some_and(|v| v > 0.0)
    }

    /// Whether a mount at `loss` (a POSITIVE figure is a loss) and gross `notional` breaches a LATCH arm:
    /// past a positive [`Self::max_loss`] or [`Self::max_notional`]; an arm `None` or `<= 0.0` is off, and
    /// a NaN figure breaches nothing. The ONE breach rule a mount latches by (`vike_core`'s
    /// `sweep_mount_budgets_in`), here so the backtest latches by it too. [`Self::max_leverage`] is no
    /// part of it.
    pub fn breached(&self, loss: f64, notional: f64) -> bool {
        self.max_loss.is_some_and(|ml| ml > 0.0 && loss > ml)
            || self.max_notional.is_some_and(|mn| mn > 0.0 && notional > mn)
    }
}

/// GUI-facing projection of one held (Quarantine / Hybrid-quarantined) `crate::recon::ReconAlert`
/// (Task 17) — kind/detail/count only, NEVER the raw proposed `Event`s (those stay fold-thread
/// state until an operator confirms; see `Command::ConfirmReconcile`). `id` is the key an operator
/// confirms by (`CoreThread::reconcile_reports` assigns it: the digest of the divergence's identity,
/// stable across a restart - see the `Command::ConfirmReconcile` doc).
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileAlertView {
    pub id: u64,
    /// The held divergence's kind. Its word (the variant name) is what the observe wire carries
    /// (`vike_tradehub_client::wire::ReconcileAlertViewWire::kind`).
    pub kind: vike_model::ReconcileDivergenceKind,
    pub detail: String,
    /// how many events are held pending confirm (never the events themselves).
    pub proposed_event_count: usize,
    /// **WHICH BOOK this divergence is about** — the raising pass's route key
    /// (`crate::ReconcileReports::route_key`), or the venue itself for a venue with one
    /// account, which is every venue on a box with no `[accounts]` table.
    ///
    /// ⚠ It exists because this projection is the whole of what an operator SEES, and it carried
    /// kind/detail/count and nothing else. At fifty accounts of one exchange that is fifty rows
    /// reading `PositionDrift` with no way to tell which account each belongs to — and confirming
    /// one of them moves one specific book. The confirm still routes on the fold thread's own
    /// stored key; this is the label that lets somebody decide WHICH id to confirm.
    pub account: String,
    /// **The instrument the row's proposed fills book, and their signed net quantity** (`None` and `0.0` for a
    /// row that proposes no fill). The node's confirm door reads them: a confirm whose symbol the account's
    /// engine does not trade has it admitted first, through the door's own symbol plan
    /// (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `plan_confirm_admission`). Not on the observe wire.
    pub fill_symbol: Option<crate::Interned>,
    pub fill_qty: f64,
}

/// Reconciliation state block (Task 17): every currently-held alert awaiting an operator
/// `Command::ConfirmReconcile`, plus the wall-clock ms of the most recently completed reconcile pass
/// (0 before the first pass ever runs). Empty/zero by default — a core that never reconciles, or
/// whose policy is fully `Synthesize` (today's default), publishes this byte-identically empty.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReconcileBlock {
    pub alerts: Vec<ReconcileAlertView>,
    pub last_pass_timestamp: i64,
}

/// Per-venue ledger block (cross-venue runtime): one per engine, primary included, so the
/// GUI renders every venue uniformly while the scalar fields keep mirroring the primary.
#[derive(Debug, Clone, PartialEq)]
pub struct VenueBlock {
    pub venue: String,
    /// **WHICH ACCOUNT of `venue`** — `None` for the unlabelled one, the convention
    /// `vike_model::accounts::account_keys::AccountLabel` states for every carrier of that type.
    ///
    /// `vike_core::snapshot::build` derives it from the engine's own `route_key` through
    /// `vike_model::accounts::account_keys::label_of_route_key`, so on a block built there it cannot disagree
    /// with [`Self::route_key`] about which account this block is.
    ///
    /// ⚠ That is a property of the BUILDER, not of the type. A block the desktop's observe bridge
    /// rebuilds from the wire reads the label and the route key as two separate wire fields, so a
    /// skewed node could make them disagree; a reader that needs the ROUTING answer derives it from
    /// `route_key` through the same inverse and never trusts this field for it.
    ///
    /// `None` on every single-account process — which is every process with no `[accounts]` table —
    /// so a consumer that ignores it reads what it read before the field existed.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// **This engine's own ROUTING key** — `route_key_of(venue, account)`, i.e. the bare venue id
    /// for the default account and `venue#LABEL` for a labelled one.
    ///
    /// ⚠ **This is the field a gate must compare against, never [`Self::venue`]**, and that is the
    /// whole reason it is published. A two-account node emits two blocks whose `venue` is the same
    /// string, so `venues[].venue` is evidence that cannot tell two books apart — the defect
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s *What would reopen
    /// this* named in advance ("A second account of one exchange is mounted"). `venue` keeps doing
    /// the CAPABILITY job it has always done (`vike_venues::capabilities_for` and every table keyed on
    /// `vike_model::VENUES`); this does the routing one.
    ///
    /// Equal to `venue` on a single-account process, by construction rather than by care — see
    /// `vike_model::accounts::account_keys::AccountRef::route_key`.
    pub route_key: String,
    /// **The symbol this engine was mounted for.** `crate::Engine::symbol`, the first
    /// half of the set `accepts_symbol` answers for. EMPTY only on a block rebuilt from a node that
    /// did not publish it (the desktop's observe bridge reading an older node), which a reader
    /// treats as "not said", never as "trades nothing".
    pub symbol: String,
    /// **The other symbols it trades** — the engine's `extra_symbols` verbatim. Empty for an engine
    /// `vike_tradehub_core::make_engine` builds (it sets none), which is every venue mount; **not empty
    /// everywhere**: the paper multi-strategy builder (`vike_tradehub_core`'s
    /// `build_paper_multi_strategy_core_with`) fills it, and runtime combo admission
    /// (`CoreThread::lower_combo`) appends every new leg symbol while nothing prunes the list. It
    /// also persists through `crate::EngineSnapshot`. So wherever a combo producer exists the
    /// list only grows, and the node's wire `symbols` list carries them all in every frame. No
    /// production code constructs `OrderIntent::Combo` today, so that is latent. A [`SymbolList`]
    /// because `vike_core::snapshot::build` runs per EVENT: the engine keeps one shared list until
    /// its symbols change (`crate::Engine::published_extra_symbols`), so a publish copies a
    /// pointer, and an empty list holds no allocation at all.
    pub extra_symbols: SymbolList,
    /// **The tier of the account this engine runs** — `crate::Engine::tier`, and so what
    /// its orders go to (decision 0156: an account runs at its tier or not at all).
    /// `vike_core::snapshot::build` always says. `None` only on a block rebuilt from a node that
    /// did not publish it: drawn unknown and treated as `live` wherever safety is at stake.
    ///
    /// ⚠ **`Some(Paper)` is what an engine says until something says otherwise, so it is not proof
    /// that no real money is behind it.** The snapshot an engine is restored from carries its tier
    /// only when the engine was told one (`crate::EngineSnapshot::tier`): `crate::Engine::from_snapshot`
    /// of a snapshot written before that field rebuilds through `Engine::new`, so such an
    /// engine reports `Paper` until its caller sets `tier`. Every engine `vike_tradehub_core`
    /// builds is told explicitly (`Engine::set_tier`); a caller that restores one around a
    /// real client must tell it too, or this field understates what an order sent to it will do.
    pub tier: Option<vike_model::AccountTier>,
    pub balance: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub funding_paid: f64,
    pub balance_mode: BalanceMode,
    /// mode-aware equity at THIS venue's seed (resolver-priced — see `resolve_equity`)
    pub equity: f64,
    /// resolver-priced unrealized total for this venue (`PriceBoardEquity::unrealized_total`)
    pub unrealized: f64,
    /// open positions on this venue with no priceable source (`PriceBoardEquity::missing`)
    pub missing_prices: u32,
    /// margin currently locked by open positions on this venue (0.0 when the gate is off)
    pub margin_used: f64,
    /// free buying power (`vike_model::free_buying_power`) as the gate judges it: the equity under
    /// `policy.max_sizing_equity`, less the margin of the open positions AND of the live un-filled orders
    /// (`margin_used` above holds the positions alone); equals `equity` (floored at 0.0) when the gate is off
    pub free_buying_power: f64,
    /// `margin_used / equity` (0.0 when the gate is off or equity ≤ 0)
    pub margin_ratio: f64,
    /// the venue's effective per-order fee schedule (fee model follow-up 1) — the LIVE
    /// account-actual maker/taker the mount fetched when available, else the static published
    /// default; `None` for an engine the mount never tagged (GUI-only / test). Surfaced so the GUI
    /// can display real transaction costs; read-only, never drives the fold.
    pub fee_schedule: Option<vike_model::FeeSchedule>,
    pub trading_state: TradingState,
    /// This venue's per-symbol contract-value grid (notional kind plus per-unit factor, decision
    /// 0170), shared with the engine's `Account` by pointer-copy (`Arc::clone` — no allocation, no
    /// deep clone, on a publish path that runs per event on a sporadic feed). Read through
    /// [`VenueBlock::contract_value_of`], never directly: a symbol ABSENT from this map resolves to
    /// `contract_value_default`, not to nothing.
    ///
    /// Why the whole grid and not a field on `PositionView`: the GUI needs the multiplier for
    /// symbols it holds NO position in — the deribit options confirm-ticket prices a contract the
    /// account has never traded — so hanging it off a position would leave exactly the motivating
    /// case unanswerable.
    pub contract_values: std::sync::Arc<indexmap::IndexMap<String, vike_model::ContractValue>>,
    /// the fallback value for any symbol absent from `contract_values` (`Account`'s legacy scalar as
    /// a `Linear` unit; 1.0 for every engine `vike-tradehub-core` builds today).
    pub contract_value_default: vike_model::ContractValue,
    /// **This engine takes an order on a symbol it was not started on, by admitting the symbol
    /// first** (decision 0173/0174): it fails closed on an unadmitted one
    /// ([`vike_model::RiskLimits::deny_unadmitted_symbols`], set by the mount from the node door's
    /// `SYMBOL_RULES` `Admits` rows, so the door, the engine and this capability are ONE table).
    /// The node publishes it and a client reads it as THE capability: `false` is both "this venue
    /// does not" and "this node predates the field", and a client treats both as one symbol only.
    pub admits_symbols: bool,
    /// **The conversion rate of every currency this account holds a foreign-quoted symbol in**, with
    /// its age: `account currency per one unit of the quote currency` and the instant it was
    /// observed. EMPTY (no allocation) for every account without such a symbol, which is every
    /// account on a venue quoted in its own currency. [`Self::contract_values`] already carries the
    /// rate multiplied in; this table is how a reader says WHICH rate that was and whether it has
    /// gone stale. Read [`Self::quote_rates_stale`], not a field, for the one-bit answer.
    pub quote_rates: Vec<QuoteRateView>,
    /// The currency [`Self::quote_rates`] is expressed in (the engine's `quote_asset`); EMPTY exactly
    /// when the table is.
    pub quote_rates_in: Interned,
    /// **The currency GROUP this block's money is in** (`Engine::ledger_currency_key`): EMPTY for
    /// the dollar group (`USD`, `USDT`, and an account whose currency nobody told: every account of
    /// today's nodes), else the upper-cased code (`"EUR"`). Every figure of the block is in that
    /// currency, so a total ACROSS blocks may add only the blocks of one group: [`Portfolio`]'s totals
    /// fold the PRIMARY block's group (the first block's), and [`Portfolio::ledger_totals`] answers one
    /// row per group. EMPTY also on a block rebuilt from a node that did not publish it, which reads as
    /// the dollar group. ⚠ That is the only reading such a frame offers, not a statement that the node held
    /// only dollars: v0.1.79 to v0.1.81 told an OANDA demo account with an exposure ceiling its currency
    /// (`set_account_currency`), so an older node can hold a EUR ledger beside USDT. A new observer on it reads
    /// ONE group and adds them as v0.1.81's observer did (no regression, still the node's old mixed sum).
    pub ledger_currency: Interned,
    /// **`true` when the currency this block's ledger is anchored to ([`Self::ledger_currency`], or the dollar group
    /// when that is empty) is only the owner's ASSUMED USD**, not the venue's word (decision 0189: the venue
    /// stated none, or the read failed, and the node anchored the ledger to USD for a `demo` account with an
    /// exposure ceiling). `Engine::account_currency_assumed`, a bool copy per publish. PRESENTATION only:
    /// nothing folds on it, it is not journaled, and it is `false` for every engine that was not told an
    /// assumption (a stated currency, an account that does not convert, a block rebuilt from a node that did not
    /// publish it). ⚠ `false` is not proof the currency was stated: an account that is not anchored at all
    /// (`live`, `paper`, no ceiling) keeps the label it has and says `false` too. ⚠ It says how the label was
    /// DECIDED and stays `true` for the engine's life: the venue confirming the assumed USD later lifts the
    /// node's balance hold (a separate row) and does not clear this.
    pub currency_assumed: bool,
    /// Fills the account REFUSED because their `trade_id` was already folded under a different
    /// `(symbol, side, qty, px)` (`Account::colliding_fills_refused`): a genuine fill was dropped, so
    /// any nonzero value is a fault. A health figure for the core's own readers: the observe wire
    /// does not carry it.
    pub colliding_fills_refused: u64,
    /// Fees, funding amounts and forced-close fees this account left out of its ledger because their
    /// currency could not be converted (`Account::fees_unconverted`); `0` on an account with no
    /// foreign-quoted symbol. A health figure for the core's own readers: the observe wire does not
    /// carry it.
    pub fees_unconverted: u64,
    /// Conversions booked at a rate that was already stale when the event happened, or that stated no
    /// time (`Account::fees_at_stale_rate`); `0` likewise. Not carried by the observe wire.
    pub fees_at_stale_rate: u64,
    /// **The account code this block's engine stamps into its order client ids** (decision 0270,
    /// `crate::Engine::account_code`), for a client that mints an id for this account. `None` = not
    /// said (an engine nobody told its code, or a block rebuilt from a node that did not publish it): a client
    /// mints `00`, a label that decides nothing.
    pub account_code: Option<vike_model::orders::client_id::OrderClientIdAccountCode>,
    /// **The id shape a CLIENT mints for this block's account** (decision 0270: the full shape everywhere but
    /// Binance's fitted compact one; a client never carries an instance tag). `None` = not said: a client mints
    /// the full shape, which a venue that cannot take it receives bare (the Binance bridge never truncates).
    pub order_client_id_shape: Option<vike_model::orders::client_id::OrderClientIdShape>,
    pub positions: Vec<PositionView>,
}

/// One row of [`VenueBlock::quote_rates`]. `Copy`, like every published row that names things: the
/// currency is an [`Interned`] code, so a publish copies the table and allocates nothing for names.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuoteRateView {
    /// The quote currency of one or more symbols of the account ("JPY").
    pub currency: Interned,
    /// Account-currency units per ONE unit of [`Self::currency`] (USDJPY on a USD account: about `1/150`).
    pub rate: f64,
    /// When the node observed it (the clock the admission that carried it was stamped with).
    pub as_of_ms: i64,
    /// Older than the staleness window at the instant of this publish, or never priced. While true
    /// an OPENING order on a symbol quoted in [`Self::currency`] is denied (`quote-rate-stale`), a reducing
    /// one is not, and the valuation keeps using the last rate.
    pub stale: bool,
    /// **A position is open in a symbol quoted in [`Self::currency`]**: the stale rate is valuing money, not only
    /// refusing a new order. Judged by `Account::quote_rate_table` ONLY for a row that is `stale` (a fresh
    /// rate needs no flag, so the common publish pays nothing for it), `false` on every other row. This is
    /// what lets a `trade health` row and an alert speak of a HELD position and stay quiet about a rate nobody
    /// holds anything in (a currency admitted once and never traded again ages out for ever).
    pub held: bool,
}

/// A FLAT, unnamed block: every scalar zero, no positions, and the two enums set to the same
/// values [`CoreSnapshot::empty`] publishes before the first real build (`BalanceMode::Delta`,
/// `TradingState::Active`). Exists so a consumer TEST can name only the two or three fields it is
/// about — `VenueBlock { venue: "bybit".into(), realized_pnl: -450.0, ..Default::default() }` —
/// instead of restating eighteen; `vike-alerting` in particular cannot spell `BalanceMode` at all
/// (it has no `vike-execution` edge), so without this it could not build one.
///
/// ⚠ NOT a valid published block: `venue` is EMPTY, which no real engine ever is. Never construct
/// a snapshot for a consumer to render from this — `vike_core::snapshot::build` is the only
/// producer.
///
/// ⚠ [`VenueBlock::route_key`] is therefore EMPTY here too, and that is the consistent answer
/// rather than a gap: the spec's *"a `route_key` equal to its venue"* is satisfied only vacuously
/// on a block whose venue is itself empty, and spelling anything else would make the default block
/// claim a routing identity no engine has. [`VenueBlock::account`] is `None`, the absent/default
/// shape a single-account process publishes.
///
/// ⚠ [`VenueBlock::symbol`] is EMPTY and [`VenueBlock::tier`] is `None` for the same reason: a block
/// that names no engine names nothing that engine trades and nothing that stands behind it. A reader
/// treats both as "not said" — [`VenueBlock::trades`] answers `false` for every symbol.
impl Default for VenueBlock {
    fn default() -> Self {
        VenueBlock {
            venue: String::new(),
            account: None,
            route_key: String::new(),
            symbol: String::new(),
            extra_symbols: SymbolList::default(),
            tier: None,
            balance: 0.0,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            funding_paid: 0.0,
            balance_mode: BalanceMode::Delta,
            equity: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
            margin_used: 0.0,
            free_buying_power: 0.0,
            margin_ratio: 0.0,
            fee_schedule: None,
            trading_state: TradingState::Active,
            contract_values: std::sync::Arc::new(indexmap::IndexMap::new()),
            contract_value_default: vike_model::ContractValue::default(),
            admits_symbols: false,
            quote_rates: Vec::new(),
            quote_rates_in: Interned::default(),
            ledger_currency: Interned::default(),
            currency_assumed: false,
            colliding_fills_refused: 0,
            fees_unconverted: 0,
            fees_at_stale_rate: 0,
            account_code: None,
            order_client_id_shape: None,
            positions: Vec::new(),
        }
    }
}

impl VenueBlock {
    /// Whether any conversion rate behind this block's valuations is stale (see
    /// [`QuoteRateView::stale`]). `false` when the table is empty.
    pub fn quote_rates_stale(&self) -> bool {
        self.quote_rates.iter().any(|r| r.stale)
    }

    /// What one unit of `symbol`'s quantity is worth on this venue — the exact
    /// `Account::contract_value_of` authority, resolvable for ANY symbol (position or not), falling
    /// back to `contract_value_default` for one absent from the grid.
    pub fn contract_value_of(&self, symbol: &str) -> vike_model::ContractValue {
        self.contract_values.get(symbol).copied().unwrap_or(self.contract_value_default)
    }

    /// Whether this block's engine trades `symbol`: the engine's own
    /// `crate::Engine::accepts_symbol`, answered from what the block carries.
    ///
    /// ⚠ **`false` does NOT always mean "this account does not trade it".** A block that names no
    /// symbol ([`VenueBlock::symbol`] empty: a node that did not say, or a default block) answers
    /// `false` for EVERY `symbol`, while an empty `VenueBlock::symbol` means "not said" and never
    /// "trades nothing" — this method cannot tell the two apart. So every reader checks
    /// `block.symbol.is_empty()` FIRST and decides what "not said" allows; only a block that did say
    /// a symbol gives `false` its literal meaning. An empty `symbol` argument is `false` too.
    pub fn trades(&self, symbol: &str) -> bool {
        !symbol.is_empty()
            && (self.symbol == symbol || self.extra_symbols.iter().any(|s| s == symbol))
    }
}

/// **One account the node knows and does NOT run** (decision 0156: an account runs at the tier its
/// active row states, or not at all — never a paper engine in its place). No engine stands behind
/// it, so it has no [`VenueBlock`]; this is what a reader shows instead of a bare "not running",
/// and what the core names when it refuses an order addressed to it.
///
/// Built by `vike_tradehub_core` (its `NotRunning::view`) from the mount's own answer: [`Self::block`] is
/// the [`vike_model::ArmingBlock`] that stopped it and [`Self::reason`] its badge for a person
/// (`ArmingBlock::as_str`: `no credentials`, `flags.tradehub_live off`, …), which the node may extend
/// with what the account still holds. A program reads the block, never the badge.
///
/// The account is ONE typed value (owner ruling 2026-11-12, variant 2B): a roster account, or one this
/// build cannot name (a newer node's venue), kept as received so a reader still shows it. Its routing
/// key is computed, never stored ([`Self::route_key`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmingNotRunningView {
    /// Which account: venue and label.
    pub account: vike_model::accounts::account_keys::AccountNamed,
    /// The tier its active `account` row states; `None` when no active row states exactly one.
    pub tier: Option<vike_model::AccountTier>,
    /// Why it does not run, in words for a person: the arming block's badge, or the node's own sentence.
    pub reason: String,
    /// The arming block that stopped it; `None` when the node stopped it for a cause no block names (a
    /// restart at another tier, a remount, a vanished lane twin, restored state no account runs).
    pub block: Option<vike_model::ArmingBlock>,
    /// WHICH refusal [`Self::reason`] is when it is the node's own sentence (its catalog id), shown with it
    /// once, never decided on ([`Self::block`] is the key). `None` for a block's badge, which has no id, and in a
    /// client's view, whose `reason` already reads under its id (`vike_tradehub_client::observe_decode`).
    pub refusal_id: Option<String>,
}

impl ArmingNotRunningView {
    /// The ROUTING key an order addressed to it names (the bare venue for the default account), by the
    /// one rule (`vike_model::accounts::account_keys::route_key_of`).
    #[must_use]
    pub fn route_key(&self) -> String {
        self.account.route_key()
    }
}

/// **The core's health counters**, counted since the process started, summed over every engine of
/// the node. One name each (plan S14, the owner's "ok" of 2026-11-23): the same in the engine's
/// fields ([`crate::Engine`]), here, in [`COUNTER_NAMES`] and on the node's frame
/// (`vike_tradehub_client::wire::CoreCountersWire`). The core fills it at publish
/// (`crates/vike-core/src/runtime/publish.rs`'s `collect_counters`); nothing here counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoreCounters {
    /// Market updates replaced by a newer one for the same symbol before the core read them
    /// (latest-wins conflation in the market lane: working as designed, not a fault).
    pub market_updates_dropped_conflated: u64,
    /// Commands refused because the core's command queue was full.
    pub commands_rejected_queue_full: u64,
    /// Venue liveness or fill events dropped because the order was already closed by a kill path:
    /// the signature of a premature terminal that may leave a real venue position stranded.
    pub events_dropped_order_already_closed: u64,
    /// Closing events that could not apply while the order was still live (a lost terminal).
    pub closing_events_dropped_order_still_live: u64,
    /// Order events whose order client id the engine does not hold (an order of another session or
    /// account, or one from before a restart that reconcile did not bring back).
    pub events_dropped_order_unknown: u64,
    /// ⚠ Money-lane events refused for carrying a NaN or an infinity. The one counter with no innocent
    /// reading: no venue sends one, so a nonzero value is a venue fault or a compromised feed.
    pub events_dropped_number_not_finite: u64,
    /// Restored orders the first reconcile pass found resting and adopted back into the registry.
    pub restored_orders_adopted: u64,
    /// Restored orders that pass found neither resting nor filled.
    pub restored_orders_gone: u64,
    /// Restored orders that pass found filled while the process was down.
    pub restored_orders_filled_while_down: u64,
    /// Tagged submits that re-pointed a strategy tag whose older order was still live.
    pub tags_repointed_order_still_live: u64,
}

/// The published name of every [`CoreCounters`] field, in field order: what a surface prints and
/// the key the node's frame carries.
pub const COUNTER_NAMES: [&str; 10] = [
    "market_updates_dropped_conflated",
    "commands_rejected_queue_full",
    "events_dropped_order_already_closed",
    "closing_events_dropped_order_still_live",
    "events_dropped_order_unknown",
    "events_dropped_number_not_finite",
    "restored_orders_adopted",
    "restored_orders_gone",
    "restored_orders_filled_while_down",
    "tags_repointed_order_still_live",
];

impl CoreCounters {
    /// `(name, value)` per counter, in [`COUNTER_NAMES`] order.
    #[must_use]
    pub fn named(&self) -> [(&'static str, u64); 10] {
        let CoreCounters {
            market_updates_dropped_conflated,
            commands_rejected_queue_full,
            events_dropped_order_already_closed,
            closing_events_dropped_order_still_live,
            events_dropped_order_unknown,
            events_dropped_number_not_finite,
            restored_orders_adopted,
            restored_orders_gone,
            restored_orders_filled_while_down,
            tags_repointed_order_still_live,
        } = *self;
        let values = [
            market_updates_dropped_conflated,
            commands_rejected_queue_full,
            events_dropped_order_already_closed,
            closing_events_dropped_order_still_live,
            events_dropped_order_unknown,
            events_dropped_number_not_finite,
            restored_orders_adopted,
            restored_orders_gone,
            restored_orders_filled_while_down,
            tags_repointed_order_still_live,
        ];
        std::array::from_fn(|i| (COUNTER_NAMES[i], values[i]))
    }
}

/// One immutable core-state view. `seq` increments per publish (GUI change detection).
#[derive(Debug, Clone)]
pub struct CoreSnapshot {
    pub sequence: u64,
    pub venue: String,
    pub symbol: String,
    pub trading_state: TradingState,
    pub balance: f64,
    pub balance_mode: BalanceMode,
    /// The named live cross-venue aggregator (F24 / portfolio read-model spec): the equity totals
    /// and per-venue `venues` rows that used to sit loose here, now grouped under one type. Purely
    /// a naming reshape — every value is computed exactly as before. GUI reads `snap.portfolio.*`.
    pub portfolio: Portfolio,
    pub positions: Vec<PositionView>,
    pub marks: Vec<(Interned, Interned, f64)>,
    /// registry in insertion order
    ///
    /// ⚠ Each row is SHARED between publishes: the core keeps the published list between them and rebuilds a row
    /// only when its order was written (`crates/vike-core/src/snapshot/order_window.rs`), so every snapshot holds
    /// the same `Arc` of an order that did not move. Read a row as immutable; a reader that needs to change one
    /// copies it out (`Arc::make_mut`, `OrderView::clone`).
    pub orders: Vec<std::sync::Arc<OrderView>>,
    /// pending bracket exits held off the venue until their OTO parent fills (live-runtime OCO/OTO);
    /// empty on every non-bracket run. NOT part of `orders` (they have no registered/venue order).
    pub held_exits: Vec<OrderHeldView>,
    /// the operator's armed CORE-HELD reduce-only stops ([`OrderEmulationStopView`]); empty on every node that
    /// holds none. Overlaid at the publish site (like `reconcile_coin_deltas`), not filled by `build`.
    pub core_held_stops: Vec<OrderEmulationStopView>,
    /// the EMULATED orders the core works ([`OrderEmulationView`], decision 0275); empty on every node that
    /// emulates none. Overlaid at the publish site, like `core_held_stops`, not filled by `build`.
    pub emulated_orders: Vec<OrderEmulationView>,
    /// the core-owned bar cache: (venue, symbol, interval) -> closed bars (Arc-shared,
    /// pointer-copy per snapshot) + the forming bar. The GUI converts to its render
    /// model at this boundary (R5c).
    pub bars: indexmap::IndexMap<crate::SeriesKey, crate::BarSeries>,
    /// one [`MountView`] per live strategy mount (portfolio-observer PR-4 T5), primary mount
    /// first then `extra_mounts` in registration order — empty only before the first build or
    /// when nothing is mounted. `ready: true` for every mount when
    /// `CoreConfig::readiness_gate` is off (the default).
    ///
    /// The LAST row is the [`MountViewKind::Residual`] one (gap E) whenever any mount exists, so
    /// `Σ realized_pnl` over this whole list equals the account's realized PnL net of fees. Filter on
    /// `kind` before treating rows as mounts.
    ///
    /// ⚠ SHARED between publishes: while no row's numbers moved (and no mount came or went) the
    /// core hands every snapshot the SAME `Arc` instead of building and copying the list again
    /// (`crates/vike-core/src/runtime/mount_rows.rs`'s `published_mount_rows`). Read it as an
    /// immutable list; a reader that needs to change it copies out (`.to_vec()`).
    pub mounts: std::sync::Arc<[MountView]>,
    /// most recent delivered exec events (bounded journal feed for the GUI)
    pub recent_events: Vec<std::sync::Arc<str>>,
    /// The newest orders the core DENIED before any venue saw them, oldest first, at most the core's bound
    /// (`crates/vike-core/src/runtime/journaling.rs`'s `DENIED_ORDERS_KEPT`): a typed reader's source for a veto,
    /// which no order row carries (a denied order never enters a registry) and `recent_events` says only as text.
    /// Overlaid at the publish site, like `counters`, not filled by `build`. `None` until the core denies an order, so a
    /// publish before that costs nothing, not even a refcount.
    ///
    /// ⚠ SHARED between publishes (one refcount per publish) and IN-PROCESS only: not on the wire, so an observing
    /// client reads `None`.
    pub denied_orders: Option<std::sync::Arc<[std::sync::Arc<OrderDeniedView>]>>,
    /// set once a handler panicked — the core is HALTED in safe-state (see runtime.rs) — or once its command
    /// journal could not write (HALTED too, nothing cancelled: decision 0279's addendum of 2026-11-23)
    pub fault: Option<String>,
    /// `true` once the core HALTED because its command journal could not write (decision 0279's addendum of
    /// 2026-11-23; [`Self::fault`] says it in words), for the rest of the process: the typed fact a reader decides on
    /// (the tradehub's `journal_fence_halt` alert), never the words. Overlaid at the publish site, like `counters`.
    ///
    /// ⚠ IN-PROCESS only, like [`Self::denied_orders`]: not on the wire, so an observing client reads `false`.
    pub journal_fence_halt: bool,
    /// The core's health counters ([`CoreCounters`]). Overlaid at the publish site, like
    /// `core_held_stops`, not filled by `build`.
    pub counters: CoreCounters,
    /// Task 17: held Quarantine/Hybrid-quarantined reconcile alerts awaiting operator confirm,
    /// plus the last reconcile-pass timestamp. Empty/zero when nothing has ever been quarantined.
    pub reconcile: ReconcileBlock,
    /// Latest venue-REPORTED per-position coin delta from the reconcile path, keyed `(venue,
    /// symbol)` (Wave 5d). Populated ONLY for a venue whose `PositionStatusReport` carries a
    /// `delta` — Deribit today (`get_positions.delta`, correct for inverse contracts); every other
    /// venue leaves this empty and byte-identical. The greeks tool reads it (via
    /// [`Self::coin_delta`]) to fold a Deribit perp/future hedge leg into net portfolio greeks
    /// (`coin_delta × spot`) — `PositionView` is FILLS-derived and carries no venue delta, so the
    /// coin delta reaches the GUI through this side map instead. Refreshed each reconcile pass;
    /// cloned at every publish (per event on a sporadic feed — a no-op clone while it is empty).
    ///
    /// ⚠ **The key is per VENUE, not per ACCOUNT, and that is a declared residual rather than an
    /// oversight** — two accounts of one exchange on one instrument overwrite each other here.
    /// The argument for leaving it (the consumer has no account handle to look one up with, so a
    /// per-account key would un-price every leg instead of pricing it wrongly) and what would
    /// close it are at the write site, `CoreThread::reconcile_reports`.
    pub reconcile_coin_deltas: indexmap::IndexMap<(String, String), f64>,
    /// **WHICH SET OF ACCOUNTS this node is mounting** — a value that CHANGES whenever the mounted
    /// account set does, so a client holding an older view of this node can tell that it is stale
    /// (the account-routing spec's §6.3; the property kept from the opaque-handle candidate it
    /// refused, bought for one integer).
    ///
    /// `0` means *no fold has happened yet* — [`CoreSnapshot::empty`], the placeholder published
    /// before the first real build, where `venues` is empty and there is no account set to describe.
    /// A real build never publishes `0`.
    ///
    /// # ⚠ It is a DIGEST of the account set, not a counter, and the spec says "monotonically increasing"
    ///
    /// A declared deviation, with the argument, because the word matters at the other end. The
    /// account set of a `CoreThread` is FIXED at `assemble_core` and nothing pushes to
    /// `extra_engines` afterwards, so an in-process counter would never advance — its only real job
    /// is catching a RESTART that came back with a different set, and a counter starting at `0` on
    /// every boot publishes `0` for two different account sets, which is precisely the staleness the
    /// field exists to detect. A digest over the sorted route keys answers that question and a
    /// counter cannot.
    ///
    /// What it costs: a consumer must compare for INEQUALITY, never for ordering — "this is newer
    /// than that" is not a question this value can answer. §6.3's own use ("a confirm carrying a
    /// stale epoch is REFUSED") is an equality check, so nothing it was specified for is lost. The
    /// alternative — a counter plus a separate set digest — is two fields where one answers, and is
    /// the swap to make if an ordering consumer ever appears.
    ///
    /// The digest is FNV-1a (`vike_utility::fnv`, a frozen algorithm), fed by
    /// `vike_core::snapshot::accounts_epoch_of` rather than taken from `std::hash`: `DefaultHasher`'s algorithm is explicitly not guaranteed stable
    /// across Rust releases, and a value compared across a node upgrade must not change because
    /// the toolchain did.
    pub accounts_epoch: u64,
    /// **Every account the node knows and does NOT run, each with why** (decision 0156) — the
    /// accounts that have no [`VenueBlock`] in [`Portfolio::venues`] because no engine stands
    /// behind them.
    ///
    /// ⚠ **The core itself publishes it EMPTY, and that is deliberate.** The list is fixed when the
    /// node is built and the core keeps its own copy for its refusals (`vike_core::CoreConfig`'s
    /// `not_running`); copying it into every publish would be work on the latency-gated hop for a
    /// list no in-process reader needs. The node's observe PUBLISHER appends it to every wire frame
    /// off the fold (`vike_tradehub::publish`), and the desktop's observe bridge fills this field
    /// from the frame — so on the GUI it is the node's list, and in the node's own process it is
    /// empty. A reader inside the node asks the node (`vike_tradehub_core::Node::not_running`) instead.
    pub not_running: Vec<ArmingNotRunningView>,
}

impl CoreSnapshot {
    /// Placeholder published before the first real build (GUI renders "connecting").
    pub fn empty(venue: &str, symbol: &str) -> Self {
        CoreSnapshot {
            sequence: 0,
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            trading_state: TradingState::Active,
            balance: 0.0,
            balance_mode: BalanceMode::Delta,
            portfolio: Portfolio::default(),
            positions: Vec::new(),
            marks: Vec::new(),
            orders: Vec::new(),
            held_exits: Vec::new(),
            core_held_stops: Vec::new(),
            emulated_orders: Vec::new(),
            bars: indexmap::IndexMap::new(),
            mounts: std::sync::Arc::new([]),
            recent_events: Vec::new(),
            denied_orders: None,
            fault: None,
            journal_fence_halt: false,
            counters: CoreCounters::default(),
            reconcile: ReconcileBlock::default(),
            reconcile_coin_deltas: indexmap::IndexMap::new(),
            // ⚠ `0` = NO FOLD YET, and it must stay distinguishable from every real answer:
            // `CoreSnapshot::empty` is the window before a node's first fold, which
            // `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` measured as
            // a hole in its own gate ("the mounted set is empty, so the check answers 'no
            // evidence'"). A consumer must read this as "the node has not said yet", never as an
            // account set that happens to hash to zero — see
            // `vike_core::snapshot::accounts_epoch_of`, which never returns it.
            accounts_epoch: 0,
            not_running: Vec::new(),
        }
    }
}

/// Query surface — pure O(n) accessors over the published `Vec`s (no indexed Cache). This is the
/// READ half of the control boundary (`vike_core::control`); a CLI/MCP translates into these.
impl CoreSnapshot {
    /// What one unit of `(venue, symbol)`'s quantity is worth — the GUI-side twin of
    /// `crate::Account::contract_value_of`, which lives inside the core's per-venue engines and
    /// is otherwise unreachable from a lossy snapshot reader.
    ///
    /// Resolves for a symbol the account holds NO position in (that is the point — the deribit
    /// options confirm-ticket must price a contract it has never traded), because the whole
    /// per-venue grid is published, not just the held rows.
    ///
    /// **An UNKNOWN venue reads the default (`Linear`, 1.0)**, matching the unit-free arithmetic every
    /// caller used before the value existed. That is the permissive answer, so a caller gating on
    /// notional (e.g. the order-entry `max_notional_per_order` policy cap) must not treat it as proof
    /// the instrument is plain — it means "this snapshot knows of no value for that venue".
    pub fn contract_value_of(&self, venue: &str, symbol: &str) -> vike_model::ContractValue {
        self.portfolio
            .venue(venue)
            .map_or(vike_model::ContractValue::default(), |v| v.contract_value_of(symbol))
    }

    /// All orders for a symbol, registry insertion order (any status).
    pub fn orders_for<'a>(&'a self, symbol: &'a str) -> impl Iterator<Item = &'a OrderView> + 'a {
        self.orders.iter().map(|o| &**o).filter(move |o| o.symbol == symbol)
    }

    /// One order by order-client-id (any status).
    pub fn order(&self, order_client_id: &str) -> Option<&OrderView> {
        self.orders.iter().map(|o| &**o).find(|o| o.order_client_id == order_client_id)
    }

    /// One order by order-client-id, only while NON-TERMINAL.
    pub fn open_order(&self, order_client_id: &str) -> Option<&OrderView> {
        self.orders
            .iter()
            .map(|o| &**o)
            .find(|o| o.order_client_id == order_client_id && !o.status.is_terminal())
    }

    /// Net position for (venue, symbol) — the `BOTH` leg (hedge-mode legs via `positions`).
    pub fn position(&self, venue: &str, symbol: &str) -> Option<&PositionView> {
        self.positions.iter().find(|p| {
            p.venue == venue && p.symbol == symbol && p.position_side == PositionSide::Both
        })
    }

    /// Latest mark for (venue, symbol).
    pub fn last_mark(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.marks.iter().find(|(v, s, _)| v == venue && s == symbol).map(|(_, _, px)| *px)
    }

    /// The latest venue-REPORTED coin delta for `(venue, symbol)` from the reconcile path (Wave
    /// 5d) — `Some` only for a venue that surfaces one (Deribit `get_positions.delta`), `None`
    /// otherwise. The greeks tool passes this straight into
    /// `OptionChainPositionGreeksInput::coin_delta` so a Deribit perp/future hedge leg folds into
    /// net portfolio greeks; see `reconcile_coin_deltas`.
    pub fn coin_delta(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.reconcile_coin_deltas.get(&(venue.to_string(), symbol.to_string())).copied()
    }

    /// Mode-aware total account equity — the cross-venue `equity_total` (compensated_sum law). FOOTGUN:
    /// this is the cross-venue TOTAL, so in multi-venue mode it can differ from the raw `equity`
    /// field (primary venue only); equal single-venue. Adapters use this accessor.
    #[expect(clippy::misnamed_getters)] // deliberately the cross-venue TOTAL, not the `equity` field
    pub fn equity(&self) -> f64 {
        self.portfolio.equity_total
    }

    /// Which OPEN positions have NO priceable mark (core-ergonomics) — `(venue, symbol)` pairs,
    /// delegating to [`Portfolio::missing_price_instruments`] (see it for the exact definition).
    /// The named counterpart to `portfolio.missing_prices_total`: the count says how many, this
    /// says which. Read-only, off the fold path, like every accessor here.
    pub fn missing_price_instruments(&self) -> Vec<(String, String)> {
        self.portfolio.missing_price_instruments()
    }

    /// Primary-engine trading state (per-venue states via `venues`).
    pub fn trading_state(&self) -> vike_model::TradingState {
        self.trading_state
    }
}
