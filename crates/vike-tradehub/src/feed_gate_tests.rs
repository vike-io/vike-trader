//! The feed gate's contracts: the table overlay, the refused start of a held account, and the credentialed
//! feed's pre-core refusal. The whole-start behaviour is `tradehub_cli/tests/feed_splice/feed_outage.rs`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::{
    AdapterStartQuery, ArmingRow, ArmingTable, BackendError, ScriptedBackend, VenueBackend,
};

use super::{FeedGatedBackend, arms_through_gateway, credentialed_feed_refusal, hold_for_feeds};
use crate::VenuePlan;

fn row(
    venue: &'static str,
    label: AccountLabel,
    tier: AccountTier,
    effective: Option<AccountTier>,
    block: ArmingBlock,
) -> ArmingRow {
    ArmingRow {
        arming: VenueArming {
            venue,
            label,
            tier: Some(tier),
            effective,
            block,
            account_ids: vec![1],
        },
        book: None,
    }
}

fn table(rows: Vec<ArmingRow>) -> ArmingTable {
    ArmingTable::new(BTreeMap::new(), rows)
}

fn held(venues: &[&str]) -> BTreeSet<String> {
    venues.iter().map(|v| (*v).to_string()).collect()
}

fn find<'a>(t: &'a ArmingTable, venue: &str, tier: AccountTier) -> &'a VenueArming {
    &t.rows()
        .iter()
        .find(|r| r.arming.venue == venue && r.arming.tier == Some(tier))
        .unwrap_or_else(|| panic!("{venue} {tier:?}"))
        .arming
}

/// **Only a running `demo`/`live` account of a held venue is held**: a `paper` account runs the simulator
/// and takes no price from a venue socket of its own; an account that already does not run keeps its own,
/// more specific, block; a venue nobody mounted is untouched.
#[test]
fn only_the_live_accounts_of_the_held_venues_are_held() {
    let t = table(vec![
        row(
            "binance",
            AccountLabel::Default,
            AccountTier::Demo,
            Some(AccountTier::Demo),
            ArmingBlock::None,
        ),
        row(
            "binance",
            AccountLabel::Default,
            AccountTier::Paper,
            Some(AccountTier::Paper),
            ArmingBlock::PaperTier,
        ),
        row(
            "deribit",
            AccountLabel::Default,
            AccountTier::Live,
            Some(AccountTier::Live),
            ArmingBlock::None,
        ),
        row("oanda", AccountLabel::Default, AccountTier::Demo, None, ArmingBlock::NoCredentials),
        row(
            "ibkr",
            AccountLabel::Default,
            AccountTier::Demo,
            Some(AccountTier::Demo),
            ArmingBlock::None,
        ),
        row(
            "alpaca",
            AccountLabel::Default,
            AccountTier::Demo,
            Some(AccountTier::Demo),
            ArmingBlock::OrdersHeld,
        ),
    ]);
    let out = hold_for_feeds(&t, &held(&["binance", "deribit", "oanda", "alpaca"]));
    let b = find(&out, "binance", AccountTier::Demo);
    assert_eq!((b.effective, b.block), (None, ArmingBlock::FeedsUnavailable));
    let paper = find(&out, "binance", AccountTier::Paper);
    assert_eq!((paper.effective, paper.block), (Some(AccountTier::Paper), ArmingBlock::PaperTier));
    let d = find(&out, "deribit", AccountTier::Live);
    assert_eq!((d.effective, d.block), (None, ArmingBlock::FeedsUnavailable));
    let o = find(&out, "oanda", AccountTier::Demo);
    assert_eq!(o.block, ArmingBlock::NoCredentials, "its own, more specific reason stands");
    let i = find(&out, "ibkr", AccountTier::Demo);
    assert_eq!((i.effective, i.block), (Some(AccountTier::Demo), ArmingBlock::None));
    // OrdersHeld RUNS (it connects and reconciles, sending no order): it needs prices, so it is held too.
    let a = find(&out, "alpaca", AccountTier::Demo);
    assert_eq!((a.effective, a.block), (None, ArmingBlock::FeedsUnavailable));
}

