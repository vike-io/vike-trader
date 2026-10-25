//! A journaled `Command::SetAccountTradingState` through a REAL core with a journal: it reaches the
//! journal as one record, raises the segment to format 21 only then, replays to the state the live
//! run held (the `replay_offline` fence, and the multi-engine boot restore's tail), and a `Snap`
//! carries what a clamp left. The white-box half (precedence, the link-held engine, the refusals) is
//! `src/runtime/tests/account_trading_state.rs`.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::replay::{RestoreOutcome, replay_offline, restore_for_boot};
use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core, spawn_core_multi};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, CoreSnapshot, EngineSnapshot, Ingest};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord, SnapBooks};
use vike_model::{Clock, TradingState};

use crate::kit::engines::{engine_on, sim_engine};
use crate::kit::events::sim_bare_fill;

fn config(dir: &Path, snapshot_every: u64) -> CoreConfig {
    // Message `k` dispatches at `now_ms == k`: no wall-clock wait, a distinct stamp per record.
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every,
        }),
        ..CoreConfig::default()
    }
}

fn account(route_key: &str, state: TradingState) -> Command {
    Command::SetAccountTradingState { route_key: route_key.to_string(), state }
}

/// Four bare fills (the 4th trips the cadence `Snap`: the replay BASE), then `tail` and one more fill.
fn single_engine_run(dir: &Path, tail: Vec<Command>) {
    let handle = spawn_core(sim_engine(), config(dir, 4));
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap();
    }
    for cmd in tail {
        handle.send_command(cmd);
    }
    sender.blocking_send(sim_bare_fill("t5", 1.0, 100.0)).unwrap();
    handle.shutdown_and_join();
}

/// **The stamp is lazy.** A run that never sends the command leaves the segment at the base version
/// (18) and so a rollback onto the previous build keeps working; the first such record raises it to 21
/// (a v20 build cannot parse the variant), and the record reads back verbatim.
#[test]
fn the_v21_stamp_appears_only_after_the_first_record() {
    let plain = Scratch::reserved("account-state-stamp-absent");
    single_engine_run(&plain, vec![Command::SetTradingState(TradingState::Active)]);
    assert_eq!(
        CommandJournal::latest_segment_version(&plain).unwrap(),
        Some(18),
        "a node-wide word is a base-version record"
    );

    let used = Scratch::reserved("account-state-stamp-present");
    single_engine_run(&used, vec![account("sim", TradingState::Reducing)]);
    assert_eq!(CommandJournal::latest_segment_version(&used).unwrap(), Some(21));
    let cmds: Vec<_> = CommandJournal::read_all(&used)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Cmd {
                msg: Ingest::Command(Command::SetAccountTradingState { route_key, state }),
                ..
            } => Some((route_key, state)),
            _ => None,
        })
        .collect();
    assert_eq!(cmds, vec![("sim".to_string(), TradingState::Reducing)], "one record, verbatim");
}

/// **Replay reproduces the state.** The fence re-pumps the tail through a real core and compares its
/// `state_hash` (which covers each engine's trading state) with the source's exit `Snap`: a record the
/// replay core folded differently from the live one would fail it.
#[test]
fn a_journal_holding_the_record_replays_to_the_live_state() {
    let dir = Scratch::reserved("account-state-replay-fence");
    single_engine_run(&dir, vec![account("sim", TradingState::Halted)]);
    let out = replay_offline(&dir).expect("the fence holds");
    assert_eq!(out.engines[0].trading_state, TradingState::Halted);
    assert!(out.records > 0 && out.snaps_compared >= 1);

    // ...and the same record moving the engine back: the journal is the whole history.
    let dir = Scratch::reserved("account-state-replay-fence-back");
    single_engine_run(
        &dir,
        vec![account("sim", TradingState::Halted), account("sim", TradingState::Active)],
    );
    let out = replay_offline(&dir).expect("the fence holds");
    assert_eq!(out.engines[0].trading_state, TradingState::Active);
}

fn small_file() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 }
}

fn three_engines() -> Vec<EngineSnapshot> {
    vec![
        sim_engine().snapshot_state(),
        engine_on("sim2", "ETHUSDT", RecordingClient::default()).snapshot_state(),
        engine_on("sim3", "SOLUSDT", RecordingClient::default()).snapshot_state(),
    ]
}

/// A journal a crash left: one `Snap` of three Active engines, then `tail` (no exit `Snap`), so the
/// boot restore's multi-engine refold is what folds the records.
fn crash_journal(dir: &Path, tail: Vec<Command>) {
    let mut j = CommandJournal::open(dir, small_file()).unwrap();
    let engines = three_engines();
    let hash = vike_exec::state_hash(&engines);
    j.append_snap(10, &engines, "cafef00d", 0, 0, SnapBooks::default(), hash).unwrap();
    for (i, cmd) in tail.into_iter().enumerate() {
        j.append_cmd(11 + i as i64, &Ingest::Command(cmd)).unwrap();
    }
}

fn restored_states(dir: &Path, tag: &str) -> Vec<(String, TradingState)> {
    let tmp = Scratch::created(tag);
    let restore =
        restore_for_boot(Some(&JournalConfig::at(dir.to_path_buf())), &tmp, 7).expect("readable");
    assert_eq!(restore.outcome(), &RestoreOutcome::Full, "{:?}", restore.notes());
    restore.restored_engines().iter().map(|e| (e.venue.clone(), e.trading_state)).collect()
}

