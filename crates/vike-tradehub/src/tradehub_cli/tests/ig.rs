//! The IG feed arm.

use super::*;
use std::assert_matches;

// ── IG: the narrowest feed arm — two verbs, and the absence is structural ─────────────────

fn ig_cfg() -> MakerConfig {
    wired_1m_cfg("ig", 0.1, 1.0)
}

/// ig is joined to the exec plane the same way every other live-wired slug is: an engine row
/// in `vike_tradehub_core::wired_markets` AND a `FEED_ARMS` row.
#[test]
fn ig_names_a_wired_market_and_a_live_wired_row() {
    assert_wired_and_advertised("ig");
}

/// **The ig plan accepts the wired pair and carries nothing**: the plan used to carry the resolved DEMO
/// login (api key, identifier, password; the Demo tier resolved the DEMO dealing gateway every
/// per-subscription `IgSession::login` used); the feed is a credentialed stream at the gatewayhub now,
/// which resolves the account, the tier and the gateway itself.
#[test]
fn ig_plan_accepts_the_wired_pair_and_carries_nothing() {
    assert_matches!(ig_plan(&ig_cfg()), Ok(VenuePlan::Ig));
}

/// The ig-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares) and a degenerate tick size. The interval refusal that was this venue's own — an interval
/// Lightstreamer has no chart SCALE for — is the bridge's now
/// (`feed_splice/credentialed.rs`'s `an_interval_only_a_bridge_table_can_judge_...` pins the move).
#[test]
fn ig_plan_refuses_a_foreign_symbol_and_a_degenerate_tick() {
    let mut foreign = ig_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}.NOT.THE.MOUNTED.ONE");
    let err = ig_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the epic assemble_runtime mounts: {err}");

    let mut bad_tick = ig_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        ig_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The ig arming discloses the DEMO gateway and a QUOTE-ONLY requote lane, and the second
/// half is structural.** `venue_caps::IG` declares no trade tape and no book because a DEALER
/// venue publishes neither — the same fact that keeps IG DEFERRED in
/// `market_data_conformance.rs` — so claiming the CEX `on_order_book` verb would be the
/// false-lanes defect. The PAPER remedy is alpaca's "report a bug", not ctrader's "restart":
/// `IgExecutionClient::spawn` is infallible at mount, so paper-with-resolved-credentials can
/// only be the two gates disagreeing over one map.
#[test]
fn ig_arming_discloses_the_demo_gateway_and_a_quote_only_requote_lane() {
    let live = ig_arming(true);
    assert_eq!(live.exec, "LIVE");
    assert_eq!(live.network, "DEMO", "the tier and the gateway are the same word here");
    assert_eq!(live.requote_lanes, "on_quote_tick", "no trade tape and no ladder on a dealer");
    assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

    // The caps row is the authority for the lane claim above, read rather than restated — and
    // it is what makes the two absences structural rather than unwired.
    let caps = vike_venues::caps_for("ig");
    assert!(caps.live_data.quotes && caps.live_data.bars, "the two verbs this arm subscribes");
    assert!(
        !caps.live_data.trades && !caps.live_data.book && !caps.live_data.depth,
        "…and the three IG structurally cannot serve; widening the arm past this row would \
             turn a venue fact into a mount error"
    );

    let paper = ig_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("bug"),
        "ig paper with resolved creds is a gate disagreement to report: {paper}"
    );
    assert!(
        !paper.to_lowercase().contains("add "),
        "…and it must NOT advise adding a key: `ig_plan` already proved the trio present, so \
             key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
    );
}
