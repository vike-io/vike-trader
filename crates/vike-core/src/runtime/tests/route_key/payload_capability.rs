//! The doubly loaded payload venue (residual 2), and the real wire: venue-tagged fills.

use super::*;

// -------------------------------------------------------------------------------------------
// RESIDUAL 2 — the ORDER payload's venue is doubly loaded, and the CAPABILITY half now asks the
// routed ENGINE instead of the payload.
//
// `OrderRequest::venue` is what `engine_idx_for_route_key` routes on AND what
// `vike_venues::preflight_order` selected the caps row by. Those are the two jobs the split
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
        assert_eq!(vike_venues::caps_for(caps_venue), vike_venues::caps_for(v), "{v}");
        assert_ne!(vike_venues::caps_for(caps_venue), vike_venues::VenueCaps::UNSUPPORTED, "{v}");

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
            vike_venues::venue_caps::ORDER_KINDS
                .iter()
                .find(|k| !vike_venues::caps_for(v).supported_order_kinds.contains(k))
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
        order_type: vike_model::OrderType::from_stored(kind),
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
            vike_venues::caps_for(v)
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
        order_type: vike_model::OrderType::from_stored(kind),
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
/// `multi_account` is what gates it, and this is the shape `vike_tradehub_core::assemble_runtime` actually builds —
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

// -------------------------------------------------------------------------------------------
// THE RING'S ROUTE KEY — a FOREIGN fill is routed by the gateway ring it arrived on.
//
// The shape of the incident: two accounts of ONE exchange (live and testnet hyperliquid), the
// symbol only the second one claims. A manual trade in the LIVE ring carries no coid of ours and a
// venue label that is the same for both accounts, so the SYMBOL decided and the live account's
// fill folded into the testnet book. The ring names the account; `route_event_via` asks it
// between the coid and the symbol.
// -------------------------------------------------------------------------------------------

fn ring(key: &str) -> Option<ustr::Ustr> {
    Some(ustr::Ustr::from(key))
}

/// A gateway event as the remote execution client's reader delivers it.
fn from_ring(event: Event, key: &str, seq: u64) -> Ingest {
    Ingest::GatewayEvent(Box::new(GatewayEvent {
        event,
        route_key: ustr::Ustr::from(key),
        cursor: GatewayCursor { gw_epoch: 1, seq, ts_ms: 0 },
    }))
}

/// A foreign fill on the symbol ONLY account B claims, arriving in ACCOUNT A's ring, is A's: the
/// ring outranks the symbol. And the reverse. A ring that names no engine, and no ring at all,
/// keep today's answer (the symbol), so the rung adds an answer and removes none.
#[test]
fn a_foreign_fill_routes_by_its_ring_ahead_of_the_symbol() {
    // Account A claims `SYMBOL`, account B claims `SYMBOL_B` only (the ETH-on-testnet shape).
    let core = two_accounts_on_two_symbols();
    let on_b_symbol = wire_fill(SYMBOL_B, "r-1", 2.0);
    let on_a_symbol = wire_fill(SYMBOL, "r-2", 2.0);

    assert_eq!(
        core.route_event_via(&on_b_symbol, ring(ACCOUNT_A)),
        Some(0),
        "A's ring, B's symbol"
    );
    assert_eq!(
        core.route_event_via(&on_a_symbol, ring(ACCOUNT_B)),
        Some(1),
        "B's ring, A's symbol"
    );
    assert_eq!(core.route_event_via(&on_b_symbol, ring(ACCOUNT_B)), Some(1));
    assert_eq!(core.route_event_via(&on_a_symbol, ring(ACCOUNT_A)), Some(0));

    // Fallbacks: the symbol still answers when the ring names nothing mounted, or is absent.
    assert_eq!(core.route_event_via(&on_b_symbol, ring("binance#never-mounted")), Some(1));
    assert_eq!(core.route_event_via(&on_b_symbol, None), Some(1));
    assert_eq!(core.route_event(&on_b_symbol), Some(1));
}

/// The symbol two accounts BOTH claim is unattributed without a ring (§5.4) and exact with one.
#[test]
fn a_shared_symbol_foreign_fill_is_attributed_when_its_ring_is_known() {
    let core = two_accounts_of_one_venue();
    let foreign = wire_fill(SYMBOL, "r-3", 1.0);
    assert_eq!(core.route_event_via(&foreign, None), None, "no ring: still unattributed");
    assert_eq!(core.route_event_via(&foreign, ring("binance#never-mounted")), None);
    assert_eq!(core.route_event_via(&foreign, ring(ACCOUNT_A)), Some(0));
    assert_eq!(core.route_event_via(&foreign, ring(ACCOUNT_B)), Some(1));
}

