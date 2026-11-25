//! A journal that stops taking writes while the core is LIVE (decision 0279's addendum of 2026-11-23): the order
//! fence gives up at its bound instead of freezing the core, an order that opens risk is refused NOT SENT in words,
//! a cancel and a covered exit still leave, and the node is HALTED with its fault published.
//!
//! The fault is the write thread's own: the journal's second segment cannot be made because a directory holds its
//! name (`crates/vike-journal/src/handoff_tests.rs`'s `journal_rolled_into_a_blocked_segment` trick), so every
//! hand-off past the first roll fails and is retried without end, as on a full or read-only disk.
//!
//! A REPLAY core (`CoreConfig::replay_core`, the boot refold's) under the same fault is NOT halted: its sends never
//! leave the process, so it holds no fence (`crates/vike-core/src/runtime/journaling.rs`'s `journal_send_fence`).
//!
//! The fence's halt is NOT restored by a CLEAN restart (the owner's 2A of 2026-11-24; the exit `Snapshot` states the
//! trading state before it, `crates/vike-core/src/runtime/journaling.rs`'s `JournalFenceHalt`), and an operator's or an
//! account's halt still is; a CRASH during it comes back halted and regenerates no order the halt denied. The outage
//! here comes back (the blocker removed), as the CI box's stall did.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_core::replay::{RestoreOutcome, restore_for_boot};
use vike_core::{CoreConfig, spawn_core, spawn_core_multi};
use vike_execution::execution_engine::JOURNAL_CANNOT_WRITE_REASON;
use vike_execution::testing::engine_on;
use vike_execution::{Command, OrderIntent};
use vike_model::{OrderRequest, OrderStatus, TradingState};

use crate::kit::doubles::RestingClient;
use crate::kit::engines::sim_engine_with;
use crate::kit::events::sim_bare_fill;
use crate::kit::handle::{CoreJoinGuard, poll_within, wait_for, wait_for_within};
use crate::kit::journal::journal_at;
use crate::kit::requests::sim_limit;
use crate::scratch::Scratch;

/// How many bare fills roll the journal past its first 16 KiB segment, several times over.
const PADDING_FILLS: usize = 300;

fn submit(request: OrderRequest) -> Command {
    Command::Order(OrderIntent::Submit(Box::new(request)))
}

/// A core with one resting order whose journal then stops taking writes (the module doc's fault), with a long of
/// [`PADDING_FILLS`] folded past the stop.
fn core_whose_journal_stopped(
    dir: &Path,
    replay_core: bool,
    cancels: &Arc<AtomicUsize>,
) -> CoreJoinGuard {
    let config = CoreConfig {
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(journal_at(dir, 16 * 1024, 1_000_000)),
        replay_core,
        ..CoreConfig::default()
    };
    let handle = CoreJoinGuard::new(spawn_core(
        sim_engine_with(RestingClient::new("test", Arc::clone(cancels))),
        config,
    ));

    // A resting order, sent while the journal writes: the core is up and its journal open.
    handle.send_command(submit(sim_limit("resting", 1, 1.0, 90.0)));
    wait_for(&handle, "the order rests", |s| {
        s.order("resting").map(|o| o.status) == Some(OrderStatus::Accepted)
    });

    // The second segment's name is a directory from now on: every hand-off past the first roll fails.
    std::fs::create_dir(dir.join("journal-00000001.vjl")).unwrap();
    let sender = handle.event_sender();
    for i in 0..PADDING_FILLS {
        sender.blocking_send(sim_bare_fill(&format!("pad{i}"), 1.0, 100.0)).unwrap();
    }
    wait_for(&handle, "the padding folded (a long the exit can close)", |s| {
        s.positions.iter().any(|p| p.size == PADDING_FILLS as f64)
    });
    handle
}

