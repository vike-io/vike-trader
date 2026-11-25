//! The pre-trade RiskGate — one extensible stage every order crosses.
//! PURE: owns no bus, publishes nothing; the engine acts on the
//! verdict, so risk rules run identically in backtest / paper / live.
//!
//! PARITY: Python `round()` is round-half-to-EVEN (banker's) → `f64::round_ties_even`.
//!
//! RUST-NATIVE deviations from the (retired) Python oracle, both aligning the gate with
//! `OrderBrokerSimulator::apply_fill` (the backtest reference) so live and backtest verdicts agree:
//! * the min-qty / min-notional floors gate OPENING orders only — a covered reduce/close is exempt
//!   (the anti-stranding rule);
//! * notional is read through the contract value (`ctx.contract_value`, decision 0170): a LINEAR
//!   contract is `qty × ref_price × unit` exactly as the gate's own margin calc and OrderBrokerSimulator's fill
//!   gate spell it (`unit` defaults to 1.0), an INVERSE one is `qty × unit` and the price does not enter.
//!
//! # Pre-trade impact (OPT-IN, RUST-NATIVE — no Python twin)
//!
//! [`RiskLimits::max_slippage_bps`] / [`RiskLimits::require_fillable`] add a walk-the-book
//! veto, priced by the contract's own taker price (`vike_model::ContractValue::taker_price` of
//! `ctx.contract_value`: harmonic on an inverse contract). **Where it runs is deliberate**: neither the
//! gate nor `Engine` owns a book (`RiskContext` is a `Copy` scalar struct, and the L2
//! lane `vike_execution::lanes::BookUpdate` is folded by the vike-core runtime), and plumbing one into
//! `apply_intent` would put a map lookup on the per-order path of the single-writer fold for a
//! knob that is off by default. So `impact_veto` / `fillable_veto` (the private child
//! `risk/impact.rs`) are PURE free functions, [`RiskGate::check_with_book`] is the book-aware
//! entry point, and [`RiskGate::check`] is exactly `check_with_book(.., None)`.
//!
//! **Caller contract:** the entry point is for a site holding BOTH the intent and a live book for
//! `(venue, symbol)`. **No such site exists today**: the runtime folds `BookUpdate` by value and
//! keeps no per-`(venue, symbol)` book map, so a strategy/mount with its own `on_order_book` view
//! is the cheaper first caller. Until one opts in the knobs are inert, and with both knobs off
//! `check_with_book` does no book work (not even a walk).
//!
//! **What the veto judges:** only the part of an order that can take DISPLAYED liquidity right
//! now (`take_scope`). Passive limits, stops and take-profits are never vetoed, and a covered
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
//! `Engine::modify_order` owns the lookup; the gate stays pure and venue-agnostic.
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
//! **The unit is the ENGINE — one `(venue, AccountLabel)`.** `vike_tradehub_core::make_engine_for_account`
//! builds one `Engine`, so one `RiskGate` and one copy of this cap, per account addressed
//! by `vike_execution::Engine::route_key`, narrowed by that account row's own `max_exposure`
//! when it states one (`vike-cli account set-exposure`). ⚠ **Where two labels share ONE
//! venue book the ceiling multiplies, and the tree detects that rather than refusing it**: the
//! `vike_config::venue_accounts` shared-BOOK rule is REPORTED and both engines mount
//! (`docs/decisions/0013-degrade-vs-refuse.md`). A DECLARED residual:
//! `vike_tradehub_core::make_engine_accounts`' shared-book `warn!` names this ceiling and its value when
//! armed (`vike_tradehub_core::shared_book_ceiling_note`). A cross-VENUE total is deliberately not
//! measured: that is a portfolio, and no engine holds it.
//!
//! **How the number is assembled** (`Engine::risk_context`, the producer):
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

use std::collections::VecDeque;
use vike_marketdata::L2Book;
use vike_math::summation::CompensatedSum;
#[cfg(doc)]
use vike_model::PriceCollar;
use vike_model::{OrderDenial, OrderRequest, OrderType, RiskLimits, TradingState, round_to};

use impact::{scoped_impact_veto, take_scope};
use types::{RiskContext, RiskVerdict};

mod impact;
mod throttle;
pub mod types;

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
/// `crates/vike-execution/src/execution_engine/mod.rs`'s `modify_order`, satisfies it by construction on
/// the venue lane: a fill arrives as the bare `Event::Fill` the `Account` folds AND as the
/// `OrderPartiallyFilled` wrap the FSM folds into `OrderRecord::filled_qty`, both off the same
/// venue event. ⚠ **They are deduped by two SEPARATE id sets** (`seen_trade_ids` and
/// `seen_fsm_trade_ids`), so the coupling is a property of the lane, not of the data structure: a
/// wrap delivered without its bare fill would inflate `filled_qty` while the position stayed put.
/// `crates/vike-execution/tests/engine/partial_fill_amend_accounting.rs`'s
/// `the_netting_assumes_filled_quantity_is_inside_the_position` pins the invariant and that residual.
///
/// # Guards
///
/// * **`0.0` returns `qty` UNTOUCHED**, not `(qty - 0.0).max(0.0)`: this runs BEFORE the gate's
///   `non-positive-size` check, so `qty` may be NaN or negative, and `f64::max` returns the
///   non-NaN operand — a `.max(0.0)` on the submit path would turn a NaN qty into `0.0`.
/// * **A garbage `already_executed` nets NOTHING**: only a FINITE, STRICTLY POSITIVE value takes
///   the netting arm; `-0.0`, negative, `NaN` and `±INFINITY` return `qty` verbatim. A NaN or
///   `+INFINITY` would otherwise collapse the remainder to `0.0` and vacate the exposure and margin
///   projections. `vike_venues::AmendSemantics::already_in_position` screens the same values; the
///   guard is duplicated because this fn is `pub`, so its safety must not depend on the caller.
/// * **The clamp at `0.0`**: an amend BELOW the executed qty leaves nothing executable, and a
///   negative remainder would under-state the exposure projection while
///   `vike_model::initial_margin` charges its magnitude and `vike_model::is_covered_reduce` gets a
///   negative qty. The venues refuse such an amend today; that is THEIR guard, and
///   `an_amend_below_the_executed_quantity_is_judged_as_a_zero_remainder` pins this one.
#[must_use]
#[inline]
pub fn still_executable(qty: f64, already_executed: f64) -> f64 {
    if already_executed.is_finite() && already_executed > 0.0 {
        (qty - already_executed).max(0.0)
    } else {
        qty
    }
}

/// How much of a live, un-filled order the margin and exposure lanes count, the ONE rule live's
/// `Engine::counted_remaining` and the backtest's `OrderBrokerSimulator` folds both ask: `remaining`,
/// gross, unless the order commits nothing, for a non-finite or non-positive remainder, or for a
/// covered reduce against `position` (it frees margin rather than consuming it, and charging it
/// would deny flattens during a drawdown).
///
/// [`vike_model::is_covered_reduce`] takes no `reduce_only` flag: a flag arm would be implied by the
/// first, so a `reduce_only` order is judged the same either way.
#[must_use]
#[inline]
pub fn committed_quantity(remaining: f64, side: i32, position: f64) -> Option<f64> {
    // Spelled so NaN is visible.
    if !remaining.is_finite() || remaining <= 0.0 {
        return None;
    }
    (!vike_model::is_covered_reduce(side, position, remaining)).then_some(remaining)
}

