//! **A coin-margined MOUNT through the node's real remote-backend path** (decisions 0170 item 6 and 0258): the
//! contract value the gatewayhub's start states reaches the engine the node assembles, for every venue, and a
//! gatewayhub that cannot state one does not run a mount whose value the default would get wrong.
//!
//! * Over the gatewayhub's REAL server (its scripted trade lane answering the start with the bridge's value), the
//!   engine `make_engine_for_account` assembles through `VenueBackendGateway` holds okx's `BTCUSD.P` (`BTC-USD-SWAP`) at
//!   `Inverse(100)` and bybit's `BTCUSD.P` at `Inverse(1)`, and each is a COIN LEDGER (`Account::coin_key`) of its
//!   mount, settled in BTC. No venue list is read on this path: the stated value is carried as is.
//! * A gatewayhub that predates `gateway_trade_contract_value` answers every start with the field ABSENT, which reads
//!   `Linear` 1.0. The node refuses such a start (NOT RUNNING, `adapter failed`) when the catalog cannot vouch
//!   that the mounted name's value IS that default (`vike_catalog::contract_value`): okx's and bybit's inverse
//!   mounts are refused, a linear binance perpetual still starts.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::arming_fixtures::armed_fields;
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::{LoopbackGateway, TradeKit};
use vike_gatewayhub::testkit::rest::RestLaneScript;
use vike_gatewayhub::testkit::trade::{ScriptedStart, TradeLaneScript};
use vike_gatewayhub::trade::adapters::bridge_trade::TradeLaneBridge;
use vike_gatewayhub::trade::adapters::snapshot::TradeLaneSnapshot;
use vike_gatewayhub::trade::lane::TradeLane as _;
use vike_gatewayhub_client::proto::{
    ArmingRowWire, ArmingTableWire, FEATURE_GATEWAY_TRADE_CONTRACT_VALUE, Request, Response,
    trade_venue_feature,
};
use vike_gatewayhub_client::test_support::fake_trade_peer::{TradePeer, TradePeerConfig, link_to};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, ContractValue};
use vike_model::{ArmingBlock, VenueAdapterDeclarationView};
use vike_tradehub::venue_backend::gateway::VenueBackendGateway;
use vike_tradehub_core::test_support::account_book::keyed_policy;
use vike_tradehub_core::test_support::risk::budget;
use vike_tradehub_core::{
    AdapterEnv, AdapterPolicy, AdapterStartQuery, VenueBackend as _, VenueBackendError,
    make_engine_for_account,
};

const ID: &str = "vike-tradehub-inverse-mount";

/// The two coin-margined mounts and the value each bridge states for it.
const INVERSE_MOUNTS: [(&str, &str, f64); 2] =
    [("okx", "BTCUSD.P", 100.0), ("bybit", "BTCUSD.P", 1.0)];

/// A demo account of `venue` with the fields that arm its bridge.
fn policy(venue: &str) -> AdapterPolicy {
    keyed_policy(venue, AccountTier::Demo, armed_fields(venue))
}

/// The table the PRODUCTION lane (`TradeLaneBridge` over the real registry) serves for `policy`.
fn production_table(policy: &AdapterPolicy) -> ArmingTableWire {
    TradeLaneBridge::with_snapshot(
        REGISTRY,
        std::collections::HashMap::new(),
        TradeLaneSnapshot {
            accounts: policy.accounts.clone(),
            venue_settings: policy.venue_settings.clone(),
            venue_settings_unread: None,
            preflight_failed: policy.preflight_failed.clone(),
            ..TradeLaneSnapshot::default()
        },
    )
    .arming_table()
}

/// **The value the start states is the engine's, and an inverse one makes a coin ledger**, over the real server
/// and the real assembly, for okx and bybit alike.
#[test]
fn an_okx_and_a_bybit_inverse_mount_reach_the_engine_as_coin_ledgers() {
    for (venue, mount, unit) in INVERSE_MOUNTS {
        let policy = policy(venue);
        let value = ContractValue::inverse(unit);
        let gw = LoopbackGateway::start_trade_with(
            DataLaneScript::new(&[venue]),
            RestLaneScript::new(),
            TradeLaneScript::new(&[venue]).with_arming(production_table(&policy)).with_start(
                venue,
                None,
                ScriptedStart::live().contract_value(value),
            ),
            ServerConfig::PRODUCTION,
            TradeKit { state_dir: None, halt_path: None },
        );
        let backend = VenueBackendGateway::connect(
            gw.trade_link(),
            policy.accounts.clone(),
            format!("{ID}-{venue}"),
            Duration::from_secs(5),
        );
        let table = backend.arming_table().expect("the gatewayhub answers");
        let (tx, _rx) = vike_execution::event_channel(64);
        let mut live = HashSet::new();
        let risk = budget();
        let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
        env.policy = Some(&policy);
        env.risk_profile = Some(&risk);
        let (engine, _reconcile) =
            make_engine_for_account(&mut env, venue, mount, &AccountLabel::Default, &[])
                .unwrap_or_else(|e| panic!("{venue} {mount}: the engine runs: {e}"));
        assert_eq!(engine.tier, AccountTier::Demo, "{venue}");
        assert_eq!(engine.account.contract_value_of(mount), value, "{venue} {mount}");
        let coin = engine.account.coin_key().expect("an inverse mount is a coin ledger");
        assert_eq!((coin.0.as_str(), coin.1.as_str()), (venue, mount), "{venue}");
        assert_eq!(
            vike_catalog::inverse_settle_coin(venue, mount).as_deref(),
            Some("BTC"),
            "{venue} {mount}: its books are BTC"
        );
    }
}

