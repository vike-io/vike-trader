//! The ready banner (`ready_mode_line`): the one authority on paper-vs-live.

use super::*;

// ---------------------------------------------------------------------------------------------
// The ready banner (`ready_mode_line`) — the string `docs/ops/tradehub-the CI box.md` and
// `deploy/vike-tradehub.service` both call the ONE authority on paper-vs-live.
// ---------------------------------------------------------------------------------------------

/// A `live_venues` ARMING RECORD, spelled the way `vike_mount::build_node` hands one over.
fn armed(venues: &[&str]) -> std::collections::HashSet<String> {
    venues.iter().map(|v| (*v).to_string()).collect()
}

/// A store that ANSWERED — the arm an ABSENT store also lands in, so every assertion below
/// that uses it is asserting the banner an unconfigured box prints.
fn readable() -> vike_bridge_core::credentials::StoreHealth {
    vike_bridge_core::credentials::StoreHealth::Readable
}

/// A store that EXISTS and would not OPEN, carrying `SecretsError`'s own Display the way
/// `load_workspace_secrets_from_env_checked` hands one over.
fn unreadable() -> vike_bridge_core::credentials::StoreHealth {
    vike_bridge_core::credentials::StoreHealth::Unreadable(
        "credential store /x/settings/db/vike.db could not be read: a writer of the settings \
             database was killed mid-write"
            .to_string(),
    )
}

/// **THE DEFECT, reproduced from the measurement that found it.**
///
/// The record below is verbatim what the CI box's shipped daemon logged on one startup, and the
/// banner it printed two lines later was `LIVE (venue=bybit)` — the single venue its profile
/// mounted. Nine venues held live authenticated exec sessions and the operator's one authority
/// on paper-vs-live named one of them.
///
/// The expectation is written out LONGHAND rather than derived from the input, so the test
/// cannot agree with a renderer that has the same bug (the declaration-pinning failure this
/// repo has been bitten by three times).
#[test]
fn the_banner_names_every_armed_venue_not_the_one_the_profile_mounts() {
    let record = armed(&[
        "hyperliquid",
        "deribit",
        "okx",
        "bybit",
        "alpaca",
        "aster",
        "binance",
        "ig",
        "oanda",
    ]);
    assert_eq!(
        ready_mode_line(true, &record, &readable()),
        "LIVE (venue=alpaca+aster+binance+bybit+deribit+hyperliquid+ig+oanda+okx)"
    );
    // ...and the shape the defect actually printed is now impossible from this record. Spelled
    // separately because a renderer that named only the FIRST armed venue would satisfy no
    // equality above but would still be the same class of lie.
    assert_ne!(ready_mode_line(true, &record, &readable()), "LIVE (venue=bybit)");
}

/// Sorted, so an operator diffing two startups of ONE binary never sees a reordering and reads
/// it as a change: `HashSet` iteration order is not a function of the contents alone.
///
/// Two records built by inserting the same venues in OPPOSITE orders must render identically —
/// and must render in the order written here, which is neither insertion order.
#[test]
fn the_banner_sorts_the_arming_record_rather_than_iterating_it() {
    let forwards = armed(&["okx", "binance", "aster"]);
    let backwards = armed(&["aster", "binance", "okx"]);
    assert_eq!(ready_mode_line(true, &forwards, &readable()), "LIVE (venue=aster+binance+okx)");
    assert_eq!(
        ready_mode_line(true, &forwards, &readable()),
        ready_mode_line(true, &backwards, &readable())
    );
}

/// The LIVE GATE ON with NOTHING ARMED — the empty-credential-store shape every
/// `venue_feed_splice_smoke` case runs the shipped binary in, and the shape a fresh deployment
/// has before its first key is added.
///
/// It must name the sentinel, and it must NOT collapse to `PAPER`: the gate being on is an
/// operator-visible fact independent of what armed (live feeds, real prices, the B11
/// live-account locks held, and one credential appearing arms real exec on the next start).
#[test]
fn an_armed_gate_with_nothing_armed_reads_none_and_not_paper() {
    let mode = ready_mode_line(true, &armed(&[]), &readable());
    assert_eq!(mode, "LIVE (venue=none)");
    assert_ne!(mode, "PAPER", "the live gate is ON — collapsing to PAPER would hide the arm");
    // Not a truncated `LIVE (venue=)`, which reads as a broken line rather than a statement.
    assert!(!mode.ends_with("venue=)"), "the empty set must render a WORD: {mode}");
}

/// The PAPER arm is byte-identical to the pre-fix daemon — the string, and nothing else.
///
/// Asserted against a NON-EMPTY record too: a paper mount builds no live client by any path, so
/// this pairing cannot occur, and the test exists to pin that the renderer answers from the GATE
/// on that arm rather than falling through to the venue rendering if it ever did.
#[test]
fn the_paper_arm_is_untouched() {
    assert_eq!(ready_mode_line(false, &armed(&[]), &readable()), "PAPER");
    assert_eq!(ready_mode_line(false, &armed(&["binance", "bybit"]), &readable()), "PAPER");
}

