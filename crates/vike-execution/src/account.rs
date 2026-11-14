//! FillEvent-derived account read-model: positions + realized PnL from the fill stream.
//!
//! - per-venue: `apply_fill` refuses a fill whose `venue` is not `self.venue` ([`FillFold::ForeignVenue`]).
//! - per-symbol contract value (kind plus per-unit factor): `contract_value_of(symbol)` with the legacy scalar default as a linear unit.
//! - explicit `balance_mode` (Delta | Authoritative); `equity_all(seed)` is mode-aware;
//!   `apply_account_state` flips the mode after each authoritative balance assignment.
//! - `fold` is the SOLE writer of position + realized PnL (called by both `apply_fill` and
//!   `apply_liquidation` — one compute_fill block, no drift).
//! - `apply_fill` is IDEMPOTENT PER `trade_id` (see "Fill dedup" below).
//! - NO equity cache anywhere (catastrophic-cancellation risk) — every read is a sparse
//!   full recompute.
//! - Maps are `IndexMap` so f64 summation order in `equity_all` is FIXED. **NEVER a HashMap** —
//!   the insertion order IS the f64 fold order the parity gate pins, bit-for-bit, against the
//!   frozen `fixtures/r0/account_scenarios.json` bytes, which are the oracle themselves.
//!
//! ## Fill dedup: the check lives ON the aggregate that owns the money
//!
//! [`Account::apply_fill`] moves money (`balance -= commission`, `fees_paid +=`, the position
//! fold, `realized_pnl +=`), and several producers re-deliver an already-folded fill BY DESIGN:
//! the `exec_actor::run_loop` gap sentinel's resync, the `user_data::run_user_resync`
//! replay, hyperliquid's `userFills` `isSnapshot` frame (re-sent on EVERY reconnect), and the
//! venue twins listed on [`Account::duplicate_fills_refused`]. So [`Account`] owns the ledger,
//! [`Account::apply_fill`] consults it before anything moves and reports the outcome as a
//! [`FillFold`]: a caller that ignores the return still cannot double-count.
//! `Engine`'s `seen_trade_ids()`, `local_view()`, `EngineSnapshot` wire and
//! `seed_seen_trade_ids` all read/write THIS set — one authority.
//!
//! **The key choice: the bare `trade_id`, its narrowness DETECTED rather than assumed away.**
//! A fully qualified key would be `(AccountId, InstrumentId, TradeId)`. The account component is redundant here
//! (an `Account` IS per-venue-per-account), the instrument one is NOT: binance/aster `t` and okx
//! `tradeId` are per-SYMBOL venue sequences, so on a multi-symbol engine
//! (`Engine::extra_symbols`, which combo lowering populates) two executions can share an
//! id string; `vike_paper`'s `paper-{n}` and `vike_execution::testing::TestExecutionClient`'s
//! `simt{n}` are per-CLIENT counters, so `ExecutionClientPaperMulti`'s per-symbol books collide by
//! construction. The key stays bare because WIDENING it loses money: the reconcile lane compares
//! venue ids against this set as BARE STRINGS (`vike_execution::recon::diff`'s
//! `local.seen_trade_ids.contains(f.trade_id.as_str())`), and a `FillReport`'s symbol is the
//! venue's spelling, not necessarily the unified one, so with a composite key `diff` would call an
//! already-folded fill a `MissingFill` and `resolve` would fold it a SECOND time. Widening also
//! moves `EngineSnapshot::seen_trade_ids` off `Vec<String>`, changing the journal `state_hash`
//! fence, which is the deferred schema-versioning question (`docs/decisions/`).
//!
//! **The fingerprint collision detector.** Beside each id the ledger stores an FNV-1a64 over
//! `(symbol, side, last_qty, last_price)` (`fill_print`). Same id AND same fingerprint is a
//! re-delivery: refused, counted into [`Account::duplicate_fills_refused`], silent. Same id and a
//! DIFFERENT fingerprint is an id COLLISION, a genuine fill about to be dropped: counted into
//! [`Account::colliding_fills_refused`] and logged at ERROR naming both fills. It is still
//! refused: folding on a mismatch would make the guard depend on venue price/qty rounding being
//! stable, and a re-delivery whose px re-encoded in the last ulp would double count. The key errs
//! toward DROPPING a genuine fill; the fingerprint makes that drop visible so an operator can
//! reconcile. A restored entry ([`Account::seed_seen_fill_ids`]) carries [`PRINT_UNKNOWN`] and is
//! never reported as a collision: a prior session's fingerprints are not on the journal wire.
//!
//! An EMPTY `trade_id` is unrepresentable (`FillEvent::trade_id` is a `TradeId`, whose
//! constructor refuses the empty string), so no fill escapes the guard.
//!
//! **Why the set is UNBOUNDED in the process, and a WINDOW in the journal.** It grows tens of bytes
//! per distinct fill for the process lifetime. Every COUNT cap (Hummingbot's 2000, an LRU) re-admits
//! a duplicate the moment a replay window reaches back further than the cap, and the widest window
//! here is not a fill count: `config.reconcile_lookback_ms` is one hour by default and up to 49.7
//! days, and some venue history endpoints are row-count-bounded with no start time. So the process
//! keeps every id, and only the journal `Snapshot` (rewritten whole every 1024 records) keeps a TIME
//! window of them, with a floor that refuses what the window cannot judge: `fill_window`'s module
//! doc. See `crates/vike-execution/CLAUDE.md`.
//!
//! ## Key types: interned, not owned
//!
//! [`PositionKey`] and the mark-slot key are `(Ustr, Ustr, …)`: `FillEvent::venue`/`symbol` are
//! ALREADY [`Ustr`] and `position_side` is the [`PositionSide`] enum, so building a key in
//! `apply_fill`, `apply_liquidation`, the mark slot and every `mark_of`/`unrealized_pnl` read
//! allocates nothing. Interning soundness: venue is ~10 values ever and symbol is bounded by the
//! instruments a session touches (`vike_model::events`' module doc is the authority); the
//! `&str`-taking read accessors ([`Account::mark_of`], [`Account::unrealized_pnl`], …) intern
//! their arguments at the boundary.
//!
//! WIRE IS UNCHANGED: `Ustr` is serde-transparent and `PositionSide` serializes to
//! `"BOTH"/"LONG"/"SHORT"` (`rename_all = "UPPERCASE"`), so
//! [`crate::engine_snapshot::EngineSnapshotAccount`] and the `state_hash` fence over its canonical JSON
//! are byte-identical to `String` keys (pinned by
//! `engine_snapshot::tests::account_snapshot_key_wire_is_byte_identical`).
//!
//! The third key element is the closed-set [`PositionSide`]: every producer (the fill lane and
//! `ReconcileSnapshot::position_sides`) already carries the enum, so no string reaches a key.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use trade_id_ledger::TradeIdLedger;
use ustr::Ustr;
use vike_model::MarginMode;
use vike_model::events::{
    AccountState, FeeAdjustmentEvent, FillEvent, FundingEvent, PositionLiquidated, PositionSide,
};

pub mod fill_window;
pub(crate) mod fx;
mod marks;
pub(crate) mod trade_id_ledger;
mod valuation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BalanceMode {
    Delta,
    Authoritative,
}

/// A position ledger entry: `{"size", "average_price"}`, plus the margin-mode carrier.
///
/// `margin_mode`/`isolated_margin` are `#[serde(skip_serializing_if)]` on their default
/// (`Cross` / `None`), so a cross position serializes to EXACTLY `{"size":…,"average_price":…}` —
/// keeping every snapshot/journal, and the `EngineSnapshotAccount` `state_hash` fence, byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct PositionEntry {
    pub size: f64,
    pub average_price: f64,
    /// Per-position margin mode (`Cross` = whole-account collateral). Carried forward across
    /// fills by `Account::fold`.
    #[serde(default, skip_serializing_if = "MarginMode::is_cross")]
    pub margin_mode: MarginMode,
    /// Allocated isolated-margin wallet (account currency); `None` for cross, meaningful only
    /// under `MarginMode::Isolated`. The core's liq-pool inputs subtract it from the cross pool
    /// (`vike_core::snapshot::build`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolated_margin: Option<f64>,
}

/// Position key: (venue, symbol, position_side) — position_side `Both` for one-way/spot;
/// the tuple reserves the hedge-mode dimension for perps. All three components are `Copy`, so
/// building one allocates nothing (module doc, "Key types").
pub type PositionKey = (Ustr, Ustr, PositionSide);

/// Mark-slot key: (venue, symbol), interned/`Copy` like [`PositionKey`].
pub type MarkKey = (Ustr, Ustr);

/// WHICH PRICE CONCEPT a write into the account mark slot carries.
///
/// `Account.marks` is a SINGLE UNTAGGED SCALAR per symbol, read by the pre-trade risk gate
/// (`RiskContext.mark_price`, `margin_in_use_priced`, `equity_all`), the margin-call law
/// (`check_margin_call_priced`), the trailing-stop seed and `LiveBroker.price`. Feeds write it
/// at very different cadences, so without a rule a klines lane at one bar/second would stomp a 1s
/// venue mark.
///
/// **THE LAW** (enforced inside [`Account::set_mark_from`], nowhere else):
/// while a symbol's slot holds a GENUINE VENUE MARK ([`MarkSource::is_venue_mark`]) observed
/// within its source's OWNERSHIP WINDOW, that mark OWNS the slot and a [`MarkSource::BarClose`]
/// or [`MarkSource::TradeTick`] write for that symbol is DROPPED. Otherwise last-write-wins.
///
/// **PER-SOURCE WINDOWS**, keyed off the CURRENT OWNER's source:
/// - [`MarkSource::VenueMark`] — a streamed mark (~1s cadence) — uses `mark_staleness_ms`
///   (default 10s, ~10 missed ticks).
/// - [`MarkSource::ReconcileMark`] — sampled at the RECONCILE cadence (default 60s) — uses the
///   longer `reconcile_staleness_ms` (default 150s), so ownership stays CONTINUOUS across passes
///   instead of expiring and alternating with closes every minute.
///
/// Consequences:
/// - A venue with NO mark stream and NO reconcile mark never has an owned slot: plain
///   last-write-wins.
/// - A mark stream that DIES hands the slot back after its window and closes resume, so
///   valuation degrades to the previous rung instead of freezing at a stale mark.
/// - A venue mark never blocks ANOTHER venue mark, so a reconcile mark replaced by a streamed
///   mark immediately reverts to the short streamed window.
///
/// Nothing is lost to the resolver: the dropped concepts still fill their own `PriceBoard` slots
/// (`set_bar_close` / `set_last_trade`), where the per-source valuation chain lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkSource {
    /// The venue's OWN mark/index price off a dedicated stream (`@markPrice@1s`, bybit
    /// `tickers.markPrice`, okx `mark-price`, HL `activeAssetCtx.markPx`) or off a fill's
    /// `mark_price` field. The concept the perp liquidation/valuation law is defined in terms of.
    VenueMark,
    /// The venue's own mark for a position as REPORTED BY A RECONCILE SNAPSHOT
    /// (`ExecReport::position_mark_price`). Genuinely the venue's mark — just sampled at the
    /// reconcile cadence (default 60s) rather than streamed, so it goes stale between passes and
    /// hands the slot back. Ranked EQUAL to [`Self::VenueMark`]: it is the same number from the
    /// same venue, and for a venue with no mark stream it is the only real mark available.
    ReconcileMark,
    /// The close of a COMPLETED candle. Up to one full bar interval stale by construction, and
    /// on a perp it is the LAST TRADED price, not the index-derived mark the venue liquidates on.
    BarClose,
    /// A sub-bar print: a trade tick, or a quote-derived price on the tick lane. Fresher than a
    /// close, still not the mark.
    TradeTick,
}