/// Our own order's fill keeps routing by its COID, whatever ring carried it.
#[test]
fn the_coid_still_outranks_the_ring() {
    let mut core = two_accounts_on_two_symbols();
    core.coid_venue.insert("ours-on-b".to_string(), 1);
    let mut ours = wire_fill(SYMBOL_B, "r-4", 1.0);
    if let Event::Fill(f) = &mut ours {
        f.client_order_id = "ours-on-b".to_string();
    }
    assert_eq!(core.route_event_via(&ours, ring(ACCOUNT_A)), Some(1));
}

/// A ring never claims a fill of another exchange: a binance fill in the bybit ring is routed as
/// before (by the symbol).
#[test]
fn a_ring_of_another_exchange_does_not_claim_the_fill() {
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![
            (0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL_B)),
            (0.0, engine_with_account_venue("bybit", "bybit", "bybit", SYMBOL_B)),
        ],
        CoreConfig::default(),
    );
    assert_eq!(core.route_event_via(&wire_fill(SYMBOL_B, "r-5", 1.0), ring("bybit")), Some(1));
}

/// END TO END through `dispatch`, the incident itself: the LIVE ring's foreign fill on a symbol only
/// the TESTNET engine claims must NOT fold into the testnet book (and the reverse). It is routed to
/// the ring's own engine, which does not trade the symbol and so drops the fill
/// (`ExecutionEngine::accepts_symbol`): neither book moves, and reconcile re-converges the live one.
/// The CONTROL is the plain-`Event` door (no ring), which still books by symbol, so the assertion
/// above it cannot pass by the fill never folding at all.
#[test]
fn a_gateway_foreign_fill_never_folds_into_the_other_accounts_book() {
    let mut core = two_accounts_on_two_symbols();

    core.dispatch(from_ring(wire_fill(SYMBOL_B, "d-1", 3.0), ACCOUNT_A, 1));
    assert_eq!(
        held_at(&core, 1, CANON, SYMBOL_B),
        0.0,
        "A's ring, B's symbol: not the testnet book"
    );
    assert_eq!(held_at(&core, 0, CANON, SYMBOL_B), 0.0, "…A does not trade it, so nothing folds");

    core.dispatch(from_ring(wire_fill(SYMBOL, "d-2", 5.0), ACCOUNT_B, 1));
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 0.0, "the reverse: B's ring, A's symbol");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 0.0);

    core.dispatch(Ingest::Event(wire_fill(SYMBOL_B, "d-3", 7.0)));
    assert_eq!(held_at(&core, 1, CANON, SYMBOL_B), 7.0, "control, no ring: the symbol, as before");
    assert_eq!(held_at(&core, 0, CANON, SYMBOL_B), 0.0);
}

/// END TO END, the case the ring now ANSWERS: both accounts trade the symbol, so the fill was
/// unattributed (nothing folded); with its ring known it folds into exactly that account's book.
#[test]
fn a_gateway_foreign_fill_on_a_shared_symbol_folds_into_its_rings_book() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(CANON, ACCOUNT_B, CANON, SYMBOL))],
        CoreConfig::default(),
    );

    core.dispatch(from_ring(wire_fill(SYMBOL, "s-1", 3.0), ACCOUNT_A, 1));
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 3.0, "A's ring: A's book");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 0.0);

    core.dispatch(from_ring(wire_fill(SYMBOL, "s-2", 5.0), ACCOUNT_B, 1));
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 5.0, "B's ring: B's book");
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 3.0);

    core.dispatch(Ingest::Event(wire_fill(SYMBOL, "s-3", 9.0)));
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 3.0, "no ring, two claimants: still unattributed");
    assert_eq!(held_at(&core, 1, CANON, SYMBOL), 5.0);
}

// -------------------------------------------------------------------------------------------
// A VENUE EVENT NEVER REACHES AN ENGINE OF ANOTHER EXCHANGE — the venue that runs only LABELLED
// accounts.
//
// `multi_account` means "two engines share one exchange". With binance's DEFAULT account at engine 0
// and bybit's ONLY account the labelled `bybit#DEMO`, nothing is shared, so the flag is false and the
// venue lookup (`sole_account_of("bybit")`, a route key that is exactly the bare venue) finds NO
// engine: bybit has no default account. The last line of `route_event_via` then answered ENGINE 0,
// which is binance's. A bybit fill on a symbol binance also mounts PANICKED the fold at
// `Account::apply_fill`'s venue assert; an UNSTAMPED bybit funding payment was booked into binance's
// balance (the engine has no venue check there) while a STAMPED one was dropped by binance's own key
// check, so the labelled account lost every funding payment, liquidation and balance it was sent;
// and `bybit#DEMO`, whose own orders carry a `coid_venue` row, never saw a fill of its own.
//
// The rule these tests hold: an event reaches an engine of ITS OWN venue, found by the payload's
// stamped route key, then its coid, then (a fill) its ring, then, for an event that names nothing,
// the venue's ONLY live engine (there is only one account it can belong to: no guess). With none
// or several, a stamped key that names no engine, or a ring that names an account nobody runs
// here, it is FOREIGN: logged as UNATTRIBUTED, folded nowhere, never a panic.
// -------------------------------------------------------------------------------------------

