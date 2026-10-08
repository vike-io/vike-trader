//! The amend-path accounting for a PARTIALLY FILLED order, in BOTH directions: the executed qty is
//! not charged twice on an IN-PLACE amend venue, and is still charged in full on a CANCEL-REPLACE
//! one, where charging it is CORRECT, not conservative.
//!
//! `ManagedOrder::apply` rewrites `request.qty` wholesale on `Event::OrderModified` and leaves
//! `filled_qty` untouched, so an amend's `new_qty` is the new TOTAL.
//! `ExecutionEngine::modify_order` gates the PROJECTED order while `RiskContext::position_size`
//! (`ExecutionEngine::gate_position_size`) already holds that order's own fills, so un-netted, the
//! executed lots sit on BOTH sides of every `position + side × qty` projection: exposure, buying
//! power, the `Halted` exemption (`vike_model::is_covered_reduce`) and the reduce-only overshoot
//! guard.
//!
//! ⚠ **Double-counting errs toward DENY; a careless netting ADMITS, on a live venue.** So every
//! lane is pinned twice (the double count is gone; a genuinely over-limit amend of the same
//! partially-filled order is still denied), and the lanes that judge the wire order are pinned NOT
//! to move.
//!
//! The convention is a per-venue fact (`vike_model`'s `AmendSemantics`), so this file drives the
//! REAL table: [`in_place_and_replace_are_what_the_table_says`] ties the venue constants to it. The
//! engine's venue string is a LABEL, not a client, so `vike_exec::ExecutionClient::amend_semantics`
//! answers FIRST and the table only when the client declines
//! ([`a_client_that_declares_its_own_convention_overrides_the_venue_table`]).
//!
//! Every assertion is on what reached the venue (`RecordingClient::modifies`), never on an internal
//! call count.

use vike_exec::risk::still_executable;
use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Outbox, RiskLimits, TradingState};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderPartiallyFilled, OrderSubmitted};
use vike_model::{AmendSemantics, OrderRequest};

use crate::support::EngineBuilder;

/// A venue whose amend leaves the order in place with its executions attached, so the amend's qty is
/// the new TOTAL and the executed part must be netted out of every position projection.
const IN_PLACE: &str = "binance";
/// A venue whose amend is a native cancel-replace: the resting order dies and a FRESH order of the
/// amend's qty takes its place, carrying no execution history — so the whole qty is still coming.
const CANCEL_REPLACE: &str = "hyperliquid";
/// A roster venue that IS amendable but whose convention is deliberately not established. It must
/// behave exactly like [`CANCEL_REPLACE`]: an undeclared venue keeps the conservative arithmetic.
const UNDECLARED: &str = "aster";

const SYMBOL: &str = "BTCUSDT";
/// The order that rests, and the part of it that executes before the amend.
const RESTING_QTY: f64 = 10.0;
const EXECUTED: f64 = 4.0;
const PX: f64 = 100.0;

fn engine(venue: &str, limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    EngineBuilder { venue: venue.into(), symbol: SYMBOL.into(), limits, ..Default::default() }
        .build()
}

fn limit(
    venue: &str,
    coid: &str,
    side: i32,
    qty: f64,
    price: f64,
    reduce_only: bool,
) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(price),
        reduce_only,
        ts: 1,
        ..Default::default()
    }
}

fn fill(
    venue: &str,
    coid: &str,
    trade_id: &'static str,
    side: i32,
    qty: f64,
    px: f64,
) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: venue.into(),
        symbol: SYMBOL.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".to_string().into(),
        ts: 0,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    }
}

/// Submit `req` and drive it to ACCEPTED through the real emitter split, so the order rests and is
/// `is_modifiable()`.
fn rest(e: &mut ExecutionEngine<RecordingClient>, req: &OrderRequest) {
    let before = e.client.submissions.len();
    let mut outbox = Outbox::default();
    e.submit_order(req, 0, &mut outbox);
    assert_eq!(
        e.client.submissions.len(),
        before + 1,
        "precondition: the order itself must be admitted, or the test proves nothing"
    );
    let coid = req.client_order_id.clone();
    let mut bus = EventBus::new();
    bus.publish(Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.clone(), ts: 0 }), e);
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        e,
    );
}