#[test]
fn a_journal_that_cannot_write_refuses_an_opening_order_lets_a_cancel_and_an_exit_leave_and_halts()
{
    let dir = Scratch::reserved("journal-unwritable");
    let cancels = Arc::new(AtomicUsize::new(0));
    let handle = core_whose_journal_stopped(&dir, false, &cancels);

    // An order that opens risk: NOT SENT, ended in the journal's words, and the node halts.
    handle.send_command(submit(sim_limit("opening", 1, 1.0, 95.0)));
    let refused = format!("OrderRejected opening ({JOURNAL_CANNOT_WRITE_REASON})");
    let snap = wait_for_within(&handle, Duration::from_secs(20), "the refusal and the HALT", |s| {
        s.order("opening").map(|o| o.status) == Some(OrderStatus::Rejected)
            && s.trading_state == TradingState::Halted
    });
    assert!(snap.recent_events.iter().any(|l| **l == *refused), "{:?}", snap.recent_events);
    assert!(snap.fault.is_some(), "the fault is published for `trade health`");
    assert!(
        snap.journal_fence_halt,
        "the typed fact the tradehub's `journal_fence_halt` alert tells"
    );
    assert_eq!(cancels.load(Ordering::Relaxed), 0, "the halt cancels nothing");

    // What reduces risk still leaves, unrecorded: the cancel of the resting order, and an exit the long covers.
    handle.send_command(Command::Order(OrderIntent::Cancel("resting".into())));
    handle.send_command(submit(sim_limit("exit", -1, 1.0, 110.0)));
    poll_within(Duration::from_secs(10), "the cancel reached the client", || {
        cancels.load(Ordering::Relaxed) == 1
    });
    wait_for(&handle, "the cancel landed and the exit rests", |s| {
        s.order("resting").map(|o| o.status) == Some(OrderStatus::Canceled)
            && s.order("exit").map(|o| o.status) == Some(OrderStatus::Accepted)
    });
    // The stop gives up on the failing hand-offs within its own bound.
    handle.shutdown_and_join();
}

/// The boot refold's REPLAY core under the same fault, past the live fence's bound: every replayed submit still
/// reaches the client and the core ends NOT halted, so a boot never applies a halt its scratch journal made.
#[test]
fn a_replay_core_whose_scratch_journal_cannot_write_replays_every_submit_and_does_not_halt() {
    let dir = Scratch::reserved("journal-unwritable-replay");
    let cancels = Arc::new(AtomicUsize::new(0));
    let handle = core_whose_journal_stopped(&dir, true, &cancels);
    // Longer than the live fence's bound (`vike_journal`'s `FENCE_BOUND`, 1 s), the write thread failing throughout.
    std::thread::sleep(Duration::from_millis(1_500));

    let ids = ["replayed0", "replayed1", "replayed2"];
    for id in ids {
        handle.send_command(submit(sim_limit(id, 1, 1.0, 95.0)));
    }
    let snap =
        wait_for_within(&handle, Duration::from_secs(20), "every replayed submit settled", |s| {
            ids.iter().all(|id| {
                s.order(id).is_some_and(|o| {
                    matches!(o.status, OrderStatus::Accepted | OrderStatus::Rejected)
                })
            })
        });
    for id in ids {
        assert_eq!(
            snap.order(id).map(|o| o.status),
            Some(OrderStatus::Accepted),
            "{id}: {:?}",
            snap.recent_events
        );
    }
    assert_eq!(snap.trading_state, TradingState::Active, "{:?}", snap.recent_events);
    assert!(snap.fault.is_none(), "{:?}", snap.fault);
    handle.shutdown_and_join();
}

