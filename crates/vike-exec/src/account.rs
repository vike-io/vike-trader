//! FillEvent-derived account read-model: positions + realized PnL from the fill stream.
//!
//! - per-venue: `apply_fill` asserts `fill.venue == self.venue`.
//! - per-symbol multiplier: `multiplier_of(symbol)` with the legacy scalar default.
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
//! the `exec_actor::run_loop` gap sentinel's resync, the `user_data::run_resync_supervisor`
//! replay, hyperliquid's `userFills` `isSnapshot` frame (re-sent on EVERY reconnect), and the
//! venue twins listed on [`Account::duplicate_fills_refused`]. So [`Account`] owns the ledger,
//! [`Account::apply_fill`] consults it before anything moves and reports the outcome as a
//! [`FillFold`]: a caller that ignores the return still cannot double-count.
//! `ExecutionEngine`'s `seen_trade_ids()`, `local_view()`, `EngineSnapshot` wire and
//! `seed_seen_trade_ids` all read/write THIS set — one authority.
//!
//! **The key choice: the bare `trade_id`, its narrowness DETECTED rather than assumed away.**
//! A fully qualified key would be `(AccountId, InstrumentId, TradeId)`. The account component is redundant here
//! (an `Account` IS per-venue-per-account), the instrument one is NOT: binance/aster `t` and okx
//! `tradeId` are per-SYMBOL venue sequences, so on a multi-symbol engine
//! (`ExecutionEngine::extra_symbols`, which combo lowering populates) two executions can share an
//! id string; `vike_paper`'s `paper-{n}` and `vike_exec::testing::TestExecutionClient`'s
//! `simt{n}` are per-CLIENT counters, so `MultiPaperExecutionClient`'s per-symbol books collide by
//! construction. The key stays bare because WIDENING it loses money: the reconcile lane compares
//! venue ids against this set as BARE STRINGS (`vike_exec::recon::diff`'s
//! `local.seen_trade_ids.contains(f.trade_id.as_str())`), and a `FillReport`'s symbol is the
//! venue's spelling, not necessarily the unified one, so with a composite key `diff` would call an
//! already-folded fill a `MissingFill` and `resolve` would fold it a SECOND time. Widening also
//! moves `EngineSnapshot::seen_trade_ids` off `Vec<String>`, changing the journal `state_hash`
//! fence, which is the deferred schema-versioning question (`docs/decisions/`).
//!
//! **The fingerprint collision detector.** Beside each id the ledger stores an FNV-1a64 over
//! `(symbol, side, last_qty, last_px)` (`fill_print`). Same id AND same fingerprint is a
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
//! **Why the set is UNBOUNDED on purpose.** It grows tens of bytes per distinct fill for the
//! process lifetime. Every cap (Hummingbot's 2000, an LRU) re-admits a duplicate the moment a
//! replay window reaches back further than the cap, and the widest window here is not a fill
//! COUNT: `VIKE_RECONCILE_LOOKBACK_MS` is one hour by default and the venue history endpoints are
//! row-count-bounded with no start time. This platform's peak sustained fill rate is unmeasured,
//! so no cap can be shown to sit above that window. Bounding errs toward silently re-admitting a
//! duplicate; not bounding errs toward memory. See `crates/vike-exec/CLAUDE.md`.
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
//! [`crate::engine_snapshot::AccountSnapshot`] and the `state_hash` fence over its canonical JSON
//! are byte-identical to `String` keys (pinned by
//! `engine_snapshot::tests::account_snapshot_key_wire_is_byte_identical`).
//!
//! ONE deliberate narrowing: the third key element is the closed-set [`PositionSide`], so a venue
//! label outside `{BOTH,LONG,SHORT}` folds to `Both` rather than minting a fourth, orphaned key.
//! Every venue already normalizes to those three before `ReconcileSnapshot::position_sides`, and
//! the fill lane never had a string there, so no live producer changes behavior.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ustr::Ustr;
use vike_model::MarginMode;
use vike_model::compute_fill;
use vike_model::events::{AccountState, FillEvent, FundingEvent, PositionLiquidated, PositionSide};