/// Deliver ONE partial execution of a resting order exactly as a venue lane does: the bare
/// `Event::Fill` the ACCOUNT folds into the position, then the `OrderPartiallyFilled` WRAP the FSM
/// folds into `filled_qty`. Both are needed: they land on OPPOSITE sides of the gate's sum.
fn partially_fill(
    e: &mut ExecutionEngine<RecordingClient>,
    venue: &str,
    coid: &str,
    trade_id: &'static str,
    side: i32,
    qty: f64,
    px: f64,
) {
    let mut bus = EventBus::new();
    let f = fill(venue, coid, trade_id, side, qty, px);
    bus.publish(Event::Fill(f.clone()), e);
    bus.publish(
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid.into(),
            fill: f,
            ts: 2,
        }),
        e,
    );
}

/// Fold a position onto the account with no local order behind it (a bare fill for an unregistered
/// coid folds into the `Account` only): the reduce-only cases' "you already hold this".
fn seed_position(
    e: &mut ExecutionEngine<RecordingClient>,
    venue: &str,
    side: i32,
    qty: f64,
    px: f64,
) {
    let mut bus = EventBus::new();
    bus.publish(Event::Fill(fill(venue, "seed", "seed-trade", side, qty, px)), e);
    assert_eq!(
        e.position_size("BOTH"),
        side as f64 * qty,
        "precondition: the seeded position must be folded"
    );
}

fn rejection(outbox: &Outbox) -> Option<String> {
    outbox.0.iter().find_map(|ev| match ev {
        Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
        _ => None,
    })
}

/// Issue one amend and report `(reached_the_venue, rejection_reason)`.
fn amend(
    e: &mut ExecutionEngine<RecordingClient>,
    coid: &str,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> (bool, Option<String>) {
    let before = e.client.modifies.len();
    let mut outbox = Outbox::default();
    e.modify_order(coid, new_qty, new_price, 5, &mut outbox);
    (e.client.modifies.len() > before, rejection(&outbox))
}

/// A long BUY that rests and then partially executes — the setup every exposure/margin row shares.
fn rested_and_partly_filled(venue: &str, limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    let mut e = engine(venue, limits);
    rest(&mut e, &limit(venue, "c1", 1, RESTING_QTY, PX, false));
    partially_fill(&mut e, venue, "c1", "t1", 1, EXECUTED, PX);
    e
}

/// The three venue constants must be what `vike_model::amend_semantics` says, or a row change there
/// leaves every test below green while asserting the opposite of its name.
#[test]
fn in_place_and_replace_are_what_the_table_says() {
    assert_eq!(vike_model::amend_semantics(IN_PLACE), AmendSemantics::InPlaceTotal);
    assert_eq!(vike_model::amend_semantics(CANCEL_REPLACE), AmendSemantics::CancelReplace);
    assert_eq!(vike_model::amend_semantics(UNDECLARED), AmendSemantics::Unknown);
}

/// THE PREMISE: after a partial fill the SAME executed qty is both account position and part of the
/// resting order's `qty`, which stays the TOTAL (`ManagedOrder::accumulate_fill` tracks it in
/// `filled_qty` and never subtracts it from `request.qty`), so the outstanding size is DERIVED.
#[test]
fn the_executed_qty_is_both_the_position_and_part_of_the_resting_orders_qty() {
    let e = rested_and_partly_filled(IN_PLACE, RiskLimits::new());

    assert_eq!(e.position_size("BOTH"), EXECUTED, "the executed part became position");
    let mo = e.registry.get("c1").expect("the order still rests");
    assert_eq!(mo.filled_qty, EXECUTED, "…and the SAME quantity is the order's filled_qty");
    assert_eq!(
        mo.request.qty, RESTING_QTY,
        "…while request.qty stays the TOTAL — outstanding is derived, never stored"
    );
    assert!(mo.status.is_modifiable(), "a partially filled order is amendable");
}

// --- LANE 1 — projected exposure ---

/// The cap every exposure row shares, chosen so the resting order is comfortably admitted at SUBMIT
/// (`RESTING_QTY × PX` is under it): a refused re-quote is a contradiction, not a configuration.
const EXPOSURE_CAP: f64 = 1_200.0;

fn exposure_verdict(venue: &str, amend_total: f64) -> (bool, Option<String>) {
    let mut e = rested_and_partly_filled(
        venue,
        RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() },
    );
    amend(&mut e, "c1", Some(amend_total), None)
}

