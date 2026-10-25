//! **What okx's claim to list trigger orders means for a RESTART** (decision 0192): the recon client
//! answers `lists_trigger_orders` true, so the restore planner may call a stop the report does not list
//! gone, but only on proof. Every case runs the REAL `OkxReconClient` over a scripted `OkxTransport`
//! (frames CAPTURED on the demo account on 2026-10-24, `tests/fixtures/algo/`) and hands what it reports
//! to the REAL restore planner (`crates/vike-exec/src/recon/restore.rs`'s `plan_restore`) or to a REAL
//! spawned core (`vike_core::spawn_core`), the way the reconcile manager does.
//!
//! The proof rules, one test each:
//! - a stop the report LISTS is adopted, and an operator's stop only on a `demo` engine whose client
//!   claims (never on a `live` one, never without the claim);
//! - a stop that FIRED while the node was down is `filled_while_down` however many quiet passes the
//!   record has, because its child's fills are named by the stop's own id (`orders-algo-history`);
//! - a stop that was CANCELLED while down (the report lists nothing, no fill) is held until the quiet
//!   passes, and only then gone: the claim is what lets it resolve at all;
//! - the fill naming is STRICT: a stop history that cannot be read fails the fill read for the first
//!   three passes since the last good read (a fill list that silently lacked the names would let the
//!   planner read a filled stop as gone), then goes ahead unnamed, and a good read makes it strict again.
//!
//! NOT proved here: a stop that fired while the node was DOWN across a real restart on the venue (the
//! live drill owes it; the frames are captures and the join runs offline).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_core::order_owners::OrderOwnerLog;
use vike_core::replay::JournalOrder;
use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_exec::recon::{
    BalanceTol, RESTORE_QUIET_PASSES, ReconClient, ReconMode, ReconPolicy, RestoreScope,
    RestoredOrderRef, plan_restore,
};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, Command, ExecutionEngine, ReconcileReports, RiskGate};
use vike_model::{AccountTier, OrderType, RiskLimits};
use vike_okx::OkxReconClient;
use vike_okx::transport::OkxTransport;
use vike_secrets::credentials::Credentials;

use crate::triggered_stop::{CHILD_CLORDID, CHILD_ORDID, CT_VAL, STOP, SYMBOL, fixture};

/// An operator's stop (a GUI ticket's, no mount minted it) the node restored from its record.
const OP_STOP: &str = "b7okxop1";

// ---- a scripted OKX -----------------------------------------------------------------------------------

/// What the scripted venue answers, shared with the test so a case can change it between passes.
struct State {
    /// The rows of `orders-algo-pending`.
    stops: Mutex<Vec<Value>>,
    /// The `trade/fills` envelope.
    fills: Mutex<Value>,
    /// The `orders-algo-history` answer.
    history: Mutex<Result<Value, VenueApiError>>,
    /// How many times `orders-algo-history` was asked.
    history_reads: AtomicUsize,
}

struct Venue(Arc<State>);

fn envelope(rows: Vec<Value>) -> Value {
    json!({"code": "0", "msg": "", "data": rows})
}

impl OkxTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, Value)],
        _signer: &OkxV5Signer,
    ) -> Result<Value, VenueApiError> {
        match path.rsplit('/').next().unwrap_or(path) {
            "orders-pending" => Ok(envelope(Vec::new())),
            "orders-algo-pending" => Ok(envelope(self.0.stops.lock().unwrap().clone())),
            "fills" => Ok(self.0.fills.lock().unwrap().clone()),
            "orders-algo-history" => {
                self.0.history_reads.fetch_add(1, Ordering::SeqCst);
                self.0.history.lock().unwrap().clone()
            }
            other => panic!("a restore pass reads orders, stops and fills only: {other}"),
        }
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        panic!("the recon client must use the SIGNED lane, not public ({path})");
    }
}

fn no_fills() -> Value {
    envelope(Vec::new())
}

/// A client over a venue that lists `stops`, has done `fills` and answers `history`.
fn client(
    stops: Vec<Value>,
    fills: Value,
    history: Result<Value, VenueApiError>,
) -> (OkxReconClient<Venue>, Arc<State>) {
    let state = Arc::new(State {
        stops: Mutex::new(stops),
        fills: Mutex::new(fills),
        history: Mutex::new(history),
        history_reads: AtomicUsize::new(0),
    });
    let creds = Credentials {
        api_key: "k".to_string().into(),
        api_secret: "s".to_string().into(),
        passphrase: Some("p".to_string().into()),
    };
    let client = OkxReconClient::new(
        OkxV5Signer::new(&creds, || 0),
        Venue(Arc::clone(&state)),
        "https://unused.invalid",
        SYMBOL,
        CT_VAL,
    );
    (client, state)
}

