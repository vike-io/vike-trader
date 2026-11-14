//! The strategy FILE a mount runs, in the journal (2b's J30): written once as the mount opens (a boot mount
//! at assembly, a runtime one in the mount arm), carried on the mount's own `Snapshot` row, and read back by the
//! boot restore, so "which version of the file traded" is answerable after a restart. A core whose mounts no
//! file names writes what it always did.

use super::runtime_mount_tests::{mount_command, spec};
use super::*;
use crate::replay::restore_for_boot;
use crate::runtime::test_support::core_with;
use crate::scratch::Scratch;
use vike_execution::MountSpec;
use vike_journal::{Journal, JournalFileConfig, JournalRecord, JournalStrategyFile};

/// A strategy that does nothing: the subject is the mount, not what it trades.
struct Idle;

impl Strategy<LiveBroker> for Idle {}

fn journal(dir: &Scratch) -> JournalConfig {
    JournalConfig {
        dir: dir.to_path_buf(),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    }
}

fn file(path: &str, sha256: &str) -> JournalStrategyFile {
    JournalStrategyFile { path: path.into(), sha256: sha256.into() }
}

/// The id the core keys the boot mount by (`sim`/`BTCUSDT`/`1m`, no controller id).
fn boot_id() -> String {
    crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m")
}

/// A journaled core with one boot mount whose file is `boot.rhai` (sha256 `aa`) and a factory whose runtime
/// mounts are `live.rhai` (sha256 `bb`).
fn core_on(dir: &Scratch) -> CoreThread<vike_execution::testing::RecordingClient> {
    core_with(CoreConfig {
        clock: Box::new(|| 1_000),
        journal: Some(journal(dir)),
        strategy: Some(crate::testkit::mount_of("sim", "BTCUSDT", "1m", Box::new(Idle))),
        mount_strategy_files: [(boot_id(), file("boot.rhai", "aa"))].into(),
        strategy_factory: Some(Box::new(|_: &MountSpec| {
            Ok((
                Box::new(Idle) as Box<dyn Strategy<LiveBroker> + Send>,
                Some(file("live.rhai", "bb")),
            ))
        })),
        ..CoreConfig::default()
    })
}

/// Every `StrategyFileMounted` record of the journal in `dir`, in order.
fn mounted(dir: &Scratch) -> Vec<(String, JournalStrategyFile)> {
    Journal::read_all(dir)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::StrategyFileMounted { mount_id, strategy_file, .. } => {
                Some((mount_id, strategy_file))
            }
            _ => None,
        })
        .collect()
}

/// **A mount writes the record, once, as it opens**: the boot mount at assembly and a runtime mount in its arm,
/// each under the id the core keys it by, and the `Snapshot` after carries each on the mount's own row. The record
/// is the stored-words step's (format 30).
#[test]
fn a_boot_and_a_runtime_mount_each_journal_their_strategy_file_once() {
    let dir = Scratch::reserved("vco-strategy-file-mount");
    let mut core = core_on(&dir);
    core.dispatch(mount_command(spec("rt-file")));
    assert_eq!(core.mounts.len(), 2, "mounted at runtime: {:?}", core.recent);
    core.write_snap(2_000);
    drop(core);

    let want =
        [(boot_id(), file("boot.rhai", "aa")), ("rt_file".to_string(), file("live.rhai", "bb"))];
    assert_eq!(mounted(&dir), want, "one record per mount, as it opened");
    let rows: Vec<(String, Option<JournalStrategyFile>)> = Journal::read_all(&dir)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
                Some(mount_attribute)
            }
            _ => None,
        })
        .flatten()
        .map(|row| (row.mount_id, row.strategy_file))
        .collect();
    let want_rows: Vec<(String, Option<JournalStrategyFile>)> =
        want.into_iter().map(|(id, f)| (id, Some(f))).collect();
    assert_eq!(rows, want_rows, "the Snapshot carries each mount's file on its own row");
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(30), "the stored-words step");
}

/// **The restore reads it back**: after a crash (no exit `Snapshot`) the latest `Snapshot`'s rows and the records of its
/// tail say which file each mount last ran, and the boot's notes say it.
#[test]
fn a_restart_reads_back_which_file_each_mount_ran() {
    let dir = Scratch::reserved("vco-strategy-file-restore");
    let tmp = Scratch::created("vco-strategy-file-restore-tmp");
    let mut core = core_on(&dir);
    core.write_snap(2_000);
    core.dispatch(mount_command(spec("rt-file")));
    drop(core); // a crash: the runtime mount's record is in the tail only

    let restore = restore_for_boot(Some(&journal(&dir)), &tmp, 3_000).expect("readable");
    assert_eq!(
        restore.strategy_files(),
        [(boot_id(), file("boot.rhai", "aa")), ("rt_file".to_string(), file("live.rhai", "bb"))],
        "the Snapshot's row, then the tail's record"
    );
    let note = "journal restore: mount `rt_file` last ran strategy file live.rhai (sha256 bb)";
    assert!(restore.notes().iter().any(|n| n == note), "{:?}", restore.notes());
}

/// **A core whose mounts no file names writes what it always did**: no record, no key on a `Snapshot` row, and so no
/// step of its own (the segment keeps the base stamp the phase-4 step will set for every segment).
#[test]
fn a_mount_no_file_names_writes_no_record_and_no_key() {
    let dir = Scratch::reserved("vco-strategy-file-none");
    let mut core = core_with(CoreConfig {
        journal: Some(journal(&dir)),
        strategy: Some(crate::testkit::mount_of("sim", "BTCUSDT", "1m", Box::new(Idle))),
        ..CoreConfig::default()
    });
    core.write_snap(2_000);
    drop(core);

    assert!(mounted(&dir).is_empty(), "no record");
    let rows: Vec<_> = Journal::read_all(&dir)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } => {
                Some(mount_attribute)
            }
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(rows.len(), 1, "the premise: a Snapshot with the mount's row");
    assert_eq!(rows[0].strategy_file, None);
    let bytes: Vec<u8> = std::fs::read_dir(&*dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "vjl"))
        .flat_map(|p| std::fs::read(p).unwrap())
        .collect();
    let written = String::from_utf8_lossy(&bytes);
    assert!(!written.contains("strategy_file") && !written.contains("StrategyFileMounted"));
}
