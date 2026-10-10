//! Payloads with no coid to route by: `AccountState`, then `Funding` and `PositionLiquidated`.

use super::*;

// -------------------------------------------------------------------------------------------
// THE PAYLOAD WITH NO SYMBOL: `Event::AccountState`
// -------------------------------------------------------------------------------------------
//
// The section above closes the two-account routing question with the SYMBOL, and that answer is
// exact for every venue-tagged payload that HAS one. `Event::AccountState` does not: it is an
// account-wide balance snapshot, so it fell through to the venue lookup and a second account's
// balances folded into the FIRST account's book — while that same account's fills and positions
// routed correctly, which is what made it easy to miss.
//
// The cure is a route key on the payload, stamped by the MOUNT
// (`vike_mount::account_event_sender` -> `vike_exec::EventSender::routed`) rather than by a bridge,
// because a venue adapter holds one credential set and cannot name an account. These tests drive
// `route_event` with exactly what that lane produces.

/// A balance snapshot as a VENUE emits one: canonical venue, no route key. What every bridge in
/// this tree pushes, and what a DEFAULT-account lane leaves untouched.
fn wire_account_state(balance: f64) -> Event {
    Event::AccountState(vike_model::events::AccountState {
        venue: CANON.into(),
        balances: vec![("USDT".to_string(), balance)],
        ts: 1,
        route_key: None,
    })
}

/// …and the same snapshot after a LABELLED account's lane has stamped it.
fn stamped_account_state(route_key: &str, balance: f64) -> Event {
    let mut ev = wire_account_state(balance);
    if let Event::AccountState(a) = &mut ev {
        a.route_key = Some(route_key.into());
    }
    ev
}

fn balance_at(core: &CoreThread<RecordingClient>, idx: usize) -> f64 {
    core.eng(idx).account.balance
}

/// **THE GATE: a second account's balance snapshot reaches ITS OWN engine.**
///
/// The payload names no symbol, so the disambiguator the fill tests rest on cannot fire — the key
/// it carries is the whole of the answer.
#[test]
fn a_stamped_account_state_reaches_its_own_account() {
    let mut core = two_accounts_on_two_symbols();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let a = wire_account_state(1_000.0);
    let b = stamped_account_state(ACCOUNT_B, 7_000.0);
    assert_eq!(
        core.route_event(&a),
        Some(0),
        "the default account's snapshot: unstamped, venue-routed"
    );
    assert_eq!(
        core.route_event(&b),
        Some(1),
        "the second account's snapshot: routed by its stamped key"
    );

    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(balance_at(&core, 0), 1_000.0);
    assert_eq!(balance_at(&core, 1), 7_000.0);
}

/// THE NEGATIVE CONTROL — strip the stamp and the bug is back, in this exact harness.
///
/// It is what makes the gate above a verdict rather than a coincidence: with both snapshots
/// unstamped the venue lookup answers `0` for both, the second account's engine never sees its own
/// balance, and the first account's book ends up holding the SECOND account's money.
#[test]
fn removing_the_stamp_reproduces_the_wrong_book_bug() {
    let mut core = two_accounts_on_two_symbols();
    let a = wire_account_state(1_000.0);
    let b = wire_account_state(7_000.0);
    assert_eq!(core.route_event(&a), Some(0));
    assert_eq!(
        core.route_event(&b),
        Some(0),
        "no symbol, no key: the second account is unreachable"
    );

    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(balance_at(&core, 0), 7_000.0, "the second account's money in the first book");
    assert_eq!(balance_at(&core, 1), 0.0, "…and its own book never saw a balance at all");
}

/// **The INERTNESS half: a single-account process never sees a stamped payload at all**, because
/// nothing stamps a key equal to its own venue (`vike_exec::EventSender::routed`). Driving the
/// unstamped payload every such box produces takes the identical branch it always took.
#[test]
fn a_single_account_process_routes_account_state_exactly_as_before() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
        CoreConfig::default(),
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");

    assert_eq!(core.route_event(&wire_account_state(500.0)), Some(0));
    let mut bybit = wire_account_state(500.0);
    if let Event::AccountState(a) = &mut bybit {
        a.venue = "bybit".into();
    }
    assert_eq!(core.route_event(&bybit), Some(1));
}

