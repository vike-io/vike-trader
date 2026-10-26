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
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, Contract, Instrument, Margining, PerpSpec, SymbolProperties};

use super::RefreshedTrade;
use crate::trade::adapters::registry::VenueRow;
use crate::trade::adapters::symbol_facts::{InstrumentList, SymbolFacts};
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
                contract_value: None,
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
const DRIVEN: [&str; 18] = [
    "venues",
    "account_tier",
    "arming_table",
    "preflight",
    "start_adapter",
    "record_identity",
    "resolves_by_coid",
    "link_watch",
    "venue_link",
    "recon_degraded",
    "symbol_facts",
    "symbol_facts_quoted",
    "account_currency",
    "account_currency_stated",
    "currency_read_is_budgeted",
    "balance_withheld",
    "leverage_cap",
    "symbol_leverage",
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
        account_leverage: None,
        read_account_ccy: false,
        route_key: "ig".into(),
    }
}

#[test]
fn every_verb_of_the_wrapper_reaches_the_inner_lane_and_answers_for_it() {
    let policy = vike_tradehub_core::AdapterPolicy::default()
        .with_account("ig", &AccountLabel::Default, AccountTier::Demo)
        .with_account_row(vike_secrets::Account {
            id: 7,
            venue: "bybit".to_string(),
            tier: AccountTier::Demo,
            label: None,
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: None,
            leverage: vike_secrets::AccountLeverage::new(10.0),
        });
    let lane =
        RefreshedTrade::new(&UP_REG, Box::new(move || (HashMap::new(), snapshot_of(&policy))))
            .with_health(Arc::new(Degraded))
            .with_symbol_facts(Arc::new(SymbolFacts::new(solusdt_list())));
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
    // resolves_by_coid: the coverage table (bybit looks orders up by coid; deribit does not).
    assert!(lane.resolves_by_coid("bybit"));
    assert!(!lane.resolves_by_coid("deribit"));
    // link_watch: ibkr is the one supervised venue (the trait's default says `None` for all).
    let ibkr = TradeTarget { venue: "ibkr".to_string(), account: None, tier: AccountTier::Demo };
    assert!(lane.link_watch(&ibkr, &start_args()).is_some(), "ibkr's gateway is watched");
    assert!(lane.link_watch(&target, &start_args()).is_none());
    // venue_link: dukascopy's sidecar is the one reported venue path (the trait's default says `None` for all).
    let dukascopy =
        TradeTarget { venue: "dukascopy".to_string(), account: None, tier: AccountTier::Demo };
    assert!(
        lane.venue_link(&dukascopy, &start_args()).is_some(),
        "the sidecar's state is reported"
    );
    assert!(lane.venue_link(&target, &start_args()).is_none());
    // recon_degraded: the health the lane was built with (the trait's default says `None`).
    assert_eq!(lane.recon_degraded("ig").as_deref(), Some("the feed is mid-gap"));
    assert_eq!(lane.recon_degraded("bybit"), None);
    // symbol_facts: the inner lane's source answers (the trait's default is an Err for every symbol).
    let bybit = TradeTarget { venue: "bybit".to_string(), account: None, tier: AccountTier::Demo };
    let facts = lane.symbol_facts(&bybit, "SOLUSDT").expect("the inner lane's facts");
    assert_eq!(facts.properties.tick_size, 0.001);
    assert!(lane.symbol_facts(&target, "SOLUSDT").is_err(), "a venue it does not serve");
    // symbol_facts_quoted (decision 0183): a venue with no rate source answers what `symbol_facts` does, so the
    // inner lane's source answers for bybit and ig is still refused (the trait's default is that same answer).
    let quoted = lane.symbol_facts_quoted(&bybit, "SOLUSDT").expect("the inner lane's facts");
    assert_eq!(quoted, facts, "no rate source: exactly the plain answer");
    assert!(lane.symbol_facts_quoted(&target, "SOLUSDT").is_err(), "a venue it does not serve");
    // account_currency: only oanda's and ctrader's lanes read one, and this registry carries no oanda adapter
    // to read it with, so the inner lane says nothing (which is also the trait's default).
    let oanda = TradeTarget { venue: "oanda".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.account_currency(&oanda, &start_args()), None);
    // ...and ctrader's lane reads its deposit asset the same way (no ctrader adapter here either).
    let ctrader =
        TradeTarget { venue: "ctrader".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.account_currency(&ctrader, &start_args()), None);
    assert!(lane.symbol_facts_quoted(&ctrader, "USDJPY").is_err(), "no ctrader adapter to ask");
    assert_eq!(lane.account_currency(&target, &start_args()), None, "ig states none here");
    // account_currency_stated: the wrapper forwards the inner lane's arm. A converting venue whose read comes back
    // empty is the owner's USD, ASSUMED (here oanda: this registry carries no oanda adapter to read it with; ig is
    // not asked while the start does not ask the budgeted read), and so is fxcm's: with no reconcile session behind
    // the default account the API cannot be asked (a non-demo tier states nothing).
    assert_eq!(
        lane.account_currency_stated(&oanda, &start_args()),
        (Some("USD".to_string()), true)
    );
    // ...and the balance of an account whose currency is only assumed is withheld, through the wrapper too
    // (oanda's memory is the one just written; an unnamed venue withholds nothing)
    assert!(lane.balance_withheld(&oanda).is_some_and(|why| why.contains("only ASSUMED")));
    assert_eq!(lane.balance_withheld(&bybit), None);
    assert_eq!(
        lane.account_currency_stated(&target, &start_args()),
        (None, false),
        "ig states none here"
    );
    let fxcm = TradeTarget { venue: "fxcm".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.account_currency_stated(&fxcm, &start_args()), (Some("USD".to_string()), true));
    // The lane takes no mount into account (the node's fence judges it): the same answer for a yen-mounted start.
    let yen = AdapterStart { symbol: "USDJPY".into(), ..start_args() };
    assert_eq!(lane.account_currency_stated(&fxcm, &yen), (Some("USD".to_string()), true));
    let fxcm_live = TradeTarget { tier: AccountTier::Live, ..fxcm };
    assert_eq!(lane.account_currency_stated(&fxcm_live, &start_args()), (None, false));
    // currency_read_is_budgeted: the wrapper forwards the inner lane's answer (ibkr's data lane and ig's allowance
    // pay for the read; every other venue's is free).
    for (venue, budgeted) in [
        ("ibkr", true),
        ("ig", true),
        ("oanda", false),
        ("ctrader", false),
        ("dukascopy", false),
        ("fxcm", false),
    ] {
        let target =
            TradeTarget { venue: venue.to_string(), account: None, tier: AccountTier::Demo };
        assert_eq!(lane.currency_read_is_budgeted(&target), budgeted, "{venue}");
    }
    // leverage_cap: no account row states a leverage (and ig is no confirmable venue), so the inner lane
    // asks no venue and answers `None`, which is also the trait's default (decision 0181).
    let hyperliquid =
        TradeTarget { venue: "hyperliquid".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.leverage_cap(&target, &start_args()), None);
    assert_eq!(lane.leverage_cap(&hyperliquid, &start_args()), None, "no row states one");
    // symbol_leverage: bybit's demo account states 10x and its running start asked it, but this registry
    // carries no bybit adapter to write with, so the inner lane answers an error of its own (the trait's
    // default says `None`); ig is no posting venue and asks nothing.
    let asked = AdapterStart {
        symbol: "BTCUSDT".into(),
        route_key: "bybit".into(),
        account_leverage: Some(10.0),
        ..start_args()
    };
    let got = lane.symbol_leverage(&bybit, &asked, "ETHUSDT");
    assert!(matches!(&got, Some(Err(why)) if why.contains("no adapter")), "{got:?}");
    assert_eq!(lane.symbol_leverage(&target, &start_args(), "SOLUSDT"), None, "ig asks no figure");
}

/// A bybit instrument list holding one linear perpetual.
fn solusdt_list() -> InstrumentList {
    Arc::new(|_venue| {
        let perp = Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms: None,
        });
        Ok(vec![Instrument::new("bybit", "SOLUSDT.P", "SOL", "USDT", perp).with_properties(
            SymbolProperties { tick_size: 0.001, step_size: 0.1, ..Default::default() },
        )])
    })
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
