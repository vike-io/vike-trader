//! [`account_refusal`] in isolation — the pure verdict for the fault
//! [`venue_refusal`]'s evidence cannot see. The symptom this closes is the one the spec opens
//! with: `vike-cli trade order submit binance/NOSUCH BTCUSDT buy 1` answered `accepted`,
//! because the node Acked the frame and the core refused the order out of band, on the far side
//! of the single-writer lane where no wire response can be reached.
//!
//! ⚠ **Two planes, one sentence.** The mount plane (`WireCommand::MountStrategy`) was a
//! declared residual when this suite was written and is checked now; its tests are below,
//! beside the submit ones, because the only thing that differs between the two verdicts is the
//! sentence's SUBJECT — and a suite that proved one plane and inferred the other would leave
//! exactly the arm this gate had already been wrong about once.
//!
//! ⚠ The roster these tests are written against is the ROUTE-KEY one
//! ([`crate::publish::PublisherHandle::engine_route_keys`]), not the venue one — a two-account
//! node publishes `binance` twice under `venues[].venue` and `["binance", "binance#ALT"]` under
//! `venues[].route_key`, and only the second can tell the two books apart.

use super::refusal::{
    account_refusal, awaiting_balance_refusal, not_running_refusal, route_key_is_of_venue,
    venue_refusal,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vike_tradehub_client::wire::{WireCommand, WireTradingState};

/// A node running TWO accounts of one exchange plus a single-account venue — the configuration
/// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s *What would reopen
/// this* named in advance, and the only one in which this gate has anything to say.
fn roster() -> Vec<String> {
    vec!["binance".to_string(), "binance#ALT".to_string(), "polymarket".to_string()]
}

fn submit(venue: &str, account: Option<&str>) -> WireCommand {
    WireCommand::Submit(vike_tradehub_client::wire::WireOrderRequest {
        client_order_id: "c-1".into(),
        venue: venue.into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

/// The SECOND plane this gate answers for. Its account field has the same three wire states a
/// submit's does, and `WireCommand::MountStrategy`'s own doc rates a miss a rung WORSE: a
/// misrouted order is one order on the wrong book, a misrouted mount is every order that
/// strategy will ever place.
fn mount(venue: &str, account: Option<&str>) -> WireCommand {
    WireCommand::MountStrategy {
        venue: venue.into(),
        account: account.map(str::to_string),
        symbol: "SYM".into(),
        interval: "1m".into(),
        controller_id: None,
        name: Some("buy_hold".into()),
        rhai: None,
        params: serde_json::json!({}),
    }
}

/// A submit naming an account this node does not run is REFUSED ON THE WIRE, naming the
/// accounts that do exist. Before this it was Acked and refused out of band as a recent-events
/// note, so the client printed `accepted` over an order that never existed.
#[test]
fn a_submit_naming_an_unheld_account_is_refused_naming_the_roster() {
    let msg = account_refusal(&submit("binance", Some("NOSUCH")), &roster()).expect("refused");
    assert!(msg.contains("NOSUCH"), "the refusal quotes what was asked for: {msg}");
    assert!(msg.contains("binance"), "…and the venue it was asked for on: {msg}");
    assert!(msg.contains("ALT"), "…and the labelled account that DOES exist: {msg}");
    assert!(
        msg.contains("DEFAULT"),
        "…and the unlabelled one, under the one spelling a client can send back: {msg}"
    );
    // ⚠ The VENUE gate is silent on exactly this frame, which is why a second gate exists at
    // all rather than a wider first one: `binance` IS a venue this node runs.
    assert_eq!(
        venue_refusal(&submit("binance", Some("NOSUCH")), &["binance".to_string()]),
        None,
        "the venue question has a different, correct answer here — this is a second question"
    );
}

/// A submit naming an account this node DOES run is untouched — both spellings, because the
/// unlabelled account's key is the BARE VENUE ID and a gate that suffixed the label text
/// unconditionally would refuse the one book it names.
#[test]
fn a_submit_naming_a_held_account_is_not_refused() {
    assert_eq!(account_refusal(&submit("binance", Some("ALT")), &roster()), None);
    assert_eq!(account_refusal(&submit("binance", Some("DEFAULT")), &roster()), None);
    assert_eq!(
        account_refusal(&submit("polymarket", Some("DEFAULT")), &roster()),
        None,
        "a single-account venue's route key IS its venue id, so `DEFAULT` addresses it"
    );
}

/// ⚠ INHERITED FROM [`venue_refusal`]: an EMPTY roster means the core has not published yet and
/// is treated as UNKNOWN. Refusing here is a DEADLOCK, not a conservative default — a core
/// publishes when its state goes dirty and a refused command never reaches the core, so a
/// feed-less daemon that refused on an empty roster would refuse every command for ever.
///
/// The inheritance is structural as well as asserted: `engine_venues` and `engine_route_keys`
/// both project one `portfolio.venues`, so a caller can never hold one populated roster and one
/// empty one — which is what keeps this gate from being armed while the venue gate is blind.
/// ⚠ Every verb but a BRACKET — see `a_bracket_is_refused_while_the_roster_is_empty`.
#[test]
fn an_empty_roster_refuses_nothing() {
    assert_eq!(account_refusal(&submit("binance", Some("NOSUCH")), &[]), None);
    assert_eq!(account_refusal(&submit("anything-at-all", Some("ALT")), &[]), None);
    assert_eq!(account_refusal(&submit("binance", Some("DEFAULT")), &[]), None);
}

/// ⚠ A submit naming NO account is unchanged on every roster shape, including a multi-account
/// one. Its case belongs to `vike_core`'s `CoreThread::ambiguous_accounts` — a sender that
/// named NOTHING on a venue with several accounts — and this gate must not take it over: the
/// refusals read differently (*which book did you mean* against *you named a book I do not
/// have*), and folding them would tell an operator to name an account while this gate had no
/// account to check.
#[test]
fn a_submit_naming_no_account_is_never_refused_by_this_gate() {
    for roster in [roster(), vec!["binance".to_string()], Vec::new()] {
        assert_eq!(account_refusal(&submit("binance", None), &roster), None, "{roster:?}");
        assert_eq!(account_refusal(&submit("okx", None), &roster), None, "{roster:?}");
    }
}

/// ⚠ **THE RULING ON `DEFAULT`, and it is the same one the core made.** `DEFAULT` is a NAME,
/// never an omission, and it resolves through `route_key_of` — which renders the unlabelled
/// account as the BARE VENUE ID. So it matches a roster holding that id and is refused by a
/// roster that does not, which is the sharp edge: a node whose binance accounts are ALL
/// labelled runs no `binance` key, so `DEFAULT` names a book it does not have.
///
/// `vike_core`'s `naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue` and
/// `a_payload_naming_default_on_an_unmounted_venue_refuses_while_its_account_less_twin_does_not`
/// pin the two halves one plane down, and this gate composes the key with the SAME function
/// `CoreThread::route_for_payload_account` composes it with, so the two planes cannot disagree
/// about a spelling.
#[test]
fn naming_default_resolves_the_bare_venue_key_exactly_as_the_core_resolves_it() {
    // Held: the two-account node publishes the bare id beside the labelled one.
    assert_eq!(account_refusal(&submit("binance", Some("DEFAULT")), &roster()), None);
    // NOT held: every account of this venue is labelled, so there is no bare key to match.
    let all_labelled = vec!["binance#A".to_string(), "binance#B".to_string()];
    let msg = account_refusal(&submit("binance", Some("DEFAULT")), &all_labelled).expect("refused");
    assert!(msg.contains("`DEFAULT`"), "names the account that was asked for: {msg}");
    assert!(msg.contains("it runs: A, B"), "…and the two that exist: {msg}");
    // ⚠ …and `binance#DEFAULT` is NOT the key that was looked for. A gate that suffixed the
    // label text unconditionally would MATCH this roster and REFUSE the held one above — both
    // wrong, and both wrong silently. The key is unmintable (`route_key_of` never produces it,
    // because `AccountLabel::parse` refuses the reserved spelling), so the account beside it is
    // what the refusal names.
    let decoy = vec!["binance#DEFAULT".to_string(), "binance#ALT".to_string()];
    let msg = account_refusal(&submit("binance", Some("DEFAULT")), &decoy).expect("refused");
    assert!(
        msg.contains("it runs: ALT"),
        "the unmintable key names no account at all, so only `ALT` is held: {msg}"
    );
}

/// The venue is the OTHER half of the key, and a submit on a venue this roster knows nothing
/// about is [`venue_refusal`]'s refusal, not this one's. Answering here too would put two
/// sentences on one fault — `crate::config::no_engine_refusal` makes that argument for the
/// sentence it owns.
#[test]
fn a_venue_with_no_engine_at_all_is_the_venue_gates_refusal_not_this_ones() {
    assert_eq!(account_refusal(&submit("okx", Some("ALT")), &roster()), None);
    assert!(
        venue_refusal(&submit("okx", Some("ALT")), &["binance".to_string()]).is_some(),
        "…and the venue gate, which runs FIRST in `accept_command`, does answer it"
    );
}

/// ⚠ EXACT match, never case-folded or trimmed — [`venue_refusal`]'s rule one field along.
/// A label this gate cannot PARSE is deliberately not its refusal either: `parse_wire_account`
/// owns the charset, the length bound and the case-sensitivity that makes `alt` a different
/// string from `ALT`, and `lower_command` refuses on it INSIDE `accept_command`, before the
/// Ack. So `alt` is still refused at the edge — by the parser, in one sentence rather than two.
#[test]
fn the_match_is_exact_and_an_unparseable_label_belongs_to_the_parser() {
    let msg = account_refusal(&submit("binance", Some("ALTX")), &roster()).expect("refused");
    assert!(msg.contains("ALTX"), "a well-formed label that names no book is ours: {msg}");
    for slip in ["alt", "Alt", " ALT", "ALT "] {
        assert_eq!(
            account_refusal(&submit("binance", Some(slip)), &roster()),
            None,
            "`{slip}` is not readable as `ALT` here, and its refusal is `lower_command`'s"
        );
        assert!(
            vike_model::accounts::account_keys::parse_wire_account(slip).is_err(),
            "…which is only true because the PARSER refuses it: `{slip}`"
        );
    }
}

/// The printed roster is SORTED and DEDUPLICATED, like the venue gate's, and it names ONLY the
/// accounts of the venue that was asked about — a node running forty other books must not
/// answer a binance typo with all of them.
#[test]
fn the_named_accounts_are_scoped_to_the_venue_sorted_and_deduplicated() {
    let wide = vec![
        "binance#ALT".to_string(),
        "binance".to_string(),
        "binance#ALT".to_string(),
        "okx#TREASURY".to_string(),
        "polymarket".to_string(),
    ];
    let msg = account_refusal(&submit("binance", Some("NOSUCH")), &wide).expect("refused");
    assert!(msg.contains("ALT, DEFAULT"), "sorted, and deduped: {msg}");
    assert!(!msg.contains("TREASURY"), "another venue's accounts are not this answer: {msg}");
    assert!(!msg.contains("polymarket"), "…nor another venue at all: {msg}");
}

/// **THE MOUNT PLANE — the same defect one rung worse, and the reason this gate grew a
/// subject.** A `MountStrategy` naming an account this node does not run was Acked at the edge
/// and refused by `vike_core`'s `CoreThread::mount_strategy_runtime` as a recent-events note,
/// which is precisely the out-of-band shape this gate exists to delete for orders — except
/// that what is silently mis-Acked here is not one order but a strategy's whole future order
/// flow, sized against a book nobody named.
///
/// The refusal must NAME the account and the venue, exactly as the order plane's does: an
/// operator reading it learns which book they asked for and that this node does not hold it.
#[test]
fn a_mount_naming_an_unheld_account_is_refused_naming_the_roster() {
    let msg = account_refusal(&mount("binance", Some("NOSUCH")), &roster()).expect("refused");
    assert!(msg.contains("NOSUCH"), "the refusal quotes what was asked for: {msg}");
    assert!(msg.contains("binance"), "…and the venue it was asked for on: {msg}");
    assert!(msg.contains("ALT"), "…and the labelled account that DOES exist: {msg}");
    assert!(
        msg.contains("DEFAULT"),
        "…and the unlabelled one, under the one spelling a client can send back: {msg}"
    );
    // ⚠ The SUBJECT is what makes this sentence about a MOUNT rather than about an order. It is
    // `crate::config::no_engine_refusal`'s parameter one gate along, and it is the whole of the
    // wording change — the rest of the sentence is shared, because two spellings of one refusal
    // teach an operator to read two different faults into one situation.
    assert!(msg.contains("The mount was REFUSED"), "the subject names the MOUNT: {msg}");
    assert!(
        !msg.contains("The command was REFUSED"),
        "…and not the order plane's subject, which would make a mount read as one order: {msg}"
    );
}

/// The mount plane inherits every one of this gate's silences, and none of them is re-decided
/// for it: a HELD account (both spellings), an account-less mount, and an EMPTY roster are all
/// byte-identical to before the arm existed. The last is the deadlock rule — a core publishes
/// when it goes dirty and a refused command never reaches the core.
#[test]
fn a_mount_is_refused_only_for_the_one_fault_this_gate_owns() {
    assert_eq!(account_refusal(&mount("binance", Some("ALT")), &roster()), None);
    assert_eq!(
        account_refusal(&mount("binance", Some("DEFAULT")), &roster()),
        None,
        "`DEFAULT` NAMES the unlabelled book, whose route key is the bare venue id"
    );
    assert_eq!(
        account_refusal(&mount("binance", None), &roster()),
        None,
        "an account-less mount is unchanged: absence is not this gate's subject"
    );
    assert_eq!(
        account_refusal(&mount("binance", Some("NOSUCH")), &[]),
        None,
        "an EMPTY roster is UNKNOWN, never `no engines` — refusing would deadlock the node"
    );
    assert_eq!(
        account_refusal(&mount("okx", Some("ALT")), &roster()),
        None,
        "a venue with no engine at all is `venue_refusal`'s sentence, not this one's"
    );
}

/// The three risk-REDUCING verbs, each naming `account` on `venue`, beside the SUBJECT each
/// one's refusal must carry.
fn reducers(venue: &str, account: Option<&str>) -> Vec<(&'static str, WireCommand)> {
    let account = account.map(str::to_string);
    vec![
        (
            "The mass-cancel",
            WireCommand::MassCancel {
                venue: Some(venue.into()),
                symbol: None,
                account: account.clone(),
            },
        ),
        (
            "The flatten",
            WireCommand::Flatten {
                venue: venue.into(),
                symbol: "SYM".into(),
                account: account.clone(),
            },
        ),
        ("The market exit", WireCommand::MarketExit { venue: Some(venue.into()), account }),
    ]
}

/// ⚠ **THE REDUCING PLANE — owner ruling "B", 2026-09-26.** A reducing verb naming an account
/// this node does not run is REFUSED ON THE WIRE, naming the accounts it does run, in the
/// sentence the submit and mount planes already share with only the SUBJECT changed.
///
/// This gate used to return `None` for all three, and the reason it gave was a ruling rather
/// than a gap: the core DROPPED the account and fanned the verb over every account of the
/// venue, so refusing a named one here would have refused a verb whose capability said it was
/// supported. The core honours the account now (`vike_core`'s reducing arms resolve it through
/// `route_for_payload_account`), and with that the named-but-unheld case became exactly the
/// misroute this gate exists for: Acked, then refused out of band by a core whose refusal no
/// wire response can reach.
#[test]
fn a_reducing_verb_naming_an_unheld_account_is_refused_naming_the_roster() {
    for (subject, cmd) in reducers("binance", Some("NOSUCH")) {
        let msg = account_refusal(&cmd, &roster()).unwrap_or_else(|| panic!("{subject}"));
        assert!(msg.contains("NOSUCH"), "{subject}: quotes what was asked for: {msg}");
        assert!(msg.contains("binance"), "{subject}: …and the venue: {msg}");
        assert!(msg.contains("ALT, DEFAULT"), "{subject}: …and the held roster: {msg}");
        assert!(
            msg.contains(&format!("{subject} was REFUSED")),
            "{subject}: the SUBJECT names the verb, not an order: {msg}"
        );
        assert!(!msg.contains("The command was REFUSED"), "{subject}: {msg}");
    }
}

/// …and every one of the gate's existing silences is INHERITED for the reducing plane rather
/// than re-decided: a HELD account (both spellings), an EMPTY roster (the deadlock rule), and a
/// venue with no engine at all (`venue_refusal`'s sentence).
#[test]
fn a_reducing_verb_is_refused_only_for_the_fault_this_gate_owns() {
    for account in ["ALT", "DEFAULT"] {
        for (subject, cmd) in reducers("binance", Some(account)) {
            assert_eq!(account_refusal(&cmd, &roster()), None, "{subject} naming {account}");
        }
    }
    for (subject, cmd) in reducers("binance", Some("NOSUCH")) {
        assert_eq!(account_refusal(&cmd, &[]), None, "{subject}: an EMPTY roster is UNKNOWN");
    }
    for (subject, cmd) in reducers("okx", Some("ALT")) {
        assert_eq!(account_refusal(&cmd, &roster()), None, "{subject}: the VENUE's question");
    }
}

/// ⚠ **§4.5's LAW, still gated: a reducing verb naming NO account is never this gate's.** It
/// fans out to every account of its venue — the sender meant all of them — and the UNSCOPED
/// panic button names neither a venue nor an account and must reach every engine. Regressing
/// either is worse than the misroute the arm above closes: a way OUT of a position that now
/// needs an argument to work.
#[test]
fn an_account_less_reducing_verb_and_the_panic_button_are_never_refused() {
    for roster in [roster(), Vec::new()] {
        for (subject, cmd) in reducers("binance", None) {
            assert_eq!(account_refusal(&cmd, &roster), None, "{subject}: {roster:?}");
        }
        for cmd in [
            WireCommand::MarketExit { venue: None, account: None },
            WireCommand::MassCancel { venue: None, symbol: None, account: None },
        ] {
            assert_eq!(account_refusal(&cmd, &roster), None, "{cmd:?} is the panic button");
        }
    }
}

/// ⚠ **An account named with NO venue is refused BY NAME, and the roster does not enter into
/// it.** An account label names one book OF a venue, so on its own it names nothing — and the
/// arm such a frame would otherwise reach is the GLOBAL one, every engine on the node. Unlike
/// the unheld-account refusal this is not a question the roster answers, so an empty roster
/// (UNKNOWN) refuses it too: there is no publish that could make it resolvable.
#[test]
fn an_account_named_with_no_venue_is_refused_whatever_the_roster() {
    for roster in [roster(), Vec::new()] {
        for (subject, cmd) in [
            (
                "The mass-cancel",
                WireCommand::MassCancel { venue: None, symbol: None, account: Some("ALT".into()) },
            ),
            (
                "The market exit",
                WireCommand::MarketExit { venue: None, account: Some("ALT".into()) },
            ),
        ] {
            let msg = account_refusal(&cmd, &roster)
                .unwrap_or_else(|| panic!("{subject} with no venue must refuse: {roster:?}"));
            assert!(msg.contains("`ALT`"), "{subject}: names the account: {msg}");
            assert!(msg.contains("no venue"), "{subject}: names what is missing: {msg}");
            assert!(msg.contains(&format!("{subject} was REFUSED")), "{subject}: {msg}");
        }
    }
}

/// The verbs that name NO account at all are not this gate's — order-scoped by coid, or the
/// account-wide kill switch.
#[test]
fn a_verb_with_no_account_field_is_not_this_gates() {
    let roster = roster();
    for cmd in [
        WireCommand::Cancel("c-1".into()),
        WireCommand::Modify { client_order_id: "c-1".into(), new_qty: None, new_price: None },
        WireCommand::SetTradingState(WireTradingState::Halted),
    ] {
        assert_eq!(account_refusal(&cmd, &roster), None, "{cmd:?} is not this gate's");
    }
}

/// A bracket on `venue` that names NO account — the shape the tests below start from ([`bracket_on`]
/// names one).
fn bracket(venue: &str) -> WireCommand {
    WireCommand::Bracket(vike_tradehub_client::wire::WireBracketSpec {
        venue: venue.into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(1.0),
        stop_loss: 0.9,
        take_profit: 1.1,
        account: None,
    })
}

/// ⚠ **A bracket goes ONLY to a venue whose one account is the default one.** On the two-account
/// venue the core would refuse it as ambiguous, but AFTER this node's `Ack`, so the client would
/// print success over a bracket that never existed. This gate says so before the `Ack`, and it names
/// the accounts.
#[test]
fn a_bracket_goes_only_to_a_venue_whose_one_account_is_the_default() {
    assert_eq!(account_refusal(&bracket("polymarket"), &roster()), None, "one default account");
    let msg = account_refusal(&bracket("binance"), &roster()).expect("two accounts: refused");
    assert!(msg.contains("binance"), "the refusal names the venue: {msg}");
    assert!(msg.contains("ALT") && msg.contains("DEFAULT"), "...and both accounts: {msg}");
    assert!(msg.contains("bracket"), "...and says what was refused: {msg}");
    // ⚠ ONE ORDER across the gate: the rendered labels sorted as strings, exactly as the
    // submit/mount/reduce sentence above prints them (`ALT, DEFAULT`), never `AccountLabel`'s own
    // `Ord`, which would put `DEFAULT` first.
    assert!(msg.contains("ALT, DEFAULT"), "...in the gate's one order: {msg}");
}

/// ⚠ **A venue whose ONLY account is labelled refuses too**, and this is the one shape that is a
/// silent misroute without this gate. The core looks for a bare `binance` route key, finds none, and
/// falls back to engine 0, which may be another exchange's book.
#[test]
fn a_bracket_on_a_venue_whose_only_account_is_labelled_is_refused() {
    let roster = vec!["binance#ALT".to_string(), "polymarket".to_string()];
    let msg = account_refusal(&bracket("binance"), &roster).expect("refused");
    assert!(msg.contains("ALT"), "{msg}");
}

/// A venue whose accounts are ALL labelled refuses whatever their number, naming every one: with
/// two of them the core would refuse it as ambiguous after the `Ack`, and with neither labelled
/// account the default there is no book this frame could mean.
#[test]
fn a_bracket_on_a_venue_with_two_labelled_accounts_is_refused_naming_both() {
    let roster = vec!["binance#A".to_string(), "binance#B".to_string(), "polymarket".to_string()];
    let msg = account_refusal(&bracket("binance"), &roster).expect("refused");
    assert!(msg.contains("A, B"), "{msg}");
    assert!(!msg.contains("DEFAULT"), "no unlabelled account is held here: {msg}");
}

/// ⚠ The bracket gate decides which keys are a venue's by SHAPE (`route_key_is_of_venue`), a third
/// spelling of the route-key separator. It is pinned to `route_key_of` — the renderer every engine
/// key comes from — over the whole roster: each venue's default and labelled keys are its own, and
/// no other roster venue's are.
#[test]
fn the_route_key_predicate_agrees_with_route_key_of_for_every_roster_venue() {
    use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    for &owner in vike_model::VENUES {
        for key in [route_key_of(owner, &AccountLabel::Default), route_key_of(owner, &alt)] {
            for &venue in vike_model::VENUES {
                assert_eq!(
                    route_key_is_of_venue(venue, &key),
                    venue == owner,
                    "is `{key}` a key of `{venue}`?"
                );
            }
        }
    }
}

/// ⚠ **The venue's route keys are counted RAW**, because the core counts ENGINES
/// (`vike_core`'s `CoreThread::engines_of_venue`) and never parses a label to do it. A gate that
/// deduplicated, or dropped a key whose label it could not parse, could answer "exactly one" where
/// the core answers "several" — and the core's answer arrives after the `Ack`. Neither shape is
/// mintable today (`route_key_of` renders only parsed labels, and the publisher documents one key
/// per engine), which is why it is pinned here rather than left to hold by accident.
#[test]
fn a_brackets_venue_is_counted_raw_as_the_core_counts_engines() {
    for roster in [
        vec!["binance".to_string(), "binance".to_string()],
        vec!["binance".to_string(), "binance#not a label".to_string()],
    ] {
        assert!(
            account_refusal(&bracket("binance"), &roster).is_some(),
            "two keys of one venue are two engines, whatever they spell: {roster:?}"
        );
    }
    // ...and a key of ANOTHER venue that merely starts with this one's name is not this venue's.
    let roster = vec!["binance".to_string(), "binanceus".to_string()];
    assert_eq!(account_refusal(&bracket("binance"), &roster), None, "{roster:?}");
}

/// ⚠ **The one verb an EMPTY roster refuses.** Every other verb treats empty as UNKNOWN
/// (`an_empty_roster_refuses_nothing` above), because refusing them all would stop a feed-less core
/// from ever publishing. A refused bracket leaves every other command flowing, and "this venue's one
/// account is the default" is a claim an empty roster cannot back.
#[test]
fn a_bracket_is_refused_while_the_roster_is_empty() {
    let msg = account_refusal(&bracket("binance"), &[]).expect("refused, unlike every other verb");
    assert!(msg.contains("not published"), "{msg}");
    assert!(msg.contains("first snapshot"), "...and says what ends the window: {msg}");
}

/// A venue this node runs NO engine for is the venue gate's sentence, not this one's: two sentences
/// on one fault teach an operator to read two faults into it.
#[test]
fn a_bracket_on_a_venue_this_node_does_not_run_is_the_venue_gates() {
    assert_eq!(account_refusal(&bracket("okx"), &roster()), None);
    assert!(
        venue_refusal(&bracket("okx"), &["binance".to_string(), "polymarket".to_string()])
            .is_some()
    );
}

/// [`bracket`] naming `account` (the wire spelling: a label, or `DEFAULT`).
fn bracket_on(venue: &str, account: &str) -> WireCommand {
    match bracket(venue) {
        WireCommand::Bracket(mut b) => {
            b.account = Some(account.to_string());
            WireCommand::Bracket(b)
        }
        other => other,
    }
}

/// ⚠ **A bracket that NAMES an account is judged like a submit naming one**: the key must be on the
/// roster, `DEFAULT` is the bare venue id, and a miss is the plane's one sentence naming the books the
/// node does run. The account-less bracket keeps today's refusal on the very same rosters: naming an
/// account is an opt-in, never a loosening for the bracket that names none.
#[test]
fn a_bracket_naming_an_account_is_judged_like_a_submit_naming_one() {
    assert_eq!(account_refusal(&bracket_on("binance", "ALT"), &roster()), None, "a held account");
    assert_eq!(account_refusal(&bracket_on("binance", "DEFAULT"), &roster()), None, "the bare key");
    let msg = account_refusal(&bracket_on("binance", "NOSUCH"), &roster()).expect("refused");
    assert!(
        msg.contains("NOSUCH") && msg.contains("ALT, DEFAULT"),
        "names the books it runs: {msg}"
    );
    assert!(msg.contains("The bracket was REFUSED"), "{msg}");
    // A venue whose accounts are ALL labelled: naming one is accepted (the account-less bracket was
    // refused there, because the core's engine-0 fallback may be another exchange's book).
    let labelled = vec!["binance#ALT".to_string(), "polymarket".to_string()];
    assert_eq!(account_refusal(&bracket_on("binance", "ALT"), &labelled), None);
    assert!(account_refusal(&bracket_on("binance", "DEFAULT"), &labelled).is_some(), "not held");
    // ...and the ACCOUNT-LESS bracket is refused on both rosters, as it always was.
    assert!(account_refusal(&bracket("binance"), &roster()).is_some());
    assert!(account_refusal(&bracket("binance"), &labelled).is_some());
    // A label this gate cannot parse is `lower_command`'s refusal, as for a submit.
    assert_eq!(account_refusal(&bracket_on("binance", "bad label!"), &roster()), None);
}

/// A named bracket is still the one verb an EMPTY roster refuses: "this account runs" is a claim an
/// empty roster cannot back, and the refusal leaves every other command flowing.
#[test]
fn a_named_bracket_is_refused_while_the_roster_is_empty() {
    let msg = account_refusal(&bracket_on("binance", "ALT"), &[]).expect("refused");
    assert!(msg.contains("not published") && msg.contains("first snapshot"), "{msg}");
}

/// The two gates that read an ACCOUNT of a command read a bracket's own: a named bracket for an
/// account that does not run, or that waits for its balance, is refused naming THAT account, while the
/// default account's account-less bracket beside it is not (it used to be judged as the default
/// account whatever it named, which would let `ALT`'s bracket past the hold on `ALT`).
#[test]
fn the_not_running_and_waiting_gates_read_a_brackets_account() {
    let not_running = [vike_exec::NotRunningAccount {
        venue: "binance".into(),
        account: vike_model::accounts::account_keys::AccountLabel::parse("ALT").ok(),
        route_key: "binance#ALT".into(),
        tier: Some(vike_model::AccountTier::Demo),
        reason: "no credentials".into(),
    }];
    let msg = not_running_refusal(&bracket_on("binance", "ALT"), &not_running).expect("refused");
    assert!(
        msg.contains("account `ALT` of venue `binance`") && msg.contains("The bracket"),
        "{msg}"
    );
    assert_eq!(not_running_refusal(&bracket("binance"), &not_running), None, "the default runs");

    let (facts, _synced) = facts_waiting();
    let held = awaiting_balance_refusal(&bracket_on("binance", "ALT"), Some(&facts));
    let msg = held.expect("held");
    assert!(
        msg.contains("account `ALT` of venue `binance`") && msg.contains("The bracket"),
        "{msg}"
    );
    assert_eq!(
        awaiting_balance_refusal(&bracket("binance"), Some(&facts)),
        None,
        "the default account does not wait"
    );
}

/// Decision 0156 item 2: a command to an account this node knows and does NOT run is REFUSED ON
/// THE WIRE with the reason the mount gave it — never Acked into the core, never routed to another
/// book of the venue, never filled by a paper simulator standing in for it. The node here runs
/// binance's `ALT` account; its DEFAULT account and okx's do not run.
#[test]
fn a_command_to_an_account_that_does_not_run_is_refused_naming_its_reason() {
    let not_running = [
        vike_exec::NotRunningAccount {
            venue: "binance".into(),
            account: None,
            route_key: "binance".into(),
            tier: Some(vike_model::AccountTier::Live),
            reason: "no credentials".into(),
        },
        vike_exec::NotRunningAccount {
            venue: "okx".into(),
            account: None,
            route_key: "okx".into(),
            tier: Some(vike_model::AccountTier::Demo),
            reason: "flags.tradehub_live off".into(),
        },
    ];
    // An account-less order addresses the DEFAULT account: it does not run, so it is refused —
    // before the decision it would have reached a paper engine standing in for it.
    for (cmd, subject) in [
        (submit("binance", None), "The command"),
        (submit("binance", Some("DEFAULT")), "The command"),
        (mount("binance", None), "The mount"),
    ] {
        let msg = not_running_refusal(&cmd, &not_running).expect("refused");
        assert!(msg.contains("does NOT RUN") && msg.contains("no credentials"), "{msg}");
        assert!(msg.contains("tier `live`") && msg.contains(subject), "{msg}");
        assert!(msg.contains("no paper simulator stands in"), "{msg}");
    }
    let okx = not_running_refusal(&submit("okx", None), &not_running).expect("refused");
    assert!(okx.contains("flags.tradehub_live off") && okx.contains("tier `demo`"), "{okx}");
    // CONTROL: the account that runs passes this gate, and so does a venue it knows nothing of
    // (the venue gate's business).
    assert_eq!(not_running_refusal(&submit("binance", Some("ALT")), &not_running), None);
    assert_eq!(not_running_refusal(&submit("polymarket", None), &not_running), None);
    // The reducing plane keeps its fan-out: an account-less cancel-all reaches what runs.
    let cancel_all =
        WireCommand::MassCancel { venue: Some("binance".into()), account: None, symbol: None };
    assert_eq!(not_running_refusal(&cancel_all, &not_running), None);
    // No list, no verdict: a node with every account running.
    assert_eq!(not_running_refusal(&submit("binance", None), &[]), None);
}

// ---- an account started in the running node, still waiting for its venue balance (decision 0169) ----

/// The source a hot start of `binance`'s `ALT` account and `polymarket`'s default one announced a wait to.
fn facts_waiting() -> (crate::remote_backend::facts::LeaseFacts, Arc<AtomicBool>) {
    let facts = crate::remote_backend::facts::LeaseFacts::default();
    let synced = Arc::new(AtomicBool::new(false));
    facts.engine_awaiting_balance("binance#ALT", &synced);
    facts.engine_awaiting_balance("polymarket", &synced);
    (facts, synced)
}

fn reducing(venue: &str, account: Option<&str>) -> WireCommand {
    match submit(venue, account) {
        WireCommand::Submit(mut req) => {
            req.reduce_only = true;
            WireCommand::Submit(req)
        }
        other => other,
    }
}

/// The 2026-10-18 okx drill: an order three seconds after a hot start was judged on the engine's SEED cash.
/// While the account's venue balance has not landed the door holds its OPENING orders, in words that say what
/// to do, and holds nothing else: a reducing order, a cancel, a flatten and a market exit still go.
#[test]
fn an_account_awaiting_its_balance_is_refused_an_opening_order_and_nothing_that_reduces_risk() {
    let (facts, _synced) = facts_waiting();
    let held = |cmd: &WireCommand| awaiting_balance_refusal(cmd, Some(&facts));

    for (cmd, subject) in [
        (submit("binance", Some("ALT")), "The order"),
        (mount("binance", Some("ALT")), "The mount"),
        (bracket("polymarket"), "The bracket"),
    ] {
        let msg = held(&cmd).unwrap_or_else(|| panic!("{cmd:?} must be held"));
        assert!(msg.contains("awaiting the venue balance after the account start"), "{msg}");
        assert!(msg.contains("retry in a few seconds"), "the operator is told what to do: {msg}");
        assert!(msg.contains(subject) && msg.contains("REFUSED"), "{msg}");
        assert!(msg.contains("Reducing orders, cancels"), "...and what is NOT held: {msg}");
    }
    let msg = held(&submit("binance", Some("ALT"))).expect("held");
    assert!(msg.contains("account `ALT` of venue `binance`"), "it names the book: {msg}");

    let reducing_plane = [
        reducing("binance", Some("ALT")),
        WireCommand::Cancel("c-1".into()),
        WireCommand::Modify { client_order_id: "c-1".into(), new_qty: Some(2.0), new_price: None },
        WireCommand::MassCancel {
            venue: Some("binance".into()),
            account: Some("ALT".into()),
            symbol: None,
        },
        WireCommand::Flatten {
            venue: "binance".into(),
            symbol: "SYM".into(),
            account: Some("ALT".into()),
        },
        WireCommand::MarketExit { venue: Some("binance".into()), account: Some("ALT".into()) },
    ];
    for cmd in reducing_plane {
        assert_eq!(held(&cmd), None, "{cmd:?} reduces risk and is never held");
    }
}

/// Only the account that waits is held: its sibling of the same venue, another venue, a surface with no
/// source, and an account no start ever announced (the boot's engines, a `paper` account) are not.
#[test]
fn only_the_account_that_waits_is_held() {
    let (facts, synced) = facts_waiting();
    assert_eq!(awaiting_balance_refusal(&submit("binance", None), Some(&facts)), None, "DEFAULT");
    assert_eq!(awaiting_balance_refusal(&submit("binance", Some("BOB")), Some(&facts)), None);
    assert_eq!(awaiting_balance_refusal(&submit("okx", None), Some(&facts)), None);
    assert_eq!(awaiting_balance_refusal(&submit("binance", Some("ALT")), None), None, "no source");
    let unannounced = crate::remote_backend::facts::LeaseFacts::default();
    assert_eq!(
        awaiting_balance_refusal(&submit("binance", Some("ALT")), Some(&unannounced)),
        None,
        "a start that announced nothing holds nothing"
    );
    // A label the gate cannot parse is `lower_command`'s refusal, not this gate's.
    assert_eq!(
        awaiting_balance_refusal(&submit("binance", Some("bad label!")), Some(&facts)),
        None
    );

    // And once the balance has landed, the account is released.
    synced.store(true, Ordering::Release);
    assert_eq!(awaiting_balance_refusal(&submit("binance", Some("ALT")), Some(&facts)), None);
}

/// The hold reaches the order THROUGH `accept_command`, the production call the table above bypasses: an
/// opening order is refused before anything is lowered or sent, the same order is accepted the moment the
/// flag rises, and a reducing order was never in the way. The paper core exists only to supply a real
/// `CommandSink`.
///
/// ⚠ KILL PROOF: drop the `awaiting_balance_refusal` call from `accept_command` and the first
/// `expect_err` finds an accepted order.
#[test]
fn accept_command_holds_an_opening_order_until_the_balance_lands() {
    let mount = vike_tradehub_core::build_paper_maker_core(
        &vike_tradehub_core::MakerConfig::outcome_token(
            "polymarket",
            "AWAITING_BALANCE_TOKEN",
            Some(3_000_000_000),
        ),
    );
    let sink = mount.handle.command_sink();
    let (facts, synced) = facts_waiting();
    let mut limits = super::control::ControlLimits::new(super::control::ControlLimitsConfig {
        max_notional: None,
        rate_per_sec: 1e9,
    });
    let mut accept = |cmd: WireCommand| {
        super::control::accept_command(
            cmd,
            None,
            &mut limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            Some(&facts),
            None,
            None,
        )
    };

    let err = accept(submit("binance", Some("ALT"))).expect_err("held: the balance has not landed");
    let msg = err.message();
    assert!(msg.contains("awaiting the venue balance after the account start"), "{msg}");
    assert!(!err.is_fatal(), "a hold never closes the surface");
    // The reducing order is not in the way of the account that waits.
    assert!(accept(reducing("binance", Some("ALT"))).is_ok(), "a reducing order goes");

    synced.store(true, Ordering::Release);
    assert!(accept(submit("binance", Some("ALT"))).is_ok(), "the balance landed: accepted");
    mount.handle.shutdown_and_join();
}
