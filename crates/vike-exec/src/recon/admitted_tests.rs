use super::*;

#[test]
fn a_new_symbol_is_kept_upper_cased_in_admission_order() {
    let a = AdmittedSymbols::default();
    assert!(a.admit("ethusdt.p", "BTCUSDT.P"));
    assert!(a.admit("SOLUSDT.P", "BTCUSDT.P"));
    assert_eq!(a.symbols(), ["ETHUSDT.P", "SOLUSDT.P"]);
}

#[test]
fn the_mounted_symbol_an_empty_one_and_a_repeat_are_not_admitted() {
    let a = AdmittedSymbols::default();
    assert!(!a.admit("btcusdt.p", "BTCUSDT.P"), "the mounted symbol is read anyway");
    assert!(!a.admit("", "BTCUSDT.P"));
    assert!(a.admit("ETHUSDT.P", "BTCUSDT.P"));
    assert!(!a.admit("ethusdt.p", "BTCUSDT.P"), "a repeat is not a second slot");
    assert_eq!(a.symbols(), ["ETHUSDT.P"]);
}

/// The engine's own cap (`MAX_EXTRA_SYMBOLS`): the client never reads a symbol the engine could not
/// have admitted, so a 17th extra symbol is refused and the first sixteen stay.
#[test]
fn the_list_is_bounded_by_the_engines_extra_symbol_cap() {
    let a = AdmittedSymbols::default();
    for i in 0..MAX_EXTRA_SYMBOLS {
        assert!(a.admit(&format!("S{i}USDT.P"), "BTCUSDT.P"), "slot {i}");
    }
    assert!(!a.admit("OVERUSDT.P", "BTCUSDT.P"));
    assert_eq!(a.symbols().len(), MAX_EXTRA_SYMBOLS);
    assert!(!a.symbols().contains(&"OVERUSDT.P".to_string()));
}

/// A symbol the engine gave back leaves the list (in any case), keeps the others in their order, and frees
/// its place: after it a 17th symbol is taken that a full list refused.
#[test]
fn a_forgotten_symbol_frees_its_place() {
    let a = AdmittedSymbols::default();
    for i in 0..MAX_EXTRA_SYMBOLS {
        assert!(a.admit(&format!("S{i}USDT.P"), "BTCUSDT.P"));
    }
    assert!(!a.admit("OVERUSDT.P", "BTCUSDT.P"), "full");
    assert!(a.forget("s3usdt.p"), "held, whatever the case");
    assert!(!a.forget("S3USDT.P"), "a second forget is a no-op");
    assert!(!a.forget("NEVERUSDT.P"), "so is a symbol never held");
    assert_eq!(a.symbols().len(), MAX_EXTRA_SYMBOLS - 1);
    assert_eq!(a.symbols()[3], "S4USDT.P", "the others keep their order");
    assert!(a.admit("OVERUSDT.P", "BTCUSDT.P"), "the freed place takes the next symbol");
}

fn list(symbols: &[&str]) -> Option<Vec<String>> {
    Some(symbols.iter().map(ToString::to_string).collect())
}

/// A client that keeps no list (`ReconClient::admitted_symbols`' default) claims nothing, so the
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
