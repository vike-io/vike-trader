//! Pins that every venue's ARMING ANNOUNCEMENT inside `wire_venue_feeds` is corrected for the
//! venue's other ACCOUNTS — and that the arming ROWS actually reach that function.
//!
//! # Why this exists
//!
//! The decision is fully unit-tested: `other_live_accounts`, `exec_badge` and
//! `with_other_live_accounts` are pure and gated in `main.rs`' own test module, over the real
//! `VenueArming::route_key`. What no test in this workspace can reach is the WIRING — `live_mount`
//! spawns a core, opens sockets and reads the credential store, so nothing links it — and the
//! wiring is where the whole feature can be switched off in one character:
//!
//! * passing `&[]` (or any other slice) for `venue_arming` at the ONE production
//!   `wire_venue_feeds(` call site makes `other_live_accounts` answer EMPTY for every venue, and
//!   every badge reverts to the per-venue answer that was the defect. It compiles clean, and every
//!   pure test above stays green because none of them goes through this file.
//! * a venue arm that builds its `CexArming` WITHOUT the `with_other_live_accounts` wrap keeps the
//!   old badge for that one venue, silently, while its neighbours are correct.
//!
//! So it is checked the way this repo already checks `main.rs` surface it cannot link against: a
//! TEXT gate over the source, claiming nothing beyond what text can show. The sibling
//! `cex_feed_wiring_pin.rs` is the same idiom and carries the fuller argument for it.
//!
//! [`the_wrap_scanner_can_actually_fail`] is the mutation self-test — an unmutated pin is the exact
//! defect this file exists to prevent.

use std::path::{Path, PathBuf};

/// The daemon entry point — still owns the ONE production call site
/// (`feeds.push(wire_venue_feeds(...))`, inside `live_mount_with`) and the `let arming = ...` row
/// computation that feeds it. Named literally: if either moves, this gate must be re-anchored
/// deliberately, never quietly satisfied.
const MAIN: &str = "crates/vike-tradehub/src/tradehub_cli.rs";

/// The file that owns `wire_venue_feeds` ITSELF — the function whose arms are scanned by
/// [`wire_venue_feeds_body`]. ⚠ This used to be `main.rs` too (`MAIN` alone covered both halves of
/// this file's gate): the tradehub-main-split refactor's Task 3 moved `wire_venue_feeds` out of
/// `main.rs` into this crate's LIBRARY as `feeds.rs`, so the DEFINITION and the CALL SITE now sit
/// in different files and need their own constants — `MAIN` still answers the call-site test
/// below, `FEEDS` answers the definition-body one.
const FEEDS: &str = "crates/vike-tradehub/src/feeds.rs";

/// The signature line of the one function whose arms are scanned.
const FN_ANCHOR: &str = "fn wire_venue_feeds(";

/// The `CexArming` PRODUCERS. Every call to one of these inside `wire_venue_feeds` must be
/// enclosed in a `with_other_live_accounts(` correction — the badge each of them produces is the
/// per-venue one, which is exactly what under-claims once a venue has a second account.
///
/// ⚠ `data_only_arming` is deliberately absent: it takes an already-built `CexArming` and replaces
/// only the remedy, so it is never the OUTERMOST producer in an arm.
const PRODUCERS: [&str; 6] = [
    "cex_arming(",
    "alpaca_arming(",
    "ctrader_arming(",
    "oanda_arming(",
    "deribit_arming(",
    "ig_arming(",
];

/// The correction every arm must route its producer through.
const WRAP: &str = "with_other_live_accounts(";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read_main() -> String {
    let path = workspace_root().join(MAIN);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{MAIN}`: {e}"))
}

fn read_feeds() -> String {
    let path = workspace_root().join(FEEDS);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{FEEDS}`: {e}"))
}