impl MarkSource {
    /// Whether this source is a genuine venue mark — the two variants that may OWN the slot.
    pub fn is_venue_mark(self) -> bool {
        matches!(self, MarkSource::VenueMark | MarkSource::ReconcileMark)
    }
}

/// Default ownership window for a STREAMED venue mark: about ten missed ticks of a 1s venue mark
/// stream. Mirrored by `vike_core::CoreConfig::mark_staleness_ms`, which overwrites it on the
/// live path.
pub const DEFAULT_MARK_STALENESS_MS: i64 = 10_000;

/// Default ownership window for a RECONCILE mark ([`MarkSource::ReconcileMark`]): 2.5× the
/// default reconcile interval (`config.reconcile_interval_ms`, 60s), so the slot stays owned
/// CONTINUOUSLY between passes.
///
/// It matters only where a reconcile mark is WRITTEN: `Engine::apply_snapshot`, which no
/// runtime path calls (tests and the frozen `fixtures/r5/` replay only; the live reconcile driver
/// sends `Command::ReconcileReports`, which files none). It is NOT a gate on any reconcile pass: a longer
/// interval skips and delays nothing. Mirrored by
/// `vike_core::CoreConfig::reconcile_mark_staleness_ms`, which overwrites it on the live path.
pub const DEFAULT_RECONCILE_STALENESS_MS: i64 = 150_000;

/// How long a quote-currency conversion rate (`Account::quote_rate_of`) stays usable, in the engine's
/// clock ms: past it the symbols quoted in that currency refuse OPENING orders (`quote-rate-stale`)
/// while a covered reduce still passes and the valuation keeps the last rate. The node refreshes a
/// rate by re-admitting the symbol, about once a minute while a position is held, so two minutes is
/// two missed refreshes. A constant, not a knob: the verdict is part of what a replay reproduces, so
/// making it configurable means putting it where the gate's other limits are journaled.
pub const DEFAULT_QUOTE_RATE_TTL_MS: i64 = 120_000;

/// How far AHEAD of the engine's clock a rate's `rate_as_of_ms` may be and still be believed. The
/// node stamps the rate with the same process clock the engine's `now_ms` comes from, so the
/// honest difference is zero; five seconds absorbs a stamp taken on one thread and folded on
/// another. A stamp beyond it (a µs or ns epoch, `i64::MAX`, a clock that stepped) is refused
/// (`quote-rate-future`) because it would otherwise PIN the rate: every real refresh is older than
/// it, so the newer-wins rule would keep the pinned rate for ever and the age test (`now - as_of`)
/// would never call it stale.
pub const QUOTE_RATE_FUTURE_SKEW_MS: i64 = 5_000;

/// How much LATER than a refused jump the offer that confirms it must be stamped. A jump is
/// accepted only when it is offered a second time (`MAX_QUOTE_RATE_STEP`); two offers with one stamp
/// (a re-delivered command, or two symbols of one currency in one refresh batch) are ONE
/// observation, not two, and never confirm each other.
pub const MIN_QUOTE_RATE_CONFIRM_GAP_MS: i64 = 1_000;

/// What `Account::restore_fx_symbol` did with one symbol of a restored admission list.
pub(crate) enum FxRestore {
    /// The snapshot does not list the symbol as foreign-quoted: restore it as an ordinary one.
    NotForeign,
    /// The symbol is back in the fx book with its currency's rate and its effective value.
    Restored,
    /// The snapshot lists the symbol as foreign-quoted but holds no rate for its currency: it is left
    /// out, so it cannot be folded at its own unit.
    Unusable,
}

/// The largest factor (either way) a currency's rate may move in ONE accepted update. A rate beyond
/// it is refused as `quote-rate-jump` and remembered with its stamp; the SAME rate offered again,
/// stamped at least [`MIN_QUOTE_RATE_CONFIRM_GAP_MS`] later and no older than
/// [`DEFAULT_QUOTE_RATE_TTL_MS`], is accepted as a real move (two observations, not one delivered
/// twice). This is the engine's only defence against a rate in the wrong direction after a good one
/// (USDJPY as `150` after `0.00667` is a 22,500x step); the first rate of a currency has nothing to
/// be compared with, so orientation is the node's rate builder's to get right.
pub const MAX_QUOTE_RATE_STEP: f64 = 1.5;

/// How many reconcile passes may take the venue's figure after a fill or a liquidation this account
/// refused ([`Account::note_unfolded_cash`]). Two, not one: a pass's balance read can leave just
/// before the fill lands at the venue and be folded just after the refusal, so the first pass may
/// show nothing yet and the second is the one that heals.
pub const UNFOLDED_CASH_PASSES: u8 = 2;

/// The fingerprint stored for a ledger entry whose identity is UNKNOWN — a `trade_id` seeded from a
/// durable source ([`Account::seed_seen_fill_ids`]) rather than observed by this session's fold.
/// Compares equal to nothing, so such an entry is refused as a plain duplicate, never reported as
/// a collision (prior-session fingerprints are not on the journal wire).
pub const PRINT_UNKNOWN: u64 = 0;

/// FNV-1a64 over the four fields that IDENTIFY a fill within one account: `(symbol, side,
/// last_qty, last_price)`. Two deliveries of ONE venue execution agree on all four; two DIFFERENT
/// executions sharing a `trade_id` string almost never do. Deliberately excludes
/// `commission`/`ts`/`order_client_id`/`mark_price`: a re-delivery can legitimately restate a fee
/// or a timestamp (binance perp's early TRADE_LITE fill carries no commission, its authoritative
/// twin does), and that must not read as a collision.
///
/// f64s are hashed by `to_bits()` — an identity test, not arithmetic (a non-finite fill is refused
/// by the engine's `numbers_finite` guard before it reaches here). Allocation-free. Never returns
/// [`PRINT_UNKNOWN`] (a computed `0` maps to `1`).
#[inline]
fn fill_print(fill: &FillEvent) -> u64 {
    let mut digest = vike_utility::fnv::Fnv1a64::new();
    digest.write(fill.symbol.as_str().as_bytes());
    digest.write(&(fill.side as i64).to_le_bytes());
    digest.write(&fill.last_qty.to_bits().to_le_bytes());
    digest.write(&fill.last_price.to_bits().to_le_bytes());
    let h = digest.finish();
    if h == PRINT_UNKNOWN { 1 } else { h }
}

/// Whether `asset` is one of the three DOLLAR labels a venue states its money in (`USD`, `USDC`,
/// `USDT`): the one place this file decides "a dollar", for the inverse-fee conversions
/// ([`Account::commission_in_quote`], [`Account::commission_in_settle`]) and the lone-balance adoption
/// ([`Account::apply_account_state`]).
/// Case-sensitive, like the match it replaced: every venue writes these upper-case.
#[inline]
fn is_dollar_asset(asset: &str) -> bool {
    matches!(asset, "USD" | "USDC" | "USDT")
}

/// What [`Account::apply_fill`] DID with the fill it was handed — the money-side answer, reported
/// so a caller can react (deliver `on_fill`, publish, count) without needing its own dedup set.
///
/// Deliberately NOT `#[must_use]`: ignoring it is safe by construction (a [`Self::Duplicate`] moved
/// nothing); a caller CAN observe the refusal, it need not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillFold {
    /// The fill was folded: position, realized PnL, cash and fees all moved.
    Applied,
    /// REFUSED — this `trade_id` had already been folded into this account, and the fill's
    /// `fill_print` MATCHES the one folded under it: an ordinary re-delivery. Nothing moved, and
    /// [`Account::duplicate_fills_refused`] advanced.
    Duplicate,
    /// REFUSED — this `trade_id` had already been folded, but under a DIFFERENT
    /// `(symbol, side, qty, px)`. That is an id COLLISION, not a re-delivery: a genuine fill has
    /// been dropped. Nothing moved, [`Account::colliding_fills_refused`] advanced, and the fold
    /// logged it at ERROR. The caller cannot recover it — reconcile can.
    Collision,
    /// REFUSED — the fill names another venue than this account's: it was routed to the wrong
    /// engine. Nothing moved (the `trade_id` is not burned either), and the refusal logged at ERROR.
    /// The core's router answers an engine of the event's own venue, so this is the backstop for a
    /// router fault, never an expected outcome.
    ForeignVenue,
    /// REFUSED — the ledger holds a FLOOR (a bounded `Snapshot` left ids out of it, `fill_window`'s module
    /// doc), the fill states NO stamp (`ts <= 0`) and its `trade_id` is not in the ledger: nothing can
    /// prove it new, so it may be a re-delivery of a fill the window dropped. Nothing moved and the id is
    /// not burned; the engine parks it for the operator (`Engine::take_unproven_fills`), never
    /// books it.
    Unproven,
}

impl FillFold {
    /// Whether the fill moved money, without enumerating the refusal reasons.
    pub fn applied(self) -> bool {
        matches!(self, FillFold::Applied)
    }
}

/// What [`Account::apply_fee_adjustment`] DID with the correction it was handed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FeeFold {
    /// The correction moved the ledger: `charge` is the CHANGE booked into `balance` and `fees_paid`,
    /// in the account's currency (signed; negative when the fee turned out smaller than booked).
    Applied { charge: f64 },
    /// The total this event states is the one already booked under its `trade_id` (a re-delivery, or
    /// IB repeating a commission it had already sent): nothing moved.
    Unchanged,
    /// REFUSED: no fill under this `trade_id` was folded into this account (it was dropped, belongs to
    /// another account, or the journal that held it was set aside), so a fee for it is a fee for
    /// nothing this ledger holds. Nothing moved; the venue's own record is what reconcile compares.
    UnknownFill,
    /// REFUSED: the adjustment names another venue than this account's (a routing fault). Nothing moved.
    ForeignVenue,
}

/// What the corrections of ONE execution's fee have booked, remembered under its `trade_id`
/// ([`Account::apply_fee_adjustment`]): the TOTAL extra fee they stated, and what that cost the
/// ledger in the account's currency when it was last booked, so the next correction books only the
/// change and a rate that moved in between does not leave a residue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeBooked {
    /// The `extra_commission` of the last correction applied, in [`Self::asset`].
    pub extra: f64,
    /// The fee currency of [`Self::extra`] (empty = not surfaced).
    pub asset: Ustr,
    /// What [`Self::extra`] has cost `balance` and `fees_paid` in all, in the account's currency.
    pub charge: f64,
}