/// The largest amend TOTAL that still reaches the venue — derived by probing the real gate rather
/// than asserted, so the number cannot be "corrected" to match a broken implementation.
fn exposure_boundary(venue: &str) -> f64 {
    let mut last_admitted = 0.0;
    for total in 1..=30 {
        let total = f64::from(total);
        if exposure_verdict(venue, total).0 {
            last_admitted = total;
        }
    }
    last_admitted
}

/// THE ARITHMETIC IDENTITY, pinned as a DIFFERENCE rather than two hardcoded numbers: same order,
/// fill and cap, the admit/deny boundary sits exactly `EXECUTED` lots higher on the in-place venue,
/// the only class where executed lots are not charged twice. Charging the total puts both
/// boundaries in one place; any other netting moves them apart by something else.
#[test]
fn the_exposure_boundary_moves_by_exactly_the_executed_qty() {
    let in_place = exposure_boundary(IN_PLACE);
    let replace = exposure_boundary(CANCEL_REPLACE);

    assert!(
        replace > 0.0,
        "the cancel-replace venue must admit SOME amend, or nothing is measured"
    );
    assert_eq!(
        in_place - replace,
        EXECUTED,
        "in-place boundary {in_place} vs cancel-replace {replace}: they must differ by exactly the \
         executed qty"
    );
    // …and an UNDECLARED venue is on the conservative side of that gap, not the permissive one.
    assert_eq!(
        exposure_boundary(UNDECLARED),
        replace,
        "an undeclared venue keeps today's arithmetic"
    );
}

/// The discriminating pair on the in-place venue: an amend whose outstanding part fits the cap is
/// admitted, and one lot more is refused.
#[test]
fn an_in_place_amend_is_judged_on_what_the_order_can_still_execute() {
    let admits = EXPOSURE_CAP / PX; // the whole post-amend position the cap allows
    let (sent, reason) = exposure_verdict(IN_PLACE, admits);
    assert!(
        sent,
        "a total whose OUTSTANDING part fits exactly under the cap is admitted: {reason:?}"
    );

    let (sent, reason) = exposure_verdict(IN_PLACE, admits + 1.0);
    assert!(!sent, "one lot past it must be refused — the ceiling still binds");
    assert!(
        reason.is_some_and(|r| r.contains("over-max-exposure")),
        "and the refusal must name the exposure lane"
    );
}

/// The same pair on the cancel-replace venue, where the whole-qty sum is CORRECT: a replacement
/// order really can execute its whole qty on top of the position, so the amend the in-place venue
/// admits must be refused here. This is the row a careless fix breaks.
#[test]
fn a_cancel_replace_amend_is_still_judged_on_the_whole_qty() {
    let admits = EXPOSURE_CAP / PX;
    let (sent, reason) = exposure_verdict(CANCEL_REPLACE, admits);
    assert!(!sent, "netting must NOT happen on a venue that rests a fresh order");
    assert!(reason.is_some_and(|r| r.contains("over-max-exposure")));

    // What it does admit is exactly `EXECUTED` lots less.
    let (sent, reason) = exposure_verdict(CANCEL_REPLACE, admits - EXECUTED);
    assert!(sent, "…and it admits the amend whose WHOLE qty fits under the cap: {reason:?}");
}

/// A maker's null re-quote (the terms the order already rests on) is admitted after a partial fill
/// on the in-place venue, where it asks for nothing new; otherwise a quote stops being re-priceable
/// the moment it starts getting hit.
#[test]
fn a_null_re_quote_survives_a_partial_fill_on_an_in_place_venue() {
    let mut e = rested_and_partly_filled(
        IN_PLACE,
        RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() },
    );
    let (sent, reason) = amend(&mut e, "c1", Some(RESTING_QTY), Some(PX));
    assert!(sent, "the order cannot hold more than it already asked for: {reason:?}");
}

/// THE OTHER DIRECTION: a genuinely over-limit amend of a partially filled order stays denied on
/// EVERY venue class. Admitting it breaks the ceiling rather than correcting the arithmetic.
#[test]
fn a_genuinely_over_limit_amend_of_a_partially_filled_order_is_still_denied() {
    for venue in [IN_PLACE, CANCEL_REPLACE, UNDECLARED] {
        let mut e = rested_and_partly_filled(
            venue,
            RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() },
        );
        let (sent, reason) = amend(&mut e, "c1", Some(100.0), None);
        assert!(!sent, "{venue}: an amend to 100 lots must never reach the venue under this cap");
        assert!(
            reason.is_some_and(|r| r.contains("over-max-exposure")),
            "{venue}: and it must be the exposure lane that says so"
        );
        assert!(
            e.registry.get("c1").is_some_and(|mo| mo.request.qty == RESTING_QTY),
            "{venue}: a denied amend leaves the resting terms alone"
        );
    }
}

