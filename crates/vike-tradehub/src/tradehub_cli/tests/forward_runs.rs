//! The forward-run recorder (book chapter 22): a mount opens a `forward` run and an unmount closes it; a restart
//! resumes the open run of the same test, and a changed file ends it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::super::forward_runs::{ForwardMountFacts, ForwardRecorder, start};

fn facts(sha: &str) -> ForwardMountFacts {
    let file = vike_journal::JournalStrategyFile {
        path: "trend/ema.rhai".to_string(),
        sha256: sha.to_string(),
        sdk: None,
    };
    ForwardMountFacts {
        params: serde_json::from_value(serde_json::json!({ "fast": 12 })).unwrap(),
        ..ForwardMountFacts::new(None, "binance", "BTCUSDT", "1m", None, &file)
    }
}

fn live(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn by_id(f: &ForwardMountFacts) -> BTreeMap<String, ForwardMountFacts> {
    BTreeMap::from([(f.mount_id.clone(), f.clone())])
}

/// Every forward run under `runs`: its directory name, what it recorded and its end.
fn forward_runs(runs: &std::path::Path) -> Vec<(String, vike_runs::ForwardRun, Option<i64>)> {
    vike_runs::scan_runs(runs)
        .runs
        .into_iter()
        .map(|s| {
            let (run, end) = vike_runs::read_forward_run(&s.dir).unwrap();
            (s.run_id, run, end)
        })
        .collect()
}

#[test]
fn a_mount_opens_a_forward_run_and_an_unmount_closes_it() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let mount = facts("aa");
    assert_eq!(mount.mount_id, "binance__BTCUSDT__1m");
    assert_eq!(mount.account, "binance/DEFAULT");
    let mut recorder = ForwardRecorder::new(runs.clone());

    // A live mount with no facts (no strategy file names it) records nothing.
    assert!(recorder.pass(&live(&["other__X__1m"]), &by_id(&mount), 1_000).is_empty());
    assert!(!runs.exists(), "nothing written");

    let id = mount.mount_id.as_str();
    assert!(recorder.pass(&live(&[id]), &by_id(&mount), 1_767_225_600_000).is_empty());
    let all = forward_runs(&runs);
    assert_eq!(all.len(), 1);
    let (_, run, end) = &all[0];
    assert_eq!((run.started_ms, *end), (1_767_225_600_000, None));
    assert_eq!((run.strategy_sha256.as_str(), run.account.as_str()), ("aa", "binance/DEFAULT"));
    assert_eq!(run.params, mount.params);

    // Still live: nothing more is written.
    assert!(recorder.pass(&live(&[id]), &by_id(&mount), 1_767_225_700_000).is_empty());
    assert_eq!(forward_runs(&runs).len(), 1);

    // Unmounted: the run gets its end.
    assert!(recorder.pass(&live(&[]), &by_id(&mount), 1_767_312_000_000).is_empty());
    let all = forward_runs(&runs);
    assert_eq!((all.len(), all[0].2), (1, Some(1_767_312_000_000)));
}

#[test]
fn a_restart_resumes_the_open_run_and_a_changed_file_ends_it() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let mount = facts("aa");
    let id = mount.mount_id.clone();
    let mut before = ForwardRecorder::new(runs.clone());
    before.pass(&live(&[&id]), &by_id(&mount), 1_767_225_600_000);
    // The process stops: nothing is ended. A new one sees the same test live and resumes it.
    drop(before);
    let mut after = ForwardRecorder::new(runs.clone());
    assert!(after.pass(&live(&[&id]), &by_id(&mount), 1_767_230_000_000).is_empty());
    let all = forward_runs(&runs);
    assert_eq!(all.len(), 1, "resumed, not minted again");
    let (first, run, end) = all[0].clone();
    assert_eq!((run.started_ms, end), (1_767_225_600_000, None), "its own start kept, still open");

    // The next restart mounts a CHANGED file under the same id: the old test ends, a new one starts.
    let changed = facts("bb");
    let mut next = ForwardRecorder::new(runs.clone());
    assert!(next.pass(&live(&[&id]), &by_id(&changed), 1_767_240_000_000).is_empty());
    let all = forward_runs(&runs);
    assert_eq!(all.len(), 2);
    let old = all.iter().find(|(dir, ..)| *dir == first).unwrap();
    assert_eq!(old.2, Some(1_767_240_000_000), "the old test ended at the new mount");
    let new = all.iter().find(|(dir, ..)| *dir != first).unwrap();
    assert_eq!(
        (new.1.strategy_sha256.as_str(), new.1.started_ms, new.2),
        ("bb", 1_767_240_000_000, None)
    );

    // …and inside one process, a re-mount with other params between two passes does the same.
    let mut retuned = changed.clone();
    retuned.params = vike_model::params::Params::default();
    assert!(next.pass(&live(&[&id]), &by_id(&retuned), 1_767_250_000_000).is_empty());
    let all = forward_runs(&runs);
    assert_eq!(all.len(), 3);
    assert_eq!(all.iter().filter(|(.., end)| end.is_none()).count(), 1, "one open test");
}

