//! A RUNNING reconcile driver is given a leg (decision 0169, slice S1): the mailbox's two messages
//! applied to a manager, and the pass that follows them.

use super::*;
use crate::empty_report_fetches;

struct Quiet;
impl ReconcileClient for Quiet {
    empty_report_fetches!();
}

fn manager_with_one_unnamed_leg()
-> (ReconcileManager, tokio::sync::mpsc::Receiver<Ingest>, tokio::sync::mpsc::Sender<Ingest>) {
    super::first_balance_tests::manager_over(vec![ReconcileLeg::sole_account_of(
        "bybit",
        Box::new(Quiet),
    )])
}

fn second_account() -> ReconcileLeg {
    ReconcileLeg::account("bybit", "bybit#ALT", Box::new(Quiet))
}

/// A venue's FIRST account is named before the second engine joins the core: a leg carrying no
/// route key means "this venue's sole account", which stops being true.
#[test]
fn naming_a_venues_accounts_stamps_the_unnamed_leg_and_only_it() {
    let (mut manager, _rx, _tx) = manager_with_one_unnamed_leg();
    assert_eq!(
        manager.clients[0].route_key, None,
        "premise: the sole account carries no route key"
    );
    manager.apply_leg_change(ReconcileLegChange::NameAccountsOf("bybit".to_string()));
    assert_eq!(manager.clients[0].route_key.as_deref(), Some("bybit"));
    manager.apply_leg_change(ReconcileLegChange::NameAccountsOf("okx".to_string()));
    assert_eq!(
        manager.clients[0].route_key.as_deref(),
        Some("bybit"),
        "another venue's name is not it"
    );
}

/// A leg added to a running manager joins `clients` AND the liveness rows (they are parallel, by
/// index), and the next pass reads both accounts, each enqueue naming its own engine.
#[test]
fn an_added_leg_is_passed_over_beside_the_one_that_was_running() {
    let (mut manager, mut rx, _tx) = manager_with_one_unnamed_leg();
    manager.apply_leg_change(ReconcileLegChange::NameAccountsOf("bybit".to_string()));
    manager
        .apply_leg_change(ReconcileLegChange::Add { leg: second_account(), venue_is_shared: true });
    assert_eq!(manager.clients.len(), 2);
    assert_eq!(manager.liveness.len(), 2, "one liveness row per leg, in the same order");
    assert_eq!(manager.liveness[1].leg_key, "bybit#ALT");

    let stop = AtomicBool::new(false);
    let not_ready = manager.run_pass_at(&stop, 1_000);
    assert!(not_ready.is_empty());

    let mut routed = Vec::new();
    while let Ok(Ingest::Command(vike_execution::Command::ReconcileReports(r))) = rx.try_recv() {
        routed.push((r.venue.clone(), r.route_key.clone()));
    }
    routed.sort();
    assert_eq!(
        routed,
        vec![
            ("bybit".to_string(), Some("bybit".to_string())),
            ("bybit".to_string(), Some("bybit#ALT".to_string())),
        ],
        "both accounts reconcile, each naming its own engine"
    );
}

/// A new DEFAULT account on a venue that already has a labelled one still names itself: the same
/// predicate as the boot's, because the core refuses an unnamed pass on a two-engine venue.
#[test]
fn a_new_default_leg_on_a_shared_venue_names_itself() {
    let (mut manager, _rx, _tx) = manager_with_one_unnamed_leg();
    manager.clients.clear();
    manager.liveness.clear();
    let leg = ReconcileLeg::sole_account_of("bybit", Box::new(Quiet));
    manager.apply_leg_change(ReconcileLegChange::Add { leg, venue_is_shared: true });
    assert_eq!(manager.clients[0].route_key.as_deref(), Some("bybit"));
}

