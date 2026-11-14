//! **The `.rs` mounts the start deactivated for their BUILD** (the owner's ruling of 2026-11-14: the tradehub
//! does not wait for a build). The start judged each file without building it
//! (`crate::config::ProfileDaemon::resolve_mount_awaiting_build`) and the core PARKED its mount
//! (`vike_core::CoreConfig::mounts_awaiting_strategy`); here, once the core runs, each file is built and opened
//! in its sandboxed process on a thread of its own, and the mount is handed to the core
//! (`vike_execution::AccountChange::AddMounts`, the path the deactivated FX mounts start by), which mounts it on
//! the fields the start built for it and journals its strategy file then.
//!
//! - While it builds, the mount is NOT RUNNING with the reason `building <file>`: its row says so
//!   (`crate::config::MountBuilds`, read by the publisher), and the core holds no slot for it, so it places
//!   no order.
//! - A build the compiler refuses (or a file that refuses itself, or no sandbox here) leaves it deactivated with
//!   that reason, said at ERROR; the node runs on.
//! - A builder that did not answer (not up yet after a restart, a version skew, a refused key) is asked again,
//!   [`FIRST_PAUSE`] doubling to [`LONGEST_PAUSE`], its row saying why meanwhile.
//! - Nothing waits for these threads: the ready line, a restart's shutdown and the teardown never do. A stop
//!   ends a pause at once; a build in flight is abandoned with the process.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::MountResolved;
use crate::config::{MountBuilds, OpenRefusal, SandboxBuild, SandboxOpen, SandboxOpened};

/// The first pause before a builder that did not answer is asked again; doubled at each miss.
const FIRST_PAUSE: Duration = Duration::from_secs(2);
/// The longest pause between two asks of a builder that does not answer.
const LONGEST_PAUSE: Duration = Duration::from_secs(60);
/// How often a pause looks at the stop flag.
const SLICE: Duration = Duration::from_millis(100);

