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
use vike_model::compute_fill;
use vike_model::events::{AccountState, FillEvent, FundingEvent, PositionLiquidated, PositionSide};
use vike_model::MarginMode;

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
    if h == PRINT_UNKNOWN {
        1
    } else {
        h
    }
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
    /// — see [`Account::multiplier_grid`] and `vike_core::snapshot::VenueBlock::multipliers`.
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
        if !src.is_venue_mark() {
            if let Some((owner, at)) = self.mark_meta.get(&key) {
                // The ownership window is keyed off the CURRENT OWNER's source: a streamed mark
                // (~1s cadence) holds for `mark_staleness_ms`, a reconcile mark (sampled at the
                // ~60s reconcile cadence) for the longer `reconcile_staleness_ms`, so its slot
                // does not expire and alternate between passes. See [`MarkSource`].
                let window = self.ownership_window(*owner);
                if owner.is_venue_mark() && now_ms.saturating_sub(*at) <= window {
                    return false;
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrealized_at_applies_multiplier_and_side() {
        // multiplier 10 for "BTC", default 1
        let mut mults = IndexMap::new();
        mults.insert("BTC".to_string(), 10.0);
        let acc = Account::new(1.0, "binance", Some(mults), BalanceMode::Delta);
        // long 2 @ 100, priced at 105 -> (105-100)*2*10 = 100
        assert_eq!(acc.unrealized_at("BTC", PositionSide::Long, 2.0, 100.0, 105.0), 100.0);
        // short 2 @ 100 (size stored signed, mirroring unrealized_pnl): short valued at 105 ->
        // loss -> (105-100) * (-2) * 10 = -100
        assert_eq!(acc.unrealized_at("BTC", PositionSide::Short, -2.0, 100.0, 105.0), -100.0);
        // default multiplier (unknown symbol) = 1
        assert_eq!(acc.unrealized_at("ETH", PositionSide::Long, 1.0, 10.0, 12.0), 2.0);
    }

    #[test]
    fn unrealized_at_matches_unrealized_pnl_when_px_equals_mark() {
        let mut mults = IndexMap::new();
        mults.insert("BTC".to_string(), 10.0);
        let mut acc = Account::new(1.0, "binance", Some(mults), BalanceMode::Delta);
        let key: PositionKey = ("binance".into(), "BTC".into(), PositionSide::Long);
        acc.positions.insert(key, PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() });
        acc.set_mark_from("binance", "BTC", 105.0, MarkSource::VenueMark, 0);

        let via_marks = acc.unrealized_pnl("binance", "BTC", "LONG");
        let pos = acc.positions[&key];
        let via_supplied_px =
            acc.unrealized_at("BTC", PositionSide::Long, pos.size, pos.avg_px, 105.0);

        assert_eq!(via_supplied_px, via_marks);
    }

    // --- margin_in_use (the ONE shared margin fold) ------------------------------------------

    /// Two open positions, one with a bespoke multiplier and one flat, plus one unmarked.
    fn margin_account() -> Account {
        let mut mults = IndexMap::new();
        mults.insert("ETHUSDT".to_string(), 10.0); // multiplier ≠ 1
        let mut a = Account::new(1.0, "binance", Some(mults), BalanceMode::Delta);
        // BTC: long 2 @ ..., marked 100, mult 1
        a.positions.insert(
            ("binance".into(), "BTCUSDT".into(), "BOTH".into()),
            PositionEntry { size: 2.0, avg_px: 90.0, ..Default::default() },
        );
        a.set_mark_from("binance", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        // ETH: short 3 @ ..., marked 50, mult 10
        a.positions.insert(
            ("binance".into(), "ETHUSDT".into(), "BOTH".into()),
            PositionEntry { size: -3.0, avg_px: 55.0, ..Default::default() },
        );
        a.set_mark_from("binance", "ETHUSDT", 50.0, MarkSource::VenueMark, 0);
        a
    }

    #[test]
    fn margin_in_use_folds_abs_size_mark_mult_rate_in_order() {
        let a = margin_account();
        // flat rate 0.1 over both: BTC 2·100·1·0.1 = 20 ; ETH 3·50·10·0.1 = 150 → 170
        let used = a.margin_in_use(|_s| Some(0.1));
        assert_eq!(used.to_bits(), 170.0_f64.to_bits());
        // multiplier IS folded for the mult=10 position (else ETH would be 15, total 35).
        assert_ne!(used, 35.0);
    }

    #[test]
    fn margin_in_use_none_rate_excludes_that_position() {
        let a = margin_account();
        // rate only for BTC; ETH declined → excluded. 2·100·1·0.1 = 20.
        let used = a.margin_in_use(|s| if s == "BTCUSDT" { Some(0.1) } else { None });
        assert_eq!(used.to_bits(), 20.0_f64.to_bits());
    }

    #[test]
    fn margin_in_use_skips_flat_and_unmarked() {
        let mut a = margin_account();
        // add a flat position (counts 0) and an unmarked one (unpriceable → 0)
        a.positions.insert(
            ("binance".into(), "FLAT".into(), "BOTH".into()),
            PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
        );
        a.set_mark_from("binance", "FLAT", 10.0, MarkSource::VenueMark, 0);
        a.positions.insert(
            ("binance".into(), "NOMARK".into(), "BOTH".into()),
            PositionEntry { size: 5.0, avg_px: 10.0, ..Default::default() },
        );
        // still 170 — the flat and the unmarked contribute nothing.
        let used = a.margin_in_use(|_s| Some(0.1));
        assert_eq!(used.to_bits(), 170.0_f64.to_bits());
    }

    #[test]
    fn margin_in_use_gate_and_snapshot_policies_agree_on_no_override_position() {
        // Reproduce the closed divergence at the Account level: per-symbol margin armed for BTC
        // only (im_by_symbol={BTCUSDT:0.2}, no global default). ETH has NO override.
        let a = margin_account();
        let mut im_by_symbol: IndexMap<String, f64> = IndexMap::new();
        im_by_symbol.insert("BTCUSDT".to_string(), 0.2);
        // im_for(sym) = im_by_symbol.get(sym).or(None)  → BTC Some(0.2), ETH None.
        let im_for = |s: &str| im_by_symbol.get(s).copied();

        // GATE policy (order on BTC, order im 0.2): no-override ETH falls back to the order im.
        let order_im = 0.2_f64;
        let gate = a.margin_in_use(|s| Some(im_for(s).unwrap_or(order_im)));

        // OLD snapshot policy (the BUG): im_for with no fallback → ETH skipped, understated.
        let old_snapshot = a.margin_in_use(im_for);

        // NEW snapshot policy: fall back to the max armed rate (0.2 here) → ETH now counted.
        let fallback = 0.2_f64;
        let new_snapshot = a.margin_in_use(|s| Some(im_for(s).unwrap_or(fallback)));

        // BTC 2·100·1·0.2 = 40 ; ETH 3·50·10·0.2 = 300.
        assert_eq!(old_snapshot.to_bits(), 40.0_f64.to_bits()); // ETH invisible → understated
        assert_eq!(gate.to_bits(), 340.0_f64.to_bits()); // gate always counted ETH
        assert_eq!(new_snapshot.to_bits(), gate.to_bits()); // divergence closed
    }

    /// The delegation pin: `margin_in_use_priced` with a marks-lookup price closure IS
    /// `margin_in_use_by` bit-for-bit (the fold law stays ONE — only the price input
    /// generalizes), and a caller-supplied price re-values the SAME fold at that price.
    #[test]
    fn margin_in_use_priced_marks_closure_is_bit_identical_and_price_generalizes() {
        let a = margin_account();
        let via_by = a.margin_in_use_by(|_k, _p| Some(0.1));
        let via_priced = a.margin_in_use_priced(
            |(v, s, _side), _p| a.marks.get(&(*v, *s)).copied(),
            |_k, _p| Some(0.1),
        );
        assert_eq!(via_by.to_bits(), via_priced.to_bits());
        // a different price basis re-values the fold: BTC 2·90·1·0.1 = 18 ; ETH 3·45·10·0.1 = 135
        let repriced = a.margin_in_use_priced(
            |(_v, s, _side), _p| Some(if s == "BTCUSDT" { 90.0 } else { 45.0 }),
            |_k, _p| Some(0.1),
        );
        assert_eq!(repriced.to_bits(), 153.0_f64.to_bits());
        // None from the price closure is the same unpriceable skip the unmarked position takes
        let btc_only = a.margin_in_use_priced(
            |(_v, s, _side), _p| (s == "BTCUSDT").then_some(100.0),
            |_k, _p| Some(0.1),
        );
        assert_eq!(btc_only.to_bits(), 20.0_f64.to_bits());
    }

    // --- net-exposure query helpers (Ext 3) --------------------------------------------------

    #[test]
    fn net_qty_sums_signed_size_per_symbol() {
        let a = margin_account(); // BTC long 2, ETH short 3
        assert_eq!(a.net_qty("BTCUSDT"), 2.0);
        assert_eq!(a.net_qty("ETHUSDT"), -3.0);
        assert_eq!(a.net_qty("NOPE"), 0.0);
    }

    #[test]
    fn net_and_gross_notional_price_signed_and_abs() {
        let a = margin_account();
        let marks =
            |(v, s, _side): &PositionKey, _p: &PositionEntry| a.marks.get(&(*v, *s)).copied();
        // BTC 2·100·1 = 200 (long, +) ; ETH 3·50·10 = 1500 (short, −) → net −1300
        assert_eq!(a.net_notional_priced(marks).to_bits(), (-1_300.0_f64).to_bits());
        // gross never nets long against short: 200 + 1500 = 1700 (multiplier folded for ETH)
        assert_eq!(a.gross_notional_priced(marks).to_bits(), 1_700.0_f64.to_bits());
    }

    #[test]
    fn notional_skips_flat_and_unpriceable_like_margin() {
        let mut a = margin_account();
        a.positions.insert(
            ("binance".into(), "FLAT".into(), "BOTH".into()),
            PositionEntry { size: 0.0, avg_px: 10.0, ..Default::default() },
        );
        // NOMARK has a size but no marks entry → price_of returns None → excluded.
        a.positions.insert(
            ("binance".into(), "NOMARK".into(), "BOTH".into()),
            PositionEntry { size: 5.0, avg_px: 10.0, ..Default::default() },
        );
        let marks =
            |(v, s, _side): &PositionKey, _p: &PositionEntry| a.marks.get(&(*v, *s)).copied();
        // still 1700 / −1300 — the flat and the unpriceable contribute nothing.
        assert_eq!(a.gross_notional_priced(marks).to_bits(), 1_700.0_f64.to_bits());
        assert_eq!(a.net_notional_priced(marks).to_bits(), (-1_300.0_f64).to_bits());
    }

    // --- margin_mode carrier (feat/margin-mode-field) ----------------------------------------

    #[test]
    fn position_entry_defaults_to_cross_none() {
        let p = PositionEntry::default();
        assert_eq!(p.margin_mode, MarginMode::Cross);
        assert_eq!(p.isolated_margin, None);
        // an explicit two-field construction is Cross/None too (the byte-identity default path).
        let q = PositionEntry { size: 1.0, avg_px: 2.0, ..Default::default() };
        assert_eq!(q.margin_mode, MarginMode::Cross);
        assert_eq!(q.isolated_margin, None);
    }

    /// THE byte-identity proof at the serde level: a Cross/None position serializes to EXACTLY
    /// the two-field object it did before this field existed (no `margin_mode`, no
    /// `isolated_margin` key). Since `state_hash` is FNV over these canonical JSON bytes, absence
    /// of both keys is proof the hash is unchanged for any all-cross account.
    #[test]
    fn cross_position_serializes_without_the_new_keys() {
        let p = PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"size":2.0,"avg_px":100.0}"#);
        assert!(!json.contains("margin_mode") && !json.contains("isolated_margin"));
    }

    /// A `Cash` position also leaves `isolated_margin` None — but Cash is NOT the skip default, so
    /// it DOES serialize its mode (only `Cross` is skipped). Documents the carrier's shape.
    #[test]
    fn isolated_position_carries_mode_and_wallet_through_serde() {
        let iso = PositionEntry {
            size: 2.0,
            avg_px: 100.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(250.0),
        };
        let json = serde_json::to_string(&iso).unwrap();
        assert!(json.contains(r#""margin_mode":"Isolated""#), "{json}");
        assert!(json.contains(r#""isolated_margin":250.0"#), "{json}");
        let back: PositionEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, iso);

        // Cash carries its mode but no wallet.
        let cash = PositionEntry {
            size: 1.0,
            avg_px: 10.0,
            margin_mode: MarginMode::Cash,
            isolated_margin: None,
        };
        let cjson = serde_json::to_string(&cash).unwrap();
        assert!(cjson.contains(r#""margin_mode":"Cash""#), "{cjson}");
        assert!(!cjson.contains("isolated_margin"), "{cjson}");
        assert_eq!(serde_json::from_str::<PositionEntry>(&cjson).unwrap(), cash);
    }

    /// Compat pin: an OLD two-field journal/snapshot object (no mode keys) deserializes to
    /// Cross/None — `#[serde(default)]` makes the new fields optional on the wire.
    #[test]
    fn legacy_two_field_json_deserializes_to_cross_none() {
        let p: PositionEntry = serde_json::from_str(r#"{"size":-3.0,"avg_px":55.0}"#).unwrap();
        assert_eq!(p, PositionEntry { size: -3.0, avg_px: 55.0, ..Default::default() });
        assert_eq!(p.margin_mode, MarginMode::Cross);
        assert_eq!(p.isolated_margin, None);
    }

    /// The fold (sole position writer) carries an isolated position's mode + wallet forward across
    /// a subsequent fill — the mode is not reset to Cross by a fill on an existing isolated pos.
    #[test]
    fn fold_carries_margin_mode_forward_across_fills() {
        let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
        let key: PositionKey = ("binance".into(), "BTCUSDT".into(), "BOTH".into());
        // Seed an ISOLATED position directly (the mode-parsing PR will do this from a venue frame).
        a.positions.insert(
            key,
            PositionEntry {
                size: 1.0,
                avg_px: 100.0,
                margin_mode: MarginMode::Isolated,
                isolated_margin: Some(50.0),
            },
        );
        // Add to it via the fold (a buy 1 @ 110).
        a.fold(key, 1, 1.0, 110.0, 1.0);
        let p = a.positions[&key];
        assert_eq!(p.size, 2.0);
        assert_eq!(p.margin_mode, MarginMode::Isolated, "mode must survive the fill");
        assert_eq!(p.isolated_margin, Some(50.0), "allocated wallet must survive the fill");
    }

    /// A cross (default) position folded through a fill stays Cross/None — byte-identical carrier.
    #[test]
    fn fold_keeps_cross_positions_cross() {
        let mut a = Account::new(1.0, "binance", None, BalanceMode::Delta);
        let key: PositionKey = ("binance".into(), "BTCUSDT".into(), "BOTH".into());
        a.fold(key, 1, 1.0, 100.0, 1.0);
        a.fold(key, 1, 1.0, 120.0, 1.0);
        let p = a.positions[&key];
        assert_eq!(p.margin_mode, MarginMode::Cross);
        assert_eq!(p.isolated_margin, None);
    }
}

#[cfg(test)]
mod mark_law_tests {
    //! THE PRECEDENCE LAW, tested where it lives. Every one of these would have caught a caller
    //! that wrote the slot without naming its concept — which is now impossible to express,
    //! because `marks` is private and `set_mark_from` is the only door.

    use super::*;

    fn acct() -> Account {
        Account::new(1.0, "sim", None, BalanceMode::Delta)
    }

    #[test]
    fn a_fresh_venue_mark_owns_the_slot_against_a_candle_close() {
        let mut a = acct();
        assert!(a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000));
        assert!(
            !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 5_000),
            "the close must be REFUSED while the mark is fresh"
        );
        assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
        assert_eq!(a.mark_provenance("sim", "BTC"), Some((MarkSource::VenueMark, 1_000)));
    }

    #[test]
    fn a_fresh_venue_mark_owns_the_slot_against_a_trade_tick_too() {
        // The tick lane (`drive_strategy_tick`) is a SEPARATE writer from the bar lane, and it
        // fires far more often. Rounds 1 and 2 guarded only the bar lane.
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        assert!(!a.set_mark_from("sim", "BTC", 99.0, MarkSource::TradeTick, 2_000));
        assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
    }

    #[test]
    fn a_venue_mark_never_blocks_another_venue_mark() {
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        assert!(a.set_mark_from("sim", "BTC", 101.0, MarkSource::VenueMark, 1_500));
        assert!(a.set_mark_from("sim", "BTC", 102.0, MarkSource::ReconcileMark, 1_600));
        assert_eq!(a.mark_of("sim", "BTC"), Some(102.0));
    }

    #[test]
    fn a_reconcile_mark_owns_the_slot_exactly_like_a_streamed_one() {
        // The deliberate decision (round 3): `ExecReport::position_mark_px` IS the venue's mark,
        // so it ranks with the streamed one — on EVERY venue, including those with no mark stream.
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 1_000);
        assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 2_000));
        assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
    }

    #[test]
    fn a_silent_mark_stream_hands_the_slot_back_after_the_window() {
        // The documented degradation path: valuation must fall back to the next-best concept,
        // never freeze at a mark whose stream died.
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        assert!(
            !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_000),
            "age == window"
        );
        assert!(
            a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_001),
            "one ms past the window the close reclaims the slot"
        );
        assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
        assert_eq!(a.mark_provenance("sim", "BTC"), Some((MarkSource::BarClose, 11_001)));
    }

    // --- PER-SOURCE WINDOWS (round 4): reconcile marks own for a longer, cadence-sized window ---

    /// (a) A reconciled STREAM-LESS venue: the reconcile mark holds the slot CONTINUOUSLY across
    /// the reconcile cadence (default 60s), so candle closes raining in every couple of seconds
    /// never alternate it away. Under the pre-round-4 shared 10s window the slot flipped
    /// reconcile-mark → closes → next pass every minute — the alternation this window kills.
    #[test]
    fn a_reconcile_mark_holds_the_slot_across_the_reconcile_cadence() {
        let mut a = acct();
        // pass 1 at t=0
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 0);
        // closes arrive every ~2s for a full minute — none may take the slot
        for t in (1_000..=59_000).step_by(2_000) {
            assert!(
                !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, t as i64),
                "a close must not displace the reconcile mark within its window (t={t})"
            );
        }
        assert_eq!(a.mark_of("sim", "BTC"), Some(100.0), "one concept, continuously");
        // pass 2 at t=60_000 refreshes ownership for another window
        assert!(a.set_mark_from("sim", "BTC", 101.0, MarkSource::ReconcileMark, 60_000));
        assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 119_000));
        assert_eq!(a.mark_of("sim", "BTC"), Some(101.0));
    }

    /// (b) The degradation law for the reconcile source: once reconcile STOPS for longer than
    /// `reconcile_staleness_ms` (150s default), the candle close reclaims the slot — valuation
    /// never freezes at a mark whose reconcile passes have ceased.
    #[test]
    fn a_stopped_reconcile_hands_the_slot_back_after_its_window() {
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 1_000);
        assert!(
            !a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 151_000),
            "age == window: still owned"
        );
        assert!(
            a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 151_001),
            "one ms past the reconcile window the close reclaims the slot"
        );
        assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
    }

    /// (c) A STREAMED venue mark still uses the SHORT window even though the (longer) reconcile
    /// window is also configured — the window is keyed off the CURRENT OWNER's source. This is the
    /// byte-identical-to-head guarantee for streamed-mark venues: the reconcile window must never
    /// leak into the streamed-mark path.
    #[test]
    fn a_streamed_mark_keeps_the_short_window_regardless_of_the_reconcile_window() {
        let mut a = acct();
        a.set_mark_staleness_ms(10_000);
        a.set_reconcile_staleness_ms(150_000);
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        // a close reclaims at 10s+1 — the STREAMED window, not the reconcile one
        assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_000));
        assert!(a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_001));
        assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
    }

    /// The "keyed off the current owner" nuance: a reconcile mark REPLACED by a streamed mark
    /// immediately reverts to the short streamed window (a venue mark never blocks another venue
    /// mark, and the fresher streamed mark then owns under the shorter horizon).
    #[test]
    fn a_streamed_mark_replacing_a_reconcile_mark_reverts_to_the_short_window() {
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::ReconcileMark, 0);
        assert!(a.set_mark_from("sim", "BTC", 101.0, MarkSource::VenueMark, 1_000));
        assert!(!a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_000));
        assert!(a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 11_001));
        assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
    }

    #[test]
    fn ownership_is_per_symbol_and_per_venue() {
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        assert!(a.set_mark_from("sim", "ETH", 50.0, MarkSource::BarClose, 1_000));
        assert!(a.set_mark_from("other", "BTC", 42.0, MarkSource::BarClose, 1_000));
        assert_eq!(a.mark_of("sim", "BTC"), Some(100.0));
        assert_eq!(a.mark_of("sim", "ETH"), Some(50.0));
        assert_eq!(a.mark_of("other", "BTC"), Some(42.0));
    }

    #[test]
    fn a_venue_with_no_mark_at_all_is_byte_identical_to_last_write_wins() {
        // The preservation claim, stated as a test: with no venue mark ever written, every close
        // and tick lands, in order, exactly as the removed `set_mark` did.
        let mut a = acct();
        for (i, px) in [100.0_f64, 101.0, 99.5, 103.25].iter().enumerate() {
            assert!(a.set_mark_from("sim", "BTC", *px, MarkSource::BarClose, i as i64 * 60_000));
        }
        assert_eq!(a.mark_of("sim", "BTC").map(f64::to_bits), Some(103.25_f64.to_bits()));
    }

    #[test]
    fn a_zero_window_restores_pure_last_write_wins() {
        let mut a = acct();
        a.set_mark_staleness_ms(0);
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        assert!(a.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 1_001));
        assert_eq!(a.mark_of("sim", "BTC"), Some(90.0));
    }

    #[test]
    fn a_restored_account_starts_with_an_unowned_slot() {
        // Provenance is deliberately not snapshotted: replay must never inherit a stale owner
        // that would silently refuse every close for the rest of the session.
        let mut a = acct();
        a.set_mark_from("sim", "BTC", 100.0, MarkSource::VenueMark, 1_000);
        let restored = Account::restore(&a.snapshot());
        assert_eq!(restored.mark_of("sim", "BTC"), Some(100.0), "the price survives");
        assert_eq!(restored.mark_provenance("sim", "BTC"), None, "the ownership does not");
        let mut restored = restored;
        assert!(restored.set_mark_from("sim", "BTC", 90.0, MarkSource::BarClose, 1_001));
    }
}