/// The wrapper answers the overlay from `arming_table`, and a start of a held venue's live account is the
/// backend refusing with the block (`FeedsUnavailable`; the reason goes to the log), asked of the inner
/// backend NEVER; a venue nobody holds is the inner backend's.
#[test]
fn a_held_account_is_not_started_and_the_error_says_why() {
    let inner = Arc::new(ScriptedBackend::new(table(vec![
        row(
            "binance",
            AccountLabel::Default,
            AccountTier::Demo,
            Some(AccountTier::Demo),
            ArmingBlock::None,
        ),
        row(
            "bybit",
            AccountLabel::Default,
            AccountTier::Demo,
            Some(AccountTier::Demo),
            ArmingBlock::None,
        ),
    ])));
    let gated = FeedGatedBackend::wrap(
        Arc::clone(&inner) as Arc<dyn VenueBackend>,
        held(&["binance"]),
        "the price feeds are unavailable: datahub down".to_string(),
    );
    let t = gated.arming_table().expect("answers");
    assert_eq!(find(&t, "binance", AccountTier::Demo).block, ArmingBlock::FeedsUnavailable);
    assert_eq!(find(&t, "bybit", AccountTier::Demo).block, ArmingBlock::None, "not held");

    let (tx, _rx) = vike_exec::event_channel(4);
    let account = AccountLabel::Default;
    let query = |venue: &'static str| AdapterStartQuery {
        venue,
        account: &account,
        tier: AccountTier::Demo,
        symbol: "BTCUSDT",
        declared_legs: &[],
        recon_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        route_key: venue,
        properties_rec: None,
    };
    let refused = gated.start_adapter(query("binance"), &tx, None).expect_err("held");
    assert_eq!(refused, BackendError::NotRunning(ArmingBlock::FeedsUnavailable));
    assert!(inner.calls().is_empty(), "the inner backend was never asked: {:?}", inner.calls());
    let _ = gated.start_adapter(query("bybit"), &tx, None);
    assert_eq!(inner.calls().len(), 1, "a venue nobody holds reaches the inner backend");
}

/// **A credentialed-data venue whose DEFAULT demo account holds no usable key set is refused before a core
/// exists**, in the words that name the account, the verb and the way out. Every other case is left to the
/// gatewayhub: a `data_only` venue, a venue that is not credentialed-data, a gatewayhub that did not answer,
/// a labelled account, a row at another tier.
#[test]
fn a_credentialed_feed_without_credentials_is_refused_before_the_core() {
    let no_creds = |venue, label, tier, block| table(vec![row(venue, label, tier, None, block)]);
    let t =
        no_creds("alpaca", AccountLabel::Default, AccountTier::Demo, ArmingBlock::NoCredentials);
    let none = HashSet::new();
    let why = credentialed_feed_refusal("alpaca", &VenuePlan::Alpaca, &t, &none)
        .expect("a credentialed feed on absent credentials is refused");
    for needle in
        ["alpaca", "CREDENTIALED", "no usable key set", "vike-cli secrets", "before any core"]
    {
        assert!(why.contains(needle), "{needle}: {why}");
    }
    // The four credentialed-data venues, each by its own plan.
    for (venue, plan) in
        [("ctrader", VenuePlan::Ctrader), ("oanda", VenuePlan::Oanda), ("ig", VenuePlan::Ig)]
    {
        let t =
            no_creds(venue, AccountLabel::Default, AccountTier::Demo, ArmingBlock::NoCredentials);
        assert!(credentialed_feed_refusal(venue, &plan, &t, &none).is_some(), "{venue}");
    }
    // …and the cases it leaves alone.
    let declared: HashSet<String> = ["alpaca".to_string()].into_iter().collect();
    assert!(
        credentialed_feed_refusal("alpaca", &VenuePlan::Alpaca, &t, &declared).is_none(),
        "data_only"
    );
    assert!(
        credentialed_feed_refusal(
            "binance",
            &VenuePlan::Cex { venue: crate::CexVenue::Binance, mainnet: true },
            &t,
            &none
        )
        .is_none(),
        "a keyless-feed venue"
    );
    let unreachable = no_creds(
        "alpaca",
        AccountLabel::Default,
        AccountTier::Demo,
        ArmingBlock::GatewayUnavailable,
    );
    assert!(credentialed_feed_refusal("alpaca", &VenuePlan::Alpaca, &unreachable, &none).is_none());
    let labelled = no_creds(
        "alpaca",
        AccountLabel::parse("ALT").expect("a label"),
        AccountTier::Demo,
        ArmingBlock::NoCredentials,
    );
    assert!(credentialed_feed_refusal("alpaca", &VenuePlan::Alpaca, &labelled, &none).is_none());
    let live_row = no_creds(
        "alpaca",
        AccountLabel::Default,
        AccountTier::Live,
        ArmingBlock::LiveCredentialsAbsent,
    );
    assert!(credentialed_feed_refusal("alpaca", &VenuePlan::Alpaca, &live_row, &none).is_none());
    let armed = no_creds("alpaca", AccountLabel::Default, AccountTier::Demo, ArmingBlock::None);
    assert!(credentialed_feed_refusal("alpaca", &VenuePlan::Alpaca, &armed, &none).is_none());
    // And the error type the fold maps: unavailable, with the words.
    let e = BackendError::Unavailable("x".into());
    assert!(e.to_string().contains("unavailable"));
}

