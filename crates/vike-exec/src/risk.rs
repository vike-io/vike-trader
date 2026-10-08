//! The pre-trade RiskGate — one extensible stage every order crosses.
//! PURE: owns no bus, publishes nothing; the engine acts on the
//! verdict, so risk rules run identically in backtest / paper / live.
//!
//! PARITY: Python `round()` is round-half-to-EVEN (banker's) → `f64::round_ties_even`.
//!
//! RUST-NATIVE deviations from the (retired) Python oracle, both aligning the gate with
//! `SimBroker::apply_fill` (the backtest reference) so live and backtest verdicts agree:
//! * the min-qty / min-notional floors gate OPENING orders only — a covered reduce/close is exempt
//!   (the anti-stranding rule);
//! * notional includes the contract multiplier (`qty × ref_price × ctx.multiplier`), exactly as
//!   the gate's own margin calc and SimBroker's fill gate do (`multiplier` defaults to 1.0).
//!
//! # Pre-trade impact (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::max_slippage_bps`] / [`RiskLimits::require_fillable`] add a walk-the-book
//! veto over `vike_model::L2Book::simulate_fill`. **Where it runs is deliberate**: neither the
//! gate nor `ExecutionEngine` owns a book (`RiskContext` is a `Copy` scalar struct, and the L2
//! lane `vike_exec::lanes::BookUpdate` is folded by the vike-core runtime), and plumbing one into
//! `apply_intent` would put a map lookup on the per-order path of the single-writer fold for a
//! knob that is off by default. So [`impact_veto`] / [`fillable_veto`] are PURE free functions,
//! [`RiskGate::check_with_book`] is the book-aware entry point, and [`RiskGate::check`] is exactly
//! `check_with_book(.., None)`.
//!
//! **Caller contract:** the entry point is for a site holding BOTH the intent and a live book for
//! `(venue, symbol)`. **No such site exists today**: the runtime folds `BookUpdate` by value and
//! keeps no per-`(venue, symbol)` book map, so a strategy/mount with its own `on_order_book` view
//! is the cheaper first caller. Until one opts in the knobs are inert, and with both knobs off
//! `check_with_book` does no book work (not even a `simulate_fill`).
//!
//! **What the veto judges:** only the part of an order that can take DISPLAYED liquidity right
//! now ([`take_scope`]). Passive limits, stops and take-profits are never vetoed, and a covered
//! reduce is bypassed, mirroring the buying-power check.
//!
//! # Combo orders (OPT-IN entry point — combo-orders spec §5)
//!
//! [`RiskGate::check_combo`] crosses a multi-leg combo as an ATOMIC batch of synthetic per-leg
//! checks: all legs pass or the whole combo yields ONE denial naming the failing leg, and it
//! consumes exactly ONE throttle slot (it is one venue order). Legs ACCUMULATE, so a combo is
//! never admitted where the same legs sent sequentially would be refused. Both entry points share
//! `check_inner`; the ONLY difference is who consumes the rate slot.
//!
//! # Amending a PARTIALLY FILLED order (the `already_executed` parameter)
//!
//! [`RiskGate::check_modify`] judges the PROJECTED order, and on an IN-PLACE amend venue its qty is
//! the order's new TOTAL, executed part included — lots the position already holds, so
//! `position + side × qty` counted them TWICE. The caller-supplied `already_executed` is netted out
//! by [`still_executable`] in the lanes that project the post-order world and in NO other lane
//! (that function is the authority on which, and why). `0.0` on every non-amend path.
//!
//! ⚠ **The gate does not, and must not, derive that number itself.** It is a PER-VENUE fact
//! (`vike_model`'s `AmendSemantics`): a CANCEL-REPLACE venue (hyperliquid rests a whole fresh
//! order) must net NOTHING, and subtracting there would ADMIT an order this gate should refuse.
//! `ExecutionEngine::modify_order` owns the lookup; the gate stays pure and venue-agnostic.
//!
//! # Fat-finger price collar (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::price_collar`] (+ the per-symbol [`RiskLimits::collar_by_symbol`] override) is the
//! one axis that compares the order's own PRICE to the mark. Every size axis measures a MAGNITUDE,
//! so a limit BUY at 10× the mark (or a Polymarket `0.55` sent as `0.055`) passes them all. An
//! OPENING order carrying a limit or trigger price is DENIED (`"price-collar"`) when
//! `|price − ctx.mark_price|` exceeds `max(pct × mark, abs_floor)`, both prices TICK-ROUNDED so a
//! limit and the equivalent trigger get the same verdict at the band edge. **BOTH halves of the
//! band are required** ([`PriceCollar::band`] takes the MAX): a pure percentage denies ordinary
//! quoting on a `0.02` mark, and a pure absolute floor is useless on a `100_000` mark.
//!
//! Skips: an UNPRICED mark (non-finite or ≤ 0 — the mount's readiness gate guarantees
//! priced-before-order-flow, so a deny would be a pure false positive); a COMBO's net price (the
//! SIGNED net across legs, in no single instrument's mark units — [`RiskGate::check_combo`] clears
//! each leg's price, so legs carry none to collar); and ⚠ a COVERED REDUCE, which here is the
//! difference between a safety knob and a catastrophe: a protective bracket leg
//! (`vike_model::build_bracket`'s stop-loss / take-profit, `reduce_only: true`) is priced far from
//! the mark BY DESIGN, and collaring it would veto exactly the order that limits the loss. An
//! uncovered order OPENS exposure whatever the caller tagged it, and is collared.
//!
//! # The ACCOUNT-aggregate exposure ceiling (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::max_account_exposure`] caps the WHOLE ACCOUNT's projected gross open notional —
//! the axis [`RiskLimits::max_total_exposure`] is named for and is not: the per-symbol cap has no
//! memory of the other symbols, so a strategy inside it on ten symbols is inside nothing at the
//! account. **The two lanes are independent, both may deny, and they carry DIFFERENT reasons**
//! (`"over-max-exposure"` for the symbol, `"over-account-exposure"` for the account) so a refusal
//! says which ceiling stopped it; the account one also carries the projected total and the cap.
//!
//! **The unit is the ENGINE — one `(venue, AccountLabel)`.** `vike_mount::make_engine_for_account`
//! builds one `ExecutionEngine`, so one `RiskGate` and one copy of this cap, per account addressed
//! by `vike_exec::ExecutionEngine::route_key`. ⚠ **Where two labels share ONE venue book the
//! ceiling multiplies, and the tree detects that rather than refusing it**: the
//! `vike_config::venue_accounts` shared-BOOK rule is REPORTED and both engines mount
//! (`docs/decisions/0013-degrade-vs-refuse.md`). A DECLARED residual:
//! `vike_mount::make_engine_accounts`' shared-book `warn!` names this ceiling and its value when
//! armed (`vike_mount::shared_book_ceiling_note`). A cross-VENUE total is deliberately not
//! measured: that is a portfolio, and no engine holds it.
//!
//! **How the number is assembled** (`ExecutionEngine::risk_ctx`, the producer):
//! [`RiskContext::account_exposure_excl_order`] is this account's gross exposure with the ORDER
//! UNDER JUDGEMENT left out — every other symbol's position plus every live un-filled order but
//! this one — and the gate adds the order symbol's own PROJECTED notional back on, so the
//! comparison answers "what will this account hold AFTER this order". Counting the RESTING orders
//! stops N orders submitted inside one fill window from each being judged as though the others
//! committed nothing.
//!
//! ⚠ **A COVERED REDUCE BYPASSES THIS AXIS**, deliberately: an account over its ceiling (after an
//! operator LOWERS the number, or a mark moves) must still be closable, and an ACCOUNT total does
//! not shrink in the symbols the order does not touch, so the first flatten leg of a panic exit
//! would be refused by the ceiling it is trying to get back under (`docs/ops/kill-switches.md`: a
//! ceiling may never trap you in a position).
//!
//! `None` (the default) for this cap or the collar means the axis does not exist: no sum is
//! folded, no comparison runs, and the serialized [`RiskLimits`] is byte-identical, so
//! [`crate::engine_snapshot::state_hash`] and every recorded journal are unaffected.