/// The netting is scoped to the ORDER BEING AMENDED, not the position: a new order submitted while
/// the partially-filled one rests is judged on its whole qty.
#[test]
fn the_submit_path_nets_nothing() {
    let mut e = rested_and_partly_filled(
        IN_PLACE,
        RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() },
    );
    let before = e.client.submissions.len();
    let mut outbox = Outbox::default();
    // |EXECUTED + 9| × PX = 1300 > the cap. Netting on the submit path would admit it.
    e.submit_order(&limit(IN_PLACE, "c2", 1, 9.0, PX, false), 6, &mut outbox);
    assert_eq!(e.client.submissions.len(), before, "a new order is judged on its whole qty");
}

// --- LANE 2 — buying power ---

/// Buying power, both directions, built from explicit state rather than a folded fill (the
/// `risk_lane_pricing.rs` idiom) so the arithmetic is unambiguous: `margin_used` already funds the
/// `EXECUTED` lots. In place, the amend is charged for the UNEXECUTED part only, so an amend the
/// account affords is admitted; on cancel-replace the whole qty is new margin and is refused; and
/// an amend to a total the account cannot fund is still refused.
#[test]
fn the_buying_power_lane_charges_only_what_can_still_execute() {
    const EQUITY: f64 = 120.0;
    const IM: f64 = 0.1;

    let build = |venue: &str, amend_total: f64| {
        let mut e = engine(venue, RiskLimits { im_requirement: Some(IM), ..RiskLimits::new() });
        e.equity_seed = EQUITY;
        rest(&mut e, &limit(venue, "c1", 1, RESTING_QTY, PX, false));
        e.account.positions.insert(
            (venue.into(), SYMBOL.into(), "BOTH".into()),
            vike_exec::PositionEntry { size: EXECUTED, avg_px: PX, ..Default::default() },
        );
        e.account.set_mark_from(venue, SYMBOL, PX, vike_exec::MarkSource::VenueMark, 0);
        e.price_board.set_mark(venue, SYMBOL, PX, 1);
        e.registry.get_mut("c1").expect("resting").filled_qty = EXECUTED;
        amend(&mut e, "c1", Some(amend_total), None)
    };

    // free BP = EQUITY − EXECUTED × PX × IM = 80; the unexecuted 6 lots need 60, the whole 10 need
    // 100. So the same amend is affordable on one convention and not on the other.
    let (sent, reason) = build(IN_PLACE, RESTING_QTY);
    assert!(sent, "in place: only the unexecuted part is new margin, and it fits: {reason:?}");

    let (sent, reason) = build(CANCEL_REPLACE, RESTING_QTY);
    assert!(!sent, "cancel-replace: the whole qty IS new margin, and it does not fit");
    assert!(reason.is_some_and(|r| r.contains("insufficient-margin")));

    // THE CEILING STILL BINDS on the in-place venue: 16 unexecuted lots need 160 against 80 free.
    let (sent, reason) = build(IN_PLACE, 20.0);
    assert!(!sent, "an amend the account cannot fund is still refused after the fix");
    assert!(
        reason.is_some_and(|r| r.contains("insufficient-margin")),
        "and the refusal still comes from the buying-power lane"
    );
}

// --- LANE 3 — the Halted kill-switch exemption (coverage) ---

