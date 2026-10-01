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

use super::*;

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
            vike_model::account_keys::parse_wire_account(slip).is_err(),
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