/// Decision 0169, slice S2: a STOPPED account's leg is skipped by every pass and its liveness row is no
/// longer judged; the leg stays in the vector (indices are what the NOT READY retry names) and the same
/// account starting again gets a NEW leg that is passed over.
#[test]
fn a_retired_leg_is_skipped_and_the_same_account_can_get_a_new_one() {
    let (mut manager, mut rx, _tx) = manager_with_one_unnamed_leg();
    manager.apply_leg_change(ReconcileLegChange::NameAccountsOf("bybit".to_string()));
    manager
        .apply_leg_change(ReconcileLegChange::Add { leg: second_account(), venue_is_shared: true });
    manager.apply_leg_change(ReconcileLegChange::Retire("bybit#ALT".to_string()));
    assert_eq!(manager.clients.len(), 2, "the leg is not removed");
    assert!(
        manager.liveness[1].retired && !manager.liveness[0].retired,
        "only the named account's row"
    );

    let stop = AtomicBool::new(false);
    assert!(manager.run_pass_at(&stop, 1_000).is_empty());
    let mut routed = Vec::new();
    while let Ok(Ingest::Command(vike_execution::Command::ReconcileReports(r))) = rx.try_recv() {
        routed.push(r.route_key.clone());
    }
    assert_eq!(routed, vec![Some("bybit".to_string())], "the stopped account is not reconciled");

    // The account starts again: a new leg under the same key, passed over like any other.
    manager
        .apply_leg_change(ReconcileLegChange::Add { leg: second_account(), venue_is_shared: true });
    assert_eq!(manager.clients.len(), 3);
    assert!(!manager.liveness[2].retired);
    assert!(manager.run_pass_at(&stop, 2_000).is_empty());
    let mut routed = Vec::new();
    while let Ok(Ingest::Command(vike_execution::Command::ReconcileReports(r))) = rx.try_recv() {
        routed.push(r.route_key.clone());
    }
    routed.sort();
    assert_eq!(routed, vec![Some("bybit".to_string()), Some("bybit#ALT".to_string())]);
}

/// Decision 0169: an account started in the RUNNING node has no balance until a pass visits its leg, and
/// its engine judges orders on `seed_cash` until then (the 2026-10-18 okx drill: a 4x order was denied
/// `insufficient-margin` three seconds after the start). Its leg is the LAST of `clients`, so it used to
/// be the last one visited; a leg no pass has visited is now visited FIRST, once.
#[test]
fn a_leg_no_pass_has_visited_is_visited_first_and_only_once() {
    let (mut manager, mut rx, _tx) = manager_with_one_unnamed_leg();
    manager.apply_leg_change(ReconcileLegChange::NameAccountsOf("bybit".to_string()));
    manager
        .apply_leg_change(ReconcileLegChange::Add { leg: second_account(), venue_is_shared: true });
    let key = |text: &str| Some(text.to_string());

    // The boot: every leg is new, so the first pass keeps the order the legs were given in.
    assert_eq!(visit_order(&mut manager, &mut rx, 1_000), vec![key("bybit"), key("bybit#ALT")]);

    // A third account joins the running driver: last in `clients`, first in the pass...
    let third = ReconcileLeg::account("bybit", "bybit#THIRD", Box::new(Quiet));
    manager.apply_leg_change(ReconcileLegChange::Add { leg: third, venue_is_shared: true });
    assert!(manager.liveness[2].fresh && !manager.liveness[0].fresh && !manager.liveness[1].fresh);
    assert_eq!(
        visit_order(&mut manager, &mut rx, 2_000),
        vec![key("bybit#THIRD"), key("bybit"), key("bybit#ALT")]
    );

    // ...and first only once.
    assert!(!manager.liveness[2].fresh);
    assert_eq!(
        visit_order(&mut manager, &mut rx, 3_000),
        vec![key("bybit"), key("bybit#ALT"), key("bybit#THIRD")]
    );
}

/// One full pass at `now_ms`: the route key of each report it enqueued, in the order it visited the legs.
fn visit_order(
    manager: &mut ReconcileManager,
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    now_ms: i64,
) -> Vec<Option<String>> {
    let stop = AtomicBool::new(false);
    assert!(manager.run_pass_at(&stop, now_ms).is_empty());
    let mut order = Vec::new();
    while let Ok(Ingest::Command(vike_execution::Command::ReconcileReports(r))) = rx.try_recv() {
        order.push(r.route_key.clone());
    }
    order
}

// ---- the balance window of a started account (decision 0169) -------------------------------------------

/// A client whose balance read answers as told, whose mass-status fetch fails while `down` (as NOT READY
/// while `not_ready` too), so a test walks one leg through the ways a pass does not happen.
struct Scripted {
    balance: Result<Option<f64>, String>,
    down: Arc<AtomicBool>,
    not_ready: Arc<AtomicBool>,
}