/// A resting stop as `orders-algo-pending` lists it.
fn stop_row(coid: &str) -> Value {
    json!({
        "instId": SYMBOL, "algoId": "9001", "algoClOrdId": coid, "ordType": "conditional",
        "side": "sell", "sz": "0.01", "state": "live", "slTriggerPx": "80211",
        "reduceOnly": "true", "uTime": "1700000000000"
    })
}

fn down() -> Result<Value, VenueApiError> {
    Err(VenueApiError { code: 50011, msg: "rate limit".into() })
}

fn history_of_the_fired_stop() -> Result<Value, VenueApiError> {
    Ok(fixture!("rest_orders_algo_history_effective.json"))
}

// ---- the planner, over what the real client reports ------------------------------------------------

/// A restored stop of the record: the kind, venue and symbol the ownership file's line carries.
fn restored(coid: &str, quiet_passes: u32) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some("okx".to_string()),
        symbol: Some(SYMBOL.to_string()),
        account: None,
        order_type: Some(OrderType::Stop),
        venue_order_id: None,
        quiet_passes,
    }
}

/// The scope the core builds for the pass of `client`: its claim rides the pass.
fn scope_of(client: &impl ReconClient) -> RestoreScope {
    RestoreScope {
        venue: "okx".to_string(),
        account: None,
        symbols: vec![SYMBOL.to_string()],
        lists_trigger_orders: client.lists_trigger_orders(),
    }
}

#[test]
fn a_resting_stop_the_client_lists_is_adopted_as_a_live_stop() {
    let (client, _) = client(vec![stop_row(OP_STOP)], no_fills(), down());
    let orders = client.fetch_order_status_reports(0).expect("orders");
    let fills = client.fetch_fill_reports(0).expect("fills");
    let plan = plan_restore(&scope_of(&client), &[restored(OP_STOP, 0)], &orders, &fills);
    assert_eq!(plan.adopt.len(), 1, "{plan:?}");
    assert_eq!(plan.adopt[0].client_order_id.as_deref(), Some(OP_STOP));
    assert_eq!(plan.adopt[0].order_type, OrderType::Stop);
    assert!(plan.gone.is_empty() && plan.unproven.is_empty(), "{plan:?}");
}

/// **A stop that FIRED while the node was down is filled, never gone**, even for a record that has
/// already counted every quiet pass (the worst case for a stop that did its job): the report lists no
/// stop, the venue's own child order carries `O…`, and `orders-algo-history` names the child's fills
/// under the stop's coid, so the planner joins them to the record.
#[test]
fn a_stop_that_fired_while_down_is_filled_not_gone_after_every_quiet_pass() {
    let (client, state) =
        client(Vec::new(), fixture!("rest_fills.json"), history_of_the_fired_stop());
    let orders = client.fetch_order_status_reports(0).expect("orders");
    assert!(orders.is_empty(), "a fired stop is no longer pending: {orders:?}");
    let fills = client.fetch_fill_reports(0).expect("fills");
    let child = fills.iter().find(|f| f.venue_order_id.as_str() == CHILD_ORDID).expect("child");
    assert_eq!(child.client_order_id.as_deref(), Some(STOP), "named, not {CHILD_CLORDID}");
    assert_eq!(state.history_reads.load(Ordering::SeqCst), 1, "one history read for the pass");

    let plan =
        plan_restore(&scope_of(&client), &[restored(STOP, RESTORE_QUIET_PASSES)], &orders, &fills);
    assert_eq!(plan.filled_while_down, [STOP], "{plan:?}");
    assert!(plan.gone.is_empty() && plan.unproven.is_empty() && plan.adopt.is_empty(), "{plan:?}");
}