const BYBIT: &str = "bybit";
const DEMO_KEY: &str = "bybit#DEMO";
const LIVE_KEY: &str = "bybit#LIVE";

/// binance's DEFAULT account at engine 0 and bybit's ONLY account, labelled, at engine 1, both on
/// [`SYMBOL`] (the same ticker on two exchanges is ordinary; it is what lets engine 0 ACCEPT a bybit
/// fill, which is what used to reach the assert).
fn default_binance_and_labelled_only_bybit() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(BYBIT, DEMO_KEY, BYBIT, SYMBOL))],
        CoreConfig::default(),
    )
}

/// …and the venue with TWO labelled accounts and no default one: binance's default at engine 0,
/// `bybit#DEMO` at 1, `bybit#LIVE` at 2, all on [`SYMBOL`]. Bybit has no single account an
/// unlabelled event could belong to.
fn default_binance_and_two_labelled_bybit() -> CoreThread<RecordingClient> {
    core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![
            (0.0, engine_with_account_venue(BYBIT, DEMO_KEY, BYBIT, SYMBOL)),
            (0.0, engine_with_account_venue(BYBIT, LIVE_KEY, BYBIT, SYMBOL)),
        ],
        CoreConfig::default(),
    )
}

/// A bybit fill as the venue emits it: the canonical venue, no account, the coid the caller names.
fn bybit_fill(symbol: &str, coid: &str, trade_id: &'static str, qty: f64) -> Event {
    let mut ev = wire_fill(symbol, trade_id, qty);
    if let Event::Fill(f) = &mut ev {
        f.venue = BYBIT.into();
        f.client_order_id = coid.to_string();
    }
    ev
}

fn bybit_funding(route_key: Option<&str>, amount: f64) -> Event {
    Event::Funding(vike_model::events::FundingEvent {
        venue: BYBIT.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        funding_rate: 0.0001,
        amount,
        mark_price: None,
        ts: 1,
        route_key: route_key.map(Into::into),
        ccy: None,
    })
}

fn bybit_account_state(route_key: Option<&str>, balance: f64) -> Event {
    Event::AccountState(vike_model::events::AccountState {
        venue: BYBIT.into(),
        balances: vec![("USDT".to_string(), balance)],
        ts: 1,
        route_key: route_key.map(Into::into),
    })
}

fn bybit_liquidation(route_key: Option<&str>, qty: f64, trade_id: &str) -> Event {
    Event::PositionLiquidated(vike_model::events::PositionLiquidated {
        venue: BYBIT.into(),
        symbol: SYMBOL.into(),
        position_side: vike_model::events::PositionSide::Both,
        qty,
        liq_price: 100.0,
        fee: 0.0,
        ts: 1,
        trade_id: trade_id.into(),
        route_key: route_key.map(Into::into),
        fee_ccy: None,
    })
}

fn no_position_anywhere(core: &CoreThread<RecordingClient>) -> bool {
    (0..=core.extra_engines.len()).all(|i| core.eng(i).account.positions.is_empty())
}

fn unattributed_notes(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|l| l.contains("UNATTRIBUTED")).count()
}

