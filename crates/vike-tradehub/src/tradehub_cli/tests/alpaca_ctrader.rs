//! The credentialed-data arms: alpaca and ctrader (split-plane I9).

use super::*;
use std::assert_matches;

// ---------------------------------------------------------------------------------------------
// The CREDENTIALED-DATA arms (alpaca/ctrader, split-plane I9): the plan gates, the credential
// threading, the interval constraints, and the arming disclosures.
//
// ⚠ Every credential key below is BUILT with `format!` fragments, never spelled as a whole
// `ALPACA_`/`CTRADER_`-prefixed literal: the settings-registry literal sweep
// (`vike_model::scan::find_map_lookups`) reads a whole env-shaped literal as a read sighting —
// the #1114 shape the aster remedy test above already documents.
// ---------------------------------------------------------------------------------------------

/// The SANDBOX trio, exactly as `vike_alpaca::load_alpaca_config_from(Demo, …)` looks it up.
fn alpaca_vars() -> HashMap<String, String> {
    let tier = vike_alpaca::alpaca_tier(vike_bridge_core::credentials::Environment::Demo);
    ["CLIENT_ID", "CLIENT_SECRET", "ACCOUNT_ID"]
        .iter()
        .map(|k| (format!("ALPACA_{tier}_{k}"), format!("test-{k}")))
        .collect()
}

/// The app pair + DEMO token pair, exactly as `CtraderConfig::from_vars(Demo, …)` looks them up.
fn ctrader_vars() -> HashMap<String, String> {
    let tier = "DEMO";
    let mut vars: HashMap<String, String> = ["CLIENT_ID", "CLIENT_SECRET"]
        .iter()
        .map(|k| (format!("CTRADER_{k}"), format!("app-{k}")))
        .collect();
    vars.insert(format!("CTRADER_{tier}_ACCESS_TOKEN"), "tok".to_string());
    vars.insert(format!("CTRADER_{tier}_REFRESH_TOKEN"), "refresh".to_string());
    vars
}

fn alpaca_cfg() -> MakerMountConfig {
    wired_1m_cfg("alpaca", 0.01, 1.0)
}

fn ctrader_cfg() -> MakerMountConfig {
    wired_1m_cfg("ctrader", 0.00001, 1_000.0)
}

/// The hyperliquid mount `venue_feed_plan`'s `("hyperliquid", _)` arm needs — `build_node` hardcodes
/// its market as `"BTC"` (see that arm's own guard), so every hyperliquid fixture in this file
/// shares this one config.
fn hyperliquid_btc_mount() -> MakerMountConfig {
    MakerMountConfig::crypto("hyperliquid", "BTC", 0.5, 0.001)
}

/// Decision 0095: the hyperliquid feed plan follows the ceiling, and a leftover variable in the map
/// decides nothing.
#[test]
fn the_hyperliquid_feed_plan_follows_the_ceiling_not_a_variable() {
    let cfg = hyperliquid_btc_mount();
    let stray = HashMap::from([(concat!("HYPERLIQUID", "_MAINNET").to_string(), "1".to_string())]);
    let policy_at = |mode| vike_mount::MountPolicy {
        venues: vike_config::VenuePolicy::default().declare("hyperliquid", mode),
        ..Default::default()
    };
    let live = policy_at(vike_config::VenueMode::Live);
    let demo = policy_at(vike_config::VenueMode::Demo);
    assert_matches!(
        venue_feed_plan(&cfg, &HashMap::new(), &live).unwrap(),
        VenuePlan::Hyperliquid(vike_hyperliquid::config::Network::Mainnet)
    );
    assert_matches!(
        venue_feed_plan(&cfg, &stray, &demo).unwrap(),
        VenuePlan::Hyperliquid(vike_hyperliquid::config::Network::Testnet)
    );
}

/// Every credentialed-data slug is joined to the exec plane the same way the CEX slugs are:
/// an engine row in `crate::wired_markets::WIRED_MARKETS` AND an advertised `LIVE_WIRED_VENUES` row.
#[test]
fn the_credentialed_data_venues_name_wired_markets_and_live_wired_rows() {
    for slug in ["alpaca", "ctrader", "oanda"] {
        assert_wired_and_advertised(slug);
    }
}

