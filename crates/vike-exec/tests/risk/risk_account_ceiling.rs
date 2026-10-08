//! The ACCOUNT-aggregate exposure ceiling (`RiskLimits::max_account_exposure`), end to end through
//! `ExecutionEngine::submit_order`: what `max_total_exposure`'s NAME promises and its SCOPE does
//! not deliver. The per-symbol lane stays per-symbol (`risk_lane_pricing.rs`'s
//! `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`); this is a second, independent axis.
//!
//! Every test drives the REAL `submit_order`, never `RiskGate::check` with a hand-built context:
//! what can be wrong in production is the PRODUCER
//! (`ExecutionEngine::resolved_account_exposure_excluding`, folded into `risk_ctx`), and a
//! hand-supplied `account_exposure_excl_order` would pass against an engine that never folds it.

use super::risk_lane_common::*;
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{ExecutionEngine, Outbox, PositionEntry, RiskLimits};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// The engine this suite judges: mounted on `("sim", "BTCUSDT")`, also accepting `ETHUSDT`, with
/// BOTH exposure ceilings armed and every other lane off (`RiskLimits::new()` leaves
/// `im_requirement`/`min_*`/`max_notional_per_order`/the throttle disarmed). `symbol_cap` is
/// deliberately LOOSER per symbol than `account_cap` in aggregate: the only configuration in which
/// the two lanes are distinguishable.
fn account_engine(
    symbol_cap: Option<f64>,
    account_cap: Option<f64>,
) -> ExecutionEngine<RecordingClient> {
    let mut e = engine_with(RiskLimits {
        max_total_exposure: symbol_cap,
        max_account_exposure: account_cap,
        ..RiskLimits::new()
    });
    e.extra_symbols = vec!["ETHUSDT".to_string()];
    e
}

/// Price ONE symbol on BOTH stores, so no resolver arm can move the number under test
/// (`risk_lane_coverage.rs`'s `price_at` idiom, per symbol).
fn price_sym(e: &mut ExecutionEngine<RecordingClient>, venue: &str, symbol: &str, px: f64) {
    e.account.set_mark_from(venue, symbol, px, MarkSource::VenueMark, 0);
    e.price_board.set_mark(venue, symbol, px, 1);
}

