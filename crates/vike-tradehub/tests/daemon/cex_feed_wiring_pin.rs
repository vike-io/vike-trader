//! Pins `live_mount`'s CEX FEED WIRING: each `CexVenue` arm must call its OWN bridge crate, on both
//! lanes.
//!
//! # Why this exists
//!
//! `live_mount` is a `main.rs` function that spawns a core, opens two sockets and reads the
//! credential store, so no test can call it. Before this file, `grep "live_mount(" crates/vike-tradehub`
//! found exactly one production caller and one string literal inside `live_wired_venues_pin.rs`'
//! own fixture — the whole feed-wiring block, the part that decides what a strategy actually
//! RECEIVES, was unexercised.
//!
//! Its decisions have since been split out and unit-tested where they could be (`cex_arming` and
//! `cex_plan`, in `main.rs`' own test module). What is left is irreducibly a wiring question — WHICH
//! crate's constructor each arm calls — and it is checked the way this repo already checks
//! `main.rs` surface it cannot link against: a TEXT gate over the source, claiming nothing beyond
//! what text can show. `crates/vike-ops/tests/graceful_stop_pin.rs`,
//! `crates/vike-ops/tests/kill_switch_gate.rs` and the sibling `live_wired_venues_pin.rs` are the
//! same idiom.
//!
//! # What is pinned, and the failure it catches
//!
//! Two `match venue` dispatches sit inside the CEX arm — the kline `Feeds` (LANE 1) and the
//! `spawn_*_market_data` tick pump (LANE 2) — and each has one arm per venue, all three shaped
//! identically bar the crate name. That is precisely the shape a copy-paste slips through.
//!
//! ⚠ **Be precise about what the COMPILER already covers, because it covers half of this and not
//! the dangerous half.** `CexTicks`/`CexBars` variants carry venue-SPECIFIC types, so putting okx's
//! pump inside `CexTicks::Bybit(..)` is a type error and needs no gate. What type-checks perfectly
//! is a mismatched SCRUTINEE — the arm that reads
//!
//! ```text
//! CexVenue::Bybit => CexTicks::Okx(vike_okx::market_data::spawn_okx_market_data(..))
//! ```
//!
//! because the match's result type admits any variant. MEASURED: that edit compiles clean (`cargo
//! check -p vike-tradehub`, zero diagnostics) and, before this file, 161/161 vike-tradehub tests
//! still passed. Live it subscribes OKX's book onto a bybit engine — and it also silently re-keys
//! the reconcile health map, since `CexBars::slug` derives from the VARIANT, so bybit's reconcile
//! passes start gating on a feed status nothing publishes.
//!
//! Nothing else in the tree can see it. `CexVenue::slug` is pinned in `main.rs`, but the slug and
//! the constructor are chosen at two different sites, and every test that builds a `CexBars` builds
//! it through its own helper rather than through `live_mount`.
//!
//! So: for every venue, BOTH of that venue's arms must name that venue's crate, and NEITHER may
//! name another CEX crate.
//!
//! [`the_arm_scanner_can_actually_fail`] is the mutation self-test — an unmutated pin is the exact
//! defect this file was written for.

use std::path::{Path, PathBuf};

/// The file that owns `wire_venue_feeds` — `live_mount`'s per-venue feed-construction arm, and
/// home to both dispatches this file scans. Named literally: if the function moves again, this
/// gate must be re-anchored deliberately, never quietly satisfied.
///
/// ⚠ This used to be `main.rs`; the tradehub-main-split refactor's Task 3 moved `wire_venue_feeds`
/// (and both `match venue { ... }` dispatches inside it) out of `main.rs` into this crate's
/// LIBRARY as `feeds.rs`. `live_mount` itself stayed in `main.rs` and calls in through
/// `use vike_tradehub::feeds::wire_venue_feeds`.
const MAIN: &str = "crates/vike-tradehub/src/feeds.rs";

/// The two feed dispatches, each anchored on the statement that OPENS it. Both anchors are asserted
/// unique below rather than assumed — a non-unique anchor makes every assertion here inspect a slice
/// nobody chose.
const DISPATCHES: [(&str, &str); 2] =
    [("let mut bars = match venue {", "klines"), ("let pump = match venue {", "tick pump")];