/// A half-done EXIT under a halt. `vike_model::is_covered_reduce` compares the position against the
/// order's qty, and an exit's executed part has ALREADY left the position: measuring the total
/// would make a covered exit stop reading as covered once it starts working, and the kill switch
/// refuse to re-price the order it exists to let through (`docs/ops/kill-switches.md`).
///
/// Both directions: the pure re-price is admitted on the in-place venue, and an amend that would
/// OVERSHOOT the remaining position is still refused there: `Halted` never admits opening risk.
#[test]
fn a_halted_re_price_of_a_half_done_exit_is_admitted_but_an_overshoot_is_not() {
    let exit = |venue: &str, fill_first: bool| {
        let mut e = engine(venue, RiskLimits::new());
        seed_position(&mut e, venue, 1, RESTING_QTY, PX); // long 10
        rest(&mut e, &limit(venue, "x1", -1, RESTING_QTY, 99.0, true)); // reduce-only exit for all of it
        if fill_first {
            partially_fill(&mut e, venue, "x1", "t1", -1, EXECUTED, 99.0);
        }
        e.trading_state = TradingState::Halted;
        e
    };

    // Control: nothing executed, coverage is exact, the halt exemption admits the re-price.
    let (sent, reason) = amend(&mut exit(IN_PLACE, false), "x1", None, Some(98.0));
    assert!(
        sent,
        "unfilled: the exit covers the position, so the re-price is admitted ({reason:?})"
    );

    // THE FIX: half done, and still covered — the outstanding exit exactly matches what is left.
    let (sent, reason) = amend(&mut exit(IN_PLACE, true), "x1", None, Some(98.0));
    assert!(
        sent,
        "in place: the outstanding exit still covers the remaining position ({reason:?})"
    );

    // THE CEILING: an amend whose outstanding part OVERSHOOTS the remaining position is opening
    // risk, and a halt must refuse it.
    let (sent, reason) = amend(&mut exit(IN_PLACE, true), "x1", Some(30.0), Some(98.0));
    assert!(!sent, "an overshooting amend is not a covered reduce, halted or otherwise");
    assert!(
        reason.is_some_and(|r| r.contains("halted")),
        "and the refusal comes from the kill-switch lane"
    );

    // The cancel-replace venue keeps the conservative reading: a fresh 10-lot exit against a 6-lot
    // position IS an overshoot there, so the refusal is correct rather than over-conservative.
    let (sent, reason) = amend(&mut exit(CANCEL_REPLACE, true), "x1", None, Some(98.0));
    assert!(!sent, "cancel-replace: a fresh full-size exit overshoots what is left");
    assert!(reason.is_some_and(|r| r.contains("halted")));
}

// --- LANE 4 — the reduce-only overshoot guard ---

/// The opt-in overshoot guard reads the same comparison as the halt exemption, with the same
/// ceiling: an amend that really would overshoot is still refused.
#[test]
fn the_reduce_only_overshoot_guard_reads_what_can_still_execute() {
    let exit = |venue: &str| {
        let mut e =
            engine(venue, RiskLimits { block_reduce_only_overshoot: true, ..RiskLimits::new() });
        seed_position(&mut e, venue, 1, RESTING_QTY, PX);
        rest(&mut e, &limit(venue, "x1", -1, RESTING_QTY, 99.0, true));
        e
    };

    // Control: unfilled, exit size == position, not an overshoot.
    let mut e = exit(IN_PLACE);
    let (sent, reason) = amend(&mut e, "x1", None, Some(98.5));
    assert!(sent, "an exit the size of the position is not an overshoot ({reason:?})");

    // THE FIX: after a partial, outstanding exit == remaining position, still not an overshoot.
    partially_fill(&mut e, IN_PLACE, "x1", "t1", -1, EXECUTED, 99.0);
    let (sent, reason) = amend(&mut e, "x1", None, Some(98.0));
    assert!(sent, "in place: the outstanding exit matches what is left ({reason:?})");

    // THE CEILING: raise the amend past the remaining position and it is an overshoot again.
    let (sent, reason) = amend(&mut e, "x1", Some(30.0), None);
    assert!(!sent, "an amend past the remaining position is still an overshoot");
    assert!(reason.is_some_and(|r| r.contains("reduce-only-overshoot")));

    // Conservative on the cancel-replace venue: the replacement really would be full size.
    let mut e = exit(CANCEL_REPLACE);
    partially_fill(&mut e, CANCEL_REPLACE, "x1", "t1", -1, EXECUTED, 99.0);
    let (sent, reason) = amend(&mut e, "x1", None, Some(98.0));
    assert!(!sent, "cancel-replace: a fresh full-size exit overshoots what is left");
    assert!(reason.is_some_and(|r| r.contains("reduce-only-overshoot")));
}

// --- The lanes that judge the ORDER AS IT GOES ON THE WIRE: pinned NOT to move ---

