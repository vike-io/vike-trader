//! FillEvent-derived account read-model: positions + realized PnL from the fill stream.
//! Exact port of `exec/accounting.py::Account` (origin/main, ledger unification S1).
//!
//! - per-venue: `apply_fill` asserts `fill.venue == self.venue`.
//! - per-symbol multiplier: `multiplier_of(symbol)` with the legacy scalar default.
//! - explicit `balance_mode` (Delta | Authoritative); `equity_all(seed)` is mode-aware;
//!   `apply_account_state` flips the mode after each authoritative balance assignment.
//! - `fold` is the SOLE writer of position + realized PnL (called by both `apply_fill` and
//!   `apply_liquidation` — one compute_fill block, no drift).
//! - `apply_fill` is IDEMPOTENT PER `trade_id` — the dedup ledger lives on this type, welded to
//!   the mutation it protects (see "Fill dedup" below).
//! - NO equity cache anywhere (catastrophic-cancellation risk) — every read is a sparse
//!   full recompute.
//! - Maps are `IndexMap` (insertion-ordered like Python dicts — that is where the choice came
//!   from) so f64 summation order in `equity_all` is FIXED. **NEVER a HashMap** — the insertion
//!   order IS the f64 fold order the parity gate pins, bit-for-bit, against the frozen
//!   `fixtures/r0/account_scenarios.json` bytes. Nothing re-derives those bytes from Python any
//!   more; they are the oracle themselves, so a HashMap here would not "diverge from Python", it
//!   would change this code's arithmetic and redden the gate.
//!
//! ## Fill dedup: the check lives ON the aggregate that owns the money
//!
//! [`Account::apply_fill`] moves money — `balance -= commission`, `fees_paid +=`, the position
//! fold, `realized_pnl +=`. It is therefore not safe to call twice for one venue execution, and
//! until now the guard against that lived in a DIFFERENT type: `ExecutionEngine`'s own
//! `seen_trade_ids` set, checked before it called in here. That arrangement is only as good as
//! every caller's memory, and the number of paths that re-deliver an already-folded fill has been
//! GROWING — the `exec_actor::run_loop` gap sentinel's venue-history resync, the
//! `user_data::run_resync_supervisor` replay, hyperliquid's `userFills` `isSnapshot` frame (re-sent
//! on EVERY reconnect, deliberately, because the fills that executed while the socket was down
//! exist only there), bybit's `execution.fast`/`execution` twin, binance perp's early+authoritative
//! pair, alpaca's `since_id` boundary re-delivery. Every one of those is a producer that KNOWS it
//! duplicates and relies on a dedup it cannot see.
//!
//! So the ledger moved here. [`Account`] owns the set, [`Account::apply_fill`] consults it before
//! anything moves, and it reports the outcome as a [`FillFold`] instead of returning `()`. A caller
//! that forgets to check the return still cannot double-count — nothing moved — it only loses the
//! ability to react. `ExecutionEngine`'s field is GONE; its `seen_trade_ids()` accessor,
//! `local_view()`, `EngineSnapshot` wire and `seed_seen_trade_ids` all read/write THIS set, so
//! there is exactly one authority and no second set that can disagree with it.
//!
//! **Keyed on the bare `trade_id` — the SAME key the engine-side guard used — and the narrowness is
//! DETECTED rather than assumed away.** Nautilus keys `(AccountId, InstrumentId, TradeId)`. The
//! first component is redundant here (an `Account` IS per-venue-per-account — `apply_fill` asserts
//! `fill.venue == self.venue`), but the instrument component is NOT redundant, and pretending
//! otherwise would be wrong: binance/aster `t` and okx `tradeId` are per-SYMBOL sequences at the
//! venue, so on a multi-symbol engine (`ExecutionEngine::extra_symbols`, which combo lowering
//! populates) two different executions can carry the same id string. `vike_paper`'s
//! `paper-{n}` and `vike_exec::testing::TestExecutionClient`'s `simt{n}` are per-CLIENT counters,
//! so `MultiPaperExecutionClient`'s N per-symbol books mint colliding ids by construction.
//!
//! The key is nevertheless the bare id, because WIDENING it is the move that loses money. The
//! reconcile lane compares venue-reported ids against this set as BARE STRINGS
//! (`vike_exec::recon::diff`'s `local.seen_trade_ids.contains(f.trade_id.as_str())`), and a
//! `FillReport`'s symbol is the venue's own spelling of the instrument, not necessarily the unified
//! symbol the fold keyed on. A composite key would therefore stop matching there, `diff` would call
//! an already-folded fill a `MissingFill`, and `resolve` would synthesize a SECOND fold of it —
//! the exact double-count this whole change exists to prevent, reintroduced through the other door.
//! Widening also moves `EngineSnapshot::seen_trade_ids` off `Vec<String>`, changing the journal
//! `state_hash` fence, which is the deferred schema-versioning question (`docs/decisions/`).
//!
//! So instead of choosing between two silent failure modes, the ledger stores a FINGERPRINT
//! alongside each id — an FNV-1a64 over `(symbol, side, last_qty, last_px)`, see [`fill_print`] —
//! and a refusal compares it. Same id AND same fingerprint is a re-delivery: refused, counted into
//! [`Account::duplicate_fills_refused`], silent (it is routine — see that field). Same id and a
//! DIFFERENT fingerprint is an id COLLISION: a genuine, unfolded fill about to be dropped, which is
//! never routine and never benign, so it is counted into
//! [`Account::colliding_fills_refused`] and logged at ERROR naming both fills. The fill is still
//! refused — folding on a fingerprint mismatch would make the guard depend on venue price/qty
//! rounding being stable, and a re-delivery whose px re-encoded in the last ulp would then double
//! count. That trade is deliberate: this key errs toward DROPPING a genuine fill, and the
//! fingerprint's job is to guarantee that when it does, an operator can see it and reconcile,
//! instead of the loss being indistinguishable from a reconnect.
//!
//! A restored ledger entry (see [`Account::seed_seen_fill_ids`]) carries [`PRINT_UNKNOWN`] and is
//! never reported as a collision — a prior session's fingerprints are not on the journal wire, so
//! the honest answer for those ids is "cannot tell", not "collision".
//!
//! **An EMPTY `trade_id` is unrepresentable**, so there is no undedupable-fill case to account for:
//! `FillEvent::trade_id` is a `TradeId` whose constructor refuses the empty string (#1341). Before
//! that type existed this was the one genuine hole — an empty id skipped the guard entirely and every
//! replay re-booked the money — and it was reached by five venue mappers through
//! `unwrap_or_default()`. The counter that used to make the hole visible is gone with the hole, not
//! kept as a field that can only read zero.
//!
//! **The set is UNBOUNDED on purpose.** It grows one interned-length `String` per distinct fill for
//! the process lifetime, which is a leak measured in tens of bytes per fill and bounded in practice
//! by the session. Every capped alternative (Hummingbot's 2000, an LRU) re-admits a duplicate the
//! moment a replay window reaches back further than the cap, and the widest replay window in this
//! workspace is not a fill COUNT at all — `VIKE_RECONCILE_LOOKBACK_MS` is one hour by default and
//! the venue history endpoints are row-count-bounded with no start time. Nobody has measured this
//! platform's peak sustained fill rate, so no cap can be justified as safely above that window
//! today. Bounding it errs toward silently re-admitting an old duplicate; not bounding it errs
//! toward memory. See `crates/vike-exec/CLAUDE.md`.
//!
//! ## Key types: interned, not owned (perf audit 2026-07-28, finding #3)
//!
//! [`PositionKey`] and the mark-slot key are `(Ustr, Ustr, …)`, not `(String, String, …)`.
//! `FillEvent::venue`/`symbol` are ALREADY [`Ustr`] and `position_side` is already the
//! [`PositionSide`] enum (see `vike_model::events`' interning contract), so the fill fold used to
//! `to_string()` three times PER FILL — allocating fresh heap copies *from interned values* on the
//! live fold thread, then freeing them again when the map entry was replaced. Keying on the
//! interned/`Copy` types makes `apply_fill`'s key construction allocation-free, and the same for
//! `apply_liquidation`, the mark slot (which allocated FOUR strings per write: two for the key,
//! two more for its `clone()` into the second map) and every `mark_of`/`unrealized_pnl` read.
//!
//! WIRE IS UNCHANGED. `Ustr` is serde-transparent (serializes as its `str`), and `PositionSide`
//! serializes to the same `"BOTH"/"LONG"/"SHORT"` strings via `rename_all = "UPPERCASE"`, so
//! [`crate::engine_snapshot::AccountSnapshot`] — and therefore the `state_hash` determinism
//! fence over its canonical JSON — is byte-identical (pinned by
//! `engine_snapshot::tests::account_snapshot_key_wire_is_byte_identical`).
//!
//! The interning-soundness contract still holds: venue is ~10 values ever and symbol is bounded by
//! the instruments a session touches (`vike_model::events`' module doc is the authority). The
//! `&str`-taking read accessors ([`Account::mark_of`], [`Account::unrealized_pnl`], …) intern their
//! arguments at the boundary — a hash + table probe, no allocation, and no `free` afterwards.
//!
//! ONE narrowing worth naming: the third key element is now the closed-set [`PositionSide`]
//! instead of a free-form `String`, so a venue label outside `{BOTH,LONG,SHORT}` folds to `Both`
//! rather than minting a fourth, permanently-orphaned key. Every venue already normalizes to those
//! three labels before `ReconcileSnapshot::position_sides` (okx maps `posSide` long/short→LONG/
//! SHORT, bybit maps `positionIdx` 1/2→LONG/SHORT, binance/aster pass their own uppercase labels),
//! and the fill lane never had a string there at all, so no live producer changes behavior.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ustr::Ustr;
use vike_model::MarginMode;
use vike_model::compute_fill;
use vike_model::events::{AccountState, FillEvent, FundingEvent, PositionLiquidated, PositionSide};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BalanceMode {
    Delta,
    Authoritative,
}

