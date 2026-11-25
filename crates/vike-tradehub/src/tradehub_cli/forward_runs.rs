//! The FORWARD-RUN RECORDER (book chapter 22): every strategy-file mount of this daemon is recorded as a
//! `vike_runs` run of kind `forward` (`vike_runs::FORWARD_RUN_KIND`), so a forward test can be found and held
//! against its backtest (`forward_report`'s `forward_run` recipe key).
//!
//! # The contract
//!
//! - **ON unless the row says `false`**: `flags.record_forward_runs` is on by default (the owner's ruling of
//!   2026-11-17). A `false` row starts no thread and writes nothing ([`start`] answers `None`), and a process
//!   that declared no project has no runs root: it records nothing and says so once at `warn`.
//! - **Off the core fold**: its own named thread, `th-forward`, reads the core's published snapshot (the
//!   lossy arc-swap cell the summary thread reads) every [`FORWARD_CHECK_EVERY`]. A live
//!   `vike_execution::MountViewKind::Mount` row whose id has FACTS opens a run; a recorded id no longer live
//!   gets its end (`vike_runs::end_forward_run`). The run's start is the pass that first saw the mount live,
//!   so it is late by at most one period.
//! - **The facts are the node's own resolution** ([`note_mount`]): the boot mounts as `resolve_mounts`
//!   resolved them, a runtime or resurrected mount as `crate::mount_factory::strategy_factory` did (on the
//!   fold, where it only inserts into a map: nothing here touches disk on the fold). A mount no file names has
//!   no facts and no run.
//! - **A restart is not an unmount** (the owner's rule that a restart never cancels): opening a mount first
//!   looks under the runs root for an OPEN forward run of the same mount id. The same file sha256, params and
//!   account resume it with its own start; any other gets its end now and a new run is minted. A clean stop
//!   ends nothing either.
//! - A failure (a disk that will not take the run) is said at `warn` once per distinct text, and the mount
//!   trades on: the record is worth keeping, not worth stopping a strategy over.
//! - **Below the optional writers' free-space floor** (`config.optional_writes_min_free_mib`,
//!   `vike_boot::disk_floor`, REVIEW-LIVE-MONEY H1) a pass writes NOTHING and says so once, so a forward run
//!   never fills the disk this node's journal shares; the first pass with room catches up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_runs::{FORWARD_RUN_KIND, FORWARD_RUN_SCHEMA, ForwardRun};
use vike_venues::reconnect::ReconnectBackoff;

/// How often the recorder compares the live mounts with the runs it holds.
pub(super) const FORWARD_CHECK_EVERY: Duration = Duration::from_secs(2);

/// How often the sleeping thread looks at the stop flag.
const STOP_POLL: Duration = Duration::from_millis(200);

/// What the node resolved for one strategy-file mount: what its forward run records.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ForwardMountFacts {
    pub(crate) mount_id: String,
    pub(crate) venue: String,
    pub(crate) symbol: String,
    pub(crate) interval: String,
    /// `venue/LABEL`, `DEFAULT` for the venue's unlabelled account.
    pub(crate) account: String,
    pub(crate) strategy_file: String,
    pub(crate) strategy_sha256: String,
    pub(crate) params: vike_model::params::Params,
}

impl ForwardMountFacts {
    /// The facts of a mount on `account` of `venue` (`None`: the unlabelled account) running `file`, with no
    /// params (a caller sets the ones it has).
    pub(crate) fn new(
        controller_id: Option<&str>,
        venue: &str,
        symbol: &str,
        interval: &str,
        account: Option<&vike_model::accounts::account_keys::AccountLabel>,
        file: &vike_journal::JournalStrategyFile,
    ) -> Self {
        let label = account.map_or_else(|| "DEFAULT".to_string(), ToString::to_string);
        ForwardMountFacts {
            mount_id: vike_core::strategy_state::mount_id_with(
                controller_id,
                venue,
                symbol,
                interval,
            ),
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            account: format!("{venue}/{label}"),
            strategy_file: file.path.clone(),
            strategy_sha256: file.sha256.clone(),
            params: vike_model::params::Params::default(),
        }
    }

    /// The forward run these facts open at `started_ms`.
    fn run_at(&self, started_ms: i64) -> ForwardRun {
        ForwardRun {
            schema: FORWARD_RUN_SCHEMA,
            mount_id: self.mount_id.clone(),
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            interval: self.interval.clone(),
            account: self.account.clone(),
            strategy_file: self.strategy_file.clone(),
            strategy_sha256: self.strategy_sha256.clone(),
            params: self.params.clone(),
            // shortcut: no door names a reference run yet (the daemon profile is stored as rows, so a key needs a
            // store column: the STOP in FORWARD-IMPROVE.md); the study's recipe names the backtest meanwhile.
            reference_run: None,
            started_ms,
        }
    }

    /// Whether `run` is the same forward test as these facts: the same file bytes, params and account.
    fn same_test(&self, run: &ForwardRun) -> bool {
        run.strategy_sha256 == self.strategy_sha256
            && run.params == self.params
            && run.account == self.account
    }
}

