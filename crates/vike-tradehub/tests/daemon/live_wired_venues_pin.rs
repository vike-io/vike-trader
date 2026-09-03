//! Pins [`vike_tradehub::config::LIVE_WIRED_VENUES`] against `live_mount`'s ACTUAL venue arms —
//! which live in `venue_feed_plan`, the per-mount planning helper `live_mount` loops over the
//! mount set since split-plane I10 (the ONE `match cfg.venue.as_str()` dispatch, wherever `MAIN`
//! names; the scan below anchors on the match statement itself, so a further move of
//! `venue_feed_plan` costs only an update to `MAIN`, not to the scan). ⚠ `MAIN` no longer names
//! the same file `live_mount` itself lives in: the tradehub-main-split refactor's Task 3 moved
//! `venue_feed_plan` out of `main.rs` into this crate's library as `crates/vike-tradehub/src/
//! feeds.rs`, and `MAIN` was re-anchored there with it — see that constant's own doc.
//!
//! # Why this exists — a pin that was written and never mutated
//!
//! `LIVE_WIRED_VENUES`' own doc used to say "extending it is a deliberate two-place edit: a row HERE
//! and the matching `live_mount` arm — `live_wired_venues_are_all_mounted_by_build_node` pins the
//! containment." It does not. That test checks this list against `vike_run::WIRED_MARKETS`, the
//! table of every venue **`build_node` mounts an ENGINE for** — which contained binance, bybit, okx,
//! deribit, aster, alpaca, ctrader, ig and oanda when this was written, none of which `live_mount`
//! had a market-FEED arm for at the time. Several have since gained one (binance/bybit/okx first,
//! then aster/alpaca/ctrader/oanda through split-plane I9), and the ones that have not are still
//! engine-only — but this file deliberately does NOT name which is which any more: that IS the set
//! the gate below computes, and a prose copy of it went stale within days of being written both
//! times it was attempted. Run the gate to see the answer.
//! Its converse, `every_unwired_venue_is_still_refused`, `continue`s on any venue already in
//! this list. So neither direction can see a venue added here alone: adding `"binance"` was MEASURED
//! green across the entire vike-tradehub suite.
//!
//! The previous hardcoded `(venue, symbol)` allow-list forced a match-arm edit that the completeness
//! test caught. A one-line const does not — the "declaration-pinning tests don't gate" failure mode
//! this repo has already been bitten by three times. This file is the missing half, built the way
//! `crates/vike-ops/tests/graceful_stop_pin.rs` already reads that same `main.rs`: a TEXT gate over
//! the source, claiming nothing beyond what text can show.
//!
//! # What is pinned
//!
//! Set EQUALITY, both directions, over the arms of `live_mount`'s venue `match`:
//!
//!   - a row here with no arm ⇒ the daemon advertises a venue whose live feed nothing subscribes.
//!     `live_mount`'s `v => return Err(…)` catch-all still makes that a loud startup failure rather
//!     than a silent live mount — which is why the round-2 review called this `needs_changes` and
//!     not `do_not_merge` — but "the operator finds out at 3am" is not the gate the doc promised.
//!   - an arm with no row ⇒ `validate_for_live` refuses a venue the daemon can actually mount, so a
//!     legitimate profile is rejected and the arm is dead code nobody notices.
//!
//! Feature-aware in both directions: the `polymarket` arm and the `polymarket` row are each behind
//! `#[cfg(feature = "polymarket")]`, so the scan resolves each arm's cfg against THIS build rather
//! than comparing a default build's const against a source file that lists every arm.
//!
//! [`the_arm_scanner_can_actually_fail`] is the mutation self-test. It is not optional here: an
//! unmutated pin is the exact defect this file was written to repair.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_tradehub::config::LIVE_WIRED_VENUES;

