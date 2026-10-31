//! The operator's un-latch of one mount's budget (`Command::UnlatchMount`, decision 0239): refused in words unless
//! the mount is latched, flat and quiet; accepted, it clears the latch and re-arms the loss budget from the mount's
//! current net realized PnL; and the result rides the journal's `Snap`, so a restart puts the mount back UNLATCHED
//! with its new base (the latch itself is `mount_budget_latch.rs`'s subject).

use super::mount_budget::{
    budget_journal_dir, budget_mount, clock_budget_core, coid_fill, one_budget, reduce_only_sent,
};
use super::*;

/// The id the (controller-id-less) budget mount is published under.
fn mount_id() -> String {
    crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m")
}

fn unlatch(core: &mut CoreThread<RecordingClient>, id: &str) {
    core.dispatch(Ingest::Command(Command::UnlatchMount { mount_id: id.to_string() }));
}

/// The refusal lines the ring holds, oldest first.
fn refusals(core: &CoreThread<RecordingClient>) -> Vec<String> {
    core.recent
        .iter()
        .filter(|e| e.contains("MOUNT UNLATCH REFUSED"))
        .map(|e| e.to_string())
        .collect()
}

/// The live reduce-only order a flatten left in engine 0's registry.
fn live_flatten(core: &CoreThread<RecordingClient>) -> String {
    core.engine
        .registry
        .iter()
        .find(|(_, mo)| mo.request.reduce_only && mo.status.is_live())
        .map(|(coid, _)| coid.clone())
        .expect("a live reduce-only flatten is out")
}

/// The venue refuses `coid` (`RecordingClient` emits no accept, so a rejection is the one terminal the FSM takes
/// straight from `Initialized`).
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

/// Latch the budgeted mount (a long 1 at 100 marked at 40: a loss of 60 against a budget of 50) on the clock's first
/// sweep. Its flatten (when the budget flattens) goes out whole and stays `Initialized`.
fn latched_core(flatten: bool) -> CoreThread<RecordingClient> {
    let mut core = clock_budget_core(flatten, None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "precondition, latched: {:?}", core.recent);
    core
}

