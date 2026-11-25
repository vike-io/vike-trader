//! **Every `TradeLane` verb of the wrapper reaches the inner lane and answers what it answers** (review 15,
//! finding 4): `delegation_tests` reads names, this reads behaviour. Each case is chosen so that the
//! answer differs from what a wrapper that forgot the verb would give (the trait's default body, a
//! constant, a wrong generation): the bridge's own `start` is counted, a venue-confirmed book is
//! consumed by `record_identity`, `link_watch` answers for the bridge that declares a supervised session, the health gate
//! reports a degraded feed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ArmingResolution, BookConfirmed, VenueAdapter,
    VenueAdapterDeclaration, VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning,
};
use vike_bridge_core::venue_adapter_fixture::PLANTED_DECLARATION;
use vike_gatewayhub_client::proto::{AdapterStart, PreflightStep};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, Contract, ContractPerp, Instrument, Margining, SymbolProperties};

use super::RefreshedTrade;
use crate::trade::adapters::registry::VenueAdapterRow;
use crate::trade::adapters::symbol_facts::{InstrumentList, SymbolPropertiesCache};
use crate::trade::adapters::testutil::snapshot_of;
use crate::trade::lane::{TradeLane, TradeLaneTarget, account_currency_wire};
use vike_tradehub_core::test_support::doubles::NoopClient;

static STARTS: AtomicUsize = AtomicUsize::new(0);

/// An armed venue that comes UP, counts its starts and confirms a book (so `record_identity` has
/// something of the lane's to consume).
struct Up;

impl VenueAdapter for Up {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        VenueAdapterDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        STARTS.fetch_add(1, Ordering::SeqCst);
        AdapterOutcome {
            exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
                client: Box::new(NoopClient),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            reconcile: None,
            identity: Some(BookConfirmed {
                book: "the-confirmed-book".to_string(),
                evidence: "`userRole`",
                tier: AccountTier::Demo,
            }),
        }
    }
    /// Declares a supervised session, which `link_watch` never asks for: ig's connection row heals its own session.
    fn link_probe(
        &self,
        _inputs: &AdapterInputs<'_>,
        _route_key: &str,
    ) -> Option<Box<dyn vike_bridge_core::venue_link::VenueLinkProbe>> {
        Some(Box::new(Answers))
    }
}

/// A probe that always answers, on a one-second cadence.
#[derive(Debug)]
struct Answers;

impl vike_bridge_core::venue_link::VenueLinkProbe for Answers {
    fn reachable(&self) -> Result<(), String> {
        Ok(())
    }
    fn probe_every(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
    fn first_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
    fn max_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
}

static UP: Up = Up;
static UP_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&UP)];

/// A bridge on a venue whose connection row is `Supervised` (fxcm's) that declares a probe, so `link_watch` has
/// something of the inner lane's to answer (the trait's default is `None` for every venue).
struct Supervises;

impl VenueAdapter for Supervises {
    fn venue(&self) -> &'static str {
        "fxcm"
    }
    fn declaration(&self) -> VenueAdapterDeclaration {
        PLANTED_DECLARATION
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> ArmingResolution {
        ArmingResolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome::paper()
    }
    fn link_probe(
        &self,
        _inputs: &AdapterInputs<'_>,
        _route_key: &str,
    ) -> Option<Box<dyn vike_bridge_core::venue_link::VenueLinkProbe>> {
        Some(Box::new(Answers))
    }
}

static SUPERVISES: Supervises = Supervises;
static SUPERVISES_REG: [VenueAdapterRow; 1] = [VenueAdapterRow::Adapter(&SUPERVISES)];

struct Degraded;

impl crate::health::VenueHealth for Degraded {
    fn degraded(&self, venue: &str) -> Option<String> {
        (venue == "ig").then(|| "the feed is mid-gap".to_string())
    }
}

/// The verbs this test drives: it must be EXACTLY the trait's (checked against `lane.rs` below), so a
/// verb added to `TradeLane` without a behavioural case here fails.
const DRIVEN: [&str; 25] = [
    "venues",
    "account_tier",
    "row_of",
    "row_label",
    "signs_with_row",
    "arming_table",
    "preflight",
    "start_adapter",
    "record_identity",
    "resolves_by_order_client_id",
    "link_watch",
    "venue_link",
    "reconcile_degraded",
    "credentials_changed",
    "symbol_facts",
    "symbol_facts_quoted",
    "account_currency",
    "account_currency_stated",
    "currency_read_is_budgeted",
    "native_time_release",
    "broker_code_len",
    "balance_withheld",
    "leverage_cap",
    "symbol_leverage",
    "venue_read",
];

