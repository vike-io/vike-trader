//! `HotAccounts`: one more account in a RUNNING node (decision 0169, slices S1 to S3), over a
//! `ScriptedBackend`: the engine joins the core, a refusal is that one account's, the guards reach no
//! venue, the reconcile leg arrives in the order the core needs, a `live` account crosses the gates the
//! boot crosses, and a venue the node mounted nothing for gets its first engine appended.

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::{HotStart, HotStop};
use crate::preflight::{CredentialGap, ServerTimeGap};
use crate::symbol_grid::GridSource;
use crate::testutil::{NoopClient, empty_report_fetches};
use crate::{
    AdapterBind, AdapterPolicy, ArmingRow, ArmingTable, BackendError, LegChange, NetReading,
    PreflightPlan, PreflightQuery, ScriptedBackend, ScriptedCall, VenueBackend, VenueFacts,
};

struct EmptyRecon;
impl vike_exec::recon::ReconClient for EmptyRecon {
    empty_report_fetches!();
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn budget() -> vike_model::ProfileRisk {
    vike_model::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

fn row_on(
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

fn row(
    label: AccountLabel,
    tier: AccountTier,
    effective: Option<AccountTier>,
    block: ArmingBlock,
) -> ArmingRow {
    row_on("bybit", label, tier, effective, block)
}

fn facts(adapter_present: bool) -> VenueFacts {
    VenueFacts {
        adapter_present,
        addresses_accounts: true,
        takes_recon_trigger: false,
        grid_source: GridSource::NoGrid,
        process_exclusive: None,
        clock_decl_word: "not-wired",
    }
}

/// A table of `venues` (adapter present or not, as stated) holding `rows`.
fn table_of(venues: &[&str], adapter_present: bool, rows: Vec<ArmingRow>) -> ArmingTable {
    let facts: BTreeMap<String, VenueFacts> =
        venues.iter().map(|v| ((*v).to_string(), facts(adapter_present))).collect();
    ArmingTable::new(facts, rows)
}

/// A table of ONE venue (`bybit`, adapter present) holding `rows`.
fn table(rows: Vec<ArmingRow>) -> ArmingTable {
    table_of(&["bybit"], true, rows)
}

fn armed_at(label: AccountLabel, tier: AccountTier) -> ArmingRow {
    row(label, tier, Some(tier), ArmingBlock::None)
}

fn armed(label: AccountLabel) -> ArmingRow {
    armed_at(label, AccountTier::Demo)
}

fn armed_on(venue: &'static str, label: AccountLabel) -> ArmingRow {
    row_on(venue, label, AccountTier::Demo, Some(AccountTier::Demo), ArmingBlock::None)
}

fn bind_at(recon: bool, tier: AccountTier) -> AdapterBind {
    AdapterBind {
        client: Box::new(NoopClient),
        recon: recon.then(|| Box::new(EmptyRecon) as Box<dyn vike_exec::recon::ReconClient>),
        bound_tier: tier,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        confirmed_book: None,
        leverage_cap: None,
        account_ccy: None,
        account_ccy_assumed: false,
        mount_symbol: None,
    }
}

fn bind(recon: bool) -> AdapterBind {
    bind_at(recon, AccountTier::Demo)
}

fn demo_policy() -> AdapterPolicy {
    AdapterPolicy::default().with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
}

/// `Arc<dyn VenueBackend>` of a scripted backend that the test keeps a handle to (its `calls()`).
fn dynb<B: VenueBackend + 'static>(backend: &Arc<B>) -> Arc<dyn VenueBackend> {
    Arc::clone(backend) as Arc<dyn VenueBackend>
}

/// A node running `venue`'s default demo account, over a backend that arms exactly that.
fn node_on(venue: &'static str, recon_enabled: bool) -> crate::AssembledRuntime {
    node_with(venue, recon_enabled, 10_000.0, budget())
}

/// [`node_on`] with the equity seed every extra engine gets and the operator's `[risk]` budget stated.
fn node_with(
    venue: &'static str,
    recon_enabled: bool,
    seed_cash: f64,
    risk: vike_model::ProfileRisk,
) -> crate::AssembledRuntime {
    let policy =
        AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Demo);
    let backend: Arc<dyn VenueBackend> = Arc::new(
        ScriptedBackend::new(table_of(
            &[venue],
            true,
            vec![armed_on(venue, AccountLabel::Default)],
        ))
        .script_start(venue, Ok(bind(recon_enabled))),
    );
    let cfg = crate::RuntimeConfig {
        backend,
        vars: Default::default(),
        properties_rec: None,
        seed_cash,
        recon_enabled,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: Some(risk),
        policy,
    };
    crate::assemble_runtime_with_preflight(cfg, &crate::preflight::PreflightReport::default())
        .expect("a scripted node assembles")
}

/// A node running `bybit`'s default demo account, over a backend that arms exactly that.
fn running_node(recon_enabled: bool) -> crate::AssembledRuntime {
    node_on("bybit", recon_enabled)
}

/// The number of engine blocks the core PUBLISHES right now. A fresh core publishes nothing until
/// something marks it dirty, so this pokes it (a state it already has) and waits for the publish.
fn venues_now(node: &crate::AssembledRuntime) -> usize {
    let before = node.handle.snapshot().seq;
    node.handle.send_command(vike_exec::Command::SetTradingState(vike_model::TradingState::Active));
    let deadline = Instant::now() + Duration::from_secs(10);
    while node.handle.snapshot().seq == before && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    node.handle.snapshot().portfolio.venues.len()
}

fn wait_for_venues(node: &crate::AssembledRuntime, want: usize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if node.handle.snapshot().portfolio.venues.len() == want {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The route keys of the engine blocks the core publishes, in the core's order.
fn published_keys(node: &crate::AssembledRuntime) -> Vec<String> {
    node.handle.snapshot().portfolio.venues.iter().map(|v| v.route_key.clone()).collect()
}

/// The teardown a test must not forget: the forwarders drain-and-drop first, then the core stops.
fn stop(node: crate::AssembledRuntime) {
    node.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    node.handle.shutdown_and_join();
}

/// The headline: a `demo` account on a venue the node already runs joins the RUNNING core. Nothing
/// restarts, the account that was running is still there, and the core publishes a second block.
#[test]
fn a_demo_account_on_a_mounted_venue_joins_the_running_core() {
    let mut node = running_node(false);
    assert_eq!(venues_now(&node), 1);
    assert!(node.hot.runs("bybit") && !node.hot.runs("bybit#ALT"));

    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![armed(AccountLabel::Default), armed(alt())]))
            .script_start("bybit#ALT", Ok(bind(false))),
    );
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Demo).accounts;
    let outcome = node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &alt(), None);

    match outcome {
        HotStart::Started { route_key, tier } => {
            assert_eq!(route_key, "bybit#ALT");
            assert_eq!(tier, AccountTier::Demo);
        }
        other => panic!("expected the account to start, got {other:?}"),
    }
    assert!(node.hot.runs("bybit#ALT"), "the node knows the account runs now");
    assert!(wait_for_venues(&node, 2), "the core published the appended engine");
    // The account that was running was never asked for anything.
    assert!(
        hot_backend
            .calls()
            .iter()
            .all(|c| !matches!(c, ScriptedCall::Start { account, .. } if account.is_default())),
        "only the new account was started: {:?}",
        hot_backend.calls()
    );
    stop(node);
}

