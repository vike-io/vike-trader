//! The node-wide DEFAULT mount budget (`CoreConfig::mount_budget_default`, decision 0249): the source the
//! keyed `mount_budgets` map never had. With none (the default) a mount is watched by nothing, exactly as
//! before; with one, every mount that names no key of its own latches at the stated loss on the 30 s risk
//! clock, the operator's un-latch clears and re-arms it, and a restart restores it. A keyed entry still wins.

use super::mount_budget::{
    budget_journal_dir, budget_mount, coid_fill, one_budget, reduce_only_sent,
};
use super::*;
use crate::runtime::test_support::coid_of;

/// The budget the live mount derives from `policy.max_mount_loss = 50` (flattening by default).
fn default_budget(flatten: bool) -> MountBudget {
    MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: flatten }
}

/// A node whose risk clock is armed (`venues` empty: every venue is mounted), one mount with NO key of its
/// own in `mount_budgets`, and the node-wide `default`. The mount rests an order and takes a long 1 at 100.
fn default_core(
    default: Option<MountBudget>,
    keyed: std::collections::HashMap<String, MountBudget>,
    journal: Option<JournalConfig>,
) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: keyed,
        mount_budget_default: default,
        price_only: Some(PriceOnlyConfig {
            venues: std::collections::BTreeSet::new(),
            sweep_interval: std::time::Duration::from_secs(30),
            max_price_age: std::time::Duration::from_secs(60),
            price_source: std::collections::BTreeMap::new(),
        }),
        journal,
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid, "BTCUSDT", 1, 1.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    core.arm_boundary_timers(0);
    core
}

fn mount_id() -> String {
    crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m")
}

fn unlatch(core: &mut CoreThread<RecordingClient>, id: &str) {
    core.dispatch(Ingest::Command(Command::UnlatchMount { mount_id: id.to_string() }));
}

fn latch_notes(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|e| e.starts_with("MOUNT BUDGET LATCH")).count()
}

/// ⚠ **No default budget is the path every node ran before the key: nothing is watched.** The same loss that
/// latches the budgeted mount below (a long 1 at 100 marked at 40) is not judged, the sweep is never armed
/// and no order is cancelled. The control half of every test in this file.
#[test]
fn no_default_budget_watches_nothing_on_the_clock() {
    let mut core = default_core(None, std::collections::HashMap::new(), None);
    assert!(!core.any_mount_budget, "no budget anywhere must leave the sweep un-armed");
    assert_eq!(core.mount_budget[0], None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    core.drive_due_timers(61_000);
    assert!(!core.mount_latched[0], "a loss of 60 is nobody's business without a budget");
    assert!(core.engine.client.cancels.is_empty(), "…and nothing is cancelled");
    assert_eq!(latch_notes(&core), 0, "{:?}", core.recent);
    assert_eq!(core.engine.trading_state, TradingState::Active);
}

/// An INACTIVE default (no positive arm) is inert exactly like an inactive keyed entry: it never arms the
/// sweep. (The live mount never builds one — the file edge refuses a non-positive number — but the core must
/// not depend on that.)
#[test]
fn an_inactive_default_budget_arms_nothing() {
    let inert = MountBudget { max_loss: Some(0.0), max_notional: None, flatten_on_breach: true };
    let mut core = default_core(Some(inert), std::collections::HashMap::new(), None);
    assert!(!core.any_mount_budget);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(!core.mount_latched[0]);
}

/// ⚠ **A default budget latches a mount that has no key of its own, at the stated loss, on the 30 s clock —
/// between bars, once.** A long 1 at 100: marked at 60 it is 40 down (inside a budget of 50) and is not
/// latched; marked at 40 it is 60 down and the first sweep after that latches the mount, cancels its own
/// order, and (the budget flattens) sends ONE reduce-only market close of its position. The account's state
/// is not touched, and the next sweeps add nothing.
#[test]
fn a_default_budget_latches_a_keyless_mount_at_the_stated_loss_on_the_clock() {
    let mut core = default_core(Some(default_budget(true)), std::collections::HashMap::new(), None);
    assert!(core.any_mount_budget, "the default arms the sweep");
    assert_eq!(core.mount_budget[0], Some(default_budget(true)), "the mount took the default");
    let coid = coid_of(&core, 0);

    core.engine.price_board.set_mark("sim", "BTCUSDT", 60.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(!core.mount_latched[0], "a loss of 40 is inside the budget of 50: {:?}", core.recent);

    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 55_000);
    core.drive_due_timers(60_100);
    assert!(core.mount_latched[0], "a loss of 60 is over it: {:?}", core.recent);
    assert!(core.engine.client.cancels.contains(&coid), "its own resting order is cancelled");
    assert_eq!(reduce_only_sent(&core), (1, 1.0), "the budget flattens: one close of its 1");
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "a mount latch never touches the account"
    );

    core.drive_due_timers(91_000);
    assert_eq!(latch_notes(&core), 1, "fire-once: {:?}", core.recent);
    assert_eq!(reduce_only_sent(&core).0, 1, "…and no second close beside the first");
}