/// **A spot (CASH) account cannot sell more base than it holds**: the verdict on a SELL of `qty`, the ONE
/// rule live's `Engine::cash_oversell_denial` and the backtest's `OrderBrokerSimulator` both ask. What is free
/// is the long `position` holds (none when flat or short) less `out`, the remaining quantity of every
/// live sell already out on the symbol but those of this order's own linked group. `group` is that group
/// when the order is a leg of one ([`crate::execution_engine::LinkedExits`]): each set's live legs'
/// remaining quantity, and the set this order belongs to; the group claims its largest set (this order
/// added to its own), never the sum of every leg. Refused, in words, when the claim passes what is free.
#[must_use]
pub fn cash_oversell_refusal(
    position: f64,
    out: f64,
    group: Option<(&[f64], usize)>,
    qty: f64,
) -> Option<String> {
    let free = (position.max(0.0) - out).max(0.0);
    let claim = group.map_or(qty, |(sets, own)| {
        sets.iter()
            .enumerate()
            .map(|(i, live)| live + if i == own { qty } else { 0.0 })
            .fold(0.0, f64::max)
    });
    (claim > free + 1e-12).then(|| {
        format!("{}: it has {free}, this sells {qty}; nothing was sent", OrderDenial::CashOversell)
    })
}

/// Where an order's TRIGGER goes on its symbol's `tick`: the model's side-directed snap (decision 0269
/// row 27, the owner's G1), so one stop rests at one price on every venue whatever that venue would do
/// with an off-grid one. A STOP's goes away from the market ([`vike_model::snap_stop`]: a buy up, a sell
/// down), so it never fires before its level; a STOP-LIMIT's and a TAKE-PROFIT's go as a limit's price
/// ([`vike_model::snap_limit`]: a buy down, a sell up). It moves a trigger by less than one tick.
///
/// Left as stated: any other kind's trigger (a trailing order's is its ACTIVATION, whose side is the
/// venue's), no tick, and a non-finite trigger, which the snap would turn into a NaN that every cap
/// below lets through where an infinity is caught.
///
/// The gate's rule, and also the rule of the two places that hold a stop and FIRE it themselves, the
/// core's held arms and the backtest simulator ([`working_trigger_on_grid`]), so a stop fires where the
/// venue would have rested it.
pub fn trigger_on_grid(order_type: &OrderType, side: i32, trigger: f64, tick: Option<f64>) -> f64 {
    match order_type {
        _ if !trigger.is_finite() => trigger,
        OrderType::Stop => vike_model::snap_stop(side, trigger, tick),
        OrderType::StopLimit | OrderType::TakeProfit => vike_model::snap_limit(side, trigger, tick),
        _ => trigger,
    }
}

/// [`trigger_on_grid`] for an order that rests where it fires (a core-held arm, a backtest's resting
/// order), whose `price` is its trigger: a stop's, a stop-limit's and a take-profit's go on the grid,
/// any other kind is left as it is.
pub fn working_trigger_on_grid(order: &mut vike_model::OrderResting, tick: Option<f64>) {
    use vike_model::OrderKind;
    let order_type = match order.kind {
        OrderKind::Stop => OrderType::Stop,
        OrderKind::StopLimit => OrderType::StopLimit,
        OrderKind::TakeProfit => OrderType::TakeProfit,
        _ => return,
    };
    order.price = order.price.map(|p| trigger_on_grid(&order_type, order.side, p, tick));
}

/// Apply [`RiskLimits::max_sizing_equity`] to an already-resolved equity, the ONE place the ceiling
/// is applied: `Engine::cap_sizing_equity` and the backtest's `OrderBrokerSimulator::gate_order` both
/// call it. `None` returns the figure verbatim (bit-identical, not `min`'d against an infinity).
///
/// ⚠ **A comparison, deliberately NOT `f64::min`.** `min` returns the OTHER operand for a NaN, so a
/// poisoned equity (`Account::equity_all` goes NaN off one NaN mark slot) would come out as the
/// operator's ceiling — a plausible number the gate ADMITS against, where the NaN would have failed
/// every comparison and denied. A cap may only LOWER a real figure; `NaN > cap` is false, so the NaN
/// passes through to every downstream guard.
#[must_use]
#[inline]
pub fn capped_equity(ceiling: Option<f64>, equity: f64) -> f64 {
    match ceiling {
        Some(cap) if equity > cap => cap,
        _ => equity,
    }
}

/// The ONE reason word a mount's leverage cap ([`crate::MountBudget::max_leverage`]) refuses an
/// opening order by: every refusal [`mount_leverage_refusal`] words starts with it, so the live
/// drain (`vike_core`'s `drain_broker`) and the backtest (`OrderBrokerSimulator::gate_order`) say
/// one thing, and a backtest denial IS the live denial. The word is the order catalog's own literal for its
/// id (`vike_model::OrderDenialId::OverMountLeverage`, `order.risk.over_mount_leverage`).
pub const MOUNT_LEVERAGE_REFUSAL: &str = vike_model::OrderDenialId::OverMountLeverage.template();

/// One row of a mount's book as [`judge_mount_leverage`] reads it: the mount's attributed position on one
/// instrument (a [`crate::MountAttribution`] row's `size` and `average_price`), keyed as its engine keys the
/// row, that instrument's MARK now (`None` when nothing prices it) and its contract value.
#[derive(Debug, Clone)]
pub struct MountLeverageRow<K> {
    pub key: K,
    pub size: f64,
    pub average_price: f64,
    pub price: Option<f64>,
    pub value: vike_model::ContractValue,
}

/// One leg of an order [`judge_mount_leverage`] judges: a single order is one leg, an amend what
/// [`mount_leverage_amend`] judges it as (its growth, all it rests at a newly named price, a held child's whole
/// new size after the ancestors that arm it), an
/// order list one per leg, a combo one per leg row. `side` · `quantity`, its OWN price (its limit or trigger,
/// a discretionary limit's worst; `None` for a market order), the earlier leg whose fill arms it (an OTO
/// `parent`), and the legs it is an ALTERNATIVE to (`vike_model::OrderListLeg::linked`, OCO or OUO: of a
/// linked set one leg at most counts).
#[derive(Debug, Clone, Default)]
pub struct MountLeverageLeg {
    pub side: i32,
    pub quantity: f64,
    pub price: Option<f64>,
    pub parent: Option<usize>,
    pub linked: Vec<usize>,
}

