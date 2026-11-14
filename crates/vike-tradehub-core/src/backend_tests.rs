//! The venue port, judged apart from any venue: `VenueBackendPaper`'s rows, and the fold's side of the
//! seam driven through a `VenueBackendScript` — WHEN the fold asks the backend (the budget refusal
//! before the start, the identity record after the fee schedule, a stop word that reaches the
//! start) and what it does with each answer.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use vike_config::ArmingAccount;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, ArmingBlock, GridSource, VenueAdapterDeclarationView, Withhold};
use vike_strategies_check::makers::spread_maker::SpreadMaker;

use super::*;
use crate::empty_report_fetches;
use crate::test_support::doubles::NoopClient;
use crate::test_support::risk::budget;
use crate::{
    AdapterEnv, AdapterPolicy, EngineError, make_engine_for_account, stopped_by_the_live_switch,
};

// ---- VenueBackendPaper ----------------------------------------------------------------------------

fn paper_rows(policy: &AdapterPolicy) -> Vec<ArmingAccount> {
    let table = VenueBackendPaper::new(&policy.accounts).arming_table().expect("it always answers");
    crate::venue_arming(&table, policy)
}

/// A paper box: a `paper` account runs the simulator, a `demo` or `live` one is NOT RUNNING with the
/// switch's own word — never a paper stand-in — and an account the table refuses keeps its cause.
#[test]
fn the_paper_backend_runs_paper_and_stops_everything_else() {
    let policy = AdapterPolicy::default()
        .with_account("binance", &AccountLabel::Default, AccountTier::Paper)
        .with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
        .with_account("okx", &AccountLabel::Default, AccountTier::Live);
    let rows = paper_rows(&policy);
    let of = |venue: &str| {
        let r = rows.iter().find(|r| r.venue == venue && r.is_default_account()).expect("a row");
        (r.tier, r.effective, r.block)
    };
    assert_eq!(
        of("binance"),
        (Some(AccountTier::Paper), Some(AccountTier::Paper), ArmingBlock::PaperTier)
    );
    assert_eq!(of("bybit"), (Some(AccountTier::Demo), None, ArmingBlock::LiveSwitchOff));
    assert_eq!(of("okx"), (Some(AccountTier::Live), None, ArmingBlock::LiveSwitchOff));
    assert_eq!(of("deribit"), (None, None, ArmingBlock::NoAccountRow), "no row at all");
}

/// The paper backend's stops ARE `stopped_by_the_live_switch`'s: one answer for the switch, two
/// readers (the paper arm publishes it, the backend's table shows it).
#[test]
fn the_paper_backend_agrees_with_the_live_switch_stop() {
    let policy = AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
        .with_account("bybit", &AccountLabel::parse("ALT").unwrap(), AccountTier::Live)
        .with_account("okx", &AccountLabel::Default, AccountTier::Paper);
    let from_table: Vec<(String, AccountLabel)> = paper_rows(&policy)
        .iter()
        .filter(|r| r.block == ArmingBlock::LiveSwitchOff)
        .map(|r| (r.venue.to_string(), r.label.clone()))
        .collect();
    let stopped: Vec<(String, AccountLabel)> =
        stopped_by_the_live_switch(vike_model::VENUES, &policy)
            .into_iter()
            .filter(|n| n.block == ArmingBlock::LiveSwitchOff)
            .map(|n| (n.venue, n.account))
            .collect();
    assert!(!stopped.is_empty(), "premise: the policy stops something");
    let mut a = from_table;
    let mut b = stopped;
    a.sort();
    b.sort();
    assert_eq!(a, b);
}

