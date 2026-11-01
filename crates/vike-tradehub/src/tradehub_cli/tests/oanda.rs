//! The OANDA feed arm.

use super::*;
use std::assert_matches;

// ── OANDA: the third credentialed-data arm, and the only one with no socket at all ────────

fn oanda_cfg() -> MakerConfig {
    wired_1m_cfg("oanda", 0.00001, 1_000.0)
}

/// **The oanda plan accepts the wired pair and carries nothing**: the plan used to carry the resolved
/// PRACTICE session (the Bearer token, the account id and both fxPractice bases, so the REST candle poll
/// and the pricing stream could not end up on different networks); the feed is a credentialed stream at
/// the gatewayhub now, which resolves the account and both bases itself.
#[test]
fn oanda_plan_accepts_the_wired_pair_and_carries_nothing() {
    assert_matches!(oanda_plan(&oanda_cfg()), Ok(VenuePlan::Oanda));
}

/// The oanda-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares) and a degenerate tick size. The interval refusal that was this venue's own — an interval the
/// candles endpoint has no GRANULARITY code for — is the bridge's now
/// (`feed_splice/credentialed.rs`'s `an_interval_only_a_bridge_table_can_judge_...` pins the move).
#[test]
fn oanda_plan_refuses_a_foreign_symbol_and_a_degenerate_tick() {
    let mut foreign = oanda_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = oanda_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol assemble_runtime mounts: {err}");

    let mut bad_tick = oanda_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        oanda_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The oanda arming discloses the venue's own network word and its quote-only requote
/// lane.** There is no `OANDA_MAINNET` flag — `make_engine` resolves the Demo tier
/// unconditionally and `oanda_hosts` maps it to fxPractice — and the venue publishes neither a
/// book lane nor a trade tape, so claiming the CEX `on_order_book` verb would be the
/// false-lanes defect. The PAPER remedy is alpaca's "report a bug", not ctrader's "restart":
/// `OandaExecutionClient::spawn` is infallible at mount, so paper-with-resolved-credentials
/// can only be the two gates disagreeing over one map.
#[test]
fn oanda_arming_discloses_the_practice_network_and_a_quote_only_requote_lane() {
    let live = oanda_arming(true);
    assert_eq!(live.exec, "LIVE");
    assert_eq!(live.network, "PRACTICE", "the venue's own word for its non-live environment");
    assert_eq!(live.requote_lanes, "on_quote_tick", "no book lane and no trade tape here");
    assert_eq!(live.remedy, None, "a live mount has nothing to remedy");

    let paper = oanda_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("bug"),
        "oanda paper with resolved creds is a gate disagreement to report: {paper}"
    );
    assert!(
        !paper.to_lowercase().contains("add "),
        "…and it must NOT advise adding a key: the gatewayhub refuses a credentialed stream that lacks the pair, \
             so key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
    );
}