/// **The ONE judge of a mount's leverage cap** ([`crate::MountBudget::max_leverage`]), a SOFT pre-trade
/// budget judged per verb, the live drain's (`vike_core`'s `leverage_refusal_on`) and the backtest's
/// (`OrderBrokerSimulator::leverage_refusal`): why ONE verb is refused, or `None`. The verb is `order`, the
/// rows it trades (`(key, contract value, legs there)` each): one row for an order, a list, a bracket or an
/// amend, one row per leg for a COMBO, which fills all its legs or none. Each verb is judged ALONE against
/// the mount's OWN attributed `rows`, never the account's positions and never beside the other verbs of its
/// callback, so a verdict does not depend on the order a callback's verbs are judged in. Verbs of one
/// callback are NOT summed, and orders resting from earlier callbacks are NOT counted: the engine's
/// `RiskGate` is the account's hard limit.
///
/// 1. The verb's OUTCOMES on each row it trades, in QUANTITY: a single row's two reach extremes (an OTO
///    child counting only where its parent filled, at any depth, and of linked legs, OCO or OUO, the worst
///    single one, never their sum), a combo's none or all.
/// 2. A COVERED REDUCE (every outcome moves every row it trades only toward flat) passes unjudged,
///    reduce-only or not; any other verb is judged, a reduce-only order too (its fill is bounded by the
///    ACCOUNT's position, which a sibling's holding can make larger than the mount's).
/// 3. Each outcome's gross notional: every other row at its mark (a row nothing prices at its own
///    `average_price`: a settled or expired leg no mount closed must not block the mount for ever), plus
///    each traded row NETTED: its size after the outcome (long 100, sell 200 is 100 short, not 300), valued
///    at the larger notional (through the row's `ContractValue`) of the mark and every order price that moves
///    it there, so a sell through the market counts at the market and a buy limit above the market at its
///    limit, and an exit never offsets exposure as profit. A buy above the mark revalues the WHOLE row after
///    it, what the mount already held included, at that price. A leg moving a row with no price of its own
///    and no mark is of UNKNOWN value and REFUSED: fail closed. Summed by the house compensated sum; each
///    outcome then meets [`mount_leverage_refusal`], the comparison, against `sizing_equity`.
///
/// Refuse-only: never a clamp, never a latch, never a venue write.
#[must_use]
pub fn judge_mount_leverage<K: PartialEq>(
    cap: Option<f64>,
    sizing_equity: f64,
    rows: &[MountLeverageRow<K>],
    order: &[(&K, vike_model::ContractValue, &[MountLeverageLeg])],
) -> Option<String> {
    let cap = cap.filter(|c| c.is_finite() && *c >= 1.0)?;
    let unknown = || {
        Some(format!(
            "{MOUNT_LEVERAGE_REFUSAL}: this order's value cannot be known (no price of its own and no \
             mark on its instrument), so the mount's leverage cannot be judged; an opening order is \
             refused, a reduce is not"
        ))
    };
    if order.iter().flat_map(|o| o.2).any(|l| !l.quantity.is_finite()) {
        return unknown();
    }
    let combo = order.len() > 1;
    // per traded row: its row, its legs' signed quantities, and its shift in each of the two outcomes
    let traded: Vec<_> = order
        .iter()
        .map(|&(key, value, legs)| {
            let own = rows.iter().find(|r| r.key == *key);
            let quantities: Vec<f64> =
                legs.iter().map(|l| f64::from(l.side) * l.quantity).collect();
            let (down, up) = reach(legs, &quantities);
            let shifts = if combo { [0.0, down + up] } else { [down, up] };
            (own, value, legs, quantities, shifts)
        })
        .collect();
    let held = |own: Option<&MountLeverageRow<K>>| own.map_or(0.0, |r| r.size);
    let covered = traded.iter().all(|(own, _, _, _, shifts)| {
        let held = held(*own);
        shifts.iter().all(|s| held + s >= held.min(0.0) && held + s <= held.max(0.0))
    });
    if covered {
        return None;
    }
    let notional_of = |r: &MountLeverageRow<K>| {
        r.value.gross_notional(r.size, mark(r.price).unwrap_or(r.average_price))
    };
    let mut others = CompensatedSum::default();
    for r in rows.iter().filter(|r| r.size != 0.0 && !order.iter().any(|o| r.key == *o.0)) {
        others.add(notional_of(r));
    }
    for outcome in 0..2 {
        let mut gross = CompensatedSum::default();
        gross.add(others.total());
        for (own, value, legs, quantities, shifts) in &traded {
            let (shift, after) = (shifts[outcome], held(*own) + shifts[outcome]);
            if after == 0.0 {
                continue;
            }
            if shift == 0.0 {
                gross.add(own.map_or(0.0, notional_of));
                continue;
            }
            let own_mark = own.and_then(|r| mark(r.price));
            let mut notional = own_mark.map_or(0.0, |p| value.gross_notional(after, p));
            for (leg, _) in legs.iter().zip(quantities).filter(|(_, q)| **q * shift > 0.0) {
                let Some(price) = mark(leg.price).or(own_mark) else { return unknown() };
                notional = notional.max(value.gross_notional(after, price));
            }
            gross.add(notional);
        }
        let why = mount_leverage_refusal(Some(cap), gross.total(), sizing_equity);
        if why.is_some() {
            return why;
        }
    }
    None
}

/// The price the mount leverage cap values an order's leg at, both engines' one rule: its LIMIT (a
/// discretionary limit's worst it may fill at, `vike_model::OrderResting::discretion_limit`, the one place the
/// discretion is applied), else its trigger; `None` for a market order.
#[must_use]
pub fn mount_leverage_price(
    side: i32,
    (limit, trigger): (Option<f64>, Option<f64>),
    discretion: Option<f64>,
) -> Option<f64> {
    let resting = vike_model::OrderResting {
        discretionary_amount: discretion,
        ..vike_model::OrderResting::new(vike_model::OrderKind::Limit, side, 0.0)
    };
    limit.map(|p| resting.discretion_limit(p)).or(trigger)
}

/// The order an AMEND names, as [`mount_leverage_amend`] reads it: its `side`, what is `left` to fill (all of
/// a held OTO child), its kind as an amend's prices land on it (`vike_model::modified_prices`; `None` for a
/// kind no price lands on), its `limit` and `trigger` and a discretionary limit's `discretion` before the
/// amend, and whether it is `reduce_only`.
#[derive(Debug, Clone)]
pub struct MountLeverageAmended {
    pub side: i32,
    pub left: f64,
    pub kind: Option<OrderType>,
    pub limit: Option<f64>,
    pub trigger: Option<f64>,
    pub discretion: Option<f64>,
    pub reduce_only: bool,
}

