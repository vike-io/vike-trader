//! The credentialed-data arms (alpaca, ctrader, oanda, ig): the tradehub resolves no credential, and the
//! gatewayhub's refusal of the stream is the mount's refusal.

use super::*;
use std::assert_matches;

/// The four venues, each with a tick size and quantity for its wired market, and a field its
/// refusal names (a stand-in for the gatewayhub's text, which names the account's fields).
const CREDENTIALED: [(&str, f64, f64, &str); 4] = [
    ("alpaca", 0.01, 1.0, "CLIENT_ID"),
    ("ctrader", 0.00001, 1_000.0, "ACCESS_TOKEN"),
    ("oanda", 0.00001, 1_000.0, "ACCOUNT_ID"),
    ("ig", 0.1, 1.0, "IDENTIFIER"),
];

fn cfg_of(venue: &str, tick_size: f64, qty: f64) -> MakerConfig {
    let symbol = wired_symbol(venue).unwrap_or_else(|| panic!("assemble_runtime mounts {venue}"));
    let mut cfg = MakerConfig::crypto(venue, symbol, tick_size, qty);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    cfg
}

/// **The plan of a credentialed-data venue carries and requires no credential.** The old plans resolved
/// the venue's default `demo` account's fields out of the mount policy and REFUSED an absent trio at
/// plan time; the feed is a credentialed stream at the gatewayhub now, which reads the account itself
/// (`AccountDirectory::data_feed_credentials`), so the plan is the same value over an EMPTY policy and
/// holds nothing a `Debug` could leak. The refusal moved with the credential (the next test).
#[test]
fn a_credentialed_venues_plan_resolves_no_credential() {
    let table = vike_tradehub_core::ArmingTable::default();
    let empty = vike_tradehub_core::AdapterPolicy::default();
    for (venue, tick, qty, _) in CREDENTIALED {
        let plan = venue_feed_plan(&cfg_of(venue, tick, qty), &table, &empty)
            .unwrap_or_else(|e| panic!("{venue} must plan over an empty policy: {e}"));
        match venue {
            "alpaca" => assert_matches!(plan, crate::VenuePlan::Alpaca),
            "ctrader" => assert_matches!(plan, crate::VenuePlan::Ctrader),
            "oanda" => assert_matches!(plan, crate::VenuePlan::Oanda),
            _ => assert_matches!(plan, crate::VenuePlan::Ig),
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
    for (venue, tick, qty, field) in CREDENTIALED {
        let cfg = cfg_of(venue, tick, qty);
        let plan = venue_feed_plan(
            &cfg,
            &vike_tradehub_core::ArmingTable::default(),
            &vike_tradehub_core::AdapterPolicy::default(),
        )
        .expect("the plan reads no credential");

        let sentinel = std::env::temp_dir()
            .join(format!("vike-tradehub-feed-splice-cred-{venue}-{}", std::process::id()))
            .join("HALT");
        let strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> =
            Box::new(vike_strategy::BuyHold::new(0.001, None));
        let mount = build_paper_multi_strategy_core_with(
            vec![StrategyMountSpec { strategy, spec: cfg.mount_spec() }],
            PaperCoreOpts { halt: PaperHalt::Pinned(sentinel), ..Default::default() },
        );

        let refusal = format!(
            "{venue}'s market data is CREDENTIALED, so a live {venue} mount cannot run on absent \
             credentials - give the DEFAULT {venue} account its {field} field"
        );
        let ctors = StubCtors { refusal: Some(refusal.clone()) };
        let wrap = |sink: Arc<dyn LiveDataSink>| sink;
        let refused = wire_venue_feeds(
            &plan,
            &[&cfg],
            &mount.handle,
            &HashSet::new(),
            &[],
            &HashSet::new(),
            &wrap,
            &ctors,
        );
        let err = match refused {
            Ok(_) => panic!("{venue}: a refused credentialed stream must refuse the mount"),
            Err(e) => e,
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
/// subscription at the gatewayhub (oanda, IG, and deribit through its `BarIntervalGate`). That refusal is
/// held end to end by `tests/refusal_chain.rs`, which sends each through the real gatewayhub, datahub and
/// feed arm. Alpaca keeps its own refusal: its "1m only" rule is a string, not a table.
#[test]
fn an_interval_only_a_bridge_table_can_judge_is_no_longer_the_plans_to_refuse() {
    for (venue, tick, qty, interval, interval_ms) in [
        ("oanda", 0.00001, 1_000.0, "7m", 420_000),
        ("ig", 0.1, 1.0, "15m", 900_000),
        ("deribit", 0.5, 10.0, "4h", 14_400_000),
    ] {
        let mut cfg = cfg_of(venue, tick, qty);
        cfg.interval = interval.to_string();
        cfg.interval_ms = interval_ms;
        assert!(
            venue_feed_plan(
                &cfg,
                &vike_tradehub_core::ArmingTable::default(),
                &vike_tradehub_core::AdapterPolicy::default(),
            )
            .is_ok(),
            "{venue} at {interval}: the plan holds no interval table any more"
        );
    }
    let mut alpaca = cfg_of("alpaca", 0.01, 1.0);
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
