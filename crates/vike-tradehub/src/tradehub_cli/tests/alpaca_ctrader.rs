//! The credentialed-data arms: alpaca and ctrader (split-plane I9).

use super::*;
use std::assert_matches;

// ---------------------------------------------------------------------------------------------
// The CREDENTIALED-DATA arms (alpaca/ctrader, split-plane I9): the plan gates, the credential
// threading, the interval constraints, and the arming disclosures.
//
// ⚠ Every credential below is a FIELD of an account (decision 0156 item 6), never a composed
// `ALPACA_`/`CTRADER_`-prefixed name: nothing reads one any more, and the settings-registry
// literal sweep (`vike_model::scan::find_map_lookups`) would read a whole env-shaped literal as a
// read sighting — the #1114 shape the aster remedy test documents.
// ---------------------------------------------------------------------------------------------

/// The sandbox trio, as the account fields `vike_alpaca::load_alpaca_config_from(Demo, …)` reads.
const ALPACA_FIELDS: [(&str, &str); 3] = [
    ("CLIENT_ID", "test-CLIENT_ID"),
    ("CLIENT_SECRET", "test-CLIENT_SECRET"),
    ("ACCOUNT_ID", "test-ACCOUNT_ID"),
];

fn alpaca_fields() -> CredentialFields {
    fields(&ALPACA_FIELDS)
}

/// The DEMO account's OAuth grant, as the account fields `CtraderConfig::from_credentials(Demo, …)`
/// reads.
fn ctrader_account() -> CredentialFields {
    fields(&[("ACCESS_TOKEN", "tok"), ("REFRESH_TOKEN", "refresh")])
}