/// **A refusal at wiring time is survived only when it is a transient one AND no live account of the venue
/// runs**: the core cannot drop an engine, so a venue whose `demo`/`live` account holds an exec client ends the
/// start as before; a configuration refusal (a bad interval, credentials) always does. `live_venues` holds ROUTE
/// KEYS (`venue`, `venue#LABEL`).
#[test]
fn only_a_transient_refusal_of_a_venue_with_no_running_live_account_is_survived() {
    use super::{survivable_refusal, venue_runs_live};
    let transient = "deribit subscribe_bars BTC-PERPETUAL@1m: Subscribe(\"gatewayhub 127.0.0.1:1: the link is \
         down (not dialled yet); the subscription was not started and is retried on the caller's next pass\")";
    let permanent = "oanda subscribe_bars EURUSD@7m: Subscribe(\"no OANDA candle granularity for this interval\")";
    let none: HashSet<String> = HashSet::new();
    let set =
        |keys: &[&str]| -> HashSet<String> { keys.iter().map(|k| (*k).to_string()).collect() };

    assert!(survivable_refusal(transient, "deribit", &none), "nothing live runs: survived");
    assert!(
        survivable_refusal(transient, "deribit", &set(&["binance", "deribitx"])),
        "another venue's account, or a name that merely starts alike, is not this venue's"
    );
    assert!(
        !survivable_refusal(transient, "deribit", &set(&["deribit"])),
        "the default account runs"
    );
    assert!(
        !survivable_refusal(transient, "deribit", &set(&["deribit#ALT"])),
        "a labelled account runs"
    );
    assert!(
        !survivable_refusal(permanent, "oanda", &none),
        "a configuration refusal ends the start"
    );
    assert!(venue_runs_live("binance", &set(&["binance#A", "bybit"])));
    assert!(!venue_runs_live("bin", &set(&["binance"])));
}

