use super::*;

fn list(symbols: &[&str]) -> Option<Vec<String>> {
    Some(symbols.iter().map(ToString::to_string).collect())
}

/// A client that keeps no list (`ReconcileClient::admitted_symbols`' default) claims nothing, so the
/// pass keeps the engine's scope, exactly as before the claim existed.
#[test]
fn a_client_with_no_list_claims_nothing() {
    assert_eq!(admitted_throughout(None, None), None);
}

/// The claim is what the client held at BOTH ends of the fetch. A symbol told only after the fetch
/// started was not in the fetch, and one forgotten meanwhile (the daemon restarted under it) may not
/// have been either: claiming either would let the restore planner read the venue's silence as
/// "gone".
#[test]
fn the_claim_is_what_the_client_held_before_and_after_the_fetch() {
    let before = list(&["ETHUSDT.P", "SOLUSDT.P"]);
    assert_eq!(
        admitted_throughout(before.clone(), list(&["SOLUSDT.P", "ETHUSDT.P", "XRPUSDT.P"])),
        list(&["ETHUSDT.P", "SOLUSDT.P"]),
        "told later: not claimed; the order is the first read's"
    );
    assert_eq!(
        admitted_throughout(before.clone(), list(&[])),
        list(&[]),
        "forgotten meanwhile: not claimed"
    );
    assert_eq!(
        admitted_throughout(list(&[]), list(&["ETHUSDT.P"])),
        list(&[]),
        "told during the fetch: not claimed"
    );
    assert_eq!(
        admitted_throughout(before, None),
        list(&[]),
        "a client that stops answering claims nothing"
    );
}