/// What a restart boots after a journal OUTAGE that comes back (the CI box, 2026-11-24: a write thread stalled past the
/// fence's bound under the box's load): `before` is sent as the journal stops, the cancel of the resting order meets
/// the fence (it waits the bound, leaves unrecorded, and the node halts unless it is halted already), `during` while
/// that holds, then the disk comes back and the node stops. Returns the trading state the journal's restore states
/// and the one a core over that restore boots in.
fn boot_after_an_outage(
    tag: &str,
    before: Option<Command>,
    during: Option<Command>,
) -> (TradingState, TradingState) {
    let dir = Scratch::reserved(tag);
    let tmp = Scratch::created(&format!("{tag}-tmp"));
    let cancels = Arc::new(AtomicUsize::new(0));
    let handle = core_whose_journal_stopped(&dir, false, &cancels);
    if let Some(command) = before {
        handle.send_command(command);
    }
    handle.send_command(Command::Order(OrderIntent::Cancel("resting".into())));
    poll_within(Duration::from_secs(10), "the cancel left, unrecorded (Q1)", || {
        cancels.load(Ordering::Relaxed) == 1
    });
    // The fence's halt is told (the alert's typed fact) on a node an operator had halted already too.
    wait_for(&handle, "the node halted and the fence's halt told", |s| {
        s.trading_state == TradingState::Halted && s.journal_fence_halt
    });
    if let Some(command) = during {
        handle.send_command(command);
    }
    std::fs::remove_dir(dir.join("journal-00000001.vjl")).unwrap(); // the disk comes back
    handle.shutdown_and_join();

    let journal = journal_at(&dir, 16 * 1024, 1_000_000);
    let restore = restore_for_boot(Some(&journal), &tmp, 7, &[]).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    let journaled = restore.restored_engines()[0].trading_state;
    let mut config = CoreConfig::default();
    restore.apply_to(&mut config);
    let cancels = Arc::new(AtomicUsize::new(0));
    let booted = CoreJoinGuard::new(spawn_core(
        sim_engine_with(RestingClient::new("test", Arc::clone(&cancels))),
        config,
    ));
    // A venue fill makes the booted core publish; its ring carries the restore's line.
    booted.event_sender().blocking_send(sim_bare_fill("after-boot", 1.0, 100.0)).unwrap();
    let snap = wait_for(&booted, "the boot published", |s| {
        s.recent_events.iter().any(|l| l.contains("journal restore: full"))
    });
    booted.shutdown_and_join();
    (journaled, snap.trading_state)
}

/// **A CRASH during the fence's halt boots `Halted`, and an order the halted node refused is not regenerated.** Only
/// the exit `Snapshot` states the trading state from before the halt; the halt's own `Snapshot` and every other one
/// state the engines as they are, so a refold from one denies what the live core denied. The live run: the fence
/// refuses an opening order and halts, the disk comes back, a second opening order is DENIED by the halt, and the
/// process dies there (`kill -9`: a copy of the files once they hold that order's record, no exit `Snapshot`).
#[test]
fn a_crash_during_a_journal_fence_halt_boots_halted_and_regenerates_no_refused_order() {
    let dir = Scratch::reserved("journal-fence-crash");
    let copy = Scratch::reserved("journal-fence-crash-copy");
    let tmp = Scratch::created("journal-fence-crash-tmp");
    let cancels = Arc::new(AtomicUsize::new(0));
    let handle = core_whose_journal_stopped(&dir, false, &cancels);
    handle.send_command(submit(sim_limit("opening", 1, 1.0, 95.0)));
    wait_for_within(&handle, Duration::from_secs(20), "the fence's refusal and the HALT", |s| {
        s.order("opening").map(|o| o.status) == Some(OrderStatus::Rejected)
            && s.trading_state == TradingState::Halted
    });
    std::fs::remove_dir(dir.join("journal-00000001.vjl")).unwrap(); // the disk comes back
    handle.send_command(submit(sim_limit("after-halt", 1, 1.0, 95.0)));
    wait_for(&handle, "the halted node denied it", |s| {
        s.denied_orders
            .as_deref()
            .is_some_and(|d| d.iter().any(|v| v.event.order_client_id == "after-halt"))
            || s.recent_events.iter().any(|l| l.contains("after-halt"))
    });
    crate::kit::journal::copy_journal_files_once(
        &dir,
        &copy,
        "the denied order's record in the files",
        |records| {
            records.iter().any(|r| {
                matches!(r, vike_journal::JournalRecord::Command {
                    message: vike_execution::Ingest::Command(Command::Order(OrderIntent::Submit(req))),
                    ..
                } if req.order_client_id == "after-halt")
            })
        },
    );
    handle.shutdown_and_join();

    let restore = restore_for_boot(Some(&journal_at(&copy, 16 * 1024, 1_000_000)), &tmp, 7, &[])
        .expect("readable");
    assert_eq!(
        restore.restored_engines()[0].trading_state,
        TradingState::Halted,
        "{:?}",
        restore.notes()
    );
    let mut config = CoreConfig::default();
    restore.apply_to(&mut config);
    let booted = CoreJoinGuard::new(spawn_core(
        sim_engine_with(RestingClient::new("test", Arc::new(AtomicUsize::new(0)))),
        config,
    ));
    booted.event_sender().blocking_send(sim_bare_fill("after-boot", 1.0, 100.0)).unwrap();
    let snap = wait_for(&booted, "the boot published", |s| {
        s.recent_events.iter().any(|l| l.contains("journal restore"))
    });
    assert_eq!(snap.trading_state, TradingState::Halted, "{:?}", snap.recent_events);
    assert_eq!(
        snap.order("after-halt").map(|o| o.status),
        None,
        "the order the halted node denied is not regenerated: {:?}",
        snap.recent_events
    );
    booted.shutdown_and_join();
}