/// [`latched_core`] with its flatten FILLED at 40: the mount is flat, its realized PnL is -60 and nothing of it
/// is live. `journal` turns the command journal on.
///
/// `RecordingClient` emits no accept, so the entry and the flatten stay `Initialized` in the registry beside their
/// folded fills (a bare `Fill` moves the ledger and not the order's lifecycle): a mount with an order the registry
/// still holds live is NOT quiet and the un-latch refuses it, which is the point of that check. So the venue ends
/// them here (a rejection is the one terminal the FSM takes straight from `Initialized`), as it would have filled
/// them.
fn flat_latched_core(journal: Option<JournalConfig>) -> CoreThread<RecordingClient> {
    let mut core = clock_budget_core(true, journal);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "precondition, latched: {:?}", core.recent);
    let flatten = live_flatten(&core);
    core.bus.publish(Event::Fill(coid_fill(&flatten, "BTCUSDT", -1, 1.0, 40.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(core.mount_attr[0].size, 0.0, "precondition, flat");
    assert_eq!(core.mount_attr[0].realized_pnl, -60.0, "precondition, the loss that latched it");
    let live: Vec<String> = core
        .engine
        .registry
        .iter()
        .filter(|(_, mo)| mo.status.is_live())
        .map(|(coid, _)| coid.clone())
        .collect();
    for coid in &live {
        venue_rejects(&mut core, coid);
    }
    assert!(
        core.engine.registry.iter().all(|(_, mo)| !mo.status.is_live()),
        "precondition, quiet: no order of the mount is live"
    );
    core
}

/// One quote tick for the budgeted mount: its strategy submits one market order per hook call.
fn tick(core: &mut CoreThread<RecordingClient>) {
    core.drive_strategy_tick("sim", "BTCUSDT", 40.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
}

/// ⚠ **The un-latch of a flat, quiet mount clears the latch and re-arms the loss budget from where the mount
/// stands.** Without a new base the sweep judges the cumulative loss that latched the mount and latches it again
/// at the next interval (the control below: the flag cleared alone). With one, the mount trades again, is not
/// judged on its history, and latches again only after a further loss of `max_loss`. The account's state and the
/// mount's orders are not touched, and the note SAYS the new base.
#[test]
fn an_unlatch_of_a_flat_quiet_mount_clears_the_latch_and_re_arms_the_loss_budget_from_where_it_stands()
 {
    // The control: the flag alone is not an un-latch, the mount latches again at the next sweep.
    let mut naive = flat_latched_core(None);
    naive.mount_latched[0] = false;
    naive.drive_due_timers(60_100);
    assert!(
        naive.mount_latched[0],
        "without a new base the cumulative loss of 60 latches it again: {:?}",
        naive.recent
    );

    let mut core = flat_latched_core(None);
    let sent = core.engine.client.submissions.len();
    tick(&mut core);
    assert_eq!(
        core.engine.client.submissions.len(),
        sent,
        "latched: the mount's intent is discarded"
    );

    unlatch(&mut core, &mount_id());
    assert!(!core.mount_latched[0], "cleared: {:?}", core.recent);
    assert_eq!(core.mount_loss_base[0], -60.0, "the base is the mount's net realized PnL");
    assert!(refusals(&core).is_empty(), "{:?}", core.recent);
    let note = core
        .recent
        .iter()
        .find(|e| e.contains("MOUNT BUDGET UNLATCHED"))
        .expect("the operator is told")
        .to_string();
    assert!(note.contains("-60.00"), "the new base is named: {note}");
    assert!(
        note.contains("60.00") && note.contains("50.00"),
        "what it counted, what it may lose: {note}"
    );
    assert!(note.contains("by the operator") && note.contains(&mount_id()), "{note}");
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "an un-latch never touches the account"
    );

    tick(&mut core);
    assert_eq!(
        core.engine.client.submissions.len(),
        sent + 1,
        "un-latched: the mount trades again"
    );

    // The base keeps it free at the sweeps…
    core.drive_due_timers(60_100);
    assert!(!core.mount_latched[0], "not judged on its history: {:?}", core.recent);
    // …a loss of 20 beyond the base does not latch it…
    core.mount_attr[0].size = 1.0;
    core.mount_attr[0].avg_px = 100.0;
    core.engine.price_board.set_mark("sim", "BTCUSDT", 80.0, 85_000);
    core.drive_due_timers(90_100);
    assert!(!core.mount_latched[0], "a further loss of 20 is inside the budget: {:?}", core.recent);
    // …and one of 60 does.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 115_000);
    core.drive_due_timers(120_100);
    assert!(core.mount_latched[0], "a further loss of 60 latches it again: {:?}", core.recent);
}

/// A mount that is not latched, one the node does not run and an empty id are refused in words, and nothing moves.
#[test]
fn an_unlatch_refuses_a_mount_that_is_not_latched_and_one_that_does_not_exist() {
    let mut core = clock_budget_core(false, None);
    assert!(!core.mount_latched[0]);
    unlatch(&mut core, &mount_id());
    let said = refusals(&core);
    assert_eq!(said.len(), 1, "{:?}", core.recent);
    assert!(said[0].contains("is not latched") && said[0].contains("nothing to clear"), "{said:?}");
    assert_eq!(core.mount_loss_base[0], 0.0, "no base was written");

    unlatch(&mut core, "no-such-mount");
    let said = refusals(&core);
    assert!(
        said[1].contains("no live strategy mount") && said[1].contains(&mount_id()),
        "the refusal lists the mounts that run: {said:?}"
    );
    unlatch(&mut core, "   ");
    assert!(refusals(&core)[2].contains("empty mount id"), "{:?}", refusals(&core));
    assert_eq!(core.recent.iter().filter(|e| e.contains("UNLATCHED")).count(), 0);
}

/// The id is read the way an unmount reads it (`mount_id_with`'s sanitizing): a near miss names no mount and is
/// refused, the published spelling finds it.
#[test]
fn an_unlatch_names_the_mount_by_its_published_id_and_a_near_miss_is_refused() {
    let mut core = flat_latched_core(None);
    unlatch(&mut core, "sim-BTCUSDT-1m");
    assert_eq!(
        refusals(&core).len(),
        1,
        "`sim_BTCUSDT_1m` is not this mount's id (`sim__BTCUSDT__1m`)"
    );
    assert!(core.mount_latched[0]);
    unlatch(&mut core, &mount_id());
    assert!(!core.mount_latched[0], "{:?}", core.recent);
}

/// ⚠ **A mount that still holds a position is refused, and the refusal says what is closing it.** A budget that
/// flattens has its close offered again when the operator asks (it is out already: none is stacked on it; it was
/// rejected: it is sent again now; the engine is halted: it waits), a cancel-only budget is closed by hand. The
/// latch stays.
#[test]
fn an_unlatch_refuses_a_mount_that_still_holds_a_position_and_says_what_is_closing_it() {
    let mut core = latched_core(true);
    assert_eq!(reduce_only_sent(&core), (1, 1.0), "the latch's own flatten");
    unlatch(&mut core, &mount_id());
    let said = refusals(&core);
    assert!(said[0].contains("still holds") && said[0].contains("once it is flat"), "{said:?}");
    assert!(said[0].contains("already out") && said[0].contains("none was stacked"), "{said:?}");
    assert!(core.mount_latched[0], "the latch stays");
    assert_eq!(reduce_only_sent(&core).0, 1, "no second close beside the first");

    // The venue refused the flatten: the unlatch attempt offers it again, now.
    let first = live_flatten(&core);
    venue_rejects(&mut core, &first);
    unlatch(&mut core, &mount_id());
    let said = refusals(&core);
    assert!(said[1].contains("offered again just now"), "{said:?}");
    assert_eq!(
        reduce_only_sent(&core),
        (2, 2.0),
        "the missing quantity went out: {:?}",
        core.recent
    );
    assert!(core.mount_latched[0]);

    // A halted engine refuses even a close: it waits for the operator's resume.
    let second = live_flatten(&core);
    venue_rejects(&mut core, &second);
    core.engine.trading_state = TradingState::Halted;
    unlatch(&mut core, &mount_id());
    assert!(refusals(&core)[2].contains("waits"), "{:?}", refusals(&core));
    assert_eq!(reduce_only_sent(&core).0, 2, "nothing was sent into a halted engine");

    // A cancel-only budget never flattens: the operator closes it by hand.
    let mut cancel_only = latched_core(false);
    unlatch(&mut cancel_only, &mount_id());
    let said = refusals(&cancel_only);
    assert!(said[0].contains("cancel-only") && said[0].contains("by hand"), "{said:?}");
    assert_eq!(reduce_only_sent(&cancel_only), (0, 0.0), "no flatten was invented for it");
    assert!(cancel_only.mount_latched[0]);
}

/// A flat mount that still has a live order, or an armed conditional, is refused too: the latch cancels its
/// resting orders ONCE and a cancel the venue held back is not offered again. The order the refusal names is the
/// mount's own, and once it ends the un-latch goes through.
#[test]
fn an_unlatch_refuses_a_mount_with_a_live_order_or_an_armed_conditional() {
    let mut core = flat_latched_core(None);
    let req = vike_model::OrderRequest {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(10.0),
        ts: 31_000,
        ..Default::default()
    };
    let coids = core.apply_intent_routed(
        OrderIntent::Submit(Box::new(req)),
        31_000,
        CancelIntent::Unspecified,
        EngineRoute::Mount(0),
    );
    assert_eq!(coids.len(), 1, "precondition: one order out");
    for c in &coids {
        core.own_coid(c.clone(), 0, None);
    }
    unlatch(&mut core, &mount_id());
    let said = refusals(&core);
    assert!(said[0].contains("1 live order(s)") && said[0].contains(&coids[0]), "{said:?}");
    assert!(core.mount_latched[0]);

    venue_rejects(&mut core, &coids[0]);
    core.cond_engine.insert(
        "arm-1".to_string(),
        ArmedBy { engine: 0, mount: Some(0), reduce_only: false, covered: false, rearms: 0 },
    );
    unlatch(&mut core, &mount_id());
    let said = refusals(&core);
    assert!(
        said[1].contains("0 live order(s)") && said[1].contains("1 armed conditional"),
        "{said:?}"
    );
    assert!(core.mount_latched[0]);

    core.cond_engine.clear();
    unlatch(&mut core, &mount_id());
    assert!(!core.mount_latched[0], "quiet at last: {:?}", core.recent);
}

/// An un-latch of a budget with no loss arm says so (it re-arms nothing it does not have), and keeps the notional
/// cap, which judges the open exposure and has no history.
#[test]
fn an_unlatch_of_a_notional_only_budget_says_it_has_no_loss_arm() {
    let mut core = core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: None, max_notional: Some(500.0), flatten_on_breach: false },
        ),
        ..CoreConfig::default()
    });
    core.mount_latched[0] = true;
    unlatch(&mut core, &mount_id());
    assert!(!core.mount_latched[0], "{:?}", core.recent);
    let note = core.recent.iter().find(|e| e.contains("UNLATCHED")).expect("said").to_string();
    assert!(note.contains("no loss arm") && note.contains("notional cap 500.00"), "{note}");
}

