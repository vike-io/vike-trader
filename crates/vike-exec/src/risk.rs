//! The pre-trade RiskGate — one extensible stage every order crosses.
//! Exact port of `exec/risk.py`. PURE: owns no bus, publishes nothing; the engine acts on the
//! verdict, so risk rules run identically in backtest / paper / live.
//!
//! PARITY: Python `round()` is round-half-to-EVEN (banker's) → `f64::round_ties_even`.
//!
//! RUST-NATIVE deviations from the (retired) Python oracle, both aligning the gate with
//! `SimBroker::apply_fill` (the backtest reference) so live and backtest verdicts agree:
//! * the min-qty / min-notional floors gate OPENING orders only — a pure reduce/close is exempt
//!   (the anti-stranding rule; the margin and impact checks already bypassed on the same
//!   `pure_reduce` predicate);
//! * notional includes the contract multiplier (`qty × ref_price × ctx.multiplier`), exactly as
//!   the gate's own margin calc and SimBroker's fill gate do. `multiplier` defaults to 1.0, so
//!   ordinary instruments are bit-identical.
//!
//! # Pre-trade impact (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::max_slippage_bps`] / [`RiskLimits::require_fillable`] add a walk-the-book
//! veto over `vike_model::L2Book::simulate_fill`. **Where it runs is deliberate**: the gate
//! owns no book, and neither does `ExecutionEngine` — `RiskContext` is a `Copy` scalar
//! struct and the L2 lane (`vike_exec::lanes::BookUpdate`) is folded by the vike-core
//! runtime, NOT by the engine. Plumbing a book channel into `apply_intent` would put a map
//! lookup (and an `Arc`/clone) on the per-order path of the single-writer fold for a knob
//! that is off by default — so instead:
//!
//! * [`impact_veto`] / [`fillable_veto`] are PURE free functions (no gate, no state), and
//! * [`RiskGate::check_with_book`] is the book-aware entry point; [`RiskGate::check`] is
//!   exactly `check_with_book(.., None)` — byte-identical to before for every existing caller.
//!
//! **Caller contract:** the entry point is for any site that holds BOTH the intent and a live
//! book for `(venue, symbol)`. **No such site exists today** — the vike-core runtime folds
//! `BookUpdate` by value (updates the price board, drives `on_order_book`, then drops it) and
//! keeps no per-`(venue, symbol)` book map, so `check_with_book(req, ctx, Some(book))` cannot
//! be written from the runtime without first ADDING book retention there — i.e. paying the
//! per-symbol `L2Book` clone/`Arc` this seam was shaped to avoid until someone wants it. A
//! strategy/mount with its own `on_order_book` view is the cheaper first caller: it already
//! retains a book. Until a site opts in the knobs are inert: `check` never looks at a book,
//! and when both knobs are `None`/`false` `check_with_book` does no book work either (not even
//! a `simulate_fill`). No allocation and no logging are added to the fold.
//!
//! **What the veto judges:** only the part of an order that can take DISPLAYED liquidity right
//! now ([`take_scope`]). Passive limits, stops and take-profits are never vetoed — a resting
//! quote pays no slippage — and a pure reduce/close is bypassed entirely, mirroring the
//! buying-power check.
//!
//! # Combo orders (OPT-IN entry point — combo-orders spec §5)
//!
//! [`RiskGate::check_combo`] crosses a multi-leg combo as an ATOMIC batch of synthetic per-leg
//! checks: all legs pass or the whole combo yields ONE denial naming the failing leg, and the
//! combo consumes exactly ONE throttle slot (it is one venue order). Legs ACCUMULATE — margin
//! committed and position projected by an admitted leg are visible to the next — so the combo is
//! never admitted where the same legs sent sequentially would be refused. Both entry points share the
//! single gate body (`check_inner`); the ONLY difference is who consumes the rate slot, so the
//! single-order [`RiskGate::check`] path is byte-identical to before combos existed. Callers with
//! no combo never reach any of it — an `OrderRequest` with empty `combo_legs` is exactly the order
//! it always was.
//!
//! # Amending a PARTIALLY FILLED order (the `already_executed` parameter)
//!
//! [`RiskGate::check_modify`] judges the PROJECTED order — the resting request with the amend's
//! qty/price folded in — and on an IN-PLACE amend venue that qty is the order's new TOTAL, executed
//! part included. The account position the gate projects against already holds those same executed
//! lots, so `position + side × qty` counted them TWICE: a maker re-quoting an order that had just
//! started getting hit was refused at a cap the identical order was admitted under at submit, and
//! under `Halted` the re-price of a half-done EXIT stopped reading as a covered reduce.
//!
//! The correction is one caller-supplied number, `already_executed`, netted out by
//! [`still_executable`] in the lanes that project the post-order world and in NO other lane (that
//! function is the authority on which, and on why the wire-judging notional lanes keep the total).
//! `0.0` — every non-amend path — is byte-identical to before it existed.
//!
//! ⚠ **The gate does not, and must not, derive that number itself.** Whether the executed part is
//! inside the amend's qty is a PER-VENUE fact: `vike_model`'s `AmendSemantics` declares it, and a
//! CANCEL-REPLACE venue (hyperliquid rests a whole fresh order) must net NOTHING, because there the
//! untouched sum is the correct projection and subtracting would ADMIT an order this gate should
//! refuse. The gate stays pure and venue-agnostic; `ExecutionEngine::modify_order` owns the lookup.
//!
//! # Fat-finger price collar (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::price_collar`] (+ the per-symbol [`RiskLimits::collar_by_symbol`] override, the
//! same shape `im_by_symbol` uses for margin) closes the one hole every other axis leaves open:
//! **nothing in this ladder ever compared the order's own PRICE to the mark.** The size axes
//! (min/max notional, exposure, buying power) all measure a MAGNITUDE, so a limit BUY at 10× the
//! mark sails through every one of them as long as its notional fits under the cap — and a
//! mis-scaled price (a Polymarket `0.55` sent as `0.055` or `5.5`) is a total loss the instant it
//! fills. The collar is the price-sanity axis: an OPENING order carrying a limit or a trigger
//! price is DENIED (`"price-collar"`) when `|price − ctx.mark_price|` exceeds
//! `max(pct × mark, abs_floor)`. Both prices are compared TICK-ROUNDED (`round_to(p,
//! lim.tick_size)`), so a limit and the equivalent trigger get the same verdict at the band edge.
//!
//! **BOTH halves of the band are required, deliberately.** A pure percentage collar is useless on
//! a cheap instrument — 10% of a `0.02` mark is `0.002`, which denies ordinary quoting — and a
//! pure absolute floor is useless on a `100_000` mark. [`PriceCollar::band`] takes the MAX of the
//! two, so the percentage governs expensive instruments and the floor governs cheap ones.
//!
//! Deliberate skips (the COVERED-REDUCE one, below, is the load-bearing third):
//! * **an unpriced mark skips the check entirely** (`mark_price` non-finite or ≤ 0). The mount's
//!   readiness gate guarantees priced-before-order-flow, so denying on an absent mark would be a
//!   pure false positive on a knob whose whole value is that it never fires spuriously;
//! * **a COMBO's net price is never collared.** `OrderRequest::price` on a combo is the SIGNED NET
//!   across legs (`ComboSpec::net_limit`) — negative for a credit structure — and is simply not a
//!   quantity in any single instrument's mark units. The combo's LEGS are still collared:
//!   [`RiskGate::check_combo`] clears each synthetic leg's `price`/`trigger_price`, so each leg
//!   prices off its own mark and carries no price to collar, exactly as it carries none today.
//!
//! Like the min floors / margin / impact axes, the collar **bypasses a COVERED REDUCE** — and for
//! this axis that bypass is not a nicety, it is the whole difference between a safety knob and a
//! catastrophe. A protective bracket leg (`vike_model::build_bracket`'s stop-loss / take-profit —
//! `order_type: "stop"`, `trigger_price: Some(..)`, `reduce_only: true`) is BY DESIGN priced far
//! from the mark: that distance IS the protection. Collaring it would veto exactly the order that
//! limits the loss, the precise inversion of this feature's purpose. So the collar runs AFTER
//! `covered_reduce` is computed and only judges orders that are NOT one — the same anti-stranding
//! rule the floors, buying power and the impact veto already follow. An uncovered order (flat book,
//! or a reversal overshooting the position) OPENS exposure whatever the caller tagged it, and is
//! collared like any opening order. `None`/empty (the default) = the axis does not exist: no
//! comparison runs and the serialized [`RiskLimits`] is byte-identical.

use indexmap::IndexMap;
use std::collections::VecDeque;
use vike_model::{L2Book, OrderRequest};

/// `skip_serializing_if` predicate for default-`false` bool knobs (see [`RiskLimits`]).
#[inline]
fn is_false(b: &bool) -> bool {
    !*b
}

/// `skip_serializing_if` predicate for the empty per-symbol collar map (see
/// [`RiskLimits::collar_by_symbol`]). Spelled as a named fn rather than `IndexMap::is_empty` so
/// the path resolves without leaning on inference inside a serde attribute.
#[inline]
fn is_empty_collar_map(m: &IndexMap<String, PriceCollar>) -> bool {
    m.is_empty()
}

/// `skip_serializing_if` predicate for the empty per-symbol grid map (see
/// [`RiskLimits::grid_by_symbol`]) — same shape and same fence reason as
/// [`is_empty_collar_map`] above.
#[inline]
fn is_empty_grid_map(m: &IndexMap<String, SymbolGrid>) -> bool {
    m.is_empty()
}

/// A per-symbol override of the venue PRICE/SIZE GRID: the tick and lot an order is rounded
/// onto, and the floors it must clear.
///
/// [`RiskLimits`]'s own `tick_size`/`lot_size`/`min_notional`/`min_qty` are SCALARS built from
/// ONE symbol's `SymbolProperties`, and every order is rounded onto them regardless of which
/// instrument it names. That is correct for a single-symbol engine and WRONG the moment an
/// engine admits a second symbol (`extra_symbols`): a coarse mount lot silently destroys a valid
/// finer-grid order — `round_to(0.5, Some(1.0)) == 0.0`, which the gate then denies as
/// `"non-positive-size"`, a refusal whose reason names nothing about the real cause.
///
/// Each field falls back INDEPENDENTLY to the scalar, so an override that only pins `lot_size`
/// still inherits the engine's tick and floors rather than silently disabling them.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SymbolGrid {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lot_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_notional: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_qty: Option<f64>,
}

impl SymbolGrid {
    /// Build one symbol's override from THAT symbol's own venue-fetched instrument grid — the
    /// per-leg twin of [`RiskLimits::from_properties`], over the same four fields and the same
    /// `0.0`-means-unconstrained fold (`vike_model::nz_step`).
    ///
    /// It exists because the mount is the only place that can populate
    /// [`RiskLimits::grid_by_symbol`], and the only thing a venue hands back per symbol is a
    /// `SymbolProperties`. Spelling the mapping HERE, beside the scalar builder rather than at the
    /// mount, is what keeps the two from drifting:
    /// `a_symbol_grid_matches_the_scalar_builder_field_for_field` runs both over one input, so a swapped `step_size`/`tick_size` in either
    /// fails.
    ///
    /// ⚠ **A `0.0` field becomes `None`, and `None` here means INHERIT THE MOUNT SCALAR — not
    /// "unconstrained".** So a leg whose venue publishes no lot at all still rounds on the MOUNTED
    /// symbol's lot. This is a NARROWING of the defect the map exists to fix (which today applies
    /// the mounted symbol's grid to a foreign symbol on all four fields, always), not a full
    /// repair, and it is deliberate rather than overlooked: [`SymbolGrid`] has no spelling for
    /// "explicitly unconstrained", and adding one (`Option<Option<f64>>`, or a sentinel) changes
    /// the serialized shape of [`RiskLimits`] — whose canonical bytes feed
    /// [`crate::engine_snapshot::state_hash`], the journal determinism fence compared ACROSS BINARY
    /// VERSIONS. `a_zero_field_from_the_venue_inherits_the_mount_scalar` pins the residual so it
    /// can be neither quietly widened nor quietly closed.
    pub fn from_properties(f: &vike_model::SymbolProperties) -> Self {
        use vike_model::nz_step as nz;
        SymbolGrid {
            tick_size: nz(f.tick_size),
            lot_size: nz(f.step_size),
            min_notional: nz(f.min_notional),
            min_qty: nz(f.min_qty),
        }
    }
}

/// The grid actually applied to ONE order — a per-symbol override resolved field-by-field over
/// the engine's scalars. Returned by [`RiskLimits::grid_for`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedGrid {
    pub tick_size: Option<f64>,
    pub lot_size: Option<f64>,
    pub min_notional: Option<f64>,
    pub min_qty: Option<f64>,
}

/// A fat-finger PRICE COLLAR band: how far an order's own limit/trigger price may sit from the
/// mark before the gate refuses it (see the module doc's collar section).
///
/// The band is `max(pct × mark, abs_floor)` — BOTH halves are load-bearing: the percentage
/// governs expensive instruments, the absolute floor governs cheap ones (a percentage collar is
/// useless when the mark is `0.02`). Values are in the instrument's quote units; `pct` is a
/// FRACTION (`0.10` = 10%), not basis points and not a percent number.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PriceCollar {
    /// fraction of the mark the price may deviate by, e.g. `0.10` ⇒ ±10%
    pub pct: f64,
    /// absolute band floor in quote units, e.g. `0.02` ⇒ always at least ±0.02
    pub abs_floor: f64,
}