/// Block the NEXT segment the journal at `dir` rolls into (a directory takes its name, the module doc's fault).
fn block_next_segment(dir: &Path) -> std::path::PathBuf {
    let last = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            name.strip_prefix("journal-")?.strip_suffix(".vjl")?.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0);
    let blocker = dir.join(format!("journal-{:08}.vjl", last + 1));
    std::fs::create_dir(&blocker).unwrap();
    blocker
}

/// **An account that JOINS during the fence's halt keeps its own journaled halt across a clean restart.** Run 1:
/// the operator halts `sim2` alone. Run 2: `sim2`'s venue is not connected at the boot (its book waits), the fence
/// halts the node, `sim2` joins and gets its journaled book back, the disk comes back, a clean stop. The restart: the
/// fence's halt is not restored (`sim` is `Active`), `sim2`'s own halt is.
#[test]
fn a_late_joining_operator_halted_account_stays_halted_after_a_clean_restart() {
    let dir = Scratch::reserved("journal-fence-late-join");
    let tmp = Scratch::created("journal-fence-late-join-tmp");
    let journal = || journal_at(&dir, 16 * 1024, 1_000_000);
    let config = |not_running: Vec<vike_execution::ArmingNotRunningView>| CoreConfig {
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(journal()),
        not_running,
        ..CoreConfig::default()
    };
    let client = |cancels: &Arc<AtomicUsize>| RestingClient::new("test", Arc::clone(cancels));
    let sim2 = || engine_on("sim2", "ETHUSDT", client(&Arc::new(AtomicUsize::new(0))));
    let sim2_waits = || {
        vec![vike_execution::ArmingNotRunningView {
            account: vike_model::accounts::account_keys::AccountNamed::of(
                "sim2",
                vike_model::accounts::account_keys::AccountLabel::Default,
            ),
            tier: None,
            reason: "adapter failed".to_string(),
            block: Some(vike_model::ArmingBlock::AdapterFailed),
            refusal_id: None,
        }]
    };

    // ---- run 1: both accounts run, and the operator halts sim2 alone.
    let cancels = Arc::new(AtomicUsize::new(0));
    let handle = CoreJoinGuard::new(spawn_core_multi(
        sim_engine_with(client(&cancels)),
        vec![(500.0, sim2())],
        config(Vec::new()),
    ));
    handle.send_command(Command::SetAccountTradingState {
        route_key: "sim2".into(),
        state: TradingState::Halted,
    });
    wait_for(&handle, "sim2 halted on its own", |s| {
        s.recent_events.iter().any(|l| l.contains("account sim2 trading state -> Halted"))
    });
    handle.shutdown_and_join();

    // ---- run 2: sim2 waits for its venue; the fence halts the node; then sim2 joins.
    let restore = restore_for_boot(Some(&journal()), &tmp, 7, &[]).expect("readable");
    let mut config2 = config(sim2_waits());
    restore.apply_to(&mut config2);
    let handle = CoreJoinGuard::new(spawn_core_multi(
        sim_engine_with(client(&cancels)),
        Vec::new(),
        config2,
    ));
    handle.send_command(submit(sim_limit("resting", 1, 1.0, 90.0)));
    wait_for(&handle, "the order rests", |s| {
        s.order("resting").map(|o| o.status) == Some(OrderStatus::Accepted)
    });
    let blocker = block_next_segment(&dir);
    let sender = handle.event_sender();
    for i in 0..PADDING_FILLS {
        sender.blocking_send(sim_bare_fill(&format!("pad{i}"), 1.0, 100.0)).unwrap();
    }
    wait_for(&handle, "the padding folded", |s| {
        s.positions.iter().any(|p| p.size == PADDING_FILLS as f64)
    });
    handle.send_command(Command::Order(OrderIntent::Cancel("resting".into())));
    poll_within(Duration::from_secs(10), "the cancel left, unrecorded", || {
        cancels.load(Ordering::Relaxed) == 1
    });
    wait_for(&handle, "the fence's halt", |s| {
        s.trading_state == TradingState::Halted && s.journal_fence_halt
    });
    handle.event_sender().add_account_engine(500.0, sim2()).unwrap();
    wait_for(&handle, "sim2 joined with its journaled book", |s| {
        s.recent_events.iter().any(|l| l.contains("RESTORE ON JOIN sim2"))
    });
    std::fs::remove_dir(&blocker).unwrap(); // the disk comes back
    handle.shutdown_and_join();

    // ---- the clean restart.
    let restore = restore_for_boot(Some(&journal()), &tmp, 8, &[]).expect("readable");
    let state_of = |venue: &str| {
        restore.restored_engines().iter().find(|e| e.venue == venue).map(|e| e.trading_state)
    };
    assert_eq!(
        (state_of("sim"), state_of("sim2")),
        (Some(TradingState::Active), Some(TradingState::Halted)),
        "{:?}",
        restore.notes()
    );
}