/// The facts of every strategy-file mount this process resolved, by mount id: written by the boot resolve and
/// the mount factory (on the fold: one insert), read by the recorder's thread. Bounded by the mounts a node runs;
/// a later resolution of the same id replaces the earlier one.
static MOUNT_FACTS: Mutex<BTreeMap<String, ForwardMountFacts>> = Mutex::new(BTreeMap::new());

/// Note what the node resolved for one mount ([`MOUNT_FACTS`]).
pub(crate) fn note_mount(facts: ForwardMountFacts) {
    MOUNT_FACTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(facts.mount_id.clone(), facts);
}

/// How long a mount whose forward run could not be opened waits before its next try, the first time
/// (REVIEW-LIVE-MONEY L4: a try scans every manifest under the runs root, so a failing disk is not read every
/// [`FORWARD_CHECK_EVERY`]).
pub(super) const FORWARD_RETRY_FIRST: Duration = Duration::from_secs(30);

/// The longest wait between two tries of one mount: each failure doubles the wait up to this (the one ladder,
/// `vike_venues::reconnect::ReconnectBackoff`).
const FORWARD_RETRY_MAX: Duration = Duration::from_secs(600);
const _: () = assert!(FORWARD_RETRY_MAX.as_secs() >= 2 * FORWARD_RETRY_FIRST.as_secs());

/// The free-space check a recorder asks over its runs root before each pass: `Some(why)` while no room.
type ForwardWriteFloor = Box<dyn Fn(&Path) -> Option<String> + Send>;

/// The open forward runs one recorder holds, by mount id, under one runs root.
pub(super) struct ForwardRecorder {
    runs_root: PathBuf,
    open: BTreeMap<String, (PathBuf, ForwardRun)>,
    /// [`Self::with_write_floor`]'s check, and whether the last pass found no room.
    write_floor: Option<ForwardWriteFloor>,
    held_back: bool,
    /// The live mounts whose open failed: when each is tried next, and the ladder of its waits.
    retry_at: BTreeMap<String, (i64, ReconnectBackoff)>,
}

impl ForwardRecorder {
    pub(super) fn new(runs_root: PathBuf) -> Self {
        ForwardRecorder {
            runs_root,
            open: BTreeMap::new(),
            write_floor: None,
            held_back: false,
            retry_at: BTreeMap::new(),
        }
    }

    /// This recorder, asking `floor` over its runs root before each pass (REVIEW-LIVE-MONEY H1: the optional
    /// writers' free-space floor). Below it a pass writes nothing, opens, resumes and ends nothing, and says so
    /// once; the next pass with room catches up (an end is then stamped at that pass).
    pub(super) fn with_write_floor(
        mut self,
        floor: impl Fn(&Path) -> Option<String> + Send + 'static,
    ) -> Self {
        self.write_floor = Some(Box::new(floor));
        self
    }