/// What the mount leverage cap judges an AMEND as, both engines' one rule (`vike_core`'s `amend_leverage_hold`,
/// the simulator's `OrderBrokerSimulator::amend_leverage_legs`), for [`judge_mount_leverage`], or `None` when
/// it passes unjudged. `order` is the order amended to `new_qty` at `new_price` / `new_trigger`; `held_row` is
/// what the mount holds on its row; `ancestors` are, for a HELD OTO child (never at the venue, all of it
/// unfilled), the unfilled ancestors that arm it, root first, each armed by the one before, and empty for
/// every other order.
///
/// - An amend that NAMES A PRICE is judged at its new price on all the order will rest (the new size, else
///   what is left), since a repriced order, a smaller one too, can become marketable; but a REDUCE-ONLY order
///   that the mount's row, or the position its own list puts on (`held_row` and its ancestors), covers adds no
///   exposure and passes.
/// - An amend that names a size alone: a decrease passes; a grow is judged on its growth over what is left,
///   and a held child's on its WHOLE new size after its ancestors, exactly as its list was judged (its existing
///   size may offset what its parent puts on, so its growth alone would under-count).
/// - The price is where the amend's prices land on the order's kind: a stop's one price is its trigger, a
///   stop-limit keeps its limit as its price and a new trigger moves only its trigger
///   ([`mount_leverage_price`]: the limit, else the trigger).
#[must_use]
pub fn mount_leverage_amend(
    order: &MountLeverageAmended,
    (new_qty, new_price, new_trigger): (Option<f64>, Option<f64>, Option<f64>),
    held_row: f64,
    ancestors: Vec<MountLeverageLeg>,
) -> Option<Vec<MountLeverageLeg>> {
    let priced = new_price.is_some() || new_trigger.is_some();
    let quantity = if priced {
        new_qty.unwrap_or(order.left)
    } else {
        let grown = new_qty.filter(|q| *q > order.left)?;
        if ancestors.is_empty() { grown - order.left } else { grown }
    };
    if quantity.is_nan() || quantity <= 0.0 {
        return None;
    }
    if priced && order.reduce_only {
        let mut after = CompensatedSum::default();
        after.add(held_row);
        for a in &ancestors {
            after.add(f64::from(a.side) * a.quantity);
        }
        let (after, exit) = (after.total(), f64::from(order.side) * quantity);
        if exit * after < 0.0 && exit.abs() <= after.abs() {
            return None;
        }
    }
    let (limit, trigger) = match &order.kind {
        Some(kind) => vike_model::modified_prices(kind, new_price, new_trigger),
        None => (None, None),
    };
    let terms = (limit.or(order.limit), trigger.or(order.trigger));
    let price = mount_leverage_price(order.side, terms, order.discretion);
    let parent = ancestors.len().checked_sub(1);
    let mut legs = ancestors;
    legs.push(MountLeverageLeg { side: order.side, quantity, price, parent, ..Default::default() });
    Some(legs)
}

/// A price that is a mark: finite and above zero.
fn mark(price: Option<f64>) -> Option<f64> {
    price.filter(|p| p.is_finite() && *p > 0.0)
}

/// How far `legs`' fills can move their row: `(down, up)`, the least and the most they add of `signed`
/// (each leg's signed quantity). A leg counts only where its parent filled (a leg whose
/// parent is no earlier leg counts alone), and of a linked set under one parent (OCO, OUO) one leg at most,
/// the worst: siblings that exclude each other are alternatives, never a sum. One walk, children first.
fn reach(legs: &[MountLeverageLeg], signed: &[f64]) -> (f64, f64) {
    let n = legs.len();
    let root = |set: &[usize], mut i: usize| {
        while set[i] != i {
            i = set[i];
        }
        i
    };
    let mut set: Vec<usize> = (0..n).collect();
    for (i, leg) in legs.iter().enumerate() {
        for &j in leg.linked.iter().filter(|&&j| j < n) {
            let (a, b) = (root(&set, i), root(&set, j));
            set[a.max(b)] = a.min(b);
        }
    }
    // per (parent, linked set): the least and the most one member of it adds
    let mut options: Vec<(Option<usize>, usize, f64, f64)> = Vec::with_capacity(n);
    let under = |options: &[(Option<usize>, usize, f64, f64)], parent: Option<usize>, own: f64| {
        let (mut down, mut up) = (CompensatedSum::default(), CompensatedSum::default());
        down.add(own);
        up.add(own);
        for &(_, _, d, u) in options.iter().filter(|o| o.0 == parent) {
            down.add(d.min(0.0));
            up.add(u.max(0.0));
        }
        (down.total(), up.total())
    };
    for i in (0..n).rev() {
        let (d, u) = under(&options, Some(i), signed[i]);
        let parent = legs[i].parent.filter(|&p| p < i);
        let linked = root(&set, i);
        match options.iter_mut().find(|o| o.0 == parent && o.1 == linked) {
            Some(o) => (o.2, o.3) = (o.2.min(d), o.3.max(u)),
            None => options.push((parent, linked, d, u)),
        }
    }
    under(&options, None, 0.0)
}

/// **The comparison of a mount's leverage cap** ([`crate::MountBudget::max_leverage`]): why an
/// OPENING order is refused, or `None`. [`judge_mount_leverage`] is its caller, which decides what
/// is judged and nets `gross_notional_after`, the mount's gross notional after the order;
/// `sizing_equity` is its account engine's ACTING equity (decision 0048).
///
/// `cap` `None`, or not a finite figure `>= 1.0`, is OFF. Otherwise the order passes while
/// `gross_notional_after <= cap × sizing_equity`, written as that comparison so a NaN figure
/// refuses (fail closed), as does a positive notional on an equity at or below zero.
/// Refuse-only: never a clamp, never a latch, never a venue write.
#[must_use]
pub fn mount_leverage_refusal(
    cap: Option<f64>,
    gross_notional_after: f64,
    sizing_equity: f64,
) -> Option<String> {
    let cap = cap.filter(|c| c.is_finite() && *c >= 1.0)?;
    if gross_notional_after <= cap * sizing_equity {
        return None;
    }
    Some(format!(
        "{MOUNT_LEVERAGE_REFUSAL}: this order takes the mount's gross notional to \
         {gross_notional_after}, past {cap} times its account's sizing equity of {sizing_equity}; \
         an opening order is refused, a reduce is not"
    ))
}

/// Pre-trade gate; one per venue/account session. The throttle window is shared across ALL
/// symbols routed through it (a session-level order-rate limit).
pub struct RiskGate {
    pub limits: RiskLimits,
    /// **The mark floor** (default `false`): an OPENING order on the engine's MOUNTED symbol is denied
    /// `no-mark-for-mounted-symbol` while the engine holds no mark for it, meaning NONE AT ALL: absent,
    /// zero, negative or non-finite (the producer says so with a NaN [`RiskContext::mark_price`]).
    /// Without it, an engine nobody prices prices a market order at 0.0 and `over-max-notional`,
    /// `over-max-exposure`, `over-account-exposure` and `insufficient-margin` all see notional zero and
    /// pass: the engine's own comment, "a 0.0 mark VACATES the exposure cap at any size". A covered
    /// reduce is never held up by it.
    ///
    /// ⚠ **"Never priced since boot", NOT "priced now".** The floor tests presence, not age: every
    /// shipped path leaves the engine's `price_config` at `PriceBoardConfig::default()`, which sets no freshness
    /// window, so a mark the engine ever held stands for ever and one from a dead feed passes the
    /// floor. Open limit; a window is a behaviour change of its own.
    ///
    /// ⚠ NOT part of [`RiskLimits`] and not serialized, on purpose: `EngineSnapshot` carries the limits,
    /// so a flag in them would reach `Engine::from_snapshot`'s replay engine, which has no
    /// market data at all and would deny every opening order the live run admitted. The live assembly
    /// (`vike_tradehub_core`'s `assemble_engine`) sets it; a snapshot-built engine never has it.
    pub require_mark: bool,
    order_times: VecDeque<i64>,
    /// Whether the last verdict read data a journal does not hold ([`Self::last_verdict_read_market_data`]).
    read_market_data: bool,
}

