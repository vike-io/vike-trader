//! The IG feed arm.

use super::*;

// ── IG: the narrowest feed arm — two verbs, and the absence is structural ─────────────────

/// The DEMO trio, exactly as `load_ig_config_from(Demo, …)` looks it up
/// (`vike_ig::ig_env_var_names` is the naming authority; spelled through it so a rename of the
/// key grid reddens here rather than silently testing dead names).
fn ig_vars() -> HashMap<String, String> {
    let (key_k, id_k, pw_k) =
        vike_ig::ig_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    HashMap::from([
        (key_k, "key-xyz".to_string()),
        (id_k, "myuser".to_string()),
        (pw_k, "s3cr3t".to_string()),
    ])
}

fn ig_cfg() -> MakerMountConfig {
    wired_1m_cfg("ig", 0.1, 1.0)
}

/// ig is joined to the exec plane the same way every other live-wired slug is: an engine row
/// in `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
#[test]
fn ig_names_a_wired_market_and_a_live_wired_row() {
    assert_wired_and_advertised("ig");
}

/// **The ig plan CARRIES the resolved DEMO login** (the same vars-lifetime argument as
/// alpaca's, ctrader's and oanda's — `vars` is moved into the `NodeConfig` before the feed
/// block runs), and the Demo tier resolves the DEMO dealing gateway, which is the base every
/// per-subscription `IgSession::login` will use.
#[test]
fn ig_plan_accepts_the_wired_pair_and_carries_the_demo_session() {
    match ig_plan(&ig_cfg(), &ig_vars()) {
        Ok(VenuePlan::Ig(config)) => {
            assert_eq!(
                config.rest_base,
                vike_ig::ig_rest_base(vike_bridge_core::credentials::Environment::Demo),
                "the Demo tier must resolve the DEMO gateway, read from the venue's own \
                     resolver rather than restated here"
            );
            let dbg = format!("{config:?}");
            for secret in ["key-xyz", "myuser", "s3cr3t"] {
                assert!(
                    !dbg.contains(secret),
                    "the plan is `Debug`ged by the allow-list refusals — no IG secret may be \
                         in that output, and {secret} was: {dbg}"
                );
            }
        }
        other => panic!("the wired ig pair with the trio present must plan, got {other:?}"),
    }
}

/// **Absent credentials REFUSE the ig mount** — the alpaca divergence-from-exec argument:
/// every Lightstreamer subscription opens its own IG login, so there is no keyless half to
/// fall back to. The refusal must name the exact trio and the store. A PARTIAL trio refuses
/// too — the loader is all-or-nothing, and a half-filled store is exactly how an operator
/// arrives here.
#[test]
fn ig_plan_refuses_absent_credentials_naming_the_whole_trio() {
    let err = ig_plan(&ig_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in ["_API_KEY", "_IDENTIFIER", "_PASSWORD", "DEMO", "vike-cli secrets set"] {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    let mut partial = ig_vars();
    partial.retain(|k, _| !k.ends_with("_PASSWORD"));
    assert!(ig_plan(&ig_cfg(), &partial).is_err(), "a trio missing its password must refuse");
}

/// The ig-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares), a degenerate tick size, and — this venue's own — an interval Lightstreamer has no
/// chart SCALE for. The last is DERIVED from `vike_ig::market_data::ig_scale` in both
/// directions here: a streamable interval must pass and an unstreamable one must fail, so the
/// gate cannot drift from the table it reads.
#[test]
fn ig_plan_refuses_a_foreign_symbol_and_an_unstreamable_interval() {
    let mut foreign = ig_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}.NOT.THE.MOUNTED.ONE");
    let err = ig_plan(&foreign, &ig_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the epic build_node mounts: {err}");

    let mut bad_tick = ig_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        ig_plan(&bad_tick, &ig_vars()).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );

    // The venue's own refusal, both directions against the one table. `15m` is a width the
    // crate's REST history ladder serves and the STREAMING set does not, which is precisely
    // why the gate reads `ig_scale` rather than "does IG have candles at all".
    let mut unstreamable = ig_cfg();
    unstreamable.interval = "15m".to_string();
    unstreamable.interval_ms = 900_000;
    assert!(
        vike_ig::market_data::ig_scale("15m").is_none(),
        "the fixture interval must genuinely be unstreamable, or this proves nothing"
    );
    let err = ig_plan(&unstreamable, &ig_vars()).expect_err("unstreamable interval");
    assert!(err.contains("15m"), "the refusal must name the interval asked for: {err}");
    assert!(
        err.contains("ig_scale"),
        "…and the table that decided it, so the operator can look up what IS streamed: {err}"
    );

    // …and a NON-1m interval the venue DOES stream must pass — this is not alpaca, whose WS
    // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
    let mut five_minute = ig_cfg();
    five_minute.interval = "5m".to_string();
    five_minute.interval_ms = 300_000;
    assert!(
        vike_ig::market_data::ig_scale("5m").is_some() && ig_plan(&five_minute, &ig_vars()).is_ok(),
        "ig streams several scales; only an UNSTREAMABLE interval may be refused"
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
    let caps = vike_model::caps_for("ig");
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

/// **The ig feed contributes NO reconcile health row** — the oanda decision once more, and the
/// closed-market case makes it sharpest: a status handle of the CEX row's shape exists, shared
/// last-writer-wins across subscription threads that each hold their own IG session, and FX
/// closes every weekend, so a gate keyed on it would suppress passes on quiet rather than on
/// failure. Constructible for real — `Feeds::new` is network-free (threads are spawned per
/// subscription, and this feed has none).
#[test]
fn the_ig_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
    let config =
        vike_ig::load_ig_config_from(vike_bridge_core::credentials::Environment::Demo, &ig_vars())
            .expect("the fixture trio resolves");
    let feeds = vike_ig::market_feed::Feeds::new(Arc::new(NullSink), || {}, config);
    assert!(
        !feeds.status.lock().expect("status").is_empty(),
        "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
    );
    let live = LiveFeeds::Ig(feeds);
    assert!(
        live.recon_feed_statuses().is_empty(),
        "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' ig \
             paragraph for what would earn the row"
    );
}