/// The paper backend dials nothing: every venue's adapter is "present" (the wiring table's engine
/// rows are the wired markets), it starts no client, and a start is the switch's stop.
#[test]
fn the_paper_backend_dials_nothing() {
    let backend = VenueBackendPaper::new(&AdapterPolicy::default().accounts);
    let table = backend.arming_table().expect("it always answers");
    assert_eq!(table.wired_markets(), crate::wired_markets(), "every engine row is wired");
    let (tx, _rx) = vike_execution::event_channel(4);
    let account = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue: "bybit",
        account: &account,
        tier: AccountTier::Demo,
        symbol: "BTCUSDT",
        declared_legs: &[],
        reconcile_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        route_key: "bybit",
        properties_rec: None,
    };
    assert_eq!(
        backend.start_adapter(query, &tx, None).unwrap_err(),
        VenueBackendError::NotRunning(ArmingBlock::LiveSwitchOff)
    );
    assert!(
        backend
            .preflight_plan(&PreflightQuery { clock_venues: &[] })
            .unwrap()
            .credential_venues
            .is_empty()
    );
    assert_eq!(backend.net_reading(), NetReading::Unknown);
}

// ---- the fold's side of the seam, over a VenueBackendScript -------------------------------------

/// One default-account row of `bybit` in a table that says the venue's adapter is present.
fn bybit_table(
    tier: Option<AccountTier>,
    effective: Option<AccountTier>,
    block: ArmingBlock,
) -> ArmingTable {
    venue_table("bybit", tier, effective, block)
}

/// [`bybit_table`] for any `venue`.
fn venue_table(
    venue: &'static str,
    tier: Option<AccountTier>,
    effective: Option<AccountTier>,
    block: ArmingBlock,
) -> ArmingTable {
    let facts = BTreeMap::from([(
        venue.to_string(),
        VenueAdapterDeclarationView {
            adapter_present: true,
            addresses_accounts: true,
            takes_reconcile_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_declaration_word: vike_model::ClockDeclarationKind::NotWired,
        },
    )]);
    ArmingTable::new(
        facts,
        vec![ArmingRow {
            arming: ArmingAccount {
                venue,
                label: AccountLabel::Default,
                tier,
                effective,
                block,
                account_ids: vec![1],
            },
            book: None,
        }],
    )
}

/// A reconcile client that logs the order its two optional reads are made in.
struct OrderedReconcile(Arc<Mutex<Vec<&'static str>>>);

impl vike_execution::recon::ReconcileClient for OrderedReconcile {
    empty_report_fetches!();
    fn fetch_fee_rates(&self) -> Result<Option<vike_model::FeeSchedule>, String> {
        self.0.lock().expect("log").push("fee_rates");
        Ok(None)
    }
    fn fetch_account_identity(&self) -> Result<Option<String>, String> {
        self.0.lock().expect("log").push("account_identity");
        Ok(None)
    }
}

fn bind(
    bound_tier: AccountTier,
    recon: Option<Box<dyn vike_execution::recon::ReconcileClient>>,
) -> AdapterBind {
    AdapterBind {
        client: Box::new(NoopClient),
        reconcile: recon,
        bound_tier,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        leverage_cap: None,
        account_currency: None,
        account_currency_assumed: false,
        native_time_release: false,
        broker_code_len: 0,
        mount_symbol: None,
    }
}

/// One start of bybit's default account through the fold over `backend` and `table`.
fn start(
    backend: &VenueBackendScript,
    table: &ArmingTable,
    policy: &AdapterPolicy,
    risk: Option<&vike_model::ProfileRisk>,
) -> Result<crate::EngineAndReconcile, EngineError> {
    start_on("bybit", backend, table, policy, risk)
}

/// [`start`] for any `venue` the table and the backend script know.
fn start_on(
    venue: &str,
    backend: &VenueBackendScript,
    table: &ArmingTable,
    policy: &AdapterPolicy,
    risk: Option<&vike_model::ProfileRisk>,
) -> Result<crate::EngineAndReconcile, EngineError> {
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut live = HashSet::new();
    let mut env = AdapterEnv::new(backend, table, &tx, &mut live);
    env.risk_profile = risk;
    env.policy = Some(policy);
    make_engine_for_account(&mut env, venue, "BTCUSDT", &AccountLabel::Default, &[])
}

/// The NOT RUNNING answer of a start, or a panic naming what came back instead.
fn stopped_of(result: Result<crate::EngineAndReconcile, EngineError>) -> crate::ArmingNotRunning {
    match result {
        Err(EngineError::NotRunning(stopped)) => stopped,
        Err(other) => panic!("expected NOT RUNNING, got the refusal: {other}"),
        Ok((engine, _)) => panic!("expected NOT RUNNING, got an engine at {}", engine.tier),
    }
}

fn demo_policy() -> AdapterPolicy {
    AdapterPolicy::default().with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
}

/// **R38: the identity record comes AFTER the fee schedule, never before.** On binance spot the
/// account id and the commission rates ride ONE body and the client keeps the id from whichever
/// call fetches first: asking first costs two signed reads instead of zero extra. The scripted
/// reconcile client logs its two reads in the order they were made.
#[test]
fn the_identity_is_recorded_after_the_fee_schedule() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let backend = VenueBackendScript::new(table.clone()).script_start(
        "bybit",
        Ok(bind(AccountTier::Demo, Some(Box::new(OrderedReconcile(Arc::clone(&log)))))),
    );
    let (engine, recon) =
        start(&backend, &table, &demo_policy(), Some(&budget())).expect("a scripted bind starts");
    assert_eq!(engine.tier, AccountTier::Demo, "the engine runs at the tier the client bound");
    assert!(recon.is_some(), "the reconcile client is handed to the caller");
    assert_eq!(
        *log.lock().expect("log"),
        vec!["fee_rates", "account_identity"],
        "the fee-rate read comes first, the identity read after it"
    );
    let calls = backend.calls();
    assert!(matches!(
        calls[0],
        VenueBackendScriptCall::Start { tier: AccountTier::Demo, withhold: None, .. }
    ));
    assert_eq!(
        calls[1],
        VenueBackendScriptCall::RecordIdentity {
            venue: "bybit".to_string(),
            account: AccountLabel::Default,
            bound_tier: AccountTier::Demo,
            reconcile_present: true,
        }
    );
    assert_eq!(calls.len(), 2);
}

