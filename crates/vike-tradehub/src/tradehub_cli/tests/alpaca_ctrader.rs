//! The credentialed-data arms: alpaca and ctrader (split-plane I9).

use super::*;
use std::assert_matches;

// ---------------------------------------------------------------------------------------------
// The CREDENTIALED-DATA arms (alpaca/ctrader, split-plane I9): the plan gates, the interval
// constraints, and the arming disclosures. What the arm does with the gatewayhub's refusal of the
// credentialed stream is `feed_splice/credentialed.rs`'s.
//
// ⚠ The plans resolve NO credential any more (the feed is a credentialed stream at the gatewayhub, which
// reads the account itself): the credential fixtures this file carried — the sandbox trio, the OAuth
// grant, the Spotware application pair — went with the loaders they fed.
// ---------------------------------------------------------------------------------------------

fn alpaca_cfg() -> MakerConfig {
    wired_1m_cfg("alpaca", 0.01, 1.0)
}

fn ctrader_cfg() -> MakerConfig {
    wired_1m_cfg("ctrader", 0.00001, 1_000.0)
}

/// The hyperliquid mount `venue_feed_plan`'s `("hyperliquid", _)` arm needs — `assemble_runtime` hardcodes
/// its market as `"BTC"` (see that arm's own guard), so every hyperliquid fixture in this file
/// shares this one config.
fn hyperliquid_btc_mount() -> MakerConfig {
    MakerConfig::crypto("hyperliquid", "BTC", 0.5, 0.001)
}

/// Decisions 0095 and 0119: the hyperliquid feed plan follows the default account's own tier (an
/// ACTIVE `live` row with no key is still the mainnet feed), and a leftover variable in the map
/// decides nothing.
#[test]
fn the_hyperliquid_feed_plan_follows_the_account_tier_not_a_variable() {
    let cfg = hyperliquid_btc_mount();
    let stray = HashMap::from([(concat!("HYPERLIQUID", "_MAINNET").to_string(), "1".to_string())]);
    let policy_at = |mode| {
        vike_tradehub_core::AdapterPolicy::default().with_account(
            "hyperliquid",
            &vike_model::accounts::account_keys::AccountLabel::Default,
            mode,
        )
    };
    let live = policy_at(vike_model::AccountTier::Live);
    let demo = policy_at(vike_model::AccountTier::Demo);
    assert_matches!(
        venue_feed_plan(
            &cfg,
            &crate::scripted_arming_tests::plan_table(&HashMap::new(), &live),
            &live
        )
        .unwrap(),
        VenuePlan::Hyperliquid(crate::HlNetwork::Mainnet)
    );
    assert_matches!(
        venue_feed_plan(&cfg, &crate::scripted_arming_tests::plan_table(&stray, &demo), &demo)
            .unwrap(),
        VenuePlan::Hyperliquid(crate::HlNetwork::Testnet)
    );
}

/// Every credentialed-data slug is joined to the exec plane the same way the CEX slugs are:
/// an engine row in `vike_tradehub_core::wired_markets` AND a `FEED_ARMS` row.
#[test]
fn the_credentialed_data_venues_name_wired_markets_and_feed_arm_rows() {
    for slug in ["alpaca", "ctrader", "oanda"] {
        assert_wired_and_advertised(slug);
    }
}

/// **The alpaca plan accepts the wired pair and carries nothing**: the plan used to carry the resolved
/// sandbox config (the mount policy was moved into the `RuntimeConfig` before the feed block ran), and
/// its tier-keyed hosts with it; the feed is a credentialed stream at the gatewayhub now, so the
/// plan is a unit variant and the sandbox tier is the gatewayhub's (`FEED_TIER`).
#[test]
fn alpaca_plan_accepts_the_wired_pair_and_carries_nothing() {
    assert_matches!(alpaca_plan(&alpaca_cfg()), Ok(VenuePlan::Alpaca));
}

/// The alpaca-shaped per-mount refusals: a foreign symbol (the silent-drop hazard, same as
/// `cex_plan`) and a non-1m interval (the WS serves 1m bars only, and bars drive the live
/// watchdogs — a `5m` mount would run with both dead).
#[test]
fn alpaca_plan_refuses_a_foreign_symbol_and_a_non_1m_interval() {
    let mut foreign = alpaca_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = alpaca_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol assemble_runtime mounts: {err}");

    let mut five = alpaca_cfg();
    five.interval = "5m".to_string();
    five.interval_ms = 300_000;
    let err = alpaca_plan(&five).expect_err("non-1m interval");
    assert!(err.contains("1m"), "the refusal must name the one servable interval: {err}");

    let mut bad_tick = alpaca_cfg();
    bad_tick.tick_size = 0.0;
    assert!(
        alpaca_plan(&bad_tick).expect_err("degenerate tick").contains("tick_size"),
        "the tick refusal names the field"
    );
}

/// **The ctrader plan accepts the wired pair and carries nothing** (see the alpaca twin): the DEMO host
/// the old plan carried (`demo.ctraderapi.com`, the exec side's own pin) is the gatewayhub's now, and
/// its token pair is read there.
#[test]
fn ctrader_plan_accepts_the_wired_pair_and_carries_nothing() {
    assert_matches!(ctrader_plan(&ctrader_cfg()), Ok(VenuePlan::Ctrader));
}

/// The ctrader foreign-symbol refusal — the same silent-drop hazard as every other venue.
#[test]
fn ctrader_plan_refuses_a_foreign_symbol() {
    let mut foreign = ctrader_cfg();
    let wired = foreign.token_id.clone();
    foreign.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
    let err = ctrader_plan(&foreign).expect_err("foreign symbol");
    assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
    assert!(err.contains(&wired), "names the symbol assemble_runtime mounts: {err}");
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
    let rows = vike_tradehub_core::AdapterPolicy::default()
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

/// **No feed contributes a reconcile feed-status row** (see `LiveFeeds::recon_feed_statuses`). The map's
/// one row was the CEX venue's bridge `Feeds::status` string, a handle this process no longer owns (the
/// sockets are the gatewayhub's); the alpaca, ctrader, oanda, deribit and ig arms never had one worth
/// keying a health gate on, and hyperliquid and polymarket kept the empty shape on purpose. Built from the
/// feed doubles the splice tests use: an invented row could only suppress passes.
#[test]
fn no_feed_contributes_a_recon_health_row() {
    let tracked = || Tracked::new(Box::new(StubFeed::default()));
    for feeds in [
        LiveFeeds::Hyperliquid(crate::feeds::QuoteFeed::new(tracked(), Default::default())),
        LiveFeeds::Alpaca(tracked()),
        LiveFeeds::Ctrader(tracked()),
        LiveFeeds::Oanda(tracked()),
        LiveFeeds::Deribit(crate::feeds::QuoteFeed::new(tracked(), Default::default())),
        LiveFeeds::Ig(tracked()),
    ] {
        assert!(
            feeds.recon_feed_statuses().is_empty(),
            "no status handle exists on this seam — an invented row could only suppress passes"
        );
    }
    assert!(recon_feed_statuses_of(&[LiveFeeds::Alpaca(tracked())]).is_empty());
}