/// Asking again for an account that runs is a no-op that reaches nothing.
#[test]
fn an_account_that_runs_is_not_started_twice() {
    let mut node = running_node(false);
    let backend = Arc::new(ScriptedBackend::new(table(vec![armed(AccountLabel::Default)])));
    let outcome = node.hot.start_account(
        &dynb(&backend),
        demo_policy().accounts,
        "bybit",
        &AccountLabel::Default,
        None,
    );
    assert!(matches!(outcome, HotStart::AlreadyRunning), "{outcome:?}");
    assert!(backend.calls().is_empty(), "nothing was asked of the venue");
    stop(node);
}

/// A refusal is THAT account NOT RUNNING, with the backend's word, and the core is untouched.
#[test]
fn a_refused_start_is_that_one_account_not_running() {
    let mut node = running_node(false);
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![armed(AccountLabel::Default), armed(alt())]))
            .script_start("bybit#ALT", Err(BackendError::NotRunning(ArmingBlock::AdapterFailed))),
    );
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Demo).accounts;
    match node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &alt(), None) {
        HotStart::NotRunning(nr) => {
            assert_eq!(nr.route_key(), "bybit#ALT");
            assert_eq!(nr.block, ArmingBlock::AdapterFailed);
        }
        other => panic!("expected NOT RUNNING, got {other:?}"),
    }
    assert!(!node.hot.runs("bybit#ALT"));
    assert_eq!(venues_now(&node), 1, "no engine joined the core");
    stop(node);
}

/// An account the FRESH table does not run is NOT RUNNING with the table's word, and the venue is
/// never asked to start it (no lease would be opened for an account the gatewayhub does not arm).
#[test]
fn a_row_the_fresh_table_does_not_run_is_never_dialled() {
    let mut node = running_node(false);
    let unarmed = row(alt(), AccountTier::Demo, None, ArmingBlock::NoCredentials);
    let hot_backend =
        Arc::new(ScriptedBackend::new(table(vec![armed(AccountLabel::Default), unarmed])));
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Demo).accounts;
    match node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &alt(), None) {
        HotStart::NotRunning(nr) => assert_eq!(nr.block, ArmingBlock::NoCredentials),
        other => panic!("expected NOT RUNNING, got {other:?}"),
    }
    assert!(hot_backend.calls().is_empty(), "no adapter start was asked for");
    stop(node);
}

/// Slice S4: a `paper` account joins the running core as the simulator engine the boot would have built, and
/// it reaches no venue: the backend is not asked for a table, a start or a lease, so it starts with the
/// gatewayhub away too.
#[test]
fn a_paper_account_joins_the_running_core_and_reaches_no_venue() {
    let mut node = running_node(false);
    let backend = Arc::new(ScriptedBackend::new(table(vec![armed(AccountLabel::Default)])));
    let paper_rows = demo_policy().with_account("bybit", &alt(), AccountTier::Paper).accounts;
    let outcome = node.hot.start_account(&dynb(&backend), paper_rows, "bybit", &alt(), None);
    match outcome {
        HotStart::Started { route_key, tier } => {
            assert_eq!(route_key, "bybit#ALT");
            assert_eq!(tier, AccountTier::Paper);
        }
        other => panic!("expected the paper account to start, got {other:?}"),
    }
    assert!(backend.calls().is_empty(), "it touched no venue: {:?}", backend.calls());
    assert!(node.hot.runs("bybit#ALT"));
    assert!(wait_for_venues(&node, 2), "the core published the appended engine");
    stop(node);
}

/// A backend whose gatewayhub is away: it can answer nothing.
struct GatewayDown;

impl VenueBackend for GatewayDown {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        Err(BackendError::Unavailable("the gatewayhub is away".to_string()))
    }
    fn preflight_plan(&self, _q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        unreachable!("a paper start asks no preflight")
    }
    fn server_time_ms(&self, _venue: &str, _tier: AccountTier) -> Result<i64, ServerTimeGap> {
        unreachable!("a paper start reads no clock")
    }
    fn authed_read(&self, _venue: &str) -> Result<(), CredentialGap> {
        unreachable!("a paper start reads no credential")
    }
    fn net_reading(&self) -> NetReading {
        NetReading::Unknown
    }
    fn report_not_running(&self, _venue: &str, _rows: &[VenueArming]) {}
    fn start_adapter(
        &self,
        _q: crate::AdapterStartQuery<'_>,
        _events: &vike_exec::EventSender,
        _trigger: Option<std::sync::mpsc::Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        unreachable!("a paper start opens no lease")
    }
    fn record_identity(
        &self,
        _q: &crate::IdentityRecord<'_>,
        _recon: Option<&dyn vike_exec::recon::ReconClient>,
    ) -> Result<(), BackendError> {
        unreachable!("a paper start records no identity")
    }
}

/// The paper start needs nothing from the gatewayhub, while a `demo` start of the same node says the
/// gatewayhub is unavailable (and is retried by the watcher).
#[test]
fn a_paper_account_starts_while_the_gatewayhub_is_away_and_a_demo_one_does_not() {
    let mut node = running_node(false);
    let down: Arc<dyn VenueBackend> = Arc::new(GatewayDown);
    let rows = demo_policy()
        .with_account("bybit", &alt(), AccountTier::Paper)
        .with_account("bybit", &AccountLabel::parse("BOB").unwrap(), AccountTier::Demo)
        .accounts;
    let started = node.hot.start_account(&down, rows.clone(), "bybit", &alt(), None);
    assert!(matches!(started, HotStart::Started { tier: AccountTier::Paper, .. }), "{started:?}");
    let bob = AccountLabel::parse("BOB").unwrap();
    match node.hot.start_account(&down, rows, "bybit", &bob, None) {
        HotStart::NotRunning(nr) => assert_eq!(nr.block, ArmingBlock::GatewayUnavailable),
        other => panic!("a demo account needs the gatewayhub, got {other:?}"),
    }
    stop(node);
}

