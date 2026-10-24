//! [`venue_refusal`] in isolation — the pure verdict. The end-to-end proof (the refusal
//! reaching a real control peer as a `Response::Error`, and a matched address reaching the
//! engine it names on a two-engine core) is `tests/daemon/venue_routing.rs`.

use super::refusal::venue_refusal;
use vike_tradehub_client::wire::{WireCommand, WireTradingState};

fn roster() -> Vec<String> {
    vec!["polymarket".to_string(), "binance".to_string()]
}

fn submit(venue: &str) -> WireCommand {
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
        account: None,
    })
}

#[test]
fn a_venue_this_node_runs_is_not_refused() {
    assert_eq!(venue_refusal(&submit("binance"), &roster()), None);
    assert_eq!(venue_refusal(&submit("polymarket"), &roster()), None);
}

#[test]
fn an_unmatched_venue_is_refused_and_the_message_names_the_whole_roster() {
    let msg = venue_refusal(&submit("okx"), &roster()).expect("refused");
    assert!(msg.contains("okx"), "{msg}");
    assert!(msg.contains("binance") && msg.contains("polymarket"), "{msg}");
}

/// The roster is printed SORTED and DEDUPED, so a two-account node does not name one exchange
/// twice and the list reads the same however the engines were registered. The refusal DECISION
/// is unaffected by either (it is a membership test).
#[test]
fn the_named_roster_is_sorted_and_deduplicated() {
    let two_accounts = vec!["binance".to_string(), "polymarket".to_string(), "binance".to_string()];
    let msg = venue_refusal(&submit("okx"), &two_accounts).expect("refused");
    assert!(msg.contains("binance, polymarket"), "{msg}");
    assert!(!msg.contains("binance, binance"), "{msg}");
}

/// ⚠ EXACT match, never case-folded or trimmed — the core selects an engine by string equality
/// on its route key, so accepting `"BINANCE"` here would hand it a string it fails to route and
/// silently sends to engine 0: this gate's own defect, reintroduced by being helpful.
#[test]
fn the_match_is_exact_so_a_case_or_space_slip_is_refused_rather_than_guessed() {
    for slip in ["BINANCE", "Binance", " binance", "binance "] {
        let msg = venue_refusal(&submit(slip), &roster())
            .unwrap_or_else(|| panic!("`{slip}` must not be read as `binance`"));
        assert!(msg.contains(slip), "the refusal quotes what was asked for: {msg}");
    }
}

/// An EMPTY roster is "the core has not published yet", NOT "this node runs no engines", and
/// refuses nothing. Refusing on it would be permanent: a core publishes when its state goes
/// dirty, and a refused command never reaches the core to make it dirty.
#[test]
fn an_empty_roster_refuses_nothing_because_it_means_unknown() {
    assert_eq!(venue_refusal(&submit("okx"), &[]), None);
    assert_eq!(venue_refusal(&submit("anything-at-all"), &[]), None);
}

/// Every ADDRESS-LESS verb passes untouched — including the UNSCOPED panic button, which must
/// never need an argument.
#[test]
fn an_address_less_command_is_never_refused() {
    let roster = roster();
    for cmd in [
        WireCommand::Cancel("c-1".into()),
        WireCommand::Modify { client_order_id: "c-1".into(), new_qty: None, new_price: None },
        WireCommand::SetTradingState(WireTradingState::Halted),
        WireCommand::MassCancel { venue: None, symbol: None, account: None },
        WireCommand::MarketExit { venue: None, account: None },
        WireCommand::UnmountStrategy { controller_id: "m-1".into() },
    ] {
        assert_eq!(venue_refusal(&cmd, &roster), None, "{cmd:?} names no venue");
    }
}

/// ...and every SCOPED one is checked, whichever verb it is. A `flatten okx` or a
/// `market-exit okx` on a node with no okx engine acts on the PRIMARY's book without this.
#[test]
fn every_scoped_verb_is_checked_not_just_submit() {
    let roster = roster();
    for cmd in [
        WireCommand::Flatten { venue: "okx".into(), symbol: "SYM".into(), account: None },
        WireCommand::MassCancel { venue: Some("okx".into()), symbol: None, account: None },
        WireCommand::MarketExit { venue: Some("okx".into()), account: None },
        WireCommand::UpdateParams {
            venue: "okx".into(),
            symbol: "SYM".into(),
            interval: "1m".into(),
            mount_id: None,
            params: serde_json::json!({}),
        },
        WireCommand::MountStrategy {
            venue: "okx".into(),
            account: None,
            symbol: "SYM".into(),
            interval: "1m".into(),
            controller_id: None,
            name: Some("buy_hold".into()),
            rhai: None,
            params: serde_json::json!({}),
        },
    ] {
        assert!(venue_refusal(&cmd, &roster).is_some(), "{cmd:?} addresses okx and must refuse");
    }
}
