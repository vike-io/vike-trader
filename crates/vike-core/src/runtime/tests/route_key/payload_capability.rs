//! The doubly loaded payload venue (residual 2), and the real wire: venue-tagged fills.

use super::*;

// -------------------------------------------------------------------------------------------
// RESIDUAL 2 — the ORDER payload's venue is doubly loaded, and the CAPABILITY half now asks the
// routed ENGINE instead of the payload.
//
// `OrderRequest::venue` is what `engine_idx_for_route_key` routes on AND what
// `vike_model::preflight_order` selected the caps row by. Those are the two jobs the split
// separated on `ExecutionEngine`; the payload has one field, so a per-ACCOUNT routing key on it
// used to reach `preflight_order` — where a string outside `vike_model::VENUES` does NOT fail
// closed to `VenueCaps::UNSUPPORTED` but returns `Ok(())`, i.e. every check skipped (order kind,
// TIF, margin mode) for one account of one exchange, silently.
//
// `CoreThread::caps_venue` resolves it from the engine the request routed to instead. Inert while
// every engine's route key IS its venue, because the routed engine's `venue` is then the same
// string the request already carried.
// -------------------------------------------------------------------------------------------

/// A decorated per-account routing key for `venue` — the second-account spelling, applied to every
/// roster venue in turn below.
fn sub_account_key(venue: &str) -> String {
    format!("{venue}-sub2")
}

/// EXHAUSTIVE over `vike_model::VENUES`: a ticket addressed by a second account's ROUTING KEY
/// reaches that account's engine, and the capability lookup for it still resolves the REAL venue
/// row. Both halves asserted for every roster venue, with no skip arm — a venue joining the roster
/// is covered the day it joins.
///
/// The last assertion is what makes this about money rather than tidiness: the routing key is NOT a
/// roster id, and `preflight_order`'s unknown-venue affordance answers `Ok(())` for a non-roster
/// string. Reading the routing string here is therefore not a conservative miss — it is the
/// preflight not running at all.
#[test]
fn a_ticket_addresses_the_right_engine_while_caps_resolve_the_real_venue_row() {
    for v in vike_model::VENUES {
        let key = sub_account_key(v);
        let core = core_of(
            engine(v, v, SYMBOL),
            vec![(0.0, engine(v, &key, SYMBOL))],
            CoreConfig::default(),
        );

        let routed = core.engine_idx_for_route_key(RouteKey::declared(&key));
        assert_eq!(routed, Some(1), "{v}: the ticket must address the SECOND account");

        let caps_venue = core.caps_venue(routed, &key);
        assert_eq!(caps_venue, *v, "{v}: the caps row must be the routed ENGINE's canonical venue");
        assert_eq!(vike_model::caps_for(caps_venue), vike_model::caps_for(v), "{v}");
        assert_ne!(vike_model::caps_for(caps_venue), vike_model::VenueCaps::UNSUPPORTED, "{v}");

        // The precondition that makes the whole thing bite — a fact about the roster and the
        // chosen string, independent of anything this test exercises.
        assert!(
            !vike_model::VENUES.contains(&key.as_str()),
            "{v}: a routing key is not a roster id, so keying caps on it SKIPS preflight entirely"
        );
    }
}

/// …and an UNROUTED payload keeps its own string, which preserves the unknown-venue affordance the
/// paper/sim engines behind non-roster ids depend on. Attributing it to the primary engine's row
/// instead would start refusing traffic that flows today — the one way this change could have moved
/// behaviour, pinned so it cannot.
#[test]
fn an_unrouted_payload_keeps_its_own_venue_for_the_caps_lookup() {
    let core = two_accounts_of_one_venue();
    let routed = core.engine_idx_for_route_key(RouteKey::sole_account_of("no-such-venue"));
    assert_eq!(routed, None, "precondition: nothing answers for this venue");
    assert_eq!(core.caps_venue(routed, "no-such-venue"), "no-such-venue");
}