/// A paper account that is stopped and started again is a NEW engine, like any other tier.
#[test]
fn a_paper_account_stops_and_starts_again() {
    let mut node = running_node(false);
    let backend = Arc::new(ScriptedBackend::new(table(vec![armed(AccountLabel::Default)])));
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Paper).accounts;
    assert!(matches!(
        node.hot.start_account(&dynb(&backend), rows.clone(), "bybit", &alt(), None),
        HotStart::Started { .. }
    ));
    assert_eq!(node.hot.stop_account(stop_of(&alt()), None), HotStop::Stopped);
    assert!(!node.hot.runs("bybit#ALT"));
    assert!(matches!(
        node.hot.start_account(&dynb(&backend), rows, "bybit", &alt(), None),
        HotStart::Started { .. }
    ));
    assert!(node.hot.runs("bybit#ALT"));
    assert!(wait_for_venues(&node, 3), "the old engine stays, the new one is appended");
    stop(node);
}

/// The reconcile leg reaches the driver in the order the core needs: the venue's FIRST account is
/// named before the second engine joins (a leg with no route key is refused on a two-engine venue),
/// then the new account's own leg, marked as sharing its venue.
#[test]
fn the_reconcile_legs_arrive_named_first_and_added_second() {
    let mut node = running_node(true);
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![armed(AccountLabel::Default), armed(alt())]))
            .script_start("bybit#ALT", Ok(bind(true))),
    );
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Demo).accounts;
    let (legs_tx, legs_rx) = std::sync::mpsc::channel::<LegChange>();
    let outcome =
        node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &alt(), Some(&legs_tx));
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");

    match legs_rx.try_recv().expect("the first message") {
        LegChange::NameAccountsOf(venue) => assert_eq!(venue, "bybit"),
        other => panic!("the venue's existing leg is named first, got {other:?}"),
    }
    match legs_rx.try_recv().expect("the second message") {
        LegChange::Add { leg, venue_is_shared } => {
            assert!(venue_is_shared, "the venue has two engines now");
            assert_eq!(leg.venue, "bybit", "the canonical venue stays beside the route key");
            assert_eq!(leg.route_key.as_deref(), Some("bybit#ALT"));
        }
        other => panic!("then the new account's leg, got {other:?}"),
    }
    assert!(legs_rx.try_recv().is_err(), "and nothing else");
    stop(node);
}

fn stop_of(label: &AccountLabel) -> vike_exec::NotRunningAccount {
    vike_exec::NotRunningAccount {
        venue: "bybit".to_string(),
        account: (!label.is_default()).then(|| label.clone()),
        route_key: vike_model::accounts::account_keys::route_key_of("bybit", label),
        tier: None,
        reason: "account inactive".to_string(),
    }
}

/// Start `alt` on a running node over a scripted backend (the helper the stop tests share).
fn start_alt(
    node: &mut crate::AssembledRuntime,
    legs: Option<&std::sync::mpsc::Sender<LegChange>>,
) {
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![armed(AccountLabel::Default), armed(alt())]))
            .script_start("bybit#ALT", Ok(bind(legs.is_some()))),
    );
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Demo).accounts;
    let outcome = node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &alt(), legs);
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
}

/// The stop (slice S2): the account stops RUNNING for the node, its engine stays in the core, nothing
/// else is touched; stopping it again reaches nothing; and the same account can START again, which
/// appends a NEW engine (the core's list only grows).
#[test]
fn a_stopped_account_stops_running_keeps_its_engine_and_can_start_again() {
    let mut node = running_node(false);
    start_alt(&mut node, None);
    assert!(wait_for_venues(&node, 2));
    assert!(node.hot.runs("bybit#ALT"));

    assert_eq!(node.hot.detach_account("bybit#ALT"), HotStop::NotRunning, "not stopped yet");
    assert_eq!(node.hot.stop_account(stop_of(&alt()), None), HotStop::Stopped);
    assert_eq!(
        node.hot.detach_account("bybit#ALT"),
        HotStop::Stopped,
        "phase two follows phase one"
    );
    assert_eq!(
        node.hot.detach_account("bybit"),
        HotStop::NotRunning,
        "a running account is not detached"
    );
    assert!(!node.hot.runs("bybit#ALT"), "the node no longer counts it as running");
    assert!(node.hot.runs("bybit"), "the other account is untouched");
    assert_eq!(node.hot.running_route_keys(), vec!["bybit".to_string()]);
    assert_eq!(
        venues_now(&node),
        2,
        "the stopped engine stays published: an index is an attribution key"
    );

    assert_eq!(
        node.hot.stop_account(stop_of(&alt()), None),
        HotStop::NotRunning,
        "stopping what is stopped sends nothing"
    );

    start_alt(&mut node, None);
    assert!(node.hot.runs("bybit#ALT"));
    assert!(wait_for_venues(&node, 3), "the restart is a NEW engine");
    stop(node);
}

/// An account that never ran here is not stopped by naming it, and no leg message is sent for it.
#[test]
fn stopping_an_account_that_does_not_run_sends_nothing() {
    let mut node = running_node(true);
    let (legs_tx, legs_rx) = std::sync::mpsc::channel::<LegChange>();
    assert_eq!(node.hot.stop_account(stop_of(&alt()), Some(&legs_tx)), HotStop::NotRunning);
    assert!(legs_rx.try_recv().is_err());
    assert_eq!(venues_now(&node), 1);
    stop(node);
}