/// A position ledger entry: `{"size", "avg_px"}` in Python, plus the additive margin-mode
/// carrier (`feat/margin-mode-field`).
///
/// `margin_mode`/`isolated_margin` are INERT carriers — no fold, margin, or liquidation math
/// reads them yet (that is the scope-parameterized-law PR). They exist so a position CAN be
/// isolated. Both are `#[serde(skip_serializing_if)]` on their default (`Cross` / `None`) so a
/// cross position serializes to EXACTLY `{"size":…,"avg_px":…}` — keeping every existing
/// snapshot/journal, and the `AccountSnapshot` `state_hash` determinism fence, byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct PositionEntry {
    pub size: f64,
    pub avg_px: f64,
    /// Per-position margin mode (`Cross` default = whole-account collateral, today's behavior).
    /// Carried forward across fills by [`Account::fold`]; absent from a cross position's JSON.
    #[serde(default, skip_serializing_if = "MarginMode::is_cross")]
    pub margin_mode: MarginMode,
    /// Allocated isolated-margin wallet for this position (account currency). `None` for cross
    /// (collateral is the shared account equity); a value only carries meaning under
    /// `MarginMode::Isolated`. Inert — no math reads it yet; absent from a cross position's JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolated_margin: Option<f64>,
}

/// Position key: (venue, symbol, position_side) — position_side `Both` for one-way/spot;
/// the tuple reserves the hedge-mode dimension for perps.
///
/// All three components are `Copy` (interned `Ustr` / closed-set enum), so building one on the
/// fill-fold path allocates nothing — see the module doc's "Key types" section. Serde-wise it is
/// the SAME three JSON strings the `(String, String, String)` key produced.
pub type PositionKey = (Ustr, Ustr, PositionSide);

/// Mark-slot key: (venue, symbol). Same interned/`Copy` rationale as [`PositionKey`] — and it is
/// the hotter of the two, since a mark write happens per venue mark tick / bar close / trade tick
/// rather than per fill.
pub type MarkKey = (Ustr, Ustr);

/// WHICH PRICE CONCEPT a write into the account mark slot carries.
///
/// `Account.marks` is a SINGLE UNTAGGED SCALAR per symbol, and it is read by the pre-trade risk
/// gate (`RiskContext.mark_price`, `margin_in_use_by`, `equity_all`), by the margin-call law
/// (`check_margin_call` — which positions liquidate and by how much), by the trailing-stop seed,
/// and by `LiveBroker.price` (what a mounted strategy calls "the price"). Several feeds write it
/// at wildly different cadences, so without a rule it alternates between concepts at whatever
/// rate they happen to interleave — a klines lane at one bar/second stomping a 1s venue mark is
/// the exact bug this enum exists to make impossible.
///
/// **THE LAW** (enforced inside [`Account::set_mark_from`], nowhere else):
/// while a symbol's slot holds a GENUINE VENUE MARK ([`MarkSource::is_venue_mark`]) observed
/// within its source's OWNERSHIP WINDOW, that mark OWNS the slot and a [`MarkSource::BarClose`]
/// or [`MarkSource::TradeTick`] write for that symbol is DROPPED. Otherwise last-write-wins,
/// exactly as before this rule existed.
///
/// **PER-SOURCE WINDOWS.** The two venue-mark variants are refreshed at very different cadences,
/// so they own for different lengths of time:
/// - [`MarkSource::VenueMark`] — a streamed mark (~1s cadence) — uses `mark_staleness_ms`
///   (default 10s, ~10 missed ticks).
/// - [`MarkSource::ReconcileMark`] — the venue's own mark sampled at the RECONCILE cadence
///   (default 60s) — uses the longer `reconcile_staleness_ms` (default 150s). Sizing it to cover
///   the reconcile interval is the whole point: a shared 10s window would EXPIRE between passes,
///   so a reconciled stream-less venue's slot would alternate reconcile-mark → closes → next
///   pass every minute. The wider window keeps ownership CONTINUOUS across passes; closes only
///   take over if reconcile stops entirely (see the degradation consequence below).
///
/// Consequences worth stating plainly:
/// - A venue with NO mark stream and NO reconcile mark never has an owned slot, so every close
///   and tick writes byte-identically to pre-law behavior.
/// - A mark stream that DIES hands the slot back after its window (`mark_staleness_ms` for a
///   streamed mark, `reconcile_staleness_ms` for a reconcile mark); closes resume. Valuation
///   degrades to the previous rung instead of freezing at a stale mark.
/// - A venue mark never blocks ANOTHER venue mark — a fresher mark always replaces an older one,
///   and the window is keyed off the CURRENT OWNER's source, so a reconcile mark replaced by a
///   streamed mark immediately reverts to the short streamed window.
///
/// Nothing is lost to the resolver: the dropped concepts still fill their own `PriceBoard` slots
/// (`set_bar_close` / `set_last_trade`), which is where the per-source valuation chain lives.
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

