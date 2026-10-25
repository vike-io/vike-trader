//! The shared confirm form's own contract: the verdict table, the dispatch ledger with its lookup
//! window, the admission gate and the closure that ties them. Each venue's lookup is tested in its
//! own crate (`crates/bridges/<venue>/tests/offline/confirm_by_coid.rs`).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use super::*;

struct Mock<const WINDOW_MS: u64> {
    answer: Result<Option<String>, i64>,
}

impl<const WINDOW_MS: u64> VenueRest for Mock<WINDOW_MS> {
    fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
        Vec::new()
    }
    fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
        Ok(())
    }
}

impl<const WINDOW_MS: u64> ConfirmByCoid for Mock<WINDOW_MS> {
    const LOOKUP_WINDOW: Option<Duration> =
        if WINDOW_MS == 0 { None } else { Some(Duration::from_millis(WINDOW_MS)) };

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let query = self.answer.clone().map_err(|code| VenueApiError { code, msg: "stub".into() });
        resolve_confirm_by_id(coid, ts, query, absent_rejects)
    }
}

fn request(coid: &str) -> OrderRequest {
    OrderRequest { client_order_id: coid.to_string(), ..Default::default() }
}

fn failed(code: i64) -> Result<Option<String>, VenueApiError> {
    Err(VenueApiError { code, msg: "x".into() })
}

fn is_accepted_with(events: &[Event], id: &str) -> bool {
    matches!(events, [Event::OrderAccepted(a)] if a.venue_order_id.as_deref() == Some(id))
}

#[test]
fn found_is_the_callers_mapping_whatever_the_dispatch_state() {
    for dispatched in [true, false] {
        let events = resolve_confirm("c", 5, Ok(Some("v1".to_string())), dispatched, |id| {
            vec![crate::rest::resolve_ambiguous_submit("c", 5, Ok(Some(id)))]
        });
        assert!(is_accepted_with(&events, "v1"), "dispatched={dispatched}: {events:?}");
    }
    assert!(is_accepted_with(
        &resolve_confirm_by_id("c", 5, Ok(Some("v2".to_string())), false),
        "v2"
    ));
}

#[test]
fn absent_rejects_only_when_the_dispatch_is_proven() {
    let rejected = resolve_confirm_by_id("c", 5, Ok(None), true);
    assert!(
        matches!(&rejected[..], [Event::OrderRejected(r)] if r.reason.contains("venue confirms order absent")),
        "{rejected:?}"
    );
    assert!(resolve_confirm_by_id("c", 5, Ok(None), false).is_empty(), "not dispatched: silence");
}

/// A failed query says nothing: no id-less accept (the old weak form's answer), no terminal.
#[test]
fn a_failed_query_is_silence_either_way() {
    for absent_rejects in [true, false] {
        for code in [crate::transport::E_TIMEOUT_AMBIGUOUS, -2015, 401, 418, 429, 503] {
            assert!(
                resolve_confirm_by_id("c", 5, failed(code), absent_rejects).is_empty(),
                "code {code}, absent_rejects {absent_rejects}"
            );
        }
    }
}

/// A submit that blocks until the test lets it return, announcing when it is inside.
struct Blocking {
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl VenueRest for Blocking {
    fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
        lock(&self.entered).send(()).expect("the test is listening");
        lock(&self.release).recv().expect("the test releases the submit");
        Vec::new()
    }
    fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
        Ok(())
    }
}

/// Stamping BEFORE the call (or never) would pass a mock that returns at once; this one is observed
/// while its submit is still in flight: an in-flight POST is not "dispatched", only a returned one is.
#[test]
fn dispatched_marks_a_coid_only_after_its_submit_returned() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let ledger = Arc::new(DispatchLedger::default());
    let rest = Dispatched {
        rest: Blocking { entered: Mutex::new(entered_tx), release: Mutex::new(release_rx) },
        ledger: ledger.clone(),
    };
    std::thread::scope(|s| {
        let submit = s.spawn(|| rest.submit_order(&request("a")));
        entered_rx.recv().expect("the submit started");
        assert!(!ledger.provably_dispatched("a", None), "in flight: not dispatched yet");
        release_tx.send(()).unwrap();
        submit.join().unwrap();
    });
    assert!(ledger.provably_dispatched("a", None), "returned: dispatched");
}

/// The batch door marks every member (a defaulted `submit_batch` would have skipped it).
#[test]
fn dispatched_marks_every_member_of_a_batch() {
    let ledger = Arc::new(DispatchLedger::default());
    let rest = Dispatched { rest: Mock::<0> { answer: Ok(None) }, ledger: ledger.clone() };
    rest.submit_batch(&[request("b"), request("c")]);
    assert!(ledger.provably_dispatched("b", None) && ledger.provably_dispatched("c", None));
    assert!(!ledger.provably_dispatched("d", None));
}

#[test]
fn the_ledger_proves_a_dispatch_only_inside_the_window() {
    let ledger = DispatchLedger::default();
    let ago =
        |secs| Instant::now().checked_sub(Duration::from_secs(secs)).expect("a long-lived clock");
    ledger.mark_at("old", ago(100));
    ledger.mark("new");
    let window = Some(Duration::from_secs(60));
    assert!(ledger.provably_dispatched("new", window));
    assert!(!ledger.provably_dispatched("old", window), "past the window: absent proves nothing");
    assert!(ledger.provably_dispatched("old", None), "no window stated: any age");
    assert!(!ledger.provably_dispatched("never", None));
}