/// The reconcile leg is retired BEFORE the core is told to stop, and a restart counts the stopped
/// engine in its venue (the core still holds it, so the venue has two engines and the legs must name
/// themselves).
#[test]
fn the_stop_retires_the_leg_and_a_restart_names_the_legs_again() {
    let mut node = running_node(true);
    let (legs_tx, legs_rx) = std::sync::mpsc::channel::<LegChange>();
    start_alt(&mut node, Some(&legs_tx));
    while legs_rx.try_recv().is_ok() {}

    assert_eq!(node.hot.stop_account(stop_of(&alt()), Some(&legs_tx)), HotStop::Stopped);
    match legs_rx.try_recv().expect("the retire message") {
        LegChange::Retire(key) => assert_eq!(key, "bybit#ALT"),
        other => panic!("expected the leg to be retired, got {other:?}"),
    }
    assert!(legs_rx.try_recv().is_err());

    start_alt(&mut node, Some(&legs_tx));
    assert!(matches!(legs_rx.try_recv(), Ok(LegChange::NameAccountsOf(_))));
    match legs_rx.try_recv().expect("the new leg") {
        LegChange::Add { leg, venue_is_shared } => {
            assert!(venue_is_shared);
            assert_eq!(leg.route_key.as_deref(), Some("bybit#ALT"));
        }
        other => panic!("expected the new leg, got {other:?}"),
    }
    stop(node);
}

// ---- slice S3: the `live` tier --------------------------------------------------------------------

/// The rows of a node whose `bybit` default account is `demo` and whose `ALT` account is `live`.
fn live_alt_rows() -> vike_secrets::account_directory::AccountDirectory {
    demo_policy().with_account("bybit", &alt(), AccountTier::Live).accounts
}

/// **A `live` account joins the RUNNING core through the same construction as boot.** It is started at
/// the tier its row states, the venue is asked for that tier (the interlock refuses a client bound at
/// another), and only that account is asked for anything.
#[test]
fn a_live_account_joins_the_running_core_at_its_own_tier() {
    let mut node = running_node(true);
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed_at(alt(), AccountTier::Live),
        ]))
        .script_start("bybit#ALT", Ok(bind_at(false, AccountTier::Live))),
    );
    let outcome =
        node.hot.start_account(&dynb(&hot_backend), live_alt_rows(), "bybit", &alt(), None);
    match outcome {
        HotStart::Started { route_key, tier } => {
            assert_eq!(route_key, "bybit#ALT");
            assert_eq!(tier, AccountTier::Live);
        }
        other => panic!("expected the live account to start, got {other:?}"),
    }
    assert!(wait_for_venues(&node, 2));
    let starts: Vec<_> = hot_backend
        .calls()
        .into_iter()
        .filter_map(|c| match c {
            ScriptedCall::Start { account, tier, .. } => Some((account, tier)),
            _ => None,
        })
        .collect();
    assert_eq!(starts, vec![(alt(), AccountTier::Live)], "one start, at the row's tier");
    stop(node);
}

/// A venue-client account needs the operator's `[risk]` budget. At boot a missing one refuses the NODE
/// (`RuntimeError::RiskBudget`); in a running node it is THAT account NOT RUNNING, the venue is never
/// asked to start it, and the account that runs is untouched.
#[test]
fn a_live_start_with_no_risk_budget_is_that_one_account_not_running() {
    let mut node = running_node(true);
    node.hot.risk_profile = None;
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed_at(alt(), AccountTier::Live),
        ]))
        .script_start("bybit#ALT", Ok(bind_at(false, AccountTier::Live))),
    );
    match node.hot.start_account(&dynb(&hot_backend), live_alt_rows(), "bybit", &alt(), None) {
        HotStart::NotRunning(nr) => {
            assert_eq!(nr.route_key(), "bybit#ALT");
            assert_eq!(nr.tier, Some(AccountTier::Live));
            assert_eq!(nr.block, ArmingBlock::AdapterFailed);
        }
        other => panic!("expected NOT RUNNING, got {other:?}"),
    }
    assert!(
        hot_backend.calls().iter().all(|c| !matches!(c, ScriptedCall::Start { .. })),
        "the budget refusal sits before the start: {:?}",
        hot_backend.calls()
    );
    assert!(node.hot.runs("bybit") && !node.hot.runs("bybit#ALT"));
    assert_eq!(venues_now(&node), 1);
    stop(node);
}

/// **The armed set is recomputed from the fresh table and must name the account.** A table that says an
/// account runs on a venue whose adapter it also says is absent is inconsistent: the set the boot's lock
/// claim reads (`armed_live_venues`) does not name the account, so nothing is built and the venue is not
/// asked to start it.
#[test]
fn an_account_the_recomputed_armed_set_does_not_name_is_never_built() {
    let mut node = running_node(true);
    let inconsistent = table_of(
        &["bybit"],
        false,
        vec![armed(AccountLabel::Default), armed_at(alt(), AccountTier::Live)],
    );
    let hot_backend = Arc::new(
        ScriptedBackend::new(inconsistent)
            .script_start("bybit#ALT", Ok(bind_at(false, AccountTier::Live))),
    );
    match node.hot.start_account(&dynb(&hot_backend), live_alt_rows(), "bybit", &alt(), None) {
        HotStart::NotRunning(nr) => assert_eq!(nr.block, ArmingBlock::AdapterFailed),
        other => panic!("expected NOT RUNNING, got {other:?}"),
    }
    assert!(hot_backend.calls().iter().all(|c| !matches!(c, ScriptedCall::Start { .. })));
    assert!(!node.hot.runs("bybit#ALT"));
    stop(node);
}

/// **The armed set follows the rows.** Recomputed from the fresh table after the operator activated a
/// `live` account, it names that account beside the one that ran: the set a lock claim is compared with.
#[test]
fn the_armed_set_recomputed_from_the_fresh_rows_names_both_accounts() {
    let t = table(vec![armed(AccountLabel::Default), armed_at(alt(), AccountTier::Live)]);
    let policy = demo_policy().with_account("bybit", &alt(), AccountTier::Live);
    let mut armed_now = crate::armed_live_venues(&t, &policy);
    armed_now.sort();
    assert_eq!(armed_now, vec!["bybit".to_string(), "bybit#ALT".to_string()]);
}

/// **A `live` account is not started in a node that booted with reconciliation off because no account was
/// armed**: reconciliation is decided once, at the boot, and a `live` account trading with no leg would run
/// unreconciled until the next restart (the boot turns reconcile ON for exactly this case). It keeps the
/// restart path, and answering that reaches no venue. A `demo` account is unchanged
/// (`a_demo_account_on_a_mounted_venue_joins_the_running_core`).
#[test]
fn a_live_account_in_a_node_booted_without_reconcile_keeps_the_restart_path() {
    let mut node = running_node(false);
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed_at(alt(), AccountTier::Live),
        ]))
        .script_start("bybit#ALT", Ok(bind_at(false, AccountTier::Live))),
    );
    let outcome =
        node.hot.start_account(&dynb(&hot_backend), live_alt_rows(), "bybit", &alt(), None);
    assert!(matches!(outcome, HotStart::RestartRequired(_)), "{outcome:?}");
    assert!(hot_backend.calls().is_empty(), "it touched no venue: {:?}", hot_backend.calls());
    assert!(!node.hot.runs("bybit#ALT"));
    assert_eq!(venues_now(&node), 1);
    stop(node);
}