/// **The pre-connect budget refusal comes BEFORE the backend is asked to start**: an armed row
/// with no live risk budget is refused as a missing budget and the venue is never touched — the
/// position the refusal had in the old body, between the arming probe and the bridge's start.
#[test]
fn the_budget_refusal_is_before_the_start() {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let backend = VenueBackendScript::new(table.clone());
    let refused = start(&backend, &table, &demo_policy(), None);
    assert!(
        matches!(refused, Err(EngineError::MissingRiskBudget { ref venue, .. }) if venue == "bybit"),
        "expected the missing-budget refusal",
    );
    assert!(backend.calls().is_empty(), "the backend was never asked: {:?}", backend.calls());
}

/// A row the table does NOT arm is still sent to the backend — it is the backend that speaks for
/// the refusal in the venue's own words — but the budget is not demanded of it (an account that
/// does not run needs no budget), and the word it answers is the account's NOT RUNNING reason.
#[test]
fn a_row_that_is_not_armed_reaches_the_backend_without_a_budget() {
    let table = bybit_table(Some(AccountTier::Demo), None, ArmingBlock::NoCredentials);
    let backend = VenueBackendScript::new(table.clone())
        .script_start("bybit", Err(VenueBackendError::NotRunning(ArmingBlock::NoCredentials)));
    let stopped = stopped_of(start(&backend, &table, &demo_policy(), None));
    assert_eq!(
        (stopped.venue.as_str(), stopped.tier, stopped.block),
        ("bybit", Some(AccountTier::Demo), ArmingBlock::NoCredentials)
    );
    assert_eq!(backend.calls().len(), 1, "one start, and nothing else");
}

/// The tradehub's own tier must equal the table row's: a mismatch is the account NOT RUNNING with
/// the table's word, never silently armed, and the backend is not asked.
#[test]
fn a_table_that_disagrees_with_the_account_rows_does_not_run() {
    // The policy says `demo`; the (stale) table says the account is `live` and armed.
    let table = bybit_table(Some(AccountTier::Live), Some(AccountTier::Live), ArmingBlock::None);
    let backend = VenueBackendScript::new(table.clone());
    let stopped = stopped_of(start(&backend, &table, &demo_policy(), Some(&budget())));
    assert_eq!(stopped.block, ArmingBlock::None, "the table's own word is reported");
    assert!(backend.calls().is_empty(), "{:?}", backend.calls());
}