impl RiskGate {
    pub fn new(limits: RiskLimits) -> Self {
        RiskGate {
            limits,
            require_mark: false,
            order_times: VecDeque::new(),
            read_market_data: false,
        }
    }

    /// **Whether the last verdict read data the command journal does not hold** (package 2's C.1b): a mark, a price
    /// or a book, or a figure priced from one (equity, committed margin, exposure). A replay of the journal has the
    /// marks of its base `Snapshot` only, so such a verdict may come out otherwise there; the boot restore then
    /// records the order's reports as unverified instead of regenerating them (`vike_core`'s
    /// `CoreThread::journal_execution_report`). Set only where a lane that ran reads such data; the throttle, the
    /// trading state, the position, the grid and the quote rate are journaled and set nothing. Nothing on the live
    /// path reads it.
    pub fn last_verdict_read_market_data(&self) -> bool {
        self.read_market_data
    }

    /// True if the order shrinks abs(position): explicit reduce_only, or opposite a non-zero pos.
    fn reduces(request: &OrderRequest, ctx: &RiskContext) -> bool {
        request.reduce_only || vike_model::is_reducing_direction(request.side, ctx.position_size)
    }

    fn deny(denial: OrderDenial) -> RiskVerdict {
        Self::deny_as(denial, denial.to_string())
    }

    /// A denial whose reason carries more than its word (figures, the leg it names); `reason` starts with
    /// or names `denial`'s word.
    fn deny_as(denial: OrderDenial, reason: String) -> RiskVerdict {
        RiskVerdict::denied(denial, reason)
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
    /// amend would throttle an amend-heavy maker (`vike_market_making`'s `SpreadMaker`) out of quoting. Every
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
        let mut market = false;
        let verdict =
            self.judge(request, ctx, book, (consume_throttle, already_executed), &mut market);
        self.read_market_data = market;
        verdict
    }

    /// [`Self::check_inner`]'s lanes; `market` is set by each lane that runs and reads data a journal does not hold
    /// ([`Self::last_verdict_read_market_data`]).
    fn judge(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        book: Option<&L2Book>,
        (consume_throttle, already_executed): (bool, f64),
        market: &mut bool,
    ) -> RiskVerdict {
        let limit = &self.limits;
        // The only quantity the position-projecting lanes below may use ([`still_executable`]).
        let executable = |qty: f64| still_executable(qty, already_executed);

        // side validation — must be exactly +1 or -1
        if request.side != 1 && request.side != -1 {
            return Self::deny(OrderDenial::InvalidSide);
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
                request.side,
                ctx.position_size,
                executable(request.qty),
            )
        {
            return Self::deny(OrderDenial::Halted);
        }
        if ctx.trading_state == TradingState::Reducing && !Self::reduces(request, ctx) {
            return Self::deny(OrderDenial::ReduceOnly);
        }

        // reduce-only overshoot (perp) — on the still-executable size, for the same reason as the
        // halt arm: the executed part of a half-done exit is already OUT of the position.
        if limit.block_reduce_only_overshoot
            && request.reduce_only
            && ctx.position_size.abs() < executable(request.qty).abs()
        {
            return Self::deny(OrderDenial::ReduceOnlyOvershoot);
        }

        // normalize: round price to tick, size to lot — on the ORDER SYMBOL's grid. `limit`'s
        // scalars are ONE symbol's, so on an engine with `extra_symbols` they are the WRONG grid
        // for another (`round_to(0.5, Some(1.0)) == 0.0`, then denied as `non-positive-size`).
        // `grid_for` returns the scalars verbatim when no override exists. A TRIGGER goes on the
        // same grid by the order's side ([`trigger_on_grid`]), and so does a stop's one price, which
        // IS its trigger (an amend projects it into `price`: `vike_model::modified_price_is_trigger`).
        let grid = limit.grid_for(&request.symbol);
        let on_grid =
            |p: f64| trigger_on_grid(&request.order_type, request.side, p, grid.tick_size);
        let price = request.price.map(|p| {
            if vike_model::modified_price_is_trigger(&request.order_type) {
                on_grid(p)
            } else {
                round_to(p, grid.tick_size)
            }
        });
        let qty = round_to(request.qty, grid.lot_size);
        let mut req = request.clone();
        req.price = price;
        req.qty = qty;
        req.trigger_price = request.trigger_price.map(on_grid);

        // validity
        if req.qty <= 0.0 {
            return Self::deny(OrderDenial::NonPositiveSize);
        }

        // ONE reduce predicate gates EVERY bypass in this ladder — the floors, the collar, the
        // account ceiling, buying power and the impact veto all read `covered_reduce`. A bare
        // `reduce_only` FLAG is not trusted: on a flat book there is nothing for the venue to
        // reduce, so nothing server-side catches a mis-tagged opening order, and
        // `OrderBrokerSimulator::apply_fill` (the reference) derives closing-ness from the ACTUAL position.
        //
        // ⚠ IT IS THE STILL-EXECUTABLE SIZE THAT IS JUDGED, not the wire qty
        // ([`still_executable`]): an amend's executed lots already left the position. On every
        // non-amend path `execution_quantity` IS `req.qty`.
        let execution_quantity = executable(req.qty);
        let covered_reduce =
            vike_model::is_covered_reduce(req.side, ctx.position_size, execution_quantity);

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
            return Self::deny(OrderDenial::Halted);
        }

        // ---- the CONVERSION RATE of a foreign-quoted symbol has gone stale (RUST-NATIVE) ----
        // Every number below for such a symbol — notional, exposure, margin — is the venue's unit
        // times a rate that has not been renewed for longer than the staleness window, i.e. a number
        // the market has left. An OPENING order is denied (fail closed, never par); a COVERED reduce
        // is never held up ("a stale rate must never trap a position"), the same predicate that
        // exempts the floors, the collar and the ceilings. Before the throttle, like every veto: a
        // denied order consumes no rate slot. A symbol quoted in the account's own currency is never
        // stale (`ctx.quote_stale` is `false`), so this is one bool test per order.
        if ctx.quote_stale && !covered_reduce {
            return Self::deny(OrderDenial::QuoteRateStale);
        }

        // ---- the MARK FLOOR (OPT-IN per gate, `Self::require_mark`; RUST-NATIVE) ----
        // The engine holds NO mark at all for its OWN symbol (never priced since boot: not a freshness
        // test, see `Self::require_mark`) and says so with a NaN `mark_price`
        // (`Engine::risk_context`, the only producer that answers it, and only for the mounted
        // symbol of an engine whose gate is armed): a market order would be judged at notional zero
        // and a limit order against itself, so every price-based cap below passes at any size. An
        // OPENING order is denied (fail closed) by a name that says why; a COVERED reduce is never
        // held up, the same predicate that exempts the collar and the ceilings. Before the collar and
        // the throttle, like every veto: the real cause, and no rate slot consumed. Unarmed it is
        // one bool test per order.
        if self.require_mark && !covered_reduce {
            *market = true; // whether the engine holds a mark
            if ctx.mark_price.is_nan() {
                return Self::deny(OrderDenial::NoMarkForMountedSymbol);
            }
        }