/// A driver config that never runs a pass inside a test: the startup delay is an hour.
fn idle_recon() -> crate::HotRecon {
    crate::HotRecon::new(|| crate::ReconConfig {
        startup_delay: Duration::from_secs(3600),
        interval: None,
        audit_interval: None,
        ..crate::ReconConfig::default()
    })
}

/// Slice S4: a node that booted with reconciliation off (nothing was armed) starts the DRIVER itself with the
/// first account that has a reconcile client. A `live` account is no longer a restart there.
#[test]
fn a_live_account_in_a_node_booted_without_reconcile_starts_the_driver() {
    let mut node = running_node(false);
    node.hot.recon_hot = Some(idle_recon());
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed_at(alt(), AccountTier::Live),
        ]))
        .script_start("bybit#ALT", Ok(bind_at(true, AccountTier::Live))),
    );
    assert!(!node.hot.reconcile_started_hot());
    let outcome =
        node.hot.start_account(&dynb(&hot_backend), live_alt_rows(), "bybit", &alt(), None);
    assert!(matches!(outcome, HotStart::Started { tier: AccountTier::Live, .. }), "{outcome:?}");
    assert!(node.hot.reconcile_started_hot(), "the driver runs now");
    assert!(node.hot.recon_enabled, "and the node reconciles from here on");
    assert!(node.hot.recon_hot.is_none(), "the hook is used up");
    // The start was made WITH reconciliation on (a client is only built then) and a trigger to adopt.
    assert!(
        hot_backend
            .calls()
            .iter()
            .any(|c| matches!(c, ScriptedCall::Start { recon_enabled: true, .. })),
        "{:?}",
        hot_backend.calls()
    );
    node.hot.begin_shutdown_hot_reconcile();
    stop(node);
}

/// The driver starts only when an account brings a reconcile client: a start that has none leaves the hook for
/// the next one, and says nothing false. Once the driver runs, later accounts join it as legs.
#[test]
fn the_driver_waits_for_an_account_with_a_reconcile_client_and_later_accounts_join_it() {
    let mut node = running_node(false);
    node.hot.recon_hot = Some(idle_recon());
    let bob = AccountLabel::parse("BOB").expect("a legal label");
    let hot_backend = Arc::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed(alt()),
            armed(bob.clone()),
        ]))
        .script_start("bybit#ALT", Ok(bind(false)))
        .script_start("bybit#BOB", Ok(bind(true))),
    );
    let rows = demo_policy()
        .with_account("bybit", &alt(), AccountTier::Demo)
        .with_account("bybit", &bob, AccountTier::Demo)
        .accounts;
    assert!(matches!(
        node.hot.start_account(&dynb(&hot_backend), rows.clone(), "bybit", &alt(), None),
        HotStart::Started { .. }
    ));
    assert!(!node.hot.reconcile_started_hot(), "no client, no driver");
    assert!(node.hot.recon_hot.is_some(), "the hook stays for the next start");

    assert!(matches!(
        node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &bob, None),
        HotStart::Started { .. }
    ));
    assert!(node.hot.reconcile_started_hot());
    node.hot.begin_shutdown_hot_reconcile();
    stop(node);
}

// ---- slice S3: a venue the node mounted nothing for ------------------------------------------------

fn rank_of(venue: &str) -> u8 {
    crate::wired_markets()
        .iter()
        .find(|m| m.venue == venue)
        .map(|m| m.engine_rank)
        .expect("a roster venue with an engine row")
}

fn okx_and_bybit_rows() -> (vike_secrets::account_directory::AccountDirectory, Vec<ArmingRow>) {
    let policy = AdapterPolicy::default()
        .with_account("okx", &AccountLabel::Default, AccountTier::Demo)
        .with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
        .with_account("bybit", &alt(), AccountTier::Demo);
    (
        policy.accounts,
        vec![
            armed_on("okx", AccountLabel::Default),
            armed_on("bybit", AccountLabel::Default),
            armed_on("bybit", alt()),
        ],
    )
}

/// **The decision (plan H9), as a test: the default engine of a venue the node mounted nothing for is
/// APPENDED, outside `engine_rank` order.** The node runs `okx` (rank 2); `bybit` (rank 1, the lower
/// one) is activated later and its engine goes LAST: the core's list only grows, an index is an
/// attribution key and never moves, and nothing the core does after `spawn_core_multi` reads a rank. The
/// published order shows it, both accounts run, and the primary is still `okx`'s.
#[test]
fn the_default_engine_of_an_unmounted_venue_is_appended_outside_rank_order() {
    assert!(rank_of("bybit") < rank_of("okx"), "the premise: the later arrival has the lower rank");
    let mut node = node_on("okx", false);
    assert_eq!(venues_now(&node), 1);
    assert!(!node.hot.mounts_venue("bybit"), "the node mounted nothing for bybit");

    let (rows, table_rows) = okx_and_bybit_rows();
    let hot_backend = Arc::new(
        ScriptedBackend::new(table_of(&["okx", "bybit"], true, table_rows))
            .script_start("bybit", Ok(bind(false))),
    );
    let outcome =
        node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &AccountLabel::Default, None);
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");

    assert!(wait_for_venues(&node, 2));
    assert_eq!(
        published_keys(&node),
        vec!["okx".to_string(), "bybit".to_string()],
        "appended after okx although bybit ranks first"
    );
    assert_eq!(node.hot.running_route_keys(), vec!["okx".to_string(), "bybit".to_string()]);
    assert!(node.hot.mounts_venue("bybit"));
    stop(node);
}