/// The file that owns `venue_feed_plan` — `live_mount`'s per-mount venue dispatch, and the ONE
/// function this whole gate is actually about. Named literally: if the function moves again, this
/// gate must be re-anchored deliberately, never quietly satisfied.
///
/// ⚠ This used to be `main.rs`, back when `venue_feed_plan` was a private function `live_mount`
/// (also in `main.rs`) called inline. The tradehub-main-split refactor's Task 3 moved
/// `venue_feed_plan` (plus `wire_venue_feeds`, `recon_feed_statuses_of` and the feed-object types
/// they name) into this crate's LIBRARY as `feeds.rs`, so a bin/lib crate boundary now separates
/// `venue_feed_plan` from `live_mount` — `live_mount` stayed in `main.rs` and calls in through
/// `use vike_tradehub::feeds::venue_feed_plan`. `MAIN` moved with the function it names, not with
/// `live_mount`.
const MAIN: &str = "crates/vike-tradehub/src/feeds.rs";

/// The statement that opens the venue dispatch (in `venue_feed_plan`, `live_mount`'s per-mount
/// planning helper since I10). It is the ONE `match` in that file keyed on a mount's venue, and
/// every live feed the daemon can wire hangs off it.
const VENUE_MATCH: &str = "match cfg.venue.as_str() {";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{rel}`: {e}"))
}

/// The venue arms of `live_mount`'s dispatch that are COMPILED IN THIS BUILD.
///
/// Pure over the source text so [`the_arm_scanner_can_actually_fail`] can feed it known cases. The
/// slice runs from [`VENUE_MATCH`] to its brace-matched close; inside it, a line of the shape
/// `"venue" => {` is an arm, and an arm is included only when the `#[cfg(feature = "…")]` on the
/// line above it (if any) is satisfied by `enabled`. Whole-line `//` comments are dropped first —
/// `live_mount`'s arms carry long prose that quotes venue names.
fn venue_arms(raw: &str, enabled: &[&str]) -> BTreeSet<String> {
    // Whole-line `//` comments go FIRST, before the brace match — `live_mount`'s arms carry long
    // prose, and one comment containing an unbalanced `{` would otherwise walk the scan off the end
    // of the dispatch and silently return an empty (vacuously equal) set.
    let src: String =
        raw.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let src = src.as_str();
    let Some(start) = src.find(VENUE_MATCH) else {
        panic!(
            "`{MAIN}` no longer contains `{VENUE_MATCH}` — `live_mount`'s venue dispatch moved or \
             was renamed. Re-anchor this gate on whatever decides the live venue set now; do NOT \
             delete it, or the two-place edit stops being enforced again."
        )
    };
    // Brace-match from the `{` that opens the match body.
    let body_start = start + VENUE_MATCH.len() - 1;
    let bytes = src.as_bytes();
    let mut depth = 0usize;
    let mut end = body_start;
    for (i, b) in bytes.iter().enumerate().skip(body_start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &src[body_start..=end];

    let mut arms = BTreeSet::new();
    let mut pending_cfg: Option<String> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#[cfg(feature = \"") {
            pending_cfg = rest.split('"').next().map(str::to_string);
            continue;
        }
        // `"hyperliquid" => {`
        if let Some(rest) = line.strip_prefix('"') {
            if let Some((venue, tail)) = rest.split_once('"') {
                if tail.trim_start().starts_with("=>") {
                    let gated = pending_cfg.take();
                    if gated.as_deref().is_none_or(|f| enabled.contains(&f)) {
                        arms.insert(venue.to_string());
                    }
                    continue;
                }
            }
        }
        // Any other non-blank line ends the attribute's reach (an attribute binds to what follows).
        if !line.is_empty() {
            pending_cfg = None;
        }
    }
    arms
}

/// The cargo features this test binary was built with, as the scanner's `enabled` set.
fn enabled_features() -> Vec<&'static str> {
    let mut f = Vec::new();
    if cfg!(feature = "polymarket") {
        f.push("polymarket");
    }
    f
}