        // ⚠ A MARKET order fills at the market whatever price it names, so that number prices nothing:
        // 10 BTC "at 1" is judged exactly as 10 BTC with no price, at the mark, or refused below for
        // want of one. A TRIGGER on one still sizes it: `fixtures/r5/risk.json` pins that (case 133).
        // A DISCRETIONARY limit (decision 0267) is judged at its worst price, L + d for a buy and L − d
        // for a sell, by the collar, the notional and the margin alike: one `Option` test per order.
        let limit_price = if matches!(req.order_type, OrderType::Market) {
            None
        } else {
            match req.attributes.and_then(|a| a.discretionary_amount) {
                Some(d) => req.price.map(|p| if req.side > 0 { p + d } else { p - d }),
                None => req.price,
            }
        };

        // ---- fat-finger PRICE COLLAR (OPT-IN, RUST-NATIVE; see the module doc) ----
        // HERE, right after `covered_reduce` and BEFORE every price-derived axis: a mis-scaled
        // price also distorts notional/exposure/margin, so the operator would otherwise read
        // `over-max-notional` (or nothing) instead of the real cause. Before the throttle, like
        // every veto: a denied order must never consume a rate slot. Unarmed it is one lookup on
        // an empty map. The covered-reduce, combo and unpriced-mark skips are the module doc's.
        if let Some(collar) = limit.collar_for(&req.symbol) {
            let mark = ctx.mark_price;
            if !covered_reduce && req.combo_legs.is_empty() {
                *market = true; // the band is the mark's, and a missing mark skips it
            }
            if !covered_reduce && req.combo_legs.is_empty() && mark.is_finite() && mark > 0.0 {
                let band = collar.band(mark);
                // The TRIGGER as it goes out: a stop kind's is on the grid already (above), any
                // other kind's is tick-rounded for the comparison only, as `req.price` was.
                let trig = req.trigger_price.map(|p| round_to(p, grid.tick_size));
                // Symmetric: 10× above and 10× below the mark are the same fat finger. A
                // non-finite price is denied outright — `NaN > band` is false, so a NaN would
                // otherwise be admitted. A market order has no price of its own and is skipped:
                // it is judged at the mark, whatever number it names, and no band refuses the mark.
                let outside = |p: f64| !p.is_finite() || (p - mark).abs() > band;
                if limit_price.is_some_and(outside) || trig.is_some_and(outside) {
                    return Self::deny(OrderDenial::PriceCollar);
                }
            }
        }

        // The below-min floors gate OPENING/increasing orders ONLY: a covered reduce/close is
        // exempt, the standing rule of `OrderBrokerSimulator::apply_fill` ("a closing fill must ALWAYS execute
        // so a position is never stranded below-min"). A dust flatten under the venue floor must
        // reach the venue, which may still reject it — the venue's call, not a local strand.
        //
        // A below-min REVERSAL (|qty| > |position|, flipping through flat) is NOT a covered reduce
        // — it opens the far side — so the gate denies it WHOLE, while `OrderBrokerSimulator::fill_terms`
        // splits it: the closing half fills, the opening half is refused. Neither opens the far
        // side. The capped flatten (qty ≤ |position|) is admitted, so nothing is stranded.
        if let Some(min_qty) = grid.min_qty
            && !covered_reduce
            && req.qty.abs() < min_qty
        {
            return Self::deny(OrderDenial::BelowMinQty);
        }
        // stop orders carry price=None + trigger_price; use trigger before mark
        let own_price = limit_price.or(req.trigger_price);
        let ref_price = own_price.unwrap_or(ctx.mark_price);
        // A priceless order on a symbol `AdmitSymbol` installed (decision 0173) whose reference price
        // has not arrived: its notional below would be ZERO and every price-based cap would pass it.
        // Only an `on_demand` row asks (a declared leg and the mounted symbol are judged as before),
        // a covered reduce is never stranded by it, and the table is read once per opening order (whether the
        // row asks decides whether the verdict read the mark).
        // ...and so is an order with a price of its own on such a symbol while the ENGINE holds no mark
        // for it: the collar and the exposure caps would have nothing to judge it against.
        if !covered_reduce
            && limit.grid_by_symbol.get(req.symbol.as_str()).is_some_and(|g| g.on_demand)
        {
            *market = true; // whether the engine holds a mark for the symbol
            if !(ref_price.is_finite() && ref_price > 0.0)
                || !(ctx.mark_price.is_finite() && ctx.mark_price > 0.0)
            {
                return Self::deny(OrderDenial::NoReferencePrice);
            }
        }
        // A priceless order's notional, below, is the mark's: the floor and the cap read it.
        if own_price.is_none()
            && !covered_reduce
            && (grid.min_notional.is_some() || limit.max_notional_per_order.is_some())
        {
            *market = true;
        }
        // NOTIONAL IS A MAGNITUDE, every factor ABSOLUTE: a COMBO's `price` is the SIGNED net
        // (`OrderCombo::net_limit`), NEGATIVE for a credit structure, which would trip
        // `min_notional` on every credit combo while `notional > cap` could never trip. It
        // includes the CONTRACT MULTIPLIER, as the margin calc below and `OrderBrokerSimulator::apply_fill`
        // do, so `min_notional` gates identically live and backtest for options and inverse perps.
        let notional = ctx.contract_value.order_notional(req.qty, ref_price);
        // ⚠ `min_notional` is the VENUE's floor, stated in the venue's own (QUOTE) currency, while
        // `notional` is in the ACCOUNT's (the contract value carries the rate). The comparison
        // converts the venue's number: `min_notional · ctx.quote_rate`, never the engine's. The rate
        // is exactly `1.0` for a symbol quoted in the account's currency, and `x · 1.0` is `x`.
        if let Some(min_notional) = grid.min_notional
            && !covered_reduce
            && notional < min_notional * ctx.quote_rate
        {
            // ⚠ A priceless order the gate cannot price (no price or trigger of its own, no usable mark) was
            // valued at ZERO above, which is no verdict on its SIZE: say what is missing, as the on-demand lane
            // does. The first market order of a labelled live engine whose price lane has not delivered a mark
            // yet read `below-min-notional` for 10.53 USD (the CI box, 2026-10-25). Only the NAME of a refusal moves:
            // the order was denied here already. An INVERSE contract's notional never reads the price, so its
            // floor stays a size verdict.
            if own_price.is_none()
                && !(ctx.mark_price.is_finite() && ctx.mark_price > 0.0)
                && ctx.contract_value.kind == vike_model::NotionalKind::Linear
            {
                return Self::deny(OrderDenial::NoReferencePrice);
            }
            return Self::deny(OrderDenial::BelowMinNotional);
        }

        // per-order notional cap. A COVERED reduce passes it, as it passes the floors and the account ceiling: a
        // position built from several orders is worth more than one order's cap, and its flatten is one order, so
        // this lane left a panic close open. An overshoot and a flagged add are no covered reduce and stay capped.
        if let Some(cap) = limit.max_notional_per_order
            && !covered_reduce
            && notional > cap
        {
            return Self::deny(OrderDenial::OverMaxNotional);
        }