/// A venue the table carries no adapter for runs no `demo`/`live` account, and the backend is not
/// asked: `FeatureAbsent` for a venue it names without an adapter, `NoLiveArm` for an id it never
/// heard of.
#[test]
fn a_venue_without_an_adapter_never_reaches_the_backend() {
    let mut absent = bybit_table(Some(AccountTier::Demo), None, ArmingBlock::FeatureAbsent);
    absent = ArmingTable::new(
        BTreeMap::from([("bybit".to_string(), VenueAdapterDeclarationView::absent())]),
        absent.rows().to_vec(),
    );
    let backend = VenueBackendScript::new(absent.clone());
    let stopped = stopped_of(start(&backend, &absent, &demo_policy(), Some(&budget())));
    assert_eq!(stopped.block, ArmingBlock::FeatureAbsent);
    let unknown = ArmingTable::default();
    let stopped = stopped_of(start(&backend, &unknown, &demo_policy(), Some(&budget())));
    assert_eq!(stopped.block, ArmingBlock::NoLiveArm);
    assert!(backend.calls().is_empty(), "{:?}", backend.calls());
}

/// The preflight's stop reaches the START as the withhold: the fold overlays the stop on the row
/// (so the account is not armed and needs no budget) and tells the backend to take the venue's
/// credentials away, so the bridge reads none.
#[test]
fn the_preflight_stop_reaches_the_start_as_a_withhold() {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let backend = VenueBackendScript::new(table.clone())
        .script_start("bybit", Err(VenueBackendError::NotRunning(ArmingBlock::PreflightFailed)));
    let mut policy = demo_policy();
    policy.preflight_failed.insert("bybit".to_string());
    let stopped = stopped_of(start(&backend, &table, &policy, None));
    assert_eq!(stopped.block, ArmingBlock::PreflightFailed);
    assert!(
        matches!(
            backend.calls()[0],
            VenueBackendScriptCall::Start { withhold: Some(Withhold::PreflightFailed), .. }
        ),
        "{:?}",
        backend.calls()
    );
}

/// A backend that cannot answer stops the account — with its own word (`GatewayUnavailable`, the
/// gatewayhub did not answer), and never on a paper stand-in.
#[test]
fn an_unavailable_backend_stops_the_account() {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let backend = VenueBackendScript::new(table.clone());
    let stopped = stopped_of(start(&backend, &table, &demo_policy(), Some(&budget())));
    assert_eq!(stopped.block, ArmingBlock::GatewayUnavailable)
}

/// The scripted backend's table is what the assembly reads, whole: a `RuntimeConfig` over it
/// refuses a strategy mount on an account its table does not run — with no venue anywhere.
#[test]
fn the_assembly_reads_the_backends_table() {
    let table = bybit_table(Some(AccountTier::Demo), None, ArmingBlock::NoCredentials);
    let backend: Arc<dyn VenueBackend> = Arc::new(VenueBackendScript::new(table));
    let config = crate::RuntimeConfig {
        backend,
        vars: Default::default(),
        properties_rec: None,
        seed_cash: 10_000.0,
        reconcile_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: demo_policy(),
    };
    match crate::assemble_runtime_with_preflight(
        config,
        &crate::preflight::PreflightReport::default(),
    ) {
        Err(crate::RuntimeError::NoAccountRuns { not_running }) => {
            assert!(
                not_running
                    .iter()
                    .any(|n| n.venue == "bybit" && n.block == ArmingBlock::NoCredentials),
                "{not_running:?}"
            );
        }
        Err(other) => panic!("expected the no-account refusal, got {other}"),
        Ok(node) => {
            node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
            node.handle.shutdown_and_join();
            panic!("a table that runs nothing built a node");
        }
    }
}

/// A reconcile client whose venue reports the account's own fee rate, `Ok(rate)`.
struct ReportedRate(Option<vike_model::FeeSchedule>);