#[cfg(test)]
mod fill_dedup_tests {
    //! The WELD: `apply_fill` refuses an already-folded `trade_id` on its own, with no help from
    //! `ExecutionEngine`. Every test here drives `Account` DIRECTLY — that is the point, since the
    //! defect being fixed was a guard that only existed one type up.

    use super::*;
    use vike_model::events::FillEvent;

    fn fill(trade_id: &str, symbol: &str, side: i32, qty: f64, px: f64, comm: f64) -> FillEvent {
        FillEvent {
            trade_id: vike_model::events::TradeId::new(trade_id).expect("test ids are non-empty"),
            client_order_id: "c1".to_string(),
            venue: "binance".into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_px: px,
            commission: comm,
            commission_asset: "USDT".into(),
            liquidity_side: "taker".into(),
            ts: 1,
            mark_price: None,
            position_side: "BOTH".into(),
        }
    }

    fn acct() -> Account {
        Account::new(1.0, "binance", None, BalanceMode::Delta)
    }

    fn btc_key() -> PositionKey {
        ("binance".into(), "BTCUSDT".into(), PositionSide::Both)
    }

    /// THE INVARIANT. Folding one fill twice moves money exactly ONCE — every quantity, not just the
    /// position: `balance` (the commission), `fees_paid`, `fees_by_asset`, `realized_pnl` and
    /// `closed_pnls`. Before the ledger moved onto `Account` this test could not be written at all:
    /// the guard lived in `ExecutionEngine`, so a direct caller double-counted every field below.
    #[test]
    fn applying_the_same_fill_twice_moves_money_exactly_once() {
        let mut a = acct();
        let open = fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.5);