        // projected exposure cap — ONE SYMBOL at ONE VENUE, not the account: `ctx.position_size`
        // is the order symbol's own bucket (`Engine::gate_position_size`), and
        // `RiskLimits::max_total_exposure`'s doc carries the full statement. Pinned by
        // `crates/vike-execution/tests/risk/risk_lane_pricing.rs`'s
        // `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`. Valued at `ctx.mark_price`
        // with the contract multiplier like its siblings, over `execution_quantity` (the lane the amend
        // double count was widest in). Computed ONCE and shared with the account lane below, so
        // the two ceilings can never disagree about what this order does to its own symbol.
        let projected_symbol_notional = ctx.contract_value.gross_notional(
            ctx.position_size + req.side as f64 * execution_quantity,
            ctx.mark_price,
        );
        // the projection is valued at the mark
        *market |= limit.max_total_exposure.is_some();
        if let Some(cap) = limit.max_total_exposure
            && projected_symbol_notional > cap
        {
            return Self::deny(OrderDenial::OverMaxExposure);
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
        // the account's exposure is resolver-priced, and so is the projection added to it
        *market |= limit.max_account_exposure.is_some() && !covered_reduce;
        if let Some(cap) = limit.max_account_exposure
            && !covered_reduce
        {
            // ⚠ FAIL-CLOSED: the producer answers a non-finite figure when a position or live order
            // it had to count has no price (a restart's positions before the first quote). A book
            // that cannot be valued cannot be judged against the ceiling, so an order that is not a
            // covered reduce is denied until it can; `NaN > cap` is false, so this must be its own test.
            if !ctx.account_exposure_excl_order.is_finite() {
                return Self::deny_as(
                    OrderDenial::AccountExposureUnpriced,
                    format!(
                        "{} (an open position or live order on this account has no price yet, so max_account_exposure cannot be judged)",
                        OrderDenial::AccountExposureUnpriced
                    ),
                );
            }
            let projected = projected_symbol_notional + ctx.account_exposure_excl_order;
            if projected > cap {
                return Self::deny_as(
                    OrderDenial::OverAccountExposure,
                    format!(
                        "{} (projected {projected:.2} > max_account_exposure {cap:.2})",
                        OrderDenial::OverAccountExposure
                    ),
                );
            }
        }

        // pre-trade buying-power check (RUST-NATIVE; LEAN BuyingPowerModel semantics), before the
        // throttle so a denied order never consumes a rate slot. A COVERED reduce bypasses: it
        // frees margin rather than consuming it, so charging it could deny the very exit that
        // releases the margin. An UNCOVERED order (flat book, overshooting reversal) opens
        // exposure whatever its flag, and faces buying power like any opening order.
        if let Some(initial_margin_requirement) = limit.initial_margin_for(&req.symbol)
            && !covered_reduce
        {
            *market = true; // equity, committed margin and the closing credit are resolver-priced
            // `execution_quantity`, not the wire qty ([`still_executable`]): `ctx.margin_used` already funds
            // the amended order's executed lots as POSITION.
            let order_margin = ctx.contract_value.initial_margin(
                ref_price,
                req.side as f64 * execution_quantity,
                1.0,
                initial_margin_requirement,
            );
            let free = vike_model::free_buying_power(
                ctx.equity,
                ctx.margin_used,
                ctx.closing_credit,
                limit.required_free_buying_power_percent,
            );
            if !vike_model::has_sufficient_margin(free, order_margin) {
                return Self::deny(OrderDenial::InsufficientMargin);
            }
        }

        // pre-trade market-impact veto (OPT-IN; RUST-NATIVE), before the throttle like the margin
        // check. Unarmed it does NO book work (`impact_veto` returns on `budget?`). A COVERED
        // reduce never impact-vetoes (`OrderBrokerSimulator::apply_fill`: closing fills always execute): a
        // flatten in a liquidity vacuum is when the book looks worst and the exit must go through.
        // Like the floors above: an uncovered REVERSAL is vetoable whole, and the capped flatten
        // (qty <= |position|) still bypasses, so no position is stranded.
        if let Some(b) = book
            && !covered_reduce
            && (limit.require_fillable || limit.max_slippage_bps.is_some())
        {
            *market = true; // the live L2 book
            // `execution_quantity` for the same reason as the lanes above. No amend reaches here today
            // (`check_modify` passes no book); spelled correctly so wiring one in later cannot
            // reintroduce the double count in this lane.
            let scope = take_scope(b, req.side, &req.order_type, req.price, req.peg.as_ref());
            if let Some(d) = scoped_impact_veto(
                b,
                scope,
                req.side,
                execution_quantity.abs(),
                limit.max_slippage_bps,
                limit.require_fillable,
                ctx.contract_value,
            ) {
                return Self::deny(d.denial());
            }
        }

        // sliding-window throttle (only accepted orders consume a slot)
        if consume_throttle && !self.admit_throttle(ctx.now_ms) {
            return Self::deny(OrderDenial::RateLimited);
        }

        RiskVerdict { ok: true, request: Some(req), reason: String::new(), denial: None }
    }