/// The `false` row (`flags.record_forward_runs = false`): no thread, no read of the mounts, nothing written.
#[test]
fn an_explicit_false_starts_no_recorder() {
    let stop = Arc::new(AtomicBool::new(false));
    let floor = vike_boot::disk_floor::DiskFloor::of_mib(1);
    assert!(start(false, floor, || unreachable!("a false row reads no mount"), stop).is_none());
}

/// REVIEW-LIVE-MONEY L4: a mount whose run cannot be opened (the runs root is a FILE, so no run directory can be
/// made) is not tried again every pass: the next try waits 30 s, the one after 60 s, and an unmount forgets it.
#[test]
fn a_failing_open_is_tried_again_only_after_its_backoff() {
    // The first wait, verbatim; the second is twice it (the one ladder).
    let first: i64 = 30_000;
    assert_eq!(super::super::forward_runs::FORWARD_RETRY_FIRST.as_millis(), 30_000);
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    std::fs::write(&runs, b"not a directory").unwrap();
    let mount = facts("aa");
    let id = mount.mount_id.clone();
    let mut recorder = ForwardRecorder::new(runs.clone());
    let t0 = 1_767_225_600_000;
    let tried =
        |r: &mut ForwardRecorder, at: i64| !r.pass(&live(&[&id]), &by_id(&mount), at).is_empty();
    assert!(tried(&mut recorder, t0), "the first pass tries and fails");
    assert!(!tried(&mut recorder, t0 + 2_000), "the next pass waits");
    assert!(tried(&mut recorder, t0 + first), "after 30 s it tries again");
    let doubled = t0 + first + 2 * first;
    assert!(!tried(&mut recorder, doubled - 1), "the second wait is 60 s");
    assert!(tried(&mut recorder, doubled));

    // Unmounted, then live again: a fresh mount is tried at once.
    assert!(recorder.pass(&live(&[]), &by_id(&mount), doubled + 1).is_empty());
    assert!(tried(&mut recorder, doubled + 2));
}

/// REVIEW-LIVE-MONEY H1: below the free-space floor (a FAKE disk of 1 GiB free of 100 through
/// `vike_boot::disk_floor`) a pass writes nothing and says so ONCE; with room the run opens.
#[test]
fn below_the_free_space_floor_nothing_is_recorded_and_it_is_said_once() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let mount = facts("aa");
    let id = mount.mount_id.clone();
    let free = Arc::new(AtomicU64::new(1 << 30));
    let reader = Arc::clone(&free);
    let mut recorder = ForwardRecorder::new(runs.clone()).with_write_floor(move |dir| {
        vike_boot::disk_floor::DiskFloor::of_mib(10_240).refusal_with(dir, |_: &std::path::Path| {
            Ok(vike_boot::disk_floor::FilesystemSpace {
                available_bytes: reader.load(Ordering::SeqCst),
                total_bytes: 100 << 30,
            })
        })
    });
    let said = recorder.pass(&live(&[&id]), &by_id(&mount), 1_767_225_600_000);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("until the disk has room"), "{said:?}");
    assert!(recorder.pass(&live(&[&id]), &by_id(&mount), 1_767_225_602_000).is_empty(), "once");
    assert!(!runs.exists(), "nothing written below the floor");

    free.store(50 << 30, Ordering::SeqCst);
    assert!(recorder.pass(&live(&[&id]), &by_id(&mount), 1_767_225_604_000).is_empty());
    assert_eq!(forward_runs(&runs).len(), 1, "room: the run opens");
}