/// END TO END through the real submit path: an order kind the venue's declared row does NOT
/// support, addressed by a second account's routing key, is REFUSED — it never reaches the client.
///
/// The (venue, kind) pair is DERIVED from the live caps table rather than written down, and the
/// derivation `expect`s rather than skipping, so a table that stopped declining anything fails here
/// instead of quietly passing.
#[test]
fn an_unsupported_order_kind_is_still_refused_for_a_second_account() {
    let (venue, kind) = vike_model::VENUES
        .iter()
        .find_map(|v| {
            vike_model::venues::venue_caps::ORDER_KINDS
                .iter()
                .find(|k| !vike_model::caps_for(v).supported_order_kinds.contains(k))
                .map(|k| (*v, *k))
        })
        .expect("some roster venue declines some declared order kind");

    let key = sub_account_key(venue);
    let mut core = core_of(
        engine(venue, venue, SYMBOL),
        vec![(0.0, engine(venue, &key, SYMBOL))],
        CoreConfig::default(),
    );
    let req = vike_model::OrderRequest {
        client_order_id: "c-1".into(),
        venue: key.clone(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: kind.into(),
        price: Some(100.0),
        trigger_price: Some(100.0),
        ..Default::default()
    };
    core.apply_intent(OrderIntent::Submit(Box::new(req)), 0);

    assert_eq!(
        core.eng(1).client.submissions.len(),
        0,
        "{venue}/{kind}: the venue's real row declines this kind, so it must never reach the client"
    );
    assert_eq!(core.eng(0).client.submissions.len(), 0, "{venue}/{kind}: …nor the other account's");
}

/// The CONTROL for the test above, and what makes it non-vacuous: the same order, same engine, same
/// routing key, but a kind the venue DOES support — which must reach the client. A preflight that
/// refused everything (or a routing that reached no engine at all) would pass the test above and
/// fail this one.
#[test]
fn a_supported_order_kind_still_reaches_the_second_accounts_client() {
    let (venue, kind) = vike_model::VENUES
        .iter()
        .find_map(|v| {
            vike_model::caps_for(v)
                .supported_order_kinds
                .iter()
                .find(|k| k.eq_ignore_ascii_case("market") || k.eq_ignore_ascii_case("limit"))
                .map(|k| (*v, *k))
        })
        .expect("some roster venue supports market or limit");

    let key = sub_account_key(venue);
    let mut core = core_of(
        engine(venue, venue, SYMBOL),
        vec![(0.0, engine(venue, &key, SYMBOL))],
        CoreConfig::default(),
    );
    let req = vike_model::OrderRequest {
        client_order_id: "c-2".into(),
        venue: key.clone(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: kind.into(),
        price: Some(100.0),
        ..Default::default()
    };
    core.apply_intent(OrderIntent::Submit(Box::new(req)), 0);

    assert_eq!(
        core.eng(1).client.submissions.len(),
        1,
        "{venue}/{kind}: a supported kind must still reach the addressed account's client"
    );
}

// -------------------------------------------------------------------------------------------
// THE REAL WIRE: a venue-tagged payload carries the CANONICAL venue, never a route key
// -------------------------------------------------------------------------------------------
//
// Every test above drives `route_event` with a payload whose `venue` field IS a route key, which is
// what the reconcile lane can now do (its payload carries both) and what a venue's own WS pump can
// NOT: a Binance fill says `"binance"`, and nothing on that wire says which binance account it
// belongs to. So with two accounts mounted, the venue lookup resolves the FIRST engine forever and
// the second account's fills fold into the first account's book — the exact defect
// `ExecutionEngine::route_key` exists to make impossible, reintroduced one layer up.
//
// `CoreThread::route_event`'s `multi_account` branch closes it with the two handles a payload CAN
// carry, in order of exactness: the client-order-id (resolved through the submit-time `coid_venue`
// map — exact for every order this process placed, and needing nothing on any wire) and then the
// SYMBOL, which answers only while exactly ONE engine of the venue claims it.
//
// ⚠ That last qualifier is a correction. This comment used to say the symbol was exact "because
// `vike_config::symbol_conflicts` refuses to ARM two active accounts of one venue on one symbol" —
// and that refusal is GONE: two accounts on one instrument is an ordinary spread. So the coid moved
// ahead of the symbol, and `engine_idx_for_venue_symbol` answers `None` on ambiguity rather than
// picking the first match.

/// **THE GATE: a wire fill reaches the account that trades its symbol**, even though the payload
/// names only the exchange.
#[test]
fn a_venue_tagged_fill_reaches_the_account_that_trades_its_symbol() {
    let mut core = two_accounts_on_two_symbols();
    assert!(core.multi_account, "the harness must actually be two accounts of one exchange");

    let a = wire_fill(SYMBOL, "w-a", 3.0);
    let b = wire_fill(SYMBOL_B, "w-b", 7.0);
    assert_eq!(core.route_event(&a), Some(0), "the primary account's symbol");
    assert_eq!(core.route_event(&b), Some(1), "the second account's symbol");

    let (ia, ib) =
        (core.route_event(&a).expect("attributed"), core.route_event(&b).expect("attributed"));
    core.publish_to(ia, a);
    core.publish_to(ib, b);

    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 3.0);
    assert_eq!(held_at(&core, 1, CANON, SYMBOL_B), 7.0);
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL_B),
        0.0,
        "the second account's fill must not fold into the first account's book"
    );
}