/// **One engine halted, the others Active — through the replay.** The boot restore folds the tail
/// across all three engines through the same dispatch: only the addressed engines move.
#[test]
fn the_boot_restore_replays_the_record_onto_its_engine_alone() {
    use TradingState::{Active, Halted, Reducing};
    let dir = Scratch::reserved("account-state-restore-tail");
    crash_journal(&dir, vec![account("sim2", Halted), account("sim3", Reducing)]);
    assert_eq!(
        restored_states(&dir, "account-state-restore-tail-tmp"),
        vec![
            ("sim".to_string(), Active),
            ("sim2".to_string(), Halted),
            ("sim3".to_string(), Reducing)
        ]
    );

    let dir = Scratch::reserved("account-state-restore-tail-resume");
    crash_journal(&dir, vec![account("sim2", Halted), account("sim2", Active)]);
    assert_eq!(
        restored_states(&dir, "account-state-restore-tail-resume-tmp"),
        vec![
            ("sim".to_string(), Active),
            ("sim2".to_string(), Active),
            ("sim3".to_string(), Active)
        ],
        "resuming the one engine leaves the others where they were"
    );
}

/// An unknown route key replays as the refusal it was: no engine moves.
#[test]
fn an_unknown_route_key_replays_as_a_refusal() {
    let dir = Scratch::reserved("account-state-restore-unknown");
    crash_journal(&dir, vec![account("sim9", TradingState::Halted)]);
    let all = restored_states(&dir, "account-state-restore-unknown-tmp");
    assert!(all.iter().all(|(_, s)| *s == TradingState::Active), "{all:?}");
}

fn wait_for(
    handle: &CoreHandle,
    what: &str,
    pred: impl Fn(&CoreSnapshot) -> bool,
) -> Arc<CoreSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snap = handle.snapshot();
        if pred(&snap) {
            return snap;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn block_state(snap: &CoreSnapshot, venue: &str) -> Option<TradingState> {
    snap.portfolio.venues.iter().find(|b| b.venue == venue).map(|b| b.trading_state)
}

/// **A real two-engine core, killed before it writes its exit `Snap`.** The node-wide `reduce` is the floor;
/// the second engine is halted and then asked `Active`: it lands AT the floor (the published block says so).
/// The journal is COPIED while the core still runs, which is what a crash leaves (no exit `Snap`: the
/// clean-stop `Snap` of a normal shutdown would carry the live state to the restore by itself), and the next
/// boot over that copy restores exactly what the live run held, not the `Active` an unclamped fold would give.
///
/// (That the clamp's own `Snap` is what carries it where the replay core could NOT reproduce the floor — the
/// drawdown latch is not journaled — is pinned white-box:
/// `a_clamp_writes_a_snap_and_a_plain_set_does_not`.)
#[test]
fn a_clamp_is_published_and_a_crash_before_the_exit_snap_restores_it() {
    use TradingState::{Active, Halted, Reducing};
    let dir = Scratch::reserved("account-state-clamp");
    let crash = Scratch::reserved("account-state-clamp-crash-copy");
    let handle = spawn_core_multi(
        sim_engine(),
        vec![(500.0, engine_on("sim2", "ETHUSDT", RecordingClient::default()))],
        config(&dir, 1_000),
    );
    handle.send_command(Command::SetTradingState(Reducing));
    handle.send_command(account("sim2", Halted));
    wait_for(&handle, "sim2 halted, sim reducing", |s| {
        block_state(s, "sim2") == Some(Halted) && block_state(s, "sim") == Some(Reducing)
    });
    handle.send_command(account("sim2", Active));
    let snap =
        wait_for(&handle, "the clamp", |s| s.recent_events.iter().any(|l| l.contains("CLAMPED")));
    assert_eq!(block_state(&snap, "sim2"), Some(Reducing), "the node's reduce holds");
    assert_eq!(block_state(&snap, "sim"), Some(Reducing));
    // The crash: take the journal as it stands, before the clean stop writes its exit Snap.
    crate::kit::journal::copy_journal_files(&dir, &crash);
    handle.shutdown_and_join();

    assert_eq!(
        restored_states(&crash, "account-state-clamp-tmp"),
        vec![("sim".to_string(), Reducing), ("sim2".to_string(), Reducing)],
        "the restart restores what the live run held"
    );
}

/// **A hold survives a node-wide resume — through the replay.** The account watcher's closing-only hold of
/// a kept-up lane twin, then a node-wide `resume`: the restore folds the tail through the same dispatch and
/// the held engine comes back closing-only, the others `Active` (a node-wide word cannot loosen a hold).
#[test]
fn a_hold_replays_and_a_node_wide_resume_does_not_loosen_it() {
    use TradingState::{Active, Halted, Reducing};
    let hold = |on| Command::HoldAccount { route_key: "sim2".into(), hold: on };

    let dir = Scratch::reserved("account-state-hold-replay");
    crash_journal(
        &dir,
        vec![hold(true), Command::SetTradingState(Halted), Command::SetTradingState(Active)],
    );
    assert_eq!(
        restored_states(&dir, "account-state-hold-replay-tmp"),
        vec![
            ("sim".to_string(), Active),
            ("sim2".to_string(), Reducing),
            ("sim3".to_string(), Active)
        ],
    );

    let dir = Scratch::reserved("account-state-hold-replay-released");
    crash_journal(&dir, vec![hold(true), Command::SetTradingState(Active), hold(false)]);
    assert_eq!(
        restored_states(&dir, "account-state-hold-replay-released-tmp"),
        vec![
            ("sim".to_string(), Active),
            ("sim2".to_string(), Active),
            ("sim3".to_string(), Active)
        ],
        "the release lifts what the hold set"
    );

    // …and the record is a v21 one, so the segment is stamped.
    assert_eq!(CommandJournal::latest_segment_version(&dir).unwrap(), Some(21));
}
