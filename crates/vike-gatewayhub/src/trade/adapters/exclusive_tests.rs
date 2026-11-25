//! `exclusive`'s unit tests: a claim is taken once, released when it drops, names its holder, and a
//! running resource's claim rides its client and ends with it.

use super::*;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// One claim per resource per process.
#[test]
fn the_claim_is_taken_once_and_refused_while_it_is_held() {
    let claimed = Mutex::new(Vec::new());
    let held = claim_in(&claimed, "JForex sidecar").expect("the first caller claims it");
    assert!(claim_in(&claimed, "JForex sidecar").is_none(), "refused while it is held");
    drop(held);
    assert!(claim_in(&claimed, "JForex sidecar").is_some(), "…and free once the guard dropped");
}

/// THE RELEASE (Review Focus 3): a claim dropped frees the resource for the next account.
#[test]
fn a_claim_that_is_dropped_is_released() {
    let claimed = Mutex::new(Vec::new());
    {
        let _failed = claim_in(&claimed, "JForex sidecar").expect("claimed");
        assert!(
            claim_in(&claimed, "JForex sidecar").is_none(),
            "held while the start is in flight"
        );
    }
    let second = claim_in(&claimed, "JForex sidecar").expect("a FAILED start leaves it claimable");
    assert!(claim_in(&claimed, "JForex sidecar").is_none(), "…and the one that started holds it");
    drop(second);
}

/// A claim held on one resource does not block another resource's.
#[test]
fn two_resources_claim_independently() {
    let claimed = Mutex::new(Vec::new());
    let a = claim_in(&claimed, "JForex sidecar").expect("the first claim wins");
    assert!(claim_in(&claimed, "another venue's resource").is_some(), "another resource is free");
    drop(a);
}

/// Hand `claimed` to a thread that panics while holding its lock, so the mutex is POISONED.
fn poison(claimed: &Mutex<Vec<String>>) {
    std::thread::scope(|s| {
        let joined = s
            .spawn(|| {
                let _held = claimed.lock().expect("not yet poisoned");
                panic!("a thread dies holding the claim set");
            })
            .join();
        assert!(joined.is_err(), "the thread panicked");
    });
    assert!(claimed.is_poisoned(), "precondition: the set is poisoned");
}

/// ⚠ A POISONED set is recovered, not read as "already claimed". Both halves: a claim over it is
/// granted, and a guard dropped over it still releases — otherwise the next account would be
/// refused with a reason that is not true.
#[test]
fn a_poisoned_claim_set_still_claims_and_releases() {
    let claimed = Mutex::new(Vec::new());
    poison(&claimed);
    {
        let _first = claim_in(&claimed, "JForex sidecar").expect("a poisoned set still claims");
        assert!(claim_in(&claimed, "JForex sidecar").is_none(), "…and still refuses a second");
    }
    assert!(
        claim_in(&claimed, "JForex sidecar").is_some(),
        "a guard dropped over a poisoned set still releases its resource"
    );
}

/// The holder rule.
#[test]
fn the_policy_names_the_holder_and_the_default_yields() {
    assert_eq!(pick_holder(&[]), AccountLabel::Default);
    assert_eq!(pick_holder(&[label("3716974")]), label("3716974"));
    assert_eq!(
        pick_holder(&[label("3709890"), label("3716974")]),
        label("3709890"),
        "the pick must be deterministic, or two starts of one box would trade two brokers"
    );
}

fn live_outcome() -> vike_bridge_core::venue_adapter::AdapterOutcome {
    use vike_bridge_core::venue_adapter::{
        AdapterOutcome, VenueAdapterExecutionOutcome, VenueAdapterExecutionRunning,
    };
    AdapterOutcome {
        exec: VenueAdapterExecutionOutcome::Running(VenueAdapterExecutionRunning {
            client: Box::new(vike_tradehub_core::test_support::doubles::NoopClient),
            bound_tier: vike_model::AccountTier::Demo,
            grid: None,
            contract_value: None,
            margin_mode: None,
            leg_grids: Vec::new(),
        }),
        reconcile: None,
        identity: None,
    }
}

