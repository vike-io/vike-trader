//! The DERIBIT feed arm.

use super::*;

// ── DERIBIT: the widest feed arm, and the one with NO credential gate ─────────────────────

fn deribit_cfg() -> MakerConfig {
    wired_1m_cfg("deribit", 0.5, 10.0)
}

/// deribit is joined to the exec plane the same way every other live-wired slug is: an engine
/// row in `vike_tradehub_core::wired_markets` AND a `FEED_ARMS` row.
#[test]
fn deribit_names_a_wired_market_and_a_live_wired_row() {
    assert_wired_and_advertised("deribit");
}

/// **The deribit plan takes NO credentials and refuses none** — the property that separates
/// this arm from its three credentialed-data neighbours, asserted in the only way that cannot
/// rot: the plan function takes no vars map at all, and `venue_feed_plan` reaches it with an
/// EMPTY one and still succeeds. Its feed is keyless public MAINNET, so absent keys are the
/// ordinary unconfigured state and leave a working feed over a paper book.
#[test]
fn deribit_plan_needs_no_credentials_and_starts_on_an_empty_store() {
    match deribit_plan(&deribit_cfg()) {
        Ok(VenuePlan::Deribit) => {}
        other => panic!(
            "the wired deribit pair must plan with no credential map in sight, got {other:?}"
        ),
    }
    // …and the venue really is one `venue_feed_plan` reaches with an empty store, which is the
    // end-to-end statement of the same fact (the three credentialed arms cannot do this).
    assert!(
        // The all-paper default policy is inert here — this call exercises the deribit arm, which
        // never reads it.
        venue_feed_plan(
            &deribit_cfg(),
            &vike_tradehub_core::ArmingTable::default(),
            &vike_tradehub_core::AdapterPolicy::default()
        )
        .is_ok(),
        "an empty credential map must still plan a deribit live mount — a keyless feed has no \
             refusal to make, and inventing one would be the failure this arm exists not to copy"
    );
}

/// The deribit-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares) and a degenerate tick size. The interval refusal that was this venue's own — an interval the
/// chart channel has no RESOLUTION code for — LEFT with the bridge table it read; it is not the bridge's
/// at `subscribe_bars` either (the feed thread sets a status line and exits), which is a recorded DOUBT
/// (`feed_splice/credentialed.rs`'s `an_interval_only_a_bridge_table_can_judge_...` pins the move).
#[test]
fn deribit_plan_refuses_a_foreign_symbol_and_a_degenerate_tick() {
    let mut foreign = deribit_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = deribit_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol assemble_runtime mounts: {err}");

    let mut bad_tick = deribit_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        deribit_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The deribit arming discloses the TESTNET exec network, the MAINNET quote source, and the
/// full requote pair.** All three are facts rather than style: `make_engine` arms only a `demo`
/// account and the bridge's authed sockets are hardcoded testnet while every public read is
/// hardcoded mainnet, and `venue_caps::DERIBIT` declares `book: true`, which this arm really
/// subscribes. The PAPER remedy is the CEX "add the keys" one — reachable, unlike the
/// credentialed arms' "report a bug" — and must NOT invent a `{VENUE}_MAINNET` flag, which
/// this venue does not have.
#[test]
fn deribit_arming_discloses_testnet_exec_over_mainnet_prices() {
    let live = deribit_arming(true);
    assert_eq!(live.exec, "LIVE");
    assert_eq!(live.network, "TESTNET", "every authed deribit socket is hardcoded testnet");
    assert_eq!(
        live.requote_lanes, "on_quote_tick + on_order_book",
        "this venue declares `book: true` and the arm subscribes the lossless book lane"
    );
    assert!(
        live.quote_source.contains("MAINNET"),
        "the feed reads a DIFFERENT network from exec — a disclosure that hid that would be a \
             half-truth: {}",
        live.quote_source
    );
    assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

    // The caps row is the authority for the lane claim above, read rather than restated.
    let caps = vike_venues::caps_for("deribit");
    assert!(caps.live_data.book, "the `on_order_book` half of the claim comes from this row");
    // The venue now serves the conflating DOM lane too (the datahub mounts it, 2026-10-04), which
    // is a fact about the VENUE: this arm still subscribes only the lossless book, because a depth
    // subscription emits `l2_snapshot` into sinks this daemon owns that drop it.
    assert!(caps.live_data.depth, "…and the venue serves the conflating DOM lane as well");

    let paper = deribit_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("API_KEY + API_SECRET")
            && paper.contains("vike-cli secrets set --venue deribit <FIELD>")
            && paper.contains("vike-cli secrets account add --venue deribit --tier demo"),
        "the keyless-feed venue's remedy IS reachable and must name the exec account's fields, \
             the verb that writes them and the one that creates the account: {paper}"
    );
    assert!(!paper.contains("DERIBIT_DEMO"), "no composed credential name: {paper}");
    assert!(
        !paper.contains("MAINNET=1"),
        "deribit's network is never the ceiling (its bridge's mount binds testnet, and vike-tradehub-core's \
             `the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing` pins it) — \
             advising a flag that is read nowhere is the unreachable-advice defect \
             `CexArming::remedy` documents: {paper}"
    );
}
