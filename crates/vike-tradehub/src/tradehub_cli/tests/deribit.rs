//! The DERIBIT feed arm.

use super::*;

// ── DERIBIT: the widest feed arm, and the one with NO credential gate ─────────────────────

fn deribit_cfg() -> MakerMountConfig {
    wired_1m_cfg("deribit", 0.5, 10.0)
}

/// deribit is joined to the exec plane the same way every other live-wired slug is: an engine
/// row in `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
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
fn deribit_plan_needs_no_credentials_and_mounts_on_an_empty_store() {
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
        venue_feed_plan(&deribit_cfg(), &HashMap::new(), &vike_mount::MountPolicy::default())
            .is_ok(),
        "an empty credential map must still plan a deribit live mount — a keyless feed has no \
             refusal to make, and inventing one would be the failure this arm exists not to copy"
    );
}

/// The deribit-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares), a degenerate tick size, and — this venue's own — an interval the chart channel has
/// no RESOLUTION code for. The last is DERIVED from `vike_deribit::data::resolution_code` in
/// both directions here: a mappable interval must pass and an unmappable one must fail, so the
/// gate cannot drift from the table it reads.
#[test]
fn deribit_plan_refuses_a_foreign_symbol_and_an_unservable_interval() {
    let mut foreign = deribit_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = deribit_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut bad_tick = deribit_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        deribit_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );

    // The venue's own refusal, both directions against the one table. `4h` is not an arbitrary
    // fixture: the deribit resolution enum genuinely skips it, which is exactly the kind of
    // gap that makes a hand-written list of intervals unsafe here.
    let mut unservable = deribit_cfg();
    unservable.interval = "4h".to_string();
    unservable.interval_ms = 14_400_000;
    assert!(
        vike_deribit::data::resolution_code("4h").is_err(),
        "the fixture interval must genuinely be unmappable, or this proves nothing"
    );
    let err = deribit_plan(&unservable).expect_err("unservable interval");
    assert!(err.contains("4h"), "the refusal must name the interval asked for: {err}");
    assert!(
        err.contains("resolution_code"),
        "…and the table that decided it, so the operator can look up what IS servable: {err}"
    );

    // …and a NON-1m interval the venue DOES serve must pass — this is not alpaca, whose WS
    // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
    let mut two_hour = deribit_cfg();
    two_hour.interval = "2h".to_string();
    two_hour.interval_ms = 7_200_000;
    assert!(
        vike_deribit::data::resolution_code("2h").is_ok() && deribit_plan(&two_hour).is_ok(),
        "deribit serves many resolutions; only an UNMAPPABLE interval may be refused"
    );
}

/// **The deribit arming discloses the TESTNET exec network, the MAINNET quote source, and the
/// full requote pair.** All three are facts rather than style: `make_engine` loads the DEMO
/// key tier and the bridge's authed sockets are hardcoded testnet while every public read is
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
    let caps = vike_model::caps_for("deribit");
    assert!(caps.live_data.book, "the `on_order_book` half of the claim comes from this row");
    // The venue now serves the conflating DOM lane too (the datahub mounts it, 2026-10-04), which
    // is a fact about the VENUE: this arm still subscribes only the lossless book, because a depth
    // subscription emits `l2_snapshot` into sinks this daemon owns that drop it.
    assert!(caps.live_data.depth, "…and the venue serves the conflating DOM lane as well");

    let paper = deribit_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        paper.contains("_API_KEY") && paper.contains("_API_SECRET"),
        "the keyless-feed venue's remedy IS reachable and must name the exec keys: {paper}"
    );
    assert!(
        !paper.contains("MAINNET=1"),
        "deribit's network is never the ceiling (its bridge's mount binds testnet, and vike-mount's \
             `the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing` pins it) — \
             advising a flag that is read nowhere is the unreachable-advice defect \
             `CexArming::remedy` documents: {paper}"
    );
}

/// **The deribit feed contributes NO reconcile health row** — the oanda decision, one lane
/// count worse. This client exposes a `status` handle of the CEX row's shape, but four lanes
/// share it last-writer-wins, and the book lane writes an error string on every deliberate
/// resync (a chain gap IS a session fault here), so a healthy feed would publish
/// `Degraded`-reading text as ordinary operation and suppress passes. Constructible for real —
/// `Feeds::new` is network-free.
#[test]
fn the_deribit_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
    let feeds = vike_deribit::market_feed::Feeds::new(Arc::new(NullSink), || {});
    assert!(
        !feeds.status.lock().expect("status").is_empty(),
        "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
    );
    let live = LiveFeeds::Deribit(Box::new(feeds));
    assert!(
        live.recon_feed_statuses().is_empty(),
        "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' deribit \
             paragraph for what would earn the row"
    );
}
