//! **Every `TradeLane` verb of the wrapper reaches the inner lane and answers what it answers** (review 15,
//! finding 4): `delegation_tests` reads names, this reads behaviour. Each case is chosen so that the
//! answer differs from what a wrapper that forgot the verb would give (the trait's default body, a
//! constant, a wrong generation): the bridge's own `start` is counted, a venue-confirmed book is
//! consumed by `record_identity`, `link_watch` answers for the one supervised venue, the health gate
//! reports a degraded feed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ExecOutcome, IdentityReport, LiveExec,
    Resolution, VenueAdapter, VenueDeclaration,
};
use vike_bridge_core::venue_adapter_fixture::PLANTED_DECLARATION;
use vike_gatewayhub_client::proto::{AdapterStart, PreflightStep};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::RefreshedTrade;
use crate::trade::adapters::registry::VenueRow;
use crate::trade::adapters::testutil::{NoopClient, snapshot_of};
use crate::trade::lane::{TradeLane, TradeTarget};

static STARTS: AtomicUsize = AtomicUsize::new(0);

/// An armed venue that comes UP, counts its starts and confirms a book (so `record_identity` has
/// something of the lane's to consume).
struct Up;

impl VenueAdapter for Up {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        STARTS.fetch_add(1, Ordering::SeqCst);
        AdapterOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(NoopClient),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon: None,
            identity: Some(IdentityReport {
                book: "the-confirmed-book".to_string(),
                evidence: "`userRole`",
                tier: AccountTier::Demo,
            }),
        }
    }
}

static UP: Up = Up;
static UP_REG: [VenueRow; 1] = [VenueRow::Adapter(&UP)];

struct Degraded;

impl crate::health::VenueHealth for Degraded {
    fn degraded(&self, venue: &str) -> Option<String> {
        (venue == "ig").then(|| "the feed is mid-gap".to_string())
    }
}

/// The verbs this test drives: it must be EXACTLY the trait's (checked against `lane.rs` below), so a
/// verb added to `TradeLane` without a behavioural case here fails.
const DRIVEN: [&str; 9] = [
    "venues",
    "account_tier",
    "arming_table",
    "preflight",
    "start_adapter",
    "record_identity",
    "resolves_by_coid",
    "link_watch",
    "recon_degraded",
];

fn start_args() -> AdapterStart {
    AdapterStart {
        rid: 1,
        symbol: "EURUSD".into(),
        declared_legs: Vec::new(),
        recon_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        route_key: "ig".into(),
    }
}

#[test]
fn every_verb_of_the_wrapper_reaches_the_inner_lane_and_answers_for_it() {
    let policy = vike_tradehub_core::AdapterPolicy::default().with_account(
        "ig",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    let lane =
        RefreshedTrade::new(&UP_REG, Box::new(move || (HashMap::new(), snapshot_of(&policy))))
            .with_health(Arc::new(Degraded));
    let target = TradeTarget { venue: "ig".to_string(), account: None, tier: AccountTier::Demo };

    // venues: the registry's adapter rows.
    assert_eq!(lane.venues(), ["ig"]);
    // account_tier: the account row's tier.
    assert_eq!(lane.account_tier("ig", None), Ok(AccountTier::Demo));
    // arming_table: the registry's venue with the account's tier.
    assert!(
        lane.arming_table()
            .rows
            .iter()
            .any(|r| r.venue == "ig" && r.tier == Some(AccountTier::Demo)),
        "the table is the inner lane's"
    );
    // preflight: the plan step answers venue lists, which is not what a default would say.
    assert!(lane.preflight(&["ig".to_string()], PreflightStep::Plan).plan.is_some());
    // start_adapter: the bridge's own start ran, once.
    let before = STARTS.load(Ordering::SeqCst);
    let (events, _rx) = vike_exec::event_channel(8);
    let started = lane.start_adapter(&target, &start_args(), events, None);
    assert!(started.is_ok(), "the planted venue comes up");
    assert_eq!(STARTS.load(Ordering::SeqCst), before + 1, "the bridge was started by the wrapper");
    // record_identity: consumes the book that start parked in the inner lane.
    assert_eq!(lane.current_confirmed_books(), 1);
    assert_eq!(lane.record_identity(&target, AccountTier::Demo, None), Ok(()));
    assert_eq!(lane.current_confirmed_books(), 0, "the inner lane recorded and forgot the book");
    // resolves_by_coid: the coverage table (bybit looks orders up by coid; binance does not).
    assert!(lane.resolves_by_coid("bybit"));
    assert!(!lane.resolves_by_coid("binance"));
    // link_watch: ibkr is the one supervised venue (the trait's default says `None` for all).
    let ibkr = TradeTarget { venue: "ibkr".to_string(), account: None, tier: AccountTier::Demo };
    assert!(lane.link_watch(&ibkr, &start_args()).is_some(), "ibkr's gateway is watched");
    assert!(lane.link_watch(&target, &start_args()).is_none());
    // recon_degraded: the health the lane was built with (the trait's default says `None`).
    assert_eq!(lane.recon_degraded("ig").as_deref(), Some("the feed is mid-gap"));
    assert_eq!(lane.recon_degraded("bybit"), None);
}

/// The driven list is the trait's verb list, no more and no fewer.
#[test]
fn the_behaviour_test_drives_exactly_the_verbs_of_the_trait() {
    let source = include_str!("../lane.rs");
    let body = source
        .split("pub trait TradeLane: Send + Sync {")
        .nth(1)
        .expect("the trait exists")
        .split("\n}\n")
        .next()
        .expect("the trait ends");
    let mut declared: Vec<String> = body
        .lines()
        .filter_map(|line| line.strip_prefix("    fn "))
        .filter_map(|rest| rest.split(['(', '<']).next())
        .map(str::to_string)
        .collect();
    declared.sort();
    let mut driven: Vec<String> = DRIVEN.iter().map(|v| (*v).to_string()).collect();
    driven.sort();
    assert_eq!(declared, driven, "a TradeLane verb without a behavioural case in this file");
}