/// A running resource's claim rides its client: held while the client lives, released the moment it
/// drops (the adapter ended), claimable again afterwards.
#[test]
fn a_running_resources_claim_is_held_by_its_client_and_released_when_it_drops() {
    const RESOURCE: &str = "the hold-while-running test's resource";
    let claim = claim(RESOURCE).expect("the first caller claims it");
    let outcome = hold_while_running(claim, live_outcome());
    assert!(super::claim(RESOURCE).is_none(), "held while the client lives");
    drop(outcome);
    let again = super::claim(RESOURCE);
    assert!(again.is_some(), "the client dropped, so the resource is claimable again");
}

/// An outcome that came up empty (`Paper`) takes the claim down at once.
#[test]
fn a_start_that_came_up_empty_releases_the_claim_at_once() {
    const RESOURCE: &str = "the empty-outcome test's resource";
    let claim = claim(RESOURCE).expect("the first caller claims it");
    let outcome =
        hold_while_running(claim, vike_bridge_core::venue_adapter::AdapterOutcome::paper());
    assert!(super::claim(RESOURCE).is_some(), "nothing runs, so nothing is held");
    drop(outcome);
}

/// Records every `ExecutionClient` door it is called through, by name, with a distinctive answer where the
/// trait has a default that would differ.
struct Recording {
    calls: std::sync::Arc<Mutex<Vec<&'static str>>>,
}

impl Recording {
    fn note(&self, door: &'static str) {
        self.calls.lock().expect("the log").push(door);
    }
}

impl vike_execution::ExecutionClient for Recording {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {
        self.note("submit");
    }
    fn cancel(&mut self, _order_client_id: &str) {
        self.note("cancel");
    }
    fn cancel_with_intent(
        &mut self,
        _order_client_id: &str,
        _intent: vike_model::OrderCancelIntent,
    ) {
        self.note("cancel_with_intent");
    }
    fn modify(&mut self, _order: &vike_model::OrderRequest, _q: Option<f64>, _p: Option<f64>) {
        self.note("modify");
    }
    fn modify_trigger(
        &mut self,
        _order: &vike_model::OrderRequest,
        _q: Option<f64>,
        _p: Option<f64>,
        _t: f64,
    ) -> Result<(), &'static str> {
        self.note("modify_trigger");
        Ok(())
    }
    fn submit_batch(&mut self, _requests: &[vike_model::OrderRequest]) {
        self.note("submit_batch");
    }
    fn cancel_batch(&mut self, _order_client_ids: &[String]) {
        self.note("cancel_batch");
    }
    fn cancel_batch_with_intent(
        &mut self,
        _ids: &[String],
        _intent: vike_model::OrderCancelIntent,
    ) {
        self.note("cancel_batch_with_intent");
    }
    fn confirm(&mut self, _order_client_id: &str) {
        self.note("confirm");
    }
    fn begin_detach(&mut self) {
        self.note("begin_detach");
    }
    fn detach(&mut self) {
        self.note("detach");
    }
    fn poll_events(&mut self) -> Option<vike_model::events::Event> {
        self.note("poll_events");
        None
    }
    fn on_bar(&mut self, _bar: &vike_marketdata::Bar) {
        self.note("on_bar");
    }
    fn on_bar_sides(
        &mut self,
        _bar: &vike_marketdata::Bar,
        _sides: Option<&vike_marketdata::BarSides>,
    ) {
        self.note("on_bar_sides");
    }
    fn on_quote(&mut self, _symbol: &str, _bid: f64, _ask: f64, _timestamp: i64) {
        self.note("on_quote");
    }
    fn fill_at_close(&mut self, _order_client_ids: &[String], _bar_width_ms: i64) {
        self.note("fill_at_close");
    }
    fn hold_for_close(&mut self, _hold: bool) {
        self.note("hold_for_close");
    }
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        self.note("amend_semantics");
        Some(vike_venues::AmendSemantics::CancelReplace)
    }
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        self.note("wire_id_for");
        Some(format!("wire-of-{order_client_id}"))
    }
    fn adopt_restored(&mut self, _orders: &[vike_execution::RestoredOrder]) {
        self.note("adopt_restored");
    }
    fn on_contract_value(&mut self, _symbol: &str, _value: vike_model::ContractValue) {
        self.note("on_contract_value");
    }
}