/// A key naming NO mounted engine falls through to the venue lookup rather than being dropped —
/// the same unknown-key affordance `engine_idx_for_route_key` has everywhere else. The ENGINE then
/// refuses it (`ExecutionEngine::on_event`'s route-key filter), so the stray snapshot changes no
/// book; this pins the ROUTER half of that pair.
#[test]
fn an_unmountable_route_key_falls_through_to_the_venue_lookup() {
    let core = two_accounts_on_two_symbols();
    assert_eq!(core.route_event(&stamped_account_state("binance#never-mounted", 5.0)), Some(0));
}

// -------------------------------------------------------------------------------------------
// THE OTHER TWO PAYLOADS WITH NO CLIENT-ORDER-ID: `Funding` and `PositionLiquidated`
// -------------------------------------------------------------------------------------------
//
// The `AccountState` section above was written on the claim that it is "the one payload with
// NEITHER a coid nor a symbol to fall back on". The first half of that is right and the second half
// is a trap: `Event::Funding` and `Event::PositionLiquidated` carry a SYMBOL and no coid, and the
// symbol stopped being an account key the moment two accounts of one venue were allowed onto one
// instrument — which is the whole point of the mount `account` field, so it is not a corner case
// but the supported configuration.
//
// So on a SHARED symbol both of them fell through `engine_idx_for_venue_symbol`'s ambiguity `None`
// to the venue lookup, and landed on the venue's DEFAULT engine whichever account they belonged to:
// a labelled account's funding debit on the default account's `balance`, and a labelled account's
// liquidation CLOSING the default account's position at the venue's liq price while the account
// that was actually liquidated went on reporting the position open. Both books wrong, silently, and
// nothing about a strategy's own order flow is involved — the coid lane above cannot help.
//
// They are stamped now, by the same mount lane that stamps `AccountState`
// (`vike_exec::EventSender::routed`). These tests drive the SHARED-SYMBOL harness deliberately: on
// two symbols the old code would have routed correctly by accident.

/// Two accounts of one exchange on ONE symbol — the ordinary spread the deleted collision rule
/// refused, and the configuration in which the symbol answers nothing.
fn two_accounts_on_one_symbol() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
        CoreConfig::default(),
    )
}

/// A funding payment as a VENUE emits one: canonical venue, a symbol, no coid, no route key.
fn wire_funding(amount: f64) -> Event {
    Event::Funding(vike_model::events::FundingEvent {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        ts: 1,
        route_key: None,
    })
}

fn stamped_funding(route_key: &str, amount: f64) -> Event {
    let mut ev = wire_funding(amount);
    if let Event::Funding(f) = &mut ev {
        f.route_key = Some(route_key.into());
    }
    ev
}

/// A liquidation as a VENUE emits one. `trade_id` is the per-engine dedup key and is deliberately
/// NOT a routing handle — see `PositionLiquidated::route_key`.
fn wire_liquidation(qty: f64, trade_id: &str) -> Event {
    Event::PositionLiquidated(vike_model::events::PositionLiquidated {
        venue: CANON.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        qty,
        liq_price: 100.0,
        fee: 0.0,
        ts: 1,
        trade_id: trade_id.into(),
        route_key: None,
    })
}

fn stamped_liquidation(route_key: &str, qty: f64, trade_id: &str) -> Event {
    let mut ev = wire_liquidation(qty, trade_id);
    if let Event::PositionLiquidated(p) = &mut ev {
        p.route_key = Some(route_key.into());
    }
    ev
}

fn funding_paid_at(core: &CoreThread<RecordingClient>, idx: usize) -> f64 {
    core.eng(idx).account.funding_paid
}

