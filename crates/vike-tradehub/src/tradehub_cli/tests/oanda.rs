//! The OANDA feed arm.

use super::*;

// ── OANDA: the third credentialed-data arm, and the only one with no socket at all ────────

/// The practice-tier token + account pair, exactly as `load_oanda_config_from(Demo, …)` looks
/// them up (`vike_oanda::oanda_env_var_names` is the naming authority; spelled through it so
/// a rename of the key grid reddens here rather than silently testing dead names).
fn oanda_vars() -> HashMap<String, String> {
    let (key_k, acct_k) =
        vike_oanda::oanda_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    HashMap::from([(key_k, "tok-abc-123".to_string()), (acct_k, "101-004-1234567-001".to_string())])
}

fn oanda_cfg() -> MakerMountConfig {
    wired_1m_cfg("oanda", 0.00001, 1_000.0)
}

/// **The oanda plan CARRIES the resolved PRACTICE session** (the same vars-lifetime argument
/// as alpaca's and ctrader's), and the Demo tier resolves the fxPractice hosts on BOTH bases
/// — the REST one the candle poll fetches from and the STREAM one the pricing stream dials.
/// Both travel in the one config, so the two feed lanes cannot end up on different networks.
#[test]
fn oanda_plan_accepts_the_wired_pair_and_carries_the_practice_session() {
    match oanda_plan(&oanda_cfg(), &oanda_vars()) {
        Ok(VenuePlan::Oanda(config)) => {
            assert_eq!(config.account_id, "101-004-1234567-001", "the account travels");
            assert!(
                config.rest_base.contains("fxpractice")
                    && config.stream_base.contains("fxpractice"),
                "Demo tier ⇒ fxPractice on BOTH bases (the exec side's own pin): {} / {}",
                config.rest_base,
                config.stream_base
            );
            assert!(
                !format!("{config:?}").contains("tok-abc-123"),
                "the plan is `Debug`ged by the allow-list refusals — the bearer token must \
                     never be in that output"
            );
        }
        other => panic!("the wired oanda pair with the pair present must plan, got {other:?}"),
    }
}

/// **Absent credentials REFUSE the oanda mount** — the alpaca divergence-from-exec argument,
/// doubled: BOTH of this venue's feed lanes are Bearer-authed, so there is no keyless half to
/// fall back to either. The refusal must name the exact pair and the store.
#[test]
fn oanda_plan_refuses_absent_credentials_naming_the_token_and_account() {
    let err = oanda_plan(&oanda_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in ["_API_KEY", "_ACCOUNT_ID", "DEMO", "vike-cli secrets set"] {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    // A token with no account id is the same refusal — the loader is all-or-nothing, and a
    // half-configured store is exactly how an operator arrives here.
    let mut partial = oanda_vars();
    partial.retain(|k, _| !k.ends_with("_ACCOUNT_ID"));
    assert!(oanda_plan(&oanda_cfg(), &partial).is_err(), "a lone token must refuse too");
}

/// The oanda-shaped per-mount refusals: a foreign symbol (the silent-drop hazard every venue
/// shares), a degenerate tick size, and — the one that is this venue's own — an interval the
/// candles endpoint has no GRANULARITY code for. The last is DERIVED from
/// `vike_oanda::granularity` in both directions here: a mappable interval must pass and an
/// unmappable one must fail, so the gate cannot drift from the table it reads.
#[test]
fn oanda_plan_refuses_a_foreign_symbol_and_an_unservable_interval() {
    let mut foreign = oanda_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = oanda_plan(&foreign, &oanda_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut bad_tick = oanda_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        oanda_plan(&bad_tick, &oanda_vars()).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );

    // The venue's own refusal, both directions against the one table.
    let mut unservable = oanda_cfg();
    unservable.interval = "7m".to_string();
    unservable.interval_ms = 420_000;
    assert!(
        vike_oanda::granularity("7m").is_none(),
        "the fixture interval must genuinely be unmappable, or this proves nothing"
    );
    let err = oanda_plan(&unservable, &oanda_vars()).expect_err("unservable interval");
    assert!(err.contains("7m"), "the refusal must name the interval asked for: {err}");
    assert!(
        err.contains("granularity"),
        "…and the table that decided it, so the operator can look up what IS servable: {err}"
    );

    // …and a NON-1m interval the venue DOES serve must pass — this is not alpaca, whose WS
    // serves one bar width. A copy of alpaca's `interval != "1m"` refusal would fail here.
    let mut four_hour = oanda_cfg();
    four_hour.interval = "4h".to_string();
    four_hour.interval_ms = 14_400_000;
    assert!(
        vike_oanda::granularity("4h").is_some() && oanda_plan(&four_hour, &oanda_vars()).is_ok(),
        "oanda serves many granularities; only an UNMAPPABLE interval may be refused"
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
        "…and it must NOT advise adding a key: `oanda_plan` already proved the pair present, \
             so key advice here would be the unreachable-advice defect `CexArming::remedy` \
             documents: {paper}"
    );
}

/// **The oanda feed contributes NO reconcile health row either — and this one is a genuine
/// DECISION rather than an absence.** Unlike `AlpacaDataClient`/`CtraderData`, this client DOES
/// expose a `status` handle of exactly the shape the CEX row keys on, so the row is withheld on
/// its merits: one last-writer-wins string is shared by the quote reader and every candle
/// poller, and a transient poll failure would read `Degraded` and suppress a reconcile pass
/// the bar lane has nothing to do with. Constructible for real here — `Feeds::new` is
/// network-free (threads are spawned per subscription, and this feed has none).
#[test]
fn the_oanda_feed_contributes_no_recon_health_row_despite_owning_a_status_handle() {
    let feeds = vike_oanda::market_feed::Feeds::new(Arc::new(NullSink), || {});
    assert!(
        !feeds.status.lock().expect("status").is_empty(),
        "the handle this test is ABOUT must exist and be readable, or the decision below is \
             about nothing"
    );
    let live = LiveFeeds::Oanda(Box::new(feeds));
    assert!(
        live.recon_feed_statuses().is_empty(),
        "the handle exists but is not per-lane evidence — see `recon_feed_statuses`' oanda \
             paragraph for what would earn the row"
    );
}