/// The per-order notional cap judges what is SENT, and an in-place amend sends the TOTAL: netting
/// here would silently stop the cap capping the order the venue receives.
#[test]
fn the_per_order_notional_cap_still_measures_the_whole_wire_qty() {
    let mut e = rested_and_partly_filled(
        IN_PLACE,
        RiskLimits { max_notional_per_order: Some(RESTING_QTY * PX), ..RiskLimits::new() },
    );
    // Outstanding would be 7 lots (700) — comfortably under the cap — but 11 lots go on the wire.
    let (sent, reason) = amend(&mut e, "c1", Some(RESTING_QTY + 1.0), None);
    assert!(!sent, "the cap must judge the total the venue is asked to rest");
    assert!(
        reason.is_some_and(|r| r.contains("over-max-notional")),
        "and it must be the per-order cap that says so"
    );
}

/// The venue FLOORS judge the wire order too: an amend whose outstanding part is below the floor
/// but whose TOTAL clears it is admitted.
#[test]
fn the_min_qty_floor_still_measures_the_whole_wire_qty() {
    let floor = EXECUTED + 1.0;
    let mut e = rested_and_partly_filled(
        IN_PLACE,
        RiskLimits { min_qty: Some(floor), ..RiskLimits::new() },
    );
    // Outstanding is 1 lot — under the floor — while the wire carries 5, which clears it.
    let (sent, reason) = amend(&mut e, "c1", Some(floor), None);
    assert!(sent, "the floor judges the order the venue receives, not the remainder: {reason:?}");
}

// --- THE CLAMP — an amend BELOW the executed qty ---

/// `still_executable`'s `.max(0.0)` is the ONLY guard against a NEGATIVE remainder, whose effects
/// contradict each other, so neither reads as "a bit more conservative":
///
/// * the exposure lane computes `|position + side × (negative)|` and UNDER-states the projection
///   (long 8, resting BUY 10 with 8 executed, amend to 2 ⇒ it reads 200 where the truth is 800);
/// * `vike_model::initial_margin` charges `rate × mult × price × qty × im`, so a negative remainder
///   is charged as its MAGNITUDE — an amend that asks for LESS is billed for more;
/// * and `vike_model::is_covered_reduce` is handed a negative qty.
///
/// Beyond the clamp, only the VENUES refuse such an amend (binance rejects it, okx marks the order
/// FILLED), so the identity is pinned here under every lane: **an amend below the executed qty is
/// judged exactly as an amend AT it**, because both leave a ZERO remainder.
#[test]
fn an_amend_below_the_executed_qty_is_judged_as_a_zero_remainder() {
    // The pure arithmetic first, so a failure says which half broke.
    assert_eq!(still_executable(EXECUTED - 2.0, EXECUTED), 0.0, "a below-executed amend nets to 0");
    assert_eq!(still_executable(EXECUTED, EXECUTED), 0.0, "…the same as an amend AT it");

    // LANE 1 — buying power, where a negative remainder is charged as its magnitude. Equity 45 with
    // the executed 4 lots funded at im 0.1 leaves 5 free: a 2-lot remainder (20) is refused, a zero
    // one is free. Injected, because the resting order needs the pre-drawdown equity at submit.
    const IM: f64 = 0.1;
    let margin_verdict = |amend_total: f64| {
        let mut e = engine(IN_PLACE, RiskLimits { im_requirement: Some(IM), ..RiskLimits::new() });
        e.equity_seed = 120.0;
        rest(&mut e, &limit(IN_PLACE, "c1", 1, RESTING_QTY, PX, false));
        e.account.positions.insert(
            (IN_PLACE.into(), SYMBOL.into(), "BOTH".into()),
            vike_exec::PositionEntry { size: EXECUTED, avg_px: PX, ..Default::default() },
        );
        e.account.set_mark_from(IN_PLACE, SYMBOL, PX, vike_exec::MarkSource::VenueMark, 0);
        e.price_board.set_mark(IN_PLACE, SYMBOL, PX, 1);
        e.registry.get_mut("c1").expect("resting").filled_qty = EXECUTED;
        e.equity_seed = 45.0;
        amend(&mut e, "c1", Some(amend_total), None)
    };

    // The lane is ARMED (a 2-lot remainder denies), so a gate admitting everything cannot pass.
    let (sent, reason) = margin_verdict(EXECUTED + 2.0);
    assert!(!sent, "a remainder of 2 lots costs 20 against 5 free and must be refused");
    assert!(reason.is_some_and(|r| r.contains("insufficient-margin")));

    // …and the amend to EXECUTED − 2, whose UNCLAMPED remainder is −2 and would be charged the very
    // same 20, is admitted — because the remainder is zero, not negative.
    let at_zero = margin_verdict(EXECUTED);
    assert!(at_zero.0, "nothing can still execute, so nothing is charged: {:?}", at_zero.1);
    for below in [0.5, 1.0, 2.0, 3.0] {
        assert_eq!(
            margin_verdict(below).0,
            at_zero.0,
            "amend to {below} (below the executed {EXECUTED}) must be judged as the zero remainder \
             it leaves, not as a negative one"
        );
    }

    // LANE 2 — exposure, where a negative remainder UNDER-states the projection instead. The cap is
    // armed AFTER the order rests, because a cap tight enough to discriminate here would have
    // refused the resting order itself.
    let exposure_below = |amend_total: f64| {
        let mut e = rested_and_partly_filled(IN_PLACE, RiskLimits::new());
        // |position 4 + remainder 0| × 100 = 400 is over; the unclamped |4 − 2| × 100 = 200 is not.
        e.gate.limits.max_total_exposure = Some(300.0);
        amend(&mut e, "c1", Some(amend_total), None)
    };
    let at_zero = exposure_below(EXECUTED);
    assert!(!at_zero.0, "the position alone already exceeds this cap, so nothing more may rest");
    for below in [0.5, 1.0, 2.0, 3.0] {
        let (sent, reason) = exposure_below(below);
        assert!(
            !sent,
            "amend to {below}: a remainder that cannot go below zero cannot SHRINK the projected \
             position either ({reason:?})"
        );
        assert!(reason.is_some_and(|r| r.contains("over-max-exposure")));
    }
}