/// **THE GATE: a second account's funding payment reaches ITS OWN engine, on a shared symbol.**
#[test]
fn a_stamped_funding_payment_reaches_the_account_that_paid_it() {
    let mut core = two_accounts_on_one_symbol();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");
    // The premise: the symbol answers NOTHING here, so the stamp is the whole of the answer.
    assert_eq!(core.engine_idx_for_venue_symbol(CANON, SYMBOL), None);

    let a = wire_funding(-1.0);
    let b = stamped_funding(ACCOUNT_B, -8.0);
    assert_eq!(
        core.route_event(&a),
        Some(0),
        "the default account's payment: unstamped, venue-routed"
    );
    assert_eq!(
        core.route_event(&b),
        Some(1),
        "the second account's payment: routed by its stamped key"
    );

    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(funding_paid_at(&core, 0), -1.0);
    assert_eq!(funding_paid_at(&core, 1), -8.0, "the labelled account's own book");
}

/// THE NEGATIVE CONTROL for it — strip the stamp and both payments land in one book, which is
/// exactly what shipped before this: the default account absorbs a debit it never incurred.
#[test]
fn an_unstamped_funding_payment_on_a_shared_symbol_lands_on_the_default_account() {
    let mut core = two_accounts_on_one_symbol();
    let a = wire_funding(-1.0);
    let b = wire_funding(-8.0);
    assert_eq!(core.route_event(&a), Some(0));
    assert_eq!(
        core.route_event(&b),
        Some(0),
        "no key, no coid, an ambiguous symbol: the default account"
    );

    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    core.publish_to(ia, a);
    core.publish_to(ib, b);
    assert_eq!(funding_paid_at(&core, 0), -9.0, "both debits on one balance");
    assert_eq!(funding_paid_at(&core, 1), 0.0, "…and the account that paid one saw nothing");
}

/// **The liquidation twin, and the one that moves a POSITION.** The labelled account's engine holds
/// the position; the liquidation must close ITS book, not the default account's.
#[test]
fn a_stamped_liquidation_closes_the_account_that_was_liquidated() {
    let mut core = two_accounts_on_one_symbol();
    // Both accounts are long the same instrument — the spread's own shape, and the state in which a
    // misrouted liquidation is indistinguishable from a real one.
    // ⚠ Both seeds name an order of OURS. Since §5.4 a COID-LESS fill on a symbol two accounts
    // share is UNATTRIBUTED rather than folded into the default book, so seeding account A with a
    // bare wire fill would seed nothing at all — see
    // `a_coid_less_wire_fill_on_a_shared_symbol_is_unattributed`.
    let mut a_fill = wire_fill(SYMBOL, "seed-a", 5.0);
    if let Event::Fill(f) = &mut a_fill {
        f.client_order_id = "ours-on-a".to_string();
    }
    core.coid_venue.insert("ours-on-a".to_string(), 0);
    let mut b_fill = wire_fill(SYMBOL, "seed-b-liq", 5.0);
    if let Event::Fill(f) = &mut b_fill {
        f.client_order_id = "ours-on-b".to_string();
    }
    core.coid_venue.insert("ours-on-b".to_string(), 1);
    let (ia, ib) = (
        core.route_event(&a_fill).expect("attributed"),
        core.route_event(&b_fill).expect("attributed"),
    );
    assert_eq!((ia, ib), (0, 1), "the seed fills must land one per book");
    core.publish_to(ia, a_fill);
    core.publish_to(ib, b_fill);
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 5.0);
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 5.0);

    let liq = stamped_liquidation(ACCOUNT_B, 5.0, "liq-b");
    assert_eq!(core.route_event(&liq), Some(1));
    let idx = core.route_event(&liq).expect("attributed");
    core.publish_to(idx, liq);

    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 0.0, "the liquidated account is flat");
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL),
        5.0,
        "…and the account that was NOT liquidated still holds its position"
    );
}