impl PriceCollar {
    /// `max(pct × mark, abs_floor)` — the half-width of the admissible price band around `mark`.
    /// Callers only ever reach this with a finite, positive `mark` (the gate skips otherwise).
    ///
    /// TOTAL AND NON-NEGATIVE BY CONSTRUCTION. Each half is clamped to `0.0` unless it is finite
    /// and strictly positive, so a garbage config (`NaN`, negative, a `pct` typo) can never produce
    /// a negative or `NaN` band. That matters because the gate's test is `|p − mark| > band`: with
    /// a negative band that is ALWAYS true, and this opt-in fat-finger knob would silently become a
    /// TOTAL KILL SWITCH denying every priced order. Clamped, the worst a garbage config can do is
    /// collapse the band to `0.0` — which denies only prices that differ from the mark at all, and
    /// a genuinely un-configured collar is `None`, not a zero band.
    #[inline]
    pub fn band(&self, mark: f64) -> f64 {
        // each half contributes nothing unless it is finite AND strictly positive
        let sane = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        // `mark` is finite and > 0 at every call site, so `pct * mark` is finite and >= 0.0.
        (sane(self.pct) * mark).max(sane(self.abs_floor))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TradingState {
    Active,
    /// only position-reducing orders allowed
    Reducing,
    /// no new orders (kill switch)
    Halted,
}

/// Gate configuration. All limits optional; `None` disables that check.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RiskLimits {
    pub tick_size: Option<f64>,
    pub lot_size: Option<f64>,
    pub min_notional: Option<f64>,
    /// per-order minimum quantity floor (venue lot-size grid); checked on the lot-rounded qty.
    pub min_qty: Option<f64>,
    pub max_notional_per_order: Option<f64>,
    /// Cap on **ONE SYMBOL's** projected open notional at **ONE VENUE** — despite the name, NOT
    /// an account-wide, cross-symbol or cross-venue total.
    ///
    /// `check_inner`'s `over-max-exposure` lane is the only site that evaluates it, as
    /// `|ctx.position_size + side × qty| × ctx.mark_price × ctx.multiplier > cap`, and
    /// [`RiskContext::position_size`] is the signed position in the ORDER's symbol at this
    /// engine's own venue (`ExecutionEngine::gate_position_size`). The gate holds no cross-symbol
    /// position book, and `vike_mount::make_engine` builds one engine — so one `RiskGate`, so one
    /// copy of this cap — PER VENUE. An operator trading N symbols is therefore protected by this
    /// number N times over, once each, and never once in aggregate: a number sized for a whole
    /// book is an N× weaker cap than the operator believes they set.
    ///
    /// The scope is MACHINE-PINNED, not merely claimed here:
    /// `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
    /// `max_total_exposure_is_scoped_to_one_venue_and_one_symbol` arms the cap at the exact
    /// boundary of the order symbol's own projected notional and proves a position in another
    /// symbol — and the same symbol at another venue — contributes NOTHING to it. Widen the basis
    /// and that test goes red. It is what keeps this paragraph true.
    ///
    /// **The name is deliberately KEPT** (`max_symbol_exposure` was considered and rejected).
    /// This is not merely a Rust identifier: the same word is the operator-facing `[risk]` TOML
    /// key (`ProfileRisk::max_total_exposure`), the `&'static str` that
    /// `vike_mount::require_live_risk_budget` pushes into the refusal blocking a live mount, and a
    /// serde key inside [`crate::EngineSnapshot`], whose canonical bytes feed
    /// [`crate::engine_snapshot::state_hash`] — the journal determinism fence, compared ACROSS
    /// BINARY VERSIONS. This field carries no `skip_serializing_if`, so it is present in every
    /// snapshot's JSON and a rename changes every recorded hash. A `#[serde(rename)]` would hold
    /// the wire and the TOML key steady, but it buys nothing worth its cost: the identifier would
    /// then deliberately disagree with the operator-facing string at the safety-critical
    /// diagnostic that names the key, while the OPERATOR — the person this misleads — would still
    /// read the very same word. So the truth is carried where it is actually read: here, in
    /// `ProfileRisk`'s twin, in the refusal's `BUDGET_EXAMPLES` row, and in
    /// `docs/ops/run-profile-live.toml`. Same trade [`RiskLimits::max_leverage`] below records.
    ///
    /// An account-AGGREGATE ceiling is a genuinely different lane, not a rename: it needs the
    /// whole position book inside `check`, which sits in the `p99 < 10µs` hot fold.
    pub max_total_exposure: Option<f64>,
    pub max_orders_per_window: Option<usize>,
    pub window_ms: i64,
    /// The operator's DECLARED leverage cap, recorded for the snapshot/audit surface — NOT
    /// evaluated in `check()`. Enforcement is [`RiskLimits::im_requirement`], which the config
    /// edge derives from this same number (`im = 1.0 / max_leverage`, see `ProfileRisk`); the two
    /// therefore agree by construction rather than being two independent knobs, which is what this
    /// field used to be (issue #822 — `max_leverage` was set, checked nowhere, and silently
    /// duplicated a differently-named knob that did the real work).
    ///
    /// FROZEN, deliberately NOT deleted: `RiskLimits` is serde-embedded in
    /// [`crate::EngineSnapshot`], whose canonical bytes feed
    /// [`crate::engine_snapshot::state_hash`] — the journal determinism fence. Removing the field
    /// changes that shape and, by `vike_core::journal`'s own version contract, would force
    /// `MIN_READABLE_VERSION` up to `VERSION`, turning every existing journal into a cold-start
    /// error. A recorded, honestly-documented field is the cheaper trade.
    pub max_leverage: Option<f64>,
    pub block_reduce_only_overshoot: bool,
    /// RUST-NATIVE (accounting-upgrade Phase B; no Python twin — risk.py has no margin
    /// checks): initial-margin fraction (LEAN `1/leverage`, e.g. 0.1 ⇒ 10x). `Some` enables
    /// the pre-trade buying-power check with LEAN `BuyingPowerModel` semantics (pure
    /// reduce/close orders bypass; flips get the closing credit). None (default) = today's
    /// behavior, byte-identical.
    ///
    /// This is the INTERNAL STORAGE form only — the operator-facing name is `max_leverage`
    /// (`[risk] max_leverage = 10.0` in a profile TOML), converted here by `ProfileRisk` at the
    /// config edge. `im_requirement` keeps the wire name it has always had because it is part of
    /// the [`crate::engine_snapshot::state_hash`] surface; see [`RiskLimits::max_leverage`].
    pub im_requirement: Option<f64>,
    /// Per-symbol initial-margin override (`1/leverage`), set live via `Command::SetMargin`.
    /// Falls back to `im_requirement` (the venue default) when a symbol is absent; empty by
    /// default → byte-identical to today. `IndexMap` keeps insertion order (bit-parity rule),
    /// though this is a lookup map (order does not affect any f64 sum).
    #[serde(default)]
    pub im_by_symbol: IndexMap<String, f64>,
    /// LEAN `RequiredFreeBuyingPowerPercent` haircut on equity (0.0 default).
    pub required_free_bp_pct: f64,
    /// OPT-IN pre-trade market-impact budget in basis points vs mid, evaluated against the
    /// DISPLAYED book (see the module doc). `None` (default) = off: no book is ever walked and
    /// behavior is byte-identical. Only consulted by [`RiskGate::check_with_book`] with a book.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING, not cosmetic. `RiskLimits` is embedded in
    /// [`crate::EngineSnapshot`], whose canonical `serde_json` bytes feed
    /// [`crate::engine_snapshot::state_hash`] — the journal determinism fence. Emitting
    /// `"max_slippage_bps":null` would change that hash for every snapshot with the knob OFF,
    /// so a journal recorded before this field existed would fail replay's fence with a
    /// spurious "replayed hash != recorded" instead of matching. Off ⇒ absent ⇒ same bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_slippage_bps: Option<f64>,
    /// OPT-IN own knob (default `false`): deny when displayed depth cannot cover the full order
    /// size, regardless of `max_slippage_bps`. Independent of the budget — either knob alone
    /// arms the book walk.
    ///
    /// SERDE: `skip_serializing_if` is load-bearing for the same determinism-fence reason as
    /// [`RiskLimits::max_slippage_bps`] above.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_fillable: bool,
    /// OPT-IN fat-finger PRICE COLLAR — the venue-wide default band (see the module doc's collar
    /// section). `None` (default) = the axis does not exist: no order's price is ever compared to
    /// the mark and every verdict is byte-identical to before this field existed.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING for exactly the reason spelled out on
    /// [`RiskLimits::max_slippage_bps`] — `RiskLimits` is embedded in [`crate::EngineSnapshot`],
    /// whose canonical `serde_json` bytes feed [`crate::engine_snapshot::state_hash`], the journal
    /// determinism fence. Off ⇒ absent ⇒ same bytes ⇒ same hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_collar: Option<PriceCollar>,
    /// Per-symbol price-collar override, mirroring [`RiskLimits::im_by_symbol`]: falls back to
    /// [`RiskLimits::price_collar`] (the venue default) when a symbol is absent. Empty by default
    /// ⇒ byte-identical to today. `IndexMap` keeps insertion order (bit-parity rule), though this
    /// is a lookup map (order does not affect any f64 sum).
    ///
    /// SERDE: unlike `im_by_symbol` (which predates the determinism fence and whose `{}` is
    /// already baked into the pinned hash) this map is SKIPPED when empty — same fence reason as
    /// `price_collar` above.
    #[serde(default, skip_serializing_if = "is_empty_collar_map")]
    pub collar_by_symbol: IndexMap<String, PriceCollar>,
    /// Per-symbol PRICE/SIZE GRID overrides (see [`SymbolGrid`]). Empty — every engine today —
    /// means every order is rounded onto the scalars above exactly as before.
    ///
    /// This is the precondition for admitting an order in a symbol other than the engine's own:
    /// the scalars are built from ONE symbol's `SymbolProperties`, so without an override a
    /// second symbol is judged on the first's tick, lot and floors.
    ///
    /// SERDE: SKIPPED when empty, same determinism-fence reason as `collar_by_symbol` — the
    /// canonical bytes of an engine that never sets it are unchanged, so `state_hash` and every
    /// existing journal are unaffected.
    #[serde(default, skip_serializing_if = "is_empty_grid_map")]
    pub grid_by_symbol: IndexMap<String, SymbolGrid>,
}

impl RiskLimits {
    /// The price/size grid to judge an order in `symbol` on: its per-symbol override resolved
    /// field-by-field over the engine's scalars.
    ///
    /// No override (every engine today) returns the scalars verbatim, so every existing verdict
    /// is byte-identical. Field-by-field fallback is deliberate: an override that pins only
    /// `lot_size` still inherits the engine's tick and floors, rather than silently switching
    /// them off — a half-specified grid must not be a way to disable a floor.
    pub fn grid_for(&self, symbol: &str) -> ResolvedGrid {
        match self.grid_by_symbol.get(symbol) {
            None => ResolvedGrid {
                tick_size: self.tick_size,
                lot_size: self.lot_size,
                min_notional: self.min_notional,
                min_qty: self.min_qty,
            },
            Some(g) => ResolvedGrid {
                tick_size: g.tick_size.or(self.tick_size),
                lot_size: g.lot_size.or(self.lot_size),
                min_notional: g.min_notional.or(self.min_notional),
                min_qty: g.min_qty.or(self.min_qty),
            },
        }
    }

    pub fn new() -> Self {
        RiskLimits { window_ms: 1000, ..Default::default() }
    }

    /// Resolve the initial-margin fraction for `symbol`: the per-symbol override if present,
    /// else the venue default `im_requirement`, else `None` (buying-power gate off).
    pub fn im_for(&self, symbol: &str) -> Option<f64> {
        self.im_by_symbol.get(symbol).copied().or(self.im_requirement)
    }

    /// Resolve the fat-finger price collar for `symbol`: the per-symbol override if present, else
    /// the venue default [`RiskLimits::price_collar`], else `None` (the axis is off). Same
    /// override-then-default shape as [`RiskLimits::im_for`].
    pub fn collar_for(&self, symbol: &str) -> Option<PriceCollar> {
        self.collar_by_symbol.get(symbol).copied().or(self.price_collar)
    }