/// ⚠ **THE MEASURED PANIC, and its answer.** A ring-less fill of bybit with a coid nobody here placed,
/// on a symbol binance's engine 0 accepts: it used to route to engine 0 and abort the fold at the
/// venue assert. Bybit runs exactly ONE account, `bybit#DEMO` at engine 1, so that account is the
/// only one it can belong to: it books THERE, and binance's engine never sees it.
#[test]
fn a_ring_less_coid_less_fill_of_a_labelled_only_venue_books_into_its_only_account() {
    let mut core = default_binance_and_labelled_only_bybit();
    assert!(!core.multi_account, "no exchange has two engines: the flag is false");
    assert_eq!(
        core.engine_idx_for_route_key(RouteKey::sole_account_of(BYBIT)),
        None,
        "premise: bybit has no DEFAULT account, so the venue lookup finds no engine"
    );

    for (coid, trade_id) in [("somebody-elses", "f-1"), ("", "f-2")] {
        let fill = bybit_fill(SYMBOL, coid, trade_id, 2.0);
        // The fold FIRST: this call is the one that panicked, at `Account::apply_fill`'s venue assert.
        core.dispatch(Ingest::Event(fill.clone()));
        assert_eq!(core.route_event(&fill), Some(1), "bybit's only account, not engine 0");
    }
    assert_eq!(held_at(&core, 1, BYBIT, SYMBOL), 4.0, "both booked into bybit#DEMO");
    assert!(core.eng(0).account.positions.is_empty(), "…and nothing into binance's account");
    assert_eq!(unattributed_notes(&core), 0, "an attributed fill says nothing");
}

/// **The same fill of a venue with TWO labelled accounts is FOREIGN**: nothing says which one it
/// belongs to and none is picked. Routed nowhere, said once per fill, nothing booked anywhere.
#[test]
fn a_ring_less_coid_less_fill_of_a_venue_with_two_labelled_accounts_is_unattributed() {
    let mut core = default_binance_and_two_labelled_bybit();
    assert!(core.multi_account, "bybit holds two engines");
    assert_eq!(core.engine_idx_for_venue_symbol(BYBIT, SYMBOL), None, "the symbol cannot decide");

    let fill = bybit_fill(SYMBOL, "somebody-elses", "f-3", 2.0);
    core.dispatch(Ingest::Event(fill.clone()));
    assert!(no_position_anywhere(&core), "booked into NONE of the three accounts");
    assert_eq!(unattributed_notes(&core), 1, "and said, once");
    assert_eq!(core.route_event(&fill), None, "not engine 0, which is binance's");
}

/// A ring only claims a fill of its OWN exchange. A binance ring on a bybit fill names nothing of
/// bybit's and is ignored: with ONE bybit account the fill still goes to that account, with TWO it is
/// foreign — never to the binance account the ring names. A ring that names ANOTHER bybit account
/// (one this core runs no engine for) says the fill belongs to THAT account, which is not ours to
/// book it into: foreign, even though bybit has exactly one engine here.
#[test]
fn a_ring_of_another_exchange_does_not_claim_a_labelled_only_venues_fill() {
    let mut one = default_binance_and_labelled_only_bybit();
    let fill = bybit_fill(SYMBOL, "", "f-4", 2.0);
    assert_eq!(one.route_event_via(&fill, ring(ACCOUNT_A)), Some(1));
    for other in ["bybit#never-mounted", "bybit#OTHER", BYBIT] {
        assert_eq!(one.route_event_via(&fill, ring(other)), None, "a ring naming {other}");
    }
    one.dispatch(from_ring(fill, ACCOUNT_A, 1));
    assert_eq!(held_at(&one, 1, BYBIT, SYMBOL), 2.0, "the only bybit account's");
    assert!(one.eng(0).account.positions.is_empty());
    one.dispatch(from_ring(bybit_fill(SYMBOL, "", "f-6", 5.0), "bybit#OTHER", 2));
    assert_eq!(held_at(&one, 1, BYBIT, SYMBOL), 2.0, "bybit#OTHER's fill is not bybit#DEMO's");
    assert_eq!(unattributed_notes(&one), 1, "…and it is said");

    let mut two = default_binance_and_two_labelled_bybit();
    let fill = bybit_fill(SYMBOL, "", "f-5", 2.0);
    assert_eq!(two.route_event_via(&fill, ring(ACCOUNT_A)), None);
    assert_eq!(two.route_event_via(&fill, ring("bybit#never-mounted")), None);
    two.dispatch(from_ring(fill, ACCOUNT_A, 1));
    assert!(no_position_anywhere(&two));
    assert_eq!(unattributed_notes(&two), 1);
}

/// **The fill of an order OF OURS follows its coid to its account**, whatever the flag says: the
/// row `coid_venue` holds for a labelled account's order was never read on a core where
/// `multi_account` is false, so the labelled account's own fills were booked nowhere (or, with a
/// symbol engine 0 accepts, panicked the fold).
#[test]
fn a_labelled_only_venues_own_order_fills_into_its_account() {
    let mut core = default_binance_and_labelled_only_bybit();
    core.coid_venue.insert("ours-on-bybit".to_string(), 1);

    let fill = bybit_fill(SYMBOL, "ours-on-bybit", "o-1", 2.0);
    assert_eq!(core.route_event(&fill), Some(1));

    core.dispatch(Ingest::Event(fill));
    assert_eq!(held_at(&core, 1, BYBIT, SYMBOL), 2.0, "booked into bybit#DEMO");
    assert!(core.eng(0).account.positions.is_empty(), "…and not into binance's account");
    assert_eq!(unattributed_notes(&core), 0, "an attributed fill says nothing");
}

