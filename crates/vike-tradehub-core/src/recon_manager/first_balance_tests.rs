//! The first-balance cell and the bounded wait on it (decision 0246): what a start waits for, and what each
//! account's cell says while the venue has not answered.

use std::time::Instant;

use super::*;
use crate::testutil::empty_report_fetches;

/// A venue holding `balance` (or failing to say) and nothing else to report.
struct Venue(Result<Option<f64>, String>);
impl ReconClient for Venue {
    empty_report_fetches!();
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.0.clone()
    }
}

fn manager_over(
    legs: Vec<ReconLeg>,
) -> (ReconManager, tokio::sync::mpsc::Receiver<Ingest>, tokio::sync::mpsc::Sender<Ingest>) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Ingest>(16);
    let liveness = legs.iter().map(VenueLiveness::for_leg).collect();
    let mgr = ReconManager {
        clients: legs,
        config: ReconConfig::default(),
        ingest: tx.downgrade(),
        health: None,
        liveness,
    };
    (mgr, rx, tx)
}

/// Every leg the boot builds gets a cell, keyed by the route key the control door asks for (the venue for a sole
/// account, the account's own key for a named one); a leg that already carries one keeps it and is not returned.
#[test]
fn every_leg_gets_a_cell_keyed_by_the_route_key_the_door_asks_for() {
    let kept = FirstBalance::new();
    let mut legs = vec![
        ReconLeg::sole_account_of("bybit", Box::new(Venue(Ok(None)))),
        ReconLeg::account("bybit", "bybit#ALT", Box::new(Venue(Ok(None)))),
        ReconLeg::account("okx", "okx#DEMO", Box::new(Venue(Ok(None)))).awaited(Arc::clone(&kept)),
    ];
    let waits = await_first_balances(&mut legs);
    let keys: Vec<&str> = waits.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(keys, vec!["bybit", "bybit#ALT"], "the third leg already had its cell");
    for (leg, (_, cell)) in legs.iter().zip(&waits) {
        assert!(
            leg.balance_synced.as_ref().is_some_and(|own| Arc::ptr_eq(own, cell)),
            "the leg fills the very cell it returned"
        );
        assert!(!cell.landed(), "nothing has answered");
        assert_eq!(cell.why(), "the first reconcile pass has not reached this account yet");
    }
    assert!(
        legs[2].balance_synced.as_ref().is_some_and(|own| Arc::ptr_eq(own, &kept)),
        "an existing cell is not replaced"
    );
}

/// The wait ends the moment every cell has landed, and not before.
#[test]
fn the_wait_ends_when_the_last_cell_lands() {
    let (a, b) = (FirstBalance::new(), FirstBalance::new());
    let waits = vec![("a".to_string(), Arc::clone(&a)), ("b".to_string(), Arc::clone(&b))];
    a.land();
    let lands = Arc::clone(&b);
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        lands.land();
    });
    let started = Instant::now();
    let unanswered = wait_for_first_balances(&waits, Duration::from_secs(60), || false);
    assert!(unanswered.is_empty(), "everything landed: {unanswered:?}");
    assert!(started.elapsed() >= Duration::from_millis(100), "it did wait for the second cell");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "and ended when it landed, not at the bound"
    );
    late.join().expect("the late lander");
}

/// A cell that never lands costs the wait its bound, and ONLY that cell is reported: the account whose balance
/// landed is not.
#[test]
fn an_account_the_venue_never_answers_is_reported_alone_after_the_bound() {
    let (good, bad) = (FirstBalance::new(), FirstBalance::new());
    good.land();
    let waits = vec![("good".to_string(), good), ("bad".to_string(), bad)];
    let started = Instant::now();
    let unanswered = wait_for_first_balances(&waits, Duration::from_millis(200), || false);
    assert_eq!(unanswered, vec!["bad".to_string()]);
    assert!(started.elapsed() >= Duration::from_millis(200), "the bound was waited out");
}