/// The gate: the advertised venue list and the mountable venue arms are the SAME SET.
#[test]
fn live_wired_venues_are_exactly_live_mounts_feed_arms() {
    let arms = venue_arms(&read(MAIN), &enabled_features());
    let advertised: BTreeSet<String> = LIVE_WIRED_VENUES.iter().map(|v| v.to_string()).collect();
    assert_eq!(
        advertised,
        arms,
        "`LIVE_WIRED_VENUES` and `live_mount`'s venue arms disagree.\n  advertised but NOT \
         mounted: {:?}\n  mounted but NOT advertised: {:?}\nExtending the daemon to a venue is a \
         two-place edit — a row in `crates/vike-tradehub/src/config.rs`'s `LIVE_WIRED_VENUES` AND a \
         feed arm in `{MAIN}`'s `venue_feed_plan`. A row alone advertises a venue whose live feed \
         nothing subscribes (the catch-all then fails the daemon at startup, in front of an \
         operator, instead of here); an arm alone is refused by `validate_for_live` and is dead \
         code.",
        advertised.difference(&arms).collect::<Vec<_>>(),
        arms.difference(&advertised).collect::<Vec<_>>(),
    );
}

/// The floor: the scan really found the dispatch and really found arms in it, so the equality above
/// cannot be satisfied by two empty sets — the vacuous-gate shape that produced this finding.
#[test]
fn the_pin_has_a_non_empty_input() {
    let src = read(MAIN);
    assert!(src.len() > 1_000, "`{MAIN}` is too short to hold `venue_feed_plan`'s dispatch");
    assert!(src.contains("fn venue_feed_plan"), "`{MAIN}` no longer defines `venue_feed_plan`");
    let arms = venue_arms(&src, &enabled_features());
    assert!(
        arms.contains("hyperliquid"),
        "the scan found no `hyperliquid` arm in `venue_feed_plan` — it is the daemon's one \
         always-compiled live venue, so its absence means the scan is reading the wrong block, not \
         that the arm is gone: {arms:?}"
    );
    assert!(
        !LIVE_WIRED_VENUES.is_empty(),
        "`LIVE_WIRED_VENUES` is empty — this gate would compare nothing to nothing"
    );
}

/// The mutation self-test. [`venue_arms`] is pure, so prove it says YES to a real arm, NO to the
/// things `live_mount`'s prose-heavy body would otherwise produce, and that it respects the feature
/// gate in BOTH directions. Without this the equality above could be vacuously green forever, which
/// is precisely what happened to the claim this file replaces.
#[test]
fn the_arm_scanner_can_actually_fail() {
    let src = format!(
        "fn live_mount() {{\n    let plan = {VENUE_MATCH}\n        \
         // a comment naming \"binance\" => {{ }} in prose\n        \
         \"hyperliquid\" => {{ VenuePlan::Hyperliquid }}\n        \
         #[cfg(feature = \"polymarket\")]\n        \
         \"polymarket\" => {{ VenuePlan::Polymarket }}\n        \
         v => {{ return Err(format!(\"venue '{{v}}' is not wired\")) }}\n    }};\n}}\n"
    );

    let none = venue_arms(&src, &[]);
    assert_eq!(
        none,
        BTreeSet::from(["hyperliquid".to_string()]),
        "a default build must see the ungated arm ONLY — got {none:?}"
    );

    let poly = venue_arms(&src, &["polymarket"]);
    assert_eq!(
        poly,
        BTreeSet::from(["hyperliquid".to_string(), "polymarket".to_string()]),
        "a `polymarket` build must ALSO see the gated arm — got {poly:?}"
    );

    // A venue named only in a COMMENT is not an arm (this body is full of prose that names venues).
    assert!(!none.contains("binance"), "a venue named in a comment must not read as an arm");

    // ...and an arm ADDED to the source really moves the answer — the mutation the review ran by
    // hand against `LIVE_WIRED_VENUES`, run here against the scanner instead.
    let widened = src.replace(
        "\"hyperliquid\" => {",
        "\"binance\" => { VenuePlan::Binance }\n        \"hyperliquid\" => {",
    );
    assert!(
        venue_arms(&widened, &[]).contains("binance"),
        "the scanner must SEE a newly added arm, or this gate can never notice one side changing"
    );
}