    /// One pass at `now_ms` over the mount ids `live` now and the `facts` the node resolved: close what is gone
    /// (or now runs something else), open or resume what is live. Answers every failure, in words. A mount whose
    /// open failed is tried again only after its wait ([`FORWARD_RETRY_FIRST`], doubled by each failure up to
    /// [`FORWARD_RETRY_MAX`]): each try scans the whole runs root.
    pub(super) fn pass(
        &mut self,
        live: &BTreeSet<String>,
        facts: &BTreeMap<String, ForwardMountFacts>,
        now_ms: i64,
    ) -> Vec<String> {
        if let Some(why) = self.write_floor.as_ref().and_then(|floor| floor(&self.runs_root)) {
            let first = !std::mem::replace(&mut self.held_back, true);
            return if first {
                vec![format!(
                    "NOTHING is recorded (no run opened, resumed or ended) until the disk has room: {why}"
                )]
            } else {
                Vec::new()
            };
        }
        if std::mem::take(&mut self.held_back) {
            tracing::info!("forward run: the disk has room again; recording");
        }
        self.retry_at.retain(|id, _| live.contains(id));
        let mut failures = Vec::new();
        let gone: Vec<String> = self
            .open
            .iter()
            .filter(|(id, (_, run))| {
                !live.contains(*id) || facts.get(*id).is_some_and(|f| !f.same_test(run))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            let Some((dir, _)) = self.open.remove(&id) else { continue };
            if let Err(e) = vike_runs::end_forward_run(&dir, now_ms) {
                failures.push(format!("mount `{id}`: its forward run's end was not written: {e}"));
            }
        }
        for id in live {
            let Some(mount) = facts.get(id).filter(|_| !self.open.contains_key(id)) else {
                continue;
            };
            if self.retry_at.get(id).is_some_and(|(at, _)| now_ms < *at) {
                continue;
            }
            match open_or_resume(&self.runs_root, mount, now_ms) {
                Ok(opened) => {
                    self.retry_at.remove(id);
                    self.open.insert(id.clone(), opened);
                }
                Err(why) => {
                    let mut ladder = self.retry_at.remove(id).map_or_else(
                        || ReconnectBackoff::new(FORWARD_RETRY_FIRST, FORWARD_RETRY_MAX),
                        |(_, ladder)| ladder,
                    );
                    let wait = i64::try_from(ladder.failed().as_millis()).unwrap_or(i64::MAX);
                    self.retry_at.insert(id.clone(), (now_ms.saturating_add(wait), ladder));
                    failures.push(format!("mount `{id}`: no forward run: {why}"));
                }
            }
        }
        failures
    }
}

/// The open forward run of `mount` under `runs_root`: the one a restart left open when it is the same test
/// (resumed, its own start kept), else a new one at `now_ms`, after ending every other open run of that mount id.
fn open_or_resume(
    runs_root: &Path,
    mount: &ForwardMountFacts,
    now_ms: i64,
) -> Result<(PathBuf, ForwardRun), String> {
    let mut resumed = None;
    for scanned in vike_runs::scan_runs(runs_root).runs {
        if scanned.manifest.kind != FORWARD_RUN_KIND
            || scanned.manifest.config.name.as_deref() != Some(mount.mount_id.as_str())
        {
            continue;
        }
        let Ok((run, None)) = vike_runs::read_forward_run(&scanned.dir) else { continue };
        if run.mount_id != mount.mount_id {
            continue;
        }
        if resumed.is_none() && mount.same_test(&run) {
            resumed = Some((scanned.dir, run));
        } else {
            vike_runs::end_forward_run(&scanned.dir, now_ms).map_err(|e| e.to_string())?;
        }
    }
    if let Some(open) = resumed {
        return Ok(open);
    }
    let run = mount.run_at(now_ms);
    // shortcut: no commit in the manifest (`git_sha: None`); the filter that keeps `unknown`/`frozen` out
    // lives in `crates/vike-backend/src/lib.rs`'s `manifest_git_sha`, above this crate. Move it down to name one.
    let dir = vike_runs::write_forward_run(runs_root, &run, None).map_err(|e| e.to_string())?;
    Ok((dir.path, run))
}

/// The live mount ids of the core's published snapshot.
pub(super) type LiveMounts = Box<dyn Fn() -> BTreeSet<String> + Send>;

/// The live mount ids of `handle`'s published snapshot, read off its arc-swap cell (never the fold).
pub(super) fn live_mounts_of(handle: &vike_core::CoreHandle) -> LiveMounts {
    let cell = handle.snapshot_cell();
    Box::new(move || {
        cell.load_full()
            .mounts
            .iter()
            .filter(|m| m.kind == vike_execution::MountViewKind::Mount)
            .map(|m| m.mount_id.clone())
            .collect()
    })
}

/// **The production recorder**: `None` (nothing spawned, nothing read, nothing written) when `enabled` is false
/// (`flags.record_forward_runs`) or the process declared no project (said at `warn`); else the thread over the
/// mounts `live` reads (production: [`live_mounts_of`]) and the booted project's runs root.
pub(super) fn start(
    enabled: bool,
    floor: vike_boot::disk_floor::DiskFloor,
    live: impl FnOnce() -> LiveMounts,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    if !enabled {
        return None;
    }
    let Some(runs_root) = crate::mount_factory::project_strategies_dir().and_then(|strategies| {
        let project = strategies.parent()?;
        Some(
            project
                .join(vike_paths::state_path::PROJECT_USER_DATA_DIR)
                .join(vike_paths::state_path::RUNS_SUBDIR),
        )
    }) else {
        tracing::warn!(
            "forward runs NOT recorded: this process declared no project, so it has no runs root"
        );
        return None;
    };
    let recorder = ForwardRecorder::new(runs_root).with_write_floor(move |dir| floor.refusal(dir));
    spawn_recorder(recorder, live(), stop, FORWARD_CHECK_EVERY)
}

/// Spawn `th-forward` over `recorder`, reading `live` and [`MOUNT_FACTS`] every `every` until `stop`.
pub(super) fn spawn_recorder(
    mut recorder: ForwardRecorder,
    live: LiveMounts,
    stop: Arc<AtomicBool>,
    every: Duration,
) -> Option<std::thread::JoinHandle<()>> {
    let spawned = std::thread::Builder::new().name("th-forward".into()).spawn(move || {
        let mut said: BTreeSet<String> = BTreeSet::new();
        while !stop.load(Ordering::Relaxed) {
            let facts = MOUNT_FACTS.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for why in recorder.pass(&live(), &facts, vike_model::now_ms()) {
                if said.insert(why.clone()) {
                    tracing::warn!("forward run: {why}");
                }
            }
            let mut slept = Duration::ZERO;
            while slept < every && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(STOP_POLL);
                slept += STOP_POLL;
            }
        }
    });
    match spawned {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::error!(error = %e, "the forward-run recorder could not start: no mount is recorded");
            None
        }
    }
}
