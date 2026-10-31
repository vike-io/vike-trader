//! **The shim's `fc_set_subscription` is the request FXCM's own SDK sample sends**, gated on a box with no ForexConnect
//! SDK like `fxcm_shim_table_reads.rs` and `fxcm_shim_conversion_reads.rs`: no CI runner has ever compiled
//! `crates/bridges/fxcm/src/shim/fcshim.cpp`, so what the request carries, how long it may take and which codes it
//! returns are pinned from the SOURCE. They cannot prove what FXCM answers; the answers they are held against were
//! measured on the demo login on 2026-10-26 (`tests/fixtures/subscription_probe.json`,
//! `docs/ops/fxcm-forexconnect.md`'s "Subscribing an instrument"), and `tests/fxcm_subscribe_smoke.rs` is the live
//! proof of the whole path.

use std::path::PathBuf;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `fcshim.cpp` with `//` comments removed (its prose names calls its code must not make).
fn shim_code() -> String {
    let path = crate_dir().join("src/shim/fcshim.cpp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()));
    src.lines()
        .map(|l| match l.find("//") {
            // Only a `//` outside a string literal starts a comment: this file has none inside one on these lines.
            Some(at) if !l[..at].contains('"') => &l[..at],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of `fc_set_subscription`, from its signature to the next export.
fn body() -> String {
    let code = shim_code();
    let start = code
        .find("FC_EXPORT int fc_set_subscription(")
        .expect("fcshim.cpp no longer exports fc_set_subscription");
    let rest = &code[start..];
    let end = rest[10..].find("FC_EXPORT").map_or(rest.len(), |at| at + 10);
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The request is `Command`, `SubscriptionStatus` and `OfferID` (the SDK sample's, and the documented parameter set)
/// and nothing else: no account, no rate, no order type.
#[test]
fn the_request_is_the_sdks_set_subscription_status_and_nothing_more() {
    let body = body();
    for needle in [
        "vm->setString(Command, O2G2::Commands::SetSubscriptionStatus);",
        "vm->setString(SubscriptionStatus, status);",
        "vm->setString(OfferID, offerId.c_str());",
    ] {
        assert!(body.contains(needle), "the request lost `{needle}`: {body}");
    }
    for stranger in ["AccountID", "Rate", "OrderType", "Amount", "CreateOrder", "EditOrder"] {
        assert!(
            !body.contains(stranger),
            "fc_set_subscription sends `{stranger}`, which the SDK sample does not: {body}"
        );
    }
    assert_eq!(body.matches("vm->set").count(), 3, "exactly three request parameters: {body}");
}

/// The row is found by NAME in a FRESH table (the login-time snapshot answers for what it knew at login), the
/// request is answered inside a bound, and the row is read again until it shows the new status.
#[test]
fn the_lookup_is_fresh_the_wait_is_bounded_and_the_row_is_read_back() {
    let code = shim_code();
    let body = body();
    assert!(
        !body.contains("getTableRefreshResponse"),
        "the lookup must not read the login-time snapshot: {body}"
    );
    assert!(
        code.contains("refreshTable(s, Offers, 0, resp, err, timeoutMs)"),
        "the row lookup must be a refresh-table request with its own bound"
    );
    // Every wait inside the function is bounded by the caller's budget.
    for needle in [
        "budget_ms",
        "rl->wait(atMost(SUBSCRIBE_REPLY_MS, left()))",
        "SUBSCRIBE_SETTLE_READ_MS",
        "SUBSCRIBE_SETTLE_STEP_MS",
    ] {
        assert!(body.contains(needle), "the wait lost `{needle}`: {body}");
    }
    // The answer is checked BEFORE the settle loop, and the loop ends on the budget, not on a hope.
    let answer = body.find("rl->wait(").expect("a wait for the answer");
    let settle = body.find("for (;;)").expect("a settle loop");
    assert!(answer < settle, "the settle loop must follow the answer: {body}");
    assert!(body.contains("if (left() == 0) break;"), "the settle loop has no end: {body}");
    // The subscription listener is armed before the request is sent, and released.
    let arm = body.find("rl->setRequestID(").expect("armed");
    let send = body.find("sendRequest(req)").expect("sent");
    assert!(arm < send, "setRequestID must come BEFORE the send: {body}");
    assert!(body.contains("unsubscribeResponse(rl); rl->release();"), "the listener leaks: {body}");
}

/// The returns are the ones `crate::subscription::answer_from_shim` reads: 0, -3, -4, -5, -6, -8, -9.
#[test]
fn the_returns_are_the_ones_the_rust_side_reads() {
    let body = body();
    for code in [
        "return 0;",
        "return -3;",
        "return -4;",
        "return -5;",
        "return -6;",
        "return -8;",
        "return -9;",
        "return FC_UNKNOWN_INSTRUMENT_TYPE;",
    ] {
        assert!(body.contains(code), "fc_set_subscription lost `{code}`: {body}");
    }
    // The status is validated before anything is sent: only T, V, D.
    assert!(body.contains("strchr(\"TVD\", status[0])"), "the status is not validated: {body}");
    // The one code the shim and `crate::subscription` both spell must be the same number.
    let shim_value: i32 = shim_code()
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_UNKNOWN_INSTRUMENT_TYPE"))
        .and_then(|v| v.trim().trim_start_matches('(').trim_end_matches(')').parse().ok())
        .expect("fcshim.cpp defines FC_UNKNOWN_INSTRUMENT_TYPE as a number");
    let rust = std::fs::read_to_string(crate_dir().join("src/subscription.rs"))
        .expect("read subscription.rs");
    assert!(
        rust.contains(&format!("const FC_UNKNOWN_INSTRUMENT_TYPE: i32 = {shim_value};")),
        "subscription.rs reads a different code for `not a regular forex instrument` than the shim returns ({shim_value})"
    );
}

/// **A failed LOOKUP is -8, never -4/-5/-6/-7**: the shim returns -4/-5 from its SEND stage only and the Rust side reads
/// them as a verdict on the symbol, so a reconnect blip on the first order for a symbol whose row is `T` must not refuse
/// it for ten minutes. The lookup stage is the text from the `offerRowStatus` call to the first use of its answer, and it
/// may return `-3` (not listed) or `-8` and nothing else; passing `refreshTable`'s own code through fails here.
#[test]
fn a_failed_lookup_is_the_unknown_outcome_code_and_never_a_verdict() {
    let body = body();
    let start = body.find("int rc = offerRowStatus(").expect("the lookup stage is gone");
    let end = body[start..]
        .find("std::string now = facts.status;")
        .expect("the lookup stage lost its end")
        + start;
    let lookup = &body[start..end];
    assert!(lookup.contains("return -3;") && lookup.contains("return -8;"), "{lookup}");
    for forbidden in
        ["return rc", "return -4;", "return -5;", "return -6;", "return -7;", "return -9;"]
    {
        assert!(
            !lookup.contains(forbidden),
            "the lookup stage returns `{forbidden}`; only -3 and -8 are the lookup's: {lookup}"
        );
    }
    // The settle loop reads the row too, and never returns a lookup code of its own.
    let settle = &body[body.find("for (;;)").expect("a settle loop")..];
    assert!(
        !settle.contains("return rc"),
        "the settle loop passes a lookup code through: {settle}"
    );
}

/// An instrument that is ALREADY tradable is returned at once, with no round trip, from the login-time snapshot (whose
/// STATUS column follows the venue's pushed updates, MEASURED); and a row that is not tradable is subscribed only if it
/// is a regular forex instrument, AFTER the already-tradable return (so a tradable CFD still trades) and BEFORE the send.
#[test]
fn a_tradable_instrument_costs_no_round_trip_and_only_forex_is_subscribed() {
    let code = shim_code();
    let body = body();
    let snapshot = body
        .find("snapshotReads(h->session, instrument, status)")
        .expect("the snapshot fast path is gone");
    let lookup = body.find("offerRowStatus(").expect("the lookup is gone");
    assert!(snapshot < lookup, "the login-time snapshot must be asked BEFORE any refresh: {body}");
    let reader = &code[code.find("static bool snapshotReads(").expect("snapshotReads is gone")..];
    let reader = &reader[..reader.find("struct OfferFacts").expect("snapshotReads has no end")];
    assert!(
        reader.contains("getTableRefreshResponse(Offers)"),
        "the fast path reads the login-time Offers table"
    );
    assert!(!reader.contains("refreshTable"), "the fast path must not make a round trip");

    let already = body.find("if (now == status)").expect("the already-true return is gone");
    let class =
        body.find("facts.type > (int)InstrumentCrypto").expect("the instrument-type check is gone");
    let send = body.find("createOrderRequest(").expect("the send is gone");
    assert!(
        already < class && class < send,
        "order: already-true return, then the instrument-type check, then the send: {body}"
    );
    // Every class the SDK names is subscribed (the metals, indices, commodities, shares and crypto included): the
    // old rule refused them by their multiplier, and that refusal must not come back.
    assert!(
        !body.contains("std::fabs(facts.multiplier - 1.0)")
            && !body.contains("!= (int)InstrumentForex"),
        "a non-forex row is refused again: {body}"
    );
}

/// **Every entry point that takes an instrument name resolves the VENUE's own spelling first** (`venueName`): the engine's
/// symbol is upper-case and, for six letters, slashed (`COPPER` arrives as `COP/PER`), while the Offers row is `Copper`,
/// and the SDK's name-keyed calls (`getBaseUnitSize`, the permission checker) answer 0 or "unknown" for any other spelling.
#[test]
fn every_entry_point_that_takes_a_name_resolves_the_venues_spelling_first() {
    let code = shim_code();
    assert!(code.contains("static std::string venueName("), "the resolver is gone");
    assert!(code.contains("static std::string canonInstr("), "the canonical name is gone");
    for export in ["fc_place(", "fc_base_unit_size(", "fc_edit_order(", "fc_set_subscription("] {
        let from = code
            .find(&format!("{export}Handle* h"))
            .or_else(|| code.find(&format!("{export}void* hp")))
            .unwrap_or_else(|| panic!("{export} is gone"));
        let body = &code[from..];
        let resolve = body.find("venueName(h->session, instrument)").unwrap_or(usize::MAX);
        let first_use = ["findOffer(", "getBaseUnitSize(", "snapshotReads("]
            .iter()
            .filter_map(|needle| body.find(needle))
            .min()
            .unwrap_or(usize::MAX);
        assert!(
            resolve < first_use,
            "{export} reads the instrument before it resolves the venue's name"
        );
    }
}

/// The export is OPTIONAL (no ABI bump): an installed shim without it still loads, and the Rust side then names the
/// remedy. The loader binds it by name alone; the ABI number is unchanged.
#[test]
fn the_export_is_optional_and_the_abi_does_not_move() {
    let code = shim_code();
    let version: u32 = code
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_ABI_VERSION"))
        .and_then(|v| v.trim().parse().ok())
        .expect("fcshim.cpp no longer defines FC_ABI_VERSION as a number");
    assert_eq!(version, 2, "a bump would strand installed shims; this export is bound by name");
    let loader = std::fs::read_to_string(crate_dir().join("src/loader.rs"))
        .expect("read loader.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        loader.contains("= bind_opt!(\"fc_set_subscription\", FcSetSubscription);"),
        "loader.rs must bind `fc_set_subscription` optionally, by name alone"
    );
}

/// The order path reports the status it found instead of a bare "offer not found" (the exec thread subscribes the
/// instrument first, and the lookup then judges the row with the venue's own word).
#[test]
fn an_offer_that_is_listed_but_not_tradable_is_reported_with_its_status() {
    let code = shim_code();
    assert!(
        code.contains("the offer is listed but its subscription status is `"),
        "findOffer lost the status in its refusal"
    );
    assert!(
        code.contains("offerErr.empty() ? \"offer not found\" : offerErr.c_str()"),
        "fc_place no longer passes the lookup's words on"
    );
}