    /// Build limits from a venue's fetched instrument grid (`0.0` fields = unconstrained → None).
    /// tick_size→tick_size, step_size→lot_size, min_qty→min_qty, min_notional→min_notional; other
    /// knobs inherit `new()` defaults (window_ms=1000, everything else off).
    pub fn from_properties(f: &vike_model::SymbolProperties) -> Self {
        use vike_model::nz_step as nz;
        RiskLimits {
            tick_size: nz(f.tick_size),
            lot_size: nz(f.step_size),
            min_notional: nz(f.min_notional),
            min_qty: nz(f.min_qty),
            ..RiskLimits::new()
        }
    }
}

/// Runtime state the gate evaluates an order against.
#[derive(Debug, Clone, Copy)]
pub struct RiskContext {
    /// Current SIGNED position in the ORDER's symbol, at the engine's OWN venue — exactly one
    /// `(venue, symbol)` bucket, never a book-wide total. `ExecutionEngine::gate_position_size` is
    /// the producer (it nets the hedge `LONG`/`SHORT` buckets when the one-way `BOTH` bucket is
    /// flat). Every position-derived verdict in this gate inherits that scope, including
    /// [`RiskLimits::max_total_exposure`]'s — see that field's doc.
    pub position_size: f64,
    /// price used for notional when the order has none
    pub mark_price: f64,
    pub trading_state: TradingState,
    /// injected clock (for the throttler)
    pub now_ms: i64,
    /// account equity (Phase B margin check; unused unless `im_requirement` is set)
    pub equity: f64,
    /// Σ **committed** initial margin, account currency (Phase B; 0.0 when unused): open positions
    /// PLUS this engine's live un-filled orders.
    ///
    /// ⚠ It said "of open positions" and that was the bug — the orders term was missing, so
    /// `free_buying_power` overstated what the account could back by the margin of everything in
    /// flight. `ExecutionEngine::live_order_margin` is the added half; `RiskGate::check_combo`'s
    /// `committed_margin` is the same idea for legs admitted earlier in one combo.
    pub margin_used: f64,
    /// margin freed + re-open credit when this order reverses the position (LEAN
    /// `GetMarginRemaining` closing branch; 0.0 for same-direction opens)
    pub closing_credit: f64,
    /// contract multiplier of the order's symbol (1.0 when unused)
    pub multiplier: f64,
}

impl Default for RiskContext {
    fn default() -> Self {
        RiskContext {
            position_size: 0.0,
            mark_price: 0.0,
            trading_state: TradingState::Active,
            now_ms: 0,
            equity: 0.0,
            margin_used: 0.0,
            closing_credit: 0.0,
            multiplier: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RiskVerdict {
    pub ok: bool,
    /// normalized (rounded) request when ok
    pub request: Option<OrderRequest>,
    pub reason: String,
}

// ---------------------------------------------------------------------------------------------
// STATELESS RISK PRIMITIVES — hoisted to vike-model, RE-EXPORTED here.
//
// `round_to`, the whole pre-trade impact surface (`ImpactDeny`/`TakeScope`/`impact_veto`/
// `take_scope`/`scoped_impact_veto`/`fillable_veto`) and `clamp_leverage` are pure functions of
// vike-model types — they own no state, no bus and no I/O, so they live in the common ancestor
// crate where vike-backtest and vike-chart can reach them without depending on vike-exec.
//
// These re-exports make the move a NO-OP for every existing caller: `vike_exec::risk::round_to`,
// `vike_exec::round_to`, `vike_exec::ImpactDeny`, ... all still resolve, with identical bodies.
// What STAYS in this module is the stateful half: `RiskLimits`/`TradingState` (serde-embedded in
// `EngineSnapshot`, feeding `state_hash` — the journal determinism fence), `RiskGate`'s
// `order_times` throttle window, `check_inner`'s ordered ladder and `check_margin_call`.
// ---------------------------------------------------------------------------------------------
pub use vike_model::{
    clamp_leverage, fillable_veto, impact_veto, round_to, scoped_impact_veto, take_scope,
    ImpactDeny, TakeScope,
};

/// The part of an order that can still MOVE the position: its quantity minus whatever of it has
/// ALREADY executed and is therefore already counted inside [`RiskContext::position_size`].
///
/// # Why this exists
///
/// On a venue whose amend is IN PLACE, an amend's quantity is the order's new TOTAL — executed part
/// included — while the account position already holds that order's own executed lots. Every
/// projection of the form `position + side × qty` therefore counted the SAME lots twice: an
/// amend that is economically a no-op measured larger than the world it was asking for, and the gate
/// refused exactly the quotes that were WORKING. `vike_model`'s `AmendSemantics` is the per-venue
/// authority on whether that netting applies (a cancel-replace venue rests a FRESH order and must
/// keep the whole qty); this function is only the arithmetic.
///
/// # Which lanes use it, and which deliberately do NOT
///
/// It is for lanes that project the POST-ORDER WORLD, because only those can double count:
/// `RiskGate::check_inner`'s halt-exemption coverage, its reduce-only overshoot guard, the
/// `over-max-exposure` cap and the buying-power charge.
///
/// The lanes that judge the ORDER AS IT GOES ON THE WIRE keep the whole quantity — `below-min-qty`,
/// `below-min-notional`, `over-max-notional` and the price collar. On an in-place venue the wire
/// really does carry the total, so netting there would change three limits nobody asked to change:
/// a per-order notional cap would stop capping what is actually sent, and the venue floors would
/// start judging a number the venue never sees.
///
/// # THE INVARIANT THIS ARITHMETIC RESTS ON
///
/// **`already_executed` must be a quantity that is ALREADY inside [`RiskContext::position_size`].**
/// Netting a lot that never moved the position hands the gate a smaller order than the one being
/// placed, which SILENTLY REDUCES measured risk — the one direction this whole correction was
/// shaped to avoid. The single caller,
/// `crates/vike-exec/src/execution_engine/mod.rs`'s `modify_order`, satisfies it by construction on
/// the venue lane: a fill arrives TWICE — as the bare `Event::Fill` the `Account` folds into the
/// position, and as the `OrderPartiallyFilled` wrap the FSM folds into `ManagedOrder::filled_qty` —
/// and both come off the same venue event. ⚠ **They are deduped by two SEPARATE id sets**
/// (`ExecutionEngine`'s `seen_trade_ids` and `seen_fsm_trade_ids`), so the coupling is a property of
/// the lane rather than of the data structure: a wrap delivered without its bare fill would inflate
/// `filled_qty` while the position stayed put. `crates/vike-exec/tests/engine/partial_fill_amend_accounting.rs`'s
/// `the_netting_assumes_filled_qty_is_inside_the_position` pins both the invariant and the direction
/// of that residual.
///
/// # The `0.0` path is byte-identical, on purpose
///
/// Every non-amend caller passes `0.0` and gets `qty` back UNTOUCHED — not `qty - 0.0`, and not
/// `(qty - 0.0).max(0.0)`. The distinction is not pedantry: this runs BEFORE the gate's
/// `non-positive-size` check, so `qty` may still be NaN or negative there, and `f64::max` returns
/// the non-NaN operand — a `.max(0.0)` on the submit path would silently turn a NaN qty into `0.0`
/// and change verdicts on orders that have nothing to do with amends.
///
/// # A garbage `already_executed` nets NOTHING — guarded HERE, not at the caller
///
/// The netting arm is taken only for a FINITE, STRICTLY POSITIVE `already_executed`; everything else
/// (`0.0`, `-0.0`, negative, `NaN`, `±INFINITY`) returns `qty` verbatim, which is the conservative
/// arithmetic. Both bad values fail in the ANTI-conservative direction and neither is caught by an
/// `== 0.0` test: `NaN != 0.0` takes the netting arm and `(qty - NaN).max(0.0)` is `0.0` — `f64::max`
/// returns the non-NaN operand — so a NaN would vacate the exposure projection to `|position|` and
/// the margin charge to nothing; `+INFINITY` collapses to the same `0.0`.
///
/// `vike_model::AmendSemantics::already_in_position` screens the same values one call away, and that
/// is exactly why the guard is duplicated here: this function is `pub`, so its safety must not
/// depend on which caller reached it.
///
/// # The clamp
///
/// It applies once something has executed: an amend BELOW the executed qty leaves nothing executable
/// (binance rejects such an amend, okx marks the order FILLED), and a negative "remaining" is not
/// merely unhelpful, it is wrong in two contradictory directions at once — the exposure lane
/// computes `|position + side × (negative)|` and UNDER-states the projection, while
/// `vike_model::initial_margin` charges the MAGNITUDE of that negative remainder and
/// `vike_model::is_covered_reduce` is handed a negative qty. Today the venues happen to refuse such
/// an amend server-side; that is THEIR guard, not this one's, and
/// `an_amend_below_the_executed_qty_is_judged_as_a_zero_remainder` pins it here.
#[must_use]
#[inline]
pub fn still_executable(qty: f64, already_executed: f64) -> f64 {
    if already_executed.is_finite() && already_executed > 0.0 {
        (qty - already_executed).max(0.0)
    } else {
        qty
    }
}

/// Pre-trade gate; one per venue/account session. The throttle window is shared across ALL
/// symbols routed through it (a session-level order-rate limit).
pub struct RiskGate {
    pub limits: RiskLimits,
    order_times: VecDeque<i64>,
}

impl RiskGate {
    pub fn new(limits: RiskLimits) -> Self {
        RiskGate { limits, order_times: VecDeque::new() }
    }

    /// True if the order shrinks abs(position): explicit reduce_only, or opposite a non-zero pos.
    fn reduces(request: &OrderRequest, ctx: &RiskContext) -> bool {
        if request.reduce_only {
            return true;
        }
        ctx.position_size != 0.0 && (request.side as f64 * ctx.position_size) < 0.0
    }

    fn deny(reason: &str) -> RiskVerdict {
        RiskVerdict { ok: false, request: None, reason: reason.to_string() }
    }

    /// Book-free gate — exactly [`RiskGate::check_with_book`] with no book, i.e. the impact
    /// knobs are inert. Every pre-existing caller keeps byte-identical behavior.
    pub fn check(&mut self, request: &OrderRequest, ctx: &RiskContext) -> RiskVerdict {
        self.check_with_book(request, ctx, None)
    }

    /// The gate with an optional live L2 book for the order's `(venue, symbol)` — the entry
    /// point a caller that already holds both should use (see the module doc). When the book is
    /// `None`, or both impact knobs are off, NO book work happens and the verdict is identical
    /// to [`RiskGate::check`].
    pub fn check_with_book(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        book: Option<&L2Book>,
    ) -> RiskVerdict {
        self.check_inner(request, ctx, book, true, 0.0)
    }

    /// The gate for an ORDER MODIFICATION — every risk lane [`RiskGate::check`] runs, over the
    /// PROJECTED order (the resting request with the modify's new qty/price folded in), but WITHOUT
    /// consuming a rate-limit slot.
    ///
    /// A modify is not a new order ARRIVAL: the resting order already consumed its slot when it was
    /// submitted, so charging every amend again would make an amend-heavy maker (`vike_mm`'s
    /// `SpreadMaker` requotes continuously) throttle itself out of quoting for doing the one thing
    /// it exists to do. It IS, however, a change to the SIZE of live exposure — which is why every
    /// other lane (halt/reduce-only state, the notional floor and ceiling, projected exposure,
    /// buying power) must still judge it. The same `consume_throttle: false` split
    /// [`RiskGate::check_combo`] already uses for a combo's synthetic per-leg crossings.
    ///
    /// **`already_executed` is how much of `request.qty` has ALREADY executed** and is therefore
    /// already inside `ctx.position_size` — see [`still_executable`] for the full statement, which
    /// lanes consume it and why the notional lanes deliberately do not. `0.0` on every path that is
    /// not an amend of a partially filled order, which is byte-identical to before the parameter
    /// existed. **The caller owns the venue fact**: `vike_model`'s `amend_semantics` says whether
    /// this venue's amend leaves the executions attached (so the amend's qty is a TOTAL and the
    /// executed part must be netted out) or replaces the order wholesale (so it must not) — the gate
    /// stays pure and venue-agnostic.
    ///
    /// Book-free like [`RiskGate::check`]: the impact knobs stay inert, so a venue that never
    /// supplied a book to the submit path is judged identically here.
    pub fn check_modify(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        already_executed: f64,
    ) -> RiskVerdict {
        self.check_inner(request, ctx, None, false, already_executed)
    }

    /// The one gate body. `consume_throttle` is the ONLY behavioral parameter: `true` is the
    /// single-order path (byte-identical to before this split — [`RiskGate::check_with_book`]
    /// passes it unconditionally), `false` runs every check EXCEPT the sliding-window throttle,
    /// which is what a combo's synthetic per-leg crossings need: N legs are ONE venue order and
    /// must consume ONE slot, taken once by [`RiskGate::check_combo`] after all legs pass.
    ///
    /// `already_executed` nets the amend path's double count out of the POSITION-PROJECTING lanes
    /// and nothing else — [`still_executable`] is the authority on which lanes those are, and on why
    /// the notional lanes are deliberately left judging the whole wire qty.
    fn check_inner(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        book: Option<&L2Book>,
        consume_throttle: bool,
        already_executed: f64,
    ) -> RiskVerdict {
        let lim = &self.limits;
        // The part of an order that can still MOVE the position — the only quantity the
        // position-projecting lanes below may use. Byte-identical to the qty passed in whenever
        // nothing has executed; see [`still_executable`].
        let executable = |qty: f64| still_executable(qty, already_executed);

        // side validation — must be exactly +1 or -1
        if request.side != 1 && request.side != -1 {
            return Self::deny("invalid-side");
        }

        // trading state (kill switch) — before anything else.
        //
        // ⚠ `Halted` STOPS OPENING RISK; IT MUST NEVER TRAP YOU IN A POSITION. Until this check
        // carried the `is_covered_reduce` exemption it denied EVERY order, `reduce_only` and
        // `OrderIntent::Flatten` legs included — so `market-exit`, the panic button, ran its
        // mass-cancel and then had every flatten leg come back `OrderDenied`. The operator was left
        // halted WITH the position still open and no way to close it, in exactly the situations
        // that reach `Halted` on their own (`enter_safe_state` after a fold panic, the dead-man's
        // switch). The documented escape was "un-halt, then re-issue the exit" — one more round
        // trip, from a phone, in the dark, with the strategy you just halted running again for the
        // duration. `docs/ops/kill-switches.md` states the law this now obeys: a kill switch must
        // never trap you in a position (which is also why cancels are gated by nothing, anywhere).
        //
        // THE PREDICATE IS `is_covered_reduce`, NOT `request.reduce_only`, AND NOT [`Self::reduces`]
        // — deliberately the STRICTEST of the three the tree has. A client-supplied `reduce_only` is
        // an INTENT, not a proof: it is caller-asserted, and `SimBroker::apply_fill` (the reference
        // for closing-ness in this tree) never consults a flag at all, it reads the ACTUAL position.
        // Trusting the flag here would admit two shapes that OPEN risk under a halt —
        //   * a FLAT-book `reduce_only` order (nothing to reduce ⇒ it is an opening order, and no
        //     venue catches it server-side either — there is no position for the venue to cap it
        //     against), and
        //   * a REVERSAL that overshoots the position and FLIPS through flat (long 2, `reduce_only`
        //     SELL 5 ⇒ short 3 of brand-new exposure).
        // Both are precisely what a halt exists to stop, and both are what a buggy strategy tagging
        // its entries `reduce_only` produces. `is_covered_reduce` requires DIRECTION (the order
        // opposes the position) AND COVERAGE (`|position| >= |qty|`), so what it admits can only
        // shrink `abs(position)` toward zero — it can never cross it. Its implicit arm also means a
        // genuine exit is admitted whether or not anyone remembered the flag.
        //
        // This makes the three states a strict ladder — `Halted` ⊂ `Reducing` ⊂ `Active` — pinned by
        // [`halted_admits_strictly_less_than_reducing_which_admits_less_than_active`]. `Reducing`
        // keeps the looser flag-trusting [`Self::reduces`] on purpose: it is the state you are meant
        // to be able to trade out of, whereas `Halted` is the kill switch.
        //
        // Everything else is unchanged: an admitted reduce faces the REST of this ladder exactly as
        // it does under `Reducing` (throttle, per-order notional cap, projected exposure), and a
        // covered reduce's existing bypasses — collar, min-qty/min-notional floors, buying power —
        // apply for the same anti-stranding reason they always did.
        //
        // The qty this arm judges is the STILL-EXECUTABLE one ([`still_executable`]): a half-done
        // reduce-only EXIT has already shrunk the position by its own executed lots, so comparing
        // the position against the amend's TOTAL made a covered exit stop reading as covered the
        // moment it started working — the kill-switch lane refusing to re-price the very exit it
        // exists to let through. `already_executed` is `0.0` on every non-amend path, so this is
        // `request.qty` verbatim there.
        if ctx.trading_state == TradingState::Halted
            && !vike_model::is_covered_reduce(
                request.reduce_only,
                request.side,
                ctx.position_size,
                executable(request.qty),
            )
        {
            return Self::deny("halted");
        }
        if ctx.trading_state == TradingState::Reducing && !Self::reduces(request, ctx) {
            return Self::deny("reduce-only");
        }

        // reduce-only overshoot (perp) — on the still-executable size, for the same reason the halt
        // arm above is: the executed part of a half-done exit is already OUT of the position it is
        // being compared against, so measuring the total reported an overshoot where the remaining
        // size matched the remaining position exactly.
        if lim.block_reduce_only_overshoot
            && request.reduce_only
            && ctx.position_size.abs() < executable(request.qty).abs()
        {
            return Self::deny("reduce-only-overshoot");
        }

        // normalize: round price to tick, size to lot — on the ORDER SYMBOL's grid.
        //
        // `lim`'s scalars are built from ONE symbol's `SymbolProperties`, so on an engine that
        // admits a second symbol (`extra_symbols`) they are the WRONG grid for it: a coarse mount
        // lot destroys a valid finer-grid order outright (`round_to(0.5, Some(1.0)) == 0.0`),
        // which the `non-positive-size` check below then denies under a reason that names nothing
        // about the real cause. `grid_for` returns the scalars verbatim when no override exists,
        // so every engine today is byte-identical.
        let grid = lim.grid_for(&request.symbol);
        let price = request.price.map(|p| round_to(p, grid.tick_size));
        let qty = round_to(request.qty, grid.lot_size);
        let mut req = request.clone();
        req.price = price;
        req.qty = qty;

        // validity
        if req.qty <= 0.0 {
            return Self::deny("non-positive-size");
        }

        // ONE reduce predicate now gates EVERY bypass in this ladder — the min-qty/min-notional
        // floors here, buying power and the impact veto below all read `covered_reduce`.
        //
        // THE LOOSE ARM (`reduce_only || is_implicit_reduce`) IS GONE. It used to carry the margin
        // and impact bypasses, on the rationale "perp venues enforce reduce-only server-side".
        // THAT RATIONALE DOES NOT HOLD ON A FLAT BOOK: with no position there is nothing for the
        // venue to reduce, so nothing server-side catches it either — and `is_implicit_reduce` is
        // false when position == 0 (it requires side * position < 0), so the bare caller-asserted
        // FLAG was the only thing admitting those bypasses. Net effect: an order mis-tagged
        // `reduce_only` (a strategy bug, or a UI path defaulting the flag) was admitted at ANY size
        // against ANY equity with the buying-power check skipped entirely. The opt-in
        // `block_reduce_only_overshoot` knob does catch that shape (`|position| < |qty|` includes
        // position == 0) but it DEFAULTS OFF, so it was not covering this.
        // The FLOOR bypass is stricter: it trusts only POSITION-COVERED reduces. The reduce_only
        // FLAG alone is caller-asserted — `SimBroker::apply_fill` (the reference for this rule)
        // never consults a flag, it derives closing-ness from the ACTUAL position — so a flat-book
        // "reduce_only" order is an OPENING order and must stay floor-gated; otherwise a buggy
        // strategy tagging entries reduce_only would put sub-floor opening orders on the wire.
        //
        // ⚠ IT IS THE STILL-EXECUTABLE SIZE THAT IS JUDGED, not the wire qty ([`still_executable`]).
        // Coverage asks "can the REST of this order only shrink the position?", and an amend's
        // already-executed lots left the position when they executed — counting them again made a
        // half-done exit read as an overshoot of the very position it had just been shrinking. On
        // every non-amend path `exec_qty` IS `req.qty`, so nothing else moves.
        let exec_qty = executable(req.qty);
        let covered_reduce =
            vike_model::is_covered_reduce(req.reduce_only, req.side, ctx.position_size, exec_qty);

        // THE HALT RE-CHECK, ON THE NORMALIZED SIZE. The kill-switch arm at the top of this fn had
        // to judge the RAW `request.qty` — it runs before the grid is even resolved — and lot
        // rounding is half-to-EVEN, so it can round a qty UP: with a 1.0 lot, a 1.6 order against a
        // 1.8 position is covered raw (1.8 >= 1.6) but becomes 2.0 on the wire and FLIPS the
        // position short 0.2. That is opening risk under a halt, which is the one thing this
        // exemption must not admit, so the verdict is re-taken against the size that will actually
        // be sent. Free: `covered_reduce` is already computed for the bypasses below, and the whole
        // check is one enum compare on a path that is not `Halted` in any normal run.
        //
        // The reason string stays `"halted"` rather than falling through to whatever later lane
        // would catch it, so an operator reading an `OrderDenied` is told the halt refused them.
        //
        // ⚠ THIS ONE IS THE AUTHORITY, and the arm at the top of this fn is DELIBERATE REDUNDANCY —
        // stated because a mutation test proved you cannot tell them apart from behaviour alone.
        // Weakening the early arm to trust `request.reduce_only` changes NO verdict, because this
        // check re-decides on the real predicate afterwards; the early arm exists to (a) refuse the
        // overwhelmingly common opening-order-under-halt with the reason `"halted"` rather than
        // whatever intervening lane happens to fire first (`reduce-only-overshoot`,
        // `non-positive-size`), and (b) keep the refusal ahead of every side-effecting lane, so a
        // halted order can never consume a throttle slot even if one moves above this point.
        //
        // So: if you are simplifying, DELETE THE EARLY ARM, never this one — dropping this one
        // reopens the lot-rounding flip that `halted_refuses_a_reduce_whose_lot_rounding_would_flip_
        // the_position` catches, while dropping the early arm only changes a deny reason.
        if ctx.trading_state == TradingState::Halted && !covered_reduce {
            return Self::deny("halted");
        }

        // ---- fat-finger PRICE COLLAR (OPT-IN, RUST-NATIVE; see the module doc) ----
        // Placed HERE, immediately after `covered_reduce` and BEFORE every price-derived axis:
        // a mis-scaled price also distorts notional/exposure/margin, so without this the operator
        // would read `over-max-notional` (or, on a 10× DOWN-scale, nothing at all) instead of the
        // real cause. It is also before the throttle, like the margin and impact vetoes — a denied
        // order must never consume a rate slot. UNARMED (`collar_for` → None, the default) this is
        // one map lookup on an empty map plus an `Option` check, and no comparison happens at all.
        //
        // A COVERED REDUCE BYPASSES ENTIRELY — the same anti-stranding rule the floors, buying
        // power and the impact veto below follow, and here it is load-bearing rather than merely
        // kind: `vike_model::build_bracket` emits its stop-loss and take-profit legs as
        // `reduce_only` orders whose `trigger_price`/`price` is DELIBERATELY far from the mark
        // (that distance IS the protection). Collaring them would veto exactly the order that
        // limits the loss. An UNCOVERED order opens exposure whatever the caller tagged it, so it
        // is collared like any opening order.
        //
        // A COMBO's `price` is the SIGNED NET across legs, not a price in the mark's units
        // (negative for a credit structure), so it is never collared here; `check_combo` clears
        // each synthetic leg's price/trigger, so legs carry nothing to collar either. An UNPRICED
        // mark skips: the mount's readiness gate guarantees priced-before-order-flow, so a deny
        // here would be a pure false positive.
        if let Some(collar) = lim.collar_for(&req.symbol) {
            let mark = ctx.mark_price;
            if !covered_reduce && req.combo_legs.is_empty() && mark.is_finite() && mark > 0.0 {
                let band = collar.band(mark);
                // The TRIGGER is tick-rounded for the comparison exactly as `req.price` already
                // was above, so a trigger and the equivalent limit within half a tick of the band
                // edge get the SAME verdict. `req.trigger_price` itself is NOT mutated — only
                // `price`/`qty` are normalized on the wire-bound clone.
                let trig = req.trigger_price.map(|p| round_to(p, lim.tick_size));
                // Symmetric: a price 10× ABOVE the mark and one 10× BELOW are the same fat finger.
                // A non-finite price is denied outright rather than compared — `NaN > band` is
                // false, so a NaN would otherwise be admitted by the very axis meant to catch a
                // garbage price. A market order carries NEITHER price and is skipped whole.
                let outside = |p: f64| !p.is_finite() || (p - mark).abs() > band;
                if req.price.is_some_and(outside) || trig.is_some_and(outside) {
                    return Self::deny("price-collar");
                }
            }
        }

        // The below-min floors gate OPENING/increasing orders ONLY. A covered reduce/close is
        // exempt — the same anti-stranding rule the margin and impact bypasses below follow, and
        // the same standing rule as `SimBroker::apply_fill` ("a closing fill must ALWAYS execute
        // so a position is never stranded below-min"): a dust flatten under the venue floor must
        // reach the venue — which may still reject it, but that is the venue's call, not a local
        // strand. Denying it here filled in backtest yet denied live, stranding exactly the
        // position the rule protects.
        //
        // KNOWN RESIDUAL DIVERGENCE (deliberate, gate on the conservative side): a below-min
        // REVERSAL (|qty| > |position|, flipping through flat) is NOT a covered reduce — it opens
        // the far side — so the gate denies it while `SimBroker::apply_fill` executes a flip
        // whole. The capped flatten (qty ≤ |position|) is admitted, so nothing is stranded; the
        // eventual unification is SimBroker-side (split a flip and floor-gate its opening half).
        if let Some(min_qty) = grid.min_qty {
            if !covered_reduce && req.qty.abs() < min_qty {
                return Self::deny("below-min-qty");
            }
        }
        // stop orders carry price=None + trigger_price; use trigger before mark
        let ref_price = req.price.or(req.trigger_price).unwrap_or(ctx.mark_price);
        // NOTIONAL IS A MAGNITUDE, so `ref_price` enters it ABSOLUTE. Ordinary instruments quote
        // >= 0 and `.abs()` is a no-op for them (every pre-existing verdict is byte-identical);
        // a COMBO's `price` is the SIGNED net (`ComboSpec::net_limit`) and goes NEGATIVE for a
        // credit structure. Without the abs, a credit combo's notional is negative, which trips
        // `notional < min_notional` (denying EVERY credit combo on the live path, where
        // `min_notional` comes from fetched SymbolProperties) while `notional > cap` can never
        // trip — so an arbitrarily large credit combo would escape the per-order cap entirely.
        //
        // THE CONTRACT MULTIPLIER IS PART OF THE NOTIONAL, exactly as in the margin calc below
        // (`vike_model::initial_margin(.., ctx.multiplier, ..)`) and in `SimBroker::apply_fill`
        // (`rounded * price * multiplier`). Omitting it made the same `min_notional` gate
        // differently live vs backtest for multiplier != 1 instruments (options, inverse perps) —
        // and inconsistently WITHIN this gate vs its own margin check. `ctx.multiplier` defaults
        // to 1.0, so every multiplier-1 verdict is bit-identical (`x * 1.0` is an IEEE-754 no-op).
        // The multiplier enters ABSOLUTE like the other two factors — notional is a magnitude, and
        // a signed factor would make the floor spuriously trip while the cap could never trip.
        let notional = vike_model::order_notional(req.qty, ref_price, ctx.multiplier);
        if let Some(min_notional) = grid.min_notional {
            if !covered_reduce && notional < min_notional {
                return Self::deny("below-min-notional");
            }
        }

        // per-order notional cap
        if let Some(cap) = lim.max_notional_per_order {
            if notional > cap {
                return Self::deny("over-max-notional");
            }
        }

        // projected exposure cap — ONE SYMBOL at ONE VENUE, not the account. `ctx.position_size`
        // is the order symbol's own bucket (`ExecutionEngine::gate_position_size`) and this gate
        // holds no cross-symbol book, so a position in any OTHER symbol contributes nothing here;
        // `RiskLimits::max_total_exposure`'s doc carries the full statement and the reason the
        // name stays. Pinned by `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
        // `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`.
        //
        // (valued at ctx.mark_price — intentional). THE CONTRACT MULTIPLIER
        // IS PART OF THE NOTIONAL (exactly as `order_notional`/`min_notional` above and
        // `initial_margin` below already fold it in, and as `SimBroker::apply_fill` values a
        // position `qty * price * multiplier`). Omitting it here measured this one lane's exposure
        // differently from its own min_notional/per-order-cap and margin siblings for a
        // multiplier != 1 instrument (options, inverse perps) — under-counting projected exposure,
        // so this is a risk-TIGHTENING correction. `ctx.multiplier` defaults to 1.0, so every
        // multiplier-1 verdict is bit-identical (`x * 1.0` is an IEEE-754 no-op).
        //
        // THE PROJECTION IS OVER `exec_qty`, NOT THE WIRE QTY ([`still_executable`]) — this is the
        // lane the amend double count was widest in: `position` already holds the amended order's
        // own executed lots, so `position + total` measured them twice and refused an amend at a cap
        // that the very same order had been ADMITTED under at submit.
        if let Some(cap) = lim.max_total_exposure {
            let projected = (ctx.position_size + req.side as f64 * exec_qty).abs()
                * ctx.mark_price
                * ctx.multiplier;
            if projected > cap {
                return Self::deny("over-max-exposure");
            }
        }

        // pre-trade buying-power check (RUST-NATIVE, Phase B; LEAN BuyingPowerModel
        // semantics). Runs BEFORE the throttle so a denied order never consumes a rate slot.
        //
        // A COVERED reduce/close bypasses entirely (`covered_reduce`, the same predicate the min
        // floors use — NOT the looser flag arm). Anti-stranding is preserved exactly: an order the
        // position actually covers frees margin rather than consuming it, so charging buying power
        // for it could deny the very exit that releases the margin. But an UNCOVERED order — flat
        // book, or a reversal overshooting the position — OPENS exposure no matter what the caller
        // tagged it, and must face buying power like any opening order. `SimBroker` (the reference)
        // has no `reduce_only` concept at all and derives closing-ness from the ACTUAL position;
        // this aligns the live gate's margin lane with that, as #458 already did for the floors.
        if let Some(im_req) = lim.im_for(&req.symbol) {
            if !covered_reduce {
                // `exec_qty`, not the wire qty ([`still_executable`]): `ctx.margin_used` is already
                // funding the amended order's executed lots as POSITION, so charging the whole
                // total again funded the same lots twice and refused an amend the account could
                // plainly afford.
                let order_margin = vike_model::initial_margin(
                    ref_price,
                    req.side as f64 * exec_qty,
                    ctx.multiplier,
                    1.0,
                    im_req,
                );
                let free = vike_model::free_buying_power(
                    ctx.equity,
                    ctx.margin_used,
                    ctx.closing_credit,
                    lim.required_free_bp_pct,
                );
                if !vike_model::has_sufficient_margin(free, order_margin) {
                    return Self::deny("insufficient-margin");
                }
            }
        }

        // pre-trade market-impact veto (OPT-IN; RUST-NATIVE). Runs BEFORE the throttle for the
        // same reason the margin check does: a denied order must not consume a rate slot. An
        // unarmed gate does NO book work: `require_fillable` is false, and `impact_veto` returns
        // on `budget?` before walking anything.
        // A COVERED reduce/close NEVER impact-vetoes — same bypass as the margin check above, and
        // the same standing rule as `SimBroker::apply_fill` ("closing fills always execute —
        // never strand a position"). A flatten during a liquidity vacuum is exactly when the
        // book looks worst and exactly when the exit must go through.
        //
        // This moved off the loose flag arm together with the margin check, rather than being left
        // behind as a third semantics for the same predicate. The anti-stranding rationale is what
        // justifies the bypass at all, and it is a statement about a POSITION: with no position
        // there is nothing to strand, so on a flat book the bypass protects nothing and only lets a
        // mis-tagged opening order skip the veto. Blast radius is small — the veto is opt-in and
        // does no book work unless armed. Same KNOWN RESIDUAL as the floors above: an uncovered
        // REVERSAL is now vetoable as a whole, including its closing half; the gate deliberately
        // sits on the conservative side, and the capped flatten (qty <= |position|) still bypasses,
        // so no position can be stranded.
        if let Some(b) = book {
            if !covered_reduce && (lim.require_fillable || lim.max_slippage_bps.is_some()) {
                // `exec_qty` for the same reason as the lanes above — only the still-executable part
                // can take liquidity. BYTE-IDENTICAL today: `check_modify` passes no book, so the
                // only callers that reach here are submit-path ones with nothing executed. It is
                // spelled correctly anyway, so that wiring a book into the amend path later cannot
                // silently reintroduce the double count in this one lane.
                let scope = take_scope(b, req.side, &req.order_type, req.price);
                if let Some(d) = scoped_impact_veto(
                    b,
                    scope,
                    req.side,
                    exec_qty.abs(),
                    lim.max_slippage_bps,
                    lim.require_fillable,
                ) {
                    return Self::deny(d.as_str());
                }
            }
        }

        // sliding-window throttle (only accepted orders consume a slot)
        if consume_throttle && !self.admit_throttle(ctx.now_ms) {
            return Self::deny("rate-limited");
        }

        RiskVerdict { ok: true, request: Some(req), reason: String::new() }
    }

    /// Sliding-window throttle: evict expired stamps, then admit (and record) one order.
    /// `true` = admitted. Unarmed (`max_orders_per_window == None`) always admits and records
    /// nothing — the pre-split behavior verbatim.
    fn admit_throttle(&mut self, now_ms: i64) -> bool {
        let Some(max_orders) = self.limits.max_orders_per_window else { return true };
        let cutoff = now_ms - self.limits.window_ms;
        while self.order_times.front().is_some_and(|&t| t <= cutoff) {
            self.order_times.pop_front();
        }
        if self.order_times.len() >= max_orders {
            return false;
        }
        self.order_times.push_back(now_ms);
        true
    }

    /// ATOMIC per-leg crossing for a COMBO order (combo-orders spec §5, conservative v1).
    ///
    /// A combo is ONE venue order carrying N legs (`OrderRequest::combo_legs`, `price` = the
    /// SIGNED net limit — NEGATIVE for a credit structure such as a short condor). The gate has
    /// no combo grid and no strategy-aware margin, so v1 prices the RISK off each leg's OWN mark:
    ///
    /// * every leg is crossed as a synthetic single-leg request — qty `|ratio| × combo_qty`, side
    ///   `sign(ratio) × combo_side`, `price`/`trigger_price` cleared so the leg's notional prices
    ///   off `leg_ctx(symbol).mark_price` (the stop-order convention already in `check`);
    /// * **ALL legs must pass.** The FIRST failure denies the WHOLE combo with ONE verdict whose
    ///   reason names the failing leg (`"leg BTC-…-C: below-min-qty"`); no partial admission
    ///   exists, matching the atomic-reject FSM contract (spec §2);
    /// * the combo consumes **ONE** throttle slot, taken only after every leg passes (a denied
    ///   combo never burns a rate slot — the same rule the margin/impact vetoes follow);
    /// * the NET limit is deliberately NOT used for notional. A credit combo prices its risk
    ///   negative, and a defined-risk spread is DOUBLE-COUNTED as naked legs ON PURPOSE: until
    ///   position-group margin lands, a combo must never pass a gate its naked legs would fail.
    ///
    /// **The legs ACCUMULATE, so "as conservative as N sequential naked orders" is literally
    /// true and not merely asserted.** `leg_ctx` is a per-symbol snapshot of the account BEFORE
    /// the combo, and it cannot know what earlier legs already committed, so the loop threads the
    /// commitment itself:
    ///
    /// * **buying power** — each admitted, non-reducing leg's initial margin (computed with the
    ///   SAME [`vike_model::initial_margin`] the gate body uses, off the gate's own normalized
    ///   qty) is added to the next leg's `margin_used`. Without this, N legs each fit in the same
    ///   unchanged free BP and the combo consumes N× what one leg was allowed;
    /// * **exposure / reduce-only** — each admitted leg's signed size is folded into a projected
    ///   per-symbol position, so a later leg on the SAME symbol sees the earlier one
    ///   (`max_total_exposure` no longer double-measures a repeated symbol, which
    ///   `ComboSpec::validate` does not reject, and `reduces()` judges the projected book).
    ///
    /// Account-level facts (`trading_state`, `now_ms`) are taken from `ctx` and OVERRIDE whatever
    /// `leg_ctx` returns: a `Reducing` account must not be laundered into `Active` by a caller
    /// closure that only fills per-symbol fields.
    ///
    /// TODO(spec §5 / steal-list C3): position-group (strategy-aware) margin — recognizing that a
    /// defined-risk spread's true requirement is LESS than the sum of its naked legs — is a
    /// SEPARATE later epic. Not implemented here on purpose: this gate errs strictly high.
    ///
    /// `ctx` is the ACCOUNT-level context (trading state + the throttle clock); `leg_ctx` supplies
    /// each leg's own mark/position/margin context. A leg whose mark is missing (`0.0`), negative
    /// or NaN is DENIED (`"leg X: no-mark"`) rather than priced at zero — the whole design stakes
    /// itself on each leg pricing off its own mark, and a zero mark silently vacates every
    /// price-based limit (notional cap, exposure, buying power) at once. The single-order
    /// [`RiskGate::check`] path is untouched by this entry point.
    ///
    /// The returned request on success is the combo request VERBATIM (leg rounding is a per-leg
    /// concern and the net price is formatted to the combo instrument's tick by the adapter — the
    /// one pinned Decimal site; the gate never rounds a net price it has no grid for).
    pub fn check_combo<F>(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        leg_ctx: F,
    ) -> RiskVerdict
    where
        F: Fn(&str) -> RiskContext,
    {
        // the "this is NOT a combo" sentinel (`build_combo` can never produce it)
        if request.combo_legs.is_empty() {
            return Self::deny("not-a-combo");
        }
        if request.side != 1 && request.side != -1 {
            return Self::deny("invalid-side");
        }
        if ctx.trading_state == TradingState::Halted {
            return Self::deny("halted");
        }
        // NaN/INFINITY-safe: neither is a usable size. INFINITY is NOT caught by `<= 0.0`, and an
        // infinite leg qty yields an infinite notional that sails through every UNARMED cap (the
        // default), so require finiteness explicitly rather than only rejecting NaN.
        if !request.qty.is_finite() || request.qty <= 0.0 {
            return Self::deny("non-positive-size");
        }

        // running commitment of the legs admitted SO FAR (see the doc above): margin already
        // spoken for, and the projected signed position per symbol.
        let mut committed_margin = 0.0f64;
        let mut projected: IndexMap<&str, f64> = IndexMap::new();

        for leg in &request.combo_legs {
            // `ComboSpec::validate` rejects ratio 0, but a hand-built request can carry it. Name
            // the real cause: without this it surfaces as the misleading `invalid-side` (side
            // `signum()` of 0 is 0) from inside the per-leg check.
            if leg.ratio == 0 {
                return Self::deny(&format!("leg {}: zero-ratio", leg.symbol));
            }

            let base = leg_ctx(&leg.symbol);
            // `leg_ctx` is TOTAL — it has no way to say "I have no mark for this symbol" and
            // returns 0.0. A 0.0 (or NaN) mark makes notional 0, exposure 0 and initial_margin 0,
            // i.e. EVERY price-based limit passes at any size. Refuse instead of substituting a
            // price that does not exist.
            if !base.mark_price.is_finite() || base.mark_price <= 0.0 {
                return Self::deny(&format!("leg {}: no-mark", leg.symbol));
            }
            // account-level facts are authoritative: ctx owns the trading state and the throttle
            // clock, `leg_ctx` owns only per-symbol facts (doc above). A caller closure built from
            // `RiskContext::default()` must NOT be able to downgrade `Reducing`/`Halted` to
            // `Active`. Per-symbol fields carry the running commitment of the earlier legs.
            let pos_before =
                base.position_size + projected.get(leg.symbol.as_str()).copied().unwrap_or(0.0);
            let lctx = RiskContext {
                trading_state: ctx.trading_state,
                now_ms: ctx.now_ms,
                position_size: pos_before,
                margin_used: base.margin_used + committed_margin,
                ..base
            };

            let mut leg_req = request.clone();
            leg_req.symbol = leg.symbol.clone();
            // sign law: selling the combo flips every leg (spec §1).
            leg_req.side = request.side.signum() * leg.ratio.signum();
            leg_req.qty = f64::from(leg.ratio.unsigned_abs()) * request.qty;
            // the leg prices off its OWN mark: never off the (possibly negative) net.
            leg_req.price = None;
            leg_req.trigger_price = None;
            // a synthetic leg is a plain single-leg order, not a nested combo
            leg_req.combo_legs = Vec::new();
            // reduce_only is a PER-LEG fact and must never be inherited from the combo: a
            // reduce_only combo whose legs open brand-new positions would otherwise launder every
            // one of them past the buying-power check, the impact veto and the `Reducing` gate
            // (`pure_reduce` is `req.reduce_only || ...`). Derive it from the leg's own projected
            // book instead — exactly `RiskGate::reduces`' second clause.
            leg_req.reduce_only = pos_before != 0.0 && (f64::from(leg_req.side) * pos_before) < 0.0;

            // `0.0`: a combo leg is synthesized fresh from the combo request, never from a resting
            // partially-filled order, so no part of a leg's qty has already executed.
            let v = self.check_inner(&leg_req, &lctx, None, false, 0.0);
            let Some(admitted) = v.request.filter(|_| v.ok) else {
                return Self::deny(&format!("leg {}: {}", leg.symbol, v.reason));
            };

            // fold this leg into the running commitment, off the gate's OWN normalized request
            // (lot-rounded qty) and the SAME margin fn `check_inner` used — one authority, not a
            // second formula. Reducing legs commit nothing (they are the margin bypass).
            let signed_qty = f64::from(admitted.side) * admitted.qty;
            let pure_reduce = admitted.reduce_only
                || (signed_qty * pos_before < 0.0 && pos_before.abs() >= admitted.qty.abs());
            if !pure_reduce {
                if let Some(im_req) = self.limits.im_for(&admitted.symbol) {
                    committed_margin += vike_model::initial_margin(
                        lctx.mark_price,
                        signed_qty,
                        lctx.multiplier,
                        1.0,
                        im_req,
                    )
                    .abs();
                }
            }
            *projected.entry(leg.symbol.as_str()).or_insert(0.0) += signed_qty;
        }

        // ONE slot for the whole combo, and only once every leg passed.
        if !self.admit_throttle(ctx.now_ms) {
            return Self::deny("rate-limited");
        }

        RiskVerdict { ok: true, request: Some(request.clone()), reason: String::new() }
    }

    /// Throttle-window state for the journal snapshot (replay determinism on throttle denials).
    pub fn throttle_times(&self) -> Vec<i64> {
        self.order_times.iter().copied().collect()
    }
    pub fn set_throttle_times(&mut self, times: Vec<i64>) {
        self.order_times = times.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(side: i32, qty: f64) -> OrderRequest {
        OrderRequest {
            client_order_id: "t1".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            order_type: "market".to_string(),
            side,
            qty,
            ..Default::default()
        }
    }

    // ---- per-symbol price/size grid ----

    fn market_in(symbol: &str, side: i32, qty: f64) -> OrderRequest {
        OrderRequest { symbol: symbol.to_string(), ..market(side, qty) }
    }

    /// THE BUG THE GRID FIXES. An engine's scalars come from ONE symbol's `SymbolProperties`, so a
    /// coarse mount lot is applied to a DIFFERENT symbol's order too: `round_to(0.5, Some(1.0))`
    /// is `0.0`, and the gate then denies a perfectly valid order as `"non-positive-size"` — a
    /// reason that names nothing about the real cause.
    #[test]
    fn a_coarse_mount_lot_destroys_a_finer_grid_order_without_an_override() {
        let lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
        let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
        assert!(!v.ok, "0.5 rounds to 0.0 on a lot of 1.0");
        assert_eq!(v.reason, "non-positive-size");
    }

    /// With ITS OWN grid declared, the same order survives: rounded onto 0.001 rather than 1.0.
    #[test]
    fn a_declared_symbol_is_rounded_onto_its_own_lot() {
        let mut lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
        lim.grid_by_symbol.insert(
            "ETHUSDT".to_string(),
            SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
        );
        let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
        assert!(v.ok, "the finer lot must admit it: {v:?}");
        let admitted = v.request.expect("an admitted verdict carries the request");
        assert!((admitted.qty - 0.5).abs() < 1e-12, "qty {} != 0.5", admitted.qty);
    }

    /// An override is per SYMBOL, not global: the engine's own symbol keeps the scalars.
    #[test]
    fn an_override_does_not_leak_to_other_symbols() {
        let mut lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
        lim.grid_by_symbol.insert(
            "ETHUSDT".to_string(),
            SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
        );
        let v = RiskGate::new(lim).check(&market(1, 0.5), &RiskContext::default());
        assert!(!v.ok, "BTCUSDT still rounds on the 1.0 scalar");
    }

    /// Fallback is FIELD-BY-FIELD: an override that pins only `lot_size` must still inherit the
    /// engine's `min_qty`. A half-specified grid must not become a way to switch a floor off.
    #[test]
    fn a_partial_override_still_inherits_the_engines_floors() {
        let mut lim = RiskLimits { lot_size: Some(1.0), min_qty: Some(10.0), ..RiskLimits::new() };
        lim.grid_by_symbol.insert(
            "ETHUSDT".to_string(),
            SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
        );
        let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
        assert!(!v.ok, "0.5 clears the finer lot but not the inherited min_qty of 10");
        assert_eq!(v.reason, "below-min-qty");
    }

    /// A symbol's own floors apply when it declares them.
    #[test]
    fn a_declared_symbol_uses_its_own_min_qty() {
        let mut lim = RiskLimits { lot_size: Some(1.0), min_qty: Some(10.0), ..RiskLimits::new() };
        lim.grid_by_symbol.insert(
            "ETHUSDT".to_string(),
            SymbolGrid { lot_size: Some(0.001), min_qty: Some(0.1), ..SymbolGrid::default() },
        );
        let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
        assert!(v.ok, "its own 0.1 floor admits 0.5: {v:?}");
    }

    /// `SymbolGrid::from_properties` and `RiskLimits::from_properties` must read ONE
    /// `SymbolProperties` the same way — otherwise a mount's own symbol and its declared leg would
    /// be judged on two different readings of the same venue payload, which is a worse failure than
    /// the missing-grid one the map exists to fix.
    ///
    /// NON-VACUOUS: the four fields carry four DISTINCT values, so a swapped pair (the realistic
    /// drift — `step_size` feeds `lot_size`, not `tick_size`) fails; a wholesale `Default` in either
    /// builder fails; and adding a fifth mapped field to one builder alone fails as soon as it is
    /// asserted here. An all-zero fixture would pass against almost any wrong mapping, so the
    /// values are deliberately unequal.
    #[test]
    fn a_symbol_grid_matches_the_scalar_builder_field_for_field() {
        use vike_model::SymbolProperties;
        let f = SymbolProperties {
            tick_size: 0.5,
            step_size: 0.1,
            min_qty: 0.01,
            min_notional: 5.0,
            ..Default::default()
        };
        let scalars = RiskLimits::from_properties(&f);
        let g = SymbolGrid::from_properties(&f);
        assert_eq!(g.tick_size, scalars.tick_size);
        assert_eq!(g.lot_size, scalars.lot_size);
        assert_eq!(g.min_qty, scalars.min_qty);
        assert_eq!(g.min_notional, scalars.min_notional);
        // …and the mapping itself, spelled out, so this cannot pass by both builders being wrong
        // in the same direction.
        assert_eq!(
            (g.tick_size, g.lot_size, g.min_qty, g.min_notional),
            (Some(0.5), Some(0.1), Some(0.01), Some(5.0))
        );
    }

    /// THE DECLARED RESIDUAL (see [`SymbolGrid::from_properties`]'s ⚠): a venue field of `0.0`
    /// means UNCONSTRAINED, but `nz_step` folds it to `None` and `None` in a `SymbolGrid` means
    /// INHERIT — so this leg still rounds on the mounted symbol's lot. Pinned, not fixed: closing it
    /// needs a spelling for "explicitly unconstrained", which changes the serialized `RiskLimits`
    /// shape that feeds the journal determinism fence.
    ///
    /// NON-VACUOUS: it asserts the INHERITED `0.001`, not merely `is_none()` on the override — a
    /// future `from_properties` that mapped `0.0` to a real "no rounding" answer would return
    /// `None` from `grid_for` here and fail, which is exactly the signal wanted if somebody closes
    /// this without deleting the pin.
    #[test]
    fn a_zero_field_from_the_venue_inherits_the_mount_scalar() {
        use vike_model::SymbolProperties;
        let mut lim = RiskLimits { lot_size: Some(0.001), ..RiskLimits::new() };
        // the venue publishes NO lot for this leg
        let leg = SymbolProperties { tick_size: 0.01, step_size: 0.0, ..Default::default() };
        lim.grid_by_symbol.insert("ETHUSDT".to_string(), SymbolGrid::from_properties(&leg));
        let g = lim.grid_for("ETHUSDT");
        assert_eq!(g.tick_size, Some(0.01), "the leg's own tick is applied");
        assert_eq!(
            g.lot_size,
            Some(0.001),
            "an unconstrained leg lot INHERITS the mount scalar — the declared residual"
        );
    }

    /// An EMPTY map is the identity: every verdict is exactly what it was before the grid existed.
    #[test]
    fn an_empty_grid_map_is_byte_identical() {
        let lim = RiskLimits { lot_size: Some(0.01), min_qty: Some(0.05), ..RiskLimits::new() };
        assert!(lim.grid_by_symbol.is_empty());
        let g = lim.grid_for("ANYTHING");
        assert_eq!(g.lot_size, lim.lot_size);
        assert_eq!(g.tick_size, lim.tick_size);
        assert_eq!(g.min_qty, lim.min_qty);
        assert_eq!(g.min_notional, lim.min_notional);
    }

    #[test]
    fn from_filters_maps_with_zero_as_none() {
        use vike_model::SymbolProperties;
        let f = SymbolProperties {
            tick_size: 0.5,
            step_size: 0.1,
            min_qty: 0.01,
            min_notional: 5.0,
            ..Default::default()
        };
        let l = RiskLimits::from_properties(&f);
        assert_eq!(l.tick_size, Some(0.5));
        assert_eq!(l.lot_size, Some(0.1)); // step_size -> lot_size
        assert_eq!(l.min_qty, Some(0.01));
        assert_eq!(l.min_notional, Some(5.0));
        // all-0.0 -> all None
        let z = RiskLimits::from_properties(&SymbolProperties::default());
        assert_eq!((z.tick_size, z.lot_size, z.min_qty, z.min_notional), (None, None, None, None));
        assert_eq!(z.window_ms, 1000); // inherits RiskLimits::new() defaults
    }

    #[test]
    fn check_rejects_below_min_qty() {
        let mut gate = RiskGate::new(RiskLimits { min_qty: Some(1.0), ..RiskLimits::new() });
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };

        let verdict = gate.check(&market(1, 0.5), &ctx);
        assert!(!verdict.ok && verdict.reason == "below-min-qty", "got {:?}", verdict);

        let verdict_ok = gate.check(&market(1, 1.0), &ctx);
        assert!(verdict_ok.ok || verdict_ok.reason != "below-min-qty", "got {:?}", verdict_ok);
    }

    /// LIVE-VS-BACKTEST DIVERGENCE FIX (finding A): a pure reduce/close below `min_qty` was
    /// DENIED live while `SimBroker::apply_fill` fills it in backtest ("a closing fill must
    /// ALWAYS execute so a position is never stranded below-min") — stranding exactly the
    /// position that rule protects. The gate now applies its own `pure_reduce` bypass — the one
    /// margin and impact already used — to the min floors. A NON-reducing below-min order still
    /// denies exactly as before.
    #[test]
    fn pure_reduce_dust_flatten_passes_min_qty_gate() {
        let lim = || RiskLimits { min_qty: Some(0.01), ..RiskLimits::new() };
        // long 0.005 — dust below the 0.01 floor — flattened with a sell of 0.005
        let long_dust =
            RiskContext { mark_price: 100.0, position_size: 0.005, ..RiskContext::default() };
        let mut req = market(-1, 0.005);
        req.reduce_only = true;
        let v = RiskGate::new(lim()).check(&req, &long_dust);
        assert!(v.ok, "an explicit reduce_only dust flatten must pass the min-qty floor: {v:?}");
        // the implicit form (opposite a position that fully covers it) too
        let v = RiskGate::new(lim()).check(&market(-1, 0.005), &long_dust);
        assert!(v.ok, "an implicit dust close must pass the min-qty floor: {v:?}");
        // the SAME order with no reduce intent (flat account) still denies exactly as today
        let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let v = RiskGate::new(lim()).check(&market(-1, 0.005), &flat);
        assert!(!v.ok && v.reason == "below-min-qty", "opening dust must still deny: {v:?}");
        // and a REVERSAL (qty > |position|) is NOT a pure reduce — it opens the far side
        let v = RiskGate::new(lim()).check(&market(-1, 0.008), &long_dust);
        assert!(!v.ok && v.reason == "below-min-qty", "a below-min reversal must deny: {v:?}");
        // the reduce_only FLAG alone buys nothing at the floor: with a FLAT book it is an
        // OPENING order (SimBroker derives closing-ness from the position, never a flag) —
        // a buggy strategy tagging entries reduce_only must not put sub-floor orders on the wire
        let mut flagged_open = market(-1, 0.005);
        flagged_open.reduce_only = true;
        let v = RiskGate::new(lim()).check(&flagged_open, &flat);
        assert!(
            !v.ok && v.reason == "below-min-qty",
            "flat-book reduce_only must stay floor-gated: {v:?}"
        );
    }

    /// Finding A, min-notional twin: same anti-stranding exemption, same non-reduce pin.
    #[test]
    fn pure_reduce_dust_flatten_passes_min_notional_gate() {
        let lim = || RiskLimits { min_notional: Some(5.0), ..RiskLimits::new() };
        // 0.02 @ mark 100 = 2.0 notional, under the 5.0 floor
        let long_dust =
            RiskContext { mark_price: 100.0, position_size: 0.02, ..RiskContext::default() };
        let mut req = market(-1, 0.02);
        req.reduce_only = true;
        let v = RiskGate::new(lim()).check(&req, &long_dust);
        assert!(v.ok, "a reduce_only dust flatten must pass the min-notional floor: {v:?}");
        let v = RiskGate::new(lim()).check(&market(-1, 0.02), &long_dust);
        assert!(v.ok, "an implicit dust close must pass the min-notional floor: {v:?}");
        // no reduce intent ⇒ denies exactly as today
        let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let v = RiskGate::new(lim()).check(&market(-1, 0.02), &flat);
        assert!(!v.ok && v.reason == "below-min-notional", "opening dust must still deny: {v:?}");
        // a reversal (0.03 > the 0.02 position) is not a pure reduce ⇒ still denied
        let v = RiskGate::new(lim()).check(&market(-1, 0.03), &long_dust);
        assert!(!v.ok && v.reason == "below-min-notional", "reversal must deny: {v:?}");
    }

    /// B9 FIX (fail-before/pass-after): the `max_total_exposure` lane folds `ctx.multiplier` into
    /// its projected exposure, exactly like its `min_notional`/per-order-cap and `initial_margin`
    /// siblings in the SAME gate. For a multiplier != 1 instrument (options, inverse perps) the
    /// pre-fix line under-counted projected exposure by the multiplier factor. Multiplier-1 stays
    /// bit-identical, so this test arms a multiplier of 10 to prove the fold is present.
    #[test]
    fn max_total_exposure_includes_the_contract_multiplier() {
        // flat book, BUY 2 @ mark 100 with a x10 multiplier ⇒ projected exposure = 2 * 100 * 10 =
        // 2_000. Pre-fix the multiplier was dropped ⇒ projected = 200.
        let ctx = RiskContext { mark_price: 100.0, multiplier: 10.0, ..RiskContext::default() };
        // cap 1_000 sits BETWEEN the two: 200 (pre-fix, would PASS) and 2_000 (post-fix, DENIES).
        let v =
            RiskGate::new(RiskLimits { max_total_exposure: Some(1_000.0), ..RiskLimits::new() })
                .check(&market(1, 2.0), &ctx);
        assert!(
            !v.ok && v.reason == "over-max-exposure",
            "the multiplier must enter projected exposure (2*100*10 = 2_000 > 1_000): {v:?}"
        );
        // a cap above the true multiplied exposure still passes (2_000 <= 2_500).
        let v =
            RiskGate::new(RiskLimits { max_total_exposure: Some(2_500.0), ..RiskLimits::new() })
                .check(&market(1, 2.0), &ctx);
        assert!(v.ok, "a cap above the true multiplied exposure must pass: {v:?}");
        // multiplier 1 (the default) is unchanged: 2 * 100 * 1 = 200 <= 1_000 ⇒ passes.
        let ctx1 = RiskContext { mark_price: 100.0, multiplier: 1.0, ..RiskContext::default() };
        let v =
            RiskGate::new(RiskLimits { max_total_exposure: Some(1_000.0), ..RiskLimits::new() })
                .check(&market(1, 2.0), &ctx1);
        assert!(v.ok, "multiplier-1 exposure is bit-identical and must still pass: {v:?}");
    }

    // ── HALTED admits a position-covered reduce, and NOTHING else ────────────────────────────
    //
    // The law: a halt stops OPENING risk and must never TRAP the operator in a position. Until
    // this block existed `Halted` denied every order, so `market-exit`'s flatten legs came back
    // `OrderDenied` and the panic button was disarmed exactly in the situations that reach
    // `Halted` on their own. `docs/ops/kill-switches.md` is the operator-facing statement.

    /// The FIX itself: the shape `OrderIntent::Flatten` mints — a `reduce_only` MARKET for exactly
    /// `|position|`, opposite the position — must pass the kill switch.
    #[test]
    fn halted_admits_the_flatten_shape_that_market_exit_mints() {
        let long = RiskContext {
            mark_price: 100.0,
            position_size: 2.0,
            trading_state: TradingState::Halted,
            ..RiskContext::default()
        };
        // exactly what `Flatten` builds: closing_side(pos) = -1, qty = |pos|, reduce_only.
        let mut flat_leg = market(-1, 2.0);
        flat_leg.reduce_only = true;
        let v = RiskGate::new(RiskLimits::new()).check(&flat_leg, &long);
        assert!(v.ok, "a halt must not trap the operator in a position: {v:?}");

        // the SHORT twin (short 2, BUY 2 to close)
        let short = RiskContext { position_size: -2.0, ..long };
        let mut flat_short = market(1, 2.0);
        flat_short.reduce_only = true;
        assert!(RiskGate::new(RiskLimits::new()).check(&flat_short, &short).ok);

        // a PARTIAL exit is covered too — it shrinks abs(position) without crossing zero.
        assert!(RiskGate::new(RiskLimits::new()).check(&market(-1, 1.0), &long).ok);

        // ...and the IMPLICIT form: a genuine exit is admitted whether or not the caller
        // remembered the flag, because the predicate reads the POSITION, not the flag.
        assert!(RiskGate::new(RiskLimits::new()).check(&market(-1, 2.0), &long).ok);
    }

    /// THE MUTATION SENTINEL, and the reason the predicate is `is_covered_reduce` rather than
    /// `request.reduce_only`. A gate that trusted the caller-asserted flag would pass the test
    /// above AND admit both of these — each of which OPENS risk under a halt.
    #[test]
    fn halted_refuses_a_reduce_only_flag_that_does_not_actually_reduce() {
        let halted = |pos: f64| RiskContext {
            mark_price: 100.0,
            position_size: pos,
            trading_state: TradingState::Halted,
            ..RiskContext::default()
        };

        // (1) FLAT BOOK: there is no position to reduce, so a `reduce_only` tag is an OPENING
        // order — and no venue catches it server-side either, since there is nothing to cap it
        // against. This is the shape a strategy bug that tags its entries `reduce_only` produces.
        let mut flagged_open = market(-1, 2.0);
        flagged_open.reduce_only = true;
        let v = RiskGate::new(RiskLimits::new()).check(&flagged_open, &halted(0.0));
        assert!(
            !v.ok && v.reason == "halted",
            "a flat-book reduce_only order is an OPENING order and the halt must refuse it: {v:?}"
        );

        // (2) REVERSAL: long 2, `reduce_only` SELL 5 flips to SHORT 3 of brand-new exposure.
        let mut overshoot = market(-1, 5.0);
        overshoot.reduce_only = true;
        let v = RiskGate::new(RiskLimits::new()).check(&overshoot, &halted(2.0));
        assert!(
            !v.ok && v.reason == "halted",
            "a reduce_only order that FLIPS the position opens risk under a halt: {v:?}"
        );

        // (3) SAME-DIRECTION ADD tagged reduce_only (long 5, BUY 2): magnitude-covered but
        // exposure-INCREASING. `is_covered_reduce`'s direction arm is what refuses it.
        let mut add = market(1, 2.0);
        add.reduce_only = true;
        let v = RiskGate::new(RiskLimits::new()).check(&add, &halted(5.0));
        assert!(!v.ok && v.reason == "halted", "a reduce_only ADD must not pass a halt: {v:?}");

        // (4) and the ordinary opening order, which is what a halt is FOR.
        let v = RiskGate::new(RiskLimits::new()).check(&market(1, 2.0), &halted(0.0));
        assert!(!v.ok && v.reason == "halted", "an opening order must still be halted: {v:?}");
    }

    /// The LOT-ROUNDING edge the post-normalization re-check exists for. The kill-switch arm at
    /// the top of `check_inner` judges the RAW qty (it runs before the grid is resolved), and
    /// rounding is half-to-EVEN, so it can round a qty UP across the coverage boundary: 1.6 on a
    /// 1.0 lot becomes 2.0, which against a 1.8 long FLIPS the position short 0.2.
    ///
    /// ⚠ MUTATION-CHECK THIS ONE by deleting the re-check next to `covered_reduce` — the raw-qty
    /// arm admits it (1.8 >= 1.6) and this test is the only thing that catches it.
    #[test]
    fn halted_refuses_a_reduce_whose_lot_rounding_would_flip_the_position() {
        let long = RiskContext {
            mark_price: 100.0,
            position_size: 1.8,
            trading_state: TradingState::Halted,
            ..RiskContext::default()
        };
        let lim = || RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
        let mut req = market(-1, 1.6); // covered RAW (1.8 >= 1.6); rounds to 2.0 on the wire
        req.reduce_only = true;
        let v = RiskGate::new(lim()).check(&req, &long);
        assert!(
            !v.ok && v.reason == "halted",
            "the halt verdict must be re-taken against the size actually sent: {v:?}"
        );
        // the same order on a grid that does NOT round it up is still admitted.
        let mut fine = market(-1, 1.6);
        fine.reduce_only = true;
        let v = RiskGate::new(RiskLimits { lot_size: Some(0.1), ..RiskLimits::new() })
            .check(&fine, &long);
        assert!(v.ok, "a genuinely covered reduce must still get out: {v:?}");
    }

    /// The three states are a strict LADDER — `Halted` ⊂ `Reducing` ⊂ `Active` — and the halt
    /// exemption must never widen `Halted` past `Reducing`. `Reducing` keeps the looser,
    /// flag-trusting `RiskGate::reduces` on purpose: it is the state you are meant to be able to
    /// trade out of, whereas `Halted` is the kill switch.
    #[test]
    fn halted_admits_strictly_less_than_reducing_which_admits_less_than_active() {
        let at = |state, pos: f64| RiskContext {
            mark_price: 100.0,
            position_size: pos,
            trading_state: state,
            ..RiskContext::default()
        };
        let mut flagged_open = market(-1, 2.0);
        flagged_open.reduce_only = true;

        // the flag-only order (flat book): Active yes, Reducing yes (it trusts the flag), Halted NO.
        assert!(
            RiskGate::new(RiskLimits::new())
                .check(&flagged_open, &at(TradingState::Active, 0.0))
                .ok
        );
        assert!(
            RiskGate::new(RiskLimits::new())
                .check(&flagged_open, &at(TradingState::Reducing, 0.0))
                .ok
        );
        assert!(
            !RiskGate::new(RiskLimits::new())
                .check(&flagged_open, &at(TradingState::Halted, 0.0))
                .ok
        );

        // a genuine covered exit: admitted by all three.
        let exit = market(-1, 2.0);
        for st in [TradingState::Active, TradingState::Reducing, TradingState::Halted] {
            assert!(
                RiskGate::new(RiskLimits::new()).check(&exit, &at(st, 2.0)).ok,
                "a covered exit must be admitted in every state, including {st:?}"
            );
        }

        // a plain opening order: Active only.
        let open = market(1, 2.0);
        assert!(RiskGate::new(RiskLimits::new()).check(&open, &at(TradingState::Active, 0.0)).ok);
        assert!(
            !RiskGate::new(RiskLimits::new()).check(&open, &at(TradingState::Reducing, 0.0)).ok
        );
        assert!(!RiskGate::new(RiskLimits::new()).check(&open, &at(TradingState::Halted, 0.0)).ok);
    }

    /// A COMBO stays denied wholesale under `Halted` — `check_combo`'s own kill-switch arm returns
    /// before any leg is examined, so the single-order exemption above cannot leak into it. This is
    /// a decision, not an oversight: `market_exit_flatten_legs` mints per-symbol `Flatten` intents
    /// (single orders), never a combo, so nothing on the exit path needs this; and a combo is ONE
    /// new multi-leg venue order whose legs derive their `reduce_only` from a PROJECTED book rather
    /// than a settled one. Widening it would need each leg proven covered against real state.
    #[test]
    fn halted_still_denies_a_combo_wholesale() {
        let halted = RiskContext {
            trading_state: TradingState::Halted,
            position_size: 2.0,
            ..RiskContext::default()
        };
        let v = RiskGate::new(RiskLimits::new()).check_combo(
            &combo_limit(-1, 1.0, 20.0),
            &halted,
            leg_marks,
        );
        assert!(!v.ok && v.reason == "halted", "a combo is refused as one unit under halt: {v:?}");
    }

    /// #600-P4 FIX: a `reduce_only`-tagged SAME-DIRECTION ADD (exposure-INCREASING but
    /// magnitude-covered — long 5, BUY 2 tagged reduce_only) is NOT a covered reduce. The gate
    /// must treat it as the OPENING order it is on ALL THREE bypasses that read
    /// `is_covered_reduce` — the min floors, buying power, and the impact veto — matching
    /// `SimBroker::apply_fill`'s direction-only opening split and the real venues (binance/bybit)
    /// that reject a reduce_only order which would increase the position. Pre-fix the coverage-only
    /// flag arm laundered it past all three.
    #[test]
    fn reduce_only_same_direction_add_is_not_a_covered_reduce() {
        // FLOOR lane: long 5, BUY 2 tagged reduce_only, below a 10.0 min_qty floor ⇒ DENY.
        let long = RiskContext { mark_price: 100.0, position_size: 5.0, ..RiskContext::default() };
        let mut add = market(1, 2.0);
        add.reduce_only = true;
        let v = RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() })
            .check(&add, &long);
        assert!(
            !v.ok && v.reason == "below-min-qty",
            "a reduce_only same-direction ADD must face the min floor: {v:?}"
        );
        // the short twin (short 5, SELL 2 tagged reduce_only)
        let short =
            RiskContext { mark_price: 100.0, position_size: -5.0, ..RiskContext::default() };
        let mut add_s = market(-1, 2.0);
        add_s.reduce_only = true;
        let v = RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() })
            .check(&add_s, &short);
        assert!(!v.ok && v.reason == "below-min-qty", "short-side add must face the floor: {v:?}");