/// The first account of an unmounted venue has no sibling to name and nothing to share: its leg is added
/// at once, as the venue's only one, and no existing leg is renamed.
#[test]
fn the_first_reconcile_leg_of_an_unmounted_venue_is_added_unshared() {
    let mut node = node_on("okx", true);
    let (rows, table_rows) = okx_and_bybit_rows();
    let hot_backend = Arc::new(
        ScriptedBackend::new(table_of(&["okx", "bybit"], true, table_rows))
            .script_start("bybit", Ok(bind(true))),
    );
    let (legs_tx, legs_rx) = std::sync::mpsc::channel::<LegChange>();
    let outcome = node.hot.start_account(
        &dynb(&hot_backend),
        rows,
        "bybit",
        &AccountLabel::Default,
        Some(&legs_tx),
    );
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
    match legs_rx.try_recv().expect("the new leg is the first message") {
        LegChange::Add { leg, venue_is_shared } => {
            assert!(!venue_is_shared, "bybit has one engine");
            assert_eq!(leg.venue, "bybit");
            // The default account is the venue's sole one: routed by the venue id, as at boot (the
            // second account's start names it, `a_second_account_on_a_venue_started_hot_takes_the_shared_path`).
            assert_eq!(leg.route_key, None);
        }
        other => panic!("expected the venue's first leg, got {other:?}"),
    }
    assert!(legs_rx.try_recv().is_err(), "no other leg was touched");
    stop(node);
}

/// The unmounted venue's SECOND account then takes the ordinary shared path: the first one's leg is named
/// before the second engine joins.
#[test]
fn a_second_account_on_a_venue_started_hot_takes_the_shared_path() {
    let mut node = node_on("okx", true);
    let (rows, table_rows) = okx_and_bybit_rows();
    let hot_backend = Arc::new(
        ScriptedBackend::new(table_of(&["okx", "bybit"], true, table_rows))
            .script_start("bybit", Ok(bind(true)))
            .script_start("bybit#ALT", Ok(bind(true))),
    );
    let (legs_tx, legs_rx) = std::sync::mpsc::channel::<LegChange>();
    for label in [AccountLabel::Default, alt()] {
        let outcome = node.hot.start_account(
            &dynb(&hot_backend),
            rows.clone(),
            "bybit",
            &label,
            Some(&legs_tx),
        );
        assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
    }
    assert!(matches!(legs_rx.try_recv(), Ok(LegChange::Add { venue_is_shared: false, .. })));
    assert!(matches!(legs_rx.try_recv(), Ok(LegChange::NameAccountsOf(v)) if v == "bybit"));
    assert!(matches!(legs_rx.try_recv(), Ok(LegChange::Add { venue_is_shared: true, .. })));
    stop(node);
}

/// A venue the wiring table gives no engine row is one no build can run an account on: NOT RUNNING with
/// `no live arm` (the roster has none today; the row is removed for the test), the venue never asked.
#[test]
fn a_venue_with_no_engine_row_is_not_running_with_no_live_arm() {
    let mut node = node_on("okx", false);
    node.hot.wired.retain(|(v, _, _)| v != "bybit");
    let (rows, table_rows) = okx_and_bybit_rows();
    let hot_backend = Arc::new(ScriptedBackend::new(table_of(&["okx", "bybit"], true, table_rows)));
    match node.hot.start_account(&dynb(&hot_backend), rows, "bybit", &AccountLabel::Default, None) {
        HotStart::NotRunning(nr) => assert_eq!(nr.block, ArmingBlock::NoLiveArm),
        other => panic!("expected NOT RUNNING, got {other:?}"),
    }
    assert!(hot_backend.calls().is_empty());
    stop(node);
}

// ---- slice S3: the preflight of a venue the boot never preflighted ---------------------------------

/// A scripted backend with preflight legs: the plan lists a credential read for `okx` AND `bybit`
/// (whatever is asked), `authed_read` records every venue it is asked about and refuses the ones in
/// `rejected`.
struct Probed {
    inner: ScriptedBackend,
    rejected: Mutex<HashSet<String>>,
    asked: Mutex<Vec<String>>,
}

impl Probed {
    fn new(inner: ScriptedBackend, rejected: &[&str]) -> Arc<Self> {
        Arc::new(Probed {
            inner,
            rejected: Mutex::new(rejected.iter().map(|v| (*v).to_string()).collect()),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("the log").clone()
    }
}

impl VenueBackend for Probed {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        self.inner.arming_table()
    }

    fn preflight_plan(&self, _q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        Ok(PreflightPlan {
            credential_venues: vec!["bybit".to_string(), "okx".to_string()],
            ..PreflightPlan::default()
        })
    }

    fn server_time_ms(&self, _venue: &str, _tier: AccountTier) -> Result<i64, ServerTimeGap> {
        Err(ServerTimeGap::Unreachable("this double reads no clock".to_string()))
    }

    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        self.asked.lock().expect("the log").push(venue.to_string());
        if self.rejected.lock().expect("the set").contains(venue) {
            return Err(CredentialGap::Rejected("the venue refused the key".to_string()));
        }
        Ok(())
    }

    fn net_reading(&self) -> NetReading {
        NetReading::Unknown
    }

    fn report_not_running(&self, venue: &str, rows: &[VenueArming]) {
        self.inner.report_not_running(venue, rows);
    }

    fn start_adapter(
        &self,
        q: crate::AdapterStartQuery<'_>,
        events: &vike_exec::EventSender,
        trigger: Option<std::sync::mpsc::Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        self.inner.start_adapter(q, events, trigger)
    }

    fn record_identity(
        &self,
        q: &crate::IdentityRecord<'_>,
        recon: Option<&dyn vike_exec::recon::ReconClient>,
    ) -> Result<(), BackendError> {
        self.inner.record_identity(q, recon)
    }

    fn release_refused_start(
        &self,
        venue: &str,
        account: &vike_model::accounts::account_keys::AccountLabel,
        route_key: &str,
    ) {
        self.inner.release_refused_start(venue, account, route_key);
    }
}

fn probed_bybit(rejected: &[&str]) -> Arc<Probed> {
    let (_, table_rows) = okx_and_bybit_rows();
    Probed::new(
        ScriptedBackend::new(table_of(&["okx", "bybit"], true, table_rows))
            .script_start("bybit", Ok(bind(false)))
            .script_start("bybit#ALT", Ok(bind(false))),
        rejected,
    )
}

fn starts_asked(backend: &Probed) -> usize {
    backend.inner.calls().iter().filter(|c| matches!(c, ScriptedCall::Start { .. })).count()
}