impl vike_execution::recon::ReconcileClient for ReportedRate {
    empty_report_fetches!();
    fn fetch_fee_rates(&self) -> Result<Option<vike_model::FeeSchedule>, String> {
        Ok(self.0)
    }
}

/// Assemble a node whose one bybit `demo` account binds with a venue reporting `reported`, carrying
/// an A-S maker mount on it through the armed assembly, and answer the params bag the maker was
/// built from and the one it was REBUILT from (`None`: it kept the strategy it was built with). The
/// node is shut down before it answers.
fn maker_rebuilt_under(
    reported: Option<vike_model::FeeSchedule>,
) -> (vike_model::params::Params, Option<vike_model::params::Params>) {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let backend: Arc<dyn VenueBackend> = Arc::new(VenueBackendScript::new(table).script_start(
        "bybit",
        Ok(bind(AccountTier::Demo, Some(Box::new(ReportedRate(reported))))),
    ));
    // The daemon's crypto maker on bybit's grid, its floor armed from the static lane at mount.
    let mut maker = vike_model::params::Params::new();
    maker.insert("preset", vike_market_making::SpreadMakerPreset::Crypto.word());
    maker.insert("tick_size", 0.1);
    maker.insert("qty", 0.001);
    let lane = crate::maker_round_trip_fee_for("bybit", "BTCUSDT").expect("bybit's lane is flat");
    maker.insert("round_trip_fee_rate", lane);
    let build = |p: &vike_model::params::Params| {
        SpreadMaker::from_params(p).expect("the crypto preset reads")
    };
    let core_config = vike_core::CoreConfig {
        strategy: Some(vike_core::Mount {
            order_client_id_code: None,
            account: None,
            venue: "bybit".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            strategy: Box::new(build(&maker)),
            symbols: Vec::new(),
            underlying_symbol: None,
            controller_id: None,
        }),
        ..vike_core::CoreConfig::default()
    };
    let config = crate::RuntimeConfig {
        backend,
        vars: Default::default(),
        properties_rec: None,
        seed_cash: 10_000.0,
        reconcile_enabled: false,
        core_config,
        risk_profile: Some(budget()),
        policy: demo_policy(),
    };
    let rebuilt = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&rebuilt);
    let held = maker.clone();
    let rearm: crate::FeeRearm = Box::new(
        move |schedule: Option<vike_model::FeeSchedule>| -> Option<
            Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
        > {
            let armed = vike_market_making::rearm_fee_floor(&held, schedule, "bybit", "BTCUSDT")?;
            *seen.lock().expect("the record") = Some(armed.clone());
            Some(Box::new(build(&armed)))
        },
    );
    let node = crate::assemble_runtime_armed_with_preflight(
        config,
        &crate::preflight::PreflightReport::default(),
        crate::fee_rearm_mounts(vec![Some(rearm)]),
    )
    .unwrap_or_else(|e| panic!("the scripted demo account runs: {e}"));
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
    let rebuilt = rebuilt.lock().expect("the record").clone();
    (maker, rebuilt)
}

/// The round-trip fee rate a maker params bag holds.
fn floor_of(p: &vike_model::params::Params) -> Option<u64> {
    p.get("round_trip_fee_rate").and_then(vike_model::params::ParamValue::as_f64).map(f64::to_bits)
}

/// **The maker's break-even floor is its ACCOUNT's own resolved fee rate** (one fee rate per account).
/// bybit's static row is 2 / 5.5 bps, so the mount arms a 4 bps round trip (2 + 2); the account's
/// venue reports 3.6 / 10 at the start, and the mount the core is spawned with is rebuilt at 7.2
/// (3.6 + 3.6), after the engine read the rate and before the core is up.
#[test]
fn a_reported_fee_rate_arms_the_maker_floor_at_the_accounts_own_rate() {
    let reported = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 3.6, taker_bps: 10.0 };
    let (maker, rebuilt) = maker_rebuilt_under(Some(reported));
    assert_eq!(
        floor_of(&maker),
        Some((2.0_f64 / 1e4 + 2.0_f64 / 1e4).to_bits()),
        "the static lane's floor the mount was built at"
    );
    let rebuilt = rebuilt.expect("the maker is rebuilt at the account's reported rate");
    assert_eq!(
        floor_of(&rebuilt),
        Some((3.6_f64 / 10_000.0 + 3.6_f64 / 10_000.0).to_bits()),
        "the reported 3.6 bps maker leg, twice, not the static 2"
    );
}