/// THE NEGATIVE CONTROL — unstamped, the same liquidation flattens the WRONG book and leaves the
/// liquidated account reporting a position the venue has already closed. This is the shipped
/// behaviour this section exists to remove.
#[test]
fn an_unstamped_liquidation_on_a_shared_symbol_flattens_the_default_account() {
    let mut core = two_accounts_on_one_symbol();
    // ⚠ Both seeds name an order of OURS. Since §5.4 a COID-LESS fill on a symbol two accounts
    // share is UNATTRIBUTED rather than folded into the default book, so seeding account A with a
    // bare wire fill would seed nothing at all — see
    // `a_coid_less_wire_fill_on_a_shared_symbol_is_unattributed`.
    let mut a_fill = wire_fill(SYMBOL, "seed-a", 5.0);
    if let Event::Fill(f) = &mut a_fill {
        f.client_order_id = "ours-on-a".to_string();
    }
    core.coid_venue.insert("ours-on-a".to_string(), 0);
    let mut b_fill = wire_fill(SYMBOL, "seed-b-u", 5.0);
    if let Event::Fill(f) = &mut b_fill {
        f.client_order_id = "ours-on-b".to_string();
    }
    core.coid_venue.insert("ours-on-b".to_string(), 1);
    let (ia, ib) = (
        core.route_event(&a_fill).expect("attributed"),
        core.route_event(&b_fill).expect("attributed"),
    );
    core.publish_to(ia, a_fill);
    core.publish_to(ib, b_fill);

    let liq = wire_liquidation(5.0, "u-liq");
    assert_eq!(core.route_event(&liq), Some(0), "unstamped: the venue's default engine");
    let idx = core.route_event(&liq).expect("attributed");
    core.publish_to(idx, liq);
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 0.0, "the wrong book was flattened");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 5.0, "…and the liquidated one still looks open");
}

/// **The ENGINE-side backstop**, the twin of the `AccountState` filter: even handed the payload
/// directly, an engine refuses one stamped for a different account. This is what makes the router
/// the only thing that has to be right, rather than the only thing that CAN be right — and it is
/// the half `engine_idx_for_route_key`'s unknown-key affordance needs, since a key naming no
/// mounted engine falls through to the venue lookup and would otherwise be folded there.
#[test]
fn an_engine_refuses_a_funding_or_liquidation_stamped_for_another_account() {
    let mut core = two_accounts_on_one_symbol();
    // Deliberately published to engine 0 — the answer the OLD router gave.
    core.publish_to(0, stamped_funding(ACCOUNT_B, -8.0));
    assert_eq!(funding_paid_at(&core, 0), 0.0, "the default account refused a labelled debit");

    let seed = wire_fill(SYMBOL, "bs-seed", 5.0);
    core.publish_to(0, seed);
    core.publish_to(0, stamped_liquidation(ACCOUNT_B, 5.0, "bs-liq"));
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL),
        5.0,
        "the default account refused a labelled liquidation and kept its position"
    );
    // …and an UNSTAMPED payload still folds exactly as it always did, which is what keeps every
    // single-account box byte-identical.
    core.publish_to(0, wire_funding(-1.0));
    assert_eq!(funding_paid_at(&core, 0), -1.0);
}

/// **The INERTNESS half**: nothing stamps a key equal to its own venue, so a single-account process
/// drives the identical unstamped payloads it always drove and takes the identical branch.
#[test]
fn a_single_account_process_routes_funding_and_liquidation_exactly_as_before() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
        CoreConfig::default(),
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");

    assert_eq!(core.route_event(&wire_funding(-1.0)), Some(0));
    assert_eq!(core.route_event(&wire_liquidation(1.0, "s-1")), Some(0));
    let mut bybit = wire_funding(-1.0);
    if let Event::Funding(f) = &mut bybit {
        f.venue = "bybit".into();
        f.symbol = SYMBOL_B.into();
    }
    assert_eq!(core.route_event(&bybit), Some(1));
}