/// Default ownership window for a RECONCILE mark ([`MarkSource::ReconcileMark`]). Sized to 2.5×
/// the default `VIKE_RECONCILE_INTERVAL_MS` (60s) so a reconciled stream-less venue's slot stays
/// owned by the reconcile mark CONTINUOUSLY between passes rather than expiring and alternating
/// with candle closes each minute. MUST exceed the deployment's reconcile interval; closes only
/// reclaim the slot if reconcile stops for longer than this. Mirrored by
/// `vike_core::CoreConfig::reconcile_mark_staleness_ms`, which overwrites it on the live path.
pub const DEFAULT_RECONCILE_STALENESS_MS: i64 = 150_000;

/// The fingerprint stored for a ledger entry whose identity is UNKNOWN — a `trade_id` seeded from a
/// durable source ([`Account::seed_seen_fill_ids`]) rather than observed by this session's fold.
/// Compares equal to nothing, so such an entry is refused as a plain duplicate and never reported
/// as a collision: prior-session fingerprints are not on the journal wire, and inventing a verdict
/// for them would put a false ERROR in front of every restart.
pub const PRINT_UNKNOWN: u64 = 0;

/// FNV-1a64 over the four fields that IDENTIFY a fill within one account: `(symbol, side, last_qty,
/// last_px)`. Two deliveries of ONE venue execution agree on all four; two DIFFERENT executions that
/// happen to share a `trade_id` string (per-symbol venue sequences, per-client paper counters) almost
/// never do. Deliberately excludes `commission`/`ts`/`client_order_id`/`mark_price`: a venue's
/// re-delivery can legitimately restate a fee or a timestamp (binance perp's early TRADE_LITE fill
/// and its authoritative twin differ in exactly that way — the early one carries no commission), and
/// treating that as a collision would put an ERROR in front of a designed-in duplicate.
///
/// f64s are hashed by their `to_bits()` — this is an identity test, not arithmetic, so the raw bit
/// pattern is the right comparison and NaN's non-reflexivity never enters (a non-finite fill is
/// refused by the engine's `numbers_finite` guard before it reaches here).
///
/// Allocation-free and branch-light: `symbol` is an interned `Ustr` read as bytes, the rest are
/// 8-byte little-endian words. Never returns [`PRINT_UNKNOWN`] (a computed `0` maps to `1`), so the
/// sentinel cannot be minted by accident.
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
/// nothing), and ~40 existing call sites treat `apply_fill` as a statement. The point of the return
/// is that a caller CAN observe the refusal, not that it must.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillFold {
    /// The fill was folded: position, realized PnL, cash and fees all moved.
    Applied,
    /// REFUSED — this `trade_id` had already been folded into this account, and the fill's
    /// [`fill_print`] MATCHES the one folded under it: an ordinary re-delivery. Nothing moved, and
    /// [`Account::duplicate_fills_refused`] advanced.
    Duplicate,
    /// REFUSED — this `trade_id` had already been folded, but under a DIFFERENT
    /// `(symbol, side, qty, px)`. That is an id COLLISION, not a re-delivery: a genuine fill has
    /// been dropped. Nothing moved, [`Account::colliding_fills_refused`] advanced, and the fold
    /// logged it at ERROR. The caller cannot recover it — reconcile can.
    Collision,
}

impl FillFold {
    /// Whether the fill moved money. The one question every caller actually asks, so it does not
    /// have to enumerate the two refusal reasons.
    pub fn applied(self) -> bool {
        matches!(self, FillFold::Applied)
    }
}