/// The Spotware app registration — the VENUE's application fields, one pair for every account.
fn ctrader_app() -> CredentialFields {
    fields(&[("CLIENT_ID", "app-CLIENT_ID"), ("CLIENT_SECRET", "app-CLIENT_SECRET")])
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

/// Decisions 0095 and 0119: the hyperliquid feed plan follows the default account's own tier (an
/// ACTIVE `live` row with no key is still the mainnet feed), and a leftover variable in the map
/// decides nothing.
#[test]
fn the_hyperliquid_feed_plan_follows_the_account_tier_not_a_variable() {
    let cfg = hyperliquid_btc_mount();
    let stray = HashMap::from([(concat!("HYPERLIQUID", "_MAINNET").to_string(), "1".to_string())]);
    let policy_at = |mode| {
        vike_mount::MountPolicy::default().with_account(
            "hyperliquid",
            &vike_model::accounts::account_keys::AccountLabel::Default,
            mode,
        )
    };
    let live = policy_at(vike_model::AccountTier::Live);
    let demo = policy_at(vike_model::AccountTier::Demo);
    assert_matches!(
        venue_feed_plan(&cfg, &HashMap::new(), &live).unwrap(),
        VenuePlan::Hyperliquid(vike_hyperliquid::base::config::Network::Mainnet)
    );
    assert_matches!(
        venue_feed_plan(&cfg, &stray, &demo).unwrap(),
        VenuePlan::Hyperliquid(vike_hyperliquid::base::config::Network::Testnet)
    );
}

/// Every credentialed-data slug is joined to the exec plane the same way the CEX slugs are:
/// an engine row in `vike_mount::mounted_markets` AND a `FEED_ARMS` row.
#[test]
fn the_credentialed_data_venues_name_mounted_markets_and_feed_arm_rows() {
    for slug in ["alpaca", "ctrader", "oanda"] {
        assert_wired_and_advertised(slug);
    }
}

/// **The alpaca plan CARRIES the resolved sandbox config** — the same reason `VenuePlan::Cex`
/// carries its mainnet verdict: the mount policy (whose `account` snapshot holds the credentials)
/// is moved into the `NodeConfig` before the feed block runs, so the DATA credentials must travel
/// in the plan. The threading is exec's own loader over exec's own tier, so the two planes cannot
/// resolve differently.
#[test]
fn alpaca_plan_accepts_the_wired_pair_and_carries_the_sandbox_config() {
    let account = alpaca_fields();
    match alpaca_plan(&alpaca_cfg(), &creds(&account, None)) {
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

/// **The feed authenticates as the venue's DEFAULT account at the `demo` tier**, read out of the
/// mount policy's own `account` snapshot (decision 0156 item 6) — the account and the one tier the
/// demo-only exec arm binds. The same complete trio on a `live` account is NOT that account: the
/// plan refuses rather than dial the sandbox with a live account's credentials.
#[test]
fn the_alpaca_feed_reads_the_default_demo_accounts_fields_out_of_the_policy() {
    let demo = policy_holding("alpaca", vike_model::AccountTier::Demo, &ALPACA_FIELDS);
    assert_matches!(
        venue_feed_plan(&alpaca_cfg(), &HashMap::new(), &demo),
        Ok(VenuePlan::Alpaca(config)) if config.account_id == "test-ACCOUNT_ID"
    );
    let live = policy_holding("alpaca", vike_model::AccountTier::Live, &ALPACA_FIELDS);
    let err = venue_feed_plan(&alpaca_cfg(), &HashMap::new(), &live)
        .expect_err("a live account's trio is not the demo account's");
    assert!(err.contains("DEFAULT alpaca account"), "{err}");
}

/// **Absent credentials REFUSE the alpaca mount** — the documented divergence from the exec
/// gate (which degrades to paper): there is no keyless alpaca stream, and a live mount without
/// a feed quotes into the void. The refusal must name the exact trio by FIELD, the verb that
/// writes it and the one that creates the account — never a composed credential name.
#[test]
fn alpaca_plan_refuses_absent_credentials_naming_the_sandbox_trio() {
    let err = alpaca_plan(&alpaca_cfg(), &AccountCredentials::none())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in [
        "CLIENT_ID",
        "CLIENT_SECRET",
        "ACCOUNT_ID",
        "sandbox",
        "vike-cli secrets set --venue alpaca",
        "account add --venue alpaca --tier demo",
    ] {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    assert!(!err.contains("ALPACA_"), "no credential name is composed any more: {err}");
    // A partial trio is the same refusal — the loader is all-or-nothing.
    let partial = fields(&ALPACA_FIELDS[..2]);
    assert!(
        alpaca_plan(&alpaca_cfg(), &creds(&partial, None)).is_err(),
        "a partial trio must refuse too"
    );
}

/// The alpaca-shaped per-mount refusals: a foreign symbol (the silent-drop hazard, same as
/// `cex_plan`) and a non-1m interval (the WS serves 1m bars only, and bars drive the live
/// watchdogs — a `5m` mount would run with both dead).
#[test]
fn alpaca_plan_refuses_a_foreign_symbol_and_a_non_1m_interval() {
    let account = alpaca_fields();
    let trio = creds(&account, None);
    let mut foreign = alpaca_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = alpaca_plan(&foreign, &trio).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");

    let mut five = alpaca_cfg();
    five.interval = "5m".to_string();
    five.interval_ms = 300_000;
    let err = alpaca_plan(&five, &trio).expect_err("non-1m interval");
    assert!(err.contains("1m"), "the refusal must name the one servable interval: {err}");

    let mut bad_tick = alpaca_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        alpaca_plan(&bad_tick, &trio).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The ctrader plan CARRIES the resolved DEMO config** (same policy-lifetime argument as
/// alpaca's), and the Demo tier resolves the demo HOST — the exec side's own pin.
#[test]
fn ctrader_plan_accepts_the_wired_pair_and_carries_the_demo_config() {
    let (account, app) = (ctrader_account(), ctrader_app());
    match ctrader_plan(&ctrader_cfg(), &creds(&account, Some(&app))) {
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

/// **Absent credentials REFUSE the ctrader mount**, naming the venue's app pair, the account's
/// token pair (by FIELD) and the verbs that write them — same divergence-from-exec argument as
/// alpaca's refusal.
#[test]
fn ctrader_plan_refuses_absent_credentials_naming_the_token_set() {
    let err = ctrader_plan(&ctrader_cfg(), &AccountCredentials::none())
        .expect_err("no credentials must refuse the live mount, never mount feed-less");
    for needle in [
        "CLIENT_ID",
        "CLIENT_SECRET",
        "ACCESS_TOKEN",
        "REFRESH_TOKEN",
        "vike-cli secrets set --venue ctrader",
        "account add --venue ctrader --tier demo",
    ] {
        assert!(err.contains(needle), "the refusal must name {needle}: {err}");
    }
    assert!(!err.contains("CTRADER_"), "no credential name is composed any more: {err}");
    // The app pair alone (no token pair) is the same refusal — the loader is all-or-nothing.
    let (nothing, app) = (CredentialFields::new(), ctrader_app());
    assert!(
        ctrader_plan(&ctrader_cfg(), &creds(&nothing, Some(&app))).is_err(),
        "an app registration without an OAuth grant must refuse too"
    );
}

/// The ctrader foreign-symbol refusal — the same silent-drop hazard as every other venue.
#[test]
fn ctrader_plan_refuses_a_foreign_symbol() {
    let (account, app) = (ctrader_account(), ctrader_app());
    let mut foreign = ctrader_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = ctrader_plan(&foreign, &creds(&account, Some(&app))).expect_err("foreign symbol");
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
/// their bridges arming a `demo` account only — and neither serves a book lane, so claiming the CEX
/// `on_order_book` verb here would be the false-lanes claim `CexVenue::quote_source` records.
#[test]
fn alpaca_and_ctrader_arming_disclose_fixed_network_and_quote_only_requote_lane() {
    let alpaca = alpaca_arming(true);
    assert_eq!(alpaca.exec, "LIVE");
    assert_eq!(alpaca.network, "SANDBOX", "alpaca's own network word, not DEMO");
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

/// **The withhold takes the declared venue's credentials and nothing else**
/// ([`crate::venue_arming::withhold_data_only_credentials`]): every field of every account of that
/// venue goes, and its venue-wide application fields with them (exec cannot resolve the account),
/// while every other venue's account and application fields stay (another venue's mount is
/// untouched), and the count the disclosure logs is the count removed. The ROWS stay: the account
/// keeps its tier and holds nothing. Credentials are planted by FIELD on account ROWS (decision
/// 0156 item 6), so no credential name appears here at all.
#[test]
fn withhold_data_only_credentials_takes_the_venues_accounts_and_nothing_else() {
    use vike_model::AccountTier::Demo;
    let label = vike_model::accounts::account_keys::AccountLabel::Default;
    let rows = vike_mount::MountPolicy::default()
        .with_account("oanda", &label, Demo)
        .with_account("ig", &label, Demo)
        .with_account("ctrader", &label, Demo);
    let mut book = CredentialBook::new();
    book.insert_account(1, "API_KEY", "tok");
    book.insert_account(1, "ACCOUNT_ID", "acct");
    book.insert_account(2, "API_KEY", "other-venue");
    book.insert_account(3, "ACCESS_TOKEN", "grant");
    book.insert_venue("ctrader", "CLIENT_ID", "app");
    let accounts = rows.accounts.clone().with_credentials(book);

    let (kept, withheld) = crate::venue_arming::withhold_data_only_credentials(&accounts, "oanda");
    assert_eq!(withheld, 2, "both oanda fields and only the oanda fields");
    assert!(
        !kept.credentials_for("oanda", &label, Demo).holds_any(),
        "the account oanda's mount reaches now holds nothing — that absence IS the paper gate the \
         declaration rides"
    );
    assert!(
        vike_oanda::load_oanda_config_from(Demo, &kept.credentials_for("oanda", &label, Demo))
            .is_none(),
        "…so the practice loader resolves ABSENCE"
    );
    assert_eq!(kept.credentials_for("ig", &label, Demo).get("API_KEY"), Some("other-venue"));
    assert_eq!(kept.credentials_for("ctrader", &label, Demo).venue_get("CLIENT_ID"), Some("app"));
    assert_eq!(
        kept.account_row("oanda", &label, Demo).map(|r| r.id),
        Some(1),
        "the row stays: only its credentials are withheld"
    );

    // A venue-wide application field is the venue's credential too.
    let (kept, withheld) =
        crate::venue_arming::withhold_data_only_credentials(&accounts, "ctrader");
    assert_eq!(withheld, 2, "the account's grant and the venue's app field");
    let ctrader = kept.credentials_for("ctrader", &label, Demo);
    assert!(!ctrader.holds_any() && ctrader.venue_get("CLIENT_ID").is_none());
    assert_eq!(kept.credentials_for("oanda", &label, Demo).get("API_KEY"), Some("tok"));

    // An unread directory holds nothing to withhold.
    let (_, none) = crate::venue_arming::withhold_data_only_credentials(
        &vike_secrets::account_directory::AccountDirectory::unread(),
        "oanda",
    );
    assert_eq!(none, 0);
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
        env: vike_model::AccountTier::Demo,
        hosts: vike_alpaca::hosts_for(vike_model::AccountTier::Demo),
    };
    let client = vike_alpaca::AlpacaDataClient::new(config, Arc::new(NullSink), || {});
    let feeds = LiveFeeds::Alpaca(client);
    assert!(
        feeds.recon_feed_statuses().is_empty(),
        "no status handle exists on this seam — an invented row could only suppress passes"
    );
}