/// A fill that arrives in the labelled account's ring is that account's, with no coid of ours.
#[test]
fn a_ring_tagged_fill_of_a_labelled_only_venue_books_into_that_account() {
    let mut core = default_binance_and_labelled_only_bybit();
    let fill = bybit_fill(SYMBOL, "manual-trade", "r-1", 3.0);
    assert_eq!(core.route_event_via(&fill, ring(DEMO_KEY)), Some(1));

    core.dispatch(from_ring(fill, DEMO_KEY, 1));
    assert_eq!(held_at(&core, 1, BYBIT, SYMBOL), 3.0);
    assert!(core.eng(0).account.positions.is_empty());
    assert_eq!(unattributed_notes(&core), 0);
}

/// The three payloads without a coid carry the account's route key, stamped by its lane: that key is
/// the whole answer for a labelled-only venue. UNSTAMPED, they go to the venue's ONLY account when
/// it has exactly one (an unstamped funding payment used to be booked into binance's balance, with no
/// error anywhere) and are foreign when it has several. A key that names no account of the venue is
/// foreign even when the venue has one: the payload said it belongs to another account.
#[test]
fn the_stamped_payloads_of_a_labelled_only_venue_reach_their_account_and_unstamped_ones_the_only_one()
 {
    let mut one = default_binance_and_labelled_only_bybit();

    for stamped in [
        bybit_funding(Some(DEMO_KEY), -5.0),
        bybit_account_state(Some(DEMO_KEY), 900.0),
        bybit_liquidation(Some(DEMO_KEY), 1.0, "liq-1"),
    ] {
        assert_eq!(one.route_event(&stamped), Some(1), "stamped: bybit#DEMO's");
    }
    for unstamped in [
        bybit_funding(None, -5.0),
        bybit_account_state(None, 900.0),
        bybit_liquidation(None, 1.0, "liq-2"),
    ] {
        assert_eq!(one.route_event(&unstamped), Some(1), "unstamped: the only bybit account's");
    }
    for elsewhere in [
        bybit_funding(Some("bybit#never-mounted"), -5.0),
        bybit_account_state(Some(ACCOUNT_A), 900.0),
        bybit_liquidation(Some("bybit#never-mounted"), 1.0, "liq-3"),
    ] {
        assert_eq!(one.route_event(&elsewhere), None, "a key naming another account is not ours");
    }

    one.dispatch(Ingest::Event(bybit_funding(None, -5.0)));
    assert_eq!(one.eng(1).account.funding_paid, -5.0, "bybit#DEMO paid it");
    assert_eq!(one.eng(0).account.funding_paid, 0.0, "binance paid nothing bybit owes");

    let mut two = default_binance_and_two_labelled_bybit();
    for unstamped in [
        bybit_funding(None, -5.0),
        bybit_account_state(None, 900.0),
        bybit_liquidation(None, 1.0, "liq-4"),
    ] {
        assert_eq!(two.route_event(&unstamped), None, "two accounts: no account to give it to");
    }
    two.dispatch(Ingest::Event(bybit_funding(None, -5.0)));
    assert!((0..=2).all(|i| two.eng(i).account.funding_paid == 0.0), "nobody paid it");
    two.dispatch(Ingest::Event(bybit_funding(Some(LIVE_KEY), -7.0)));
    assert_eq!(two.eng(2).account.funding_paid, -7.0, "the stamped one is bybit#LIVE's");
    assert_eq!(two.eng(1).account.funding_paid, 0.0);
}

/// **A STOPPED account is no account of the exchange** (decision 0169): the only LIVE bybit engine
/// takes an unlabelled event although a retired one sits beside it, and a venue whose only engine is
/// retired has no account left to give it to.
#[test]
fn a_retired_engine_does_not_count_as_an_account_of_the_venue() {
    let mut core = default_binance_and_two_labelled_bybit();
    core.retired_engines.push(1); // bybit#DEMO was stopped, bybit#LIVE runs
    let fill = bybit_fill(SYMBOL, "", "t-1", 1.0);
    assert_eq!(core.route_event(&fill), Some(2), "the one live bybit account");
    core.dispatch(Ingest::Event(fill));
    assert_eq!(held_at(&core, 2, BYBIT, SYMBOL), 1.0);
    assert!(core.eng(1).account.positions.is_empty() && core.eng(0).account.positions.is_empty());

    let mut none_left = default_binance_and_labelled_only_bybit();
    none_left.retired_engines.push(1);
    assert_eq!(none_left.route_event(&bybit_fill(SYMBOL, "", "t-2", 1.0)), None);
}

