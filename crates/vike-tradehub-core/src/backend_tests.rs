//! The venue port, judged apart from any venue: `PaperBackend`'s rows, and the fold's side of the
//! seam driven through a `ScriptedBackend` — WHEN the fold asks the backend (the budget refusal
//! before the start, the identity record after the fee schedule, a stop word that reaches the
//! start) and what it does with each answer.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::*;
use crate::symbol_grid::GridSource;
use crate::testutil::{NoopClient, empty_report_fetches};
use crate::{
    AdapterEnv, AdapterPolicy, EngineError, make_engine_for_account, stopped_by_the_live_switch,
};

// ---- PaperBackend ----------------------------------------------------------------------------

fn paper_rows(policy: &AdapterPolicy) -> Vec<VenueArming> {
    let table = PaperBackend::new(&policy.accounts).arming_table().expect("it always answers");
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
    let backend = PaperBackend::new(&AdapterPolicy::default().accounts);
    let table = backend.arming_table().expect("it always answers");
    assert_eq!(table.wired_markets(), crate::wired_markets(), "every engine row is wired");
    let (tx, _rx) = vike_exec::event_channel(4);
    let account = AccountLabel::Default;
    let query = AdapterStartQuery {
        venue: "bybit",
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
        route_key: "bybit",
        properties_rec: None,
    };
    assert_eq!(
        backend.start_adapter(query, &tx, None).unwrap_err(),
        BackendError::NotRunning(ArmingBlock::LiveSwitchOff)
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

// ---- the fold's side of the seam, over a ScriptedBackend -------------------------------------

/// One default-account row of `bybit` in a table that says the venue's adapter is present.
fn bybit_table(
    tier: Option<AccountTier>,
    effective: Option<AccountTier>,
    block: ArmingBlock,
) -> ArmingTable {
    let facts = BTreeMap::from([(
        "bybit".to_string(),
        VenueFacts {
            adapter_present: true,
            addresses_accounts: true,
            takes_recon_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_decl_word: "not-wired",
        },
    )]);
    ArmingTable::new(
        facts,
        vec![ArmingRow {
            arming: VenueArming {
                venue: "bybit",
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

fn budget() -> vike_model::ProfileRisk {
    vike_model::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

/// A reconcile client that logs the order its two optional reads are made in.
struct OrderedRecon(Arc<Mutex<Vec<&'static str>>>);

impl vike_exec::recon::ReconClient for OrderedRecon {
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
    recon: Option<Box<dyn vike_exec::recon::ReconClient>>,
) -> AdapterBind {
    AdapterBind {
        client: Box::new(NoopClient),
        recon,
        bound_tier,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        confirmed_book: None,
        leverage_cap: None,
        account_ccy: None,
    }
}

/// One start of bybit's default account through the fold over `backend` and `table`.
fn start(
    backend: &ScriptedBackend,
    table: &ArmingTable,
    policy: &AdapterPolicy,
    risk: Option<&vike_model::ProfileRisk>,
) -> Result<crate::EngineAndRecon, EngineError> {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mut env = AdapterEnv::new(backend, table, &tx, &mut live);
    env.risk_profile = risk;
    env.policy = Some(policy);
    make_engine_for_account(&mut env, "bybit", "BTCUSDT", &AccountLabel::Default, &[])
}

/// The NOT RUNNING answer of a start, or a panic naming what came back instead.
fn stopped_of(result: Result<crate::EngineAndRecon, EngineError>) -> crate::NotRunning {
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
    let backend = ScriptedBackend::new(table.clone()).script_start(
        "bybit",
        Ok(bind(AccountTier::Demo, Some(Box::new(OrderedRecon(Arc::clone(&log)))))),
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
        ScriptedCall::Start { tier: AccountTier::Demo, withhold: None, .. }
    ));
    assert_eq!(
        calls[1],
        ScriptedCall::RecordIdentity {
            venue: "bybit".to_string(),
            account: AccountLabel::Default,
            bound_tier: AccountTier::Demo,
            confirmed: None,
            recon_present: true,
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
    let backend = ScriptedBackend::new(table.clone());
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
    let backend = ScriptedBackend::new(table.clone())
        .script_start("bybit", Err(BackendError::NotRunning(ArmingBlock::NoCredentials)));
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
    let backend = ScriptedBackend::new(table.clone());
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
        BTreeMap::from([("bybit".to_string(), VenueFacts::absent())]),
        absent.rows().to_vec(),
    );
    let backend = ScriptedBackend::new(absent.clone());
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
    let backend = ScriptedBackend::new(table.clone())
        .script_start("bybit", Err(BackendError::NotRunning(ArmingBlock::PreflightFailed)));
    let mut policy = demo_policy();
    policy.preflight_failed.insert("bybit".to_string());
    let stopped = stopped_of(start(&backend, &table, &policy, None));
    assert_eq!(stopped.block, ArmingBlock::PreflightFailed);
    assert!(
        matches!(
            backend.calls()[0],
            ScriptedCall::Start { withhold: Some(Withhold::PreflightFailed), .. }
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
    let backend = ScriptedBackend::new(table.clone());
    let stopped = stopped_of(start(&backend, &table, &demo_policy(), Some(&budget())));
    assert_eq!(stopped.block, ArmingBlock::GatewayUnavailable)
}

/// The scripted backend's table is what the assembly reads, whole: a `RuntimeConfig` over it
/// refuses a strategy mount on an account its table does not run — with no venue anywhere.
#[test]
fn the_assembly_reads_the_backends_table() {
    let table = bybit_table(Some(AccountTier::Demo), None, ArmingBlock::NoCredentials);
    let backend: Arc<dyn VenueBackend> = Arc::new(ScriptedBackend::new(table));
    let cfg = crate::RuntimeConfig {
        backend,
        vars: Default::default(),
        properties_rec: None,
        seed_cash: 10_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: demo_policy(),
    };
    match crate::assemble_runtime_with_preflight(cfg, &crate::preflight::PreflightReport::default())
    {
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

/// **The mount turns the engine's symbol fail-closed from the POLICY's venue list** (decision 0174
/// item 7), through the real `assemble_engine`: listed, the engine denies a symbol it was never told
/// about; not listed (and by default), it is exactly the engine it always was. The list is the
/// root's to fill (`AdapterPolicy::fail_closed_symbol_venues`); this crate names no venue.
#[test]
fn the_engine_fails_closed_on_symbols_for_exactly_the_policys_listed_venues() {
    let table = bybit_table(Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None);
    let engine_under = |policy: AdapterPolicy| {
        let backend = ScriptedBackend::new(table.clone())
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