/// **A venue the boot never preflighted is preflighted for the hot start, for that venue alone.** The
/// node runs `okx` only; `bybit`'s first account is refused `preflight failed` when its credential read is
/// refused (the venue is never asked to start it, and `okx`'s credentials are not read at all); the
/// refusal is not remembered, so the next attempt asks again and, the cause fixed, starts the account with
/// no restart; and once a venue has passed, its next account is not asked about it again.
#[test]
fn a_venue_the_boot_never_preflighted_is_preflighted_alone_and_asked_again_after_a_fail() {
    let mut node = node_on("okx", false);
    let (rows, _) = okx_and_bybit_rows();
    let backend = probed_bybit(&["bybit"]);

    match node.hot.start_account(
        &dynb(&backend),
        rows.clone(),
        "bybit",
        &AccountLabel::Default,
        None,
    ) {
        HotStart::NotRunning(nr) => assert_eq!(nr.block, ArmingBlock::PreflightFailed),
        other => panic!("expected NOT RUNNING, got {other:?}"),
    }
    assert_eq!(backend.asked(), vec!["bybit".to_string()], "only the starting venue was read");
    assert_eq!(starts_asked(&backend), 0, "a failed preflight reaches no adapter start");
    assert!(!node.hot.runs("bybit"));

    backend.rejected.lock().expect("the set").clear();
    let outcome = node.hot.start_account(
        &dynb(&backend),
        rows.clone(),
        "bybit",
        &AccountLabel::Default,
        None,
    );
    assert!(
        matches!(outcome, HotStart::Started { .. }),
        "the cause fixed, no restart: {outcome:?}"
    );
    assert_eq!(backend.asked(), vec!["bybit".to_string(), "bybit".to_string()]);

    let outcome = node.hot.start_account(&dynb(&backend), rows, "bybit", &alt(), None);
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
    assert_eq!(backend.asked().len(), 2, "a venue that passed is not preflighted again");
    stop(node);
}

/// A venue the boot preflighted (an account of it was armed then) is not asked again: its verdict stands,
/// and no credential of a running account is read for a new one.
#[test]
fn a_venue_the_boot_preflighted_is_not_asked_again() {
    let mut node = node_on("bybit", false);
    let (rows, _) = okx_and_bybit_rows();
    let backend = probed_bybit(&["bybit"]);
    let outcome = node.hot.start_account(&dynb(&backend), rows, "bybit", &alt(), None);
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
    assert!(backend.asked().is_empty(), "nothing was read: {:?}", backend.asked());
    stop(node);
}

/// The boot's verdict stands for the (venue, TIER) it judged: a venue that booted with a `demo` account and
/// gets a `live` one later has had its live credentials and clock looked at by nobody, so the live start is
/// preflighted (the same venue, a different tier).
#[test]
fn a_live_account_on_a_venue_the_boot_preflighted_at_demo_is_preflighted_at_live() {
    let mut node = node_on("bybit", true);
    let backend = Probed::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed_at(alt(), AccountTier::Live),
        ]))
        .script_start("bybit#ALT", Ok(bind_at(false, AccountTier::Live))),
        &["bybit"],
    );
    match node.hot.start_account(&dynb(&backend), live_alt_rows(), "bybit", &alt(), None) {
        HotStart::NotRunning(nr) => assert_eq!(nr.block, ArmingBlock::PreflightFailed),
        other => panic!("expected the live preflight to run and fail, got {other:?}"),
    }
    assert_eq!(backend.asked(), vec!["bybit".to_string()], "the live tier was looked at");
    assert_eq!(starts_asked(&backend), 0);
    stop(node);
}

/// `flags.preflight_skip` removes the hot start's preflight as it removes the boot's.
#[test]
fn the_preflight_skip_flag_skips_the_hot_start_preflight_too() {
    let mut node = node_on("okx", false);
    node.hot.vars.insert(crate::preflight::PREFLIGHT_SKIP_ENV.to_string(), "1".to_string());
    let (rows, _) = okx_and_bybit_rows();
    let backend = probed_bybit(&["bybit"]);
    let outcome =
        node.hot.start_account(&dynb(&backend), rows, "bybit", &AccountLabel::Default, None);
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
    assert!(backend.asked().is_empty());
    stop(node);
}

// ---- the balance window of a started account (the okx drill of 2026-10-18) -----------------------------

/// Make `node`'s starts report the flag they announce on the returned channel, as `(route key, flag)`.
fn listen_for_announcements(
    node: &mut crate::AssembledRuntime,
) -> std::sync::mpsc::Receiver<(String, Arc<crate::FirstBalance>)> {
    let (told, heard) = std::sync::mpsc::channel::<(String, Arc<crate::FirstBalance>)>();
    node.hot.first_balance_notice =
        Some(Box::new(move |route_key: &str, flag: &Arc<crate::FirstBalance>| {
            told.send((route_key.to_string(), Arc::clone(flag))).expect("the test listens");
        }));
    heard
}

/// **A start that gives the driver a leg announces the flag that leg raises**, before the engine joins the
/// core: the control door holds the account's opening orders on it until the venue's balance has landed.
#[test]
fn a_start_with_a_reconcile_leg_announces_the_flag_its_leg_raises() {
    let mut node = running_node(true);
    let heard = listen_for_announcements(&mut node);
    let (legs_tx, legs_rx) = std::sync::mpsc::channel::<LegChange>();
    start_alt(&mut node, Some(&legs_tx));

    let (route_key, flag) = heard.try_recv().expect("the start announced its wait");
    assert_eq!(route_key, "bybit#ALT");
    assert!(!flag.landed(), "no balance has landed");
    assert!(heard.try_recv().is_err(), "once, for the one account started");
    assert!(matches!(legs_rx.try_recv(), Ok(LegChange::NameAccountsOf(_))));
    match legs_rx.try_recv().expect("then the new leg") {
        LegChange::Add { leg, .. } => assert!(
            leg.balance_synced.as_ref().is_some_and(|raised| Arc::ptr_eq(raised, &flag)),
            "the leg raises the very flag that was announced"
        ),
        other => panic!("the new account's leg, got {other:?}"),
    }
    stop(node);
}