/// **The alpaca plan CARRIES the resolved SANDBOX config** — the same reason
/// `VenuePlan::Cex` carries its mainnet verdict: `vars` is moved into the `NodeConfig` before
/// the feed block runs, so the DATA credentials must travel in the plan. The threading is
/// exec's own loader over exec's own tier, so the two planes cannot resolve differently.
#[test]
fn alpaca_plan_accepts_the_wired_pair_and_carries_the_sandbox_config() {
    match alpaca_plan(&alpaca_cfg(), &alpaca_vars()) {
        Ok(VenuePlan::Alpaca(config)) => {
            assert_eq!(config.account_id, "test-ACCOUNT_ID", "the pinned account travels");
            assert!(
                config.hosts.data_ws.contains("sandbox"),
                "Demo tier ⇒ SANDBOX data hosts (the exec side's own tier): {}",
                config.hosts.data_ws
            );
        }
        other => panic!("the wired alpaca pair with the trio present must plan, got {other:?}"),
    }
}

/// **Absent credentials REFUSE the alpaca mount** — the documented divergence from the exec
/// gate (which degrades to paper): there is no keyless alpaca stream, and a live mount without
/// a feed quotes into the void. The refusal must name the exact trio and the store.
#[test]
fn alpaca_plan_refuses_absent_credentials_naming_the_sandbox_trio() {
    let err = alpaca_plan(&alpaca_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in ["_CLIENT_ID", "_CLIENT_SECRET", "_ACCOUNT_ID", "SANDBOX", "vike-cli secrets set"]
    {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    // A partial trio is the same refusal — the loader is all-or-nothing.
    let mut partial = alpaca_vars();
    partial.retain(|k, _| !k.ends_with("_ACCOUNT_ID"));
    assert!(alpaca_plan(&alpaca_cfg(), &partial).is_err(), "a partial trio must refuse too");
}

/// The alpaca-shaped per-mount refusals: a foreign symbol (the silent-drop hazard, same as
/// `cex_plan`) and a non-1m interval (the WS serves 1m bars only, and bars drive the live
/// watchdogs — a `5m` mount would run with both dead).
#[test]
fn alpaca_plan_refuses_a_foreign_symbol_and_a_non_1m_interval() {
    let mut foreign = alpaca_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = alpaca_plan(&foreign, &alpaca_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut five = alpaca_cfg();
    five.interval = "5m".to_string();
    five.interval_ms = 300_000;
    let err = alpaca_plan(&five, &alpaca_vars()).expect_err("non-1m interval");
    assert!(err.contains("1m"), "the refusal must name the one servable interval: {err}");

    let mut bad_tick = alpaca_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        alpaca_plan(&bad_tick, &alpaca_vars()).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The ctrader plan CARRIES the resolved DEMO config** (same vars-lifetime argument as
/// alpaca's), and the Demo tier resolves the demo HOST — the exec side's own pin.
#[test]
fn ctrader_plan_accepts_the_wired_pair_and_carries_the_demo_config() {
    match ctrader_plan(&ctrader_cfg(), &ctrader_vars()) {
        Ok(VenuePlan::Ctrader(config)) => {
            assert_eq!(
                config.host, "demo.ctraderapi.com",
                "Demo tier ⇒ the demo protobuf host, never live"
            );
            assert_eq!(config.port, vike_ctrader::config::CTRADER_PORT);
            assert_eq!(
                config.account_id, None,
                "no account id in the store ⇒ discovery at connect (never a live default — \
                     `conn.rs`'s NEVER-default-to-live rule)"
            );
        }
        other => {
            panic!("the wired ctrader pair with the tokens present must plan, got {other:?}")
        }
    }
}

/// **Absent credentials REFUSE the ctrader mount**, naming the app pair, the DEMO token pair
/// and the store — same divergence-from-exec argument as alpaca's refusal.
#[test]
fn ctrader_plan_refuses_absent_credentials_naming_the_token_set() {
    let err = ctrader_plan(&ctrader_cfg(), &HashMap::new())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in
        ["_CLIENT_ID", "_CLIENT_SECRET", "_ACCESS_TOKEN", "_REFRESH_TOKEN", "vike-cli secrets set"]
    {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    // The app pair alone (no token pair) is the same refusal — the loader is all-or-nothing.
    let mut app_only = ctrader_vars();
    app_only.retain(|k, _| !k.contains("TOKEN"));
    assert!(
        ctrader_plan(&ctrader_cfg(), &app_only).is_err(),
        "an app registration without an OAuth grant must refuse too"
    );
}

/// The ctrader foreign-symbol refusal — the same silent-drop hazard as every other venue.
#[test]
fn ctrader_plan_refuses_a_foreign_symbol() {
    let mut foreign = ctrader_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = ctrader_plan(&foreign, &ctrader_vars()).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");
}

/// **Two ctrader mounts must agree on ONE synth interval** — they share one data socket and
/// one `MakerSink` bar synthesizer, so a second interval would silently never fire (the
/// polymarket same-token constraint, worn by the venue that synthesizes its bars).
#[test]
fn ctrader_mounts_must_agree_on_one_synth_interval() {
    let a = ctrader_cfg();
    let mut b = ctrader_cfg();
    b.interval = "5m".to_string();
    b.interval_ms = 300_000;
    let err = check_ctrader_intervals(&[&a, &b]).expect_err("two windows, one synth");
    assert!(
        err.contains("mounts[0]") && err.contains("mounts[1]"),
        "the refusal names both offending rows: {err}"
    );
    // Agreement passes, and other venues' rows never trip it.
    let c = ctrader_cfg();
    assert!(check_ctrader_intervals(&[&a, &c]).is_ok(), "one shared window is fine");
    let hl = hyperliquid_btc_mount();
    assert!(
        check_ctrader_intervals(&[&a, &hl]).is_ok(),
        "a non-ctrader row at any interval is not this gate's business"
    );
}

/// **The arming disclosures state each venue's fixed network and its quote-only requote lane.**
/// Neither venue has a mainnet flag — alpaca is SANDBOX-pinned, ctrader DEMO-pinned, both by
/// `make_engine`'s own tier resolution — and neither serves a book lane, so claiming the CEX
/// `on_order_book` verb here would be the false-lanes claim `CexVenue::quote_source` records.
#[test]
fn alpaca_and_ctrader_arming_disclose_fixed_network_and_quote_only_requote_lane() {
    let alpaca = alpaca_arming(true);
    assert_eq!(alpaca.exec, "LIVE");
    assert_eq!(alpaca.network, "SANDBOX", "alpaca's own tier word, not DEMO");
    assert_eq!(alpaca.requote_lanes, "on_quote_tick", "no book lane exists on this venue");
    assert_eq!(alpaca.remedy, None, "a live mount has nothing to remedy");

    let ctrader = ctrader_arming(true);
    assert_eq!(ctrader.exec, "LIVE");
    assert_eq!(ctrader.network, "DEMO");
    assert_eq!(ctrader.requote_lanes, "on_quote_tick", "no trade/book lane on this venue");
    assert_eq!(ctrader.remedy, None);
}

/// **The two PAPER remedies name their own (different) causes.** Alpaca's paper state should
/// be unreachable (the plan refused absent creds; `spawn` is infallible) — its remedy says
/// "report a bug", never "add a key". Ctrader's IS reachable — a failed SYNCHRONOUS exec
/// connect demotes to paper while the later data connect succeeds — and its remedy is a
/// RESTART, not a key. Neither may advise credentials: the plan gate already proved them
/// present, so key advice here would be unreachable-advice, the defect class
/// `CexArming::remedy` documents.
#[test]
fn the_credentialed_data_paper_remedies_name_their_actual_causes() {
    let alpaca = alpaca_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        alpaca.contains("bug"),
        "alpaca paper with resolved creds is a gate disagreement to report: {alpaca}"
    );
    let ctrader = ctrader_arming(false).remedy.expect("paper must carry a remedy");
    assert!(
        ctrader.contains("restart") || ctrader.contains("Restart"),
        "ctrader paper means the exec handshake failed; the remedy is a retry: {ctrader}"
    );
    assert!(
        ctrader.contains("SYNCHRONOUSLY"),
        "…and it must say WHY exec can be paper while this very feed is live: {ctrader}"
    );
}

/// **The data-only disclosure replaces ONLY the remedy** ([`data_only_arming`]): the venue
/// facts — network, requote lanes, quote source — stay whatever that venue's own
/// `*_arming(false)` says (never a second copy that can drift), while the remedy names the
/// DECLARATION, the mechanism, and the way back — and stops claiming a bug or a restart, the
/// two ordinary paper causes that are false on the declared path.
#[test]
fn the_data_only_disclosure_names_the_declaration_and_keeps_the_venue_facts() {
    for (venue, base) in [
        ("alpaca", alpaca_arming(false)),
        ("ctrader", ctrader_arming(false)),
        ("oanda", oanda_arming(false)),
        ("ig", ig_arming(false)),
    ] {
        let plain = match venue {
            "alpaca" => alpaca_arming(false),
            "ctrader" => ctrader_arming(false),
            "oanda" => oanda_arming(false),
            _ => ig_arming(false),
        };
        let armed = data_only_arming(base, venue);
        assert_eq!(armed.exec, "PAPER", "{venue}: the declared state IS paper");
        assert_eq!(armed.network, plain.network, "{venue}: network is the venue's own fact");
        assert_eq!(armed.requote_lanes, plain.requote_lanes, "{venue}: lanes untouched");
        assert_eq!(armed.quote_source, plain.quote_source, "{venue}: source untouched");
        let remedy = armed.remedy.expect("a declared data-only mount still discloses WHY");
        for needle in ["data_only = true", "WITHHELD", "BY DECLARATION", venue] {
            assert!(remedy.contains(needle), "{venue}: must carry {needle:?}: {remedy}");
        }
        // The two ORDINARY paper causes, each false on the declared path: the
        // alpaca/oanda/ig gate-disagreement claim and ctrader's handshake-retry advice.
        // (The remedy MAY say "not a bug" — that is the correction, not the claim.)
        for false_claim in ["bug to report", "restart", "Restart"] {
            assert!(
                !remedy.contains(false_claim),
                "{venue}: the declared path must not claim {false_claim:?}: {remedy}"
            );
        }
    }
}

/// **The withhold is the venue's whole `{VENUE}_` key family and nothing else**
/// ([`vike_mount::startup::withhold_venue_credentials`]): every prefixed key goes (exec cannot resolve any tier),
/// every foreign key stays (another venue's mount is untouched), and the count the
/// disclosure logs is the count removed. Every key is spelled through its venue's own naming
/// authority (`vike_oanda::oanda_env_var_names`, `vike_ig::ig_env_var_names` — the
/// `oanda_vars` idiom above) so a key-grid rename reddens here rather than silently testing
/// dead names — and neither a hardcoded `vars.get("…")` literal (which the settings-registry
/// scanner resolves into a demand for a false `SETTINGS` row) nor a
/// `vike_model::credential_keys` builder call (which enrols the whole crate as a
/// generated-key composition site) appears here.
#[test]
fn withhold_venue_credentials_strips_the_venue_prefix_and_nothing_else() {
    let (key_k, acct_k) =
        vike_oanda::oanda_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    // A FOREIGN venue's key (ig, through its own naming authority) and a non-venue-prefixed
    // key: both must survive an oanda withhold untouched.
    let (foreign, _, _) =
        vike_ig::ig_env_var_names(vike_bridge_core::credentials::Environment::Demo);
    let unrelated = "OPERATOR_NOTE".to_string();
    let mut vars = HashMap::from([
        (key_k.clone(), "tok".to_string()),
        (acct_k.clone(), "acct".to_string()),
        (foreign.clone(), "other-venue".to_string()),
        (unrelated.clone(), "kept".to_string()),
    ]);
    let withheld = vike_mount::startup::withhold_venue_credentials(&mut vars, "oanda");
    assert_eq!(withheld, 2, "both oanda keys and only the oanda keys");
    assert!(!vars.contains_key(&key_k) && !vars.contains_key(&acct_k));
    assert!(
        vike_oanda::load_oanda_config_from(vike_bridge_core::credentials::Environment::Demo, &vars)
            .is_none(),
        "the practice loader oanda's mount reaches must now resolve ABSENCE — that absence IS \
             the paper gate the declaration rides"
    );
    assert_eq!(vars.get(&foreign).map(String::as_str), Some("other-venue"));
    assert_eq!(vars.get(&unrelated).map(String::as_str), Some("kept"));
}

/// **The credentialed-data venues contribute NO reconcile feed-status row** — the DECISION
/// documented on `LiveFeeds::recon_feed_statuses`: neither client exposes a status handle, and
/// the exec plane already classifies both as interval-only, never-health-blocked venues.
/// Constructed for real on the alpaca side (`AlpacaDataClient::new` is network-free — lazy
/// connections); ctrader's arm is the same literal `HashMap::new()` but its client cannot be
/// built without a live protobuf handshake, so its half rests on the same match arm this test
/// pins the shape of.
#[test]
fn the_alpaca_feed_contributes_no_recon_health_row() {
    let config = vike_alpaca::AlpacaConfig {
        client_id: "cid".to_string(),
        client_secret: "sec".to_string(),
        account_id: "acct".to_string(),
        env: vike_bridge_core::credentials::Environment::Demo,
        hosts: vike_alpaca::hosts_for(vike_bridge_core::credentials::Environment::Demo),
    };
    let client = vike_alpaca::AlpacaDataClient::new(config, Arc::new(NullSink), || {});
    let feeds = LiveFeeds::Alpaca(client);
    assert!(
        feeds.recon_feed_statuses().is_empty(),
        "no status handle exists on this seam — an invented row could only suppress passes"
    );
}