/// Folds a stream of `FillEvent`s into positions and realized PnL.
#[derive(Debug, Clone)]
pub struct Account {
    pub venue: String,
    /// per-symbol contract values (notional kind plus per-unit factor, decision 0170) of the symbols
    /// whose value is not the default; `contract_value_of` falls back to the legacy scalar as a
    /// `Linear` unit.
    /// Written only by `new`/`restore`, [`Account::admit_contract_value`] and the foreign-quote
    /// installers of `account/fx.rs` (all cold, copy-on-write admissions), behind an `Arc` so
    /// read-models share the grid by pointer-copy ([`Account::contract_value_grid`],
    /// [`crate::VenueBlock::contract_values`]).
    ///
    /// ⚠ **For a symbol quoted in a currency other than the account's this row is the EFFECTIVE value
    /// — the venue's unit times the rate to the account's currency — not the venue's own unit.**
    /// That is what lets every money product on the order path (the fold, the gate, the margin call,
    /// the published block) stay one lookup and stay right with no edit. The venue's own (BASE) unit
    /// lives in the cold fx book, and it is the BASE that [`Account::snapshot`] writes, so nothing
    /// persists the product of a rate that has since moved.
    values: std::sync::Arc<IndexMap<String, vike_model::ContractValue>>,
    /// The per-unit factor of the `Linear` value an unlisted symbol reads (the constructor's scalar).
    default_unit: f64,
    /// Per-symbol RULING margin mode for a position this account OPENS FROM FLAT — the venue's
    /// per-ASSET truth, resolved once at the mount and handed in by the composition root.
    ///
    /// A grid, not a scalar, because the mode is a property of the ASSET:
    /// `VenueCapabilities::default_margin_mode` says `Cross` for hyperliquid, but on an `onlyIsolated`
    /// asset the mode that RULES is `Isolated` (#1087;
    /// `vike_hyperliquid::instrument::symbology::InstrumentRef::effective_margin_mode`).
    ///
    /// SPARSE ON PURPOSE — absent means `Cross`, like [`Self::mult`] for 1.0: the map is EMPTY for
    /// every venue but hyperliquid, and [`Self::default_margin_mode_of`] short-circuits on empty.
    /// It is consulted ONLY when [`Self::fold`] finds no prior entry (open-from-flat).
    ///
    /// IMMUTABLE after construction (written only by [`Account::with_default_margin_modes`]),
    /// behind an `Arc` like [`Self::mult`].
    default_margin_modes: std::sync::Arc<IndexMap<String, MarginMode>>,
    /// The same question for the symbols `Command::AdmitSymbol` ADMITTED (decision 0218): the margin
    /// mode a position opened from flat on an admitted symbol takes, stated by the admission's own
    /// `margin_mode` (an isolated-only perpetual is `Isolated`, a spot pair `Cash`). SPARSE like
    /// [`Self::default_margin_modes`] (absent means `Cross`), consulted FIRST by
    /// [`Self::default_margin_mode_of`], and written only by [`Account::admit_margin_mode`]
    /// (copy-on-write through the `Arc`, cold).
    ///
    /// Unlike the mount's grid it IS snapshotted ([`crate::engine_snapshot::EngineSnapshotAccount::admitted_margin_modes`]):
    /// an admission is runtime state the journal restores with the symbol itself
    /// (`restore_admitted_symbols`), so a restart that kept the symbol and lost its mode would open the next
    /// position on it `Cross` until the node's re-admission landed.
    admitted_margin_modes: std::sync::Arc<IndexMap<String, MarginMode>>,
    pub balance_mode: BalanceMode,
    pub positions: IndexMap<PositionKey, PositionEntry>,
    pub realized_pnl: f64,
    /// gross price PnL per closing portion, in order — a list of PnL floats, NOT `Trade`
    /// round-trip objects (the name avoids that collision; FIX/accounting call these closed PnLs).
    pub closed_pnls: Vec<f64>,
    pub balance: f64,
    /// ADDITIVE per-asset ledger (parity law A1): the raw `(asset, qty)` pairs venue AccountState
    /// events carry, upserted in venue-given order — what the `balance` scalar collapses. Never
    /// read by any fold formula.
    pub balances_by_asset: IndexMap<String, f64>,
    /// `realized_pnl` at the instant `balance` was last set from, or rolled to, a venue figure
    /// (AUTHORITATIVE sync); `None` = never synced. Realized PnL does not enter `balance` at the fill
    /// in Authoritative mode, so the cash reconcile compares venue cash against
    /// `balance + (realized_pnl - realized_pnl_at_balance_sync)` and a realized trade never
    /// false-flags, and every equity reader adds the same term
    /// ([`Self::realized_since_balance_sync`]). NOT in `EngineSnapshotAccount` (like `mark_meta`): a restore
    /// starts `None`, so the first post-restore pass re-adopts venue cash instead of diffing a stale
    /// baseline. Never read by the fold.
    pub realized_pnl_at_balance_sync: Option<f64>,
    /// How many more reconcile passes may take the venue's figure because of a fill or a liquidation
    /// this account refused to fold ([`Self::note_unfolded_cash`]). Zero on every account until one
    /// happens. Derived from journaled events, so a replay rebuilds it; NOT in `EngineSnapshotAccount`: a
    /// restored account's first pass adopts the venue's figure anyway.
    unfolded_cash_passes: u8,
    /// PRIVATE ON PURPOSE — the only way in is [`Account::set_mark_from`], which carries the
    /// precedence law (see [`MarkSource`]); a `pub` field is how the law got bypassed. Read it
    /// back with [`Account::mark_of`] / [`Account::marks_iter`].
    marks: IndexMap<MarkKey, f64>,
    /// Per-symbol provenance of the CURRENT [`Self::marks`] entry: which concept wrote it and
    /// the CORE-clock ms at which it was observed. Not in `EngineSnapshotAccount` (derived from
    /// non-journaled market data, like `marks`): a restore starts with an unowned slot and the
    /// next writer of any source wins — the safe degradation of a venue with no mark stream.
    mark_meta: IndexMap<MarkKey, (MarkSource, i64)>,
    /// How long a STREAMED venue mark ([`MarkSource::VenueMark`]) keeps ownership of a symbol's
    /// mark slot, in CORE-clock ms. Set by the runtime from `CoreConfig::mark_staleness_ms`; the
    /// default matches it so a standalone `Account` (backtest, tests, bridges) behaves the same.
    mark_staleness_ms: i64,
    /// The ownership window for a RECONCILE mark ([`MarkSource::ReconcileMark`]), longer than
    /// `mark_staleness_ms` (see [`MarkSource`]'s "PER-SOURCE WINDOWS"). Set by the runtime from
    /// `CoreConfig::reconcile_mark_staleness_ms`.
    reconcile_staleness_ms: i64,
    pub funding_paid: f64,
    /// cumulative signed commission (>0 net cost, <0 net rebate)
    pub fees_paid: f64,
    /// ADDITIVE per-asset fee attribution (parity law A1): the per-asset twin of `fees_paid`,
    /// keyed by the fill's fee currency, SAME sign (>0 paid, <0 rebate). SEPARATE from
    /// `balances_by_asset` (the holdings mirror only venue snapshots write), so a fee never
    /// masquerades as a negative holding. Populated only when the venue surfaced
    /// `commission_asset`; never read by any fold formula.
    pub fees_by_asset: IndexMap<Ustr, f64>,
    /// THE fee-adjustment ledger: for every execution whose fee was corrected AFTER its fill
    /// (`Event::FeeAdjustment`), what the corrections have booked so far ([`FeeBooked`]). PRIVATE ON
    /// PURPOSE, like [`Self::seen_fill_ids`]: the only way in is [`Account::apply_fee_adjustment`].
    /// A `BTreeMap`: it feeds no f64 fold, and its snapshot (`EngineSnapshotAccount::fee_adjusted`) is the
    /// canonical, sorted list. Sparse: empty on every account until a fee is corrected, and then
    /// absent from the snapshot, so no existing journal or `state_hash` byte moves. UNBOUNDED on
    /// purpose, for the reason the fill ledger is, and tiny: one row per CORRECTED execution.
    fee_adjusted: std::collections::BTreeMap<String, FeeBooked>,
    /// THE fill-dedup ledger — every `trade_id` this account has already folded. PRIVATE ON PURPOSE
    /// (the `marks` doctrine): the only way a fill reaches the money is [`Account::apply_fill`],
    /// which consults this first. Read it back with [`Account::seen_fill_ids`]; pre-load it with
    /// [`Account::seed_seen_fill_ids`].
    ///
    /// A hash map, unlike the rest of this type: it feeds NO f64 fold, and the one place its order
    /// would escape (`EngineSnapshot::seen_trade_ids`) sorts it. `trade_id -> (`[`fill_print`] of
    /// the fill folded under it ([`PRINT_UNKNOWN`] for a seeded entry), the engine day it was folded on
    /// (`fill_window::day_of`, what a bounded snapshot windows the ledger by))`. A [`TradeIdLedger`],
    /// not a std map, so the insert that grows it never rehashes every id on the core thread
    /// (`trade_id_ledger`'s module doc).
    seen_fill_ids: TradeIdLedger<(u64, u32)>,
    /// The FLOOR of a ledger restored from a bounded snapshot (`fill_window`'s module doc): a fill stamped
    /// before it is refused, because the snapshot left out the ids of that time and its re-delivery cannot be
    /// told from a new fill. `None` for every ledger that left nothing out (and every one in a process that
    /// never restored one). Set only by [`Self::seed_fill_ledger`].
    fill_floor_ms: Option<i64>,
    /// Count of fills REFUSED by [`Account::apply_fill`] as already-folded duplicates.
    ///
    /// ⚠ **A large value here is NORMAL on several venues, not a fault signal.** Duplicate delivery
    /// is designed-in: bybit emits every execution twice (`execution.fast` then the authoritative
    /// `execution`), binance perp emits an early fill and its authoritative twin sharing one `t`,
    /// hyperliquid re-sends its whole `userFills` snapshot on every reconnect, alpaca re-delivers
    /// across its `since_id` boundary. What means something is a CHANGE in its rate. No
    /// per-duplicate log: that would be a per-message log on the hot fold.
    pub duplicate_fills_refused: u64,
    /// Count of fills refused under an ALREADY-FOLDED `trade_id` whose `fill_print` DISAGREED —
    /// `trade_id` collisions, each a genuine fill this account dropped.
    ///
    /// ⚠ **Unlike [`Self::duplicate_fills_refused`], any nonzero value here is a fault**, one that
    /// UNDERSTATES the position: the dedup key is too narrow for the producer (binance/aster `t`
    /// and okx `tradeId`, per-SYMBOL sequences, on a multi-symbol engine; `vike_paper`'s per-client
    /// `paper-{n}` under `ExecutionClientPaperMulti`). The cure is at the PRODUCER (qualify the id)
    /// or reconcile; this counter makes the loss visible. Each occurrence is logged at ERROR — a
    /// fault transition, not per-message noise.
    pub colliding_fills_refused: u64,
    /// The multi-currency half of the ledger (`account/fx.rs`'s module doc): `None` — one pointer,
    /// no allocation — for every account without a symbol quoted in a currency other than its own,
    /// which is every account until a foreign-quoted symbol is admitted. The fold never reads it;
    /// the per-order, per-publish and snapshot paths test it for `None` before anything else.
    fx: Option<Box<fx::AccountLedgerFx>>,
    /// **A COIN LEDGER** (decision 0258): the `(venue, symbol)` of the engine's MOUNTED inverse contract, whose
    /// price values the coin. `Some` makes `balance`, `realized_pnl`, `closed_pnls`, `fees_paid` and
    /// `funding_paid` amounts of the contract's settle coin, and every equity reader multiplies them by that
    /// price ([`Self::coin_equity`]). `None` for every other account (written only by
    /// [`Self::set_coin_books`] and `restore`), so a linear ledger pays one `Option` test where it reads it.
    coin: Option<MarkKey>,
    /// A COIN LEDGER's last journaled fill price of its contract (0.0 = none yet): the price
    /// [`Self::coin_tolerance`] converts the reconcile floor at, because the reconcile pass is a journaled
    /// command and may not read a mark. Folded from fills, carried in the snapshot's coin books.
    coin_price: f64,
}

