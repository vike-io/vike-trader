use super::*;
use crate::wire::OrderRequestWire;
use std::assert_matches;
use vike_model::TradingState;

fn venues(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// **The dangerous direction: a NEW client against an OLD backend.** The backend advertises
/// nothing, so it will take any venue and apply it to its PRIMARY book while answering `Ack`.
/// The client must therefore refuse locally — and the refusal must name both what was asked
/// for and what the backend publishes, because "pick a different venue" is useless advice
/// without the list.
#[test]
fn an_old_backend_refuses_a_venue_it_does_not_publish() {
    let BackendVenueRoutingVerdict::Refuse(why) =
        venue_routing_verdict("okx", &venues(&["binance", "polymarket"]), false)
    else {
        panic!("a backend that does not check the address must not be handed an unseen venue");
    };
    assert!(why.contains("okx"), "{why}");
    assert!(why.contains("binance") && why.contains("polymarket"), "{why}");
}

/// ...but a venue that backend PUBLISHES an engine for is positive evidence, and is sent. This
/// is the only evidence available against an old backend, and it is enough: the core routes a
/// venue it runs correctly — the historical `unwrap_or(0)` fallback fires only when nothing
/// matches.
#[test]
fn an_old_backend_still_takes_a_venue_it_publishes() {
    assert_eq!(
        venue_routing_verdict("binance", &venues(&["binance", "polymarket"]), false),
        BackendVenueRoutingVerdict::Send
    );
}

/// A backend that ADVERTISES the capability is sent the command even for a venue this client
/// cannot see. This is the one rule that leans toward sending, deliberately: `node_venues`
/// comes from a pushed snapshot that lags a runtime `MountStrategy`, and the worst case here is
/// a clean `Response::Error` from a backend that knows its own engines — never a misroute.
#[test]
fn a_backend_that_checks_the_address_is_trusted_to_answer() {
    assert_eq!(
        venue_routing_verdict("okx", &venues(&["binance"]), true),
        BackendVenueRoutingVerdict::Send,
        "the backend will refuse it by name; this client must not pre-empt a fresher answer"
    );
}

/// ⚠ The most conservative case: an observer that has connected but received no frame knows
/// NOTHING about the backend. Against one that does not advertise the capability that is a
/// refusal, because "I know nothing" must never read as "anything goes" on a path that signs
/// orders — and the message says the backend has published none rather than printing an empty
/// list.
#[test]
fn knowing_nothing_about_an_old_backend_refuses_rather_than_guesses() {
    let BackendVenueRoutingVerdict::Refuse(why) = venue_routing_verdict("binance", &[], false)
    else {
        panic!("no evidence + no capability must refuse");
    };
    assert!(why.contains("published none"), "{why}");
}

/// A blank venue is refused on EVERY backend, advertised or not: an empty string is not a
/// venue, and a node's routing falls to engine 0 for it on every build. (The observer's
/// pre-first-frame snapshot placeholder carries exactly this — empty strings.)
#[test]
fn a_blank_venue_is_refused_even_by_a_backend_that_checks_addresses() {
    for venue in ["", "   "] {
        assert_matches!(
            venue_routing_verdict(venue, &venues(&["binance"]), true),
            BackendVenueRoutingVerdict::Refuse(_),
            "a blank venue names no book: {venue:?}"
        );
    }
}

/// The comparison is EXACT, matching the node's own engine selection (a string equality on the
/// route key). A case slip against an old backend is refused rather than silently routed to
/// its primary.
#[test]
fn the_venue_comparison_is_exact() {
    assert_matches!(
        venue_routing_verdict("BINANCE", &venues(&["binance"]), false),
        BackendVenueRoutingVerdict::Refuse(_)
    );
}

fn wire_submit(venue: &str) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: "c-1".into(),
        venue: venue.into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

/// The composed call, in the two directions that matter: a venue the backend publishes goes out
/// even against a backend that checks nothing; one it does not publish is held back, and the
/// refusal NAMES the venue it held back (it is the line the status strip paints).
#[test]
fn the_shell_call_sends_a_published_venue_and_holds_an_unpublished_one() {
    let published = venues(&["polymarket", "binance"]);
    assert_eq!(may_send_to_backend(&wire_submit("binance"), &published, false), Ok(()));
    let held = may_send_to_backend(&wire_submit("okx"), &published, false);
    assert!(
        held.as_ref().is_err_and(|why| why.contains("okx")),
        "an order for a venue this backend does not publish lands on its PRIMARY book: {held:?}"
    );
    assert_eq!(
        may_send_to_backend(&wire_submit("okx"), &published, true),
        Ok(()),
        "...unless the backend checks addresses, in which case it answers by name"
    );
}

/// An ADDRESS-LESS command is always sendable, whatever the backend said about itself — the
/// UNSCOPED panic button above all. A kill switch with a prerequisite is not one.
#[test]
fn an_address_less_command_is_always_sendable() {
    for cmd in [
        CommandWire::MarketExit { venue: None, account: None },
        CommandWire::Cancel("c-1".into()),
        CommandWire::SetTradingState(TradingState::Halted),
    ] {
        assert_eq!(
            may_send_to_backend(&cmd, &[], false),
            Ok(()),
            "{cmd:?} names no venue, so there is nothing to misroute"
        );
    }
}