/// ⚠ **§5.4 CLOSED THIS RESIDUAL, and this test used to pin it the other way.** It was called
/// `a_coid_less_wire_fill_on_a_shared_symbol_lands_on_the_default_account` and asserted that both
/// fills folded into the primary book — the account-routing spec's last-rung misroute, stated as a
/// property.
///
/// A COID-LESS wire fill on a symbol two accounts of one exchange both trade is, by construction, a
/// fill naming no order this process placed (`coid_venue` records the index at every submit) — a
/// FOREIGN fill. `engine_idx_for_venue_symbol`'s own doc already said what to do with one: *"that
/// is reconcile's territory, not this lane's."* So it is UNATTRIBUTED now — `route_event` answers
/// `None` and NEITHER book moves — rather than booking a stranger's size and price into a book
/// that never traded them, which no later reconcile pass can tell apart from a real position.
#[test]
fn a_coid_less_wire_fill_on_a_shared_symbol_is_unattributed() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
        CoreConfig::default(),
    );
    // The SYMBOL lookup declines rather than guessing — two engines claim the pair.
    assert_eq!(core.engine_idx_for_venue_symbol(CANON, SYMBOL), None);

    let a = wire_fill(SYMBOL, "c-a", 3.0);
    let b = wire_fill(SYMBOL, "c-b", 7.0);
    assert_eq!(core.route_event(&a), None, "no key, no coid, an ambiguous symbol: nothing folds");
    assert_eq!(core.route_event(&b), None);

    // …and the harness still SEES the ambiguity, which is the job the negative control was
    // written for: the very same payloads route exactly once either of them names an order of
    // ours (`a_shared_symbol_routes_by_coid_not_by_first_match`), and once only ONE account of
    // the venue is mounted the venue lookup answers as it always has.
    let single = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        Vec::new(),
        CoreConfig::default(),
    );
    assert_eq!(
        single.route_event(&wire_fill(SYMBOL, "c-c", 3.0)),
        Some(0),
        "ONE account of the venue: byte-identical to before §5.4"
    );

    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 0.0, "neither book moved");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 0.0);
}

/// **THE FIX for the case above: a fill naming one of OUR orders routes by its COID**, exactly,
/// with nothing added to any wire.
///
/// `coid_venue` records the routing index at SUBMIT time — when the engine was unambiguous, because
/// the submitting caller knew which account it was trading. So a venue-tagged fill on a symbol two
/// accounts share still finds its own book, and the symbol is never consulted. This is what lets the
/// symbol-collision refusal be deleted without reopening the one-book bug for real orders.
#[test]
fn a_shared_symbol_routes_by_coid_not_by_first_match() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
        CoreConfig::default(),
    );
    // …as `apply_intent_routed` records it for every order lowered onto a non-primary engine.
    core.coid_venue.insert("ours-on-b".to_string(), 1);

    let mut b = wire_fill(SYMBOL, "coid-b", 7.0);
    if let Event::Fill(f) = &mut b {
        f.client_order_id = "ours-on-b".to_string();
    }
    assert_eq!(core.route_event(&b), Some(1), "the SECOND account's own order's fill");

    let idx = core.route_event(&b).expect("attributed");
    core.publish_to(idx, b);
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 7.0);
    assert_eq!(
        held_at(&core, 0, CANON, SYMBOL),
        0.0,
        "…and nothing of it reached the default account's book"
    );

    // A coid this process never submitted is NOT invented into an answer, and §5.4 sharpened what
    // that means: it used to fall through to the venue's DEFAULT account, and it is UNATTRIBUTED
    // now. The fill names an order nobody here placed, on a symbol two of this venue's accounts
    // both trade — the one case `engine_idx_for_venue_symbol`'s doc assigns to reconcile.
    let mut foreign = wire_fill(SYMBOL, "coid-f", 1.0);
    if let Event::Fill(f) = &mut foreign {
        f.client_order_id = "somebody-elses".to_string();
    }
    assert_eq!(core.route_event(&foreign), None);
}

/// **The INERTNESS half: a MULTI-VENUE, single-account process never takes the new branch at all.**
///
/// `multi_account` is what gates it, and this is the shape `vike_mount::build_node` actually builds —
/// one engine per venue, no two sharing an exchange. The flag is false, so every venue-tagged
/// payload takes the identical `return` it always took, and the VENUE decides even where the symbol
/// would point elsewhere.
#[test]
fn one_account_per_venue_never_consults_the_symbol() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B))],
        CoreConfig::default(),
    );
    assert!(!core.multi_account, "two venues, one account each — not a multi-account process");

    // A BINANCE fill carrying the symbol only the BYBIT engine mounts still routes to binance: the
    // venue decides, and the symbol is not consulted.
    assert_eq!(core.route_event(&wire_fill(SYMBOL_B, "i-1", 2.0)), Some(0));

    // …and bybit's own fills still reach bybit.
    let mut b = wire_fill(SYMBOL_B, "i-2", 2.0);
    if let Event::Fill(f) = &mut b {
        f.venue = "bybit".into();
    }
    assert_eq!(core.route_event(&b), Some(1));
}