/// An OLDER daemon: every feature string but `gateway_trade_contract_value`, serving `venues`. Its start answers
/// `Live` with the field absent (the default script), as a daemon that predates the field does.
fn older_daemon(venues: &[&str]) -> TradePeer {
    let mut config = TradePeerConfig::new();
    config.features.retain(|f| f != FEATURE_GATEWAY_TRADE_CONTRACT_VALUE);
    config.features.extend(venues.iter().map(|v| trade_venue_feature(v)));
    let rows: Vec<ArmingRowWire> = venues
        .iter()
        .enumerate()
        .map(|(n, venue)| ArmingRowWire {
            venue: (*venue).to_string(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![n as i64 + 1],
            book: None,
        })
        .collect();
    let facts: BTreeMap<String, VenueAdapterDeclarationView> = venues
        .iter()
        .map(|venue| {
            let facts = VenueAdapterDeclarationView {
                adapter_present: true,
                addresses_accounts: true,
                takes_reconcile_trigger: false,
                grid_source: vike_model::GridSource::NoGrid,
                process_exclusive: None,
                clock_declaration_word: vike_model::ClockDeclarationKind::Wired,
            };
            ((*venue).to_string(), facts)
        })
        .collect();
    TradePeer::start(config.with_rpc(move |req| match req {
        Request::ArmingQuery => vec![Response::ArmingTable(Box::new(ArmingTableWire {
            properties: facts.clone(),
            rows: rows.clone(),
            key_changed: Vec::new(),
            start_inputs: Default::default(),
        }))],
        other => vec![Response::Error(format!("unscripted: {other:?}"))],
    }))
}

fn start_on(
    backend: &VenueBackendGateway,
    venue: &str,
    mount: &str,
) -> Result<vike_tradehub_core::AdapterBind, VenueBackendError> {
    let label = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue,
        account: &label,
        tier: AccountTier::Demo,
        symbol: mount,
        declared_legs: &[],
        reconcile_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        account_leverage: None,
        route_key: venue,
        properties_rec: None,
    };
    let (events, _lane) = vike_gatewayhub_client::test_support::fake_trade_peer::lane();
    backend.start_adapter(query, &events, None)
}

/// **A daemon that cannot state the value does not run a mount the default would value wrong**, whichever venue:
/// okx's and bybit's inverse mounts are NOT RUNNING (`adapter failed`), and a linear binance perpetual, whose
/// default value is right, still starts. The rule is the catalog's word on the mounted name, not a venue list.
///
/// Base: okx and bybit started on such a daemon (the list named deribit and ibkr only), their engines linear.
#[test]
fn a_daemon_that_predates_contract_value_does_not_run_an_inverse_mount() {
    let venues = ["okx", "bybit", "binance"];
    let peer = older_daemon(&venues);
    let accounts = venues
        .iter()
        .fold(AdapterPolicy::default(), |p, venue| {
            p.with_account(venue, &AccountLabel::Default, AccountTier::Demo)
        })
        .accounts;
    let backend = VenueBackendGateway::connect(
        link_to(&peer.address_string()),
        accounts,
        ID.to_string(),
        Duration::from_secs(5),
    );
    for (venue, mount, _) in INVERSE_MOUNTS {
        match start_on(&backend, venue, mount) {
            Err(e) => {
                assert_eq!(e, VenueBackendError::NotRunning(ArmingBlock::AdapterFailed), "{venue}")
            }
            Ok(bind) => panic!(
                "{venue} {mount}: started on a daemon that cannot state its value, at {:?}",
                bind.contract_value
            ),
        }
    }
    let linear = start_on(&backend, "binance", "BTCUSDT.P").expect("a linear mount still starts");
    assert_eq!(linear.contract_value, None, "the default, which is right for it");
}