        // MARGIN lane: the add opens real exposure, so it must face buying power. IM 0.1,
        // order margin = 2 * 100 * 0.1 = 20; equity 10 ⇒ DENY (pre-fix: bypassed, admitted).
        let poor = RiskContext {
            mark_price: 100.0,
            position_size: 5.0,
            equity: 10.0,
            multiplier: 1.0,
            ..RiskContext::default()
        };
        let v = RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() })
            .check(&add, &poor);
        assert!(
            !v.ok && v.reason == "insufficient-margin",
            "a reduce_only same-direction ADD must face buying power: {v:?}"
        );

        // GENUINE covered reduce is untouched: SELL 2 into the long 5, below the same floor, still
        // bypasses (anti-stranding preserved).
        let mut reduce = market(-1, 2.0);
        reduce.reduce_only = true;
        let v = RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() })
            .check(&reduce, &long);
        assert!(v.ok, "a genuine covered reduce must still bypass the floor: {v:?}");
    }

    /// LIVE-VS-BACKTEST DIVERGENCE FIX (finding B): the gate's notional was `qty × price` with
    /// NO contract multiplier, while `SimBroker` gates fills on `rounded × price × multiplier`
    /// and the gate's OWN margin calc already used `ctx.multiplier` — the same `min_notional`
    /// gated differently live vs backtest for multiplier != 1 instruments.
    #[test]
    fn notional_includes_the_contract_multiplier() {
        let lim = || RiskLimits {
            min_notional: Some(5.0),
            max_notional_per_order: Some(100.0),
            ..RiskLimits::new()
        };
        let m10 = RiskContext { mark_price: 1.0, multiplier: 10.0, ..RiskContext::default() };
        // qty×price = 2 (under the 5.0 floor) but ×10 multiplier = 20 ⇒ passes now
        let v = RiskGate::new(lim()).check(&market(1, 2.0), &m10);
        assert!(v.ok, "multiplier-inclusive notional must clear the floor: {v:?}");
        // vice versa: qty×price = 20 (inside the 100 cap) but ×10 = 200 ⇒ over-max-notional now
        let v = RiskGate::new(lim()).check(&market(1, 20.0), &m10);
        assert!(!v.ok && v.reason == "over-max-notional", "cap must see the multiplier: {v:?}");
        // still under the floor even WITH the multiplier: 0.2 × 1 × 10 = 2 < 5
        let v = RiskGate::new(lim()).check(&market(1, 0.2), &m10);
        assert!(!v.ok && v.reason == "below-min-notional", "{v:?}");
        // COMPAT PIN: multiplier 1.0 — the default, i.e. nearly every instrument — reproduces the
        // exact pre-fix verdicts (the same boundary cases pinned in
        // `ordinary_positive_price_verdicts_are_unchanged_by_the_abs`; `x * 1.0` is bit-exact).
        let m1 = RiskContext { mark_price: 100.0, multiplier: 1.0, ..RiskContext::default() };
        assert!(RiskGate::new(lim()).check(&market(1, 0.5), &m1).ok);
        assert_eq!(RiskGate::new(lim()).check(&market(1, 0.01), &m1).reason, "below-min-notional");
        assert_eq!(RiskGate::new(lim()).check(&market(-1, 2.0), &m1).reason, "over-max-notional");
    }

    /// The combo path routes every leg through the SAME `check_inner` (it has no notional line of
    /// its own), so the multiplier-inclusive notional flows through `leg_ctx`'s per-symbol
    /// `multiplier` automatically — pinned here so a future combo-side notional never forks.
    #[test]
    fn combo_leg_notional_uses_the_leg_multiplier() {
        // 0.1 units: leg A 0.1×50 = 5.0 (at the floor), leg B 0.1×30 = 3.0 — denied at ×1
        // (the `one_failing_leg…` case), but as a ×10 contract 3.0×10 = 30 ⇒ passes.
        let lim = || RiskLimits { min_notional: Some(5.0), ..RiskLimits::new() };
        let ctx = RiskContext::default();
        let v = RiskGate::new(lim()).check_combo(&combo_limit(1, 0.1, 20.0), &ctx, leg_marks);
        assert_eq!(v.reason, format!("leg {LEG_B}: below-min-notional"));
        let with_mult = |sym: &str| {
            let mut c = leg_marks(sym);
            if sym == LEG_B {
                c.multiplier = 10.0;
            }
            c
        };
        let v = RiskGate::new(lim()).check_combo(&combo_limit(1, 0.1, 20.0), &ctx, with_mult);
        assert!(v.ok, "the leg multiplier must enter the leg's notional: {v:?}");
    }

    /// A limit order whose `price` is a SIGNED combo net (`ComboSpec::net_limit`) — the shape a
    /// `Combo` takes through the gate once PR-2 lowers it.
    fn combo_limit(side: i32, qty: f64, net: f64) -> OrderRequest {
        use vike_model::ComboLeg;
        OrderRequest {
            client_order_id: "c1".to_string(),
            venue: "deribit".to_string(),
            symbol: String::new(),
            order_type: "limit".to_string(),
            side,
            qty,
            price: Some(net),
            combo_legs: vec![
                ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
                ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn notional_is_a_magnitude_so_credit_combos_gate_like_debit_ones() {
        // REGRESSION: `notional = qty.abs() * ref_price` (SIGNED) went NEGATIVE for a credit
        // combo, so (a) `notional < min_notional` denied EVERY credit combo, and (b)
        // `notional > cap` could never trip, letting an arbitrarily large one escape the cap.
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let limits = || RiskLimits {
            min_notional: Some(5.0),
            max_notional_per_order: Some(100.0),
            ..RiskLimits::new()
        };

        // DEBIT (+20 net, 2 units => |notional| 40): inside both bounds, passes. Unchanged.
        let mut gate = RiskGate::new(limits());
        let v = gate.check(&combo_limit(1, 2.0, 20.0), &ctx);
        assert!(v.ok, "debit combo should pass: {v:?}");

        // CREDIT (-20 net, same magnitude): must gate IDENTICALLY to the debit twin.
        let mut gate = RiskGate::new(limits());
        let v = gate.check(&combo_limit(-1, 2.0, -20.0), &ctx);
        assert!(v.ok, "credit combo must NOT be denied below-min-notional: {v:?}");

        // ...and the per-order cap must still bite on the credit side (|−60| * 2 = 120 > 100).
        let mut gate = RiskGate::new(limits());
        let v = gate.check(&combo_limit(-1, 2.0, -60.0), &ctx);
        assert!(!v.ok && v.reason == "over-max-notional", "credit cap must bite: {v:?}");
        // symmetric with the debit twin
        let mut gate = RiskGate::new(limits());
        let v = gate.check(&combo_limit(1, 2.0, 60.0), &ctx);
        assert!(!v.ok && v.reason == "over-max-notional", "{v:?}");

        // a genuinely tiny credit is still below-min-notional (the check is not disabled)
        let mut gate = RiskGate::new(limits());
        let v = gate.check(&combo_limit(-1, 1.0, -1.0), &ctx);
        assert!(!v.ok && v.reason == "below-min-notional", "{v:?}");
    }

    #[test]
    fn ordinary_positive_price_verdicts_are_unchanged_by_the_abs() {
        // The abs() must be a NO-OP for every ordinary (non-negative price) order — the
        // byte-identical-when-off guarantee.
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let lim = || RiskLimits {
            min_notional: Some(5.0),
            max_notional_per_order: Some(100.0),
            ..RiskLimits::new()
        };
        let mut gate = RiskGate::new(lim());
        // market order, priced off the mark: 0.5 * 100 = 50 => ok
        assert!(gate.check(&market(1, 0.5), &ctx).ok);
        // 0.01 * 100 = 1 < 5 => below-min-notional
        let mut gate = RiskGate::new(lim());
        assert_eq!(gate.check(&market(1, 0.01), &ctx).reason, "below-min-notional");
        // 2 * 100 = 200 > 100 => over-max-notional
        let mut gate = RiskGate::new(lim());
        assert_eq!(gate.check(&market(-1, 2.0), &ctx).reason, "over-max-notional");
    }

    // ---- pre-trade impact veto (opt-in) ----

    /// asks 100@1, 101@2, 102@3 ; bids 99@1, 98@2, 97@3 ⇒ mid = 99.5, tick 1.0
    fn book() -> L2Book {
        let mut b = L2Book::new(1.0);
        b.apply_snapshot(
            1,
            &[(99.0, 1.0), (98.0, 2.0), (97.0, 3.0)],
            &[(100.0, 1.0), (101.0, 2.0), (102.0, 3.0)],
        );
        b
    }

    #[test]
    fn impact_veto_none_budget_never_denies() {
        let b = book();
        // even a size the book cannot fill at all passes when the knob is off
        assert_eq!(impact_veto(&b, 1, 1e9, None), None);
        assert_eq!(impact_veto(&b, -1, 1e9, None), None);
        assert_eq!(impact_veto(&L2Book::new(1.0), 1, 5.0, None), None);
    }

    #[test]
    fn impact_veto_empty_book_not_fillable() {
        let empty = L2Book::new(1.0);
        assert_eq!(impact_veto(&empty, 1, 1.0, Some(1e9)), Some(ImpactDeny::NotFillable));
        assert_eq!(impact_veto(&empty, -1, 1.0, Some(1e9)), Some(ImpactDeny::NotFillable));
        assert_eq!(fillable_veto(&empty, 1, 1.0), Some(ImpactDeny::NotFillable));
        // qty 0 is vacuously fillable
        assert_eq!(fillable_veto(&empty, 1, 0.0), None);
    }

    #[test]
    fn impact_veto_partial_walk_is_not_fillable() {
        let b = book();
        // 6 units exhausts each side exactly; 7 cannot fill ⇒ slippage is unbounded ⇒ deny
        assert_eq!(fillable_veto(&b, 1, 6.0), None);
        assert_eq!(impact_veto(&b, 1, 7.0, Some(1e9)), Some(ImpactDeny::NotFillable));
        assert_eq!(impact_veto(&b, -1, 7.0, Some(1e9)), Some(ImpactDeny::NotFillable));
        assert_eq!(fillable_veto(&b, -1, 7.0), Some(ImpactDeny::NotFillable));
    }

    #[test]
    fn impact_veto_exact_fill_both_sides_and_budget_boundary() {
        let b = book();
        // BUY 3 → 100×1 + 101×2 = 302 / 3 = 100.6667 ; mid 99.5 ⇒ +117.25 bps
        let buy = b.simulate_fill(1, 3.0).slippage_bps_vs_mid.unwrap();
        assert!(buy > 117.0 && buy < 118.0, "buy slippage {buy}");
        // SELL 3 → 99×1 + 98×2 = 295 / 3 = 98.3333 ; below mid ⇒ positive (worse for taker)
        let sell = b.simulate_fill(-1, 3.0).slippage_bps_vs_mid.unwrap();
        assert!(sell > 117.0 && sell < 118.0, "sell slippage {sell}");

        // budget-EQUAL passes (strict >), a hair under denies
        assert_eq!(impact_veto(&b, 1, 3.0, Some(buy)), None);
        assert_eq!(impact_veto(&b, 1, 3.0, Some(buy - 1e-9)), Some(ImpactDeny::OverSlippageBudget));
        assert_eq!(impact_veto(&b, -1, 3.0, Some(sell)), None);
        assert_eq!(
            impact_veto(&b, -1, 3.0, Some(sell - 1e-9)),
            Some(ImpactDeny::OverSlippageBudget)
        );
        // touching only the top of book is cheapest and passes a tight budget
        assert_eq!(impact_veto(&b, 1, 1.0, Some(51.0)), None);
    }

    #[test]
    fn check_with_book_none_is_identical_to_check() {
        let lim = RiskLimits {
            max_slippage_bps: Some(0.0), // armed, but no book ⇒ inert
            require_fillable: true,
            ..RiskLimits::new()
        };
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let a = RiskGate::new(lim.clone()).check(&market(1, 5.0), &ctx);
        let b = RiskGate::new(lim).check_with_book(&market(1, 5.0), &ctx, None);
        assert!(a.ok && b.ok, "unarmed-by-absent-book must pass: {a:?} / {b:?}");
    }

    #[test]
    fn check_with_book_unarmed_limits_pass_with_a_book() {
        // book present but both knobs off ⇒ no veto even for a size the book cannot fill
        let mut gate = RiskGate::new(RiskLimits::new());
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let v = gate.check_with_book(&market(1, 1_000.0), &ctx, Some(&book()));
        assert!(v.ok, "got {v:?}");
    }

    #[test]
    fn check_with_book_denies_over_budget_and_passes_within() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let b = book();
        // within budget: 1 unit at the top of book (~50.25 bps) under a 200 bps budget
        let mut ok_gate =
            RiskGate::new(RiskLimits { max_slippage_bps: Some(200.0), ..RiskLimits::new() });
        let ok = ok_gate.check_with_book(&market(1, 1.0), &ctx, Some(&b));
        assert!(ok.ok, "got {ok:?}");
        // over budget: 3 units (~117 bps) under a 60 bps budget
        let mut deny_gate =
            RiskGate::new(RiskLimits { max_slippage_bps: Some(60.0), ..RiskLimits::new() });
        let d = deny_gate.check_with_book(&market(1, 3.0), &ctx, Some(&b));
        assert!(!d.ok && d.reason == "impact-over-slippage-budget", "got {d:?}");
    }

    #[test]
    fn check_with_book_require_fillable_is_its_own_knob() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let b = book();
        let mut gate = RiskGate::new(RiskLimits { require_fillable: true, ..RiskLimits::new() });
        // 6 units is exactly the displayed ask depth ⇒ fillable, no budget set ⇒ passes
        assert!(gate.check_with_book(&market(1, 6.0), &ctx, Some(&b)).ok);
        let v = gate.check_with_book(&market(1, 6.5), &ctx, Some(&b));
        assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
    }

    /// The IMPACT bypass moved onto `is_covered_reduce` together with the margin bypass: the
    /// anti-stranding rationale that justifies it is a statement about a POSITION, so a flat
    /// book has nothing to protect and a mis-tagged order must not skip the veto.
    #[test]
    fn impact_veto_bypass_requires_position_coverage() {
        let b = book();
        let armed = || RiskLimits { require_fillable: true, ..RiskLimits::new() };
        let ro = |side, qty| OrderRequest { reduce_only: true, ..market(side, qty) };
        // FLAT book + the flag, size beyond the displayed depth ⇒ vetoed (was: bypassed).
        let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let v = RiskGate::new(armed()).check_with_book(&ro(1, 6.5), &flat, Some(&b));
        assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
        // COVERED reduce of the same size ⇒ still bypasses (anti-stranding preserved): the
        // exit must go through exactly when the book looks worst.
        let long = RiskContext { position_size: 10.0, mark_price: 100.0, ..RiskContext::default() };
        let c = RiskGate::new(armed()).check_with_book(&ro(-1, 6.5), &long, Some(&b));
        assert!(c.ok, "covered reduce must still bypass the impact veto; got {c:?}");
    }

    fn limit(side: i32, qty: f64, px: f64) -> OrderRequest {
        OrderRequest { order_type: "limit".to_string(), price: Some(px), ..market(side, qty) }
    }

    /// REGRESSION (review major #2): a PASSIVE limit pays no slippage and must never be
    /// impact-vetoed — the pre-fix gate walked it as if it were a market taker, which denied
    /// every `SpreadMaker` quote under an armed budget.
    #[test]
    fn passive_limit_is_never_impact_vetoed() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let b = book();
        let armed = RiskLimits {
            max_slippage_bps: Some(1.0), // brutally tight
            require_fillable: true,      // and a depth floor the quote size blows through
            ..RiskLimits::new()
        };
        // BUY 99 (at the best bid, joining the queue) and SELL 100 — both rest, neither takes.
        for req in [limit(1, 5.0, 99.0), limit(-1, 5.0, 100.0), limit(1, 1e6, 98.0)] {
            let v = RiskGate::new(armed.clone()).check_with_book(&req, &ctx, Some(&b));
            assert!(v.ok, "passive limit must pass an armed gate: {req:?} -> {v:?}");
        }
        // stops/take-profits fire against a future book — also never judged on today's depth
        let mut stop = market(1, 1e6);
        stop.order_type = "stop".to_string();
        stop.trigger_price = Some(105.0);
        let v = RiskGate::new(armed).check_with_book(&stop, &ctx, Some(&b));
        assert!(v.ok, "stop must not be impact-vetoed on the current book: {v:?}");
    }

    /// A CROSSING limit does take — but only at its limit or better, and any remainder rests.
    #[test]
    fn crossing_limit_is_judged_only_at_its_limit_or_better() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let b = book();
        assert_eq!(take_scope(&b, 1, "limit", Some(100.0)), TakeScope::Crossing(100.0));
        assert_eq!(take_scope(&b, 1, "limit", Some(99.5)), TakeScope::Passive);
        assert_eq!(take_scope(&b, -1, "limit", Some(99.0)), TakeScope::Crossing(99.0));
        assert_eq!(take_scope(&b, 1, "market", None), TakeScope::Market);

        // BUY 3 crossing at 101 takes 100×1 + 101×2 ⇒ ~117 bps ⇒ over a 60 bps budget
        let mut g = RiskGate::new(RiskLimits { max_slippage_bps: Some(60.0), ..RiskLimits::new() });
        let d = g.check_with_book(&limit(1, 3.0, 101.0), &ctx, Some(&b));
        assert!(!d.ok && d.reason == "impact-over-slippage-budget", "got {d:?}");
        // the SAME size crossing only at 100 can take just 1 unit there (~50 bps); the other 2
        // rest, so the budget arm sees only the takeable slice and passes.
        let mut g = RiskGate::new(RiskLimits { max_slippage_bps: Some(60.0), ..RiskLimits::new() });
        let v = g.check_with_book(&limit(1, 3.0, 100.0), &ctx, Some(&b));
        assert!(v.ok, "the unfillable remainder rests, it does not pay impact: {v:?}");
        // but `require_fillable` is an explicit fill-this-size-now floor and still denies it
        let mut g = RiskGate::new(RiskLimits { require_fillable: true, ..RiskLimits::new() });
        let v = g.check_with_book(&limit(1, 3.0, 100.0), &ctx, Some(&b));
        assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
    }

    /// REGRESSION (review major #3): a closing order must never be stranded by the impact gate.
    #[test]
    fn pure_reduce_bypasses_the_impact_veto() {
        let b = book();
        let armed =
            RiskLimits { require_fillable: true, max_slippage_bps: Some(0.1), ..RiskLimits::new() };
        // long 10, flatten with a market sell of 10 into 6 units of displayed bid depth
        let ctx = RiskContext { mark_price: 100.0, position_size: 10.0, ..RiskContext::default() };
        let mut req = market(-1, 10.0);
        req.reduce_only = true;
        let v = RiskGate::new(armed.clone()).check_with_book(&req, &ctx, Some(&b));
        assert!(v.ok, "an explicit reduce_only exit must go through: {v:?}");
        // the implicit form (opposing side, size within the position) too
        let v = RiskGate::new(armed.clone()).check_with_book(&market(-1, 10.0), &ctx, Some(&b));
        assert!(v.ok, "an implicit close must go through: {v:?}");
        // and the same order is still vetoed when it OPENS (flat book-side depth, no position)
        let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let v = RiskGate::new(armed).check_with_book(&market(-1, 10.0), &flat, Some(&b));
        assert!(!v.ok && v.reason == "impact-not-fillable", "opening must still veto: {v:?}");
    }

    /// The two public pure fns must agree on degenerate size (review minor).
    #[test]
    fn impact_and_fillable_agree_on_non_positive_qty() {
        let b = book();
        for side in [1, -1] {
            assert_eq!(fillable_veto(&b, side, 0.0), None);
            assert_eq!(impact_veto(&b, side, 0.0, Some(0.0)), None);
            assert_eq!(fillable_veto(&b, side, -1.0), None);
            assert_eq!(impact_veto(&b, side, -1.0, Some(0.0)), None);
        }
    }

    // ---- combo crossing (spec §5: atomic per-leg, ONE throttle slot) ----

    const LEG_A: &str = "BTC-27MAR26-100000-C";
    const LEG_B: &str = "BTC-27MAR26-120000-C";

    /// per-leg marks: LEG_A 50, LEG_B 30 ⇒ a 1×/−1× call spread nets +20 (debit)
    fn leg_marks(sym: &str) -> RiskContext {
        let mark = match sym {
            LEG_A => 50.0,
            LEG_B => 30.0,
            _ => 0.0,
        };
        RiskContext { mark_price: mark, ..RiskContext::default() }
    }

    #[test]
    fn combo_passes_when_every_leg_passes_and_burns_exactly_one_slot() {
        let mut gate = RiskGate::new(RiskLimits {
            min_notional: Some(5.0),
            max_orders_per_window: Some(1),
            ..RiskLimits::new()
        });
        let ctx = RiskContext::default();
        // 2 units: leg A 2×50 = 100, leg B 2×30 = 60 — both above min_notional
        let v = gate.check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
        assert!(v.ok, "{v:?}");
        // the returned request is the COMBO verbatim — legs intact, SIGNED net untouched
        let req = v.request.unwrap();
        assert_eq!(req.combo_legs.len(), 2);
        assert_eq!(req.price, Some(20.0));
        // ONE slot for N legs, not N
        assert_eq!(gate.throttle_times().len(), 1, "a combo must consume exactly one slot");
        // ...and it really was consumed: the next combo is rate-limited
        let v = gate.check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
        assert!(!v.ok && v.reason == "rate-limited", "{v:?}");
    }

    #[test]
    fn credit_combo_gates_exactly_like_its_debit_twin() {
        // A short call spread is a CREDIT: net_limit NEGATIVE. Per-leg risk is identical to the
        // debit twin (same legs, same marks, same sizes) — nothing may treat the sign as size.
        let limits = || RiskLimits {
            min_notional: Some(5.0),
            max_notional_per_order: Some(1_000.0),
            ..RiskLimits::new()
        };
        let ctx = RiskContext::default();
        let debit =
            RiskGate::new(limits()).check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
        let credit =
            RiskGate::new(limits()).check_combo(&combo_limit(-1, 2.0, -20.0), &ctx, leg_marks);
        assert!(debit.ok, "{debit:?}");
        assert!(credit.ok, "credit combo must not be denied: {credit:?}");
        // the negative net survives the gate un-clamped and un-absolute-valued
        assert_eq!(credit.request.unwrap().price, Some(-20.0));
    }

    #[test]
    fn one_failing_leg_denies_the_whole_combo_and_names_it() {
        // LEG_B's mark is 30 ⇒ 0.1 units = 3.0 notional, under the 5.0 floor; LEG_A (5.0) passes.
        let mut gate = RiskGate::new(RiskLimits {
            min_notional: Some(5.0),
            max_orders_per_window: Some(4),
            ..RiskLimits::new()
        });
        let v = gate.check_combo(&combo_limit(1, 0.1, 20.0), &RiskContext::default(), leg_marks);
        assert!(!v.ok, "{v:?}");
        assert_eq!(v.reason, format!("leg {LEG_B}: below-min-notional"));
        assert!(v.request.is_none());
        // a DENIED combo burns no rate slot — the passing first leg must not have taken one either
        assert!(gate.throttle_times().is_empty(), "a denied combo consumed a slot: {v:?}");
    }

    #[test]
    fn combo_leg_sides_follow_the_sign_law_for_reduce_only_state() {
        // Reducing state admits only position-reducing legs. Long LEG_A / flat LEG_B:
        // BUYING the combo (+1 ratio on A) is an ADD on A ⇒ denied, naming A.
        let per_leg = |sym: &str| match sym {
            LEG_A => RiskContext {
                mark_price: 50.0,
                position_size: 10.0,
                trading_state: TradingState::Reducing,
                ..RiskContext::default()
            },
            _ => RiskContext {
                mark_price: 30.0,
                trading_state: TradingState::Reducing,
                ..RiskContext::default()
            },
        };
        let ctx = RiskContext { trading_state: TradingState::Reducing, ..RiskContext::default() };
        let mut gate = RiskGate::new(RiskLimits::new());
        let v = gate.check_combo(&combo_limit(1, 1.0, 20.0), &ctx, per_leg);
        assert_eq!(v.reason, format!("leg {LEG_A}: reduce-only"));
        // SELLING the same combo flips leg A short (a reduce on the long) — leg B (+1 after the
        // flip of its −1 ratio) is the one that now adds, so the denial moves to B.
        let mut gate = RiskGate::new(RiskLimits::new());
        let v = gate.check_combo(&combo_limit(-1, 1.0, -20.0), &ctx, per_leg);
        assert_eq!(v.reason, format!("leg {LEG_B}: reduce-only"));
    }

    #[test]
    fn combo_leg_qty_is_ratio_times_units() {
        // ratio 3 on leg A: 3 × 2 units = 6 @ 50 = 300 notional — over a 250 cap, under 350.
        let mut req = combo_limit(1, 2.0, 20.0);
        req.combo_legs[0].ratio = 3;
        let ctx = RiskContext::default();
        let v =
            RiskGate::new(RiskLimits { max_notional_per_order: Some(250.0), ..RiskLimits::new() })
                .check_combo(&req, &ctx, leg_marks);
        assert_eq!(v.reason, format!("leg {LEG_A}: over-max-notional"));
        let v =
            RiskGate::new(RiskLimits { max_notional_per_order: Some(350.0), ..RiskLimits::new() })
                .check_combo(&req, &ctx, leg_marks);
        assert!(v.ok, "{v:?}");
    }

    #[test]
    fn combo_guards_halted_bad_side_bad_qty_and_the_not_a_combo_sentinel() {
        let mut gate = RiskGate::new(RiskLimits::new());
        let halted = RiskContext { trading_state: TradingState::Halted, ..RiskContext::default() };
        assert_eq!(
            gate.check_combo(&combo_limit(1, 1.0, 20.0), &halted, leg_marks).reason,
            "halted"
        );
        let ctx = RiskContext::default();
        assert_eq!(
            gate.check_combo(&combo_limit(0, 1.0, 20.0), &ctx, leg_marks).reason,
            "invalid-side"
        );
        assert_eq!(
            gate.check_combo(&combo_limit(1, 0.0, 20.0), &ctx, leg_marks).reason,
            "non-positive-size"
        );
        assert_eq!(
            gate.check_combo(&combo_limit(1, f64::NAN, 20.0), &ctx, leg_marks).reason,
            "non-positive-size"
        );
        // an ordinary (non-combo) request routed here is the sentinel case, never a silent pass
        assert_eq!(gate.check_combo(&market(1, 1.0), &ctx, leg_marks).reason, "not-a-combo");
        assert!(gate.throttle_times().is_empty());
    }

    #[test]
    fn account_reducing_is_authoritative_even_when_leg_ctx_says_active() {
        // CRITICAL: the natural caller closure fills only PER-SYMBOL facts and leaves
        // `trading_state` at its `RiskContext::default()` value (Active). The account ctx must
        // still win, or a fully risk-ADDING combo is admitted while the account is reduce-only.
        // NOTE this test deliberately does NOT hand-thread `Reducing` into the legs.
        let ctx = RiskContext { trading_state: TradingState::Reducing, ..RiskContext::default() };
        let mut gate = RiskGate::new(RiskLimits::new());
        let v = gate.check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
        assert!(!v.ok, "a risk-adding combo must be denied while the account reduces: {v:?}");
        assert_eq!(v.reason, format!("leg {LEG_A}: reduce-only"));
        assert!(gate.throttle_times().is_empty());
        // sanity: the same combo passes when the account is Active
        assert!(
            gate.check_combo(&combo_limit(1, 2.0, 20.0), &RiskContext::default(), leg_marks).ok
        );
    }

    #[test]
    fn a_leg_with_no_mark_is_denied_not_priced_at_zero() {
        // CRITICAL: `leg_ctx` is TOTAL, so an unknown symbol yields mark 0.0 — which makes
        // notional 0, exposure 0 and initial_margin 0, i.e. every price-based limit vacuous.
        // `min_notional` would catch it, but `from_properties` maps 0.0 → None, the normal case
        // for options — the exact asset class combos exist for.
        let unarmed = || RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
        let ctx = RiskContext::default();

        // 0.0 mark (unknown symbol): would otherwise pass buying power at ZERO equity.
        let zero_mark = |sym: &str| match sym {
            // deep-pocketed so leg A itself is never the denial under test
            LEG_A => RiskContext { mark_price: 50.0, equity: 1e18, ..RiskContext::default() },
            _ => RiskContext::default(), // mark 0.0 — "I don't know this symbol", equity 0
        };
        let v = RiskGate::new(unarmed()).check_combo(&combo_limit(1, 1e6, 20.0), &ctx, zero_mark);
        assert!(!v.ok, "a zero-mark leg must not be admitted: {v:?}");
        assert_eq!(v.reason, format!("leg {LEG_B}: no-mark"));

        // NaN mark: every comparison against it is false, so it silently passes every cap.
        let nan_mark = |sym: &str| match sym {
            LEG_A => RiskContext { mark_price: 50.0, equity: 1e18, ..RiskContext::default() },
            _ => RiskContext { mark_price: f64::NAN, ..RiskContext::default() },
        };
        let v = RiskGate::new(unarmed()).check_combo(&combo_limit(1, 1e6, 20.0), &ctx, nan_mark);
        assert_eq!(v.reason, format!("leg {LEG_B}: no-mark"));
    }

    #[test]
    fn leg_margin_accumulates_so_a_combo_is_never_cheaper_than_its_naked_legs() {
        // MAJOR: each leg gets a FRESH lctx, so without accumulation N legs each fit in the same
        // unchanged free BP. im 0.1, equity 100k, marks 50/30, 12_000 units:
        //   leg A margin = 12_000×50×0.1 = 60_000, leg B = 12_000×30×0.1 = 36_000.
        // Individually both fit in 100k; together they need 96_000 — which still fits, so push
        // leg A to 1.6k units... use a size where the PAIR overflows but each leg alone does not.
        let limits = || RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
        let acct = |_: &str| RiskContext { equity: 100_000.0, ..RiskContext::default() };
        let marks_with_equity =
            move |sym: &str| RiskContext { mark_price: leg_marks(sym).mark_price, ..acct(sym) };
        let ctx = RiskContext::default();

        // 15_000 units: A = 75_000, B = 45_000. Each alone < 100_000; together 120_000 > 100_000.
        let v = RiskGate::new(limits()).check_combo(
            &combo_limit(1, 15_000.0, 20.0),
            &ctx,
            marks_with_equity,
        );
        assert!(!v.ok, "the second leg must see the first leg's committed margin: {v:?}");
        assert_eq!(v.reason, format!("leg {LEG_B}: insufficient-margin"));

        // proof it is the ACCUMULATION and not a per-leg cap: 10_000 units (50_000 + 30_000 =
        // 80_000 ≤ 100_000) still passes.
        let v = RiskGate::new(limits()).check_combo(
            &combo_limit(1, 10_000.0, 20.0),
            &ctx,
            marks_with_equity,
        );
        assert!(v.ok, "{v:?}");
    }

    #[test]
    fn repeated_leg_symbol_accumulates_exposure_instead_of_double_measuring() {
        // `ComboSpec::validate` does NOT reject a duplicate leg symbol, and each leg got a fresh
        // lctx, so the exposure cap measured the SAME 0-position twice instead of the sum.
        let mut req = combo_limit(1, 10.0, 20.0);
        req.combo_legs[1].symbol = LEG_A.into();
        req.combo_legs[1].ratio = 1; // both legs BUY 10 of LEG_A ⇒ projected 20 @ 50 = 1_000
        let ctx = RiskContext::default();
        // cap 750: leg 1 alone projects 500 (passes), the pair projects 1_000 (must deny)
        let v = RiskGate::new(RiskLimits { max_total_exposure: Some(750.0), ..RiskLimits::new() })
            .check_combo(&req, &ctx, leg_marks);
        assert!(!v.ok, "the repeated symbol must accumulate: {v:?}");
        assert_eq!(v.reason, format!("leg {LEG_A}: over-max-exposure"));
        // 1_100 clears the accumulated projection
        let v =
            RiskGate::new(RiskLimits { max_total_exposure: Some(1_100.0), ..RiskLimits::new() })
                .check_combo(&req, &ctx, leg_marks);
        assert!(v.ok, "{v:?}");
    }

    #[test]
    fn reduce_only_combo_does_not_launder_an_opening_leg() {
        // MAJOR: `leg_req` inherited `reduce_only`, and `pure_reduce` is `req.reduce_only || ..`,
        // so EVERY leg skipped buying power and passed the `Reducing` gate — including a leg
        // opening a brand-new position in an instrument never held.
        let mut req = combo_limit(1, 100.0, 20.0);
        req.reduce_only = true;
        // long LEG_A (the +1 leg genuinely reduces nothing — it BUYS more), flat LEG_B.
        let per_leg = |sym: &str| match sym {
            LEG_A => RiskContext {
                mark_price: 50.0,
                position_size: -1_000.0, // short ⇒ the BUY leg really is a reduce
                equity: 10_000.0,
                ..RiskContext::default()
            },
            _ => RiskContext { mark_price: 30.0, equity: 10_000.0, ..RiskContext::default() },
        };
        // LEG_B is SOLD (ratio −1) into a FLAT book — a brand-new short, margin 100×30×0.1 = 300
        // against equity 10_000 ⇒ fits. Tighten equity so the opening leg cannot afford it: only
        // reachable at all if reduce_only is NOT inherited.
        let poor = move |sym: &str| RiskContext { equity: 100.0, ..per_leg(sym) };
        let v = RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() })
            .check_combo(&req, &RiskContext::default(), poor);
        assert!(!v.ok, "an opening leg inside a reduce_only combo must not bypass margin: {v:?}");
        assert_eq!(v.reason, format!("leg {LEG_B}: insufficient-margin"));

        // ...and the genuinely-reducing leg (A, buying back a short) is still treated as a reduce:
        // it is checked FIRST and did not deny.
        let v = RiskGate::new(RiskLimits {
            im_requirement: Some(0.1),
            block_reduce_only_overshoot: true,
            ..RiskLimits::new()
        })
        .check_combo(&req, &RiskContext::default(), poor);
        assert_eq!(v.reason, format!("leg {LEG_B}: insufficient-margin"), "{v:?}");
    }

    #[test]
    fn combo_denies_infinite_qty_and_zero_ratio_with_its_own_reason() {
        let ctx = RiskContext::default();
        let mut gate = RiskGate::new(RiskLimits::new());
        // INFINITY is not caught by `<= 0.0`: an infinite leg notional sails through an UNARMED
        // notional cap, which is the default.
        assert_eq!(
            gate.check_combo(&combo_limit(1, f64::INFINITY, 20.0), &ctx, leg_marks).reason,
            "non-positive-size"
        );
        // a 0 ratio used to surface as the misleading `leg X: invalid-side`
        let mut req = combo_limit(1, 1.0, 20.0);
        req.combo_legs[0].ratio = 0;
        assert_eq!(
            gate.check_combo(&req, &ctx, leg_marks).reason,
            format!("leg {LEG_A}: zero-ratio")
        );
        assert!(gate.throttle_times().is_empty());
    }

    #[test]
    fn plain_orders_are_untouched_by_the_throttle_split() {
        // The single-order path must stay byte-identical: same admissions, same window state.
        let lim = || RiskLimits { max_orders_per_window: Some(2), ..RiskLimits::new() };
        let ctx = RiskContext { mark_price: 100.0, now_ms: 500, ..RiskContext::default() };
        let mut gate = RiskGate::new(lim());
        assert!(gate.check(&market(1, 1.0), &ctx).ok);
        assert!(gate.check(&market(-1, 1.0), &ctx).ok);
        let v = gate.check(&market(1, 1.0), &ctx);
        assert!(!v.ok && v.reason == "rate-limited", "{v:?}");
        assert_eq!(gate.throttle_times(), vec![500, 500]);
        // the window still slides exactly as before (window_ms = 1000)
        let later = RiskContext { now_ms: 1600, ..ctx };
        assert!(gate.check(&market(1, 1.0), &later).ok);
        assert_eq!(gate.throttle_times(), vec![1600]);
    }

    #[test]
    fn impact_denial_does_not_consume_a_throttle_slot() {
        let ctx = RiskContext { mark_price: 100.0, now_ms: 0, ..RiskContext::default() };
        let b = book();
        let mut gate = RiskGate::new(RiskLimits {
            max_slippage_bps: Some(60.0),
            max_orders_per_window: Some(1),
            ..RiskLimits::new()
        });
        let d = gate.check_with_book(&market(1, 3.0), &ctx, Some(&b));
        assert!(!d.ok && d.reason == "impact-over-slippage-budget");
        assert!(gate.throttle_times().is_empty(), "a vetoed order must not burn a rate slot");
        // the slot is still available to a within-budget order
        assert!(gate.check_with_book(&market(1, 1.0), &ctx, Some(&b)).ok);
    }

    // ---- fat-finger price collar (OPT-IN) ----

    /// `pct` fraction + absolute floor, as the venue-wide default (no per-symbol rows).
    fn collared(pct: f64, abs_floor: f64) -> RiskLimits {
        RiskLimits { price_collar: Some(PriceCollar { pct, abs_floor }), ..RiskLimits::new() }
    }

    /// A stop/take-profit carries `trigger_price` and NO `price` — the collar must judge it too.
    fn trigger_order(side: i32, qty: f64, trigger: f64) -> OrderRequest {
        OrderRequest {
            order_type: "stop".to_string(),
            trigger_price: Some(trigger),
            ..market(side, qty)
        }
    }

    #[test]
    fn price_collar_denies_a_fat_finger_in_both_directions() {
        // mark 100, band = max(0.05 * 100, 0.0) = 5.0
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        // 10x UP — the classic mis-scale. EVERY pre-existing axis admits it (no cap is set, and
        // its notional would fit under an ordinary one anyway): this is the hole the axis closes.
        let mut g = RiskGate::new(collared(0.05, 0.0));
        let v = g.check(&limit(1, 1.0, 1_000.0), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a 10x-up limit must deny: {v:?}");
        // 10x DOWN — a sell at a tenth of the mark is the same fat finger
        let v = g.check(&limit(-1, 1.0, 10.0), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a 10x-down limit must deny: {v:?}");
        // a hair outside the band on each side
        let v = g.check(&limit(1, 1.0, 105.01), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "just above the band: {v:?}");
        let v = g.check(&limit(-1, 1.0, 94.99), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "just below the band: {v:?}");
        // a TRIGGER price is judged the same way (a stop carries price = None)
        let v = g.check(&trigger_order(1, 1.0, 200.0), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a fat-finger trigger must deny: {v:?}");
        // a non-finite price is denied outright, never silently admitted (`NaN > band` is false)
        let v = g.check(&limit(1, 1.0, f64::NAN), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a NaN price must deny: {v:?}");

        // a collar denial must not burn a rate slot — the axis runs BEFORE the throttle, like
        // the margin and impact vetoes.
        let mut lim = collared(0.05, 0.0);
        lim.max_orders_per_window = Some(1);
        let mut t = RiskGate::new(lim);
        assert!(!t.check(&limit(1, 1.0, 1_000.0), &ctx).ok);
        assert!(t.throttle_times().is_empty(), "a collared order must not burn a rate slot");
        assert!(t.check(&limit(1, 1.0, 100.0), &ctx).ok, "the slot must still be available");
    }

    #[test]
    fn price_collar_allows_inside_the_band() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let mut g = RiskGate::new(collared(0.05, 0.0));
        for px in [96.0, 100.0, 104.0] {
            let v = g.check(&limit(1, 1.0, px), &ctx);
            assert!(v.ok, "price {px} is inside the +/-5 band: {v:?}");
        }
        // band-EQUAL passes on both edges (the comparison is strict `>`)
        let v = g.check(&limit(1, 1.0, 105.0), &ctx);
        assert!(v.ok, "the upper band edge must pass: {v:?}");
        let v = g.check(&limit(-1, 1.0, 95.0), &ctx);
        assert!(v.ok, "the lower band edge must pass: {v:?}");
        // an in-band trigger passes too
        let v = g.check(&trigger_order(-1, 1.0, 96.0), &ctx);
        assert!(v.ok, "an in-band trigger must pass: {v:?}");
        // a MARKET order carries neither price nor trigger => never collared, at any band
        let mut zero = RiskGate::new(collared(0.0, 0.0));
        let v = zero.check(&market(1, 1.0), &ctx);
        assert!(v.ok, "a market order has no price to collar: {v:?}");
    }

    /// Why BOTH halves of the band are required: on a cheap instrument (the Polymarket shape — a
    /// 0.02 mark) a pure percentage collar denies ordinary quoting, so the absolute floor must
    /// dominate. Above it the percentage takes back over.
    #[test]
    fn price_collar_absolute_floor_dominates_at_a_tiny_mark() {
        let ctx = RiskContext { mark_price: 0.02, ..RiskContext::default() };
        // pct alone: 10% of 0.02 = 0.002, so an ordinary 0.025 quote would be DENIED
        let v = RiskGate::new(collared(0.10, 0.0)).check(&limit(1, 100.0, 0.025), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a pct-only collar is too tight: {v:?}");
        // with a 0.01 absolute floor the band is max(0.002, 0.01) = 0.01 => 0.025 is admitted
        let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 100.0, 0.025), &ctx);
        assert!(v.ok, "the absolute floor must widen the band at a tiny mark: {v:?}");
        // ...and the mis-scale this axis exists for is STILL caught: 0.55 -> 5.5 in miniature,
        // here 0.02 -> 0.055, which is 0.035 away
        let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 100.0, 0.055), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a mis-scaled 0.055 must deny: {v:?}");
        // the DOWN mis-scale (0.02 -> 0.002) likewise
        let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(-1, 100.0, 0.002), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "a mis-scaled 0.002 must deny: {v:?}");
        // symmetrically, on an EXPENSIVE mark the percentage dominates that same 0.01 floor:
        // band = max(0.10 * 50_000, 0.01) = 5_000, so 52_000 is admitted.
        let rich = RiskContext { mark_price: 50_000.0, ..RiskContext::default() };
        let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 1.0, 52_000.0), &rich);
        assert!(v.ok, "the percentage must govern an expensive mark: {v:?}");
        // the band fn itself, pinned at both ends
        let c = PriceCollar { pct: 0.10, abs_floor: 0.01 };
        assert_eq!(c.band(50_000.0), 5_000.0);
        assert_eq!(c.band(0.02), 0.01);
    }

    /// An UNPRICED mark skips the axis entirely — the mount's readiness gate guarantees
    /// priced-before-order-flow, so denying here would be a pure false positive.
    #[test]
    fn price_collar_skips_an_unpriced_mark() {
        for mark in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let ctx = RiskContext { mark_price: mark, ..RiskContext::default() };
            let v = RiskGate::new(collared(0.05, 0.0)).check(&limit(1, 1.0, 1_000.0), &ctx);
            assert!(v.ok, "an unpriced mark ({mark}) must skip the collar: {v:?}");
        }
    }

    /// A COMBO's `price` is the SIGNED NET across legs, not a price in any single instrument's
    /// mark units — collaring it would deny every combo (a credit net is NEGATIVE). Its LEGS are
    /// not silently un-checked either: `check_combo` clears each leg's price/trigger, so a leg
    /// carries nothing to collar and prices off its own mark exactly as it does today.
    #[test]
    fn price_collar_never_judges_a_combo_net_price() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        // a +20 debit net and a -20 credit net are both far outside a +/-5 band around 100
        let v = RiskGate::new(collared(0.05, 0.0)).check(&combo_limit(1, 2.0, 20.0), &ctx);
        assert!(v.ok, "a debit combo net must not be collared: {v:?}");
        let v = RiskGate::new(collared(0.05, 0.0)).check(&combo_limit(-1, 2.0, -20.0), &ctx);
        assert!(v.ok, "a credit combo net must not be collared: {v:?}");
        // the combo entry point is likewise unaffected
        let mut g = RiskGate::new(collared(0.05, 0.0));
        let v = g.check_combo(&combo_limit(1, 2.0, 20.0), &RiskContext::default(), leg_marks);
        assert!(v.ok, "check_combo must be unaffected by an armed collar: {v:?}");
    }

    /// Per-symbol override, mirroring `im_by_symbol`/`im_for`: the map wins where it has a row,
    /// the venue default covers everything else.
    #[test]
    fn price_collar_per_symbol_override_mirrors_im_by_symbol() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let row = PriceCollar { pct: 0.05, abs_floor: 0.0 };
        let tight = PriceCollar { pct: 0.0, abs_floor: 0.0 };
        let mut map: indexmap::IndexMap<String, PriceCollar> = indexmap::IndexMap::new();
        map.insert("BTCUSDT".to_string(), row);

        // no venue default at all — only a BTCUSDT row (the symbol `market()`/`limit()` build)
        let only_btc = RiskLimits { collar_by_symbol: map.clone(), ..RiskLimits::new() };
        assert_eq!(only_btc.collar_for("BTCUSDT"), Some(row));
        assert_eq!(only_btc.collar_for("ETHUSDT"), None);
        let v = RiskGate::new(only_btc.clone()).check(&limit(1, 1.0, 1_000.0), &ctx);
        assert!(!v.ok && v.reason == "price-collar", "the BTC row must arm the axis: {v:?}");
        // an uncovered symbol with no venue default stays unarmed
        let mut eth = limit(1, 1.0, 1_000.0);
        eth.symbol = "ETHUSDT".to_string();
        let v = RiskGate::new(only_btc).check(&eth, &ctx);
        assert!(v.ok, "a symbol with no row and no default must be unarmed: {v:?}");

        // the per-symbol row OVERRIDES a brutally tight venue default...
        let mixed =
            RiskLimits { price_collar: Some(tight), collar_by_symbol: map, ..RiskLimits::new() };
        assert_eq!(mixed.collar_for("BTCUSDT"), Some(row));
        assert_eq!(mixed.collar_for("ETHUSDT"), Some(tight));
        let v = RiskGate::new(mixed.clone()).check(&limit(1, 1.0, 104.0), &ctx);
        assert!(v.ok, "the per-symbol row must override the venue default: {v:?}");
        // ...while the tight default still governs every OTHER symbol
        let mut eth2 = limit(1, 1.0, 104.0);
        eth2.symbol = "ETHUSDT".to_string();
        let v = RiskGate::new(mixed).check(&eth2, &ctx);
        assert!(!v.ok && v.reason == "price-collar", "the default must govern ETH: {v:?}");
    }

    /// THE OFF TEST — the byte-identical-when-unset guarantee, both halves:
    /// (a) with the axis `None`/empty an order that WOULD trip it is admitted, and
    /// (b) neither new key reaches the canonical JSON, so `EngineSnapshot`'s `state_hash` (the
    ///     journal determinism fence) is unchanged — see
    ///     `engine_snapshot::tests::state_hash_of_a_default_limits_snapshot_is_pinned`.
    #[test]
    fn price_collar_off_is_byte_identical() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let off = RiskLimits::new();
        assert_eq!(off.price_collar, None);
        assert!(off.collar_by_symbol.is_empty());
        assert_eq!(off.collar_for("BTCUSDT"), None);
        // a 100x limit, a 100x trigger and a NaN price — all admitted with the axis off
        let cases =
            [limit(1, 1.0, 10_000.0), trigger_order(1, 1.0, 10_000.0), limit(1, 1.0, f64::NAN)];
        for req in cases {
            let v = RiskGate::new(RiskLimits::new()).check(&req, &ctx);
            assert!(v.ok, "the OFF path must admit exactly as before: {req:?} -> {v:?}");
        }
        // the serialized shape is untouched: no key, not even a null
        let json = serde_json::to_string(&RiskLimits::new()).unwrap();
        assert!(
            !json.contains("price_collar") && !json.contains("collar_by_symbol"),
            "an off collar must not reach the canonical JSON: {json}"
        );
        // `Default` (not just `new()`) is off too — it is what serde reconstructs a pre-collar
        // journal record into, and the round trip must land back on the same value.
        let d = RiskLimits::default();
        assert_eq!(d.price_collar, None);
        assert!(d.collar_by_symbol.is_empty());
        let round: RiskLimits = serde_json::from_str(&json).unwrap();
        assert_eq!(round, RiskLimits::new());
    }

    /// REGRESSION (review major #1): an ARMED COLLAR MUST NOT DENY A PROTECTIVE BRACKET EXIT.
    /// `vike_model::build_bracket` emits its stop-loss (`order_type: "stop"`, `trigger_price`) and
    /// take-profit (`limit`, `price`) legs DELIBERATELY far from the mark — that distance IS the
    /// protection — so collaring them would veto exactly the order that limits the loss. The
    /// covered-reduce bypass (the codebase's standing anti-stranding rule) is what prevents it.
    #[test]
    fn price_collar_never_denies_a_protective_bracket_exit() {
        use vike_model::BracketSpec;
        // long 1.0 at a mark of 100, under a brutally tight +/-2 collar
        let held = RiskContext { position_size: 1.0, mark_price: 100.0, ..RiskContext::default() };
        // a MARKET entry (no price to collar), a stop 10 below the mark and a take-profit 20
        // above it — both exits 5x/10x outside the +/-2 band, BY DESIGN.
        let spec = BracketSpec {
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 1.0,
            entry_price: None,
            stop_loss: 90.0,
            take_profit: 120.0,
        };
        let [entry, sl, tp] = vike_model::build_bracket(&spec, "e1", "s1", "t1");
        // the exits are reduce-only, opposite-side and covered by the held position
        assert!(sl.reduce_only && sl.trigger_price == Some(90.0) && sl.order_type == "stop");
        assert!(tp.reduce_only && tp.price == Some(120.0) && tp.order_type == "limit");
        for leg in [&entry, &sl, &tp] {
            let v = RiskGate::new(collared(0.02, 0.0)).check(leg, &held);
            assert!(v.ok, "an armed collar must not deny a bracket leg: {leg:?} -> {v:?}");
        }

        // ...and the bypass is POSITION-COVERED, not flag-trusting: on a FLAT book the very same
        // legs open exposure (nothing server-side reduces either), so the collar still bites.
        let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        for leg in [&sl, &tp] {
            let v = RiskGate::new(collared(0.02, 0.0)).check(leg, &flat);
            assert!(
                !v.ok && v.reason == "price-collar",
                "a flat-book 'reduce_only' leg is an OPENING order and must stay collared: {v:?}"
            );
        }

        // the OPENING leg is never bypassed either: a fat-fingered LIMIT entry still denies.
        let mut far_entry = spec.clone();
        far_entry.entry_price = Some(1_000.0);
        let [bad_entry, _, _] = vike_model::build_bracket(&far_entry, "e2", "s2", "t2");
        let v = RiskGate::new(collared(0.02, 0.0)).check(&bad_entry, &held);
        assert!(!v.ok && v.reason == "price-collar", "a fat-finger entry must deny: {v:?}");
    }

    /// A garbage collar config must FAIL OPEN, never closed. Pre-fix, `band()` propagated a
    /// negative/`NaN` band straight into `|p - mark| > band` — always true — so the opt-in
    /// fat-finger knob silently became a TOTAL KILL SWITCH denying every priced order.
    #[test]
    fn price_collar_garbage_config_is_never_a_kill_switch() {
        let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
        let garbage = [
            PriceCollar { pct: f64::NAN, abs_floor: 0.0 },
            PriceCollar { pct: -0.5, abs_floor: 0.0 },
            PriceCollar { pct: 0.0, abs_floor: f64::NAN },
            PriceCollar { pct: 0.0, abs_floor: -1.0 },
            PriceCollar { pct: f64::NAN, abs_floor: f64::NAN },
            PriceCollar { pct: -0.5, abs_floor: -1.0 },
            PriceCollar { pct: f64::NEG_INFINITY, abs_floor: f64::NEG_INFINITY },
        ];
        for c in garbage {
            let band = c.band(100.0);
            assert!(band.is_finite() && band >= 0.0, "{c:?} must yield a sane band, got {band}");
            let lim = RiskLimits { price_collar: Some(c), ..RiskLimits::new() };
            // an AT-THE-MARK limit — the order a kill switch would have denied — is admitted
            let v = RiskGate::new(lim.clone()).check(&limit(1, 1.0, 100.0), &ctx);
            assert!(v.ok, "{c:?} must not deny an at-the-mark limit: {v:?}");
            // so is an at-the-mark trigger, and a market order (no price at all)
            let v = RiskGate::new(lim.clone()).check(&trigger_order(-1, 1.0, 100.0), &ctx);
            assert!(v.ok, "{c:?} must not deny an at-the-mark trigger: {v:?}");
            let v = RiskGate::new(lim).check(&market(1, 1.0), &ctx);
            assert!(v.ok, "{c:?} must not deny a market order: {v:?}");
        }
        // a garbage half does not poison a VALID half: the surviving component still governs
        assert_eq!(PriceCollar { pct: f64::NAN, abs_floor: 0.02 }.band(100.0), 0.02);
        assert_eq!(PriceCollar { pct: -0.5, abs_floor: 0.02 }.band(100.0), 0.02);
        // ...and the two WHOLLY-garbage shapes — the ones that pre-fix produced a `NaN` and a
        // NEGATIVE band, i.e. the actual kill switch — collapse to the TIGHTEST LEGAL band (0.0),
        // exactly what an explicit `PriceCollar { pct: 0.0, abs_floor: 0.0 }` already means here.
        assert_eq!(PriceCollar { pct: f64::NAN, abs_floor: f64::NAN }.band(100.0), 0.0);
        assert_eq!(PriceCollar { pct: -0.5, abs_floor: -1.0 }.band(100.0), 0.0);
    }
}