        assert_eq!(a.apply_fill(&open), FillFold::Applied);
        let after_one = (
            a.balance,
            a.fees_paid,
            a.realized_pnl,
            a.positions[&btc_key()],
            a.closed_pnls.len(),
            a.fees_by_asset.get(&Ustr::from("USDT")).copied(),
        );
        assert_eq!(a.balance.to_bits(), (-0.5_f64).to_bits(), "commission netted once");

        assert_eq!(a.apply_fill(&open), FillFold::Duplicate, "the SECOND fold is refused");
        assert_eq!(
            (
                a.balance,
                a.fees_paid,
                a.realized_pnl,
                a.positions[&btc_key()],
                a.closed_pnls.len(),
                a.fees_by_asset.get(&Ustr::from("USDT")).copied(),
            ),
            after_one,
            "a refused duplicate must not move ANY money-bearing field"
        );
        assert_eq!(a.duplicate_fills_refused, 1, "and the refusal is COUNTED, not silent");
        assert_eq!(a.colliding_fills_refused, 0, "same fill re-delivered is not a collision");
    }

    /// The equity delta of a re-delivery is EXACTLY zero — bitwise, not within a tolerance. Stated
    /// separately from the field-by-field test above because equity is the number an operator watches
    /// and the number the measured the CI box defect moved.
    #[test]
    fn the_equity_delta_of_a_redelivered_fill_is_bitwise_zero() {
        let mut a = acct();
        let f = fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.5);
        a.apply_fill(&f);
        a.set_mark_from("binance", "BTCUSDT", 110.0, MarkSource::VenueMark, 0);
        let before = a.equity_all(1_000.0);
        for _ in 0..5 {
            assert_eq!(a.apply_fill(&f), FillFold::Duplicate);
        }
        assert_eq!(a.equity_all(1_000.0).to_bits(), before.to_bits());
        assert_eq!(a.duplicate_fills_refused, 5);
    }

    /// A realized-PnL round trip: the CLOSING fill re-delivered must not book its PnL twice. The
    /// commission-only tests above would pass even if `fold` were re-run on a flat position, so this
    /// exercises the `closed_pnls` push specifically.
    #[test]
    fn a_redelivered_closing_fill_does_not_realize_pnl_twice() {
        let mut a = acct();
        a.apply_fill(&fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.0));
        assert_eq!(a.apply_fill(&fill("t2", "BTCUSDT", -1, 2.0, 110.0, 0.0)), FillFold::Applied);
        assert_eq!(a.closed_pnls.len(), 1);
        let realized = a.realized_pnl;

        assert_eq!(a.apply_fill(&fill("t2", "BTCUSDT", -1, 2.0, 110.0, 0.0)), FillFold::Duplicate);
        assert_eq!(a.closed_pnls.len(), 1, "no second closed PnL");
        assert_eq!(a.realized_pnl.to_bits(), realized.to_bits());
    }

    /// Distinct ids fold independently — the guard must not be a blanket "one fill per symbol".
    #[test]
    fn distinct_trade_ids_all_fold() {
        let mut a = acct();
        for i in 0..4 {
            assert_eq!(
                a.apply_fill(&fill(&format!("t{i}"), "BTCUSDT", 1, 1.0, 100.0, 0.0)),
                FillFold::Applied
            );
        }
        assert_eq!(a.positions[&btc_key()].size, 4.0);
        assert_eq!(a.duplicate_fills_refused, 0);
        assert_eq!(a.seen_fill_ids().count(), 4);
    }

    /// The untagged-fill case has no test because it has no code: `TradeId::new` refuses the empty
    /// string, so `fill("", …)` does not compile and an unguardable fill cannot be constructed.
    ///
    /// ⚠ This is the note that used to be a test (`an_untagged_fill_folds_every_time_and_is_counted`,
    /// asserting the fill folded twice and the hazard was counted). It is deleted rather than
    /// `#[ignore]`d, because an ignored test asserting an unrepresentable state is a claim nobody can
    /// check. The property it protected is now the type's, and `vike-model`'s
    /// `new_refuses_the_empty_string` / `default_is_not_implemented_and_that_is_the_whole_point` are
    /// where it is pinned.
    #[test]
    fn an_empty_trade_id_cannot_be_constructed_so_no_untagged_fill_exists() {
        assert!(vike_model::events::TradeId::new("").is_err(), "the whole branch rests on this");
    }

    /// THE NARROW-KEY HAZARD, made loud. Two DIFFERENT executions sharing one id string — reachable
    /// today via binance/aster `t` and okx `tradeId` (per-SYMBOL venue sequences) on a multi-symbol
    /// engine, and via `vike_paper`'s per-client `paper-` counter under `MultiPaperExecutionClient`.
    /// The second fill is still refused (see `apply_fill`'s doc for why folding on a mismatch is
    /// worse), but it is reported as a COLLISION, counted separately and logged at ERROR — so a
    /// dropped genuine fill can never be mistaken for a collapsed reconnect replay.
    #[test]
    fn a_trade_id_collision_is_reported_as_a_collision_not_a_replay() {
        let mut a = acct();
        assert_eq!(a.apply_fill(&fill("7", "BTCUSDT", 1, 2.0, 100.0, 0.0)), FillFold::Applied);
        // Same id, DIFFERENT symbol — exactly the per-symbol-sequence shape.
        assert_eq!(a.apply_fill(&fill("7", "ETHUSDT", 1, 3.0, 50.0, 0.0)), FillFold::Collision);
        assert_eq!(a.colliding_fills_refused, 1);
        assert_eq!(a.duplicate_fills_refused, 0, "a collision is NOT a routine duplicate");
        // Same id, same symbol, DIFFERENT qty — the paper-counter shape within one symbol.
        assert_eq!(a.apply_fill(&fill("7", "BTCUSDT", 1, 9.0, 100.0, 0.0)), FillFold::Collision);
        assert_eq!(a.colliding_fills_refused, 2);
        // Nothing moved on either refusal.
        assert_eq!(a.positions[&btc_key()].size, 2.0);
        let eth: PositionKey = ("binance".into(), "ETHUSDT".into(), PositionSide::Both);
        assert!(!a.positions.contains_key(&eth));
    }

    /// The fingerprint deliberately EXCLUDES `commission` and `ts`, because binance perp's early
    /// `TRADE_LITE` fill and its authoritative twin share one `t` and differ in exactly those fields.
    /// Treating that designed-in pair as a collision would put an ERROR in front of every perp fill.
    #[test]
    fn a_redelivery_that_restates_only_fee_or_ts_is_a_duplicate_not_a_collision() {
        let mut a = acct();
        assert_eq!(a.apply_fill(&fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.0)), FillFold::Applied);
        let mut authoritative = fill("t1", "BTCUSDT", 1, 2.0, 100.0, 0.4);
        authoritative.ts = 99;
        assert_eq!(a.apply_fill(&authoritative), FillFold::Duplicate);
        assert_eq!(a.colliding_fills_refused, 0);
        assert_eq!(a.balance.to_bits(), 0.0_f64.to_bits(), "the early fill carried no fee");
    }

    /// A SEEDED id (restored from `EngineSnapshot::seen_trade_ids`) carries no fingerprint, so a
    /// refusal against it is a plain duplicate — never a false collision ERROR in front of a restart.
    #[test]
    fn a_seeded_id_refuses_without_claiming_a_collision() {
        let mut a = acct();
        a.seed_seen_fill_ids(["t1".to_string()]);
        assert!(a.has_folded_fill("t1"));
        // Deliberately a DIFFERENT fill body than anything this session folded.
        assert_eq!(a.apply_fill(&fill("t1", "ETHUSDT", -1, 7.0, 5.0, 1.0)), FillFold::Duplicate);
        assert_eq!(a.colliding_fills_refused, 0, "an unknown fingerprint is not a collision");
        assert_eq!(a.duplicate_fills_refused, 1);
        assert_eq!(a.balance.to_bits(), 0.0_f64.to_bits());
    }

    /// `fill_print` is an IDENTITY hash, not arithmetic: it must separate the four fields it reads and
    /// ignore the ones it does not. Guards against a lazy implementation that concatenates the symbol
    /// and side into an ambiguous byte stream.
    #[test]
    fn fill_print_separates_the_identifying_fields() {
        let base = fill("x", "BTCUSDT", 1, 2.0, 100.0, 0.0);
        assert_eq!(fill_print(&base), fill_print(&fill("y", "BTCUSDT", 1, 2.0, 100.0, 9.9)));
        assert_ne!(fill_print(&base), fill_print(&fill("x", "ETHUSDT", 1, 2.0, 100.0, 0.0)));
        assert_ne!(fill_print(&base), fill_print(&fill("x", "BTCUSDT", -1, 2.0, 100.0, 0.0)));
        assert_ne!(fill_print(&base), fill_print(&fill("x", "BTCUSDT", 1, 2.5, 100.0, 0.0)));
        assert_ne!(fill_print(&base), fill_print(&fill("x", "BTCUSDT", 1, 2.0, 100.5, 0.0)));
        // The sentinel is never mintable, so a computed print can never be read as "unknown".
        assert_ne!(fill_print(&base), PRINT_UNKNOWN);
    }
}