/// A venue that reports no rate leaves the account on the static lane: the maker is NOT rebuilt, so
/// the core runs the very strategy it was given (and a paper account, which reports nothing, too).
#[test]
fn a_venue_that_reports_no_rate_keeps_the_mount_it_was_built_with() {
    let (_, rebuilt) = maker_rebuilt_under(None);
    assert!(rebuilt.is_none(), "rebuilt at {rebuilt:?}");
}

/// **The mount turns the engine's symbol fail-closed from the POLICY's venue list** (decision 0174
/// item 7), through the real `assemble_engine`: listed, the engine denies a symbol it was never told
/// about; not listed (and by default), it is exactly the engine it always was. The list is the
/// root's to fill (`AdapterPolicy::fail_closed_symbol_venues`); this crate names no venue.
#[test]
fn the_engine_fails_closed_on_symbols_for_exactly_the_policys_listed_venues() {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let engine_under = |policy: AdapterPolicy| {
        let backend = VenueBackendScript::new(table.clone())
            .script_start("bybit", Ok(bind(AccountTier::Demo, None)));
        start(&backend, &table, &policy, Some(&budget())).expect("a scripted bind starts").0
    };
    let listed =
        AdapterPolicy { fail_closed_symbol_venues: vec!["bybit".to_string()], ..demo_policy() };
    assert!(engine_under(listed).gate.limits.deny_unadmitted_symbols, "listed: fails closed");
    assert!(
        !engine_under(demo_policy()).gate.limits.deny_unadmitted_symbols,
        "the default policy lists nobody: every engine stays as it was"
    );
    let other =
        AdapterPolicy { fail_closed_symbol_venues: vec!["binance".to_string()], ..demo_policy() };
    assert!(!engine_under(other).gate.limits.deny_unadmitted_symbols, "another venue's entry");
}

/// **The mark floor is not armed on a venue's DEFAULT engine, whatever the venue and the tier**, through the
/// real `assemble_engine`. The floor exists for the lane twin, whose mounted symbol the venue's quote route
/// never prices; binance's spot engine IS priced by that route, and a floor on it would hold an opening limit
/// order on a mainnet account through every price-lane outage. The armed engine is the derived twin
/// (`leverage_tests.rs`'s `the_mark_floor_is_armed_for_the_derived_lane_twin_and_no_other_engine`).
#[test]
fn the_mark_floor_is_not_armed_on_a_default_engine_of_any_venue_or_tier() {
    let engine_of = |venue: &'static str, tier: AccountTier| {
        let table = venue_table(venue, Some(tier), Some(tier), ArmingBlock::None);
        let backend =
            VenueBackendScript::new(table.clone()).script_start(venue, Ok(bind(tier, None)));
        let policy = AdapterPolicy::default().with_account(venue, &AccountLabel::Default, tier);
        start_on(venue, &backend, &table, &policy, Some(&budget()))
            .expect("a scripted bind starts")
            .0
    };
    for (venue, tier) in [
        ("binance", AccountTier::Demo),
        ("binance", AccountTier::Live),
        ("bybit", AccountTier::Demo),
    ] {
        assert!(!engine_of(venue, tier).gate.require_mark, "{venue} {tier:?}");
    }
}

/// What [`HearsTheLeg`] read on each closed bar: the bar's symbol and the leg's price.
type HeardBar = (Option<String>, f64);

/// Trades `ETHUSDT` beside its own `BTCUSDT` and records every closed bar it hears with the price its
/// broker reads for the leg.
struct HearsTheLeg(Arc<Mutex<Vec<HeardBar>>>);

impl vike_model::Strategy<vike_core::LiveBroker> for HearsTheLeg {
    fn symbols(&self) -> Vec<String> {
        vec!["ETHUSDT".to_string()]
    }