/// The doors the test below drives: the `ExecutionClient` methods, all 20.
const DOORS: [&str; 21] = [
    "submit",
    "cancel",
    "cancel_with_intent",
    "modify",
    "modify_trigger",
    "submit_batch",
    "cancel_batch",
    "cancel_batch_with_intent",
    "confirm",
    "begin_detach",
    "detach",
    "poll_events",
    "on_bar",
    "on_bar_sides",
    "on_quote",
    "fill_at_close",
    "hold_for_close",
    "amend_semantics",
    "wire_id_for",
    "adopt_restored",
    "on_contract_value",
];

/// **`ExclusiveClaimClient` forwards EVERY `ExecutionClient` door to the client it wraps** (review 15, finding 4).
/// It sits in front of the dukascopy client for the life of the adapter: a door it forgot would answer
/// the trait's DEFAULT (a no-op `begin_detach`/`detach` would leave the sidecar running at teardown, a
/// `cancel_with_intent` default would drop the intent) and compile without a word. Each door is driven
/// once through the wrapper and must be seen by the inner client, in order.
#[test]
fn the_claimed_client_forwards_every_door_of_the_execution_client() {
    use vike_execution::ExecutionClient;
    const RESOURCE: &str = "the forwarding test's resource";
    let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
    let mut client = ExclusiveClaimClient {
        inner: Box::new(Recording { calls: std::sync::Arc::clone(&calls) }),
        _claim: claim(RESOURCE).expect("the first caller claims it"),
    };
    let order = vike_model::OrderRequest::default();
    let bar = vike_marketdata::Bar {
        timestamp: 1,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    client.submit(&order);
    client.cancel("c");
    client.cancel_with_intent("c", vike_model::OrderCancelIntent::RiskOff);
    client.modify(&order, Some(1.0), None);
    assert_eq!(
        client.modify_trigger(&order, None, None, 1.0),
        Ok(()),
        "the inner client's answer, not the trait's refusal"
    );
    client.submit_batch(std::slice::from_ref(&order));
    client.cancel_batch(&["c".to_string()]);
    client.cancel_batch_with_intent(&["c".to_string()], vike_model::OrderCancelIntent::RiskOff);
    client.confirm("c");
    client.begin_detach();
    client.detach();
    assert!(client.poll_events().is_none());
    client.on_bar(&bar);
    client.on_bar_sides(&bar, None);
    client.on_quote("s", 1.0, 1.0, 1);
    client.fill_at_close(&["c".to_string()], 1);
    client.hold_for_close(true);
    assert_eq!(
        client.amend_semantics(),
        Some(vike_venues::AmendSemantics::CancelReplace),
        "the inner client's answer, not the trait's default"
    );
    assert_eq!(
        client.wire_id_for("c1").as_deref(),
        Some("wire-of-c1"),
        "the inner client's answer, not the default that echoes the order client id"
    );
    client.adopt_restored(&[vike_execution::RestoredOrder::new("c", "w")]);
    client.on_contract_value("c", vike_model::ContractValue::default());

    assert_eq!(
        *calls.lock().expect("the log"),
        DOORS,
        "every door reached the inner client, once, in the order driven"
    );
}

/// The driven list is the trait's method list. The trait lives in `vike-execution` (a measured crate, never
/// edited here), so its source is READ AT RUN TIME from the workspace; a checkout that does not carry it
/// (the published mirror) skips loudly rather than failing to compile.
#[test]
fn the_forwarding_test_drives_exactly_the_methods_of_the_execution_client() {
    // Built in two steps on purpose: a trait edit is a non-comment edit of `vike-execution`, which already runs
    // this crate (a dependent), so the planner needs no `SOURCES_READ_AS_TEXT` row for a comment-only one.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vike-execution")
        .join("src/execution_engine/client.rs");
    let Ok(source) = std::fs::read_to_string(&path) else {
        eprintln!("SKIPPED: {} is not in this checkout", path.display());
        return;
    };
    let body = source
        .split("pub trait ExecutionClient {")
        .nth(1)
        .expect("the trait exists")
        .split("\n}\n")
        .next()
        .expect("the trait ends");
    let mut declared: Vec<&str> = body
        .lines()
        .filter_map(|line| line.strip_prefix("    fn "))
        .filter_map(|rest| rest.split(['(', '<']).next())
        .collect();
    declared.sort_unstable();
    let mut driven = DOORS.to_vec();
    driven.sort_unstable();
    assert_eq!(
        declared, driven,
        "an ExecutionClient method that `ExclusiveClaimClient` is not tested to forward"
    );
}