fn start_args() -> AdapterStart {
    crate::trade::adapters::testutil::ig_start(1)
}

/// Binance account row 8 under `label`: the parent (`None`), or its derived lane twin (`PERP`), same id.
fn binance_row(label: Option<&str>) -> vike_secrets::AccountRow {
    vike_secrets::AccountRow {
        id: 8,
        venue: "binance".to_string(),
        tier: AccountTier::Demo,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: None,
        facts: Default::default(),
    }
}

#[test]
fn every_verb_of_the_wrapper_reaches_the_inner_lane_and_answers_for_it() {
    let policy = vike_tradehub_core::AdapterPolicy::default()
        .with_account("ig", &AccountLabel::Default, AccountTier::Demo)
        .with_account_row(vike_secrets::AccountRow {
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
            symbol: vike_secrets::fixture_account_symbol("bybit").map(str::to_string),
            facts: Default::default(),
        })
        // A binance parent (row 8) and the lane twin derived from it, which carries the parent's id and keys.
        .with_account_row(binance_row(None))
        .with_account_row(binance_row(Some("PERP")));
    let lane =
        RefreshedTrade::new(&UP_REG, Box::new(move || (HashMap::new(), snapshot_of(&policy))))
            .with_health(Arc::new(Degraded))
            .with_symbol_facts(Arc::new(SymbolPropertiesCache::new(solusdt_list())));
    let target =
        TradeLaneTarget { venue: "ig".to_string(), account: None, tier: AccountTier::Demo };

    // venues: the registry's adapter rows.
    assert_eq!(lane.venues(), ["ig"]);
    // account_tier: the account row's tier.
    assert_eq!(lane.account_tier("ig", None), Ok(AccountTier::Demo));
    // row_of / row_label: the ig account runs on row 1, the default account (the defaults say `None`).
    assert_eq!(lane.row_of("ig", None, AccountTier::Demo), Some(1));
    assert_eq!(lane.row_label("ig", 1), Some(None));
    // signs_with_row: the row itself, and the twin that inherits its keys (the default, by `row_label`, says no to it).
    assert!(lane.signs_with_row("ig", None, 1));
    assert!(
        lane.signs_with_row("binance", Some("PERP"), 8),
        "the twin signs with its parent's keys"
    );
    assert!(!lane.signs_with_row("binance", Some("PERP"), 7), "not another row's");
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
    let (events, _rx) = vike_execution::event_channel(8);
    let started = lane.start_adapter(&target, &start_args(), events, None);
    assert!(started.is_ok(), "the planted venue comes up");
    assert_eq!(STARTS.load(Ordering::SeqCst), before + 1, "the bridge was started by the wrapper");
    // record_identity: consumes the book that start parked in the inner lane.
    assert_eq!(lane.current_confirmed_books(), 1);
    assert_eq!(lane.record_identity(&target, AccountTier::Demo, None), Ok(()));
    assert_eq!(lane.current_confirmed_books(), 0, "the inner lane recorded and forgot the book");
    // credentials_changed: the inner lane forgets the key set the start recorded (the trait's default forgets nothing).
    assert_eq!(lane.current_started_keys(), 1, "the start recorded its key set");
    lane.credentials_changed(&target);
    assert_eq!(lane.current_started_keys(), 0, "the inner lane forgot it");
    // resolves_by_order_client_id: the coverage table (bybit looks orders up by order client id; deribit does not).
    assert!(lane.resolves_by_order_client_id("bybit"));
    assert!(!lane.resolves_by_order_client_id("deribit"));
    // link_watch: the planted fxcm bridge declares a supervised session on a `Supervised` connection row (the trait's
    // default says `None` for all); ig's bridge declares one too, but its row heals itself; and a venue with no row in
    // the inner lane's table has none.
    let none = vike_tradehub_core::AdapterPolicy::default();
    let supervising = RefreshedTrade::new(
        &SUPERVISES_REG,
        Box::new(move || (HashMap::new(), snapshot_of(&none))),
    );
    let fxcm_target =
        TradeLaneTarget { venue: "fxcm".to_string(), account: None, tier: AccountTier::Demo };
    assert!(
        supervising.link_watch(&fxcm_target, &start_args()).is_some(),
        "the planted bridge's session"
    );
    assert!(lane.link_watch(&target, &start_args()).is_none(), "ig's row is not supervised");
    let ibkr =
        TradeLaneTarget { venue: "ibkr".to_string(), account: None, tier: AccountTier::Demo };
    assert!(lane.link_watch(&ibkr, &start_args()).is_none(), "no row, no watch");
    // venue_link: a venue whose connection row reports a path is reported (the trait's default says `None` for all), and
    // a venue string off the roster is not.
    let dukascopy =
        TradeLaneTarget { venue: "dukascopy".to_string(), account: None, tier: AccountTier::Demo };
    assert!(
        lane.venue_link(&dukascopy, &start_args()).is_some(),
        "the sidecar's state is reported"
    );
    let stranger =
        TradeLaneTarget { venue: "sim".to_string(), account: None, tier: AccountTier::Demo };
    assert!(
        lane.venue_link(&stranger, &start_args()).is_none(),
        "an unknown venue reports no path"
    );
    // reconcile_degraded: the health the lane was built with (the trait's default says `None`).
    assert_eq!(lane.reconcile_degraded("ig").as_deref(), Some("the feed is mid-gap"));
    assert_eq!(lane.reconcile_degraded("bybit"), None);
    // symbol_facts: the inner lane's source answers (the trait's default is an Err for every symbol).
    let bybit =
        TradeLaneTarget { venue: "bybit".to_string(), account: None, tier: AccountTier::Demo };
    // the list's linear perpetual by its user name (decision 0290; the bare `SOLUSDT` is the spot pair)
    let facts = lane.symbol_facts(&bybit, "SOLUSDT.P").expect("the inner lane's facts");
    assert_eq!(facts.properties.tick_size, 0.001);
    assert!(lane.symbol_facts(&target, "SOLUSDT").is_err(), "a venue it does not serve");
    // symbol_facts_quoted (decision 0183): a venue with no rate source answers what `symbol_facts` does, so the
    // inner lane's source answers for bybit and ig is still refused (the trait's default is that same answer).
    let quoted = lane.symbol_facts_quoted(&bybit, "SOLUSDT.P").expect("the inner lane's facts");
    assert_eq!(quoted, facts, "no rate source: exactly the plain answer");
    assert!(lane.symbol_facts_quoted(&target, "SOLUSDT").is_err(), "a venue it does not serve");
    // account_currency: only oanda's and ctrader's lanes read one, and this registry carries no oanda adapter
    // to read it with, so the inner lane says nothing (which is also the trait's default).
    let oanda =
        TradeLaneTarget { venue: "oanda".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.account_currency(&oanda, &start_args()), None);
    // ...and ctrader's lane reads its deposit asset the same way (no ctrader adapter here either).
    let ctrader =
        TradeLaneTarget { venue: "ctrader".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.account_currency(&ctrader, &start_args()), None);
    assert!(lane.symbol_facts_quoted(&ctrader, "USDJPY").is_err(), "no ctrader adapter to ask");
    assert_eq!(lane.account_currency(&target, &start_args()), None, "ig states none here");
    // account_currency_stated: the wrapper forwards the inner lane's arm. A converting venue whose read comes back
    // empty is the owner's USD, ASSUMED (here oanda: this registry carries no oanda adapter to read it with; ig is
    // not asked while the start does not ask the budgeted read), and so is fxcm's: with no reconcile session behind
    // the default account the API cannot be asked (a non-demo tier states nothing). Compared as the wire writes it.
    let told = |target: &TradeLaneTarget, start: &AdapterStart| {
        account_currency_wire(lane.account_currency_stated(target, start).as_ref())
    };
    assert_eq!(told(&oanda, &start_args()), (Some("USD".to_string()), true));
    // ...and the balance of an account whose currency is only assumed is withheld, through the wrapper too
    // (oanda's memory is the one just written; an unnamed venue withholds nothing)
    assert!(lane.balance_withheld(&oanda).is_some_and(|why| why.contains("only ASSUMED")));
    assert_eq!(lane.balance_withheld(&bybit), None);
    assert_eq!(told(&target, &start_args()), (None, false), "ig states none here");
    let fxcm =
        TradeLaneTarget { venue: "fxcm".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(told(&fxcm, &start_args()), (Some("USD".to_string()), true));
    // The lane takes no mount into account (the node's fence judges it): the same answer for a yen-mounted start.
    let yen = AdapterStart { symbol: "USDJPY".into(), ..start_args() };
    assert_eq!(told(&fxcm, &yen), (Some("USD".to_string()), true));
    let fxcm_live = TradeLaneTarget { tier: AccountTier::Live, ..fxcm };
    assert_eq!(told(&fxcm_live, &start_args()), (None, false));
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
            TradeLaneTarget { venue: venue.to_string(), account: None, tier: AccountTier::Demo };
        assert_eq!(lane.currency_read_is_budgeted(&target), budgeted, "{venue}");
    }
    // native_time_release (decision 0267's 2.6): the wrapper forwards the inner lane's answer; only an ibkr account
    // whose rows resolve to the socket backend says `true`, and no account here holds an ibkr ACCOUNT field, so every
    // venue answers `false`, which is also the trait's default.
    for venue in ["ibkr", "ig", "bybit"] {
        let target =
            TradeLaneTarget { venue: venue.to_string(), account: None, tier: AccountTier::Demo };
        assert!(!lane.native_time_release(&target, &start_args()), "{venue}");
    }
    // broker_code_len (decision 0270): the wrapper forwards the inner lane's answer, the length of an order-client-id-prefix
    // venue's broker code in the generation's deployment rows: none here (`0`, also the trait's default), and a
    // generation that holds one answers its length (`8`), for binance alone.
    let binance =
        TradeLaneTarget { venue: "binance".to_string(), account: None, tier: AccountTier::Demo };
    assert_eq!(lane.broker_code_len(&binance, &start_args()), 0);
    let coded = RefreshedTrade::new(
        &UP_REG,
        Box::new(|| {
            let vars = HashMap::from([("BINANCE_BROKER_CODE".to_string(), "Q7w2Lk9P".to_string())]);
            (vars, snapshot_of(&vike_tradehub_core::AdapterPolicy::default()))
        }),
    );
    assert_eq!(coded.broker_code_len(&binance, &start_args()), 8);
    assert_eq!(
        coded.broker_code_len(&bybit, &start_args()),
        0,
        "no order-client-id-prefix attribution"
    );
    // leverage_cap: no account row states a leverage (and ig is no confirmable venue), so the inner lane
    // asks no venue and answers `None`, which is also the trait's default (decision 0181).
    let hyperliquid = TradeLaneTarget {
        venue: "hyperliquid".to_string(),
        account: None,
        tier: AccountTier::Demo,
    };
    assert_eq!(lane.leverage_cap(&target, &start_args()), None);
    assert_eq!(lane.leverage_cap(&hyperliquid, &start_args()), None, "no row states one");
    // symbol_leverage: bybit's demo account states 10x and its running start asked it, but this registry
    // carries no bybit adapter to write with, so the inner lane answers an error of its own (the trait's
    // default says `None`); ig is no posting venue and asks nothing. The symbol is bybit's PERPETUAL
    // (`ETHUSDT.P`): its bare `ETHUSDT` is the spot pair (decision 0290, `vike_catalog::names_spot_pair`),
    // which has no leverage and gets no figure.
    let asked = AdapterStart {
        symbol: "BTCUSDT".into(),
        route_key: "bybit".into(),
        account_leverage: Some(10.0),
        ..start_args()
    };
    let got = lane.symbol_leverage(&bybit, &asked, "ETHUSDT.P");
    assert!(matches!(&got, Some(Err(why)) if why.contains("no adapter")), "{got:?}");
    assert_eq!(lane.symbol_leverage(&target, &start_args(), "SOLUSDT"), None, "ig asks no figure");
    // venue_read: the inner lane answers. This registry carries no bybit adapter, so the inner lane says so in
    // its own words (the trait's default would say `not offered by bybit`); ig's planted bridge implements none
    // and answers the one `not offered` sentence.
    use vike_model::VenueReadKind::Balance;
    let read = lane.venue_read(&bybit, Balance);
    assert!(matches!(&read, Err(why) if why.text().contains("carries no adapter")), "{read:?}");
    assert_eq!(
        lane.venue_read(&target, Balance),
        Err(vike_model::not_offered_words("ig", Balance).into())
    );
}

/// A bybit instrument list holding one linear perpetual.
fn solusdt_list() -> InstrumentList {
    Arc::new(|_venue| {
        let perp = Contract::Perp(ContractPerp {
            margining: Margining::Linear,
            settle: "USDT".into(),
            value_currency: None,
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