/// Folds a stream of `FillEvent`s into positions and realized PnL.
#[derive(Debug, Clone)]
pub struct Account {
    pub venue: String,
    /// per-symbol contract multipliers; `multiplier_of` falls back to the legacy scalar.
    /// IMMUTABLE after construction (written only by `new`/`restore`), so it is held behind an
    /// `Arc` purely so read-models can share the grid by pointer-copy instead of deep-cloning it
    /// — see [`Account::multiplier_grid`] and `vike_core::VenueBlock::multipliers`.
    mult: std::sync::Arc<IndexMap<String, f64>>,
    mult_default: f64,
    /// Per-symbol RULING margin mode for a position this account OPENS FROM FLAT — the venue's
    /// per-ASSET truth, resolved once at the mount and handed in by the composition root.
    ///
    /// ## Why a grid and not a scalar
    /// The mode a fresh position takes is a property of the ASSET, not of the venue.
    /// `VenueCaps::default_margin_mode` is per-VENUE and says `Cross` for hyperliquid, but that
    /// venue publishes `onlyIsolated` per asset and on such an asset the mode that RULES is
    /// `Isolated` (#1087; the derivation is `vike_hyperliquid::symbology::InstrumentRef::
    /// effective_margin_mode`, and the venue itself agrees — its `recon_client`'s
    /// `parse_positions` maps `leverage.type == "isolated"` → `Isolated`). One asset of a venue
    /// can therefore disagree with the next, which a scalar cannot express.
    ///
    /// ## SPARSE ON PURPOSE — absent means `Cross`
    /// Only symbols whose ruling mode DIFFERS from [`MarginMode::Cross`] get a row, exactly like
    /// [`Self::mult`] only carries a symbol whose multiplier differs from 1.0. So the map is EMPTY
    /// for 13 of the 14 roster venues and for every ordinary hyperliquid asset, and
    /// [`Self::default_margin_mode_of`] short-circuits on empty — the fold does not even hash a
    /// key. Nothing about the steady state changes either way: the lookup happens ONLY when
    /// [`Self::fold`] finds no prior entry (open-from-flat), never on a fill into a position that
    /// already exists.
    ///
    /// IMMUTABLE after construction (written only by [`Account::with_default_margin_modes`]) and
    /// behind an `Arc` for the same reason [`Self::mult`] is: `Account` is `Clone`, and a shared
    /// pointer-copy beats a deep clone of a map nothing ever mutates.
    default_margin_modes: std::sync::Arc<IndexMap<String, MarginMode>>,
    pub balance_mode: BalanceMode,
    pub positions: IndexMap<PositionKey, PositionEntry>,
    pub realized_pnl: f64,
    /// gross price PnL per closing portion, in order — a list of PnL floats, NOT `Trade`
    /// round-trip objects (the name avoids that collision; FIX/accounting call these closed PnLs).
    pub closed_pnls: Vec<f64>,
    pub balance: f64,
    /// ADDITIVE per-asset ledger (accounting-upgrade design, parity law A1): the raw
    /// `(asset, qty)` pairs venue AccountState events carry, upserted in venue-given order.
    /// The oracle collapses these into the `balance` scalar and forgets them; this map is
    /// the Rust-native memory of what was collapsed. Never read by any fold formula.
    pub balances_by_asset: IndexMap<String, f64>,
    /// `realized_pnl` captured at the last AUTHORITATIVE balance sync (the value of
    /// [`Self::realized_pnl`] at the instant [`Self::apply_account_state`] last set `balance`).
    /// `None` = never synced. The first-class cash reconcile (Feature 2, `VIKE_RECONCILE_BALANCE`)
    /// reads it to correct the money-drift CONFOUND: realized PnL never enters `balance` in
    /// Authoritative mode (see [`Self::equity_all`]), so between syncs local `balance` legitimately
    /// diverges from venue cash by Σ realized-since-sync — the reconcile diff compares venue cash
    /// against `balance + (realized_pnl - realized_pnl_at_balance_sync)`, not raw `balance`, so a
    /// realized trade never false-flags. DELIBERATELY NOT in `AccountSnapshot` (like `mark_meta`):
    /// a restore starts `None`, so the first post-restore reconcile pass re-adopts venue cash
    /// rather than diffing against a stale baseline — no spurious startup drift. Never read by any
    /// fold formula; inert unless the opt-in cash-reconcile knob is on.
    pub realized_pnl_at_balance_sync: Option<f64>,
    /// PRIVATE ON PURPOSE — the only way in is [`Account::set_mark_from`], which carries the
    /// precedence law (see [`MarkSource`]). A `pub` field here is how the law got bypassed
    /// three times; making the omission impossible is the whole point (same doctrine as the
    /// merged `SeqGate`). Read it back with [`Account::mark_of`] / [`Account::marks_iter`].
    marks: IndexMap<MarkKey, f64>,
    /// Per-symbol provenance of the CURRENT [`Self::marks`] entry: which concept wrote it and
    /// the CORE-clock ms at which it was observed. Deliberately not part of `AccountSnapshot`:
    /// it is derived from non-journaled market data exactly like `marks` itself, so a restore
    /// starts with an unowned slot and the next writer of any source wins — the documented
    /// (and safe) degradation, identical to a venue that never streamed a mark.
    mark_meta: IndexMap<MarkKey, (MarkSource, i64)>,
    /// How long a STREAMED venue mark ([`MarkSource::VenueMark`]) keeps ownership of a symbol's
    /// mark slot, in CORE-clock ms. Set by the runtime from `CoreConfig::mark_staleness_ms`; the
    /// default matches it so a standalone `Account` (backtest, tests, bridges) behaves the same.
    mark_staleness_ms: i64,
    /// The ownership window for a RECONCILE mark ([`MarkSource::ReconcileMark`]) — longer than
    /// `mark_staleness_ms` because reconcile samples the venue's mark at its own (default 60s)
    /// cadence, not a 1s stream. Set by the runtime from `CoreConfig::reconcile_mark_staleness_ms`.
    /// See [`MarkSource`]'s "PER-SOURCE WINDOWS" for why the two differ.
    reconcile_staleness_ms: i64,
    pub funding_paid: f64,
    /// cumulative signed commission (>0 net cost, <0 net rebate)
    pub fees_paid: f64,
    /// ADDITIVE per-asset fee attribution (parity law A1): the per-asset twin of `fees_paid`,
    /// keyed by the fill's fee currency, SAME sign (>0 paid, <0 rebate). Kept SEPARATE from
    /// `balances_by_asset` on purpose — that map is the authoritative holdings mirror written
    /// only by venue snapshots, so a fee never masquerades as a (negative) holding. Only
    /// populated when the venue surfaced `commission_asset`; never read by any fold formula.
    pub fees_by_asset: IndexMap<Ustr, f64>,
    /// THE fill-dedup ledger — every `trade_id` this account has already folded. PRIVATE ON PURPOSE
    /// (the `marks` doctrine): the only way a fill reaches the money is [`Account::apply_fill`],
    /// which consults this first, so no caller can bypass the check by forgetting it. Read it back
    /// with [`Account::seen_fill_ids`]; pre-load it with [`Account::seed_seen_fill_ids`].
    ///
    /// A `HashMap` rather than the `IndexMap`/`IndexSet` the rest of this type insists on: it feeds
    /// NO f64 fold, so its iteration order is not observable by the parity gate. The one place the
    /// order WOULD escape — `EngineSnapshot::seen_trade_ids` — sorts it into canonical form.
    /// `trade_id -> `[`fill_print`] of the fill folded under it ([`PRINT_UNKNOWN`] for a seeded
    /// entry).
    seen_fill_ids: HashMap<String, u64>,
    /// Count of fills REFUSED by [`Account::apply_fill`] as already-folded duplicates.
    ///
    /// ⚠ **A large value here is NORMAL on several venues and is not a fault signal by itself.**
    /// Duplicate delivery is designed-in: bybit emits every execution twice (`execution.fast` then
    /// the authoritative `execution`), binance perp emits an early fill and its authoritative twin
    /// sharing one `t`, hyperliquid re-sends its whole `userFills` snapshot on every reconnect,
    /// alpaca re-delivers across its `since_id` boundary. On those venues this counter tracks the
    /// fill count. Its job is to make the refusals COUNTABLE — the number that means something is a
    /// CHANGE in its rate, and the operator-facing statement it supports is "the money-side guard
    /// caught N re-deliveries", not "N faults occurred". Which is also why there is no per-duplicate
    /// log: it would be a per-message log on the hot fold, which this repo forbids.
    pub duplicate_fills_refused: u64,
    /// Count of fills refused under an ALREADY-FOLDED `trade_id` whose [`fill_print`] DISAGREED —
    /// i.e. `trade_id` collisions, each one a genuine fill this account dropped.
    ///
    /// ⚠ **Unlike [`Self::duplicate_fills_refused`], any nonzero value here is a fault** and one that
    /// UNDERSTATES the platform's position rather than overstating it. It means the dedup key is too
    /// narrow for whatever produced those ids — the known producers are binance/aster `t` and okx
    /// `tradeId` (per-SYMBOL venue sequences) reaching a multi-symbol engine, and `vike_paper`'s
    /// per-client `paper-{n}` counter under `MultiPaperExecutionClient`. The cure is at the PRODUCER
    /// (qualify the id) or at reconcile; this counter exists so the loss is visible at all, because a
    /// bare-keyed dedup that drops a real fill is otherwise indistinguishable from one that collapsed
    /// a reconnect replay. Each occurrence is also logged at ERROR — that IS a fault transition, not
    /// per-message noise, which is why this one logs and the routine counter above does not.
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
    /// replayed command journal. The cold-start half of the guard: the in-memory set is empty in a
    /// fresh process, so without a seed a restart re-folds whatever venue history the first resync /
    /// reconnect snapshot hands it.
    ///
    /// ⚠ Seeding is the CALLER's job and no shipped binary does it yet — see the "cold start" note
    /// on `crates/vike-exec/CLAUDE.md`. This method existing does not close that hole.
    /// Seeded entries carry [`PRINT_UNKNOWN`]: a prior session's fingerprints are not on the journal
    /// wire, so a refusal against one is reported as a plain duplicate, never as a collision.
    pub fn seed_seen_fill_ids<I: IntoIterator<Item = String>>(&mut self, ids: I) {
        self.seen_fill_ids.extend(ids.into_iter().map(|id| (id, PRINT_UNKNOWN)));
    }

