//! The venue gate: a write naming a venue the node does not mount is refused, never guessed.

use super::*;
use crate::cmd::mcp::venue_gate::{VENUE_CHECK_NONE, commanded_venue, venue_verdict};

/// The venue a write NAMES, per wire variant — the input half of the gate.
///
/// ⚠ The two OPTIONAL rows are the ones worth having a test for. `market_exit` and
/// `mass_cancel` mean "every engine" when the argument is omitted, so an omission must read as
/// "nothing to check" and never as "an empty venue to compare" — a gate that refused there
/// would take out the panic button, which is a worse failure than the one being closed.
#[test]
fn the_venue_a_write_names_is_read_per_variant() {
    let of = |tool: &str, args: Value| {
        let cmd = verbs::wire_command_for(tool, &args).expect("a valid write");
        commanded_venue(&cmd).map(str::to_string)
    };
    assert_eq!(
        of("submit_order", json!({ "venue": "sim", "symbol": "B", "side": 1, "qty": 1.0 })),
        Some("sim".to_string())
    );
    assert_eq!(of("flatten", json!({ "venue": "sim", "symbol": "B" })), Some("sim".to_string()));
    assert_eq!(of("market_exit", json!({ "venue": "sim" })), Some("sim".to_string()));
    assert_eq!(of("mass_cancel", json!({ "venue": "sim" })), Some("sim".to_string()));
    // ...and the venue-less shapes, each of which must be checked against nothing.
    assert_eq!(of("market_exit", json!({})), None, "an omitted venue means EVERY engine");
    assert_eq!(of("mass_cancel", json!({})), None, "an omitted venue means EVERY venue");
    assert_eq!(of("cancel_order", json!({ "client_order_id": "c1" })), None);
    assert_eq!(of("modify", json!({ "client_order_id": "c1", "new_qty": 2.0 })), None);
    assert_eq!(of("set_trading_state", json!({ "state": "halted" })), None);
}

/// The decision, in every direction it has.
///
/// ⚠ THE POSITION THIS PINS is the 2026-09-06 model run's: an agent invented `venue: "node"`
/// from the operator's wording, and every layer under it accepted the string — the node's
/// dry-run vets only the notional cap, and `vike_core`'s `apply_intent_routed` resolves an
/// unroutable venue with `unwrap_or(0)`, i.e. onto the node's FIRST engine with the capability
/// preflight skipped. A real order rested in the book under a venue that names nothing.
#[test]
fn a_venue_the_node_does_not_mount_is_refused_and_the_refusal_names_what_is_mounted() {
    let mounted = ["polymarket".to_string(), "binance".to_string()];
    let err = venue_verdict(Some("node"), None, Some(&mounted[..])).expect_err("must refuse");
    // The offending value, so the agent knows WHICH argument to change...
    assert!(err.contains("\"node\""), "the refusal must name the offending value: {err}");
    // ...what the node actually has, so it can fix it without a guess...
    assert!(err.contains("polymarket"), "the refusal must name the mounted venues: {err}");
    assert!(err.contains("binance"), "the refusal must name EVERY mounted venue: {err}");
    // ...and the tool that reports it, which is the step the failing run had skipped.
    assert!(err.contains("node_snapshot"), "the refusal must name the read tool: {err}");
}