/// `(variant, the crate path its arms must name)`. The crate is the AUTHORITY: `CexVenue::Binance`
/// is the binance venue precisely because its feed arms call `vike_binance`.
///
/// ⚠ Aster's row is the sharpest one here: the venue IS a binance fork whose feed modules delegate
/// to `vike_binance::family` internally, so an arm cross-wired to call `vike_binance::` directly
/// would connect, stream real prices and look healthy — while keying the wrong venue's engine,
/// slug and reconcile row. The arm must go through `vike_aster::`, whose wrappers supply aster's
/// own hosts and venue string.
const VENUES: [(&str, &str); 4] = [
    ("CexVenue::Binance", "vike_binance::"),
    ("CexVenue::Bybit", "vike_bybit::"),
    ("CexVenue::Okx", "vike_okx::"),
    ("CexVenue::Aster", "vike_aster::"),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read_main() -> String {
    let path = workspace_root().join(MAIN);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{MAIN}`: {e}"))
}

/// The body of one `match venue { … }` dispatch: everything from the opening anchor to the closing
/// `\n            };`, which is the indentation the dispatch is written at inside the CEX arm.
fn dispatch_body<'a>(src: &'a str, anchor: &str) -> &'a str {
    assert_eq!(
        src.matches(anchor).count(),
        1,
        "the anchor {anchor:?} must match {MAIN} EXACTLY once, or this gate is reading a slice \
         nobody chose — shorten/lengthen it until it does"
    );
    let at = src.find(anchor).expect("checked above");
    let rest = &src[at..];
    let end = rest
        .find("\n            };")
        .unwrap_or_else(|| panic!("{anchor:?} has no closing `}};` at the CEX arm's indentation"));
    &rest[..end]
}

/// One `CexVenue::X =>` arm's text, from the variant to the start of the next variant (or the end
/// of the dispatch). Comments are stripped so a `//` mention of another venue cannot spoof — or
/// falsely trip — the checks.
fn arm(body: &str, variant: &str) -> String {
    let at = body
        .find(variant)
        .unwrap_or_else(|| panic!("the dispatch has no `{variant}` arm:\n{body}"));
    let after = &body[at + variant.len()..];
    let end = VENUES.iter().filter_map(|(v, _)| after.find(v)).min().unwrap_or(after.len());
    after[..end].lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
}

/// Every venue's arm, on BOTH lanes, calls that venue's OWN crate — and no other CEX crate.
#[test]
fn every_cex_arm_wires_its_own_bridge_crate() {
    let src = read_main();
    for (anchor, lane) in DISPATCHES {
        let body = dispatch_body(&src, anchor);
        for (variant, own_crate) in VENUES {
            let text = arm(body, variant);
            assert!(
                text.contains(own_crate),
                "the {lane} arm for {variant} must call {own_crate} — it is the crate that MAKES \
                 this venue that venue. Got:\n{text}"
            );
            for (other, other_crate) in VENUES {
                if other == variant {
                    continue;
                }
                assert!(
                    !text.contains(other_crate),
                    "the {lane} arm for {variant} also names {other_crate}: a cross-wired arm \
                     compiles, connects and looks healthy while mounting one venue's engine \
                     against another venue's book. Got:\n{text}"
                );
            }
        }
    }
}

/// **The mutation self-test.** A text gate that cannot fail is worse than no gate — it reads as
/// coverage in review. This runs the real predicate over a cross-wired copy of the source — bybit's
/// pump arm rewritten to build the okx pump, which is the type-correct, compiles-clean mutation the
/// module doc measured — and asserts it is REJECTED, then asserts the unmutated source is ACCEPTED.
///
/// The mutation is applied to the arm TEXT rather than to the file, so this test proves the scanner
/// without a build. The end-to-end proof (mutate `main.rs`, watch
/// [`every_cex_arm_wires_its_own_bridge_crate`] go red) is recorded in the PR.
#[test]
fn the_arm_scanner_can_actually_fail() {
    let src = read_main();

    // The predicate, factored so the mutation runs the SAME code the real test does.
    let cross_wired = |text: &str, own: &str, others: [&str; 3]| {
        text.contains(own) && !others.iter().any(|o| text.contains(o))
    };

    let body = dispatch_body(&src, DISPATCHES[1].0);
    let bybit = arm(body, "CexVenue::Bybit");
    assert!(
        cross_wired(&bybit, "vike_bybit::", ["vike_binance::", "vike_okx::", "vike_aster::"]),
        "the real source must PASS the predicate, or the mutation below proves nothing:\n{bybit}"
    );

    let mutated = bybit.replace(
        "vike_bybit::market_data::spawn_bybit_market_data",
        "vike_okx::market_data::spawn_okx_market_data",
    );
    assert_ne!(mutated, bybit, "the mutation must actually change the arm text");
    assert!(
        !cross_wired(&mutated, "vike_bybit::", ["vike_binance::", "vike_okx::", "vike_aster::"]),
        "a bybit arm calling the OKX pump must be REJECTED — this is the whole point of the \
         gate:\n{mutated}"
    );

    // …and the FORK direction: an aster arm rewritten onto its template crate — the cross-wire
    // that would still stream real (binance) prices — must be rejected the same way.
    let aster = arm(body, "CexVenue::Aster");
    assert!(
        cross_wired(&aster, "vike_aster::", ["vike_binance::", "vike_okx::", "vike_bybit::"]),
        "the real aster arm must PASS the predicate:\n{aster}"
    );
    let mutated = aster.replace(
        "vike_aster::market_data::spawn_aster_market_data",
        "vike_binance::market_data::spawn_binance_market_data",
    );
    assert_ne!(mutated, aster, "the mutation must actually change the arm text");
    assert!(
        !cross_wired(&mutated, "vike_aster::", ["vike_binance::", "vike_okx::", "vike_bybit::"]),
        "an aster arm calling the BINANCE pump must be REJECTED — the fork cross-wire looks the \
         most plausible in review:\n{mutated}"
    );
}