/// A venue no engine of this process runs is foreign whole: its fill reaches no engine.
#[test]
fn a_fill_of_a_venue_this_process_does_not_run_is_unattributed() {
    let mut core = default_binance_and_labelled_only_bybit();
    let mut fill = bybit_fill(SYMBOL, "", "k-1", 1.0);
    if let Event::Fill(f) = &mut fill {
        f.venue = "kraken".into();
    }
    assert_eq!(core.route_event(&fill), None);
    core.dispatch(Ingest::Event(fill));
    assert!(no_position_anywhere(&core));
}

/// The same hole on a node that IS multi-account elsewhere: binance holds two engines, bybit one
/// labelled engine on a symbol of its own. The symbol rung still answers for a bybit fill on bybit's
/// symbol (it always did); a bybit fill on binance's symbol falls through every rung and used to land
/// on engine 0. It reaches bybit's only account now, which does not trade the symbol and drops it:
/// nothing is booked, and nothing panics.
#[test]
fn a_labelled_only_venue_on_a_multi_account_node_is_never_answered_by_engine_zero() {
    const BYBIT_SYMBOL: &str = "SOLUSDT";
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![
            (0.0, engine_with_account_venue(CANON, ALT, CANON, SYMBOL)),
            (0.0, engine_with_account_venue(BYBIT, DEMO_KEY, BYBIT, BYBIT_SYMBOL)),
        ],
        CoreConfig::default(),
    );
    assert!(core.multi_account, "binance holds two engines");
    core.coid_venue.insert("ours-on-bybit".to_string(), 2);

    // Unchanged answers: the symbol rung, the coid rung, the ring rung.
    assert_eq!(core.route_event(&bybit_fill(BYBIT_SYMBOL, "", "m-1", 1.0)), Some(2));
    assert_eq!(core.route_event(&bybit_fill(SYMBOL, "ours-on-bybit", "m-2", 1.0)), Some(2));
    assert_eq!(core.route_event_via(&bybit_fill(SYMBOL, "", "m-3", 1.0), ring(DEMO_KEY)), Some(2));

    // The hole: no rung names bybit's engine, and engine 0 is binance's.
    let foreign = bybit_fill(SYMBOL, "somebody-elses", "m-4", 1.0);
    core.dispatch(Ingest::Event(foreign.clone()));
    assert!(no_position_anywhere(&core), "bybit's only account does not trade it: dropped there");
    assert_eq!(core.route_event(&foreign), Some(2), "…and it was bybit's, not engine 0's");
    assert_eq!(
        core.route_event_via(&foreign, ring("bybit#OTHER")),
        None,
        "a ring naming a bybit account nobody runs here is not bybit#DEMO's either"
    );
}

/// **CONTROLS: what routed correctly before routes identically.** One default account per venue (the
/// gated shape): the venue decides, a coid row, a ring and the symbol are never consulted, and every
/// fill books exactly where it always did.
#[test]
fn one_default_account_per_venue_books_every_fill_where_it_always_did() {
    let mut core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![(0.0, engine_with_account_venue(BYBIT, BYBIT, BYBIT, SYMBOL))],
        CoreConfig::default(),
    );
    assert!(!core.multi_account);
    core.coid_venue.insert("ours-on-bybit".to_string(), 1);

    let binance = wire_fill(SYMBOL, "c-1", 1.0);
    let bybit_plain = bybit_fill(SYMBOL, "", "c-2", 2.0);
    let bybit_ours = bybit_fill(SYMBOL, "ours-on-bybit", "c-3", 3.0);
    assert_eq!(core.route_event(&binance), Some(0));
    assert_eq!(core.route_event(&bybit_plain), Some(1));
    assert_eq!(core.route_event(&bybit_ours), Some(1));
    assert_eq!(
        core.route_event_via(&bybit_plain, ring("binance")),
        Some(1),
        "the ring is not read"
    );

    core.dispatch(Ingest::Event(binance));
    core.dispatch(Ingest::Event(bybit_plain));
    core.dispatch(from_ring(bybit_ours, BYBIT, 1));
    assert_eq!(held_at(&core, 0, CANON, SYMBOL), 1.0);
    assert_eq!(held_at(&core, 1, BYBIT, SYMBOL), 5.0);
    assert_eq!(unattributed_notes(&core), 0);
}