use indexmap::IndexMap;
use std::collections::VecDeque;
use vike_model::{L2Book, OrderRequest};

#[cfg(doc)]
use types::PriceCollar;
use types::{RiskContext, RiskLimits, RiskVerdict};

mod throttle;
pub mod types;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TradingState {
    Active,
    /// only position-reducing orders allowed
    Reducing,
    /// no new orders (kill switch)
    Halted,
}

// The stateless primitives live in vike-model (no state, bus or I/O, so vike-backtest and
// vike-chart reach them without vike-exec) and are re-exported here. What stays in this module is
// the stateful half: `RiskLimits`/`TradingState` (serde-embedded in `EngineSnapshot`, feeding
// `state_hash`), the `order_times` throttle window and `check_inner`'s ordered ladder.
pub use vike_model::{
    ImpactDeny, TakeScope, clamp_leverage, fillable_veto, impact_veto, round_to,
    scoped_impact_veto, take_scope,
};

/// The part of an order that can still MOVE the position: its quantity minus whatever of it has
/// ALREADY executed and is therefore already counted inside [`RiskContext::position_size`].
///
/// On an IN-PLACE amend venue an amend's quantity is the order's new TOTAL, executed part
/// included, while the position already holds those lots, so `position + side × qty` counted them
/// twice and the gate refused exactly the quotes that were WORKING. `vike_model`'s
/// `AmendSemantics` is the per-venue authority on whether the netting applies (a cancel-replace
/// venue rests a FRESH order and keeps the whole qty); this function is only the arithmetic.
///
/// # Which lanes use it, and which deliberately do NOT
///
/// The lanes that project the POST-ORDER WORLD use it: `RiskGate::check_inner`'s halt-exemption
/// coverage, its reduce-only overshoot guard, the `over-max-exposure` cap and the buying-power
/// charge. The lanes that judge the ORDER AS IT GOES ON THE WIRE keep the whole quantity —
/// `below-min-qty`, `below-min-notional`, `over-max-notional` and the price collar: on an in-place
/// venue the wire really carries the total, so netting there would stop the per-order cap capping
/// what is sent, and the venue floors would judge a number the venue never sees.
///
/// # THE INVARIANT THIS ARITHMETIC RESTS ON
///
/// **`already_executed` must be a quantity that is ALREADY inside [`RiskContext::position_size`].**
/// Netting a lot that never moved the position SILENTLY REDUCES measured risk. The single caller,
/// `crates/vike-exec/src/execution_engine/mod.rs`'s `modify_order`, satisfies it by construction on
/// the venue lane: a fill arrives as the bare `Event::Fill` the `Account` folds AND as the
/// `OrderPartiallyFilled` wrap the FSM folds into `ManagedOrder::filled_qty`, both off the same
/// venue event. ⚠ **They are deduped by two SEPARATE id sets** (`seen_trade_ids` and
/// `seen_fsm_trade_ids`), so the coupling is a property of the lane, not of the data structure: a
/// wrap delivered without its bare fill would inflate `filled_qty` while the position stayed put.
/// `crates/vike-exec/tests/engine/partial_fill_amend_accounting.rs`'s
/// `the_netting_assumes_filled_qty_is_inside_the_position` pins the invariant and that residual.
///
/// # Guards
///
/// * **`0.0` returns `qty` UNTOUCHED**, not `(qty - 0.0).max(0.0)`: this runs BEFORE the gate's
///   `non-positive-size` check, so `qty` may be NaN or negative, and `f64::max` returns the
///   non-NaN operand — a `.max(0.0)` on the submit path would turn a NaN qty into `0.0`.
/// * **A garbage `already_executed` nets NOTHING**: only a FINITE, STRICTLY POSITIVE value takes
///   the netting arm; `-0.0`, negative, `NaN` and `±INFINITY` return `qty` verbatim. A NaN or
///   `+INFINITY` would otherwise collapse the remainder to `0.0` and vacate the exposure and margin
///   projections. `vike_model::AmendSemantics::already_in_position` screens the same values; the
///   guard is duplicated because this fn is `pub`, so its safety must not depend on the caller.
/// * **The clamp at `0.0`**: an amend BELOW the executed qty leaves nothing executable, and a
///   negative remainder would under-state the exposure projection while
///   `vike_model::initial_margin` charges its magnitude and `vike_model::is_covered_reduce` gets a
///   negative qty. The venues refuse such an amend today; that is THEIR guard, and
///   `an_amend_below_the_executed_qty_is_judged_as_a_zero_remainder` pins this one.
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

    /// Book-free gate — exactly [`RiskGate::check_with_book`] with no book, so the impact
    /// knobs are inert.
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
    /// consuming a rate-limit slot: the resting order consumed one at submit, and charging every
    /// amend would throttle an amend-heavy maker (`vike_mm`'s `SpreadMaker`) out of quoting. Every
    /// other lane still judges it, because an amend changes the SIZE of live exposure.
    ///
    /// **`already_executed` is how much of `request.qty` has ALREADY executed** (so is inside
    /// `ctx.position_size`); [`still_executable`] is the full statement. `0.0` on every path that
    /// is not an amend of a partially filled order. **The caller owns the venue fact**
    /// (`vike_model`'s `amend_semantics`). Book-free like [`RiskGate::check`].
    pub fn check_modify(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        already_executed: f64,
    ) -> RiskVerdict {
        self.check_inner(request, ctx, None, false, already_executed)
    }

    /// The one gate body. `consume_throttle` is the ONLY behavioral parameter: `true` is the
    /// single-order path, `false` runs every check EXCEPT the sliding-window throttle — a modify,
    /// and a combo's synthetic per-leg crossings (N legs are ONE venue order, so
    /// [`RiskGate::check_combo`] takes ONE slot after all legs pass). `already_executed` nets the
    /// amend double count out of the POSITION-PROJECTING lanes only ([`still_executable`]).
    fn check_inner(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        book: Option<&L2Book>,
        consume_throttle: bool,
        already_executed: f64,
    ) -> RiskVerdict {
        let lim = &self.limits;
        // The only quantity the position-projecting lanes below may use ([`still_executable`]).
        let executable = |qty: f64| still_executable(qty, already_executed);

        // side validation — must be exactly +1 or -1
        if request.side != 1 && request.side != -1 {
            return Self::deny("invalid-side");
        }

        // trading state (kill switch) — before anything else.
        //
        // ⚠ `Halted` STOPS OPENING RISK; IT MUST NEVER TRAP YOU IN A POSITION
        // (`docs/ops/kill-switches.md`'s law, which is also why cancels are gated by nothing). It
        // admits only `is_covered_reduce` — NOT `request.reduce_only` and NOT [`Self::reduces`],
        // deliberately the STRICTEST of the three: the flag is caller-asserted, and trusting it
        // would admit a FLAT-book `reduce_only` order and a REVERSAL that overshoots and flips
        // through flat (long 2, `reduce_only` SELL 5 ⇒ short 3), both opening risk under a halt.
        // `is_covered_reduce` requires DIRECTION and COVERAGE (`|position| >= |qty|`), so it can
        // only shrink `abs(position)`, and it admits a genuine exit whether or not the flag is set.
        //
        // The states are a strict ladder — `Halted` ⊂ `Reducing` ⊂ `Active` — pinned by
        // [`halted_admits_strictly_less_than_reducing_which_admits_less_than_active`]. `Reducing`
        // keeps the flag-trusting [`Self::reduces`] on purpose: it is the state you trade out of.
        // An admitted reduce faces the REST of this ladder exactly as under `Reducing`. The qty
        // judged is the STILL-EXECUTABLE one, so re-pricing a half-done exit still reads covered.
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

        // reduce-only overshoot (perp) — on the still-executable size, for the same reason as the
        // halt arm: the executed part of a half-done exit is already OUT of the position.
        if lim.block_reduce_only_overshoot
            && request.reduce_only
            && ctx.position_size.abs() < executable(request.qty).abs()
        {
            return Self::deny("reduce-only-overshoot");
        }

        // normalize: round price to tick, size to lot — on the ORDER SYMBOL's grid. `lim`'s
        // scalars are ONE symbol's, so on an engine with `extra_symbols` they are the WRONG grid
        // for another (`round_to(0.5, Some(1.0)) == 0.0`, then denied as `non-positive-size`).
        // `grid_for` returns the scalars verbatim when no override exists.
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

        // ONE reduce predicate gates EVERY bypass in this ladder — the floors, the collar, the
        // account ceiling, buying power and the impact veto all read `covered_reduce`. A bare
        // `reduce_only` FLAG is not trusted: on a flat book there is nothing for the venue to
        // reduce, so nothing server-side catches a mis-tagged opening order, and
        // `SimBroker::apply_fill` (the reference) derives closing-ness from the ACTUAL position.
        //
        // ⚠ IT IS THE STILL-EXECUTABLE SIZE THAT IS JUDGED, not the wire qty
        // ([`still_executable`]): an amend's executed lots already left the position. On every
        // non-amend path `exec_qty` IS `req.qty`.
        let exec_qty = executable(req.qty);
        let covered_reduce =
            vike_model::is_covered_reduce(req.reduce_only, req.side, ctx.position_size, exec_qty);

        // THE HALT RE-CHECK, ON THE NORMALIZED SIZE. Lot rounding is half-to-EVEN and can round a
        // qty UP: with a 1.0 lot, a 1.6 order against a 1.8 position is covered raw but becomes
        // 2.0 on the wire and FLIPS the position short 0.2. The reason string stays `"halted"`
        // so an operator is told the halt refused them.
        //
        // ⚠ THIS ONE IS THE AUTHORITY; the arm at the top of this fn is DELIBERATE REDUNDANCY (a
        // mutation test proved you cannot tell them apart from behaviour). The early arm gives the
        // common opening-order-under-halt the reason `"halted"` rather than whichever lane fires
        // first, and keeps the refusal ahead of every side-effecting lane (no throttle slot). If
        // you are simplifying, DELETE THE EARLY ARM, never this one — dropping this one reopens the
        // flip that `halted_refuses_a_reduce_whose_lot_rounding_would_flip_the_position` catches.
        if ctx.trading_state == TradingState::Halted && !covered_reduce {
            return Self::deny("halted");
        }

        // ---- fat-finger PRICE COLLAR (OPT-IN, RUST-NATIVE; see the module doc) ----
        // HERE, right after `covered_reduce` and BEFORE every price-derived axis: a mis-scaled
        // price also distorts notional/exposure/margin, so the operator would otherwise read
        // `over-max-notional` (or nothing) instead of the real cause. Before the throttle, like
        // every veto: a denied order must never consume a rate slot. Unarmed it is one lookup on
        // an empty map. The covered-reduce, combo and unpriced-mark skips are the module doc's.
        if let Some(collar) = lim.collar_for(&req.symbol) {
            let mark = ctx.mark_price;
            if !covered_reduce && req.combo_legs.is_empty() && mark.is_finite() && mark > 0.0 {
                let band = collar.band(mark);
                // The TRIGGER is tick-rounded for the comparison exactly as `req.price` was above;
                // `req.trigger_price` itself is NOT mutated.
                let trig = req.trigger_price.map(|p| round_to(p, lim.tick_size));
                // Symmetric: 10× above and 10× below the mark are the same fat finger. A
                // non-finite price is denied outright — `NaN > band` is false, so a NaN would
                // otherwise be admitted. A market order carries NEITHER price and is skipped.
                let outside = |p: f64| !p.is_finite() || (p - mark).abs() > band;
                if req.price.is_some_and(outside) || trig.is_some_and(outside) {
                    return Self::deny("price-collar");
                }
            }
        }

        // The below-min floors gate OPENING/increasing orders ONLY: a covered reduce/close is
        // exempt, the standing rule of `SimBroker::apply_fill` ("a closing fill must ALWAYS execute
        // so a position is never stranded below-min"). A dust flatten under the venue floor must
        // reach the venue, which may still reject it — the venue's call, not a local strand.
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
        // NOTIONAL IS A MAGNITUDE, every factor ABSOLUTE: a COMBO's `price` is the SIGNED net
        // (`ComboSpec::net_limit`), NEGATIVE for a credit structure, which would trip
        // `min_notional` on every credit combo while `notional > cap` could never trip. It
        // includes the CONTRACT MULTIPLIER, as the margin calc below and `SimBroker::apply_fill`
        // do, so `min_notional` gates identically live and backtest for options and inverse perps.
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

        // projected exposure cap — ONE SYMBOL at ONE VENUE, not the account: `ctx.position_size`
        // is the order symbol's own bucket (`ExecutionEngine::gate_position_size`), and
        // `RiskLimits::max_total_exposure`'s doc carries the full statement. Pinned by
        // `crates/vike-exec/tests/risk/risk_lane_pricing.rs`'s
        // `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`. Valued at `ctx.mark_price`
        // with the contract multiplier like its siblings, over `exec_qty` (the lane the amend
        // double count was widest in). Computed ONCE and shared with the account lane below, so
        // the two ceilings can never disagree about what this order does to its own symbol.
        let projected_symbol_notional = (ctx.position_size + req.side as f64 * exec_qty).abs()
            * ctx.mark_price
            * ctx.multiplier;
        if let Some(cap) = lim.max_total_exposure
            && projected_symbol_notional > cap
        {
            return Self::deny("over-max-exposure");
        }

        // ---- the ACCOUNT-AGGREGATE exposure cap (OPT-IN, RUST-NATIVE; see the module doc) ----
        // `ctx.account_exposure_excl_order` is every OTHER symbol's position plus every live
        // un-filled order but this one, folded once by the producer; nothing here walks a book.
        //
        // ⚠ THE REASON IS ITS OWN (`"over-account-exposure"`, not `"over-max-exposure"`): the two
        // ceilings are re-sized in different files, and a refusal naming the wrong one sends the
        // operator to widen a number that was not stopping them. ⚠ AFTER the per-symbol lane on
        // purpose: when both breach, the narrower ceiling is the more actionable answer. ⚠ A
        // COVERED REDUCE BYPASSES (the module doc argues it). Before the throttle and buying power,
        // like every veto. Unarmed, nothing below runs and no field of `ctx` is read.
        //
        // ⚠ THE REASON CARRIES THE TWO NUMBERS, unlike every bare-token reason above: the order
        // that trips an account ceiling is usually ordinary and the exposure sits in symbols the
        // operator is not looking at, so `"over-account-exposure"` alone could not tell a ceiling
        // too low from a book too big. The token stays FIRST (`starts_with` still classifies it);
        // fixed precision keeps it replay-stable.
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

        // pre-trade buying-power check (RUST-NATIVE; LEAN BuyingPowerModel semantics), before the
        // throttle so a denied order never consumes a rate slot. A COVERED reduce bypasses: it
        // frees margin rather than consuming it, so charging it could deny the very exit that
        // releases the margin. An UNCOVERED order (flat book, overshooting reversal) opens
        // exposure whatever its flag, and faces buying power like any opening order.
        if let Some(im_req) = lim.im_for(&req.symbol)
            && !covered_reduce
        {
            // `exec_qty`, not the wire qty ([`still_executable`]): `ctx.margin_used` already funds
            // the amended order's executed lots as POSITION.
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

        // pre-trade market-impact veto (OPT-IN; RUST-NATIVE), before the throttle like the margin
        // check. Unarmed it does NO book work (`impact_veto` returns on `budget?`). A COVERED
        // reduce never impact-vetoes (`SimBroker::apply_fill`: closing fills always execute): a
        // flatten in a liquidity vacuum is when the book looks worst and the exit must go through.
        // Same KNOWN RESIDUAL as the floors above: an uncovered REVERSAL is vetoable whole, and
        // the capped flatten (qty <= |position|) still bypasses, so no position is stranded.
        if let Some(b) = book
            && !covered_reduce
            && (lim.require_fillable || lim.max_slippage_bps.is_some())
        {
            // `exec_qty` for the same reason as the lanes above. No amend reaches here today
            // (`check_modify` passes no book); spelled correctly so wiring one in later cannot
            // reintroduce the double count in this lane.
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

    /// ATOMIC per-leg crossing for a COMBO order (combo-orders spec §5, conservative v1).
    ///
    /// A combo is ONE venue order carrying N legs (`OrderRequest::combo_legs`, `price` = the
    /// SIGNED net limit — NEGATIVE for a credit structure such as a short condor). The gate has
    /// no combo grid and no strategy-aware margin, so v1 prices the RISK off each leg's OWN mark:
    ///
    /// * every leg is crossed as a synthetic single-leg request — qty `|ratio| × combo_qty`, side
    ///   `sign(ratio) × combo_side`, `price`/`trigger_price` cleared so the leg's notional prices
    ///   off `leg_ctx(symbol).mark_price`;
    /// * **ALL legs must pass.** The FIRST failure denies the WHOLE combo with ONE verdict whose
    ///   reason names the failing leg (`"leg BTC-…-C: below-min-qty"`); no partial admission
    ///   exists, matching the atomic-reject FSM contract (spec §2);
    /// * the combo consumes **ONE** throttle slot, taken only after every leg passes;
    /// * the NET limit is deliberately NOT used for notional, and a defined-risk spread is
    ///   DOUBLE-COUNTED as naked legs ON PURPOSE: a combo must never pass a gate its naked legs
    ///   would fail.
    ///
    /// **The legs ACCUMULATE**, because `leg_ctx` is a per-symbol snapshot from BEFORE the combo
    /// and cannot know what earlier legs committed:
    ///
    /// * **buying power** — each admitted, non-reducing leg's initial margin (the SAME
    ///   [`vike_model::initial_margin`] the gate body uses, off its normalized qty) is added to the
    ///   next leg's `margin_used`;
    /// * **exposure / reduce-only** — each admitted leg's signed size is folded into a projected
    ///   per-symbol position, so a later leg on the SAME symbol sees the earlier one
    ///   (`ComboSpec::validate` does not reject a repeated symbol);
    /// * **account-aggregate exposure** — each admitted leg's gross notional is added to the next
    ///   leg's [`RiskContext::account_exposure_excl_order`]. ⚠ On a REPEATED symbol this
    ///   OVER-counts, the direction this entry point errs in by design.
    ///
    /// Account-level facts (`trading_state`, `now_ms`) come from `ctx` and OVERRIDE `leg_ctx`'s: a
    /// `Reducing` account must not be laundered into `Active` by a closure that fills only
    /// per-symbol fields. A leg whose mark is missing (`0.0`), negative or NaN is DENIED
    /// (`"leg X: no-mark"`): a zero mark vacates every price-based limit at once.
    ///
    /// TODO(spec §5 / steal-list C3): position-group (strategy-aware) margin is a SEPARATE later
    /// epic. Not implemented here on purpose: this gate errs strictly high.
    ///
    /// On success the request is returned VERBATIM: the gate never rounds a net price it has no
    /// grid for (the adapter formats it to the combo instrument's tick).
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
        // INFINITY is not caught by `<= 0.0`, and an infinite leg qty sails through every
        // UNARMED cap, so require finiteness explicitly.
        if !request.qty.is_finite() || request.qty <= 0.0 {
            return Self::deny("non-positive-size");
        }

        // Running commitment of the legs admitted SO FAR (see the doc above): margin, projected
        // signed position per symbol, and gross notional added to the ACCOUNT.
        let mut committed_margin = 0.0f64;
        let mut projected: IndexMap<&str, f64> = IndexMap::new();
        let mut committed_notional = 0.0f64;

        for leg in &request.combo_legs {
            // `ComboSpec::validate` rejects ratio 0 but a hand-built request can carry it; without
            // this it surfaces as a misleading `invalid-side` from the per-leg check.
            if leg.ratio == 0 {
                return Self::deny(&format!("leg {}: zero-ratio", leg.symbol));
            }

            let base = leg_ctx(&leg.symbol);
            // `leg_ctx` is TOTAL: a missing mark comes back as 0.0, which passes every
            // price-based limit at any size. Refuse instead.
            if !base.mark_price.is_finite() || base.mark_price <= 0.0 {
                return Self::deny(&format!("leg {}: no-mark", leg.symbol));
            }
            // `ctx` owns the trading state and the throttle clock (doc above); the per-symbol
            // fields carry the earlier legs' running commitment.
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
            // reduce_only is a PER-LEG fact, never inherited: a reduce_only combo whose legs open
            // new positions would launder each past buying power, the impact veto and `Reducing`.
            // Derived from the leg's projected book — exactly `RiskGate::reduces`' second clause.
            leg_req.reduce_only = pos_before != 0.0 && (f64::from(leg_req.side) * pos_before) < 0.0;

            // `0.0`: a synthetic leg is fresh, so nothing of it has executed.
            let v = self.check_inner(&leg_req, &lctx, None, false, 0.0);
            let Some(admitted) = v.request.filter(|_| v.ok) else {
                return Self::deny(&format!("leg {}: {}", leg.symbol, v.reason));
            };

            // Fold this leg into the running commitment off the gate's OWN normalized request and
            // the SAME margin fn `check_inner` used — one authority, not a second formula.
            // Reducing legs commit nothing (they are the margin bypass).
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
            // …and the ACCOUNT-exposure commitment: GROSS like the account fold it adds to; a
            // reducing leg adds nothing, mirroring the margin term above.
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
}

#[cfg(test)]
mod tests;