/// **A stop cancelled while down resolves, and only on the quiet passes.** The report lists nothing and
/// no fill names it: held (unproven) for every pass short of the count, gone at it. Before the claim
/// the same silence is held for good: the planner will not read a kind the client does not claim.
#[test]
fn a_stop_cancelled_while_down_is_gone_only_after_the_quiet_passes() {
    let (client, _) = client(Vec::new(), no_fills(), down());
    let orders = client.fetch_order_status_reports(0).expect("orders");
    let fills = client.fetch_fill_reports(0).expect("fills");
    let scope = scope_of(&client);
    assert!(scope.lists_trigger_orders, "okx claims");
    for quiet in 0..RESTORE_QUIET_PASSES {
        let plan = plan_restore(&scope, &[restored(OP_STOP, quiet)], &orders, &fills);
        assert_eq!(plan.unproven.len(), 1, "{quiet} quiet passes prove nothing: {plan:?}");
        assert!(plan.gone.is_empty(), "{plan:?}");
    }
    let proof = [restored(OP_STOP, RESTORE_QUIET_PASSES)];
    let plan = plan_restore(&scope, &proof, &orders, &fills);
    assert_eq!(plan.gone.len(), 1, "the count is the proof: {plan:?}");

    let unclaimed = RestoreScope { lists_trigger_orders: false, ..scope };
    let plan = plan_restore(&unclaimed, &proof, &orders, &fills);
    assert!(plan.gone.is_empty() && plan.unproven.len() == 1, "no claim, no verdict: {plan:?}");
}

// ---- the naming is strict -------------------------------------------------------------------------

/// How many failing passes in a row fail the fill read before it goes ahead unnamed.
const STRICT: usize = 3;

#[test]
fn the_first_three_failing_passes_fail_the_fill_read_and_the_fourth_goes_ahead_unnamed() {
    let (client, _) = client(Vec::new(), fixture!("rest_fills.json"), down());
    for n in 1..=STRICT {
        let why = client.fetch_fill_reports(0).expect_err("a fill list that lacks the names");
        assert!(why.contains("orders-algo-history"), "failing pass {n}: {why}");
    }
    let fills =
        client.fetch_fill_reports(0).expect("the pass goes ahead: the account is not blind");
    assert_eq!(fills.len(), 2);
    let child = fills.iter().find(|f| f.venue_order_id.as_str() == CHILD_ORDID).expect("child");
    assert_eq!(
        child.client_order_id.as_deref(),
        Some(CHILD_CLORDID),
        "unnamed, as the venue has it"
    );
    assert!(client.fetch_fill_reports(0).is_ok(), "and stays open while the endpoint stays down");
}

#[test]
fn a_good_history_read_makes_the_fill_read_strict_again() {
    let (client, state) = client(Vec::new(), fixture!("rest_fills.json"), down());
    for _ in 0..STRICT {
        assert!(client.fetch_fill_reports(0).is_err());
    }
    *state.history.lock().unwrap() = history_of_the_fired_stop();
    assert!(client.fetch_fill_reports(0).is_ok(), "the read works again");
    *state.history.lock().unwrap() = down();
    assert!(client.fetch_fill_reports(0).is_err(), "one good pass: strict from the first failure");
}

/// A pass with no fills has nothing to name: a good read, which also makes the next failure strict.
#[test]
fn a_pass_with_no_fills_is_a_good_read() {
    let (client, state) = client(Vec::new(), fixture!("rest_fills.json"), down());
    for _ in 0..STRICT {
        assert!(client.fetch_fill_reports(0).is_err());
    }
    let reads = state.history_reads.load(Ordering::SeqCst);
    *state.fills.lock().unwrap() = no_fills();
    assert!(client.fetch_fill_reports(0).expect("fills").is_empty());
    assert_eq!(state.history_reads.load(Ordering::SeqCst), reads, "nothing to name, nothing read");
    *state.fills.lock().unwrap() = fixture!("rest_fills.json");
    assert!(client.fetch_fill_reports(0).is_err(), "strict again, from the first failure");
}

// ---- the core: an operator's stop is adopted on a demo engine only --------------------------------------

/// The core clock of every boot: ownership records are stamped with it.
const NOW: i64 = 1_700_000_000_000;

/// The pseudo mount an operator's stop (no mount minted it) is filed under in the ownership file:
/// `vike_core::order_owners`' `PROTECTIVE_STOP_MOUNT`, crate-private.
const PROTECTIVE_STOP_MOUNT: &str = "@protective-stop";

/// A fresh state directory under the cargo target's scratch (a test target's own, removed first).
fn state_dir(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("okx-claim-restart")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory");
    dir
}