/// Book a position the venue would have opened. `RecordingClient` records a submit and never fills,
/// so a test that wants "the first order is now a position" has to say so.
fn seed_pos(e: &mut ExecutionEngine<RecordingClient>, venue: &str, symbol: &str, size: f64) {
    e.account.positions.insert(
        (venue.into(), symbol.into(), "BOTH".into()),
        PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
}

/// **A submitted order COMPLETING**: the registry entry goes terminal AND the position appears. The
/// account fold counts LIVE orders as well as positions
/// (`ExecutionEngine::resolved_account_exposure_excluding`), so seeding the position alone would
/// count the notional TWICE (as a position and as a still-working order), a state no venue
/// produces.
fn book_fill(
    e: &mut ExecutionEngine<RecordingClient>,
    coid: &str,
    venue: &str,
    symbol: &str,
    size: f64,
) {
    let mo = e.registry.get_mut(coid).expect("the order under test was registered");
    mo.status = vike_exec::OrderStatus::Filled;
    mo.filled_qty = size.abs();
    seed_pos(e, venue, symbol, size);
}

/// An opening BUY of `qty` in `symbol` at 100, under its own client-order-id.
fn open_in(coid: &str, symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest { client_order_id: coid.into(), symbol: symbol.into(), ..open_buy(qty, 100.0) }
}

/// **THE HOLE THIS AXIS CLOSES**: two symbols, each order inside the per-symbol ceiling (60 000
/// against 50 000 projected, so `over-max-exposure` can never trip), together over the account's.
/// Without it an operator trading N symbols is protected by that number N times over and never
/// once in aggregate.
///
/// ⚠ The first order's ACCEPTANCE is asserted, not assumed: otherwise this passes against an engine
/// that refuses everything (an account lane armed at zero, a producer folding the order's OWN
/// symbol twice).
#[test]
fn two_symbols_inside_their_own_cap_are_refused_at_the_account_ceiling() {
    let mut e = account_engine(Some(60_000.0), Some(90_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);

    // (1) THE FIRST ORDER IS ADMITTED. 500 @ 100 = 50 000 projected: inside the per-symbol 60 000,
    // and the account holds nothing else, so the aggregate is 50 000 against 90 000.
    assert_eq!(
        verdict_at(&mut e, &open_in("btc-1", "BTCUSDT", 500.0), 1),
        None,
        "the account ceiling must admit the first order — a lane that refuses everything proves \
         nothing about the second"
    );
    assert_eq!(
        e.client.submissions.len(),
        1,
        "…and it reached the venue: {:?}",
        e.client.submissions
    );

    // …and it FILLS (`RecordingClient` never does, so `book_fill` books both halves).
    book_fill(&mut e, "btc-1", "sim", "BTCUSDT", 500.0);

    // (2) THE SECOND ORDER, IN ANOTHER SYMBOL, IS REFUSED BY THE ACCOUNT: its own 50 000 is inside
    // the per-symbol ceiling, while the account projects 50 000 + 50 000 = 100 000 against 90 000.
    let reason = verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 500.0), 2)
        .expect("the account ceiling must refuse the pair");
    assert!(
        reason.starts_with("over-account-exposure"),
        "the ACCOUNT ceiling must refuse under its OWN reason — `over-max-exposure` here would send \
         the operator to re-size the per-symbol number that was not stopping them: {reason}"
    );
    assert_eq!(
        e.client.submissions.len(),
        1,
        "…and nothing new reached the venue: {:?}",
        e.client.submissions
    );

    // (3) THE REFUSAL NAMES THE CEILING AND THE NUMBERS: the order that trips it is ordinary and
    // the exposure is in symbols the operator is not looking at.
    assert!(
        reason.contains("max_account_exposure"),
        "the refusal must name the KEY the operator has to edit: {reason}"
    );
    assert!(
        reason.contains("90000.00") && reason.contains("100000.00"),
        "the refusal must carry the ceiling AND what the account would have projected: {reason}"
    );
}

/// **THE OFF PATH**: the SAME engine, book and order that arm (2) above refuses, with
/// `max_account_exposure` `None`: admitted. So that denial is this ceiling's alone, and a
/// deployment writing no `policy.max_account_exposure` row keeps exactly the gate it had.
#[test]
fn an_absent_account_ceiling_admits_what_an_armed_one_refuses() {
    let mut e = account_engine(Some(60_000.0), None);
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    seed_pos(&mut e, "sim", "BTCUSDT", 500.0);

    assert_eq!(
        verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 500.0), 2),
        None,
        "with the axis absent the account book is not consulted at all"
    );
    assert_eq!(e.client.submissions.len(), 1, "…and the order went to the venue");
}