impl Account {
    pub fn new(
        multiplier: f64,
        venue: &str,
        values: Option<IndexMap<String, vike_model::ContractValue>>,
        balance_mode: BalanceMode,
    ) -> Self {
        Account {
            venue: venue.to_string(),
            values: std::sync::Arc::new(values.unwrap_or_default()),
            default_unit: multiplier,
            default_margin_modes: std::sync::Arc::new(IndexMap::new()),
            admitted_margin_modes: std::sync::Arc::new(IndexMap::new()),
            balance_mode,
            positions: IndexMap::new(),
            realized_pnl: 0.0,
            closed_pnls: Vec::new(),
            balance: 0.0,
            balances_by_asset: IndexMap::new(),
            realized_pnl_at_balance_sync: None,
            unfolded_cash_passes: 0,
            marks: IndexMap::new(),
            mark_meta: IndexMap::new(),
            mark_staleness_ms: DEFAULT_MARK_STALENESS_MS,
            reconcile_staleness_ms: DEFAULT_RECONCILE_STALENESS_MS,
            funding_paid: 0.0,
            fees_paid: 0.0,
            fees_by_asset: IndexMap::new(),
            fee_adjusted: std::collections::BTreeMap::new(),
            seen_fill_ids: TradeIdLedger::default(),
            fill_floor_ms: None,
            duplicate_fills_refused: 0,
            colliding_fills_refused: 0,
            fx: None,
            coin: None,
            coin_price: 0.0,
        }
    }

    /// **Make this account a COIN LEDGER when `symbol`, the engine's mounted one, is an inverse contract**
    /// (decision 0258): its money is then kept in the contract's settle coin and valued at `symbol`'s price.
    /// Called by `Engine::new` (so `from_snapshot` too); a no-op for a linear mount, for an account
    /// that holds a foreign-quoted symbol (that ledger is the account's currency, decision 0183) and for one
    /// that already is a coin ledger (restored from a snapshot that carried its coin books).
    ///
    /// An account that BECOMES one starts its money at zero: a fresh account holds none, and one restored from a
    /// snapshot written before coin books holds dollars, which are not read (no migration). Positions stay.
    pub fn set_coin_books(&mut self, symbol: &str) {
        if self.coin.is_some()
            || self.fx.is_some()
            || self.contract_value_of(symbol).kind != vike_model::NotionalKind::Inverse
        {
            return;
        }
        self.balance = 0.0;
        self.realized_pnl = 0.0;
        self.closed_pnls.clear();
        self.fees_paid = 0.0;
        self.funding_paid = 0.0;
        self.realized_pnl_at_balance_sync = None;
        self.coin = Some((Ustr::from(&self.venue), Ustr::from(symbol)));
    }

    /// The `(venue, symbol)` whose price values this COIN LEDGER's money ([`Self::set_coin_books`]), or `None`.
    #[inline]
    pub fn coin_key(&self) -> Option<MarkKey> {
        self.coin
    }

    /// **A coin ledger's equity in the account's currency** (decision 0258): the coin cash times `rate` (the
    /// account-currency value of ONE settle coin) plus the unrealized PnL, which is already in that currency.
    /// `Delta`: `seed + (balance + realized)·rate + unrealized`; `Authoritative`: `(balance + realized since
    /// the balance sync)·rate + unrealized`. A `rate` of 0.0 (an unpriced coin) values the coin at nothing.
    #[inline]
    pub fn coin_equity(&self, seed: f64, unrealized: f64, rate: f64) -> f64 {
        if self.balance_mode == BalanceMode::Authoritative {
            (self.balance + self.realized_since_balance_sync()) * rate + unrealized
        } else {
            seed + (self.balance + self.realized_pnl) * rate + unrealized
        }
    }

    /// **The reconcile balance tolerance in this ledger's unit** (decision 0258). A dollar ledger's is `tolerance` as it
    /// is. A COIN LEDGER's absolute floor is the dollar floor converted at its last journaled fill price, so it is no
    /// looser than the dollar floor at that price (1 USD at 60,000 is 0.0000167 BTC, where the bare figure read 1
    /// BTC); with no fill yet it is zero and the relative band (1 bp of the wallet) alone decides. A pure function
    /// of folded state: the pass is journaled, so a refold judges it alike.
    pub fn coin_tolerance(
        &self,
        tolerance: crate::recon::ReconcileBalanceTolerance,
    ) -> crate::recon::ReconcileBalanceTolerance {
        if self.coin.is_none() {
            return tolerance;
        }
        let abs_floor =
            if self.coin_price > 0.0 { tolerance.abs_floor / self.coin_price } else { 0.0 };
        crate::recon::ReconcileBalanceTolerance { abs_floor, ..tolerance }
    }