/// **A failed start's teardown stops the forwarder, releases the feeds and joins the core inside its bound**:
/// the ordinary case answers `true` (graceful) and raises the forwarder's flag first. The bound itself
/// (`FAILED_START_TEARDOWN`) is `run_with_deadline`'s, which `vike-boot` tests.
#[test]
fn a_failed_starts_teardown_quiesces_the_forwarder_and_joins_the_core() {
    let symbol = vike_venues::venue_wiring::wired_symbol("deribit").expect("deribit is wired");
    let mut cfg = vike_tradehub_core::MakerConfig::crypto("deribit", symbol, 0.5, 10.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;
    let strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> =
        Box::new(vike_strategy::BuyHold::new(0.001, None));
    let sentinel = std::env::temp_dir()
        .join(format!("vike-tradehub-feed-gate-teardown-{}", std::process::id()))
        .join("HALT");
    let mount = vike_tradehub_core::build_paper_multi_strategy_core_with(
        vec![vike_tradehub_core::StrategyMountSpec { strategy, spec: cfg.mount_spec() }],
        vike_tradehub_core::PaperCoreOpts {
            halt: vike_tradehub_core::PaperHalt::Pinned(sentinel),
            ..Default::default()
        },
    );
    let forwarder = std::sync::atomic::AtomicBool::new(false);
    assert!(super::tear_down_failed_start(mount.handle, Vec::new(), &forwarder));
    assert!(
        forwarder.load(std::sync::atomic::Ordering::Relaxed),
        "the forwarder is quiesced first"
    );
}

/// **A held-back feed needs the gatewayhub only for an account that arms through it.** The late arm's plan for
/// a survived feed refusal waits on the tradehub's own link to the gatewayhub only when a `demo`/`live` account of
/// a held venue would arm through it (it runs, or it waits for the gatewayhub); a venue with `paper` accounts
/// alone, or whose live rows are blocked for their own reasons, gains nothing from it, and a box with no gatewayhub
/// key must not wait for a link that can never answer.
#[test]
fn a_held_feed_needs_the_gatewayhub_only_for_an_account_that_arms_through_it() {
    let paper_only = table(vec![row(
        "deribit",
        AccountLabel::Default,
        AccountTier::Paper,
        Some(AccountTier::Paper),
        ArmingBlock::PaperTier,
    )]);
    assert!(!arms_through_gateway(&paper_only, &["deribit"]), "paper arms through nothing");

    let blocked_for_its_own_reasons = table(vec![
        row("deribit", AccountLabel::Default, AccountTier::Demo, None, ArmingBlock::NoCredentials),
        row(
            "deribit",
            AccountLabel::Default,
            AccountTier::Live,
            None,
            ArmingBlock::AccountInactive,
        ),
    ]);
    assert!(
        !arms_through_gateway(&blocked_for_its_own_reasons, &["deribit"]),
        "no credentials / not active: the gatewayhub answering arms nothing"
    );

    let waits_for_it = table(vec![row(
        "deribit",
        AccountLabel::Default,
        AccountTier::Demo,
        None,
        ArmingBlock::GatewayUnavailable,
    )]);
    assert!(arms_through_gateway(&waits_for_it, &["deribit"]));
    let runs = table(vec![row(
        "deribit",
        AccountLabel::Default,
        AccountTier::Live,
        Some(AccountTier::Live),
        ArmingBlock::None,
    )]);
    assert!(arms_through_gateway(&runs, &["deribit"]));
    assert!(
        !arms_through_gateway(&runs, &["binance"]),
        "another venue's account is not this feed's"
    );
    assert!(!arms_through_gateway(&runs, &[]), "no venue is held");
}

/// **A cap is survived on the same terms as a transient (nothing live runs on the venue) but is a different CLASS**:
/// it is never a transient (no restart waits for it), and a transient is never a cap. A cap of a venue with a
/// running live account is not survivable (the core cannot drop an engine) and ends the start with the status the
/// unit does not restart on; every other failure keeps the plain failure.
#[test]
fn a_cap_is_survived_like_a_transient_but_is_its_own_class() {
    use super::{failed_start_code, survivable_cap, survivable_refusal};
    let cap = "deribit subscribe_bars X: Subscribe(\"gatewayhub: already hosting 16 tick pumps (the daemon cap); \
         a cap, not a link: raise the cap or drop a subscription\")";
    let transient = "gatewayhub 127.0.0.1:1: the link is down; retried on the caller's next pass";
    let none: HashSet<String> = HashSet::new();
    let live: HashSet<String> = ["deribit#ALT".to_string()].into_iter().collect();

    assert!(survivable_cap(cap, "deribit", &none));
    assert!(!survivable_cap(cap, "deribit", &live), "a running live account cannot be held");
    assert!(!survivable_cap(transient, "deribit", &none), "a transient is not a cap");
    assert!(
        !survivable_refusal(cap, "deribit", &none),
        "a cap is no transient: no restart waits for it"
    );
    assert_eq!(failed_start_code(cap), Some(crate::late_arm::EXIT_OPERATOR_NEEDED));
    assert_eq!(failed_start_code(transient), None);
    assert_eq!(failed_start_code("refusing LIVE mount: a lease is held"), None);
}