/// **A LABELLED SECOND ACCOUNT OF THE SAME VENUE IS SUMMED SEPARATELY**: the aggregate keys on the
/// ACCOUNT, not the venue STRING. `vike_mount::make_engine_for_account` builds one engine (one
/// `RiskGate`, one cap, one position book) per `(venue, account)`, addressed by
/// `ExecutionEngine::route_key` (`venue#LABEL` for a labelled account); two accounts are two
/// wallets and neither backs the other.
///
/// ⚠ **What this proves, exactly:** the fold keys on the venue string *within one engine's own
/// map*, so this pair proves that two engines carry two budgets and that the ALT engine's `Some`
/// ceiling is armed while it admits. It cannot see the ACCOUNT-to-engine routing above it (a fill
/// routed to the wrong engine would leave every assertion here green):
/// `crates/vike-core/src/runtime/tests/mount_account/core_minted_orders.rs`'s
/// `each_account_spends_its_own_share_of_the_account_exposure_ceiling` drives that, on one
/// `CoreThread` with both accounts mounted.
///
/// The LOADED default account must refuse the order (or the axis might simply never fire); the
/// empty ALT account must admit it.
#[test]
fn a_labelled_second_account_of_one_venue_has_its_own_budget() {
    let mut default_acct = account_engine(None, Some(90_000.0));
    price_sym(&mut default_acct, "sim", "BTCUSDT", 100.0);
    price_sym(&mut default_acct, "sim", "ETHUSDT", 100.0);
    seed_pos(&mut default_acct, "sim", "BTCUSDT", 500.0); // 50 000 of the 90 000 spent

    let mut alt = account_engine(None, Some(90_000.0));
    alt.route_key = "sim#ALT".to_string(); // what `vike_model::accounts::account_keys::route_key_of` renders
    price_sym(&mut alt, "sim", "BTCUSDT", 100.0);
    price_sym(&mut alt, "sim", "ETHUSDT", 100.0);

    let order = open_in("eth-1", "ETHUSDT", 500.0); // 50 000 projected, in both engines

    // The LOADED account refuses it — 50 000 already held plus 50 000 projected is 100 000.
    let reason =
        verdict_at(&mut default_acct, &order, 1).expect("the default account is over its ceiling");
    assert!(reason.starts_with("over-account-exposure"), "{reason}");

    // …and the LABELLED one, armed at the same number, admits it: a different book.
    assert_eq!(
        verdict_at(&mut alt, &order, 1),
        None,
        "a second account of one venue must carry its OWN budget: one engine's position may never \
         spend another engine's ceiling"
    );
    assert_eq!(alt.client.submissions.len(), 1);
}

/// **A FOREIGN-VENUE ROW IN THIS ACCOUNT'S MAP CONTRIBUTES NOTHING**: it reached the map through a
/// reconcile fold (the shape `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`'s third arm
/// describes), not this account's fills, and summing it would refuse orders against collateral
/// that venue never sees. A million units at another venue; the order is admitted as on a clean
/// book.
#[test]
fn the_account_sum_excludes_a_foreign_venue_row() {
    let mut e = account_engine(None, Some(90_000.0));
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    price_sym(&mut e, "other", "BTCUSDT", 100.0);
    seed_pos(&mut e, "other", "BTCUSDT", 1_000_000.0);

    assert_eq!(
        verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 500.0), 1),
        None,
        "SCOPE: the account is one venue's one wallet. If this now denies, the fold stopped \
         filtering on the engine's own venue and is aggregating a book this account cannot draw on"
    );
    assert_eq!(e.client.submissions.len(), 1);
}

/// **A COVERED REDUCE BYPASSES THE ACCOUNT CEILING**, the whole difference from the per-symbol lane
/// (which has no such bypass and CAN refuse a partial reduce). An account ABOVE its ceiling is
/// ordinary after an operator LOWERS the number or a mark moves, and is dominated by symbols the
/// exiting order does not touch, so without this bypass a panic exit's first flatten leg would be
/// refused by the ceiling it obeys (`docs/ops/kill-switches.md`: a ceiling must never trap you in a
/// position).
#[test]
fn the_account_ceiling_never_refuses_a_covered_reduce() {
    let mut e = account_engine(None, Some(10_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    // 100 000 of ETH held elsewhere in the account, against a 10 000 ceiling — massively over.
    seed_pos(&mut e, "sim", "ETHUSDT", 1_000.0);
    seed_pos(&mut e, "sim", "BTCUSDT", 5.0);

    // An OPENING order is refused, which is what makes the next arm mean something.
    let reason = verdict_at(&mut e, &open_in("btc-open", "BTCUSDT", 1.0), 1)
        .expect("an account this far over its ceiling must refuse an opening order");
    assert!(reason.starts_with("over-account-exposure"), "{reason}");

    // …and the BTC flatten (the leg `vike_core`'s panic button mints) is admitted, though the
    // account stays far over the ceiling.
    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 5.0), 2),
        None,
        "an account over its ceiling must still be closable — a ceiling that refuses the orders \
         which bring it back under is a trap, not a ceiling"
    );
    assert_eq!(
        e.client.submissions.len(),
        1,
        "the exit reached the venue: {:?}",
        e.client.submissions
    );
}

