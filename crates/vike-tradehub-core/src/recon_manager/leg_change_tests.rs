//! A RUNNING reconcile driver is given a leg (decision 0169, slice S1): the mailbox's two messages
//! applied to a manager, and the pass that follows them.

use super::*;
use crate::testutil::empty_report_fetches;

struct Quiet;
impl ReconClient for Quiet {
    empty_report_fetches!();
}

fn manager_with_one_unnamed_leg()
-> (ReconManager, tokio::sync::mpsc::Receiver<Ingest>, tokio::sync::mpsc::Sender<Ingest>) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Ingest>(16);
    let clients = vec![ReconLeg::sole_account_of("bybit", Box::new(Quiet))];
    let liveness = clients.iter().map(VenueLiveness::for_leg).collect();
    let mgr = ReconManager {
        clients,
        config: ReconConfig::default(),
        ingest: tx.downgrade(),
        health: None,
        liveness,
    };
    // The manager holds only a WEAK sender: a STRONG one is returned so it stays upgradable, and the
    // receiver so a send is not refused.
    (mgr, rx, tx)
}

fn second_account() -> ReconLeg {
    ReconLeg::account("bybit", "bybit#ALT", Box::new(Quiet))
}

/// A venue's FIRST account is named before the second engine joins the core: a leg carrying no
/// route key means "this venue's sole account", which stops being true.
#[test]
fn naming_a_venues_accounts_stamps_the_unnamed_leg_and_only_it() {
    let (mut mgr, _rx, _tx) = manager_with_one_unnamed_leg();
    assert_eq!(mgr.clients[0].route_key, None, "premise: the sole account carries no route key");
    mgr.apply_leg_change(LegChange::NameAccountsOf("bybit".to_string()));
    assert_eq!(mgr.clients[0].route_key.as_deref(), Some("bybit"));
    mgr.apply_leg_change(LegChange::NameAccountsOf("okx".to_string()));
    assert_eq!(
        mgr.clients[0].route_key.as_deref(),
        Some("bybit"),
        "another venue's name is not it"
    );
}

/// A leg added to a running manager joins `clients` AND the liveness rows (they are parallel, by
/// index), and the next pass reads both accounts, each enqueue naming its own engine.
#[test]
fn an_added_leg_is_passed_over_beside_the_one_that_was_running() {
    let (mut mgr, mut rx, _tx) = manager_with_one_unnamed_leg();
    mgr.apply_leg_change(LegChange::NameAccountsOf("bybit".to_string()));
    mgr.apply_leg_change(LegChange::Add { leg: second_account(), venue_is_shared: true });
    assert_eq!(mgr.clients.len(), 2);
    assert_eq!(mgr.liveness.len(), 2, "one liveness row per leg, in the same order");
    assert_eq!(mgr.liveness[1].leg_key, "bybit#ALT");

    let stop = AtomicBool::new(false);
    let not_ready = mgr.run_pass_at(&stop, 1_000);
    assert!(not_ready.is_empty());

    let mut routed = Vec::new();
    while let Ok(Ingest::Command(vike_exec::Command::ReconcileReports(r))) = rx.try_recv() {
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
    let (mut mgr, _rx, _tx) = manager_with_one_unnamed_leg();
    mgr.clients.clear();
    mgr.liveness.clear();
    let leg = ReconLeg::sole_account_of("bybit", Box::new(Quiet));
    mgr.apply_leg_change(LegChange::Add { leg, venue_is_shared: true });
    assert_eq!(mgr.clients[0].route_key.as_deref(), Some("bybit"));
}

/// Decision 0169, slice S2: a STOPPED account's leg is skipped by every pass and its liveness row is no
/// longer judged; the leg stays in the vector (indices are what the NOT READY retry names) and the same
/// account starting again gets a NEW leg that is passed over.
#[test]
fn a_retired_leg_is_skipped_and_the_same_account_can_get_a_new_one() {
    let (mut mgr, mut rx, _tx) = manager_with_one_unnamed_leg();
    mgr.apply_leg_change(LegChange::NameAccountsOf("bybit".to_string()));
    mgr.apply_leg_change(LegChange::Add { leg: second_account(), venue_is_shared: true });
    mgr.apply_leg_change(LegChange::Retire("bybit#ALT".to_string()));
    assert_eq!(mgr.clients.len(), 2, "the leg is not removed");
    assert!(mgr.liveness[1].retired && !mgr.liveness[0].retired, "only the named account's row");

    let stop = AtomicBool::new(false);
    assert!(mgr.run_pass_at(&stop, 1_000).is_empty());
    let mut routed = Vec::new();
    while let Ok(Ingest::Command(vike_exec::Command::ReconcileReports(r))) = rx.try_recv() {
        routed.push(r.route_key.clone());
    }
    assert_eq!(routed, vec![Some("bybit".to_string())], "the stopped account is not reconciled");

    // The account starts again: a new leg under the same key, passed over like any other.
    mgr.apply_leg_change(LegChange::Add { leg: second_account(), venue_is_shared: true });
    assert_eq!(mgr.clients.len(), 3);
    assert!(!mgr.liveness[2].retired);
    assert!(mgr.run_pass_at(&stop, 2_000).is_empty());
    let mut routed = Vec::new();
    while let Ok(Ingest::Command(vike_exec::Command::ReconcileReports(r))) = rx.try_recv() {
        routed.push(r.route_key.clone());
    }
    routed.sort();
    assert_eq!(routed, vec![Some("bybit".to_string()), Some("bybit#ALT".to_string())]);
}

/// A retired row never escalates, however long ago its last pass was.
#[test]
fn a_retired_row_is_never_escalated_as_stale() {
    let (mut mgr, _rx, _tx) = manager_with_one_unnamed_leg();
    mgr.liveness[0].last_pass_ms = Some(0);
    mgr.apply_leg_change(LegChange::Retire("bybit".to_string()));
    let stop = AtomicBool::new(false);
    mgr.run_pass_at(&stop, 10_000_000_000); // ends in the staleness escalation
    assert_eq!(
        mgr.liveness[0].last_escalation_ms, 0,
        "a stopped account's silence is not an unchecked venue"
    );
}