impl Scripted {
    fn up(balance: Result<Option<f64>, String>) -> Self {
        Scripted {
            balance,
            down: Arc::new(AtomicBool::new(false)),
            not_ready: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl ReconcileClient for Scripted {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        if !self.down.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }
        Err(if self.not_ready.load(Ordering::Acquire) {
            "NOT READY: the link is down".to_string()
        } else {
            "mass status 500".to_string()
        })
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    /// The orders read's words, typed as the gatewayhub's client types them: NOT READY is `NotSent`.
    fn fetch_mass_status(
        &self,
        since: i64,
    ) -> Result<
        vike_execution::recon::ReconcileMassStatus,
        vike_execution::recon::ReconcileFetchError,
    > {
        let outcome = if self.not_ready.load(Ordering::Acquire) {
            vike_utility::errors::FailureOutcome::NotSent
        } else {
            vike_utility::errors::FailureOutcome::Refused
        };
        let orders = self
            .fetch_order_status_reports(since)
            .map_err(|text| vike_execution::recon::ReconcileFetchError::new(outcome, text))?;
        Ok(vike_execution::recon::ReconcileMassStatus {
            orders,
            positions: Vec::new(),
            fills: Vec::new(),
        })
    }
    fn fetch_balance(&self) -> Result<Option<f64>, vike_execution::recon::ReconcileFetchError> {
        self.balance.clone().map_err(vike_execution::recon::ReconcileFetchError::unclassified)
    }
}

/// The reports a pass left on the lane: `(route key, the balance it carried)`.
fn reports_on(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<(Option<String>, Option<f64>)> {
    let mut out = Vec::new();
    while let Ok(Ingest::Command(vike_execution::Command::ReconcileReports(r))) = rx.try_recv() {
        out.push((r.route_key.clone(), r.balance));
    }
    out
}

/// **The flag an account started in the running node waits on rises with the venue's ANSWER, and with
/// nothing else.** Until it rises the control door holds the account's opening orders, because its engine
/// judges on `seed_cash`: the figure lands the venue's balance, a venue that reports none (ctrader) is on
/// the seed by design so nothing is left to wait for, and a balance read that FAILED leaves the engine on
/// its seed, so the door keeps holding. The pass itself is enqueued in every case.
#[test]
fn a_started_legs_flag_rises_with_an_answered_balance_and_not_with_a_failed_one() {
    let cases = [
        ("the venue's figure", Ok(Some(4_999.6)), Some(4_999.6), true),
        ("a venue that reports no balance", Ok(None), None, true),
        ("a balance read that failed", Err("balance 401".to_string()), None, false),
    ];
    for (what, balance, carried, raised) in cases {
        let (mut manager, mut rx, _tx) = manager_with_one_unnamed_leg();
        let synced = ReconcileFirstBalance::new();
        let leg = ReconcileLeg::account("bybit", "bybit#ALT", Box::new(Scripted::up(balance)))
            .awaited(Arc::clone(&synced));
        manager.apply_leg_change(ReconcileLegChange::NameAccountsOf("bybit".to_string()));
        manager.apply_leg_change(ReconcileLegChange::Add { leg, venue_is_shared: true });
        assert!(!synced.landed(), "{what}: no pass has run");

        let stop = AtomicBool::new(false);
        assert!(manager.run_pass_at(&stop, 1_000).is_empty());
        let alt =
            reports_on(&mut rx).into_iter().find(|(key, _)| key.as_deref() == Some("bybit#ALT"));
        assert_eq!(
            alt,
            Some((Some("bybit#ALT".to_string()), carried)),
            "{what}: the pass is enqueued"
        );
        assert_eq!(synced.landed(), raised, "{what}");
        // A balance read that failed says why, in the cell the door reads; one that landed has nothing to say.
        assert_eq!(synced.why().contains("balance fetch failed: balance 401"), !raised, "{what}");
    }
}

/// A pass that did NOT happen (the mass-status fetch failed, the gatewayhub said NOT READY, the venue's
/// feed is suppressed by the health gate) puts nothing on the lane, so the engine is still on its seed and
/// the flag stays down; the first pass that does happen raises it.
#[test]
fn a_pass_that_did_not_happen_leaves_the_flag_down_until_one_does() {
    let (mut manager, mut rx, _tx) = manager_with_one_unnamed_leg();
    let synced = ReconcileFirstBalance::new();
    let client = Scripted::up(Ok(Some(4_999.6)));
    let (down, not_ready) = (Arc::clone(&client.down), Arc::clone(&client.not_ready));
    manager.clients[0] =
        ReconcileLeg::sole_account_of("bybit", Box::new(client)).awaited(Arc::clone(&synced));
    let degraded = Arc::new(AtomicBool::new(false));
    let feed_gap = Arc::clone(&degraded);
    let health: ReconcileHealthProbe = Arc::new(move |_venue: &str| {
        if feed_gap.load(Ordering::Acquire) {
            ReconcileHealth::Degraded
        } else {
            ReconcileHealth::Healthy
        }
    });
    manager.health = Some(health);
    let stop = AtomicBool::new(false);

    assert_eq!(
        synced.why(),
        "the first reconcile pass has not reached this account yet",
        "before any pass the cell says it has not been reached"
    );
    down.store(true, Ordering::Release);
    assert!(manager.run_pass_at(&stop, 1_000).is_empty(), "a failed fetch is not NOT READY");
    assert_eq!(synced.why(), "mass-status report fetch failed: mass status 500");
    not_ready.store(true, Ordering::Release);
    assert_eq!(manager.run_pass_at(&stop, 2_000), vec![0], "NOT READY names the leg for the retry");
    assert_eq!(synced.why(), "reconcile leg not ready: NOT READY: the link is down");
    down.store(false, Ordering::Release);
    degraded.store(true, Ordering::Release);
    assert!(manager.run_pass_at(&stop, 3_000).is_empty());
    assert_eq!(synced.why(), "reconcile leg suppressed: venue health probe reports Degraded");
    assert!(reports_on(&mut rx).is_empty(), "none of the three put a pass on the lane");
    assert!(!synced.landed(), "the engine is still on its seed");

    degraded.store(false, Ordering::Release);
    assert!(manager.run_pass_at(&stop, 4_000).is_empty());
    assert_eq!(reports_on(&mut rx), vec![(None, Some(4_999.6))], "the pass that happens");
    assert!(synced.landed(), "and its balance is the answer the flag waited for");
}

/// A leg built by a constructor carries no cell: only the boot's root (`await_first_balances`) and a hot start
/// ask for one.
#[test]
fn a_leg_nobody_waits_on_carries_no_flag() {
    assert!(ReconcileLeg::sole_account_of("bybit", Box::new(Quiet)).balance_synced.is_none());
    assert!(second_account().balance_synced.is_none());
}

/// A retired row never escalates, however long ago its last pass was.
#[test]
fn a_retired_row_is_never_escalated_as_stale() {
    let (mut manager, _rx, _tx) = manager_with_one_unnamed_leg();
    manager.liveness[0].last_pass_ms = Some(0);
    manager.apply_leg_change(ReconcileLegChange::Retire("bybit".to_string()));
    let stop = AtomicBool::new(false);
    manager.run_pass_at(&stop, 10_000_000_000); // ends in the staleness escalation
    assert_eq!(
        manager.liveness[0].last_escalation_ms, 0,
        "a stopped account's silence is not an unchecked venue"
    );
}

/// A poke's route key finds its account's leg: a labelled account by its key, a default account by its bare venue (the
/// key a lease of a default account carries), and a key no leg carries finds none.
#[test]
fn a_poke_finds_the_leg_of_the_account_it_names() {
    let (mut manager, _rx, _tx) = manager_with_one_unnamed_leg();
    manager
        .apply_leg_change(ReconcileLegChange::Add { leg: second_account(), venue_is_shared: true });
    let keys = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect::<Vec<_>>();
    assert_eq!(manager.legs_of(&keys(&["bybit#ALT"])), [1]);
    assert_eq!(manager.legs_of(&keys(&["bybit", "bybit#ALT", "bybit"])), [0, 1]);
    assert!(manager.legs_of(&keys(&["okx", "bybit#NONE"])).is_empty());
}