mod marks;
mod valuation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BalanceMode {
    Delta,
    Authoritative,
}

/// A position ledger entry: `{"size", "avg_px"}`, plus the margin-mode carrier.
///
/// `margin_mode`/`isolated_margin` are `#[serde(skip_serializing_if)]` on their default
/// (`Cross` / `None`), so a cross position serializes to EXACTLY `{"size":…,"avg_px":…}` —
/// keeping every snapshot/journal, and the `AccountSnapshot` `state_hash` fence, byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct PositionEntry {
    pub size: f64,
    pub avg_px: f64,
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
/// (`RiskContext.mark_price`, `margin_in_use_by`, `equity_all`), the margin-call law
/// (`check_margin_call`), the trailing-stop seed and `LiveBroker.price`. Feeds write it at very
/// different cadences, so without a rule a klines lane at one bar/second would stomp a 1s venue
/// mark.
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
    /// (`ExecReport::position_mark_px`). Genuinely the venue's mark — just sampled at the
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
/// default `VIKE_RECONCILE_INTERVAL_MS` (60s), so the slot stays owned CONTINUOUSLY between
/// passes. MUST exceed the deployment's reconcile interval. Mirrored by
/// `vike_core::CoreConfig::reconcile_mark_staleness_ms`, which overwrites it on the live path.
pub const DEFAULT_RECONCILE_STALENESS_MS: i64 = 150_000;

/// The fingerprint stored for a ledger entry whose identity is UNKNOWN — a `trade_id` seeded from a
/// durable source ([`Account::seed_seen_fill_ids`]) rather than observed by this session's fold.
/// Compares equal to nothing, so such an entry is refused as a plain duplicate, never reported as
/// a collision (prior-session fingerprints are not on the journal wire).
pub const PRINT_UNKNOWN: u64 = 0;

/// FNV-1a64 over the four fields that IDENTIFY a fill within one account: `(symbol, side,
/// last_qty, last_px)`. Two deliveries of ONE venue execution agree on all four; two DIFFERENT
/// executions sharing a `trade_id` string almost never do. Deliberately excludes
/// `commission`/`ts`/`client_order_id`/`mark_price`: a re-delivery can legitimately restate a fee
/// or a timestamp (binance perp's early TRADE_LITE fill carries no commission, its authoritative
/// twin does), and that must not read as a collision.
///
/// f64s are hashed by `to_bits()` — an identity test, not arithmetic (a non-finite fill is refused
/// by the engine's `numbers_finite` guard before it reaches here). Allocation-free. Never returns
/// [`PRINT_UNKNOWN`] (a computed `0` maps to `1`).
#[inline]
fn fill_print(fill: &FillEvent) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(fill.symbol.as_str().as_bytes());
    eat(&(fill.side as i64).to_le_bytes());
    eat(&fill.last_qty.to_bits().to_le_bytes());
    eat(&fill.last_px.to_bits().to_le_bytes());
    if h == PRINT_UNKNOWN { 1 } else { h }
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
}

impl FillFold {
    /// Whether the fill moved money, without enumerating the two refusal reasons.
    pub fn applied(self) -> bool {
        matches!(self, FillFold::Applied)
    }
}