    /// Every `trade_id` in the fill-dedup ledger, read-only (snapshots, the reconcile `LocalView`,
    /// tests). An iterator rather than a map/set reference so the fingerprint stays an implementation
    /// detail — nothing outside this type has any business reading it.
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

    /// The whole per-symbol multiplier grid, shared by pointer-copy (`Arc::clone`, no allocation
    /// and no deep clone). Exposed so a read-model can answer `multiplier_of` for a symbol the
    /// account holds NO position in — the deribit options confirm-ticket case, where the GUI must
    /// price the pre-submit notional of a symbol it has never traded. The grid is immutable after
    /// construction, so the shared handle can never observe a torn/partial update.
    pub fn multiplier_grid(&self) -> std::sync::Arc<IndexMap<String, f64>> {
        std::sync::Arc::clone(&self.mult)
    }

    /// The fallback multiplier `multiplier_of` returns for any symbol absent from the grid.
    pub fn multiplier_default(&self) -> f64 {
        self.mult_default
    }

    /// Install the per-symbol ruling-margin-mode grid (see [`Self::default_margin_modes`]). A
    /// CONSUMING builder rather than a fifth [`Account::new`] parameter: ~40 call sites construct
    /// an `Account` and only the venue composition root has a grid to give, so a parameter would
    /// churn every one of them into passing `None`.
    ///
    /// `None` (or an empty map) leaves the account byte-identical to one that never called this —
    /// which is every venue but hyperliquid, and every hyperliquid asset that is not isolated-only.
    /// **Only rows that differ from [`MarginMode::Cross`] belong here**; a `Cross` row is
    /// indistinguishable from absence and only costs the fold a hash.
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
    /// The `is_empty` short-circuit is the point: on every venue but hyperliquid the grid is empty,
    /// and returning before `IndexMap::get` means the fold never even hashes the symbol. It is
    /// reached only on open-from-flat regardless (see [`Self::fold`]), so this is a cheap guard on
    /// an already-cold path, not a hot-path optimization.
    pub fn default_margin_mode_of(&self, symbol: &str) -> MarginMode {
        if self.default_margin_modes.is_empty() {
            return MarginMode::Cross;
        }
        self.default_margin_modes.get(symbol).copied().unwrap_or(MarginMode::Cross)
    }