/// ⚠ **THE BUG, as a string comparison.** A box whose credential store exists and will not open
/// arms nothing, so the arming record is EMPTY — identical to a correctly-unarmed box's. Under
/// the pre-fix renderer both printed `LIVE (venue=none)`, and that was the whole of the
/// operator's ability to tell a live daemon that lost its keys from one that never had any.
///
/// Asserted as an INEQUALITY against the correctly-unarmed line rather than only as an equality
/// against the new one: an equality alone would still pass on a renderer that appended
/// something to BOTH arms, which would restore the indistinguishability it is meant to remove.
#[test]
fn an_unreadable_store_is_not_the_same_banner_as_a_correctly_unarmed_box() {
    let unarmed = ready_mode_line(true, &armed(&[]), &readable());
    let broken = ready_mode_line(true, &armed(&[]), &unreadable());
    assert_eq!(unarmed, "LIVE (venue=none)", "the correctly-unarmed box's banner is unchanged");
    assert_ne!(
        broken, unarmed,
        "a store that EXISTS and will not open printed the same banner as a box with no store \
             — which is the defect: every venue is on paper for a reason nobody can see"
    );
    assert_eq!(broken, "CREDENTIAL STORE UNREADABLE — LIVE (venue=none)");
}

/// The prefix is a PREFIX: the paper-vs-live half survives verbatim in both arms, so every
/// existing `grep PAPER` / `grep LIVE` and every runbook keeps answering.
#[test]
fn the_fault_is_announced_without_taking_the_paper_vs_live_answer_away() {
    let live = ready_mode_line(true, &armed(&["binance"]), &unreadable());
    let paper = ready_mode_line(false, &armed(&[]), &unreadable());
    assert!(live.starts_with(STORE_UNREADABLE_BANNER), "{live}");
    assert!(paper.starts_with(STORE_UNREADABLE_BANNER), "{paper}");
    assert!(live.ends_with("LIVE (venue=binance)"), "the live half must survive: {live}");
    assert!(paper.ends_with("PAPER"), "the paper half must survive: {paper}");
}

/// ⚠ **An ABSENT store is BYTE-IDENTICAL to before this parameter existed**, and that is a
/// requirement rather than a side effect: no store on the box is the ordinary unconfigured
/// state, the empty map it produces is a real measurement, and `StoreHealth::Readable` is the
/// arm it lands in. A daemon on a fresh box must print exactly what it printed yesterday.
#[test]
fn a_box_with_no_store_at_all_prints_exactly_what_it_always_did() {
    assert_eq!(ready_mode_line(false, &armed(&[]), &readable()), "PAPER");
    assert_eq!(ready_mode_line(true, &armed(&[]), &readable()), "LIVE (venue=none)");
    assert_eq!(
        ready_mode_line(true, &armed(&["okx", "bybit"]), &readable()),
        "LIVE (venue=bybit+okx)"
    );
}

/// The empty-set sentinel sits in a field whose every other value is a venue id, so it must not
/// be capable of being one.
///
/// `vike_model::VENUES` is DERIVED from the `crates/bridges/*` tree (its own roster test walks
/// the directory), so a future bridge crate named `none` reddens here rather than silently
/// making the banner ambiguous between "nothing armed" and "the `none` venue armed".
#[test]
fn banner_sentinel_is_not_a_venue_id() {
    assert!(
        !vike_model::VENUES.contains(&NO_VENUE_ARMED),
        "`{NO_VENUE_ARMED}` is now a venue id — the ready banner's empty-set sentinel must be \
             renamed to something the roster cannot contain"
    );
}

/// A seed per mount, venue-addressed the way `run` builds them.
fn seeds(venues: &[&str]) -> Vec<WireMountSeed> {
    venues
        .iter()
        .map(|v| WireMountSeed {
            strategy: "buy_hold".to_string(),
            params: format!("venue={v} symbol=X interval=1m :: size=1"),
            venue: (*v).to_string(),
            asset_class: Some("CryptoPerp".to_string()),
        })
        .collect()
}

/// The `StrategyStatus` row's `live` is a PER-VENUE fact, and the two ways it used to
/// over-claim are both asserted here rather than described.
///
/// One armed venue and two unarmed mounts in one daemon: the row set must SPLIT. Under the old
/// `live: flags.tradehub_live` all three read `true`, which is a claim that three mounts place
/// real orders when one does.
#[test]
fn a_mount_row_is_live_only_when_its_own_venue_armed() {
    let rows = wire_mount_rows(seeds(&["bybit", "okx", "oanda"]), &armed(&["bybit"]));
    let by_venue: Vec<bool> = rows.iter().map(|r| r.live).collect();
    assert_eq!(
        by_venue,
        vec![true, false, false],
        "only the armed venue's mount trades live; rows: {rows:?}"
    );
}

/// The `data_only = true` mount — the case the profile loader guarantees is REACHABLE, because
/// it refuses that key unless the live gate is on.
///
/// `withhold_venue_credentials` strips the venue's keys before `build_node`, so the venue never
/// enters the arming record even though its FEED is credentialed and live. The row must read
/// paper: its orders go to the paper book.
#[test]
fn a_data_only_mounts_row_reads_paper_even_though_its_feed_is_credentialed() {
    let rows = wire_mount_rows(seeds(&["oanda"]), &armed(&[]));
    assert!(!rows[0].live, "a withheld venue mounts paper exec, and the row must say so");
    // ...and the row is otherwise untouched — this fix changes ONE field.
    assert_eq!(rows[0].strategy, "buy_hold");
    assert_eq!(rows[0].params, "venue=oanda symbol=X interval=1m :: size=1");
}

/// A PAPER daemon hands over an empty record, so every row reads paper — byte-identical to the
/// pre-fix answer on that arm, where `flags.tradehub_live` was `false` for the same rows.
#[test]
fn every_row_of_a_paper_daemon_reads_paper() {
    let rows = wire_mount_rows(seeds(&["bybit", "oanda"]), &armed(&[]));
    assert!(rows.iter().all(|r| !r.live), "rows: {rows:?}");
}