/// Folds a stream of `FillEvent`s into positions and realized PnL.
#[derive(Debug, Clone)]
pub struct Account {
    pub venue: String,
    /// per-symbol contract multipliers; `multiplier_of` falls back to the legacy scalar.
    /// IMMUTABLE after construction (written only by `new`/`restore`), behind an `Arc` so
    /// read-models share the grid by pointer-copy ([`Account::multiplier_grid`],
    /// [`crate::VenueBlock::multipliers`]).
    mult: std::sync::Arc<IndexMap<String, f64>>,
    mult_default: f64,
    /// Per-symbol RULING margin mode for a position this account OPENS FROM FLAT — the venue's
    /// per-ASSET truth, resolved once at the mount and handed in by the composition root.
    ///
    /// A grid, not a scalar, because the mode is a property of the ASSET:
    /// `VenueCaps::default_margin_mode` says `Cross` for hyperliquid, but on an `onlyIsolated`
    /// asset the mode that RULES is `Isolated` (#1087;
    /// `vike_hyperliquid::symbology::InstrumentRef::effective_margin_mode`).
    ///
    /// SPARSE ON PURPOSE — absent means `Cross`, like [`Self::mult`] for 1.0: the map is EMPTY for
    /// every venue but hyperliquid, and [`Self::default_margin_mode_of`] short-circuits on empty.
    /// It is consulted ONLY when [`Self::fold`] finds no prior entry (open-from-flat).
    ///
    /// IMMUTABLE after construction (written only by [`Account::with_default_margin_modes`]),
    /// behind an `Arc` like [`Self::mult`].
    default_margin_modes: std::sync::Arc<IndexMap<String, MarginMode>>,
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
    /// `realized_pnl` at the instant [`Self::apply_account_state`] last set `balance`
    /// (AUTHORITATIVE sync); `None` = never synced. The cash reconcile (`VIKE_RECONCILE_BALANCE`)
    /// reads it to correct the money-drift CONFOUND: realized PnL never enters `balance` in
    /// Authoritative mode (see [`Self::equity_all`]), so the diff compares venue cash against
    /// `balance + (realized_pnl - realized_pnl_at_balance_sync)` and a realized trade never
    /// false-flags. NOT in `AccountSnapshot` (like `mark_meta`): a restore starts `None`, so the
    /// first post-restore pass re-adopts venue cash instead of diffing a stale baseline. Never
    /// read by any fold formula.
    pub realized_pnl_at_balance_sync: Option<f64>,
    /// PRIVATE ON PURPOSE — the only way in is [`Account::set_mark_from`], which carries the
    /// precedence law (see [`MarkSource`]); a `pub` field is how the law got bypassed. Read it
    /// back with [`Account::mark_of`] / [`Account::marks_iter`].
    marks: IndexMap<MarkKey, f64>,
    /// Per-symbol provenance of the CURRENT [`Self::marks`] entry: which concept wrote it and
    /// the CORE-clock ms at which it was observed. Not in `AccountSnapshot` (derived from
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
    /// THE fill-dedup ledger — every `trade_id` this account has already folded. PRIVATE ON PURPOSE
    /// (the `marks` doctrine): the only way a fill reaches the money is [`Account::apply_fill`],
    /// which consults this first. Read it back with [`Account::seen_fill_ids`]; pre-load it with
    /// [`Account::seed_seen_fill_ids`].
    ///
    /// A `HashMap`, unlike the rest of this type: it feeds NO f64 fold, and the one place its order
    /// would escape (`EngineSnapshot::seen_trade_ids`) sorts it. `trade_id -> `[`fill_print`] of
    /// the fill folded under it ([`PRINT_UNKNOWN`] for a seeded entry).
    seen_fill_ids: HashMap<String, u64>,
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
    /// `paper-{n}` under `MultiPaperExecutionClient`). The cure is at the PRODUCER (qualify the id)
    /// or reconcile; this counter makes the loss visible. Each occurrence is logged at ERROR — a
    /// fault transition, not per-message noise.
    pub colliding_fills_refused: u64,
}

impl Account {
    pub fn new(
        multiplier: f64,
        venue: &str,
        multipliers: Option<IndexMap<String, f64>>,
        balance_mode: BalanceMode,
    ) -> Self {
        Account {
            venue: venue.to_string(),
            mult: std::sync::Arc::new(multipliers.unwrap_or_default()),
            mult_default: multiplier,
            default_margin_modes: std::sync::Arc::new(IndexMap::new()),
            balance_mode,
            positions: IndexMap::new(),
            realized_pnl: 0.0,
            closed_pnls: Vec::new(),
            balance: 0.0,
            balances_by_asset: IndexMap::new(),
            realized_pnl_at_balance_sync: None,
            marks: IndexMap::new(),
            mark_meta: IndexMap::new(),
            mark_staleness_ms: DEFAULT_MARK_STALENESS_MS,
            reconcile_staleness_ms: DEFAULT_RECONCILE_STALENESS_MS,
            funding_paid: 0.0,
            fees_paid: 0.0,
            fees_by_asset: IndexMap::new(),
            seen_fill_ids: HashMap::new(),
            duplicate_fills_refused: 0,
            colliding_fills_refused: 0,
        }
    }