// --- A GARBAGE `already_executed`, and the invariant the netting rests on ---

/// [`still_executable`] is `pub`, so its safety must not depend on the caller
/// (`vike_model::AmendSemantics::already_in_position` screens these values one call away, and a
/// second caller would not inherit that). A NaN (`NaN != 0.0`) or `+INFINITY` second argument
/// would make `(qty − x).max(0.0)` `0.0` (`f64::max` returns the non-NaN operand), vacating the
/// exposure projection to `|position|` and the margin charge to nothing: ANTI-conservative, and
/// invisible to an `== 0.0` test.
#[test]
fn a_garbage_already_executed_nets_nothing() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -3.0, -0.0, 0.0] {
        assert_eq!(
            still_executable(RESTING_QTY, bad),
            RESTING_QTY,
            "already_executed {bad} must leave the qty untouched — the conservative arithmetic"
        );
    }
    // and the ONE shape that legitimately nets is unchanged
    assert_eq!(still_executable(RESTING_QTY, EXECUTED), RESTING_QTY - EXECUTED);
}

/// **THE INVARIANT THE NETTING RESTS ON**: the quantity netted out of an amend must ALREADY be
/// inside `RiskContext::position_size`. Netting a lot that never moved the position hands the gate
/// a smaller order than the one placed, SILENTLY REDUCING measured risk.
///
/// `ManagedOrder::filled_qty` and the account position are fed by the SAME venue event through
/// SEPARATE dedup sets (`ExecutionEngine`'s `seen_trade_ids` for the bare `Event::Fill` that moves
/// the position, `seen_fsm_trade_ids` for the `OrderPartiallyFilled` wrap that moves `filled_qty`),
/// so the coupling is a property of the LANE, not of the data structure. Part 1 pins that the real
/// lane holds it; part 2 pins the DIRECTION of the residual when it is broken.
///
/// ⚠ If the two dedup sets are ever unified, part 2 is expected to fail: that is the FIX, so delete
/// part 2 with a line saying so; do not reintroduce the split.
#[test]
fn the_netting_assumes_filled_qty_is_inside_the_position() {
    // 1. The real lane: one venue execution, delivered as a bare fill + its wrap, moves both by the
    //    same amount — so what `modify_order` nets is exactly what the position already holds.
    let e = rested_and_partly_filled(IN_PLACE, RiskLimits::new());
    let filled = e.registry.get("c1").expect("resting").filled_qty;
    assert_eq!(filled, EXECUTED);
    assert_eq!(
        e.position_size("BOTH"),
        filled,
        "the netted quantity must be one the position already holds"
    );

    // 2. The residual, and its direction. A wrap WITHOUT its bare fill inflates `filled_qty` while
    //    the position stays put — and the gate then nets more than the position ever gained, which
    //    ADMITS more, not less.
    let mut e = rested_and_partly_filled(IN_PLACE, RiskLimits::new());
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: "c1".into(),
            fill: fill(IN_PLACE, "c1", "wrap-only", 1, 3.0, PX),
            ts: 3,
        }),
        &mut e,
    );
    let drifted = e.registry.get("c1").expect("resting").filled_qty;
    assert_eq!(drifted, EXECUTED + 3.0, "the wrap moved filled_qty on its own");
    assert_eq!(e.position_size("BOTH"), EXECUTED, "…and the position did not follow");
    assert!(
        still_executable(RESTING_QTY, drifted) < still_executable(RESTING_QTY, EXECUTED),
        "a filled_qty ahead of the position nets MORE, i.e. under-states the order the gate judges"
    );
}