// -------------------------------------------------------------------------------------------
// THE COID AND RING RUNGS OF THE COLD RESOLVER — each pinned by the one case the "only live engine"
// rung cannot answer, so deleting the rung turns a test red.
//
// With ONE live bybit engine the sole-engine rung names it whatever the coid row or ring say, so the
// two rungs above it only differ when the venue's ONLY engine is STOPPED (decision 0169): the
// sole-engine rung then counts no account and answers `None`. That is the owner-principle case: a
// stopped account's last fill of an order OF OURS (it carries a `coid_venue` row, or arrives in the
// account's own ring) follows its order's id to that engine. `ExecutionEngine::on_event` folds it
// there like any other (a stopped engine stays in the list "to fold its last events",
// `Self::engine_idx_for_route_key`), so what the code does is: ROUTED to the stopped engine and
// BOOKED into its account; nothing reaches another engine, and nothing is dropped as foreign.
// -------------------------------------------------------------------------------------------

/// The stopped sole engine and a late fill of ITS OWN order, which has a row: it books into that
/// engine's account. A fill that names no order of ours (the control) stays foreign.
#[test]
fn a_late_fill_of_a_stopped_accounts_own_order_follows_its_coid_to_that_engine() {
    let mut core = default_binance_and_labelled_only_bybit();
    core.retired_engines.push(1); // bybit#DEMO was stopped, and it is bybit's only engine
    core.coid_venue.insert("ours-on-bybit".to_string(), 1);

    let fill = bybit_fill(SYMBOL, "ours-on-bybit", "s-1", 2.0);
    assert_eq!(core.route_event(&fill), Some(1), "its own order's engine, stopped or not");
    core.dispatch(Ingest::Event(fill));
    assert_eq!(
        held_at(&core, 1, BYBIT, SYMBOL),
        2.0,
        "the stopped engine still folds its last fill"
    );
    assert!(core.eng(0).account.positions.is_empty(), "…and binance's engine never sees it");
    assert_eq!(unattributed_notes(&core), 0, "…and it is not dropped as foreign");

    // CONTROL: the same stopped venue, a fill naming no order of ours: no live account is left.
    assert_eq!(core.route_event(&bybit_fill(SYMBOL, "", "s-2", 1.0)), None);
}

/// The same through the gateway door: the stopped account's OWN ring names its engine (the route-key
/// lookup answers a stopped engine when no live one carries the key), with no coid of ours on the fill.
#[test]
fn a_late_fill_in_a_stopped_accounts_own_ring_follows_the_ring_to_that_engine() {
    let mut core = default_binance_and_labelled_only_bybit();
    core.retired_engines.push(1);

    let fill = bybit_fill(SYMBOL, "", "s-3", 3.0);
    assert_eq!(core.route_event_via(&fill, ring(DEMO_KEY)), Some(1));
    core.dispatch(from_ring(fill, DEMO_KEY, 1));
    assert_eq!(held_at(&core, 1, BYBIT, SYMBOL), 3.0);
    assert!(core.eng(0).account.positions.is_empty());
    assert_eq!(unattributed_notes(&core), 0);

    // CONTROL: no ring, no row: nothing names the stopped account.
    assert_eq!(core.route_event(&bybit_fill(SYMBOL, "", "s-4", 1.0)), None);
}

/// A row that names an engine of ANOTHER exchange (no core path writes one; a venue echoing a coid it
/// was never given is the only way to meet it) never books there. The cold resolver ignores it: with
/// one live bybit account the fill goes to that account (the sole-engine rung). The multi-account
/// branch trusts its row as it always has (a compare fewer per fill), so there the fill IS routed to
/// the binance engine, and `Account::apply_fill` refuses it: nothing is booked, nothing panics.
#[test]
fn a_coid_row_naming_another_exchanges_engine_never_books_there() {
    let mut one = default_binance_and_labelled_only_bybit();
    one.coid_venue.insert("row-to-binance".to_string(), 0);
    assert_eq!(one.route_event(&bybit_fill(SYMBOL, "row-to-binance", "w-1", 1.0)), Some(1));

    let mut two = default_binance_and_two_labelled_bybit();
    two.coid_venue.insert("row-to-binance".to_string(), 0);
    let fill = bybit_fill(SYMBOL, "row-to-binance", "w-2", 1.0);
    assert_eq!(two.route_event(&fill), Some(0), "the multi-account branch trusts its row");
    two.dispatch(Ingest::Event(fill));
    assert!(no_position_anywhere(&two), "…and the account refuses a fill of another venue");
}