    /// ATOMIC per-leg crossing for a COMBO order (combo-orders spec §5, conservative v1).
    ///
    /// A combo is ONE venue order carrying N legs (`OrderRequest::combo_legs`, `price` = the
    /// SIGNED net limit — NEGATIVE for a credit structure such as a short condor). The gate has
    /// no combo grid and no strategy-aware margin, so v1 prices the RISK off each leg's OWN mark:
    ///
    /// * every leg is crossed as a synthetic single-leg request — qty `|ratio| × combo_qty`, side
    ///   `sign(ratio) × combo_side`, `price`/`trigger_price` cleared so the leg's notional prices
    ///   off `leg_context(symbol).mark_price`;
    /// * **ALL legs must pass.** The FIRST failure denies the WHOLE combo with ONE verdict whose
    ///   reason names the failing leg (`"leg BTC-…-C: below-min-qty"`); no partial admission
    ///   exists, matching the atomic-reject FSM contract (spec §2);
    /// * the combo consumes **ONE** throttle slot, taken only after every leg passes;
    /// * the NET limit is deliberately NOT used for notional, and a defined-risk spread is
    ///   DOUBLE-COUNTED as naked legs ON PURPOSE: a combo must never pass a gate its naked legs
    ///   would fail.
    ///
    /// **The legs ACCUMULATE**, because `leg_context` is a per-symbol snapshot from BEFORE the combo
    /// and cannot know what earlier legs committed:
    ///
    /// * **buying power** — each admitted, non-reducing leg's initial margin (the SAME
    ///   [`vike_model::initial_margin`] the gate body uses, off its normalized qty) is added to the
    ///   next leg's `margin_used`;
    /// * **account-aggregate exposure** — each admitted leg's gross notional is added to the next
    ///   leg's [`RiskContext::account_exposure_excl_order`].
    ///
    /// A combo naming ONE symbol in two legs is refused whole (`repeated-leg-symbol (<symbol>)`):
    /// `OrderCombo::validate` refuses such a spec first (`ComboError::RepeatedLegSymbol`), so this is
    /// the belt for a hand-built request, because the per-leg books downstream (`leg_context` here,
    /// `OrderRecord::combo_leg_fills`) are keyed by symbol and its legs could not be told apart.
    ///
    /// **The wire owes the grid multiple.** A leg whose `|ratio| × qty` the leg's own grid would
    /// RESIZE (`check_inner` rounds it to the lot) refuses the whole combo
    /// (`leg <symbol>: off-grid-qty ...`): the request goes out VERBATIM, so a resized leg would leave
    /// the venue, or the paper book, the off-grid size the simulator refuses. Float noise of an
    /// on-grid size (0.3 on a 0.1 lot) is not a resize.
    ///
    /// Account-level facts (`trading_state`, `now_ms`) come from `ctx` and OVERRIDE `leg_context`'s: a
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
        leg_context: F,
    ) -> RiskVerdict
    where
        F: Fn(&str) -> RiskContext,
    {
        let mut market = false;
        let verdict = self.judge_combo(request, ctx, leg_context, &mut market);
        self.read_market_data = market;
        verdict
    }

    /// [`Self::check_combo`]'s body; `market` is set once a leg's mark is read (every leg is priced at its own).
    fn judge_combo<F>(
        &mut self,
        request: &OrderRequest,
        ctx: &RiskContext,
        leg_context: F,
        market: &mut bool,
    ) -> RiskVerdict
    where
        F: Fn(&str) -> RiskContext,
    {
        // the "this is NOT a combo" sentinel (`build_combo` can never produce it)
        if request.combo_legs.is_empty() {
            return Self::deny(OrderDenial::NotACombo);
        }
        // One symbol in two legs (see the doc above): refused, never counted.
        let legs = &request.combo_legs;
        if let Some(dup) =
            legs.iter().enumerate().find(|(i, l)| legs[..*i].iter().any(|e| e.symbol == l.symbol))
        {
            return Self::deny_as(
                OrderDenial::RepeatedLegSymbol,
                format!("{} ({})", OrderDenial::RepeatedLegSymbol, dup.1.symbol),
            );
        }
        if request.side != 1 && request.side != -1 {
            return Self::deny(OrderDenial::InvalidSide);
        }
        if ctx.trading_state == TradingState::Halted {
            return Self::deny(OrderDenial::Halted);
        }
        // INFINITY is not caught by `<= 0.0`, and an infinite leg qty sails through every
        // UNARMED cap, so require finiteness explicitly.
        if !request.qty.is_finite() || request.qty <= 0.0 {
            return Self::deny(OrderDenial::NonPositiveSize);
        }

        // Running commitment of the legs admitted SO FAR (see the doc above): margin, and gross
        // notional added to the ACCOUNT.
        let mut committed_margin = 0.0f64;
        let mut committed_notional = 0.0f64;

        for leg in &request.combo_legs {
            // `OrderCombo::validate` rejects ratio 0 but a hand-built request can carry it; without
            // this it surfaces as a misleading `invalid-side` from the per-leg check.
            if leg.ratio == 0 {
                return Self::deny_as(
                    OrderDenial::LegZeroRatio,
                    format!("leg {}: {}", leg.symbol, OrderDenial::LegZeroRatio),
                );
            }

            let base = leg_context(&leg.symbol);
            *market = true;
            // `leg_context` is TOTAL: a missing mark comes back as 0.0, which passes every
            // price-based limit at any size. Refuse instead.
            if !base.mark_price.is_finite() || base.mark_price <= 0.0 {
                return Self::deny_as(
                    OrderDenial::LegNoMark,
                    format!("leg {}: {}", leg.symbol, OrderDenial::LegNoMark),
                );
            }
            // `ctx` owns the trading state and the throttle clock (doc above); the account fields
            // carry the earlier legs' running commitment.
            let pos_before = base.position_size;
            let lctx = RiskContext {
                trading_state: ctx.trading_state,
                now_ms: ctx.now_ms,
                position_size: pos_before,
                margin_used: base.margin_used + committed_margin,
                account_exposure_excl_order: base.account_exposure_excl_order + committed_notional,
                ..base
            };

            let mut leg_request = request.clone();
            leg_request.symbol = leg.symbol.clone();
            // sign law: selling the combo flips every leg (spec §1).
            leg_request.side = request.side.signum() * leg.ratio.signum();
            leg_request.qty = f64::from(leg.ratio.unsigned_abs()) * request.qty;
            // the leg prices off its OWN mark: never off the (possibly negative) net.
            leg_request.price = None;
            leg_request.trigger_price = None;
            // a synthetic leg is a plain single-leg order, not a nested combo
            leg_request.combo_legs = Vec::new();
            // reduce_only is a PER-LEG fact, never inherited: a reduce_only combo whose legs open
            // new positions would launder each past buying power, the impact veto and `Reducing`.
            // Derived from the leg's projected book — exactly `RiskGate::reduces`' second clause.
            leg_request.reduce_only =
                pos_before != 0.0 && (f64::from(leg_request.side) * pos_before) < 0.0;

            // `0.0`: a synthetic leg is fresh, so nothing of it has executed.
            let v = self.check_inner(&leg_request, &lctx, None, false, 0.0);
            let Some(admitted) = v.request.filter(|_| v.ok) else {
                // The leg's own lane names the denial; the reason says which leg it was.
                let reason = format!("leg {}: {}", leg.symbol, v.reason);
                return RiskVerdict { ok: false, request: None, reason, denial: v.denial };
            };
            // The doc's grid rule: the lot rounding `check_inner` did is a resize only past float
            // noise of the wanted size.
            if (admitted.qty - leg_request.qty).abs() > 1e-9 * leg_request.qty.abs() {
                return Self::deny_as(
                    OrderDenial::LegOffGridQty,
                    format!(
                        "leg {}: {} ({} is not a lot multiple; the grid takes {})",
                        leg.symbol,
                        OrderDenial::LegOffGridQty,
                        leg_request.qty,
                        admitted.qty
                    ),
                );
            }

            // Fold this leg into the running commitment off the gate's OWN normalized request and
            // the SAME margin fn `check_inner` used — one authority, not a second formula.
            // Reducing legs commit nothing (they are the margin bypass).
            let signed_quantity = f64::from(admitted.side) * admitted.qty;
            let pure_reduce = admitted.reduce_only
                || (signed_quantity * pos_before < 0.0 && pos_before.abs() >= admitted.qty.abs());
            if !pure_reduce
                && let Some(initial_margin_requirement) =
                    self.limits.initial_margin_for(&admitted.symbol)
            {
                committed_margin += lctx
                    .contract_value
                    .initial_margin(
                        lctx.mark_price,
                        signed_quantity,
                        1.0,
                        initial_margin_requirement,
                    )
                    .abs();
            }
            // …and the ACCOUNT-exposure commitment: GROSS like the account fold it adds to; a
            // reducing leg adds nothing, mirroring the margin term above.
            if !pure_reduce {
                committed_notional +=
                    lctx.contract_value.gross_notional(signed_quantity, lctx.mark_price);
            }
        }

        // ONE slot for the whole combo, and only once every leg passed.
        if !self.admit_throttle(ctx.now_ms) {
            return Self::deny(OrderDenial::RateLimited);
        }

        RiskVerdict {
            ok: true,
            request: Some(request.clone()),
            reason: String::new(),
            denial: None,
        }
    }
}

#[cfg(test)]
mod tests;