/// **THE SUM IS GROSS, NEVER NET**: a hedge-mode LONG/SHORT pair sums to its two legs' notionals,
/// not zero, or the ceiling would never fire on the position shape carrying the most notional. Net
/// 0, gross 100 000, ceiling 60 000: netting admits, gross refuses.
#[test]
fn the_account_sum_is_gross_and_does_not_net_a_hedged_pair() {
    let mut e = account_engine(None, Some(60_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    e.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "LONG".into()),
        PositionEntry { size: 500.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.positions.insert(
        ("sim".into(), "ETHUSDT".into(), "SHORT".into()),
        PositionEntry { size: -500.0, avg_px: 100.0, ..Default::default() },
    );

    let reason = verdict_at(&mut e, &open_in("btc-1", "BTCUSDT", 1.0), 1)
        .expect("100 000 of gross hedged exposure is over a 60 000 ceiling");
    assert!(
        reason.starts_with("over-account-exposure"),
        "a netting fold would read this book as flat and admit: {reason}"
    );
}

/// **RESTING ORDERS COUNT**, or the ceiling is bypassable by an arbitrary multiple: under a
/// positions-only fold each order sees the same empty account, so quotes resting across symbols
/// blow through the ceiling until they fill (the exposure twin of
/// `ExecutionEngine::live_order_margin` on the buying-power lane). 40 000 + 40 000 fit under
/// 90 000, the third 40 000 does not; both admits are asserted.
#[test]
fn live_un_filled_orders_count_against_the_account_ceiling() {
    let mut e = account_engine(None, Some(90_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);

    assert_eq!(verdict_at(&mut e, &open_in("btc-1", "BTCUSDT", 400.0), 1), None, "first admits");
    assert_eq!(verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 400.0), 2), None, "second admits");
    assert_eq!(e.client.submissions.len(), 2, "both are working at the venue");

    let reason = verdict_at(&mut e, &open_in("eth-2", "ETHUSDT", 400.0), 3)
        .expect("80 000 of resting orders plus 40 000 more is over a 90 000 ceiling");
    assert!(
        reason.starts_with("over-account-exposure"),
        "orders in flight must be spoken for — a positions-only fold admits this one and every \
         order after it: {reason}"
    );
    assert_eq!(e.client.submissions.len(), 2, "…and the third never reached the venue");
}

/// **A RESTING COVERED REDUCE COMMITS NOTHING** (the margin twin's skip, same
/// `vike_model::is_covered_reduce` predicate): charging a working exit would refuse the next order
/// because the account is getting SMALLER. 500 BTC (50 000) held with a resting flatten of all of
/// it, ceiling 90 000: counting the exit makes 100 000 and refuses; skipping it admits 30 000 more.
#[test]
fn a_resting_covered_reduce_adds_no_account_exposure() {
    let mut e = account_engine(None, Some(90_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    price_sym(&mut e, "sim", "ETHUSDT", 100.0);
    seed_pos(&mut e, "sim", "BTCUSDT", 500.0);

    assert_eq!(
        verdict_at(&mut e, &flatten_leg("exit-1", 500.0), 1),
        None,
        "the exit itself is a covered reduce and bypasses the lane"
    );
    assert_eq!(
        verdict_at(&mut e, &open_in("eth-1", "ETHUSDT", 300.0), 2),
        None,
        "a WORKING exit must not be charged as exposure: 50 000 held + 30 000 new is inside \
         90 000, and only a fold that counted the flatten would refuse this"
    );
    assert_eq!(e.client.submissions.len(), 2);
}

/// **AN AMEND IS NOT CHARGED TWICE**: the order being judged is excluded from the resting-order
/// half by client-order-id (the `judging` skip), so a re-price is judged on the account its submit
/// was, not with the order both in the sum AND projected. 500 @ 100 rests (50 000) against 80 000;
/// the amend to 550 projects 55 000, inside, and would be 105 000 if the original were counted too.
#[test]
fn amending_a_resting_order_is_judged_without_counting_that_order_twice() {
    let mut e = account_engine(None, Some(80_000.0));
    price_sym(&mut e, "sim", "BTCUSDT", 100.0);
    let req = open_in("btc-1", "BTCUSDT", 500.0);
    // Drive it to ACCEPTED (the `risk_gate_on_modify.rs` idiom): a SUBMITTED order is not
    // `is_modifiable()`, so the modify below would be a no-op green against anything.
    let mut outbox = Outbox::default();
    e.submit_order(&req, 1, &mut outbox);
    assert_eq!(e.client.submissions.len(), 1, "precondition: the rest itself is admitted");
    let mut bus = vike_exec::EventBus::new();
    bus.publish(
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: "btc-1".into(),
            ts: 1,
        }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: "btc-1".into(),
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        &mut e,
    );

    let mut outbox = Outbox::default();
    e.modify_order("btc-1", Some(550.0), None, 2, &mut outbox);
    let rejected = outbox.0.iter().find_map(|ev| match ev {
        Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
        _ => None,
    });
    assert_eq!(
        rejected, None,
        "an amend must be judged against the account WITHOUT its own resting order in the sum"
    );
    assert_eq!(e.client.modifies.len(), 1, "…and it reached the venue: {:?}", e.client.modifies);

    // …and the lane is armed on this path (so the arm above is not vacuous): an amend to a size the
    // ACCOUNT cannot take is refused, by this ceiling.
    let mut outbox = Outbox::default();
    e.modify_order("btc-1", Some(900.0), None, 3, &mut outbox);
    let rejected = outbox
        .0
        .iter()
        .find_map(|ev| match ev {
            Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .expect("90 000 projected is over the 80 000 ceiling");
    // ⚠ `contains`, not `starts_with`: the MODIFY path wraps the gate's verdict
    // (`ExecutionEngine::modify_order` publishes `format!("risk: {reason}")`).
    assert!(rejected.contains("over-account-exposure"), "{rejected}");
    assert_eq!(e.client.modifies.len(), 1, "…and nothing new reached the venue");
}

/// **THE ARMING FOLD NARROWS AND NEVER WIDENS**: `RiskLimits::narrow_account_exposure`, the one
/// operation `vike_mount::make_engine_for_account` and its paper twin arm this ceiling through, so
/// "it can only ever REFUSE" is a property of the operation, not of nobody else writing the field.
/// `min` when both sides carry a number, in BOTH argument orders (a fold taking any incoming value
/// passes a one-directional test), and never `None` over an existing `Some`
/// (`vike_config::VenueMode::cap` is the precedent).
#[test]
fn the_account_ceiling_fold_narrows_and_never_widens() {
    let narrow = |held: Option<f64>, incoming: Option<f64>| {
        let mut lim = RiskLimits { max_account_exposure: held, ..RiskLimits::new() };
        lim.narrow_account_exposure(incoming);
        lim.max_account_exposure
    };
    assert_eq!(narrow(None, None), None, "no ceiling anywhere ⇒ the axis stays off");
    assert_eq!(
        narrow(None, Some(100.0)),
        Some(100.0),
        "the operator's file arms an unarmed engine"
    );
    assert_eq!(
        narrow(Some(100.0), None),
        Some(100.0),
        "a policy that says nothing may not DISARM a ceiling something else already set"
    );
    assert_eq!(narrow(Some(100.0), Some(250.0)), Some(100.0), "the looser incoming value loses");
    assert_eq!(narrow(Some(250.0), Some(100.0)), Some(100.0), "…and the tighter one wins");
}