// --- WHO declares the convention: the client first, the venue string second ---

/// **THE ENGINE'S VENUE STRING IS A LABEL, NOT A CLIENT.** `vike_mount::make_engine` passes the
/// REAL venue id even when the absent-credentials gate fell back to
/// `vike_paper::PaperExecutionClient` (`vike_mount::build_paper_maker_core` likewise passes its
/// profile's venue), so a paper mount on binance would otherwise be judged under `InPlaceTotal`.
///
/// So the client that receives the amend declares its convention
/// (`ExecutionClient::amend_semantics`) and only one that DECLINES (`None`, every venue adapter)
/// falls through to `vike_model::amend_semantics`. The same engine, venue and order run three
/// times, differing ONLY in what the client declares.
/// `crates/vike-paper/tests/paper_amend_is_not_the_venues_amend.rs` drives the REAL paper client
/// through this path; this test pins the PRECEDENCE.
#[test]
fn a_client_that_declares_its_own_convention_overrides_the_venue_table() {
    let verdict = |declared: Option<AmendSemantics>| {
        let mut e = rested_and_partly_filled(
            IN_PLACE,
            RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() },
        );
        e.client.declared_amend_semantics = declared;
        // |4 + 10| × 100 = 1400 under the honest reading, |4 + 6| × 100 = 1000 under netting.
        amend(&mut e, "c1", Some(RESTING_QTY), None)
    };

    // Declining (every venue adapter) ⇒ the venue table answers, and binance nets.
    assert!(verdict(None).0, "a client that declines keeps the venue's InPlaceTotal netting");

    // The paper exchange's convention: the amend qty is the order's REMAINING size, so the whole
    // qty is still coming and nothing may be netted — on the very same `InPlaceTotal` venue.
    let (sent, reason) = verdict(Some(AmendSemantics::InPlaceRemaining));
    assert!(!sent, "a client whose amend qty is the REMAINING size must not net the executed part");
    assert!(reason.is_some_and(|r| r.contains("over-max-exposure")));

    // …and the declaration is not a blanket "never net": a client declaring the in-place TOTAL
    // convention on the same venue behaves exactly like declining.
    assert!(verdict(Some(AmendSemantics::InPlaceTotal)).0);
}

/// The complement, on the OTHER venue class: every CONSERVATIVE declaration is inert on a declared
/// `CancelReplace` venue, because `already_in_position` is non-zero for exactly one variant.
///
/// ⚠ `Some(InPlaceTotal)` is deliberately NOT in this list: a client declaring it about ITSELF
/// WOULD net, on any venue string. That is by design (the client is the authority on what its own
/// `modify` does), and it is why the only override in the tree is the paper exchange's, which nets
/// nothing. A venue adapter must never override this method.
#[test]
fn conservative_client_declarations_never_net_on_a_cancel_replace_venue() {
    for declared in [
        None,
        Some(AmendSemantics::CancelReplace),
        Some(AmendSemantics::InPlaceRemaining),
        Some(AmendSemantics::Unsupported),
        Some(AmendSemantics::Unknown),
    ] {
        let mut e = rested_and_partly_filled(
            CANCEL_REPLACE,
            RiskLimits { max_total_exposure: Some(EXPOSURE_CAP), ..RiskLimits::new() },
        );
        e.client.declared_amend_semantics = declared;
        let (sent, _) = amend(&mut e, "c1", Some(EXPOSURE_CAP / PX), None);
        assert!(!sent, "{declared:?}: the whole qty is still judged on a cancel-replace venue");
    }
}
