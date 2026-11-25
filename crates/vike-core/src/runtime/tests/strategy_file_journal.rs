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
    JournalStrategyFile { path: path.into(), sha256: sha256.into(), sdk: None }
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
                Some(crate::MountStrategyFile {
                    file: file("live.rhai", "bb"),
                    fill_at_close: false,
                }),
            ))
        })),
        // `sinks.forecasts` on: the sandboxed-reply test journals its forecasts through this core.
        journal_forecasts: true,
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
/// is the stored-words step's (format 30), and the segment carries the floor, 31.
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
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(34), "the floor");
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

    let restore = restore_for_boot(Some(&journal(&dir)), &tmp, 3_000, &[]).expect("readable");
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

/// One closed `sim/BTCUSDT/1m` bar at `timestamp`, through the real `Ingest::BarClose` arm.
fn close_bar(core: &mut CoreThread<vike_execution::testing::RecordingClient>, timestamp: i64) {
    let bar = Bar::ohlcv(timestamp, 100.0, 100.0, 100.0, 100.0, 1.0);
    core.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar,
        sides: None,
    })));
}

/// Every `Forecast` record of the journal in `dir`: `(now_ms, mount_id, venue, symbol, value)`.
fn forecasts(dir: &Scratch) -> Vec<(i64, String, String, String, f64)> {
    Journal::read_all(dir)
        .unwrap()
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::Forecast { now_ms, mount_id, venue, symbol, forecast, .. } => {
                Some((now_ms, mount_id, venue, symbol, forecast.value()))
            }
            _ => None,
        })
        .collect()
}

/// A strategy that publishes two forecasts each bar: its own symbol's (`""`) and ETHUSDT's.
struct Forecasting;
impl Strategy<LiveBroker> for Forecasting {
    fn on_bar(&mut self, broker: &mut LiveBroker, bar: &Bar) {
        let forecast = |v| vike_model::Forecast::new(v, 60_000, Some(0.5)).unwrap();
        broker.publish_forecast("", forecast(bar.close / 100.0));
        broker.publish_forecast("ETHUSDT", forecast(-1.0));
    }
}

/// **Book W35: what a live strategy publishes is journaled as its step drains**, one `Forecast` record each, at the
/// CLOSE of the bar it decided on (decision 0271: a bar step decides there; the bar's own time is its open), on
/// the mount's venue, a forecast naming no symbol on the dispatching series': the record the datahub turns into
/// `kind=exec_forecast`. The first one raises the segment to format 32; a core whose strategies publish none writes
/// no record and keeps format 31, the base (decision 0290's floor). Journaling is the run profile's
/// `sinks.forecasts` (`CoreConfig::journal_forecasts`), on here.
#[test]
fn a_live_strategys_forecasts_are_journaled_and_a_silent_one_writes_none() {
    let dir = Scratch::reserved("vco-forecast-journal");
    let mut core = core_with(CoreConfig {
        clock: Box::new(|| 1_000),
        journal: Some(journal(&dir)),
        strategy: Some(crate::testkit::mount_of("sim", "BTCUSDT", "1m", Box::new(Forecasting))),
        journal_forecasts: true,
        ..CoreConfig::default()
    });
    // The 1m bar opened at 60_000 closes at 120_000: a study deciding at 119_999 must not read what it decided.
    close_bar(&mut core, 60_000);
    drop(core);
    let (sim, btc, eth) = ("sim".to_string(), "BTCUSDT".to_string(), "ETHUSDT".to_string());
    assert_eq!(
        forecasts(&dir),
        [(120_000, boot_id(), sim.clone(), btc, 1.0), (120_000, boot_id(), sim, eth, -1.0)]
    );
    assert_eq!(
        Journal::latest_segment_version(&dir).unwrap(),
        Some(34),
        "the forecast step (32) is below the floor"
    );

    let silent = Scratch::reserved("vco-forecast-journal-silent");
    let mut core = core_on(&silent);
    close_bar(&mut core, 60_000);
    drop(core);
    assert!(forecasts(&silent).is_empty(), "a strategy that publishes none journals none");
    assert_eq!(Journal::latest_segment_version(&silent).unwrap(), Some(34));
}

