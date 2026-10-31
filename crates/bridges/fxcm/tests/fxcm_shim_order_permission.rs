//! **The shim refuses, by name and before it sends, an order the account has no permission to create**
//! (`fcshim.cpp`'s `orderCreationDisabled`, `FC_ORDER_NOT_PERMITTED` = -16). Gated on a box with no ForexConnect SDK
//! like `fxcm_shim_subscription.rs`: no CI runner has ever compiled the shim, so the rule is pinned from the SOURCE.
//!
//! MEASURED on the demo login, 2026-10-26 (`tests/fixtures/order_permission_probe.json`): of the 48 regular forex
//! rows exactly one, USD/HKD, reads `canCreateEntryOrder` and `canCreateMarketOpenOrder` Disabled. It reads
//! subscription `T` and quotes normally, and an order on it was answered `ORA-20102: Access Violation` after 4.9 s
//! (the live smoke at 13:51 UTC panicked on it): a refusal that said nothing about why. The live proof of the named
//! refusal is `tests/fxcm_subscribe_smoke.rs`'s permission step.

use std::path::PathBuf;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let path = crate_dir().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// `fcshim.cpp` with `//` comments removed and whitespace collapsed (its prose names calls its code must not make).
fn shim_code() -> String {
    read("src/shim/fcshim.cpp")
        .lines()
        .map(|l| match l.find("//") {
            Some(at) if !l[..at].contains('"') => &l[..at],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The code text from `from` to the next occurrence of `to` after it.
fn between(code: &str, from: &str, to: &str) -> String {
    let start = code.find(from).unwrap_or_else(|| panic!("fcshim.cpp no longer has `{from}`"));
    let rest = &code[start..];
    let end = rest[from.len()..].find(to).map_or(rest.len(), |at| at + from.len());
    rest[..end].to_string()
}

fn at(haystack: &str, needle: &str) -> usize {
    haystack.find(needle).unwrap_or_else(|| panic!("`{needle}` is missing from: {haystack}"))
}

/// The code is -16 in BOTH halves: the shim returns it and `subscription.rs` reads it as a verdict.
#[test]
fn the_code_is_minus_sixteen_in_the_shim_and_in_the_reader() {
    let shim = shim_code();
    assert!(shim.contains("#define FC_ORDER_NOT_PERMITTED (-16)"), "the shim's code changed");
    let rust = read("src/subscription.rs");
    assert!(rust.contains("const FC_ORDER_NOT_PERMITTED: i32 = -16;"), "the reader's code changed");
    assert!(
        rust.contains("FC_ORDER_NOT_PERMITTED => definite("),
        "-16 must be a VERDICT, not an unknown outcome"
    );
}

/// ONLY `Disabled` refuses. Hidden (-2), unknown and an unreadable checker are the venue's to judge: the venue accepted an
/// order whose `canCreateNetStopLimitOrder` read Hidden, so refusing on `!= Enabled` would refuse orders FXCM takes.
#[test]
fn only_a_disabled_permission_refuses() {
    let helper = between(&shim_code(), "static bool orderCreationDisabled(", "static ");
    assert!(
        helper.contains("canCreateMarketOpenOrder(instr)")
            && helper.contains("canCreateEntryOrder(instr)")
    );
    assert!(helper.contains("== PermissionDisabled"), "{helper}");
    assert!(
        !helper.contains("!= PermissionEnabled"),
        "a Hidden permission must not refuse: {helper}"
    );
    assert!(
        helper.contains("if (!rules) return false;") && helper.contains("if (!perm) return false;"),
        "{helper}"
    );
}

/// A placement refuses BEFORE it builds or sends anything, by the market/entry kind it is about to send, and says which.
#[test]
fn a_placement_refuses_a_disabled_permission_before_it_builds_or_sends() {
    let place =
        between(&shim_code(), "static int fc_place(Handle* h", "FC_EXPORT int fc_place_entry(");
    let check = at(&place, "orderCreationDisabled(h->session, instrument, market)");
    assert!(
        check > at(&place, "findOffer(h->session"),
        "the offer is looked up first: an unknown row is still `offer not found`"
    );
    for later in ["createValueMap()", "createOrderRequest(", "sendRequest("] {
        assert!(check < at(&place, later), "the permission check must come before `{later}`");
    }
    let refusal = &place[check..at(&place, "return FC_ORDER_NOT_PERMITTED;")];
    assert!(!refusal.contains("sendRequest"), "nothing is sent for a refused order: {refusal}");
    assert!(
        refusal.contains("the account may not create") && refusal.contains("Nothing was sent"),
        "{refusal}"
    );
}

/// A subscription is not WRITTEN for an instrument no order of this bridge can use, and an already tradable one is still
/// answered at once: the check sits after the early `T` returns and before the request.
#[test]
fn a_subscription_write_is_refused_when_neither_order_kind_is_permitted() {
    let body = between(&shim_code(), "FC_EXPORT int fc_set_subscription(", "FC_EXPORT");
    let check = at(
        &body,
        "orderCreationDisabled(h->session, instrument, true) && orderCreationDisabled(h->session, instrument, false)",
    );
    assert!(
        check > at(&body, "snapshotReads(h->session, instrument, status)"),
        "an instrument that reads T needs no check here"
    );
    assert!(
        check > at(&body, "return FC_UNKNOWN_INSTRUMENT_TYPE;"),
        "the instrument-type rule comes first"
    );
    assert!(
        check < at(&body, "SetSubscriptionStatus"),
        "the refusal must come before the request is built"
    );
    assert!(body[check..].contains("return FC_ORDER_NOT_PERMITTED;"));
    assert!(!body[check..at(&body, "SetSubscriptionStatus")].contains("sendRequest"));
}

/// The measured fixture says what the rule is built on: exactly one forex row with order creation Disabled, and the
/// subscription of that row was not refused by the venue (`canChangeOfferSubscription` Enabled), so only a permission
/// read, never the subscription answer, can tell the bridge.
#[test]
fn the_measured_rows_agree_with_the_rule() {
    let fixture: serde_json::Value =
        serde_json::from_str(&read("tests/fixtures/order_permission_probe.json")).expect("JSON");
    assert_eq!(fixture["forex_rows"], 48);
    assert_eq!(fixture["rows_with_order_creation_disabled"], serde_json::json!(["USD/HKD"]));
    let disabled = fixture["permission_values"]["Disabled"].as_i64().expect("Disabled");
    let rows = fixture["permissions"].as_array().expect("rows");
    for row in rows {
        let refused = row["can_create_entry_order"].as_i64() == Some(disabled)
            || row["can_create_market_open_order"].as_i64() == Some(disabled);
        assert_eq!(refused, row["instrument"] == "USD/HKD", "{row}");
        assert_eq!(
            row["can_change_offer_subscription"], 1,
            "{row}: the subscription request is never what refuses"
        );
    }
    // Hidden (-2) is the permission the close path reads for USD/HKD: it is not Disabled, so it never refuses an open.
    assert_ne!(rows[0]["can_create_market_close_order"].as_i64(), Some(disabled));
    for placement in fixture["usdhkd_placements"].as_array().expect("placements") {
        assert!(
            placement["venue_text"].as_str().expect("text").contains("ORA-20102: Access Violation")
        );
    }
}