/// The operator's reduce-only stop as the journal knows it: a candidate under the pseudo mount.
fn operators_stop() -> JournalOrder {
    JournalOrder {
        coid: OP_STOP.to_string(),
        mount_id: PROTECTIVE_STOP_MOUNT.to_string(),
        venue: Some("okx".to_string()),
        symbol: Some(SYMBOL.to_string()),
        reduce_only: true,
        trigger_price: Some(80_211.0),
        order_type: Some(OrderType::Stop),
        tier: Some(AccountTier::Demo),
        ..JournalOrder::default()
    }
}

/// One boot of an okx core at `tier` over the ownership file in `dir`.
fn boot(dir: &Path, tier: AccountTier, journal_orders: Vec<JournalOrder>) -> CoreHandle {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        SYMBOL,
    );
    engine.tier = tier;
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(|| NOW),
        order_owners: Some(OrderOwnerLog::in_state_dir(dir)),
        journal_orders,
        ..CoreConfig::default()
    };
    spawn_core(engine, config)
}

/// One pass of `client`'s reads, as the reconcile manager builds it, claiming what the client claims
/// unless `claim` says otherwise.
fn pass(client: &impl ReconClient, claim: Option<bool>) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "okx".to_string(),
        since: 0,
        orders: client.fetch_order_status_reports(0).expect("orders"),
        fills: client.fetch_fill_reports(0).expect("fills"),
        positions: Vec::new(),
        policy: ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
        admitted_symbols: client.admitted_symbols(),
        lists_trigger_orders: claim.unwrap_or_else(|| client.lists_trigger_orders()),
    }))
}

/// What the core held after one pass and a barrier: `(the stop is a live order, the restore notes)`.
fn after_one_pass(handle: CoreHandle, pass: Command) -> (bool, Vec<String>) {
    let cell = handle.snapshot_cell();
    handle.send_command(pass);
    handle.shutdown_and_join();
    let snap = cell.load_full();
    let adopted = snap.orders.iter().any(|o| o.client_order_id == OP_STOP && o.status.is_live());
    let restore_notes = snap.recent_events.iter().filter(|l| l.contains("RESTORE okx"));
    (adopted, restore_notes.map(|l| l.to_string()).collect())
}

/// **An operator's stop on a `demo` okx engine whose report lists it is adopted**, silently: it
/// becomes a live order the node can cancel, and the pass says nothing about releasing it.
#[test]
fn an_operators_stop_on_a_demo_okx_engine_is_adopted() {
    let dir = state_dir("demo");
    let (client, _) = client(vec![stop_row(OP_STOP)], no_fills(), down());
    let handle = boot(&dir, AccountTier::Demo, vec![operators_stop()]);
    let (adopted, notes) = after_one_pass(handle, pass(&client, None));
    assert!(adopted, "adopted as a live order: {notes:?}");
    assert!(notes.iter().all(|n| !n.contains("NOT adopted")), "{notes:?}");
}

/// The same report without the claim (what okx answered until now) releases the stop at the pass: the
/// claim is what makes the adoption possible.
#[test]
fn without_the_claim_the_same_stop_is_released_not_adopted() {
    let dir = state_dir("demo-unclaimed");
    let (client, _) = client(vec![stop_row(OP_STOP)], no_fills(), down());
    let handle = boot(&dir, AccountTier::Demo, vec![operators_stop()]);
    let (adopted, notes) = after_one_pass(handle, pass(&client, Some(false)));
    assert!(!adopted);
    assert!(notes.iter().any(|n| n.contains("does not list trigger orders")), "{notes:?}");
}

/// **Never on a `live` engine** (the owner has not ruled on it): the record outlives a tier change, and
/// the pass asks the tier again, so a stop filed by a demo boot is released by a live one and the
/// venue's listing of it adopts nothing.
#[test]
fn an_operators_stop_on_a_live_okx_engine_is_never_adopted() {
    let dir = state_dir("live");
    let (client, _) = client(vec![stop_row(OP_STOP)], no_fills(), down());
    boot(&dir, AccountTier::Demo, vec![operators_stop()]).shutdown_and_join();
    let handle = boot(&dir, AccountTier::Live, Vec::new());
    let (adopted, notes) = after_one_pass(handle, pass(&client, None));
    assert!(!adopted, "a live engine does not adopt an operator's stop");
    assert!(notes.iter().any(|n| n.contains("NOT adopted")), "{notes:?}");
}