/// A start no balance will ever land for announces NOTHING, so nothing waits on it: a `paper` account (no
/// reconcile client at all), a `demo` account whose adapter gave no reconcile client, and one whose client
/// has no driver to take its leg.
#[test]
fn a_start_nothing_will_seed_announces_nothing() {
    let mut node = running_node(true);
    let heard = listen_for_announcements(&mut node);
    let (legs_tx, _legs_rx) = std::sync::mpsc::channel::<LegChange>();
    let bob = AccountLabel::parse("BOB").expect("a legal label");
    let cat = AccountLabel::parse("CAT").expect("a legal label");
    let backend = Arc::new(
        ScriptedBackend::new(table(vec![
            armed(AccountLabel::Default),
            armed(alt()),
            armed(bob.clone()),
            armed(cat.clone()),
        ]))
        .script_start("bybit#BOB", Ok(bind(false)))
        .script_start("bybit#CAT", Ok(bind(true))),
    );
    let rows = demo_policy()
        .with_account("bybit", &alt(), AccountTier::Paper)
        .with_account("bybit", &bob, AccountTier::Demo)
        .with_account("bybit", &cat, AccountTier::Demo)
        .accounts;

    // A paper account, with a driver mailbox open: it is the simulator, there is no leg to give.
    let paper =
        node.hot.start_account(&dynb(&backend), rows.clone(), "bybit", &alt(), Some(&legs_tx));
    assert!(matches!(paper, HotStart::Started { tier: AccountTier::Paper, .. }), "{paper:?}");
    // A demo account whose adapter built no reconcile client.
    let no_client =
        node.hot.start_account(&dynb(&backend), rows.clone(), "bybit", &bob, Some(&legs_tx));
    assert!(matches!(no_client, HotStart::Started { .. }), "{no_client:?}");
    // A demo account with a client and no driver: this node booted with reconciliation on, so it starts
    // none of its own and the leg has nowhere to go.
    let no_driver = node.hot.start_account(&dynb(&backend), rows, "bybit", &cat, None);
    assert!(matches!(no_driver, HotStart::Started { .. }), "{no_driver:?}");

    assert!(heard.try_recv().is_err(), "none of the three left anything waiting");
    stop(node);
}

/// The ASSEMBLY builds its legs with no cell: the composition root gives the boot's legs theirs, through
/// `crate::await_first_balances` (decision 0246), so a node assembled without a root holds nothing at the door.
#[test]
fn the_assemblys_legs_carry_no_cell() {
    let node = running_node(true);
    assert!(!node.recon_clients.is_empty(), "premise: the boot built a reconcile leg");
    assert!(node.recon_clients.iter().all(|leg| leg.balance_synced.is_none()));
    stop(node);
}

/// A reconcile client holding `balance` at the venue and nothing else to report.
struct Reporting(f64);
impl vike_exec::recon::ReconClient for Reporting {
    empty_report_fetches!();
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        Ok(Some(self.0))
    }
}

/// Poll `ready` for up to `secs` seconds.
fn until(secs: u64, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    ready()
}

/// A limit BUY from `bybit`'s `ALT` account.
fn buy_from_alt(node: &crate::AssembledRuntime, coid: &str, symbol: &str, qty: f64, price: f64) {
    let request = vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "bybit".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        account: Some(alt()),
        ..Default::default()
    };
    node.handle
        .send_command(vike_exec::Command::Order(vike_exec::OrderIntent::Submit(Box::new(request))));
}

/// **The okx drill, end to end, over the real driver and the real core.** A hot-started account at 4x on a
/// seed of 1000, whose venue holds 4999.625: 0.1 BTC at 72872.9 needs 1821.82 of margin.
///
/// * Inside the window (the driver's first pass is 1.5 s away) the engine judges on the seed and denies the
///   order `insufficient-margin`: the premise, and why the door holds the account's opening orders.
/// * The announced flag rises when the pass is on the core's lane, and an order sent the instant it is seen
///   is ADMITTED: it queues behind the pass on the core's one FIFO, so it is never judged on the seed. (An
///   order sent from the flag, without waiting for the book to show the balance, is the test of that order.)
#[test]
fn an_order_sent_when_the_first_balance_flag_rises_is_judged_on_the_venues_balance() {
    let risk = vike_model::ProfileRisk { max_leverage: Some(4.0), ..budget() };
    let mut node = node_with("bybit", true, 1_000.0, risk);
    let heard = listen_for_announcements(&mut node);
    let config =
        crate::ReconConfig { startup_delay: Duration::from_millis(1_500), ..Default::default() };
    let driver = crate::spawn_recon(&node.handle, Vec::new(), config, None);

    let backend = Arc::new(
        ScriptedBackend::new(table(vec![armed(AccountLabel::Default), armed(alt())])).script_start(
            "bybit#ALT",
            Ok(AdapterBind { recon: Some(Box::new(Reporting(4_999.625))), ..bind(false) }),
        ),
    );
    let rows = demo_policy().with_account("bybit", &alt(), AccountTier::Demo).accounts;
    let outcome =
        node.hot.start_account(&dynb(&backend), rows, "bybit", &alt(), Some(&driver.leg_sender()));
    assert!(matches!(outcome, HotStart::Started { .. }), "{outcome:?}");
    let (_, synced) = heard.try_recv().expect("the start announced its wait");
    assert!(wait_for_venues(&node, 2), "the core published the appended engine");

    let alt_block = |node: &crate::AssembledRuntime| {
        node.handle
            .snapshot()
            .portfolio
            .venues
            .iter()
            .find(|v| v.route_key == "bybit#ALT")
            .cloned()
            .expect("the appended engine is published")
    };
    let before = alt_block(&node);
    assert_eq!(before.balance_mode, vike_exec::BalanceMode::Delta);
    assert!((before.equity - 1_000.0).abs() < 1e-9, "the seed: {}", before.equity);

    // Inside the window: denied on the seed.
    buy_from_alt(&node, "early", &before.symbol, 0.1, 72_872.9);
    assert!(
        until(5, || {
            node.handle
                .snapshot()
                .recent_events
                .iter()
                .any(|line| line.contains("OrderDenied early (insufficient-margin)"))
        }),
        "the seed does not fund 1821.82 of margin: {:?}",
        node.handle.snapshot().recent_events
    );
    assert!(!synced.landed(), "still inside the window");

    // The flag rises; the order goes the instant it is seen.
    assert!(until(15, || synced.landed()), "the driver's first pass never came");
    buy_from_alt(&node, "late", &before.symbol, 0.1, 72_872.9);
    assert!(
        until(5, || node.handle.snapshot().order("late").is_some()),
        "admitted on the venue's balance: {:?}",
        node.handle.snapshot().recent_events
    );
    let denials = node
        .handle
        .snapshot()
        .recent_events
        .iter()
        .filter(|line| line.contains("OrderDenied"))
        .count();
    assert_eq!(denials, 1, "only the early order was ever denied");

    let after = alt_block(&node);
    assert_eq!(after.balance_mode, vike_exec::BalanceMode::Authoritative);
    assert!((after.equity - 4_999.625).abs() < 1e-6, "the venue's figure: {}", after.equity);

    assert!(driver.shutdown_within(Duration::from_secs(5)), "the driver ended");
    stop(node);
}