/// **The owner's 2A (b): the journal fence's AUTOMATIC halt is not restored.** The halt holds until the node
/// restarts, and the restart boots in the state the account held before it (`Active`), as the halt's words say.
#[test]
fn a_restart_after_a_journal_fence_halt_boots_active() {
    let (journaled, booted) = boot_after_an_outage("journal-fence-restart", None, None);
    assert_eq!((journaled, booted), (TradingState::Active, TradingState::Active));
}

/// **An operator's halt is restored as ever**: one asked before the outage, a node-wide one asked while the fence's
/// halt holds, and an account's own asked then.
#[test]
fn a_restart_after_an_operator_halt_boots_halted_whether_or_not_the_fence_halted_too() {
    let halt = || Command::SetTradingState(TradingState::Halted);
    let account_halt =
        Command::SetAccountTradingState { route_key: "sim".into(), state: TradingState::Halted };
    for (tag, before, during) in [
        ("journal-fence-operator-before", Some(halt()), None),
        ("journal-fence-operator-during", None, Some(halt())),
        ("journal-fence-account-during", None, Some(account_halt)),
    ] {
        let (journaled, booted) = boot_after_an_outage(tag, before, during);
        assert_eq!((journaled, booted), (TradingState::Halted, TradingState::Halted), "{tag}");
    }
}