    /// Fold one venue execution into the money: position, realized PnL, cash and per-asset fees.
    ///
    /// **IDEMPOTENT PER `trade_id`.** The dedup ledger is this type's own field, checked HERE before
    /// anything moves, so the guarantee holds for every caller and does not depend on the caller
    /// remembering a guard that lives somewhere else. A repeat returns [`FillFold::Duplicate`] having
    /// mutated nothing but [`Self::duplicate_fills_refused`]; see the module doc for the key choice,
    /// the empty-`trade_id` residual and why the set is unbounded.
    ///
    /// **REFUSED, never panicking.** A duplicate is not a programmer error this type can assert on:
    /// four venues re-deliver executions by design, and `ExecutionEngine::on_event` — the one live
    /// caller — runs on the fold thread the `p99 < 10µs` gate measures, where the standing rule is
    /// that no invariant violation may take the process down (`refuse_nonfinite` is the precedent:
    /// count, log the fault, drop the event). Nautilus panics on a double-counted fill; that is a
    /// defensible choice for a library whose caller can catch it, and the wrong one for a daemon
    /// holding live positions and resting orders.
    ///
    /// **Hot-path cost:** one `HashMap<String, u64>` lookup keyed on the fill's existing `trade_id`
    /// (`&str` borrow — no allocation on the DUPLICATE path at all), plus, on the accept path only,
    /// one `String` allocation for the inserted key. That allocation is not new: it is the
    /// `fill.trade_id.to_string()` that `ExecutionEngine::on_event` performed at this same point
    /// before the ledger moved down here. Nothing else was added, and no logging.
    pub fn apply_fill(&mut self, fill: &FillEvent) -> FillFold {
        assert_eq!(
            fill.venue, self.venue,
            "fill.venue={:?} routed to Account(venue={:?})",
            fill.venue, self.venue
        );
        // THE WELD. Consult the ledger before a single field moves, and record the id only on the
        // accept path so a refused fill never burns an id it did not fold.
        //
        // ⚠ There is NO untagged branch, and its absence is a property of the type: `fill.trade_id`
        // is a `TradeId`, which cannot be empty (#1341). The engine-side guard this replaced was
        // gated on `is_empty`, so an empty id folded UNGUARDED on every replay path — and the
        // `untagged_fills_folded` counter that recorded the hazard is gone with the hazard, rather
        // than kept as a field that can only ever read zero.
        {
            let print = fill_print(fill);
            // `get(&str)` then `insert(String, _)`, NOT the one-call `insert(to_string(), _)` — the
            // latter allocates before it knows the answer, and on bybit/binance-perp the DUPLICATE
            // path is roughly half of all fills. Two probes on the accept path, zero allocations on
            // the refuse path.
            if let Some(&folded) = self.seen_fill_ids.get(fill.trade_id.as_str()) {
                if folded == print || folded == PRINT_UNKNOWN {
                    self.duplicate_fills_refused += 1;
                    return FillFold::Duplicate;
                }
                self.colliding_fills_refused += 1;
                // A FAULT TRANSITION, not per-message noise: a re-delivery (the routine case,
                // roughly half of all bybit fills) took the silent branch above. Reaching here means
                // the ledger's key is too narrow for this venue's ids and a REAL fill was just
                // dropped — nothing in normal operation produces it, so it logs like
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
        // Perf audit finding #3: the key is built from the fill's ALREADY-interned `venue`/`symbol`
        // and its already-enum `position_side` — three `Copy` moves, zero allocation. (This used to
        // be three `to_string()`s per fill, i.e. three mallocs + three frees on the live fold
        // thread, minted FROM interned values.)
        let key: PositionKey = (fill.venue, fill.symbol, fill.position_side);
        let mult = self.multiplier_of(&fill.symbol);
        self.fold(key, fill.side, fill.last_qty, fill.last_px, mult);
        // Net the trade commission into cash, like apply_liquidation nets its fee. Signed:
        // commission > 0 is a charge (lowers balance), < 0 is a maker rebate (raises it).
        self.balance -= fill.commission;
        self.fees_paid += fill.commission;
        // ADDITIVE per-asset fee attribution (parity law A1): when the venue surfaced the fee
        // currency, accrue the SAME signed commission into `fees_by_asset` — the per-asset twin
        // of the `fees_paid` scalar. This never reads or touches the scalar fold above, and is
        // deliberately SEPARATE from `balances_by_asset` (the authoritative snapshot-only
        // holdings mirror) so a fee tally is never read as a negative holding by `value_in`.
        // Empty asset = not surfaced → no-op, byte-identical to before. NOTE: the fee asset is
        // often the base asset or a discount token (BNB), so per-asset fees legitimately span
        // assets the quote-denominated `fees_paid` scalar collapses together.
        if !fill.commission_asset.is_empty() {
            // `commission_asset` is a `Ustr` too (currency codes are a small repeated set), so the
            // per-asset ledger keys on it directly — no `to_string()` on the fill path.
            *self.fees_by_asset.entry(fill.commission_asset).or_insert(0.0) += fill.commission;
        }
        FillFold::Applied
    }

    /// Sole writer of position + realized PnL. `apply_fill` and `apply_liquidation` both call
    /// ONLY this for position/realized mutation (one compute_fill block, no drift).
    fn fold(&mut self, key: PositionKey, side_sign: i32, qty: f64, px: f64, mult: f64) {
        // Prior entry. `margin_mode`/`isolated_margin` are carried forward verbatim so an isolated
        // position stays isolated across fills; a cross position rebinds to Cross/None every fold
        // — byte-identical to the pre-field behavior.
        //
        // ⚠ OPEN-FROM-FLAT takes the ASSET's ruling mode, not `PositionEntry::default()`. This used
        // to be a bare `unwrap_or_default()`, which books EVERY brand-new position `Cross` because
        // that is `MarginMode`'s type default — venue-blind and asset-blind. On hyperliquid's
        // isolated-only assets that is simply WRONG at birth, and nothing later corrects it: HL
        // reconciles through the `recon::diff`/`resolve` lane, which never reads
        // `PositionStatusReport::margin_mode` (no `Divergence` variant is margin-shaped) and heals
        // size by synthesizing `Fill`s — which re-enter HERE and inherit the same wrong prior. The
        // one path that DOES let venue truth win, `ExecutionEngine::apply_snapshot`'s
        // `ReconcileSnapshot::position_margin` overwrite, has no hyperliquid producer at all
        // (only binance/bybit/okx perp populate that vec). So the mistake is permanent for the
        // session, and it is READ: the admitting gate's margin fold and `check_margin_call`'s pool
        // partition both key on `is_cross()`, and `vike_core`'s liquidation-price badge routes on
        // the same field — an isolated position mis-booked cross is charged against shared equity
        // it does not consume and is shown a whole-account liquidation price.
        //
        // COST: `positions.get` is unchanged, so a fill into an EXISTING position — the steady
        // state, and the only shape the p99 gate measures at volume — does exactly what it did.
        // The grid read happens only on the miss, and short-circuits on an empty grid (every venue
        // but hyperliquid) before hashing anything.
        let prior = match self.positions.get(&key) {
            Some(p) => *p,
            None => PositionEntry {
                margin_mode: self.default_margin_mode_of(&key.1),
                ..Default::default()
            },
        };
        let out = compute_fill(prior.size, prior.avg_px, side_sign, qty, px, mult);
        // rebind, not in-place mutate (matches Python's dict rebind semantics)
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

    /// THE ONLY WRITER of the account mark slot. `src` names the price CONCEPT and `now_ms` is
    /// the CORE clock (never a venue event time — see below); the precedence law documented on
    /// [`MarkSource`] is applied here so no caller can forget it. Returns whether the write
    /// landed, so a caller that wants to observe the law can (nothing on the hot path does).
    ///
    /// ONE CLOCK ON PURPOSE. Ownership is aged against the core clock on BOTH sides — the stored
    /// entry is stamped with the `now_ms` of its own write, not with the venue's event time. Venue
    /// event times still ride into the `PriceBoard` (they are the right ts for a price), but they
    /// are NOT comparable across feeds: binance stamps `E`, okx a row `ts`, hyperliquid has no
    /// timestamp at all and uses local receive. Aging a venue clock against the core clock would
    /// let a venue lagging more than the owner's window (`mark_staleness_ms` /
    /// `reconcile_staleness_ms`) look permanently stale, silently handing the slot back to candle
    /// closes forever. Same-clock-both-sides removes that failure mode.
    pub fn set_mark_from(
        &mut self,
        venue: &str,
        symbol: &str,
        px: f64,
        src: MarkSource,
        now_ms: i64,
    ) -> bool {
        // HOSTILE-VENUE GUARD. Being THE only writer of this slot makes one check cover every
        // producer at once — a fill's `mark_price`, the market-data mark lane, bar closes, trade
        // ticks, reconcile marks — so no caller can forget it and no future caller can reintroduce
        // the hole. A non-finite mark is silently corrosive rather than loud: `unrealized_pnl`
        // multiplies it into equity, so ONE poisoned slot makes `equity_all` NaN, and the RiskGate's
        // margin lane then compares against NaN — where every comparison is FALSE, so the account
        // reads as unconstrained rather than as broken.
        //
        // ⚠ A `px > 0.0` test is NOT this check (several callers have one): it excludes NaN and
        // -inf, but `+inf > 0.0` is TRUE, and +inf is just as poisonous (`inf - avg_px == inf`,
        // and `inf * 0.0 == NaN`). Rejecting here returns `false` — the same "the write did not
        // land" answer the ownership law already gives, which every caller already tolerates.
        if !px.is_finite() {
            return false;
        }
        // Interned key (perf audit finding #3): a hash + table probe instead of two `to_string()`
        // allocations — and, because `MarkKey` is `Copy`, the second insert below no longer needs
        // the `key.clone()` that made this FOUR allocations per mark write.
        let key: MarkKey = (Ustr::from(venue), Ustr::from(symbol));
        if !src.is_venue_mark()
            && let Some((owner, at)) = self.mark_meta.get(&key)
        {
            // The ownership window is keyed off the CURRENT OWNER's source: a streamed mark
            // (~1s cadence) holds for `mark_staleness_ms`, a reconcile mark (sampled at the
            // ~60s reconcile cadence) for the longer `reconcile_staleness_ms`, so its slot
            // does not expire and alternate between passes. See [`MarkSource`].
            let window = self.ownership_window(*owner);
            if owner.is_venue_mark() && now_ms.saturating_sub(*at) <= window {
                return false;
            }
        }
        self.marks.insert(key, px);
        self.mark_meta.insert(key, (src, now_ms));
        true
    }

    /// The ownership window a given owner source holds its slot for (see [`MarkSource`]'s
    /// "PER-SOURCE WINDOWS"). Non-venue-mark sources never own, so their window is unused.
    fn ownership_window(&self, owner: MarkSource) -> i64 {
        match owner {
            MarkSource::ReconcileMark => self.reconcile_staleness_ms,
            _ => self.mark_staleness_ms,
        }
    }

    /// The current mark for a symbol, if one has ever been written. Interns its `&str` arguments
    /// (bounded label sets — see the module doc) instead of allocating two `String`s per read.
    pub fn mark_of(&self, venue: &str, symbol: &str) -> Option<f64> {
        self.mark_of_key(&(Ustr::from(venue), Ustr::from(symbol)))
    }

    /// [`Self::mark_of`] for a caller that already holds the interned key — the allocation-free,
    /// intern-free read the fold-side callers use.
    pub fn mark_of_key(&self, key: &MarkKey) -> Option<f64> {
        self.marks.get(key).copied()
    }

    /// Every `(venue, symbol) -> mark` pair in insertion order (read-models, snapshots).
    pub fn marks_iter(&self) -> impl Iterator<Item = (&MarkKey, &f64)> {
        self.marks.iter()
    }

    /// Which concept currently owns a symbol's slot, and the core-clock ms it was written at.
    /// Exposed for tests and diagnostics; no fold formula reads it.
    pub fn mark_provenance(&self, venue: &str, symbol: &str) -> Option<(MarkSource, i64)> {
        self.mark_meta.get(&(Ustr::from(venue), Ustr::from(symbol))).copied()
    }

    /// Override the STREAMED venue-mark ownership window (see [`MarkSource`]). The live runtime
    /// calls this from `CoreConfig::mark_staleness_ms`; `0` makes streamed-mark ownership expire
    /// immediately, restoring pure last-write-wins for that source.
    pub fn set_mark_staleness_ms(&mut self, ms: i64) {
        self.mark_staleness_ms = ms;
    }

    /// Override the RECONCILE-mark ownership window (see [`MarkSource`]'s "PER-SOURCE WINDOWS").
    /// The live runtime calls this from `CoreConfig::reconcile_mark_staleness_ms`. Should exceed
    /// the deployment's `VIKE_RECONCILE_INTERVAL_MS` so a reconcile mark holds its slot between
    /// passes; `0` makes it expire immediately.
    pub fn set_reconcile_staleness_ms(&mut self, ms: i64) {
        self.reconcile_staleness_ms = ms;
    }

    /// Mark-to-market PnL on the open position. 0.0 if flat or no mark recorded yet.
    /// Same shape as compute_fill's realized line evaluated at the mark:
    /// `(mark - avg_px) * size * multiplier` (sign rides in the signed size).
    pub fn unrealized_pnl(&self, venue: &str, symbol: &str, position_side: &str) -> f64 {
        self.unrealized_of_key(&(
            Ustr::from(venue),
            Ustr::from(symbol),
            PositionSide::from(position_side),
        ))
    }

    /// [`Self::unrealized_pnl`] for a caller that already holds the interned key — the
    /// allocation-free, intern-free form the `equity_all` fold and the margin sweep use.
    /// Identical arithmetic and identical silent-zero for a flat / unmarked position.
    pub fn unrealized_of_key(&self, key: &PositionKey) -> f64 {
        let (venue, symbol, _side) = key;
        match (self.positions.get(key), self.marks.get(&(*venue, *symbol))) {
            (Some(p), Some(m)) => (m - p.avg_px) * p.size * self.multiplier_of(symbol),
            _ => 0.0,
        }
    }

    /// Unrealized PnL for a position valued at an externally-supplied `px` (e.g. PR-1 resolver
    /// output), multiplier-aware. Mirrors `unrealized_pnl`'s multiplier + sign convention
    /// exactly — `(px - avg_px) * size * multiplier`, trusting an already-signed `size` (same
    /// as `unrealized_pnl` trusts `PositionEntry.size`; `position_side` is not consulted, it
    /// only identifies which side's grid to key the multiplier lookup under) — but takes the
    /// price as a parameter instead of reading `self.marks`, and takes `size`/`avg_px`
    /// explicitly so the caller (already holding the `PositionEntry`) skips the position
    /// re-lookup. Byte-identical to `unrealized_pnl` when `px == self.marks[(venue, symbol)]`.
    /// Cold path only (never the p99 fold) — no logging.
    pub fn unrealized_at(
        &self,
        symbol: &str,
        _position_side: PositionSide,
        size: f64,
        avg_px: f64,
        px: f64,
    ) -> f64 {
        (px - avg_px) * size * self.multiplier_of(symbol)
    }

    /// Mode-aware total equity across ALL open positions (sparse full recompute, no cache).
    /// delta:         seed + balance + realized_pnl + Σ_open unrealized
    /// authoritative: balance + Σ_open unrealized (venue balance is absolute — seed/realized
    /// are already folded into it).
    pub fn equity_all(&self, seed: f64) -> f64 {
        // Python builtin sum() over the positions generator → Neumaier (py_sum), in
        // insertion order (IndexMap ↔ dict) — bit-parity of the fold AND the algorithm.
        let unreal = vike_model::py_sum(self.positions.keys().map(|k| self.unrealized_of_key(k)));
        if self.balance_mode == BalanceMode::Authoritative {
            return self.balance + unreal;
        }
        seed + self.balance + self.realized_pnl + unreal
    }

    /// THE margin-in-use fold — one authority for the `Σ_open |size|·mark·multiplier·rate` sum
    /// that was hand-rolled four times (pre-trade gate, combo lowering, published snapshot,
    /// liquidation watchdog) and had DRIFTED on the rate fallback. Only the RATE POLICY differs
    /// per caller; the fold (marks, multiplier, iteration order) is identical, so it lives here.
    ///
    /// `rate_of(symbol)` supplies each open position's per-unit margin rate:
    /// - `Some(rate)` → the position contributes `|size|·mark·multiplier·rate`;
    /// - `None` → the position is EXCLUDED (LEAN's unpriceable-group skip — used by nothing after
    ///   the snapshot divergence fix, but kept so the seam can express "don't count this one").
    ///
    /// A flat position (`size == 0`) and an UNMARKED position (no `marks` entry) contribute 0,
    /// exactly as every prior copy did. Folds in the `positions` IndexMap insertion order — the
    /// naive `+=` accumulation and left-to-right `|size|·mark·mult·rate` association are
    /// load-bearing for bit-parity; do NOT reorder. Cold / per-order path only (never the p99
    /// message fold): no logging, no allocation added inside the loop (the `marks` key clone
    /// matches what each hand-rolled copy already did).
    pub fn margin_in_use(&self, rate_of: impl Fn(&str) -> Option<f64>) -> f64 {
        self.margin_in_use_by(|(_v, s, _side), _p| rate_of(s))
    }

    /// The mode-aware generalization of [`Self::margin_in_use`] — the SAME fold body, but the
    /// rate policy sees the whole `(PositionKey, PositionEntry)` so a caller can price by
    /// per-position state (the liquidation law's pool partition prices only `Cross` positions
    /// into the shared pool — an `Isolated`/`Cash` position returns `None` and is excluded).
    /// `margin_in_use` delegates here with a symbol-only adapter, so the fold arithmetic
    /// (marks, multiplier, iteration order, `+=` association) still lives in exactly ONE place.
    pub fn margin_in_use_by(
        &self,
        rate_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        self.margin_in_use_priced(|(v, s, _side), _p| self.marks.get(&(*v, *s)).copied(), rate_of)
    }

    /// The price-generalized twin of [`Self::margin_in_use_by`] — the SAME fold law, but the
    /// valuation price comes from the caller's `price_of` instead of `self.marks` (the
    /// `unrealized_at`-vs-`unrealized_pnl` generalization, applied to the margin fold). ONLY the
    /// price input generalizes: `price_of` returning `None` is the exact skip the unmarked
    /// position took before ("unpriceable → contributes 0", LEAN's skip), and with
    /// `price_of = marks lookup` this is bit-identical to `margin_in_use_by` — which delegates
    /// here, so the fold arithmetic (multiplier, iteration order, `+=` association) still lives
    /// in exactly ONE place. Live callers pass the PR-1 resolver
    /// (`ExecutionEngine::resolved_margin_in_use_by`) so margin-in-use shares equity's price
    /// basis. Cold / per-order path only (never the p99 message fold).
    pub fn margin_in_use_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
        rate_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        let mut used = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let (_v, s, _side) = key;
            let Some(px) = price_of(key, p) else {
                continue; // unpriceable → contributes 0 (LEAN skip)
            };
            let Some(rate) = rate_of(key, p) else {
                continue; // caller declined to price this position
            };
            used += p.size.abs() * px * self.multiplier_of(s) * rate;
        }
        used
    }

    /// Signed NET position size for `symbol` on this account — Σ of each side's folded size (a
    /// hedge-mode LONG+SHORT pair nets, a one-way `BOTH` leg stands alone). Pure position read, no
    /// price; 0.0 for a symbol the account holds nothing in. Net-exposure query helper — a
    /// cross-side sibling of [`Self::mark_of`]'s per-symbol reads, cold path only.
    pub fn net_qty(&self, symbol: &str) -> f64 {
        let mut net = 0.0;
        for ((_v, s, _side), p) in self.positions.iter() {
            if s.as_str() == symbol {
                net += p.size;
            }
        }
        net
    }

    /// SIGNED net notional across every open position (long +, short −), priced by the caller's
    /// `price_of`. The exposure twin of [`Self::margin_in_use_priced`]: SAME iteration order (the
    /// `positions` IndexMap), SAME flat-skip and unpriceable-skip (`price_of` `None` → the position
    /// contributes 0, the LEAN skip), SAME multiplier fold — the per-position term is just
    /// `size · px · multiplier` (signed) with no rate. Naive `+=` fold like the margin twin
    /// (Rust-native surface — no bit-parity oracle behind it). Cold / per-order path only.
    pub fn net_notional_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        let mut net = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let (_v, s, _side) = key;
            let Some(px) = price_of(key, p) else {
                continue; // unpriceable → contributes 0 (LEAN skip)
            };
            net += vike_model::signed_notional(p.size, px, self.multiplier_of(s));
        }
        net
    }

    /// GROSS notional across every open position — Σ `|size| · px · multiplier`, never netting a
    /// long against a short (a hedged pair sums to its two legs' notionals, not zero). The gross
    /// twin of [`Self::net_notional_priced`]; identical skips and fold. Cold / per-order path only.
    pub fn gross_notional_priced(
        &self,
        price_of: impl Fn(&PositionKey, &PositionEntry) -> Option<f64>,
    ) -> f64 {
        let mut gross = 0.0;
        for (key, p) in self.positions.iter() {
            if p.size == 0.0 {
                continue;
            }
            let (_v, s, _side) = key;
            let Some(px) = price_of(key, p) else {
                continue; // unpriceable → contributes 0 (LEAN skip)
            };
            gross += vike_model::gross_notional(p.size, px, self.multiplier_of(s));
        }
        gross
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
        // Feature 2 (cash reconcile) baseline: a non-empty frame ALWAYS ends up setting `balance`
        // authoritatively below (one of the three selection paths fires), so snapshot the
        // realized-PnL baseline once here — `realized_pnl` does not change inside this fn. This is
        // the anchor the cash-reconcile diff subtracts to cancel the realized-since-sync confound;
        // inert unless `VIKE_RECONCILE_BALANCE` is on. Applies to LIVE venue AccountState folds too,
        // which is correct: each genuine venue balance push IS a reseed, so "realized since sync"
        // resets to zero at that instant.
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

    /// Value the per-asset ledger in `acct_ccy` (Σ converted, py_sum order = upsert order).
    /// `None` if ANY nonzero-qty asset has no conversion path — the LEAN convention: an
    /// unpriced leg is the caller's decision (error / skip / defer), never a silent number.
    /// Zero-qty assets convert to zero without needing a rate.
    pub fn value_in(&self, rates: &vike_model::RateBook, acct_ccy: &str) -> Option<f64> {
        let mut parts = Vec::with_capacity(self.balances_by_asset.len());
        for (asset, qty) in &self.balances_by_asset {
            parts.push(rates.convert(*qty, asset, acct_ccy)?);
        }
        Some(vike_model::py_sum(parts))
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
        // Python truthiness: `if ev.qty` — 0.0 (and NaN is not produced here) means whole size.
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
            // NOT snapshotted, deliberately — and NOT for `mark_meta`'s reason. This grid is MOUNT
            // CONFIGURATION derived from a live venue `meta` fetch, not folded state: the
            // composition root installs it while building the engine, so a restore that carried a
            // stale copy in `AccountSnapshot` would pin a venue's asset facts to whenever the
            // snapshot was taken. Every restored POSITION keeps its own `margin_mode` (that field
            // IS in `PositionEntry` and IS snapshotted), so nothing already open is affected; only
            // an open-from-flat after a restore reads this, and the mount is what should answer it.
            // ⚠ A future live restore path must therefore re-apply
            // [`Account::with_default_margin_modes`] after `restore`, exactly as it re-runs the
            // rest of the mount.
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
            // the next writer of any source takes it — the same state a venue that never
            // streamed a mark is always in. Never a stale owner freezing valuation after replay.
            mark_meta: IndexMap::new(),
            mark_staleness_ms: DEFAULT_MARK_STALENESS_MS,
            reconcile_staleness_ms: DEFAULT_RECONCILE_STALENESS_MS,
            funding_paid: snap.funding_paid,
            fees_paid: snap.fees_paid,
            fees_by_asset: snap.fees_by_asset.iter().cloned().collect(),
            // NOT in `AccountSnapshot`, and that is not an omission. The dedup ledger's durable home
            // is `EngineSnapshot::seen_trade_ids` — where it has always been, at the TOP level of the
            // engine snapshot — so moving the in-memory set onto `Account` left the journal wire and
            // its `state_hash` determinism fence byte-identical. `ExecutionEngine::from_snapshot`
            // seeds it here via [`Account::seed_seen_fill_ids`] immediately after this call, exactly
            // as it used to assign the engine's own field. A BARE `Account::restore` (tests, a
            // read-model) therefore starts with an empty ledger — the same state `Account::new`
            // gives, and safe, because a bare `Account` folds no live venue stream.
            seen_fill_ids: HashMap::new(),
            // The counters are LIVE HEALTH SIGNALS, not folded state (the `mark_meta` /
            // `dropped_nonfinite` doctrine): they are re-derived from the events a session actually
            // sees, and carrying a prior session's tally across a restore would misreport this
            // session's. Deliberately absent from the snapshot, so the hash fence is untouched.
            duplicate_fills_refused: 0,
            colliding_fills_refused: 0,
        }
    }
}

#[path = "account_tests.rs"]
#[cfg(test)]
mod account_tests;

#[path = "mark_law_tests.rs"]
#[cfg(test)]
mod mark_law_tests;

#[path = "fill_dedup_tests.rs"]
#[cfg(test)]
mod fill_dedup_tests;