/// The mode is the default's own: a cancel-only default cancels the mount's order and sends no close.
#[test]
fn a_cancel_only_default_cancels_and_leaves_the_position() {
    let mut core =
        default_core(Some(default_budget(false)), std::collections::HashMap::new(), None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "{:?}", core.recent);
    assert_eq!(reduce_only_sent(&core), (0, 0.0), "cancel-only sends no close");
    assert_eq!(core.mount_attr[0].size, 1.0, "the position stays for the operator");
}

/// A mount's OWN key wins over the default: a mount given a wider budget is not also held to the node's.
#[test]
fn a_keyed_budget_wins_over_the_default() {
    let keyed = one_budget(
        "BTCUSDT",
        MountBudget { max_loss: Some(500.0), max_notional: None, flatten_on_breach: false },
    );
    let mut core = default_core(Some(default_budget(true)), keyed, None);
    assert_eq!(core.mount_budget[0].map(|b| b.max_loss), Some(Some(500.0)), "its own key");
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(!core.mount_latched[0], "a loss of 60 is inside ITS budget of 500: {:?}", core.recent);
}

/// ⚠ **The operator's un-latch clears a default-budget latch and re-arms it from where the mount stands**, and
/// the mount then latches again only after a further loss of the stated 50. (The finished verb of decision
/// 0239 is the clearing half of the budget this source feeds.)
#[test]
fn the_unlatch_clears_a_default_budget_latch_and_re_arms_it() {
    let mut core = default_core(Some(default_budget(true)), std::collections::HashMap::new(), None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "{:?}", core.recent);
    // The flatten fills at 40: the mount is flat, its realized PnL is -60. Its other orders end.
    let flatten = core
        .engine
        .registry
        .iter()
        .find(|(_, mo)| mo.request.reduce_only && mo.status.is_live())
        .map(|(coid, _)| coid.clone())
        .expect("the flatten is out");
    core.bus.publish(Event::Fill(coid_fill(&flatten, "BTCUSDT", -1, 1.0, 40.0)), &mut core.engine);
    core.dispatch_applied_fills();
    let live: Vec<String> = core
        .engine
        .registry
        .iter()
        .filter(|(_, mo)| mo.status.is_live())
        .map(|(coid, _)| coid.clone())
        .collect();
    for coid in &live {
        core.bus.publish(
            Event::OrderRejected(vike_model::events::OrderRejected {
                cause: None,
                client_order_id: coid.clone(),
                reason: "venue said no".to_string().into(),
                ts: 0,
            }),
            &mut core.engine,
        );
    }
    assert_eq!(core.mount_attr[0].size, 0.0, "precondition, flat");
    assert_eq!(core.mount_attr[0].realized_pnl, -60.0, "precondition, the loss that latched it");

    unlatch(&mut core, &mount_id());
    assert!(!core.mount_latched[0], "cleared: {:?}", core.recent);
    assert_eq!(core.mount_loss_base[0], -60.0, "re-armed from where it stands");
    core.drive_due_timers(60_100);
    assert!(!core.mount_latched[0], "not judged on its history: {:?}", core.recent);
}

/// ⚠ **A restart restores a default-budget mount latched, and unlatched with its base after the verb** — the
/// latch rides the journal's `Snap` and the restart restores it for a mount that still has an ACTIVE budget,
/// which here is the node-wide default. Removing the default at the restart (the control) lifts the latch:
/// a latch is restored only for a mount that still has a budget.
#[test]
fn a_restart_restores_a_default_budget_latch_and_only_while_the_default_stands() {
    let dir = budget_journal_dir();
    let mut core = default_core(
        Some(default_budget(false)),
        std::collections::HashMap::new(),
        Some(JournalConfig::at(dir.to_path_buf())),
    );
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "{:?}", core.recent);
    core.write_snap(30_200);
    core.journal.as_mut().expect("journaling is on").flush().expect("flush");
    drop(core);

    let rows = vike_journal::CommandJournal::read_all(&dir)
        .expect("the journal reads back")
        .iter()
        .rev()
        .find_map(|r| match r {
            vike_journal::JournalRecord::Snap { mount_attr, .. } => Some(mount_attr.clone()),
            _ => None,
        })
        .expect("a Snap was written");
    assert!(rows.len() == 1 && rows[0].latched, "the latch rides the Snap: {rows:?}");

    let restart = |default: Option<MountBudget>| {
        core_with(CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(budget_mount("BTCUSDT")),
            mount_budget_default: default,
            mount_attr: rows.clone(),
            ..CoreConfig::default()
        })
    };
    let back = restart(Some(default_budget(false)));
    assert!(back.mount_latched[0], "the restart restores the latch under the same default");
    let without = restart(None);
    assert!(
        !without.mount_latched[0],
        "the control: with the row removed the mount has no budget and the latch is not restored"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