/// ⚠ **The un-latch rides the journal's `Snap` and a restart puts the mount back UNLATCHED with its new base.**
/// It writes no `Cmd` record of its own (a new journaled variant is one a build from before it cannot parse): the
/// `Snap` it writes at once carries `latched` (absent now) and `loss_base`. The control: the same restart WITHOUT
/// the base would latch the mount again at its first sweep.
#[test]
fn the_unlatch_rides_the_snap_and_a_restart_restores_the_mount_unlatched_with_its_base() {
    let dir = budget_journal_dir();
    let mut core = flat_latched_core(Some(JournalConfig::at(dir.to_path_buf())));
    unlatch(&mut core, &mount_id());
    assert!(!core.mount_latched[0], "{:?}", core.recent);
    core.journal.as_mut().expect("journaling is on").flush().expect("flush");
    drop(core);

    let records = vike_journal::CommandJournal::read_all(&dir).expect("the journal reads back");
    assert!(
        !records.iter().any(|r| matches!(
            r,
            vike_journal::JournalRecord::Cmd {
                msg: Ingest::Command(Command::UnlatchMount { .. }),
                ..
            }
        )),
        "no Cmd record: the Snap is the durable half"
    );
    let rows = records
        .iter()
        .rev()
        .find_map(|r| match r {
            vike_journal::JournalRecord::Snap { mount_attr, .. } => Some(mount_attr.clone()),
            _ => None,
        })
        .expect("the un-latch wrote a Snap");
    assert_eq!(rows.len(), 1, "one live mount: {rows:?}");
    assert!(!rows[0].latched, "no longer latched: {rows:?}");
    assert_eq!(rows[0].loss_base, -60.0, "the new base rides the Snap: {rows:?}");
    assert_eq!(rows[0].realized_pnl, -60.0, "…beside the ledger it always carried");

    let restart = |rows: Vec<vike_journal::SnapMountAttr>| {
        core_with(CoreConfig {
            seed_cash: 10_000.0,
            strategy: Some(budget_mount("BTCUSDT")),
            mount_budgets: one_budget(
                "BTCUSDT",
                MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: true },
            ),
            mount_attr: rows,
            ..CoreConfig::default()
        })
    };
    let mut back = restart(rows.clone());
    assert!(!back.mount_latched[0], "the restart restores the mount unlatched");
    assert_eq!(back.mount_loss_base[0], -60.0, "…with its base");
    back.sweep_mount_budgets(60_000);
    assert!(!back.mount_latched[0], "and the first sweep does not latch it on its history");

    // The control: a row without the base (a build from before the key) judges the whole history.
    let mut without = rows;
    without[0].loss_base = 0.0;
    let mut naive = restart(without);
    naive.sweep_mount_budgets(60_000);
    assert!(
        naive.mount_latched[0],
        "without the base the restart latches it again: {:?}",
        naive.recent
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A runtime unmount forgets the base with the slot, and a mount of the same id starts from none.
#[test]
fn an_unmount_resets_the_loss_base_of_its_slot() {
    let mut core = flat_latched_core(None);
    unlatch(&mut core, &mount_id());
    assert_eq!(core.mount_loss_base[0], -60.0);
    core.dispatch(Ingest::Command(Command::UnmountStrategy { controller_id: mount_id() }));
    assert_eq!(core.mount_loss_base[0], 0.0, "the tombstone keeps no base: {:?}", core.recent);
    assert!(!core.mount_latched[0]);
}