    /// Pre-load the fill-dedup ledger from a durable source — a restored `EngineSnapshot`, or the
    /// replayed command journal. The cold-start half of the guard: without a seed a restart
    /// re-folds whatever venue history the first resync / reconnect snapshot hands it.
    ///
    /// ⚠ Seeding is the CALLER's job and no shipped binary does it yet — see the "cold start" note
    /// on `crates/vike-exec/CLAUDE.md`. Seeded entries carry [`PRINT_UNKNOWN`], so a refusal
    /// against one is a plain duplicate, never a collision.
    pub fn seed_seen_fill_ids<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.seen_fill_ids.extend(ids.into_iter().map(|id| (id, PRINT_UNKNOWN)));
    }

    /// Every `trade_id` in the fill-dedup ledger, read-only (snapshots, the reconcile `LocalView`,
    /// tests). An iterator, so the fingerprint stays private to this type.
    pub fn seen_fill_ids(&self) -> impl Iterator<Item = &str> {
        self.seen_fill_ids.keys().map(|s| s.as_str())
    }

    /// Whether `trade_id` has already been folded into this account. The membership question on its
    /// own, for a caller that has an id and no fill (the reconcile `diff` shape).
    pub fn has_folded_fill(&self, trade_id: &str) -> bool {
        self.seen_fill_ids.contains_key(trade_id)
    }

    /// Per-symbol contract multiplier; the legacy scalar default for unlisted symbols.
    pub fn multiplier_of(&self, symbol: &str) -> f64 {
        self.mult.get(symbol).copied().unwrap_or(self.mult_default)
    }

    /// The whole per-symbol multiplier grid, shared by pointer-copy (`Arc::clone`). Exposed so a
    /// read-model can answer `multiplier_of` for a symbol the account holds NO position in (the
    /// deribit options confirm ticket prices a never-traded symbol's notional). Immutable after
    /// construction, so the handle never observes a partial update.
    pub fn multiplier_grid(&self) -> std::sync::Arc<IndexMap<String, f64>> {
        std::sync::Arc::clone(&self.mult)
    }

    /// The fallback multiplier `multiplier_of` returns for any symbol absent from the grid.
    pub fn multiplier_default(&self) -> f64 {
        self.mult_default
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
    /// where it differs from [`MarginMode::Cross`], and `Cross` everywhere else.
    ///
    /// The `is_empty` short-circuit means that on every venue but hyperliquid the fold never
    /// hashes the symbol; it is reached only on open-from-flat anyway (see `Self::fold`).
    pub fn default_margin_mode_of(&self, symbol: &str) -> MarginMode {
        if self.default_margin_modes.is_empty() {
            return MarginMode::Cross;
        }
        self.default_margin_modes.get(symbol).copied().unwrap_or(MarginMode::Cross)
    }

    /// Fold one venue execution into the money: position, realized PnL, cash and per-asset fees.
    ///
    /// **IDEMPOTENT PER `trade_id`.** The dedup ledger is this type's own field, checked HERE before
    /// anything moves, so the guarantee holds for every caller. A repeat returns
    /// [`FillFold::Duplicate`] having mutated nothing but [`Self::duplicate_fills_refused`]; see
    /// the module doc for the key choice, the collision detector and why the set is unbounded.
    ///
    /// **REFUSED, never panicking.** Several venues re-deliver executions by design, and the one
    /// live caller (`ExecutionEngine::on_event`) runs on the fold thread the `p99 < 10µs` gate
    /// measures, where no invariant violation may take the process down (`refuse_nonfinite` is the
    /// precedent: count, log the fault, drop the event). Panicking on a double-counted fill is
    /// defensible for a library whose caller can catch it, wrong for a daemon holding live
    /// positions and resting orders.
    ///
    /// **Hot-path cost:** one `HashMap<String, u64>` lookup on the fill's `trade_id` (`&str`
    /// borrow, no allocation on the duplicate path), plus one `String` allocation for the inserted
    /// key on the accept path. No logging.
    pub fn apply_fill(&mut self, fill: &FillEvent) -> FillFold {
        assert_eq!(
            fill.venue, self.venue,
            "fill.venue={:?} routed to Account(venue={:?})",
            fill.venue, self.venue
        );
        // THE WELD. Consult the ledger before a single field moves, and record the id only on the
        // accept path so a refused fill never burns an id it did not fold. There is NO untagged
        // branch: `fill.trade_id` is a `TradeId`, which cannot be empty (#1341).
        {
            let print = fill_print(fill);
            // `get(&str)` then `insert(String, _)`, NOT `insert(to_string(), _)`: the latter
            // allocates before it knows the answer, and on bybit/binance-perp the DUPLICATE path is
            // roughly half of all fills.
            if let Some(&folded) = self.seen_fill_ids.get(fill.trade_id.as_str()) {
                if folded == print || folded == PRINT_UNKNOWN {
                    self.duplicate_fills_refused += 1;
                    return FillFold::Duplicate;
                }
                self.colliding_fills_refused += 1;
                // A FAULT TRANSITION, not per-message noise (re-deliveries took the silent branch
                // above): a REAL fill was just dropped, so it logs like
                // `ExecutionEngine::refuse_nonfinite` does.
                tracing::error!(
                    target: "vike_exec::account",
                    venue = %self.venue,
                    symbol = %fill.symbol,
                    trade_id = %fill.trade_id,
                    coid = %fill.client_order_id,
                    side = fill.side,
                    qty = fill.last_qty,
                    px = fill.last_px,
                    "REFUSED a fill whose trade_id was ALREADY FOLDED under a DIFFERENT \
                     (symbol, side, qty, px) — this is an id COLLISION, not a reconnect replay, so a \
                     GENUINE fill has been dropped and this account now understates the position. \
                     The producer of these ids is not unique per account (binance/aster `t` and okx \
                     `tradeId` are per-SYMBOL sequences; vike-paper's `paper-` counter is per-CLIENT). \
                     Reconcile this venue."
                );
                return FillFold::Collision;
            }
            self.seen_fill_ids.insert(fill.trade_id.to_string(), print);
        }
        // The key is built from the fill's ALREADY-interned `venue`/`symbol` and enum
        // `position_side`: three `Copy` moves, zero allocation.
        let key: PositionKey = (fill.venue, fill.symbol, fill.position_side);
        let mult = self.multiplier_of(&fill.symbol);
        self.fold(key, fill.side, fill.last_qty, fill.last_px, mult);
        // Net the trade commission into cash, like apply_liquidation nets its fee. Signed:
        // commission > 0 is a charge (lowers balance), < 0 is a maker rebate (raises it).
        self.balance -= fill.commission;
        self.fees_paid += fill.commission;
        // ADDITIVE per-asset fee attribution (parity law A1): the SAME signed commission into
        // `fees_by_asset` when the venue surfaced the fee currency (empty = no-op). SEPARATE from
        // `balances_by_asset`, so `value_in` never reads a fee tally as a negative holding. The
        // fee asset is often the base asset or a discount token (BNB), so per-asset fees
        // legitimately span assets the quote-denominated `fees_paid` scalar collapses together.
        if !fill.commission_asset.is_empty() {
            // `commission_asset` is a `Ustr` too: no `to_string()` on the fill path.
            *self.fees_by_asset.entry(fill.commission_asset).or_insert(0.0) += fill.commission;
        }
        FillFold::Applied
    }

    /// Sole writer of position + realized PnL. `apply_fill` and `apply_liquidation` both call
    /// ONLY this for position/realized mutation (one compute_fill block, no drift).
    fn fold(&mut self, key: PositionKey, side_sign: i32, qty: f64, px: f64, mult: f64) {
        // Prior entry: `margin_mode`/`isolated_margin` are carried forward verbatim, so an
        // isolated position stays isolated across fills.
        //
        // ⚠ OPEN-FROM-FLAT takes the ASSET's ruling mode, not `PositionEntry::default()`: a bare
        // `unwrap_or_default()` books EVERY new position `Cross` (the type default), wrong at birth
        // on hyperliquid's isolated-only assets, and nothing corrects it — HL heals size through
        // `recon::diff`/`resolve`, which never reads `PositionStatusReport::margin_mode` and
        // synthesizes `Fill`s that re-enter HERE, and `ExecutionEngine::apply_snapshot`'s
        // `ReconcileSnapshot::position_margin` overwrite has no hyperliquid producer. And it is
        // READ: the admitting gate's margin fold and `check_margin_call`'s pool partition key on
        // `is_cross()`, and `vike_core`'s liquidation-price badge routes on it.
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
        let out = compute_fill(prior.size, prior.avg_px, side_sign, qty, px, mult);
        // rebind, not in-place mutate
        self.positions.insert(
            key,
            PositionEntry {
                size: out.new_size,
                avg_px: out.new_avg_px,
                margin_mode: prior.margin_mode,
                isolated_margin: prior.isolated_margin,
            },
        );
        if out.closing_qty > 0.0 {
            // a reduce / close / flip realized PnL on the closed portion
            self.realized_pnl += out.realized_pnl;
            self.closed_pnls.push(out.realized_pnl);
        }
    }

    /// Fold a periodic funding cashflow into the cash balance (signed: + received / - paid).
    pub fn apply_funding(&mut self, ev: &FundingEvent) {
        self.balance += ev.amount;
        self.funding_paid += ev.amount;
    }

    /// Set balance AUTHORITATIVELY from a venue AccountState event.
    /// Quote-asset selection: (1) the pair whose asset == `quote_asset`; (2) a single balance
    /// used unconditionally; (3) the sum of all qty values (last resort). Sets `balance`
    /// absolutely (not +=) and flips `balance_mode` to Authoritative.
    pub fn apply_account_state(&mut self, ev: &AccountState, quote_asset: &str) {
        let balances = &ev.balances;
        if balances.is_empty() {
            return;
        }
        // Cash-reconcile baseline: a non-empty frame ALWAYS sets `balance` authoritatively below,
        // so snapshot the realized-PnL baseline here (`realized_pnl` does not change in this fn).
        // Each genuine venue balance push IS a reseed, so "realized since sync" resets to zero.
        // Inert unless `VIKE_RECONCILE_BALANCE` is on.
        self.realized_pnl_at_balance_sync = Some(self.realized_pnl);
        // Per-asset ledger first (additive, upsert): partial venue frames (e.g. Binance
        // outboundAccountPosition carries only changed assets) merge instead of wiping.
        // The scalar collapse below stays byte-identical to the oracle.
        for (asset, qty) in balances {
            self.balances_by_asset.insert(asset.clone(), *qty);
        }
        for (asset, qty) in balances {
            if asset == quote_asset {
                self.balance = *qty;
                self.balance_mode = BalanceMode::Authoritative;
                return;
            }
        }
        if balances.len() == 1 {
            self.balance = balances[0].1;
            self.balance_mode = BalanceMode::Authoritative;
            return;
        }
        self.balance = vike_model::py_sum(balances.iter().map(|(_a, q)| *q)); // builtin sum()
        self.balance_mode = BalanceMode::Authoritative;
    }

    /// Forced close: realize PnL at the liq price, close min(ev.qty, held), deduct the liq fee.
    /// `ev.qty` falsy (0.0) closes the WHOLE held size; qty is clamped so an over-reported frame
    /// can never flip the position. Idempotent on a flat position. Routes through `fold`.
    pub fn apply_liquidation(&mut self, ev: &PositionLiquidated) {
        // Same allocation-free key construction as `apply_fill` — every component is already
        // interned / a `Copy` enum on the event.
        let key: PositionKey = (ev.venue, ev.symbol, ev.position_side);
        let pos = match self.positions.get(&key) {
            Some(p) if p.size != 0.0 => *p,
            _ => return, // true no-op: nothing to liquidate
        };
        let close_side = vike_model::closing_side(pos.size); // close on the opposite side
        // `ev.qty == 0.0` (NaN is not produced here) means the whole size.
        let close_qty =
            if ev.qty != 0.0 { ev.qty.abs().min(pos.size.abs()) } else { pos.size.abs() };
        let mult = self.multiplier_of(&ev.symbol);
        self.fold(key, close_side, close_qty, ev.liq_price, mult);
        self.balance -= ev.fee;
    }

    /// Full-state DTO for the journal `Snap` record. Maps become ordered Vecs.
    pub fn snapshot(&self) -> crate::engine_snapshot::AccountSnapshot {
        crate::engine_snapshot::AccountSnapshot {
            venue: self.venue.clone(),
            mult: self.mult.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            mult_default: self.mult_default,
            balance_mode: self.balance_mode,
            positions: self.positions.iter().map(|(k, v)| (*k, *v)).collect(),
            realized_pnl: self.realized_pnl,
            closed_pnls: self.closed_pnls.clone(),
            balance: self.balance,
            balances_by_asset: self
                .balances_by_asset
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            marks: self.marks.iter().map(|(k, v)| (*k, *v)).collect(),
            funding_paid: self.funding_paid,
            fees_paid: self.fees_paid,
            fees_by_asset: self.fees_by_asset.iter().map(|(k, v)| (*k, *v)).collect(),
        }
    }

    /// Inverse of [`Account::snapshot`] — rebuilds the exact Account (insertion orders preserved).
    pub fn restore(snap: &crate::engine_snapshot::AccountSnapshot) -> Account {
        Account {
            venue: snap.venue.clone(),
            mult: std::sync::Arc::new(snap.mult.iter().cloned().collect()),
            mult_default: snap.mult_default,
            // NOT snapshotted: this grid is MOUNT CONFIGURATION from a live venue `meta` fetch, not
            // folded state, and a snapshot copy would pin a venue's asset facts to snapshot time.
            // Restored POSITIONS keep their own (snapshotted) `margin_mode`; only an open-from-flat
            // reads this. ⚠ A live restore path must re-apply
            // [`Account::with_default_margin_modes`] after `restore`, as it re-runs the mount.
            default_margin_modes: std::sync::Arc::new(IndexMap::new()),
            balance_mode: snap.balance_mode,
            positions: snap.positions.iter().cloned().collect(),
            realized_pnl: snap.realized_pnl,
            closed_pnls: snap.closed_pnls.clone(),
            balance: snap.balance,
            balances_by_asset: snap.balances_by_asset.iter().cloned().collect(),
            // NOT snapshotted (see the field doc): a restored account re-adopts venue cash on its
            // first reconcile pass instead of diffing against a baseline it can't reconstruct.
            realized_pnl_at_balance_sync: None,
            marks: snap.marks.iter().cloned().collect(),
            // Provenance is NOT snapshotted (see the field doc): a restored slot is unowned, so
            // no stale owner freezes valuation after replay.
            mark_meta: IndexMap::new(),
            mark_staleness_ms: DEFAULT_MARK_STALENESS_MS,
            reconcile_staleness_ms: DEFAULT_RECONCILE_STALENESS_MS,
            funding_paid: snap.funding_paid,
            fees_paid: snap.fees_paid,
            fees_by_asset: snap.fees_by_asset.iter().cloned().collect(),
            // NOT in `AccountSnapshot`: the ledger's durable home is the top-level
            // `EngineSnapshot::seen_trade_ids`, and `ExecutionEngine::from_snapshot` seeds it via
            // [`Account::seed_seen_fill_ids`] right after this call. A BARE `Account::restore`
            // starts empty, like `Account::new` (a bare `Account` folds no live stream).
            seen_fill_ids: HashMap::new(),
            // LIVE HEALTH SIGNALS, not folded state (the `mark_meta` / `dropped_nonfinite`
            // doctrine): a prior session's tally would misreport this session's.
            duplicate_fills_refused: 0,
            colliding_fills_refused: 0,
        }
    }
}

#[cfg(test)]
mod account_tests;

#[cfg(test)]
mod mark_law_tests;

#[cfg(test)]
mod fill_dedup_tests;