// -------------------------------------------------------------------------------------------
// THE UNATTRIBUTED LOG IS RATE LIMITED — it now covers funding, liquidation, account state and
// venues no engine runs, and it allocates on the fold thread.
//
// The first `UNATTRIBUTED_FULL_LINES` events of each KIND log in full (a WARN and a recent-events
// note); after that the arm only counts, allocating nothing, and one summary WARN per
// `UNATTRIBUTED_WINDOW_MS` of core clock carries the counts. Nothing is serialized.
// -------------------------------------------------------------------------------------------

use tracing_test::traced_test;

/// What only a FULL UNATTRIBUTED line says, and what only the summary says.
const FULL_LINE: &str = "it names no order this process placed";
const SUMMARY_LINE: &str = "SUPPRESSED";

fn lines_with(lines: &[&str], needle: &str) -> usize {
    lines.iter().filter(|l| l.contains(needle)).count()
}

/// [`default_binance_and_two_labelled_bybit`] on a clock the test steps.
fn two_labelled_bybit_on_a_step_clock()
-> (CoreThread<RecordingClient>, std::sync::Arc<std::sync::atomic::AtomicI64>) {
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let read = std::sync::Arc::clone(&clock);
    let core = core_of(
        engine_with_account_venue(CANON, ACCOUNT_A, CANON, SYMBOL),
        vec![
            (0.0, engine_with_account_venue(BYBIT, DEMO_KEY, BYBIT, SYMBOL)),
            (0.0, engine_with_account_venue(BYBIT, LIVE_KEY, BYBIT, SYMBOL)),
        ],
        CoreConfig {
            clock: Box::new(move || read.load(std::sync::atomic::Ordering::Relaxed)),
            ..CoreConfig::default()
        },
    );
    (core, clock)
}

fn foreign_fill_at(
    core: &mut CoreThread<RecordingClient>,
    clock: &std::sync::atomic::AtomicI64,
    ms: i64,
    n: usize,
) {
    clock.store(ms, std::sync::atomic::Ordering::Relaxed);
    for _ in 0..n {
        core.dispatch(Ingest::Event(bybit_fill(SYMBOL, "", "u-1", 1.0)));
    }
}

/// A flood of one kind logs five full lines and says nothing more inside the window; the allowance
/// is PER KIND, so the first funding payments after a fill flood still get their lines.
#[traced_test]
#[test]
fn the_first_unattributed_events_of_each_kind_log_in_full_and_the_flood_does_not() {
    let (mut core, clock) = two_labelled_bybit_on_a_step_clock();
    foreign_fill_at(&mut core, &clock, 1_000, 20);
    assert_eq!(unattributed_notes(&core), 5, "five full notes for twenty fills");

    for _ in 0..20 {
        core.dispatch(Ingest::Event(bybit_funding(None, -1.0)));
    }
    assert_eq!(unattributed_notes(&core), 10, "a second kind gets its own five");
    logs_assert(|lines: &[&str]| match lines_with(lines, FULL_LINE) {
        10 => Ok(()),
        n => Err(format!("wanted 10 full UNATTRIBUTED lines for 40 events, got {n}")),
    });
}

/// After the allowance the arm counts, and ONE summary per window carries the counts; a quiet window
/// emits nothing, and the next window starts clean.
#[traced_test]
#[test]
fn the_unattributed_flood_is_summarised_once_per_window() {
    let (mut core, clock) = two_labelled_bybit_on_a_step_clock();
    foreign_fill_at(&mut core, &clock, 0, 8); // 5 full, 3 suppressed: the window opens at 0
    foreign_fill_at(&mut core, &clock, 9_999, 1); // inside the window: nothing said
    foreign_fill_at(&mut core, &clock, 10_000, 1); // the window closes: ONE summary, 5 events
    foreign_fill_at(&mut core, &clock, 10_001, 3); // the next window: nothing yet
    foreign_fill_at(&mut core, &clock, 20_000, 1); // ONE more summary, 4 events

    assert_eq!(unattributed_notes(&core), 5, "the summaries are log lines, not notes");
    logs_assert(|lines: &[&str]| {
        let (full, summaries) = (lines_with(lines, FULL_LINE), lines_with(lines, SUMMARY_LINE));
        if (full, summaries) != (5, 2) {
            return Err(format!("wanted 5 full lines and 2 summaries, got {full} and {summaries}"));
        }
        match (lines_with(lines, "fills=5"), lines_with(lines, "fills=4")) {
            (1, 1) => Ok(()),
            counts => Err(format!("the summaries must carry 5 then 4 fills, got {counts:?}")),
        }
    });
}
