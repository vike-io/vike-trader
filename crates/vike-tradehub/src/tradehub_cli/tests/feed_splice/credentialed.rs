//! The credentialed-data arms (alpaca, ctrader, oanda, ig): the tradehub resolves no credential, and the
//! gatewayhub's refusal of the stream is the mount's refusal.

use super::super::wired_1m_config;
use super::*;
use std::assert_matches;

/// The four venues, each with a tick size for its wired market, and a field its
/// refusal names (a stand-in for the gatewayhub's text, which names the account's fields).
const CREDENTIALED: [(&str, f64, &str); 4] = [
    ("alpaca", 0.01, "CLIENT_ID"),
    ("ctrader", 0.00001, "ACCESS_TOKEN"),
    ("oanda", 0.00001, "ACCOUNT_ID"),
    ("ig", 0.1, "IDENTIFIER"),
];

/// **The plan of a credentialed-data venue carries and requires no credential.** The old plans resolved
/// the venue's default `demo` account's fields out of the mount policy and REFUSED an absent trio at
/// plan time; the feed is a credentialed stream at the gatewayhub now, which reads the account itself
/// (`AccountDirectory::data_feed_credentials`), so the plan is the same value over an EMPTY policy and
/// holds nothing a `Debug` could leak. The refusal moved with the credential (the next test).
#[test]
fn a_credentialed_venues_plan_resolves_no_credential() {
    let table = vike_tradehub_core::ArmingTable::default();
    let empty = vike_tradehub_core::AdapterPolicy::default();
    for (venue, tick, _) in CREDENTIALED {
        let plan = venue_feed_plan(&wired_1m_config(venue, tick), &table, &empty)
            .unwrap_or_else(|e| panic!("{venue} must plan over an empty policy: {e}"));
        match venue {
            "alpaca" => assert_matches!(plan, crate::StrategyLanePlan::Alpaca),
            "ctrader" => assert_matches!(plan, crate::StrategyLanePlan::Ctrader),
            "oanda" => assert_matches!(plan, crate::StrategyLanePlan::Oanda),
            _ => assert_matches!(plan, crate::StrategyLanePlan::Ig),
        }
    }
}

/// **The gatewayhub's refusal of a credentialed stream refuses the MOUNT, in its own words.** Absent
/// credentials (and, for ctrader, a handshake that failed — it is synchronous at the gatewayhub's open)
/// are the open's refusal; it reaches the arm as the first `subscribe_*`'s error text, and every arm
/// turns it into the start's refusal rather than mounting a feed-less core that quotes into the void.
/// This is the "no keyless stream to fall back to" property the plan-time refusal used to hold, kept
/// at its new home: the REAL arm over a constructor whose feed fails every subscription with the text.
#[test]
fn a_credentialed_stream_the_gatewayhub_refuses_refuses_the_mount_with_its_words() {
    for (venue, tick, field) in CREDENTIALED {
        let config = wired_1m_config(venue, tick);
        let plan = venue_feed_plan(
            &config,
            &vike_tradehub_core::ArmingTable::default(),
            &vike_tradehub_core::AdapterPolicy::default(),
        )
        .expect("the plan reads no credential");

        let sentinel = std::env::temp_dir()
            .join(format!("vike-tradehub-feed-splice-cred-{venue}-{}", std::process::id()))
            .join("HALT");
        let strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> =
            Box::new(vike_strategy_lab::takers::buy_hold::BuyHold::new(0.001, None));
        let mount = build_paper_multi_strategy_core_with(
            vec![MountBuild { strategy, spec: config.clone() }],
            StrategyCorePaperConfig {
                halt: StrategyCorePaperHalt::Pinned(sentinel),
                ..Default::default()
            },
        );

        let refusal = format!(
            "{venue}'s market data is CREDENTIALED, so its feed cannot open on absent credentials \
             - give the DEFAULT {venue} account its {field} field"
        );
        let ctors = StubCtors { refusal: Some(refusal.clone()) };
        let wrap = |sink: Arc<dyn DataClientSink>| sink;
        let refused = wire_venue_feeds(
            &plan,
            &[&config],
            &mount.handle,
            &HashSet::new(),
            &[],
            &[vike_model::accounts::account_keys::AccountLabel::Default],
            &HashSet::new(),
            &wrap,
            &ctors,
        );
        let err = match refused {
            Ok(_) => panic!("{venue}: a refused credentialed stream must refuse the mount"),
            Err(e) => e.text,
        };
        assert!(err.contains(&refusal), "{venue}: the gatewayhub's words must survive: {err}");
        assert!(err.contains(venue), "{venue}: the refusal names the venue: {err}");
        mount.handle.shutdown_and_join();
    }
}

/// **An interval the venue's bridge cannot serve is no longer the PLAN's to refuse** — the move is
/// deliberate and pinned here so it is never mistaken for an omission. The plans used to ask the bridge
/// tables (oanda's granularity, IG's chart scales, deribit's resolution codes) and refuse BEFORE the core
/// spawned; this crate links no bridge, so the plan passes these and the bridge refuses the bar
/// subscription at the gatewayhub (oanda, IG and deribit, each in its own `subscribe_bars`). That refusal is
/// held end to end by `tests/hop/refusal_chain.rs`, which sends each through the real gatewayhub, datahub and
/// feed arm. Alpaca keeps its own refusal: its "1m only" rule is a string, not a table.
#[test]
fn an_interval_only_a_bridge_table_can_judge_is_no_longer_the_plans_to_refuse() {
    for (venue, tick, interval, interval_ms) in [
        ("oanda", 0.00001, "7m", 420_000),
        ("ig", 0.1, "15m", 900_000),
        ("deribit", 0.5, "4h", 14_400_000),
    ] {
        let mut config = wired_1m_config(venue, tick);
        config.interval = interval.to_string();
        config.interval_ms = interval_ms;
        assert!(
            venue_feed_plan(
                &config,
                &vike_tradehub_core::ArmingTable::default(),
                &vike_tradehub_core::AdapterPolicy::default(),
            )
            .is_ok(),
            "{venue} at {interval}: the plan holds no interval table any more"
        );
    }
    let mut alpaca = wired_1m_config("alpaca", 0.01);
    alpaca.interval = "5m".to_string();
    alpaca.interval_ms = 300_000;
    let err = venue_feed_plan(
        &alpaca,
        &vike_tradehub_core::ArmingTable::default(),
        &vike_tradehub_core::AdapterPolicy::default(),
    )
    .expect_err("alpaca serves 1m bars only, and that is still the plan's rule");
    assert!(err.contains("1m"), "{err}");
}