/// `wire_venue_feeds`' body: from its signature to the next item at column 0 — comments stripped,
/// so a `//` mention cannot spoof or falsely trip a check.
fn wire_venue_feeds_body(src: &str) -> String {
    assert_eq!(
        src.matches(FN_ANCHOR).count(),
        1,
        "the anchor {FN_ANCHOR:?} must match {FEEDS} EXACTLY once, or this gate is reading a slice \
         nobody chose"
    );
    let at = src.find(FN_ANCHOR).expect("checked above");
    let rest = &src[at..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{FN_ANCHOR:?} has no closing brace at column 0"));
    rest[..end].lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
}

/// **Every announcement in `wire_venue_feeds` goes through the account correction.**
///
/// The predicate is per STATEMENT, not per file: it takes each `let arming = …;` binding in the
/// function and requires the wrap inside it. A file-wide `contains` would be satisfied by ONE
/// corrected arm while five others kept the old badge.
#[test]
fn every_arming_announcement_is_corrected_for_the_venues_other_accounts() {
    let body = wire_venue_feeds_body(&read_feeds());
    let bindings = arming_bindings(&body);
    assert_eq!(
        bindings.len(),
        PRODUCERS.len(),
        "expected one `let arming = …` per producer arm; found {}:\n{bindings:#?}",
        bindings.len()
    );
    for stmt in &bindings {
        assert!(
            stmt.contains(WRAP),
            "an arming announcement is not corrected for the venue's other accounts — it will \
             announce `exec = PAPER` for a venue whose labelled account armed. Got:\n{stmt}"
        );
        assert!(
            stmt.contains("other_live_accounts("),
            "…and it must derive that correction from the arming ROWS, not from a literal:\n{stmt}"
        );
        assert!(
            PRODUCERS.iter().any(|p| stmt.contains(p)),
            "a `let arming` binding names no known producer; add it to PRODUCERS:\n{stmt}"
        );
    }
}

/// **The rows REACH the function.** `venue_arming: &[VenueArming]` is a parameter the compiler
/// happily accepts an empty slice for, and an empty slice turns the whole correction off for every
/// venue at once — the single most valuable line in this file.
#[test]
fn the_production_call_site_passes_the_real_arming_rows() {
    let src = read_main();
    // `live_mount_with` computes them into `arming` and journals from the same binding.
    assert!(
        src.contains("let arming = vike_run::venue_arming(&vars, &mount_policy.venues);"),
        "the daemon must still compute its arming rows from the POST-withhold map"
    );
    let at = src
        .rfind("feeds.push(wire_venue_feeds(")
        .expect("the production call site must still exist");
    let call = &src[at..at + 400];
    let end = call.find(")?);").expect("the call must terminate");
    let call = &call[..end];
    assert!(
        call.contains("&arming,"),
        "the production `wire_venue_feeds` call must pass the real arming rows — `&[]` compiles and \
         silently restores the per-venue badge for every venue. Got:\n{call}"
    );
}

/// Each `let arming = …;` statement in `body`, whole.
fn arming_bindings(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("let arming = ") {
        let after = &rest[at..];
        let end = after.find(";\n").map_or(after.len(), |e| e + 1);
        out.push(after[..end].to_string());
        rest = &after[end..];
    }
    out
}

/// **The mutation self-test.** A text gate that cannot fail reads as coverage in review and is not.
/// This runs the real predicates over MUTATED copies of the text — an arm that calls its producer
/// directly, and a call site handing over an empty slice — and asserts both are REJECTED, then
/// asserts the real source is ACCEPTED.
///
/// Applied to the TEXT rather than to the file, so this proves the scanner without a build; the
/// end-to-end proof (mutate `main.rs`, watch the two tests above go red) is recorded in the PR.
#[test]
fn the_wrap_scanner_can_actually_fail() {
    // 1. The per-statement wrap predicate.
    let corrected = "let arming = with_other_live_accounts(\n    ig_arming(exec_live),\n    \
                     \"ig\",\n    other_live_accounts(\"ig\", venue_arming, live_venues),\n);";
    let mutated = "let arming = ig_arming(exec_live);";
    let wrapped = |s: &str| s.contains(WRAP) && s.contains("other_live_accounts(");
    assert!(wrapped(corrected), "the real shape must pass");
    assert!(!wrapped(mutated), "an unwrapped producer must be REJECTED");

    // 2. The call-site predicate.
    let real = "feeds.push(wire_venue_feeds(\n plan,\n &venue_cfgs,\n &handle,\n &live_venues,\n \
                &arming,\n &data_only,\n &wrap,\n make,\n";
    let emptied = real.replace("&arming,", "&[],");
    assert!(real.contains("&arming,"), "the real call site must pass");
    assert!(!emptied.contains("&arming,"), "an emptied call site must be REJECTED");

    // 3. …and the live source satisfies both, so the two tests above are not vacuous.
    let body = wire_venue_feeds_body(&read_feeds());
    let bindings = arming_bindings(&body);
    assert!(!bindings.is_empty(), "the scanner must actually find the bindings");
    assert!(bindings.iter().all(|s| wrapped(s)), "every real binding is corrected");
}