#[test]
fn the_gate_queues_the_overflow_and_backs_a_coid_off() {
    let gate = ConfirmGate::default();
    let held: Vec<_> = (0..MAX_CONFIRMS_IN_FLIGHT)
        .map(|i| gate.enter(&format!("c{i}")).expect("a free slot"))
        .collect();
    let (admitted_tx, admitted_rx) = mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(|| {
            let permit = gate.enter("extra").expect("queued, not dropped");
            admitted_tx.send(()).unwrap();
            drop(permit);
        });
        assert!(
            admitted_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the fifth confirm waits while every slot is taken"
        );
        drop(held);
        admitted_rx.recv_timeout(Duration::from_secs(5)).expect("a freed slot admits the waiter");
    });
    assert_eq!(*lock(&gate.busy), 0, "every permit returned its slot");
    let first = gate.enter("again").expect("slots are free again");
    drop(first);
    assert!(gate.enter("again").is_none(), "the same coid inside the backoff is skipped");
    assert!(gate.enter("other").is_some(), "a different coid is not");
}

/// The closure reads the ledger with THE VENUE'S window: the same absent answer rejects a recent
/// dispatch, and is silence for an undispatched coid and for one past the window.
#[test]
fn the_closure_rejects_absent_only_for_a_dispatch_inside_the_venues_window() {
    let ledger = Arc::new(DispatchLedger::default());
    let confirm = confirm_closure(Mock::<60_000> { answer: Ok(None) }, ledger.clone(), |_| true);
    assert!(confirm("queued").is_empty(), "still queued: absent proves nothing");
    ledger.mark_at(
        "aged",
        Instant::now().checked_sub(Duration::from_secs(120)).expect("a long-lived clock"),
    );
    assert!(confirm("aged").is_empty(), "past the window: the venue may have forgotten it");
    ledger.mark("fresh");
    assert!(matches!(confirm("fresh")[..], [Event::OrderRejected(_)]), "inside the window");
    assert!(confirm("fresh").is_empty(), "the same coid inside the backoff is skipped");
}

#[test]
fn a_failed_query_through_the_closure_is_silence_even_after_dispatch() {
    let ledger = Arc::new(DispatchLedger::default());
    ledger.mark("c");
    let confirm = confirm_closure(Mock::<0> { answer: Err(503) }, ledger, |_| true);
    assert!(confirm("c").is_empty());
}

/// ONE client serves every confirm, and `prepare` (a spot server-time read) runs until it succeeds,
/// then never again.
#[test]
fn the_closure_prepares_its_client_until_it_succeeds_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let confirm = confirm_closure(
        Mock::<0> { answer: Ok(Some("v".to_string())) },
        Arc::new(DispatchLedger::default()),
        move |_| seen.fetch_add(1, Ordering::SeqCst) >= 1, // fails the first time, succeeds the second
    );
    for coid in ["a", "b", "c", "d"] {
        assert_eq!(confirm(coid).len(), 1, "{coid}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// A venue lookup that takes 150 ms and counts how many run at once.
struct Slow {
    inside: AtomicUsize,
    most_inside: AtomicUsize,
}

impl VenueRest for Slow {
    fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
        Vec::new()
    }
    fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
        Ok(())
    }
}

impl ConfirmByCoid for Slow {
    const LOOKUP_WINDOW: Option<Duration> = None;

    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_inside.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        self.inside.fetch_sub(1, Ordering::SeqCst);
        resolve_confirm_by_id(coid, ts, Ok(Some("v".to_string())), absent_rejects)
    }
}

/// Run `confirm` for each coid on its own thread (the `ExecActor` spawns one worker per confirm) and
/// collect every answer.
fn run_concurrently(confirm: &ConfirmFn, coids: &[String]) -> Vec<Vec<Event>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = coids.iter().map(|c| s.spawn(|| confirm(c))).collect();
        handles.into_iter().map(|h| h.join().expect("a confirm worker")).collect()
    })
}

/// The stuck-order ladder asks ONCE per coid and rejects at its backstop whatever is still unanswered,
/// so a confirm that is dropped is a live order rejected. More confirms than the bound must therefore
/// WAIT for a slot, never be skipped: every one of them answers.
#[test]
fn a_confirm_over_the_bound_waits_for_a_slot_instead_of_being_dropped() {
    let slow = Slow { inside: AtomicUsize::new(0), most_inside: AtomicUsize::new(0) };
    let confirm = confirm_closure(slow, Arc::new(DispatchLedger::default()), |_| true);
    let coids: Vec<String> = (0..MAX_CONFIRMS_IN_FLIGHT + 3).map(|i| format!("w{i}")).collect();
    let answers = run_concurrently(&confirm, &coids);
    assert!(answers.iter().all(|a| a.len() == 1), "a queued confirm still answers: {answers:?}");
}

/// Slow confirms (a bybit lookup is up to two 5 s calls) run side by side up to the bound; they are not
/// serialised behind one lock, or the third stuck order's answer lands after the ladder's backstop.
#[test]
fn confirms_run_in_parallel_up_to_the_bound() {
    let slow = Slow { inside: AtomicUsize::new(0), most_inside: AtomicUsize::new(0) };
    let confirm = confirm_closure(slow, Arc::new(DispatchLedger::default()), |_| true);
    let coids: Vec<String> = (0..MAX_CONFIRMS_IN_FLIGHT).map(|i| format!("p{i}")).collect();
    let started = Instant::now();
    run_concurrently(&confirm, &coids);
    // 4 lookups of 150 ms: serialised they take >= 600 ms, side by side about 150 ms.
    assert!(started.elapsed() < Duration::from_millis(450), "{:?}", started.elapsed());
}