    /// Pre-load the fill-dedup ledger from a durable source — a restored `EngineSnapshot`, or the
    /// replayed command journal. The cold-start half of the guard: without a seed a restart
    /// re-folds whatever venue history the first resync / reconnect snapshot hands it.
    ///
    /// ⚠ Seeding is the CALLER's job and no shipped binary does it yet — see the "cold start" note
    /// on `crates/vike-execution/CLAUDE.md`. Seeded entries carry [`PRINT_UNKNOWN`], so a refusal
    /// against one is a plain duplicate, never a collision.
    pub fn seed_seen_fill_ids<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.seen_fill_ids.extend(ids.into_iter().map(|id| (id, (PRINT_UNKNOWN, 0))));
    }

    /// The restore's seed: `ids` with the day each was folded on (`fill_window::restored`), and the `floor` the
    /// snapshot stated, kept when it is later than the one this ledger holds (a floor only moves forward).
    pub(crate) fn seed_fill_ledger(
        &mut self,
        ids: impl IntoIterator<Item = (String, u32)>,
        floor: Option<i64>,
    ) {
        self.seen_fill_ids.extend(ids.into_iter().map(|(id, day)| (id, (PRINT_UNKNOWN, day))));
        self.fill_floor_ms = self.fill_floor_ms.max(floor);
    }

    /// The ledger as a snapshot writes it (`fill_window::windowed`) and the floor it states: the floor this
    /// ledger holds, or, once a bounded snapshot leaves an id out, the window's own (the later of the two).
    pub(crate) fn fill_ledger(&self, bounded: bool) -> (fill_window::FillWindowIds, Option<i64>) {
        let w = fill_window::windowed(
            self.seen_fill_ids.iter().map(|(id, &(_, day))| (id, day)),
            bounded,
        );
        let floor = match w.newest {
            Some(n) if bounded && (w.left_out || self.fill_floor_ms.is_some()) => {
                self.fill_floor_ms.max(Some(fill_window::floor_ms(n)))
            }
            _ => self.fill_floor_ms,
        };
        (w, floor)
    }

    /// The floor a restored ledger refuses fills before ([`Self::seed_fill_ledger`]), `None` when it has none.
    pub fn fill_floor_ms(&self) -> Option<i64> {
        self.fill_floor_ms
    }

    /// Whether a fill stamped `ts` is PROVEN older than this ledger remembers: it holds a floor, the fill states a
    /// stamp and the stamp is before the floor. [`Self::apply_fill`] refuses such a fill unless its id is in the
    /// ledger, and the reconcile pass does not read such a row.
    #[inline]
    pub fn before_fill_floor(&self, ts: i64) -> bool {
        self.fill_floor_ms.is_some_and(|floor| ts > 0 && ts < floor)
    }

    /// Whether a fill stamped `ts` CANNOT be proven new: this ledger holds a floor and the fill states no stamp
    /// (`ts <= 0`, a venue frame that omitted its time field). [`Self::apply_fill`] refuses such a fill unless its
    /// id is in the ledger ([`FillFold::Unproven`]).
    #[inline]
    pub fn unprovable_fill(&self, ts: i64) -> bool {
        self.fill_floor_ms.is_some() && ts <= 0
    }

    /// Take `floor` as this ledger's floor when it is later than the one it holds (a floor only moves forward):
    /// the core calls it with the floor each `Snapshot` it writes states, so the process judges a fill as a restart
    /// from that `Snapshot` would (`vike_core`'s `write_snap`).
    pub fn adopt_fill_floor(&mut self, floor: Option<i64>) {
        self.fill_floor_ms = self.fill_floor_ms.max(floor);
    }

    /// Every `trade_id` in the fill-dedup ledger, read-only (snapshots, the reconcile `ReconcileLocalView`,
    /// tests). An iterator, so the fingerprint stays private to this type.
    pub fn seen_fill_ids(&self) -> impl Iterator<Item = &str> {
        self.seen_fill_ids.keys()
    }

    /// Test seam: how many ids the largest ONE hash table of the fill ledger has room for, which bounds
    /// how many ids one insert can rehash.
    #[cfg(test)]
    pub(crate) fn fill_ledger_largest_table(&self) -> usize {
        self.seen_fill_ids.largest_table()
    }

    /// Whether `trade_id` has already been folded into this account. The membership question on its
    /// own, for a caller that has an id and no fill (the reconcile `diff` shape).
    pub fn has_folded_fill(&self, trade_id: &str) -> bool {
        self.seen_fill_ids.contains_key(trade_id)
    }

    /// Whether THIS execution has been folded: the ledger holds its `trade_id` with its own
    /// fingerprint (`fill_print`: symbol, side, qty, price), so a colliding id that folded another
    /// symbol's or another order's fill answers `false`, and so does a seeded entry
    /// ([`PRINT_UNKNOWN`]) and a re-delivery whose qty or price re-encoded. The reconcile pass's
    /// precise clear of a held `MissingFill` asks it (`vike_core`'s `clear_folded_missing_fills`).
    pub fn has_folded_this_fill(&self, fill: &FillEvent) -> bool {
        self.seen_fill_ids.get(fill.trade_id.as_str()).map(|&(print, _)| print)
            == Some(fill_print(fill))
    }

    /// What one unit of `symbol`'s quantity is worth (decision 0170): the listed value, else the
    /// `Linear` value of the legacy scalar default for an unlisted symbol.
    #[inline]
    pub fn contract_value_of(&self, symbol: &str) -> vike_model::ContractValue {
        self.values
            .get(symbol)
            .copied()
            .unwrap_or(vike_model::ContractValue::linear(self.default_unit))
    }

    /// The whole per-symbol contract-value grid, shared by pointer-copy (`Arc::clone`). Exposed so a
    /// read-model can answer `contract_value_of` for a symbol the account holds NO position in (the
    /// deribit options confirm ticket prices a never-traded symbol's notional). A handle is
    /// never written through ([`Account::admit_contract_value`] is copy-on-write), so it never
    /// observes a partial update.
    pub fn contract_value_grid(
        &self,
    ) -> std::sync::Arc<IndexMap<String, vike_model::ContractValue>> {
        std::sync::Arc::clone(&self.values)
    }

    /// The fallback value `contract_value_of` returns for any symbol absent from the grid.
    pub fn contract_value_default(&self) -> vike_model::ContractValue {
        vike_model::ContractValue::linear(self.default_unit)
    }

    /// Install `symbol`'s contract value after construction — the one writer besides `new` and
    /// `restore`, called by `Engine::admit_symbol` (decision 0173). The grid stays SPARSE:
    /// a value equal to what an unlisted symbol reads is not stored (and removes a row an earlier
    /// admission stored). A degenerate factor folds to 1.0 first, like every value that enters the
    /// money path from outside (`ContractValue::folded`). Copy-on-write through the `Arc`, so a
    /// read-model handle taken earlier keeps the grid it was given and never sees a partial update.
    pub fn admit_contract_value(&mut self, symbol: &str, value: vike_model::ContractValue) {
        // A foreign-quoted symbol's row is its EFFECTIVE value and is written only by the fx
        // installers; writing a base here would silently value it at par.
        debug_assert!(!self.is_foreign_quoted(symbol), "{symbol} is quoted in a foreign currency");
        let value = value.folded();
        if value == self.contract_value_default() {
            if self.values.contains_key(symbol) {
                std::sync::Arc::make_mut(&mut self.values).shift_remove(symbol);
            }
        } else {
            std::sync::Arc::make_mut(&mut self.values).insert(symbol.to_string(), value);
        }
    }

    /// Forget everything this account holds for `symbol` as a symbol the engine ADMITTED (the writer is
    /// `Engine::unadmit_symbol`, decision 0205): its contract value, its place in the foreign-quoted
    /// book (`release_quoted`) and its mark slot for `venue`. The position entries (a flat one has size
    /// 0) and the money totals stay: they are the ledger, and the engine refuses to give a symbol back while
    /// it holds a position. The marks go because they are market data an old stamp must not hand back as the
    /// FIRST mark of a later admission of the same symbol. A symbol the account holds nothing for is a no-op.
    pub fn forget_symbol(&mut self, venue: &str, symbol: &str) {
        self.release_quoted(symbol);
        if self.values.contains_key(symbol) {
            std::sync::Arc::make_mut(&mut self.values).shift_remove(symbol);
        }
        let key: MarkKey = (Ustr::from(venue), Ustr::from(symbol));
        self.marks.shift_remove(&key);
        self.mark_meta.shift_remove(&key);
    }

    /// Whether a fill on `symbol` has a contract kind this account does NOT know: the engine trades an
    /// INVERSE contract and `symbol` is not in its grid. An inverse engine's grid lists the one
    /// instrument whose row the mount read, so any other symbol would be folded as the default
    /// (`Linear`, 1.0) — a manual `ETH-PERPETUAL` fill on the same sub-account would book PnL a thousand
    /// times too high. The engine DROPS such a fill (and says so) instead of folding it. The first
    /// test is a length check, so an engine with no inverse symbol pays nothing for this.
    #[inline]
    pub fn kind_is_unknown_for(&self, symbol: &str) -> bool {
        !self.values.is_empty() && !self.values.contains_key(symbol) && self.trades_inverse()
    }

    /// Whether this account trades an INVERSE contract: some listed symbol is `Inverse` (the default
    /// is `Linear`, so an inverse symbol is always listed). Cold path (balance frames, reconcile).
    pub fn trades_inverse(&self) -> bool {
        self.values.values().any(|v| v.kind == vike_model::NotionalKind::Inverse)
    }

    /// Install the per-symbol ruling-margin-mode grid (see `Self::default_margin_modes`). A
    /// CONSUMING builder rather than a fifth [`Account::new`] parameter: only the venue composition
    /// root has a grid to give. ⚠ [`Account::restore`] does not carry the grid, so a restore path
    /// must re-apply it here.
    ///
    /// `None` (or an empty map) leaves the account as if never called. **Only rows that differ
    /// from [`MarginMode::Cross`] belong here**; a `Cross` row only costs the fold a hash.
    #[must_use]
    pub fn with_default_margin_modes(
        mut self,
        modes: Option<IndexMap<String, MarginMode>>,
    ) -> Self {
        if let Some(m) = modes {
            self.default_margin_modes = std::sync::Arc::new(m);
        }
        self
    }

    /// The margin mode a position OPENED FROM FLAT on `symbol` takes — the venue's per-asset truth
    /// where it differs from [`MarginMode::Cross`], and `Cross` everywhere else. An ADMITTED symbol's
    /// own row ([`Self::admit_margin_mode`]) is read first, then the mount's grid.
    ///
    /// The `is_empty` short-circuit means that on every venue but hyperliquid the fold never
    /// hashes the symbol; it is reached only on open-from-flat anyway (see `Self::fold`).
    pub fn default_margin_mode_of(&self, symbol: &str) -> MarginMode {
        if self.default_margin_modes.is_empty() && self.admitted_margin_modes.is_empty() {
            return MarginMode::Cross;
        }
        self.admitted_margin_modes
            .get(symbol)
            .or_else(|| self.default_margin_modes.get(symbol))
            .copied()
            .unwrap_or(MarginMode::Cross)
    }

    /// State the margin mode of an ADMITTED `symbol` (decision 0218), the one writer of
    /// [`Self::admitted_margin_modes`] besides `restore`: called by `Engine::admit_symbol`.
    /// The grid stays SPARSE: `None` and `Some(Cross)` both mean "the type default" and REMOVE a row an earlier
    /// admission wrote, any other mode is stored (a repeat admission replaces it). Copy-on-write through the
    /// `Arc`, and it moves no position: a position already open keeps its own mode (the fold carries it
    /// forward), only the NEXT one opened from flat reads this.
    pub fn admit_margin_mode(&mut self, symbol: &str, mode: Option<MarginMode>) {
        match mode.filter(|m| !m.is_cross()) {
            Some(mode) => {
                std::sync::Arc::make_mut(&mut self.admitted_margin_modes)
                    .insert(symbol.to_string(), mode);
            }
            None => {
                if self.admitted_margin_modes.contains_key(symbol) {
                    std::sync::Arc::make_mut(&mut self.admitted_margin_modes).shift_remove(symbol);
                }
            }
        }
    }

    /// Fold one venue execution into the money: position, realized PnL, cash and per-asset fees.
    ///
    /// **IDEMPOTENT PER `trade_id`.** The dedup ledger is this type's own field, checked HERE before
    /// anything moves, so the guarantee holds for every caller. A repeat returns
    /// [`FillFold::Duplicate`] having mutated nothing but [`Self::duplicate_fills_refused`]; see
    /// the module doc for the key choice, the collision detector and why the set is unbounded.
    ///
    /// **REFUSED, never panicking.** Several venues re-deliver executions by design, and the one
    /// live caller (`Engine::on_event`) runs on the fold thread the `p99 < 10µs` gate
    /// measures, where no invariant violation may take the process down (`refuse_nonfinite` is the
    /// precedent: count, log the fault, drop the event). Panicking on a double-counted fill is
    /// defensible for a library whose caller can catch it, wrong for a daemon holding live
    /// positions and resting orders. That includes a fill of ANOTHER venue (a routing fault):
    /// [`FillFold::ForeignVenue`], logged at ERROR, before anything is read or moved.
    ///
    /// **Hot-path cost:** one ledger lookup on the fill's `trade_id` (`&str` borrow, no allocation on
    /// the duplicate path: a keyed hash for the shard, then that shard's std map), plus one `String`
    /// allocation for the inserted key on the accept path. No logging.
    ///
    /// The entry is stamped day `0`; the engine's fold stamps its own clock's day
    /// ([`Self::apply_fill_on_day`]), which is what a bounded snapshot windows the ledger by.
    pub fn apply_fill(&mut self, fill: &FillEvent) -> FillFold {
        self.apply_fill_on_day(fill, 0)
    }

    /// [`Self::apply_fill`], the ledger entry stamped with the engine `day` that folds it
    /// (`fill_window::day_of(now_ms)`). A ledger holding a FLOOR (`fill_window`'s module doc) also refuses a
    /// fill whose id it does not hold when the fill is stamped before the floor, as a [`FillFold::Duplicate`]
    /// (counted, nothing moved, the id not burned: the window left out the ids of that time), and when it is
    /// stamped NOT AT ALL, as [`FillFold::Unproven`] (nothing proves it new, so it never books; the engine
    /// parks it for the operator). One `Option` test on the accept path; `None` on every ledger that left
    /// nothing out.
    pub fn apply_fill_on_day(&mut self, fill: &FillEvent, day: u32) -> FillFold {
        if fill.venue != self.venue {
            return self.refuse_foreign_venue(fill);
        }
        // THE WELD. Consult the ledger before a single field moves, and record the id only on the
        // accept path so a refused fill never burns an id it did not fold. There is NO untagged
        // branch: `fill.trade_id` is a `TradeId`, which cannot be empty (#1341).
        {
            let print = fill_print(fill);
            // `get(&str)` first, and `insert` (which copies the id) only on the accept path: an
            // insert up front would allocate before it knows the answer, and on bybit/binance-perp
            // the DUPLICATE path is roughly half of all fills.
            if let Some(&(folded, _)) = self.seen_fill_ids.get(fill.trade_id.as_str()) {
                if folded == print || folded == PRINT_UNKNOWN {
                    self.duplicate_fills_refused += 1;
                    return FillFold::Duplicate;
                }
                self.colliding_fills_refused += 1;
                // A FAULT TRANSITION, not per-message noise (re-deliveries took the silent branch
                // above): a REAL fill was just dropped, so it logs like
                // `Engine::refuse_nonfinite` does.
                tracing::error!(
                    target: "vike_execution::account",
                    venue = %self.venue,
                    symbol = %fill.symbol,
                    trade_id = %fill.trade_id,
                    order_client_id = %fill.order_client_id,
                    side = fill.side,
                    qty = fill.last_qty,
                    px = fill.last_price,
                    "REFUSED a fill whose trade_id was ALREADY FOLDED under a DIFFERENT \
                     (symbol, side, qty, px) — this is an id COLLISION, not a reconnect replay, so a \
                     GENUINE fill has been dropped and this account now understates the position. \
                     The producer of these ids is not unique per account (binance/aster `t` and okx \
                     `tradeId` are per-SYMBOL sequences; vike-paper's `paper-` counter is per-CLIENT). \
                     Reconcile this venue."
                );
                self.note_unfolded_cash();
                return FillFold::Collision;
            }
            if self.fill_floor_ms.is_some() {
                if self.before_fill_floor(fill.timestamp) {
                    self.duplicate_fills_refused += 1;
                    return FillFold::Duplicate;
                }
                if self.unprovable_fill(fill.timestamp) {
                    return FillFold::Unproven;
                }
            }
            self.seen_fill_ids.insert(fill.trade_id.as_str(), (print, day));
        }
        // The key is built from the fill's ALREADY-interned `venue`/`symbol` and enum
        // `position_side`: three `Copy` moves, zero allocation.
        let key: PositionKey = (fill.venue, fill.symbol, fill.position_side);
        let value = self.contract_value_of(&fill.symbol);
        self.fold(key, fill.side, fill.last_qty, fill.last_price, value);
        // Net the trade commission into cash, like apply_liquidation nets its fee. Signed:
        // commission > 0 is a charge (lowers balance), < 0 is a maker rebate (raises it).
        //
        // ⚠ On an INVERSE contract the fee is charged in the SETTLE COIN (deribit: BTC), and the
        // ledger is one currency, the engine's quote: the coin amount enters at the fill price, the
        // same conversion the inverse PnL takes (decision 0170, item 7). A fee already in a dollar
        // asset, or on any linear contract, enters as it always did.
        //
        // ⚠ With a foreign-quoted symbol on the books the fee also has to reach the ACCOUNT's
        // currency (`fx_commission`); the test is one `Option` check, so a single-currency account
        // takes exactly the expression it always did.
        //
        // ⚠ A COIN LEDGER (decision 0258) keeps the inverse contract's coin fee in the coin; a linear
        // fill fails the kind test first and takes the expression it always did.
        let charge = if self.fx.is_none() {
            if value.kind == vike_model::NotionalKind::Inverse && self.coin.is_some() {
                if fill.last_price > 0.0 {
                    self.coin_price = fill.last_price;
                }
                Self::commission_in_settle(value, fill)
            } else {
                Self::commission_in_quote(value, fill)
            }
        } else {
            self.fx_commission_booked(value, fill)
        };
        self.balance -= charge;
        self.fees_paid += charge;
        // ADDITIVE per-asset fee attribution (parity law A1): the SAME signed commission into
        // `fees_by_asset` when the venue surfaced the fee currency (empty = no-op). SEPARATE from
        // `balances_by_asset`, so the holdings never read a fee tally as a negative holding. The
        // fee asset is often the base asset or a discount token (BNB), so per-asset fees
        // legitimately span assets the quote-denominated `fees_paid` scalar collapses together.
        if !fill.commission_asset.is_empty() {
            // `commission_asset` is a `Ustr` too: no `to_string()` on the fill path.
            *self.fees_by_asset.entry(fill.commission_asset).or_insert(0.0) += fill.commission;
        }
        FillFold::Applied
    }

    /// [`Self::apply_fill`]'s refusal of a fill of another venue: nothing moves and no id is burned.
    /// `#[cold]` and out of line, so the venue compare it follows (the one the old `assert_eq!` was)
    /// leaves the fill path as short as it was; the log is a fault transition, never per message.
    #[cold]
    #[inline(never)]
    fn refuse_foreign_venue(&self, fill: &FillEvent) -> FillFold {
        tracing::error!(
            target: "vike_execution::account",
            fill_venue = %fill.venue,
            account_venue = %self.venue,
            symbol = %fill.symbol,
            trade_id = %fill.trade_id,
            order_client_id = %fill.order_client_id,
            "REFUSED a fill of another venue: it was routed to the wrong account, so NOTHING was \
             folded. The router answers an engine of the event's own venue, so this is a routing \
             fault; reconcile re-converges the account that traded it."
        );
        FillFold::ForeignVenue
    }

    /// **A fill or a liquidation this account refused moved the venue's wallet and nothing here**, so
    /// the next reconcile passes may take the venue's figure rather than hold it for an operator
    /// (`vike_core`'s `adopt_cash_the_book_left_out`). A store of one byte, on the refusals only.
    #[inline]
    pub fn note_unfolded_cash(&mut self) {
        self.unfolded_cash_passes = UNFOLDED_CASH_PASSES;
    }

    /// Whether a recent refusal ([`Self::note_unfolded_cash`]) explains a balance drift.
    pub fn has_unfolded_cash(&self) -> bool {
        self.unfolded_cash_passes > 0
    }

    /// One pass that reported a balance has looked: the leave runs out after
    /// [`UNFOLDED_CASH_PASSES`] of them, so a later move with nothing behind it is held again.
    pub fn spend_unfolded_cash_pass(&mut self) {
        self.unfolded_cash_passes = self.unfolded_cash_passes.saturating_sub(1);
    }

    /// Fold a LATER correction of the fee of a fill this account already folded ([`FeeAdjustmentEvent`]):
    /// the one way a fee reaches the ledger after its fill, because [`Self::apply_fill`] is idempotent
    /// per `trade_id` and a second fill with another fee is a duplicate that moves nothing.
    ///
    /// **Booked once, by the difference.** The event states how much MORE the execution's fee is than
    /// its fill carried, IN TOTAL; this ledger remembers the total it has booked under the `trade_id`
    /// ([`FeeBooked`]) and books the CHANGE into `balance`, `fees_paid` and `fees_by_asset`, converted
    /// exactly as a fill's commission is (the inverse-contract coin and the foreign-quoted account's
    /// rate: `commission_in_quote`, `fx_commission_booked`). The same event again, or IB repeating a
    /// commission it already sent, states the booked total and books nothing ([`FeeFold::Unchanged`]);
    /// a later one with another total books the difference; one whose predecessor was lost still lands
    /// the right total. A `trade_id` no fill was folded under books nothing
    /// ([`FeeFold::UnknownFill`]).
    ///
    /// **Not the hot path**: an adjustment is rare, and the fill path's cost is untouched (nothing in
    /// [`Self::apply_fill`] reads the adjustment ledger).
    pub fn apply_fee_adjustment(&mut self, event: &FeeAdjustmentEvent) -> FeeFold {
        if event.venue != self.venue {
            return self.refuse_foreign_fee(event);
        }
        let trade_id = event.trade_id.as_str();
        if !self.seen_fill_ids.contains_key(trade_id) {
            return FeeFold::UnknownFill;
        }
        let prior = self.fee_adjusted.get(trade_id).copied();
        // A fill that was never corrected has booked an extra of nothing.
        let (booked_extra, booked_asset, booked_charge) = match prior {
            Some(p) => (p.extra, p.asset, p.charge),
            None => (0.0, event.commission_asset, 0.0),
        };
        if event.extra_commission == booked_extra && event.commission_asset == booked_asset {
            return FeeFold::Unchanged;
        }
        let fill = event.as_fee_only_fill();
        let value = self.contract_value_of(&fill.symbol);
        let total = if self.fx.is_none() {
            if value.kind == vike_model::NotionalKind::Inverse && self.coin.is_some() {
                Self::commission_in_settle(value, &fill)
            } else {
                Self::commission_in_quote(value, &fill)
            }
        } else {
            self.fx_commission_booked(value, &fill)
        };
        let charge = total - booked_charge;
        self.balance -= charge;
        self.fees_paid += charge;
        // The per-asset tally follows the same signed amount; a correction that changes the currency
        // takes the old amount back from the old currency.
        if let Some(p) = prior
            && !p.asset.is_empty()
        {
            *self.fees_by_asset.entry(p.asset).or_insert(0.0) -= p.extra;
        }
        if !event.commission_asset.is_empty() {
            *self.fees_by_asset.entry(event.commission_asset).or_insert(0.0) +=
                event.extra_commission;
        }
        self.fee_adjusted.insert(
            trade_id.to_string(),
            FeeBooked {
                extra: event.extra_commission,
                asset: event.commission_asset,
                charge: total,
            },
        );
        FeeFold::Applied { charge }
    }

    /// [`Self::apply_fee_adjustment`]'s refusal of an adjustment of another venue: a routing fault,
    /// nothing moves. `#[cold]` and out of line, a fault transition like [`Self::refuse_foreign_venue`].
    #[cold]
    #[inline(never)]
    fn refuse_foreign_fee(&self, event: &FeeAdjustmentEvent) -> FeeFold {
        tracing::error!(
            target: "vike_execution::account",
            adjustment_venue = %event.venue,
            account_venue = %self.venue,
            symbol = %event.symbol,
            trade_id = %event.trade_id,
            order_client_id = %event.order_client_id,
            "REFUSED a fee adjustment of another venue: it was routed to the wrong account, so \
             NOTHING was booked. The router answers an engine of the event's own venue, so this is \
             a routing fault."
        );
        FeeFold::ForeignVenue
    }

    /// Sole writer of position + realized PnL. `apply_fill` and `apply_liquidation` both call
    /// ONLY this for position/realized mutation (one compute_fill block, no drift).
    fn fold(
        &mut self,
        key: PositionKey,
        side_sign: i32,
        qty: f64,
        px: f64,
        value: vike_model::ContractValue,
    ) {
        // Prior entry: `margin_mode`/`isolated_margin` are carried forward verbatim, so an
        // isolated position stays isolated across fills.
        //
        // ⚠ OPEN-FROM-FLAT takes the ASSET's ruling mode, not `PositionEntry::default()`: a bare
        // `unwrap_or_default()` books EVERY new position `Cross` (the type default), wrong at birth
        // on hyperliquid's isolated-only assets, and nothing corrects it — HL heals size through
        // `recon::diff`/`resolve`, which never reads `PositionStatusReport::margin_mode` and
        // synthesizes `Fill`s that re-enter HERE, and `Engine::apply_snapshot`'s
        // `ReconcileSnapshot::position_margin` overwrite has no hyperliquid producer. And it is
        // READ: the admitting gate's margin fold and `check_margin_call_priced`'s pool partition
        // key on `is_cross()`, and `vike_core`'s liquidation-price badge routes on it.
        //
        // COST: a fill into an EXISTING position (the steady state the p99 gate measures) is
        // unchanged; the grid read happens only on the miss and short-circuits on an empty grid.
        let prior = match self.positions.get(&key) {
            Some(p) => *p,
            None => PositionEntry {
                margin_mode: self.default_margin_mode_of(&key.1),
                ..Default::default()
            },
        };
        let out = vike_model::compute_fill_value(
            prior.size,
            prior.average_price,
            side_sign,
            qty,
            px,
            value,
        );
        // rebind, not in-place mutate
        self.positions.insert(
            key,
            PositionEntry {
                size: out.new_size,
                average_price: out.new_average_price,
                margin_mode: prior.margin_mode,
                isolated_margin: prior.isolated_margin,
            },
        );
        if out.closing_qty > 0.0 {
            // a reduce / close / flip realized PnL on the closed portion; a COIN LEDGER books it in the
            // settle coin, the dollars at the exit divided by the exit price (`s·u·(1/e − 1/p)`, decision 0258)
            let pnl = if self.coin.is_some() {
                value.in_settle(out.realized_pnl, px)
            } else {
                out.realized_pnl
            };
            self.realized_pnl += pnl;
            self.closed_pnls.push(pnl);
        }
    }

    /// The fill's commission in the ledger's one currency. An INVERSE contract charges its fee in
    /// the settle coin: any fee asset that is not a dollar asset enters at the fill price. Everything
    /// else is the commission as the venue stated it.
    ///
    /// `pub` for the per-mount ledger (`vike-core`'s `dispatch_applied_fills`), which must book the
    /// SAME charge the account booked: a pure function of the fill and its contract value.
    #[inline]
    pub fn commission_in_quote(value: vike_model::ContractValue, fill: &FillEvent) -> f64 {
        // the kind test first, so a linear fill never compares the asset string
        if value.kind == vike_model::NotionalKind::Inverse
            && !is_dollar_asset(fill.commission_asset.as_str())
        {
            value.in_quote(fill.commission, fill.last_price)
        } else {
            fill.commission
        }
    }

    /// The fill's commission in the money its contract SETTLES in ([`vike_model::ContractValue::in_settle`]):
    /// on an INVERSE contract the settle coin, so a fee stated in a dollar asset (the paper book's) is
    /// divided by the fill price and any other is the venue's coin as stated; on a linear contract the
    /// commission as stated, whatever its asset. A fill with no positive price keeps its figure as stated
    /// (`in_settle` would answer 0.0 there).
    ///
    /// The ONE coin-fee rule: a COIN LEDGER books its inverse fees with it ([`Self::apply_fill`],
    /// [`Self::apply_fee_adjustment`]), and it is `pub` for the fill a STRATEGY hears (`vike-core`'s
    /// `dispatch_applied_fills`), whose own PnL fold, `vike_model::TradeFold::apply_value`, books an
    /// inverse contract's PnL in the settle coin and takes the fee in that same money (decision 0258), so
    /// the strategy and the ledger cannot disagree.
    #[inline]
    pub fn commission_in_settle(value: vike_model::ContractValue, fill: &FillEvent) -> f64 {
        if is_dollar_asset(fill.commission_asset.as_str()) && fill.last_price > 0.0 {
            value.in_settle(fill.commission, fill.last_price)
        } else {
            fill.commission
        }
    }

    /// Fold a periodic funding cashflow into the cash balance (signed: + received / - paid).
    pub fn apply_funding(&mut self, event: &FundingEvent) {
        // ⚠ On a ledger that holds a FOREIGN-QUOTED symbol the amount converts by the currency the
        // event states (`FundingEvent::currency`): the account's books as stated, one the book holds a
        // rate for books times that rate, any other is left out. An event that states NO currency (every
        // producer today) is left out on a foreign-quoted symbol: its amount could be the account's,
        // the symbol's quote's or a third one's, and each reading is a different number (150x apart
        // for a JPY pair), so booking it at par is the guess this engine refuses to make. What is
        // left out is counted and said, the authoritative balance already holds it (reconcile
        // re-converges the rest).
        if self.fx.is_some() {
            match self.fx_stated(
                &event.symbol,
                event.currency,
                event.amount,
                event.timestamp,
                "funding",
            ) {
                fx::AccountLedgerFxBooking::Plain => {}
                fx::AccountLedgerFxBooking::Converted(amount) => {
                    self.balance += amount;
                    self.funding_paid += amount;
                    return;
                }
                fx::AccountLedgerFxBooking::LeftOut => return,
            }
        }
        // On an INVERSE contract the amount is in the settle coin: a COIN LEDGER books it as stated
        // (decision 0258); any other ledger takes it at the event's mark when the venue stated one
        // (decision 0170, item 7).
        let amount = match event.mark_price {
            Some(mark) if self.coin.is_none() => {
                self.contract_value_of(&event.symbol).in_quote(event.amount, mark)
            }
            _ => event.amount,
        };
        self.balance += amount;
        self.funding_paid += amount;
    }

    /// Set balance AUTHORITATIVELY from a venue AccountState event.
    /// Quote-asset selection: (1) the pair whose asset == `quote_asset`; (2) a single balance,
    /// only if it is a dollar label and so is `quote_asset` (any other lone asset is reported in
    /// `balances_by_asset` and adopts nothing); (3) the sum of all qty values (last resort). Sets
    /// `balance` absolutely (not +=) and flips `balance_mode` to Authoritative.
    pub fn apply_account_state(&mut self, event: &AccountState, quote_asset: &str) {
        let balances = &event.balances;
        if balances.is_empty() {
            return;
        }
        // Per-asset ledger first (additive, upsert): partial venue frames (e.g. Binance
        // outboundAccountPosition carries only changed assets) merge instead of wiping.
        // The scalar collapse below stays byte-identical to the oracle.
        for (asset, qty) in balances {
            self.balances_by_asset.insert(asset.clone(), *qty);
        }
        // ⚠ AN INVERSE CONTRACT'S WALLET IS A COIN AMOUNT (deribit: BTC). A COIN LEDGER (decision 0258)
        // adopts the row labelled with its own quote asset: the frame the node builds from a reconcile
        // pass, whose bare figure is by definition the ledger's money (the venue's settle-coin cash). A row
        // NAMING a coin is a venue's own frame and only reported, as on any other inverse ledger, whose
        // money is dollars (decision 0170, item 7): the scalar balance, the mode and the baseline stay.
        if self.trades_inverse() {
            if self.coin.is_some()
                && let Some((_, qty)) = balances.iter().find(|(asset, _)| asset == quote_asset)
            {
                self.realized_pnl_at_balance_sync = Some(self.realized_pnl);
                self.balance = *qty;
                self.balance_mode = BalanceMode::Authoritative;
            }
            return;
        }
        // ⚠ SO IS A LEDGER WITH A FOREIGN-QUOTED SYMBOL: its `balance` is in the account's currency
        // and its PnL was converted into it, so a venue frame in ANY other currency (an IBKR
        // `CashBalance` is the mounted contract's, a multi-currency wallet lists one per currency)
        // cannot be adopted, and the fallbacks below — "a lone balance, whatever it is" and "the sum of
        // all balances" — would write yen and dollars into one number. Only the balance whose asset
        // IS `quote_asset` is the ledger; with none in the frame, the frame is reported above and
        // `balance` stays as it was. That no-match branch is a MISCONFIGURATION made loud: either the
        // engine was never told its account currency (`Engine::set_account_currency`, which a
        // foreign-quoted admission requires) or the venue's frame does not name it, and in both the
        // ledger stops following the venue until somebody fixes it, so it says so at `error!`.
        if self.fx.is_some() {
            if let Some((_, qty)) =
                balances.iter().find(|(asset, _)| vike_model::same_currency(asset, quote_asset))
            {
                self.realized_pnl_at_balance_sync = Some(self.realized_pnl);
                self.balance = *qty;
                self.balance_mode = BalanceMode::Authoritative;
            } else {
                self.fx_balance_frame_unmatched(quote_asset);
            }
            return;
        }
        // ⚠ A LONE balance is the ledger only when it is the ledger's MONEY: the quote asset itself, or
        // a dollar label against a dollar label (`USD` for `USDT`: dukascopy's one `USD` frame, a
        // `USDC`-margined wallet, both on an engine whose `quote_asset` is still the constructor's
        // `USDT` guess). Any other lone asset is another wallet entirely: binance's ACCOUNT_UPDATE
        // lists only the CHANGED assets, so a fee paid in BNB arrives as `[BNB]` alone, and writing
        // 0.5 BNB into `balance` made the ledger read half a dollar until the next USDT row. It is
        // REPORTED in `balances_by_asset` above and adopts nothing: no balance, no mode flip and no
        // cash-reconcile baseline move (the frame is not "a reseed"). Silent on purpose: this runs
        // per frame on the fold thread. The SUM fallback below is untouched (see its comment).
        if let [(asset, _)] = balances.as_slice()
            && asset != quote_asset
            && !(is_dollar_asset(asset) && is_dollar_asset(quote_asset))
        {
            return;
        }
        // Cash-reconcile baseline: a non-empty frame ALWAYS sets `balance` authoritatively below,
        // so snapshot the realized-PnL baseline here (`realized_pnl` does not change in this fn).
        // Each genuine venue balance push IS a reseed, so "realized since sync" resets to zero.
        // Inert unless `VIKE_RECONCILE_BALANCE` is on.
        self.realized_pnl_at_balance_sync = Some(self.realized_pnl);
        for (asset, qty) in balances {
            if asset == quote_asset {
                self.balance = *qty;
                self.balance_mode = BalanceMode::Authoritative;
                return;
            }
        }
        // Here a lone balance can only be a dollar label read as the dollar ledger (the guard above).
        if balances.len() == 1 {
            self.balance = balances[0].1;
            self.balance_mode = BalanceMode::Authoritative;
            return;
        }
        // shortcut: the sum of several non-quote assets is still adopted (the frozen
        // `account_state_fallbacks` fixture pins USDC+DAI+BUSD); upgrade: refuse a sum that mixes
        // a non-dollar asset, once the owner rules on the frozen fixture.
        self.balance = vike_model::py_sum(balances.iter().map(|(_a, q)| *q)); // builtin sum()
        self.balance_mode = BalanceMode::Authoritative;
    }

    /// Forced close: realize PnL at the liq price, close min(ev.qty, held), deduct the liq fee.
    /// `event.qty` falsy (0.0) closes the WHOLE held size; qty is clamped so an over-reported frame
    /// can never flip the position. Idempotent on a flat position. Routes through `fold`.
    pub fn apply_liquidation(&mut self, event: &PositionLiquidated) {
        // Same allocation-free key construction as `apply_fill` — every component is already
        // interned / a `Copy` enum on the event.
        let key: PositionKey = (event.venue, event.symbol, event.position_side);
        let pos = match self.positions.get(&key) {
            Some(p) if p.size != 0.0 => *p,
            _ => {
                // Nothing to liquidate here, and the venue's wallet moved: a position this book
                // does not hold (one a restart left held under `quarantine`).
                self.note_unfolded_cash();
                return;
            }
        };
        let close_side = vike_model::closing_side(pos.size); // close on the opposite side
        // `event.qty == 0.0` (NaN is not produced here) means the whole size.
        let close_quantity =
            if event.qty != 0.0 { event.qty.abs().min(pos.size.abs()) } else { pos.size.abs() };
        let value = self.contract_value_of(&event.symbol);
        self.fold(key, close_side, close_quantity, event.liquidation_price, value);
        // ⚠ The forced-close fee converts by the currency the event states (`fee_currency`), for the
        // funding reason (see `apply_funding`): one that states none on a FOREIGN-QUOTED symbol is
        // not charged, counted and said. The position and its PnL above are converted either way.
        let charge = if self.fx.is_some() {
            match self.fx_stated(
                &event.symbol,
                event.fee_currency,
                event.fee,
                event.timestamp,
                "liquidation fee",
            ) {
                fx::AccountLedgerFxBooking::Plain => event.fee,
                fx::AccountLedgerFxBooking::Converted(fee) => fee,
                fx::AccountLedgerFxBooking::LeftOut => 0.0,
            }
        } else {
            event.fee
        };
        self.balance -= charge;
        // The forced-close fee is a fee like any other: `fees_paid` (the figure the GUI, the summary
        // line and `resolved_own_pnl` read) and the per-asset tally take it with the SAME sign and
        // conversion `apply_fill` gives a trade commission. It used to reach `balance` alone, so every
        // liquidation under-stated `fees_paid` and the drawdown latch's own-PnL by the fee.
        self.fees_paid += charge;
        if let Some(currency) = event.fee_currency
            && !currency.is_empty()
        {
            *self.fees_by_asset.entry(currency).or_insert(0.0) += event.fee;
        }
    }

    /// Full-state DTO for the journal `Snapshot` record. Maps become ordered Vecs.
    ///
    /// ⚠ A COIN LEDGER's money rides [`crate::engine_snapshot::EngineSnapshotAccount::coin`], and its DOLLAR fields are
    /// written as nothing booked (`Delta`, zeros): a build that predates coin books reads a fresh inverse book
    /// with its positions, never a coin amount as dollars (decision 0258, item 8).
    pub fn snapshot(&self) -> crate::engine_snapshot::EngineSnapshotAccount {
        self.snapshot_with(usize::MAX)
    }

    /// [`Self::snapshot`] with the newest `closed` closed-PnL rows only (the journal `Snapshot`'s form,
    /// `crate::Engine::journal_snapshot`: nothing reads the rows after a restart).
    pub(crate) fn snapshot_with(
        &self,
        closed: usize,
    ) -> crate::engine_snapshot::EngineSnapshotAccount {
        let closed_pnls =
            || self.closed_pnls[self.closed_pnls.len().saturating_sub(closed)..].to_vec();
        let coin = self.coin.map(|(_, symbol)| crate::engine_snapshot::EngineSnapshotCoin {
            symbol: symbol.to_string(),
            balance_mode: self.balance_mode,
            balance: self.balance,
            realized_pnl: self.realized_pnl,
            closed_pnls: closed_pnls(),
            funding_paid: self.funding_paid,
            fees_paid: self.fees_paid,
            last_price: self.coin_price,
        });
        let dollars = coin.is_none();
        crate::engine_snapshot::EngineSnapshotAccount {
            venue: self.venue.clone(),
            // LINEAR values keep the shape they always had; the INVERSE ones ride the sparse list
            // below, so the canonical JSON (and `state_hash`) of an engine with none is unchanged.
            //
            // ⚠ A FOREIGN-QUOTED symbol's row is its EFFECTIVE value (the unit times today's rate);
            // what is written is its BASE (`base_value`), the rate riding `fx` below, so a restore
            // recomputes the product with the same function and nothing here goes stale with a rate.
            mult: self
                .values
                .iter()
                .map(|(k, v)| (k, self.base_value(k, *v)))
                .filter(|(_, v)| v.kind == vike_model::NotionalKind::Linear)
                .map(|(k, v)| (k.clone(), v.per_unit))
                .collect(),
            inverse: self
                .values
                .iter()
                .map(|(k, v)| (k, self.base_value(k, *v)))
                .filter(|(_, v)| v.kind == vike_model::NotionalKind::Inverse)
                .map(|(k, v)| (k.clone(), v.per_unit))
                .collect(),
            fx: self.fx_snapshot(),
            admitted_margin_modes: self
                .admitted_margin_modes
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            mult_default: self.default_unit,
            balance_mode: if dollars { self.balance_mode } else { BalanceMode::Delta },
            positions: self.positions.iter().map(|(k, v)| (*k, *v)).collect(),
            realized_pnl: if dollars { self.realized_pnl } else { 0.0 },
            closed_pnls: if dollars { closed_pnls() } else { Vec::new() },
            balance: if dollars { self.balance } else { 0.0 },
            balances_by_asset: self
                .balances_by_asset
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            marks: self.marks.iter().map(|(k, v)| (*k, *v)).collect(),
            funding_paid: if dollars { self.funding_paid } else { 0.0 },
            fees_paid: if dollars { self.fees_paid } else { 0.0 },
            fees_by_asset: self.fees_by_asset.iter().map(|(k, v)| (*k, *v)).collect(),
            fee_adjusted: self
                .fee_adjusted
                .iter()
                .map(|(id, b)| (id.clone(), b.extra, b.asset, b.charge))
                .collect(),
            coin,
        }
    }

    /// Inverse of [`Account::snapshot`] — rebuilds the exact Account (insertion orders preserved). A snapshot
    /// that carries coin books ([`crate::engine_snapshot::EngineSnapshotAccount::coin`]) restores a COIN LEDGER with
    /// that money.
    pub fn restore(snap: &crate::engine_snapshot::EngineSnapshotAccount) -> Account {
        let money = Money::of(snap);
        Account {
            venue: snap.venue.clone(),
            values: std::sync::Arc::new(
                snap.mult
                    .iter()
                    .map(|(k, v)| (k.clone(), vike_model::ContractValue::linear(*v).folded()))
                    .chain(
                        snap.inverse.iter().map(|(k, v)| {
                            (k.clone(), vike_model::ContractValue::inverse(*v).folded())
                        }),
                    )
                    .collect(),
            ),
            default_unit: snap.mult_default,
            // NOT snapshotted: this grid is MOUNT CONFIGURATION from a live venue `meta` fetch, not
            // folded state, and a snapshot copy would pin a venue's asset facts to snapshot time.
            // Restored POSITIONS keep their own (snapshotted) `margin_mode`; only an open-from-flat
            // reads this. ⚠ A live restore path must re-apply
            // [`Account::with_default_margin_modes`] after `restore`, as it re-runs the mount.
            default_margin_modes: std::sync::Arc::new(IndexMap::new()),
            // ...but an ADMITTED symbol's mode is runtime state, and the snapshot carries it.
            admitted_margin_modes: std::sync::Arc::new(
                snap.admitted_margin_modes.iter().cloned().collect(),
            ),
            balance_mode: money.balance_mode,
            positions: snap.positions.iter().cloned().collect(),
            realized_pnl: money.realized_pnl,
            closed_pnls: money.closed_pnls.to_vec(),
            balance: money.balance,
            balances_by_asset: snap.balances_by_asset.iter().cloned().collect(),
            // NOT snapshotted (see the field doc): a restored account re-adopts venue cash on its
            // first reconcile pass instead of diffing against a baseline it can't reconstruct.
            realized_pnl_at_balance_sync: None,
            unfolded_cash_passes: 0,
            marks: snap.marks.iter().cloned().collect(),
            // Provenance is NOT snapshotted (see the field doc): a restored slot is unowned, so
            // no stale owner freezes valuation after replay.
            mark_meta: IndexMap::new(),
            mark_staleness_ms: DEFAULT_MARK_STALENESS_MS,
            reconcile_staleness_ms: DEFAULT_RECONCILE_STALENESS_MS,
            funding_paid: money.funding_paid,
            fees_paid: money.fees_paid,
            fees_by_asset: snap.fees_by_asset.iter().cloned().collect(),
            // The fee corrections booked so far: folded state, so it rides the snapshot (sparse).
            fee_adjusted: fee_adjusted_of(snap),
            // NOT in `EngineSnapshotAccount`: the ledger's durable home is the top-level
            // `EngineSnapshot::seen_trade_ids`, and `Engine::from_snapshot` seeds it via
            // [`Account::seed_seen_fill_ids`] right after this call. A BARE `Account::restore`
            // starts empty, like `Account::new` (a bare `Account` folds no live stream).
            seen_fill_ids: TradeIdLedger::default(),
            fill_floor_ms: None,
            // LIVE HEALTH SIGNALS, not folded state (the `mark_meta` / `dropped_nonfinite`
            // doctrine): a prior session's tally would misreport this session's.
            duplicate_fills_refused: 0,
            colliding_fills_refused: 0,
            // `values` above holds BASE values; the fx book (when the snapshot has one) turns the
            // foreign-quoted ones into their effective value with the function the admission used.
            fx: None,
            coin: snap.coin.as_ref().map(|c| (Ustr::from(&snap.venue), Ustr::from(&c.symbol))),
            coin_price: snap.coin.as_ref().map_or(0.0, |c| c.last_price),
        }
        .with_restored_fx(snap)
    }

    /// The FOLDED half of [`Account::restore`], written onto an account its mount already built —
    /// the boot restore's account step (`crate::Engine::restore_runtime_state` is the one
    /// caller). Takes the snapshot's positions, realized PnL and closed PnLs, balance and per-asset
    /// balances, funding and fees, and the balance mode when the snapshot's is `Authoritative`
    /// (below); keeps everything this account was CONSTRUCTED with: the venue,
    /// the contract-value grid and the default margin modes (mount configuration),
    /// the marks and their provenance (market data, re-written by the first tick), the two
    /// mark-ownership windows (set by the runtime) and the live health counters. The cash-sync
    /// baseline stays `None`, as after [`Account::restore`], so the first reconcile pass re-adopts
    /// venue cash. The fill ledger is seeded separately, by the caller, from the engine snapshot.
    ///
    /// ⚠ The mode travels with the balance: an `Authoritative` balance is the venue's whole wallet,
    /// and under the mount's `Delta` [`Account::equity_all`] would add the configured seed on top of
    /// it. A ledger with a foreign-quoted (or inverse) symbol never gets re-seeded to fix that:
    /// `reconcile_reports` withholds the venue balance from it. The mode only ever moves `Delta` to
    /// `Authoritative`, so a `Delta` snapshot leaves the mount's mode alone.
    ///
    /// ⚠ The money is read only in the unit this account keeps (decision 0258): a COIN LEDGER takes a snapshot's
    /// coin books, and a snapshot without them (written before coin books, its money in dollars) restores the
    /// positions and leaves the money as the mount built it, at zero (no migration); a dollar ledger takes the
    /// dollar fields, never coin books.
    pub fn restore_folded(&mut self, snap: &crate::engine_snapshot::EngineSnapshotAccount) {
        self.positions = snap.positions.iter().cloned().collect();
        if self.coin.is_some() == snap.coin.is_some() {
            let money = Money::of(snap);
            self.realized_pnl = money.realized_pnl;
            self.closed_pnls = money.closed_pnls.to_vec();
            self.balance = money.balance;
            if money.balance_mode == BalanceMode::Authoritative {
                self.balance_mode = BalanceMode::Authoritative;
            }
            self.funding_paid = money.funding_paid;
            self.fees_paid = money.fees_paid;
            if let Some(c) = &snap.coin {
                self.coin_price = c.last_price;
            }
        }
        self.balances_by_asset = snap.balances_by_asset.iter().cloned().collect();
        self.realized_pnl_at_balance_sync = None;
        self.fees_by_asset = snap.fees_by_asset.iter().cloned().collect();
        self.fee_adjusted = fee_adjusted_of(snap);
    }
}

