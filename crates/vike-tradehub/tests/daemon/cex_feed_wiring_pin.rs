//! Pins `live_runtime`'s CEX FEED WIRING: each CEX arm must dial the venue its PLAN names, on both
//! lanes — never a venue spelled out in the arm.
//!
//! # Why this exists
//!
//! `live_runtime` is a `main.rs` function that spawns a core, opens two sockets and reads the
//! credential store, so no test can call it. The part that decides what a strategy actually RECEIVES —
//! WHICH venue each arm's dial names — is irreducibly a wiring question, and it is checked the way this
//! repo checks surface it cannot link against: a TEXT gate over the source, claiming nothing beyond what
//! text can show. `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs`,
//! `crates/vike-ops/tests/docs/kill_switch_gate.rs` and the sibling
//! `crates/vike-tradehub/tests/daemon/account_badge_wiring_pin.rs` are the same idiom.
//!
//! # What is pinned, and the failure it catches
//!
//! Before wave 2 four `match venue` dispatches each called their OWN bridge crate's constructor, and this
//! gate held "every arm names its own crate and no other" — the cross-wire that type-checks (okx's pump
//! inside bybit's arm) and streams real prices from the wrong venue. This process links no bridge now:
//! both CEX lanes are ONE dial to the datahub's strategy lane (`make.strat(slug, ..)`), and the venue is
//! the plan's own `CexVenue::slug()`, one definition for the dial, the engine and the account rows
//! (`cex_venue_slugs_are_pinned_verbatim_and_are_roster_venues` pins the slugs). The cross-wire that
//! survives is a venue SPELLED OUT in an arm: `make.strat("okx", ..)` inside the bybit start compiles,
//! connects and prices one venue's book against another's engine. So: the code of both lanes
//! (`wire_cex_kline_lane` and `spawn_cex_tick_pump`) must contain no CEX venue literal, and must take the
//! dial's venue from `slug`.
//!
//! [`the_scanner_can_actually_fail`] is the mutation self-test — an unmutated pin is the exact defect
//! this file was written for.

use std::path::{Path, PathBuf};

/// The file that owns both CEX lanes. Named literally: if they move again, this gate must be re-anchored
/// deliberately, never quietly satisfied.
const CEX: &str = "crates/vike-tradehub/src/feeds/cex.rs";

/// The venue words no lane may spell out.
const CEX_VENUES: [&str; 4] = ["binance", "bybit", "okx", "aster"];

// Same spelling as `crates/vike-ops/tests/common/repo.rs`'s `workspace_root` (keeps the `..`); the
// `parent()` twins, e.g. `crates/vike-catalog/tests/baseline_artifact.rs`'s `repo_root`, do not.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn read_cex() -> String {
    let path = workspace_root().join(CEX);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read `{CEX}`: {e}"))
}

/// The two lanes' CODE: from the kline lane's signature to the arming announcement's doc, comments
/// stripped so a `//` or `///` mention of a venue cannot spoof — or falsely trip — the check.
fn lanes(src: &str) -> String {
    for anchor in
        ["fn wire_cex_kline_lane(", "fn spawn_cex_tick_pump(", "/// The CEX arming disclosure"]
    {
        assert_eq!(
            src.matches(anchor).count(),
            1,
            "the anchor {anchor:?} must match {CEX} EXACTLY once, or this gate is reading a slice \
             nobody chose"
        );
    }
    let start = src.find("fn wire_cex_kline_lane(").expect("checked above");
    let end = src.find("/// The CEX arming disclosure").expect("checked above");
    assert!(start < end, "the lanes precede the arming announcement");
    src[start..end]
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The predicate: no venue literal, and the dial takes its venue from `slug`.
fn wired_from_the_plan(code: &str) -> Result<(), String> {
    if !code.contains("make.strat(slug,") {
        return Err("the kline lane must dial `make.strat(slug, ..)` — the plan's own slug".into());
    }
    for venue in CEX_VENUES {
        if code.contains(&format!("\"{venue}\"")) {
            return Err(format!(
                "a CEX lane spells the venue `\"{venue}\"` out: a cross-wired arm compiles, connects \
                 and looks healthy while mounting one venue's engine against another venue's book"
            ));
        }
    }
    Ok(())
}

/// Both lanes dial the venue the plan names and spell no venue out.
#[test]
fn every_cex_lane_dials_the_venue_its_plan_names() {
    let code = lanes(&read_cex());
    if let Err(why) = wired_from_the_plan(&code) {
        panic!("{why}\n{code}");
    }
}

/// **The mutation self-test.** Runs the real predicate over a cross-wired copy of the source — the
/// kline lane's dial rewritten to name okx — and asserts it is REJECTED, then asserts the unmutated source
/// is ACCEPTED. The mutation is applied to the text, so this proves the scanner without a build; the
/// end-to-end proof (mutate `feeds/cex.rs`, watch the test above go red) is recorded in the PR.
#[test]
fn the_scanner_can_actually_fail() {
    let code = lanes(&read_cex());
    assert!(
        wired_from_the_plan(&code).is_ok(),
        "the real source must PASS, or the mutation proves nothing"
    );

    let mutated = code.replace("make.strat(slug,", "make.strat(\"okx\",");
    assert_ne!(mutated, code, "the mutation must actually change the lane text");
    assert!(
        wired_from_the_plan(&mutated).is_err(),
        "a kline lane dialing okx for every start must be REJECTED — this is the whole point of the gate"
    );

    let lane2 = code.replace("subscribe_ticks(&cfg.token_id", "subscribe_ticks(\"bybit\"");
    assert_ne!(lane2, code, "the second mutation must change the text too");
    assert!(
        wired_from_the_plan(&lane2).is_err(),
        "a tick pump naming a venue literal must be REJECTED"
    );
}
