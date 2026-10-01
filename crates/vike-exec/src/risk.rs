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
//! # The ACCOUNT-aggregate exposure ceiling (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::max_account_exposure`] is the axis [`RiskLimits::max_total_exposure`] is named
//! for and has never been: a cap on the WHOLE ACCOUNT's projected gross open notional, not on one
//! symbol's. The hole it closes is arithmetic rather than subtle — the per-symbol cap is evaluated
//! once per symbol with no memory of the others, so a strategy inside its cap on ten symbols is
//! inside nothing at the account, and an operator who sized that number for a whole book got a
//! ceiling ten times looser than the one they believed they wrote. NautilusTrader's risk engine
//! checks an order against the ACCOUNT's free balance and initial margin; this is that axis.
//!
//! **The two lanes are independent and both may deny; they carry DIFFERENT reasons**
//! (`"over-max-exposure"` for the symbol, `"over-account-exposure"` for the account) precisely so a
//! refusal says which ceiling stopped it. A shared reason would send an operator to re-size the
//! wrong number. The account one also carries the projected total and the ceiling itself, because
//! the order that trips an account ceiling is usually unremarkable and the exposure that trips it
//! is in symbols the operator is not looking at — the lane's own comment argues it.
//!
//! **What "the account" is here.** The sum is over the positions of THIS engine's own account —
//! `vike_mount::make_engine_for_account` builds one `ExecutionEngine`, and so one `RiskGate` and
//! one copy of this cap, per `(venue, account)`, addressed by
//! `vike_exec::ExecutionEngine::route_key`. So a SECOND, labelled account of the same venue
//! (`BYBIT_LIVE_API_KEY__ALT`) is a different engine holding a different book and gets this budget
//! SEPARATELY. **The unit is therefore the ENGINE — one `(venue, AccountLabel)` — and that is the
//! same thing as a wallet only while two labels really are two books at the venue.**
//!
//! ⚠ **Where they are NOT, the ceiling multiplies, and the tree detects that case rather than
//! refusing it.** `vike_config::venue_accounts`' shared-BOOK rule (an agent key signing for a
//! master whose own key is also configured, or one credential set pasted under two labels) is
//! REPORTED and both engines mount anyway, by `docs/decisions/0013-degrade-vs-refuse.md`. Each
//! then carries its own copy of this cap over its own half of ONE venue ledger, so the real book
//! may hold a MULTIPLE of the number the operator wrote — the same N×-looser defect this axis
//! exists to close, wearing the account label instead of the symbol label. It is a DECLARED
//! residual, not an oversight: `vike_mount::make_engine_accounts`' shared-book `warn!` names this
//! ceiling and its value when it is armed (`vike_mount::shared_book_ceiling_note`), so an operator
//! meets the multiplication at startup rather than after a fill.
//!
//! A cross-VENUE total is deliberately NOT what this measures — that is not an account, it is a
//! portfolio, and no engine holds it.
//!
//! **How the number is assembled** (`ExecutionEngine::risk_ctx`, the producer):
//! [`RiskContext::account_exposure_excl_order`] is this account's gross exposure with the ORDER
//! UNDER JUDGEMENT left out — every other symbol's open position, plus every live un-filled order
//! but this one — and the gate adds the order symbol's own PROJECTED notional, the very term the
//! per-symbol lane computes, back on. Splitting it that way is what makes the comparison answer
//! "what will this account hold AFTER this order", rather than the pre-order world plus an order.
//! Counting the RESTING orders is what stops N orders submitted inside one fill window from each
//! being judged as though the others committed nothing (`ExecutionEngine::live_order_margin`
//! carries that incident from the buying-power lane).
//!
//! ⚠ **A COVERED REDUCE BYPASSES THIS AXIS ENTIRELY, and unlike the per-symbol lane's silence on
//! the question that is deliberate.** An account already over its ceiling — the ordinary state
//! after an operator LOWERS the number, or after a mark moves — must still be closable, and
//! `docs/ops/kill-switches.md`'s standing law is that a ceiling may never trap you in a position.
//! `max_total_exposure` gets away without the bypass because a same-symbol reduce always projects
//! smaller; an ACCOUNT total does not shrink at all in the other nine symbols, so without the
//! bypass the first flatten leg of a panic exit would be refused by the ceiling it is trying to
//! get back under. `covered_reduce` is the same predicate every other bypass in this ladder reads.
//!
//! `None` — the default, and every deployment that writes no `max_account_exposure` line — means
//! the axis does not exist: no sum is folded (the producer skips the fold entirely), no comparison
//! runs, and the serialized [`RiskLimits`] is byte-identical, so
//! [`crate::engine_snapshot::state_hash`] and every recorded journal are unaffected.
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
    /// An account-AGGREGATE ceiling is a genuinely different lane, not a rename, and it now
    /// EXISTS beside this one as [`RiskLimits::max_account_exposure`] — with its own denial reason,
    /// its own covered-reduce bypass and its own producer. It did not need the position book inside
    /// `check`, which is what this paragraph used to say made it impractical: the fold is done ONCE
    /// per order by the producer (`ExecutionEngine::risk_ctx`, the same cold per-order path that
    /// already folds equity and margin-in-use), and reaches the gate as ONE scalar on the `Copy`
    /// [`RiskContext`]. Nothing was added to the per-message fold the `p99 < 10µs` gate measures.
    pub max_total_exposure: Option<f64>,
    /// **Cap on the WHOLE ACCOUNT's projected gross open notional** — the axis
    /// [`RiskLimits::max_total_exposure`] is NAMED for and is not.
    ///
    /// Evaluated by `check_inner`'s `over-account-exposure` lane as `projected +
    /// ctx.account_exposure_excl_order > cap`, where `projected` is
    /// `|ctx.position_size + side × qty| × ctx.mark_price × ctx.multiplier` — the order symbol's own
    /// projected notional (the per-symbol lane's identical term) plus every OTHER symbol's position
    /// and every live un-filled order of this account, gross. See this module's doc for what "the
    /// account" means (one `(venue, AccountLabel)` ENGINE — a labelled second account of the venue
    /// gets its own budget, and where `vike_config::venue_accounts`' shared-BOOK rule fires that is
    /// a declared residual rather than a wallet) and for why the axis BYPASSES a covered reduce
    /// where its per-symbol sibling does not.
    ///
    /// **It can only ever REFUSE.** There is no arm in this gate that admits an order because this
    /// number is set; `None` (the default) skips the comparison, and the producer skips the fold
    /// that feeds it, so an absent ceiling is byte-identical to the gate before this field existed.
    ///
    /// The operator writes it in `<project>/settings/policy.toml` as `max_account_exposure`, NOT in
    /// a run profile's `[risk]` table — `vike_config::Policy::max_account_exposure` carries the
    /// argument for that home, and `vike_mount::MountPolicy` is the projection that brings it here.
    /// That is the one structural difference from `max_total_exposure`, whose value arrives from
    /// `crate::ProfileRisk`: an account ceiling is a property of the BOX and its wallet, not of the
    /// strategy run that happens to be pointed at it.
    ///
    /// ⚠ It is NOT part of `vike_mount::require_live_risk_budget`'s refusal, deliberately. That
    /// refusal names the two caps a live mount has always demanded, and adding a third would stop
    /// every existing live deployment from starting on upgrade — a ceiling that arrives by breaking
    /// the daemon is not the way a ceiling arrives. Absent stays absent, and loudly so: the key is
    /// documented in `docs/ops/kill-switches.md` and disclosed in `vike-cli config show`.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING for exactly the reason spelled out on
    /// [`RiskLimits::max_slippage_bps`] — [`RiskLimits`] is embedded in [`crate::EngineSnapshot`],
    /// whose canonical `serde_json` bytes feed [`crate::engine_snapshot::state_hash`], the journal
    /// determinism fence compared ACROSS BINARY VERSIONS. Off ⇒ absent ⇒ same bytes ⇒ same hash, so
    /// a journal recorded before this field existed still replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_account_exposure: Option<f64>,
    /// **The largest EQUITY FIGURE this engine's sizing and admission lanes may see**, in quote
    /// currency. `None` — the default — leaves [`crate::ExecutionEngine::sizing_equity`]
    /// bit-identical to [`crate::ExecutionEngine::resolved_equity`], so an engine that was never
    /// handed a ceiling behaves exactly as it did before this field existed.
    ///
    /// ⚠ **It is not a limit lane, and `check()` never reads it.** Every other field on this struct
    /// is a comparison the gate performs; this one is a bound on an INPUT the gate (and the sizer
    /// above it) is fed. It lives here because this is the per-engine risk configuration the mount
    /// already folds a policy onto, and because the fold is a `min`
    /// ([`RiskLimits::narrow_sizing_equity`]) exactly like [`RiskLimits::max_account_exposure`]'s.
    ///
    /// The operator writes it in `<project>/settings/policy.toml` as `max_sizing_equity`, NOT in a
    /// run profile's `[risk]` table — `vike_config::Policy::max_sizing_equity` carries the argument
    /// for that home (the same one the account ceiling's makes: it is a property of the BOX and the
    /// wallet its credentials open, not of the strategy run pointed at it), and
    /// `vike_mount::MountPolicy` is the projection that brings it here.
    ///
    /// ⚠ It is NOT part of `vike_mount::require_live_risk_budget`'s refusal, for the reason
    /// [`RiskLimits::max_account_exposure`]'s doc gives: a third mandatory ceiling would stop every
    /// existing live deployment from starting on upgrade.
    ///
    /// SERDE: `skip_serializing_if` is LOAD-BEARING for exactly the reason spelled out on
    /// [`RiskLimits::max_slippage_bps`] — [`RiskLimits`] is embedded in [`crate::EngineSnapshot`],
    /// whose canonical `serde_json` bytes feed [`crate::engine_snapshot::state_hash`], the journal
    /// determinism fence compared ACROSS BINARY VERSIONS. Off ⇒ absent ⇒ same bytes ⇒ same hash, so
    /// a journal recorded before this field existed still replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_sizing_equity: Option<f64>,
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
    /// changes that shape and, by `vike_journal`'s own version contract, would force
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

    /// **Fold an ACCOUNT ceiling in so that it can only ever NARROW** — `min` when both sides carry
    /// a number, the one that exists when only one does, `None` when neither.
    ///
    /// The one way [`RiskLimits::max_account_exposure`] is armed
    /// (`vike_mount::make_engine_for_account`, and the paper assembly beside it) goes through here
    /// rather than assigning the field, and that is the whole point: "it can only ever refuse" must
    /// be a property of the OPERATION, not of nobody else happening to write the field. A plain
    /// assignment is inert only while every other writer is absent — a later venue-specific
    /// conservative default, or a second fold, would be silently erased by it, including a `Some`
    /// overwritten with `None`. `vike_config::VenueMode::cap` is the precedent this copies: that
    /// one is a `min` by construction and is cited as the model in this field's own doc, so the
    /// arming site had better be one too.
    pub fn narrow_account_exposure(&mut self, cap: Option<f64>) {
        self.max_account_exposure = match (self.max_account_exposure, cap) {
            (Some(held), Some(incoming)) => Some(held.min(incoming)),
            (held, incoming) => held.or(incoming),
        };
    }

    /// **Fold a SIZING-EQUITY ceiling in so that it can only ever LOWER the figure** — the exact
    /// twin of [`Self::narrow_account_exposure`], and a `min` for the identical reason: "it can
    /// only ever make a decision more conservative" must be a property of the OPERATION, not of
    /// nobody else happening to write the field. The one arming site
    /// (`vike_mount::make_engine_for_account`, and the paper assembly beside it) goes through here
    /// rather than assigning [`RiskLimits::max_sizing_equity`].
    ///
    /// ⚠ **"Lower" is conservative HERE and nowhere else on this struct**, which is why the seam
    /// that consumes this field is a separate resolver rather than a substitution:
    /// [`crate::ExecutionEngine::sizing_equity`] applies it and
    /// [`crate::ExecutionEngine::resolved_equity`] — what the margin-CALL sweep judges against —
    /// does not. A `min` that reached the liquidation decision would be the widening fold's mirror
    /// image: strictly destructive rather than strictly safe.
    pub fn narrow_sizing_equity(&mut self, cap: Option<f64>) {
        self.max_sizing_equity = match (self.max_sizing_equity, cap) {
            (Some(held), Some(incoming)) => Some(held.min(incoming)),
            (held, incoming) => held.or(incoming),
        };
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
    /// **This ACCOUNT's gross exposure with the ORDER UNDER JUDGEMENT left out** — the other half
    /// of [`RiskLimits::max_account_exposure`]'s comparison, `0.0` when that ceiling is unarmed.
    ///
    /// TWO terms, both gross, never netting a long against a short:
    /// * Σ `|size| × resolver-price × multiplier` over every OPEN POSITION of this engine's own
    ///   account except the order symbol's, and
    /// * Σ `remaining × resolver-price × multiplier` over every LIVE, UN-FILLED ORDER of this
    ///   engine except the one being judged.
    ///
    /// `ExecutionEngine::resolved_account_exposure_excluding` is the producer and the authority on
    /// every skip (a flat leg, an unpriceable one, a foreign-venue row, a covered reduce) — read
    /// them there, not here.
    ///
    /// ⚠ **The RESTING-ORDER term is what makes this a ceiling rather than a suggestion.** Without
    /// it, N orders submitted before any of them fills each see the same pre-order account and each
    /// passes, so the ceiling is exceeded by an arbitrary multiple — the identical hole
    /// `ExecutionEngine::live_order_margin` was written to close on the buying-power lane, and its
    /// doc carries that incident.
    ///
    /// ⚠ **The order's own SYMBOL's position is EXCLUDED because the gate re-adds it PROJECTED**
    /// (its resting orders are not: nothing projects those). Leaving the position in would measure
    /// the pre-order world and then add the order on top, double-counting the position the order is
    /// about — which for a reduce would report the account growing as it shrinks. The split is what
    /// makes the lane answer "what will this account hold after this order".
    ///
    /// ⚠ **The two halves use different bases for the one symbol they meet on, deliberately.**
    /// Everything here is GROSS (a hedge-mode LONG/SHORT pair sums to both legs), while the order
    /// symbol re-enters NET (`ExecutionEngine::gate_position_size` nets the hedge buckets). Net is
    /// the only projectable basis: `vike_model::OrderRequest` names no position bucket, so which
    /// one an order lands in is the venue's routing decision and the gate cannot know it. The
    /// difference is bounded by one symbol's hedged overlap and errs LOW there; nowhere else.
    ///
    /// `0.0` — every caller that never sets it, which is every path with the ceiling unarmed — is
    /// byte-identical to the gate before this field existed: an unarmed lane reads it never.
    pub account_exposure_excl_order: f64,
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
            // The account ceiling's other half. `0.0` is "this account holds nothing else", which
            // is the only safe default for a term that is ADDED to the projection: any non-zero
            // default would deny orders on an account the caller never described.
            account_exposure_excl_order: 0.0,
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
    ImpactDeny, TakeScope, clamp_leverage, fillable_veto, impact_veto, round_to,
    scoped_impact_veto, take_scope,
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
        if let Some(min_qty) = grid.min_qty
            && !covered_reduce
            && req.qty.abs() < min_qty
        {
            return Self::deny("below-min-qty");
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
        if let Some(min_notional) = grid.min_notional
            && !covered_reduce
            && notional < min_notional
        {
            return Self::deny("below-min-notional");
        }

        // per-order notional cap
        if let Some(cap) = lim.max_notional_per_order
            && notional > cap
        {
            return Self::deny("over-max-notional");
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
        //
        // The projection is computed ONCE here and shared with the ACCOUNT-aggregate lane directly
        // below, so the two ceilings can never disagree about what this order does to its own
        // symbol — a second copy of this expression is exactly how the account lane would drift
        // into judging a different order than the symbol lane judged.
        let projected_symbol_notional = (ctx.position_size + req.side as f64 * exec_qty).abs()
            * ctx.mark_price
            * ctx.multiplier;
        if let Some(cap) = lim.max_total_exposure
            && projected_symbol_notional > cap
        {
            return Self::deny("over-max-exposure");
        }

        // ---- the ACCOUNT-AGGREGATE exposure cap (OPT-IN, RUST-NATIVE; see the module doc) ----
        //
        // The lane `max_total_exposure`'s NAME promises and its scope has never delivered: this
        // account's projected GROSS exposure across every symbol, against one ceiling. The order
        // symbol's own projected term is the SAME number the per-symbol lane just judged;
        // `ctx.account_exposure_excl_order` is every OTHER symbol's position plus every live
        // un-filled order but this one, folded once by the producer on the cold per-order path
        // (nothing here walks a book, or a registry).
        //
        // ⚠ THE REASON IS ITS OWN (`"over-account-exposure"`, not `"over-max-exposure"`), and that
        // is the point of the lane rather than a nicety: the two ceilings are re-sized in different
        // files by different reasoning, so a refusal that named the wrong one would send the
        // operator to widen a number that was not stopping them.
        //
        // ⚠ PLACED AFTER the per-symbol lane on purpose. When an order breaches both, the NARROWER
        // ceiling is the more actionable answer — the operator's own instrument-level number is
        // wrong, and the account was going to refuse it anyway.
        //
        // ⚠ A COVERED REDUCE BYPASSES. Unlike the per-symbol lane (whose projection always shrinks
        // for a same-symbol reduce, so it cannot trap you) an ACCOUNT total is dominated by the
        // symbols this order does not touch: an account over its ceiling would refuse the very
        // flatten legs that bring it back under, which is the trap `docs/ops/kill-switches.md`
        // forbids outright. Same predicate as every other bypass in this ladder.
        //
        // ⚠ It is before the throttle and before the buying-power check, like every other veto
        // here: a denied order must never consume a rate slot.
        //
        // UNARMED (`None`, the default) nothing below runs and no field of `ctx` is read, so the
        // verdict for every existing deployment is byte-identical.
        //
        // ⚠ THE REASON CARRIES THE TWO NUMBERS, unlike every bare-token reason above it, and that
        // is deliberate. A per-symbol refusal is self-explanatory — the operator is looking at the
        // order that caused it. An ACCOUNT refusal is not: the order that trips it is frequently a
        // perfectly ordinary one, and the exposure that trips it is in symbols the operator is not
        // looking at, so `"over-account-exposure"` alone would leave them unable to tell a ceiling
        // that is too low from a book that is too big. The token stays FIRST and unchanged, so
        // `starts_with` still classifies it; the numbers ride behind it at fixed precision, which
        // is deterministic in Rust's float formatting and therefore replay-stable.
        if let Some(cap) = lim.max_account_exposure
            && !covered_reduce
        {
            let projected = projected_symbol_notional + ctx.account_exposure_excl_order;
            if projected > cap {
                return Self::deny(&format!(
                    "over-account-exposure (projected {projected:.2} > max_account_exposure {cap:.2})"
                ));
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
        if let Some(im_req) = lim.im_for(&req.symbol)
            && !covered_reduce
        {
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
        if let Some(b) = book
            && !covered_reduce
            && (lim.require_fillable || lim.max_slippage_bps.is_some())
        {
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
    ///   `ComboSpec::validate` does not reject, and `reduces()` judges the projected book);
    /// * **account-aggregate exposure** — each admitted leg's own gross notional is added to the
    ///   next leg's [`RiskContext::account_exposure_excl_order`]. Without this, `leg_ctx` reports
    ///   each leg the account as it stood BEFORE the combo, so N legs each fit under the same
    ///   unchanged account ceiling and the combo consumes N× what one leg was allowed — the
    ///   buying-power hole above, wearing the exposure axis. ⚠ On a REPEATED symbol this
    ///   OVER-counts (the earlier leg is in the running total AND in the later leg's projected
    ///   position), which is the direction this entry point already errs in by design: a combo must
    ///   never pass a gate its naked legs would fail.
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
        // …and the gross notional the admitted legs have already added to the ACCOUNT (see the doc
        // above). Folded into each later leg's `account_exposure_excl_order`, which `leg_ctx` can
        // only ever report as of BEFORE the combo.
        let mut committed_notional = 0.0f64;

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
                account_exposure_excl_order: base.account_exposure_excl_order + committed_notional,
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
            if !pure_reduce && let Some(im_req) = self.limits.im_for(&admitted.symbol) {
                committed_margin += vike_model::initial_margin(
                    lctx.mark_price,
                    signed_qty,
                    lctx.multiplier,
                    1.0,
                    im_req,
                )
                .abs();
            }
            // …and the ACCOUNT-exposure commitment, off the gate's OWN normalized qty and the same
            // leg mark/multiplier the exposure lanes just judged it on — one authority, never a
            // second formula. GROSS (`.abs()`), matching the account fold this adds to and the
            // stated combo policy that a spread is measured as its naked legs; a REDUCING leg adds
            // nothing, mirroring the margin term directly above (it frees exposure rather than
            // taking it, and the gate never credits what it cannot project).
            if !pure_reduce {
                committed_notional +=
                    vike_model::gross_notional(signed_qty, lctx.mark_price, lctx.multiplier);
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

#[path = "risk_tests.rs"]
#[cfg(test)]
mod risk_tests;