    fn on_bar(&mut self, broker: &mut vike_core::LiveBroker, bar: &vike_marketdata::Bar) {
        let leg = vike_model::OrderBroker::price(broker, "ETHUSDT");
        self.0.lock().expect("the record").push((bar.symbol.clone(), leg));
    }
}

/// **A LIVE mount's declared leg is admitted into its account's engine** (one broker plan, step 12):
/// through the real assembly over a scripted `demo` bybit account, the leg's closed bar reaches the
/// strategy and prices the leg, so a later bar of the mount's own symbol reads the leg's close and not
/// its own. Before, the engine traded its wired symbol alone (the leg got its grid and nothing else),
/// so `drive_strategy` dropped every bar of the leg and the leg's read fell back to the own price.
#[test]
fn a_live_mounts_declared_leg_is_admitted_heard_and_priced() {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let backend: Arc<dyn VenueBackend> = Arc::new(
        VenueBackendScript::new(table).script_start("bybit", Ok(bind(AccountTier::Demo, None))),
    );
    let heard = Arc::new(Mutex::new(Vec::new()));
    let core_config = vike_core::CoreConfig {
        strategy: Some(vike_core::Mount {
            order_client_id_code: None,
            account: None,
            venue: "bybit".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1m".to_string(),
            strategy: Box::new(HearsTheLeg(Arc::clone(&heard))),
            symbols: vec![vike_core::MountLeg::same_venue("ETHUSDT")],
            underlying_symbol: None,
            controller_id: None,
        }),
        ..vike_core::CoreConfig::default()
    };
    let config = crate::RuntimeConfig {
        backend,
        vars: Default::default(),
        properties_rec: None,
        seed_cash: 10_000.0,
        reconcile_enabled: false,
        core_config,
        risk_profile: Some(budget()),
        policy: demo_policy(),
    };
    let node = crate::assemble_runtime_with_preflight(
        config,
        &crate::preflight::PreflightReport::default(),
    )
    .unwrap_or_else(|e| panic!("the scripted demo account runs: {e}"));
    let bars = node.handle.bar_sender();
    let close = |symbol: &str, close: f64| {
        bars.close(vike_execution::BarUpdate {
            venue: "bybit".into(),
            symbol: symbol.into(),
            interval: "1m".into(),
            bar: vike_marketdata::Bar::ohlcv(60_000, close, close, close, close, 1.0),
        })
        .expect("the core is up");
    };
    close("ETHUSDT", 50.0);
    close("BTCUSDT", 100.0);
    let both = vike_log::poll::wait_until(std::time::Duration::from_secs(10), || {
        heard.lock().expect("the record").len() >= 2
    });
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
    let heard = heard.lock().expect("the record").clone();
    assert!(both, "the strategy heard {heard:?}, not both bars");
    assert_eq!(
        heard,
        [(Some("ETHUSDT".to_string()), 50.0), (Some("BTCUSDT".to_string()), 50.0)],
        "the leg's bar reaches the strategy, and the leg reads its own close on the mount's bar"
    );
}

/// A `paper` account's engine admits a declared leg the same way, with a paper book of its own (one
/// single-symbol book would fill the leg on the engine symbol's bars), and an engine with no leg is the
/// engine it always was.
#[test]
fn a_paper_accounts_engine_admits_a_declared_leg() {
    let legs = ["ETHUSDT".to_string(), "BTCUSDT".to_string(), " ".to_string()];
    let (engine, _) = crate::paper_fallback::paper_engine(
        "bybit",
        "BTCUSDT",
        &AccountLabel::Default,
        &legs,
        None,
        None,
    );
    assert_eq!(engine.extra_symbols, ["ETHUSDT"], "the leg, once; never the own symbol or a blank");
    let (alone, _) = crate::paper_fallback::paper_engine(
        "bybit",
        "BTCUSDT",
        &AccountLabel::Default,
        &[],
        None,
        None,
    );
    assert!(alone.extra_symbols.is_empty(), "no leg: nothing admitted");
}