/// A SANDBOXED strategy's forecast (decision 0273's reply door) is stamped as an in-process one is: a reply to a
/// bar (`now` is that bar's open) at the bar's close, a reply to any other hook at its `now`.
#[test]
fn a_sandboxed_bar_replys_forecast_is_journaled_at_the_bars_close() {
    let reply = |now: i64, on_bar: bool, value: f64| {
        let write = move |b: &mut LiveBroker| {
            b.publish_forecast("", vike_model::Forecast::new(value, 60_000, None).unwrap());
        };
        let apply =
            crate::runtime::MountReplyIntents { write: Box::new(write), applied: Box::new(|| {}) };
        let reply =
            vike_execution::MountReply { mount_id: boot_id(), now, on_bar, apply: Box::new(apply) };
        Ingest::MountReply(Box::new(reply))
    };
    let dir = Scratch::reserved("vco-forecast-reply");
    let mut core = core_on(&dir);
    core.dispatch(reply(60_000, true, 1.0));
    core.dispatch(reply(90_000, false, 2.0));
    drop(core);
    let stamped: Vec<(i64, f64)> = forecasts(&dir).into_iter().map(|f| (f.0, f.4)).collect();
    assert_eq!(stamped, [(120_000, 1.0), (90_000, 2.0)]);
}

/// **REVIEW-LIVE-MONEY M3: a step journals at most `MAX_FORECASTS_PER_STEP` forecasts.** A strategy that publishes
/// three more in one bar has the first 512 journaled, in order, and the three past the cap refused and counted
/// on its mount; the next step is judged on its own.
#[test]
fn a_step_over_the_forecast_cap_journals_the_cap_and_counts_the_rest() {
    use crate::runtime::journaling::MAX_FORECASTS_PER_STEP;
    struct Flooding;
    impl Strategy<LiveBroker> for Flooding {
        fn on_bar(&mut self, broker: &mut LiveBroker, _: &Bar) {
            for i in 0..MAX_FORECASTS_PER_STEP + 3 {
                broker.publish_forecast(
                    "",
                    vike_model::Forecast::new(i as f64, 60_000, None).unwrap(),
                );
            }
        }
    }
    let dir = Scratch::reserved("vco-forecast-journal-cap");
    let mut core = core_with(CoreConfig {
        clock: Box::new(|| 1_000),
        journal: Some(JournalConfig { snapshot_every: 1_000_000, ..journal(&dir) }),
        strategy: Some(crate::testkit::mount_of("sim", "BTCUSDT", "1m", Box::new(Flooding))),
        journal_forecasts: true,
        ..CoreConfig::default()
    });
    close_bar(&mut core, 60_000);
    assert_eq!(core.forecasts_refused.get(&boot_id()), Some(&3), "three refused, counted");
    close_bar(&mut core, 120_000);
    assert_eq!(core.forecasts_refused.get(&boot_id()), Some(&6), "each step on its own");
    drop(core);
    let written = forecasts(&dir);
    assert_eq!(written.len(), 2 * MAX_FORECASTS_PER_STEP);
    let first_step: Vec<f64> = written.iter().take(MAX_FORECASTS_PER_STEP).map(|f| f.4).collect();
    let in_order: Vec<f64> = (0..MAX_FORECASTS_PER_STEP).map(|i| i as f64).collect();
    assert_eq!(first_step, in_order, "the first 512, in the order published");
}

/// **REVIEW-LIVE-MONEY M1: forecast journaling is OPT-IN.** By default (`sinks.forecasts` absent) the same
/// publishing strategy journals NO forecast and the segment stays at format 31, so the node keeps its rollback.
#[test]
fn by_default_a_published_forecast_is_not_journaled_and_the_format_stays() {
    let dir = Scratch::reserved("vco-forecast-journal-off");
    let mut core = core_with(CoreConfig {
        clock: Box::new(|| 1_000),
        journal: Some(journal(&dir)),
        strategy: Some(crate::testkit::mount_of("sim", "BTCUSDT", "1m", Box::new(Forecasting))),
        ..CoreConfig::default()
    });
    close_bar(&mut core, 60_000);
    drop(core);
    assert!(forecasts(&dir).is_empty(), "off: nothing journaled");
    assert_eq!(Journal::latest_segment_version(&dir).unwrap(), Some(34), "the format never moved");
}