/// The three passing dispositions, and the one that must NOT be a refusal.
#[test]
fn the_venue_check_reports_which_of_the_three_answers_it_gave() {
    let mounted = ["polymarket".to_string()];
    assert_eq!(
        venue_verdict(Some("polymarket"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
    assert_eq!(
        venue_verdict(None, None, Some(&mounted[..])),
        Ok(VENUE_CHECK_NONE),
        "nothing to check"
    );
    // ⚠ NO EVIDENCE IS NOT A REFUSAL. A node that cannot be read is a node that cannot be
    // written to either (`Server::execute` fails there), so refusing here would buy nothing and
    // would break every preview taken against an unreachable node — which is the shape
    // `mcp_transcript.rs` drives over a node-less server. What it must NOT do is present the
    // unmade check as a passed one, which is why this is its own value rather than `mounted`.
    assert_eq!(venue_verdict(Some("anything"), None, None), Ok(VENUE_CHECK_UNVERIFIED));
}

/// **AMBIGUITY REFUSES, and it is a DIFFERENT fact from absent evidence.** A node that answered
/// with two candidates told us more than a node we could not read, and a gate that collapsed
/// the two would discard the more informative answer.
#[test]
fn a_venue_carried_by_two_accounts_is_refused_rather_than_guessed() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string(), "okx".to_string()];
    let err = venue_verdict(Some("binance"), None, Some(&mounted[..]))
        .expect_err("a venue two accounts carry may not be guessed");
    assert!(err.contains("AMBIGUOUS"), "{err}");
    assert!(err.contains("binance#ALT"), "the refusal must NAME the candidates: {err}");
    assert!(
        err.contains("no preview_token was issued"),
        "the refusal must say the two-call gate was not half given away: {err}"
    );
    assert!(
        !err.contains("default"),
        "it must NEVER launder the guess as a reassurance — 0041's own charge: {err}"
    );
}

/// Naming the ROUTE KEY outright resolves it — that is what the refusal above tells the caller
/// to do, so it has to work.
#[test]
fn naming_the_route_key_resolves_the_ambiguity() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    assert_eq!(
        venue_verdict(Some("binance#ALT"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
}

/// …and so does naming it in the ACCOUNT FIELD, which is the spelling the wire actually offers
/// now that `MountStrategy` carries one. Without this arm the gate would refuse a command that
/// says exactly which book it means — the ambiguity refusal firing on the one caller who
/// removed the ambiguity.
#[test]
fn naming_the_account_field_resolves_the_ambiguity() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    assert_eq!(
        venue_verdict(Some("binance"), Some("ALT"), Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
}

/// ⚠ **`DEFAULT` IS THE ROW THIS GATE WOULD HAVE LOST, and it is the whole reason the named-
/// account arm runs BEFORE the carrier count.** Its route key is the BARE VENUE, so the count
/// for it is two on this node — identical to the account-less caller's. Judged by the count
/// alone, the caller who said "the unlabelled account, deliberately" is refused as ambiguous
/// and told to name an account, which is what they just did.
#[test]
fn naming_default_is_never_ambiguous_even_though_its_route_key_is_the_bare_venue() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    assert_eq!(
        venue_verdict(Some("binance"), Some("DEFAULT"), Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
    // …and the account-less caller on the SAME node is still refused, so this cannot be passed
    // by an arm that merely stopped counting.
    assert!(
        venue_verdict(Some("binance"), None, Some(&mounted[..])).is_err(),
        "absence is a different row and must still refuse"
    );
}

/// An account this node does not run is refused BY NAME, and the refusal shows the route key it
/// looked for — so the caller can compare it against `venues[].route_key` directly instead of
/// re-deriving the `#` grammar themselves.
#[test]
fn an_unmounted_account_is_refused_by_name_and_shows_the_key_it_sought() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    let err = venue_verdict(Some("binance"), Some("HEDGE"), Some(&mounted[..]))
        .expect_err("an unarmed account resolves no engine");
    assert!(err.contains("HEDGE"), "names the account: {err}");
    assert!(err.contains("binance#HEDGE"), "…and the route key it sought: {err}");
    assert!(
        err.contains("no preview_token was issued"),
        "…and that the two-call gate was not half given away: {err}"
    );
}

/// An account string that is not a legal account NAME is refused as such, before any lookup —
/// the grammar is `parse_wire_account`'s, so the gate has no second set of rules to drift from.
#[test]
fn an_illegal_account_string_is_refused_before_the_lookup() {
    let mounted = ["binance".to_string(), "binance#ALT".to_string()];
    let err = venue_verdict(Some("binance"), Some("alt"), Some(&mounted[..]))
        .expect_err("a lowercase label is not a legal account name");
    assert!(err.contains("not a legal account name"), "{err}");
    assert!(err.contains("no preview_token was issued"), "{err}");
}

/// ⚠ The complement, and it is what keeps the refusal from being a capability regression: a
/// venue exactly ONE account carries still passes, which is every node in production today.
#[test]
fn a_venue_one_account_carries_still_passes() {
    let mounted = ["binance".to_string(), "okx".to_string()];
    assert_eq!(venue_verdict(Some("binance"), None, Some(&mounted[..])), Ok(VENUE_CHECK_MOUNTED));
    assert_eq!(venue_verdict(Some("okx"), None, Some(&mounted[..])), Ok(VENUE_CHECK_MOUNTED));
}

/// ⚠ A venue whose name is a PREFIX of another's must not be read as that other's account.
/// `binanceus` is a roster venue, not `binance`'s account — the separator is what decides, and
/// `label_of_route_key` is what knows it.
#[test]
fn a_prefix_venue_is_not_mistaken_for_an_account_of_another() {
    let mounted = ["binance".to_string(), "binanceus".to_string()];
    assert_eq!(
        venue_verdict(Some("binance"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED),
        "binanceus is a different venue, so binance is carried by exactly one account"
    );
}
/// EXACT, never case-folded.
///
/// ⚠ Routing compares the payload's venue to an engine's `route_key` with `==`
/// (`vike_core`'s `engine_idx_for_route_key`), so `"Polymarket"` routes to nothing on a node
/// mounting `"polymarket"` and falls back to the first engine exactly as `"node"` did. A
/// case-insensitive refusal would therefore ADMIT a string the routing silently redirects —
/// the hole reopened, wearing a friendlier spelling.
#[test]
fn the_venue_comparison_is_exact_because_the_routing_is() {
    let mounted = ["polymarket".to_string()];
    assert!(venue_verdict(Some("Polymarket"), None, Some(&mounted[..])).is_err(), "case matters");
    assert!(
        venue_verdict(Some("polymarket "), None, Some(&mounted[..])).is_err(),
        "whitespace matters"
    );
    assert_eq!(
        venue_verdict(Some("polymarket"), None, Some(&mounted[..])),
        Ok(VENUE_CHECK_MOUNTED)
    );
}

/// A preview against a node-less server is UNVERIFIED, not refused — and it SAYS so.
///
/// The end-to-end half of the disposition above, over the shipped router rather than the pure
/// function: `test_server()` has no node, so the mounted set cannot be read, and every write
/// tool must still preview exactly as it did before this gate existed.
#[test]
fn a_preview_with_no_node_reports_the_venue_as_unverified_rather_than_refusing_it() {
    let resp =
        call("submit_order", json!({ "venue": "node", "symbol": "B", "side": 1, "qty": 1.0 }));
    assert_eq!(resp["result"]["isError"], false, "no node = no evidence = no refusal: {resp}");
    let sc = &resp["result"]["structuredContent"];
    assert_eq!(sc["venue_check"], VENUE_CHECK_UNVERIFIED, "{sc}");
    assert!(sc["preview_token"].is_string(), "an unverified preview still mints a token: {sc}");
}

/// Every write tool's preview carries the field, so an agent never has to infer from its
/// absence whether the venue was looked at.
#[test]
fn every_write_tool_preview_reports_its_venue_check() {
    for tool in WRITE_TOOLS {
        let resp = call(tool, every_write_tools_arguments());
        let sc = &resp["result"]["structuredContent"];
        let got = sc["venue_check"].as_str().unwrap_or_default();
        assert!(
            [VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE, VENUE_CHECK_UNVERIFIED].contains(&got),
            "{tool}: every preview reports one of the three dispositions, got {sc}"
        );
    }
}
