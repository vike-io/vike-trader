//! The per-mount budget latch is fire-once for the JUDGEMENT only: a latched mount that still holds a position
//! with nothing closing it is offered its flatten again, and the latch itself rides the journal's `Snap` and is
//! put back by a restart (`docs/ops/known-limitations-2026-10.md`: it used to be neither re-offered nor durable).

use super::mount_budget::{
    budget_journal_dir, budget_mount, clock_budget_core, coid_fill, one_budget, reduce_only_sent,
};
use super::*;

/// The live reduce-only order a flatten left in engine 0's registry.
fn live_flatten(core: &CoreThread<RecordingClient>) -> String {
    core.engine
        .registry
        .iter()
        .find(|(_, mo)| mo.request.reduce_only && mo.status.is_live())
        .map(|(coid, _)| coid.clone())
        .expect("a live reduce-only flatten is out")
}

/// The venue refuses `coid` (`RecordingClient` emits no accept, so a rejection is the one terminal the FSM
/// takes straight from `Initialized`).
fn venue_rejects(core: &mut CoreThread<RecordingClient>, coid: &str) {
    core.bus.publish(
        Event::OrderRejected(vike_model::events::OrderRejected {
            cause: None,
            client_order_id: coid.to_string(),
            reason: "venue said no".to_string().into(),
            ts: 0,
        }),
        &mut core.engine,
    );
}

fn offered_again(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|e| e.contains("its flatten is offered again")).count()
}

/// Latch the budgeted mount (a long 1 at 100 marked at 40, a loss of 60 against a budget of 50) on the
/// clock's first sweep. Its flatten goes out whole and stays `Initialized` (the client folds nothing).
fn latched_core(flatten: bool) -> CoreThread<RecordingClient> {
    let mut core = clock_budget_core(flatten, None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "precondition, latched: {:?}", core.recent);
    core
}

/// ⚠ **A latched mount whose flatten did not take is offered it again** (it was spent for ever: the gate
/// denied it, the venue rejected it, the panic guard caught the send, and the position stayed open with
/// nothing left to close it). A flatten that is still OUT is cover and is never stacked on; once it ends
/// unfilled the next sweep sends the missing quantity again, SAYS so once, and stops when the mount is flat.
#[test]
fn a_latched_mount_whose_flatten_was_rejected_is_offered_it_again_until_it_is_flat() {
    let mut core = latched_core(true);
    assert_eq!(reduce_only_sent(&core), (1, 1.0), "the latch's own flatten");

    core.drive_due_timers(60_100);
    assert_eq!(reduce_only_sent(&core).0, 1, "a flatten still out is cover: {:?}", core.recent);
    assert_eq!(offered_again(&core), 0);

    let first = live_flatten(&core);
    venue_rejects(&mut core, &first);
    core.drive_due_timers(90_100);
    assert_eq!(reduce_only_sent(&core), (2, 2.0), "offered again: {:?}", core.recent);
    assert_eq!(offered_again(&core), 1);

    let second = live_flatten(&core);
    venue_rejects(&mut core, &second);
    core.drive_due_timers(120_100);
    assert_eq!(reduce_only_sent(&core).0, 3, "and again at the next sweep: {:?}", core.recent);
    assert_eq!(offered_again(&core), 1, "said once per episode, not at every sweep");

    // the third one fills: the mount is flat, nothing more is sent and the episode ends
    let third = live_flatten(&core);
    core.bus.publish(Event::Fill(coid_fill(&third, "BTCUSDT", -1, 1.0, 40.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(core.mount_attr[0].size, 0.0, "the mount ledger is flat");
    core.drive_due_timers(150_100);
    assert_eq!(reduce_only_sent(&core).0, 3, "flat, nothing to offer: {:?}", core.recent);
    assert!(core.mount_reoffer_said.is_empty(), "the episode is over");
}

/// A budget without `flatten_on_breach` asks for NO flatten at the latch, so none is ever offered.
#[test]
fn a_latched_mount_whose_budget_does_not_flatten_is_never_offered_one() {
    let mut core = latched_core(false);
    for now in [60_100, 90_100, 120_100] {
        core.drive_due_timers(now);
    }
    assert_eq!(reduce_only_sent(&core), (0, 0.0), "{:?}", core.recent);
    assert_eq!(offered_again(&core), 0);
}

/// A HALTED engine refuses even a reduce-only close and its cure is the operator's `resume`, so the re-offer
/// waits (silently: a denial every 30 s would only fill the 64-line ring) and goes out once it is resumed.
#[test]
fn a_latched_mount_on_a_halted_engine_is_offered_its_flatten_once_it_resumes() {
    let mut core = latched_core(true);
    let first = live_flatten(&core);
    venue_rejects(&mut core, &first);
    core.engine.trading_state = TradingState::Halted;
    core.drive_due_timers(60_100);
    assert_eq!(reduce_only_sent(&core).0, 1, "halted: nothing offered: {:?}", core.recent);
    core.engine.trading_state = TradingState::Active;
    core.drive_due_timers(90_100);
    assert_eq!(reduce_only_sent(&core).0, 2, "resumed: offered again: {:?}", core.recent);
}

/// ⚠ **The latch survives a restart.** It rides the journal's `Snap` (`SnapMountAttr::latched`) and the boot
/// restore puts it back on a mount that still has an ACTIVE budget; a mount whose budget was removed from the
/// profile since comes back free (a latch with no budget behind it would discard its intents for good).
#[test]
fn the_latch_rides_the_snap_and_a_restart_puts_it_back_for_a_mount_with_a_budget() {
    let dir = budget_journal_dir();
    let mut core = clock_budget_core(false, Some(JournalConfig::at(dir.to_path_buf())));
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "{:?}", core.recent);
    core.write_snap(31_000);
    core.journal.as_mut().expect("journaling is on").flush().expect("flush");
    drop(core);

    let records = vike_journal::CommandJournal::read_all(&dir).expect("the journal reads back");
    let rows = records
        .iter()
        .rev()
        .find_map(|r| match r {
            vike_journal::JournalRecord::Snap { mount_attr, .. } => Some(mount_attr.clone()),
            _ => None,
        })
        .expect("a Snap was written");
    assert_eq!(rows.len(), 1, "one live mount: {rows:?}");
    assert!(rows[0].latched, "the Snap carries the spent latch: {rows:?}");
    assert_eq!(rows[0].size, 1.0, "…beside the ledger it always carried");

    let restart = |budget: bool| {
        core_with(CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(budget_mount("BTCUSDT")),
            mount_budgets: if budget {
                one_budget(
                    "BTCUSDT",
                    MountBudget {
                        max_loss: Some(50.0),
                        max_notional: None,
                        flatten_on_breach: false,
                    },
                )
            } else {
                std::collections::HashMap::new()
            },
            mount_attr: rows.clone(),
            ..CoreConfig::default()
        })
    };
    let back = restart(true);
    assert!(back.mount_latched[0], "the restart puts the latch back");
    assert_eq!(back.mount_attr[0].size, 1.0, "…with the ledger");
    assert!(back.mount_views()[0].latched, "…and the published row says so");
    assert!(!restart(false).mount_latched[0], "no budget any more: the mount is free");
    let _ = std::fs::remove_dir_all(&dir);
}