/// The money of a snapshot in the unit it was kept in: its coin books when it carries them (decision 0258), else
/// its dollar fields.
struct Money<'a> {
    balance_mode: BalanceMode,
    balance: f64,
    realized_pnl: f64,
    closed_pnls: &'a [f64],
    funding_paid: f64,
    fees_paid: f64,
}

impl<'a> Money<'a> {
    fn of(snap: &'a crate::engine_snapshot::EngineSnapshotAccount) -> Self {
        match &snap.coin {
            Some(c) => Money {
                balance_mode: c.balance_mode,
                balance: c.balance,
                realized_pnl: c.realized_pnl,
                closed_pnls: &c.closed_pnls,
                funding_paid: c.funding_paid,
                fees_paid: c.fees_paid,
            },
            None => Money {
                balance_mode: snap.balance_mode,
                balance: snap.balance,
                realized_pnl: snap.realized_pnl,
                closed_pnls: &snap.closed_pnls,
                funding_paid: snap.funding_paid,
                fees_paid: snap.fees_paid,
            },
        }
    }
}

/// The snapshot's fee-correction rows back into the ledger's map.
fn fee_adjusted_of(
    snap: &crate::engine_snapshot::EngineSnapshotAccount,
) -> std::collections::BTreeMap<String, FeeBooked> {
    snap.fee_adjusted
        .iter()
        .map(|(id, extra, asset, charge)| {
            (id.clone(), FeeBooked { extra: *extra, asset: *asset, charge: *charge })
        })
        .collect()
}

#[cfg(test)]
mod account_tests;

#[cfg(test)]
mod mark_law_tests;

#[cfg(test)]
mod fill_dedup_tests;

#[cfg(test)]
mod fee_adjustment_tests;