/// **The bound is for a venue that does not answer, not for one that said no.** Once every cell still down has been
/// visited by a pass that did not answer and none is coming within seconds (a failed fetch, a suppressed feed), the
/// wait ends at once and reports them; a NOT READY leg (which the driver repeats within seconds) and a leg no pass has
/// reached are waited for.
#[test]
fn a_leg_that_said_no_ends_the_wait_but_one_to_be_retried_or_not_reached_does_not() {
    let key = |name: &str, cell: &Arc<FirstBalance>| (name.to_string(), Arc::clone(cell));
    let (good, said_no) = (FirstBalance::new(), FirstBalance::new());
    good.land();
    said_no.fail("balance fetch failed: 401");
    let alone = vec![key("good", &good), key("said_no", &said_no)];
    let started = Instant::now();
    let unanswered = wait_for_first_balances(&alone, Duration::from_secs(3_600), || false);
    assert_eq!(unanswered, vec!["said_no".to_string()]);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a failed read does not sit out the bound"
    );

    let (not_ready, unreached) = (FirstBalance::new(), FirstBalance::new());
    not_ready.fail_retrying("reconcile leg not ready: NOT READY: the boot prefetch is owed");
    for (what, waiting_for) in
        [("a NOT READY leg", key("nr", &not_ready)), ("an unreached leg", key("un", &unreached))]
    {
        let waits = vec![key("said_no", &said_no), waiting_for];
        let started = Instant::now();
        let unanswered = wait_for_first_balances(&waits, Duration::from_millis(300), || false);
        assert_eq!(unanswered.len(), 2, "{what} is still waited for: {unanswered:?}");
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "{what}: the bound was waited out"
        );
    }

    // The retry answers: the cell lands, and a landed cell is never a failure again.
    not_ready.land();
    assert!(not_ready.landed());
    assert!(
        wait_for_first_balances(&[key("nr", &not_ready)], Duration::from_secs(60), || false)
            .is_empty()
    );
}

/// `give_up` (the process stop, a driver that is gone) ends the wait at once, whatever the bound.
#[test]
fn giving_up_ends_the_wait_before_the_bound() {
    let waits = vec![("a".to_string(), FirstBalance::new())];
    let started = Instant::now();
    let unanswered = wait_for_first_balances(&waits, Duration::from_secs(3_600), || true);
    assert_eq!(unanswered, vec!["a".to_string()]);
    assert!(started.elapsed() < Duration::from_secs(30), "it did not sit out the bound");
}

/// **Two accounts are independent through a real pass**: one venue answers, the other's balance read fails. After
/// the pass the first account's cell has landed and the second's has not, and says the venue's words; the wait
/// reports the second alone. A later pass that finally reads the balance lands it.
#[test]
fn a_failed_read_leaves_that_accounts_cell_down_with_the_reason_and_the_other_lands() {
    let legs = vec![
        ReconLeg::sole_account_of("bybit", Box::new(Venue(Ok(Some(4_999.6))))),
        ReconLeg::account("bybit", "bybit#ALT", Box::new(Venue(Err("balance 401".to_string())))),
    ];
    let (mut mgr, mut rx, _tx) = manager_over(legs);
    let waits = await_first_balances(&mut mgr.clients);
    ReconLeg::name_accounts_of_shared_venues(&mut mgr.clients, &["bybit", "bybit"]);

    let stop = AtomicBool::new(false);
    assert!(mgr.run_pass_at(&stop, 1_000).is_empty());
    while rx.try_recv().is_ok() {}

    let unanswered = wait_for_first_balances(&waits, Duration::from_millis(100), || false);
    assert_eq!(unanswered, vec!["bybit#ALT".to_string()], "the failed account alone");
    let (_, alt) = &waits[1];
    assert_eq!(alt.why(), "balance fetch failed: balance 401");
    assert!(waits[0].1.landed(), "the other account's balance landed all the same");

    // The venue recovers: the next pass reads the balance and the account is released.
    mgr.clients[1] = ReconLeg::account("bybit", "bybit#ALT", Box::new(Venue(Ok(Some(120.0)))))
        .awaited(Arc::clone(alt));
    assert!(mgr.run_pass_at(&stop, 2_000).is_empty());
    assert!(alt.landed(), "the pass that reads the balance releases the account");
}