/// How a mount waiting for its build is built and opened: `crate::config::open_built` in the daemon.
pub(super) type Opener =
    Arc<dyn Fn(&SandboxOpen<'_>) -> Result<SandboxOpened, OpenRefusal> + Send + Sync>;

/// One `.rs` mount the start did not build: the build, the mount's spec (what the core mounts) and how its row
/// is written.
pub(super) struct AwaitedMount {
    build: SandboxBuild,
    spec: vike_tradehub_core::MountPlacement,
    /// A `[[mounts]]` profile writes each row self-addressed (`super::mount_rows::mounts_wire_params`).
    multi: bool,
}

impl AwaitedMount {
    /// The mounts of `resolved` that wait for their build, in mount order.
    pub(super) fn of_resolved(resolved: &[MountResolved], multi: bool) -> Vec<AwaitedMount> {
        resolved
            .iter()
            .filter_map(|m| {
                Some(AwaitedMount { build: m.building.clone()?, spec: m.spec.clone(), multi })
            })
            .collect()
    }

    /// The row's params text for `strategy_params`, as the boot wrote the row's.
    fn row(&self, strategy_params: &str) -> String {
        if self.multi {
            super::mount_rows::mount_row_params(&self.spec, strategy_params)
        } else {
            strategy_params.to_string()
        }
    }

    fn said(&self) -> String {
        format!("{}/{} @ {}", self.spec.venue, self.spec.symbol, self.spec.interval)
    }
}

/// The ids the core parks: every mount of `resolved` that waits for its build.
pub(super) fn awaiting_ids(resolved: &[MountResolved]) -> Vec<String> {
    resolved.iter().filter_map(|m| m.building.as_ref().map(SandboxBuild::mount_id)).collect()
}

/// Build and start every mount of `awaited`, each on a thread of its own, into the core behind `core`; `builds`
/// is what their rows say, `stop` the process's stop flag. Returns at once.
pub(super) fn start(
    awaited: Vec<AwaitedMount>,
    core: &vike_execution::EventSender,
    builds: &MountBuilds,
    stop: &Arc<AtomicBool>,
) {
    let open: Opener = Arc::new(crate::config::open_built);
    let _ = start_with(awaited, core, builds, stop, &open);
}

/// [`start`] with the opener named (a test's stand-in builds nothing). Returns the threads, for a test to join.
pub(super) fn start_with(
    awaited: Vec<AwaitedMount>,
    core: &vike_execution::EventSender,
    builds: &MountBuilds,
    stop: &Arc<AtomicBool>,
    open: &Opener,
) -> Vec<std::thread::JoinHandle<()>> {
    let mut threads = Vec::with_capacity(awaited.len());
    for mount in awaited {
        let file = mount.build.file().to_string();
        builds.set(&mount.build.mount_id(), Err(mount.row(&crate::config::building_words(&file))));
        tracing::warn!(
            file = %file,
            "ARMING: the strategy mount on {} is DEACTIVATED: building {file}. The node runs without it, \
             every other mount runs, and it starts by itself when its build answers",
            mount.said()
        );
        let (core, builds, stop, open) =
            (core.clone(), builds.clone(), Arc::clone(stop), Arc::clone(open));
        match std::thread::Builder::new()
            .name("strategy-build".to_string())
            .spawn(move || build_and_mount(&mount, &core, &builds, &stop, &*open))
        {
            Ok(thread) => threads.push(thread),
            Err(e) => tracing::error!(
                file = %file,
                "the strategy mount's build thread could not be started ({e}): it stays DEACTIVATED until \
                 the next start"
            ),
        }
    }
    threads
}

/// One mount's whole wait: build and open it (asking a builder that did not answer again), then hand it to the
/// core; or leave it deactivated with the reason.
fn build_and_mount(
    mount: &AwaitedMount,
    core: &vike_execution::EventSender,
    builds: &MountBuilds,
    stop: &AtomicBool,
    open: &dyn Fn(&SandboxOpen<'_>) -> Result<SandboxOpened, OpenRefusal>,
) {
    let id = mount.build.mount_id();
    let mut pause = FIRST_PAUSE;
    let mut last: Option<String> = None;
    while !stop.load(Ordering::Relaxed) {
        match mount.build.open(open) {
            Ok(opened) => return mount_opened(mount, opened, core, builds, stop),
            Err(OpenRefusal::Refused(why)) => {
                tracing::error!(
                    file = mount.build.file(),
                    "the strategy mount on {} stays DEACTIVATED: {why}. The node runs on without it; fix the \
                     file and restart the tradehub",
                    mount.said()
                );
                builds.set(&id, Err(mount.row(&why)));
                return;
            }
            Err(OpenRefusal::BuilderUnanswered(why)) => {
                if last.as_deref() != Some(why.as_str()) {
                    tracing::warn!(
                        file = mount.build.file(),
                        "the strategy mount on {} waits: {why}. The builder is asked again, every {} s at \
                         the longest",
                        mount.said(),
                        LONGEST_PAUSE.as_secs()
                    );
                    builds.set(&id, Err(mount.row(&why)));
                    last = Some(why);
                }
                let mut left = pause;
                while !left.is_zero() && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(SLICE.min(left));
                    left = left.saturating_sub(SLICE);
                }
                pause = (pause * 2).min(LONGEST_PAUSE);
            }
        }
    }
}

/// The file is built and open: re-arm it at its account's fee rate when the start resolved one (as the
/// assembly re-arms a mount that opened there), give it the core's lane, and hand the mount to the core.
fn mount_opened(
    mount: &AwaitedMount,
    opened: SandboxOpened,
    core: &vike_execution::EventSender,
    builds: &MountBuilds,
    stop: &AtomicBool,
) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let SandboxOpened { strategy, echo, remote } = opened;
    if let Some(remote) = &remote {
        if let Some(schedule) = mount.build.account_fee() {
            // A sandboxed re-arm rebuilds in a fresh child and hands back no strategy (`RemoteHandle::fee_rearm`).
            let _ = (remote.fee_rearm())(schedule);
        }
        remote.attach(core.clone());
    }
    let parcel = vike_tradehub_core::MountBuild { strategy, spec: mount.spec.clone() }.into_mount();
    if core.add_account_mounts(Box::new(vec![parcel])).is_err() {
        tracing::warn!(
            file = mount.build.file(),
            "the strategy mount on {} was built, and the core had stopped: it is not mounted",
            mount.said()
        );
        return;
    }
    let params = crate::config::echo_params_line(mount.build.file(), &echo);
    tracing::info!(
        file = mount.build.file(),
        "the strategy mount on {} is BUILT and handed to the core, which mounts it now (its strategy file is \
         journaled as it opens): {params}",
        mount.said()
    );
    builds.set(&mount.build.mount_id(), Ok(mount.row(&params)));
}
